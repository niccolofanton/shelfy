// Unblock flow on the user's REAL browser (Google Chrome, else Chromium/Brave/
// Edge). Anti-bot checks (Cloudflare Turnstile & co.) loop forever inside an
// Electron window — its fingerprint is not a standard browser and the clearance
// cookie never sticks — while they pass in a normal Chrome.
//
// How it works:
//   1. spawn the browser with a DEDICATED profile (never the user's own) and a
//      DevTools port, showing the blocked page;
//   2. while the user solves the check, NOTHING is attached: progress is read
//      from the /json/list tab listing only (no CDP session on the page);
//   3. once the page is past the interstitial, Playwright attaches over CDP to
//      that same browser/profile and the normal capture runs there (same cookies,
//      fingerprint and IP → the clearance stays valid);
//   4. the browser is closed at the end.

import fs from 'fs';
import path from 'path';
import { spawn, type ChildProcess } from 'child_process';
import { app } from 'electron';
import WebSocket from 'ws';
import { CdpPage, type CdpTransport } from './cdp-page';
import { VIEWPORT, type PageDriver, type SiteSession } from './driver';

const CANDIDATES: Record<string, string[]> = {
  darwin: [
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
    '/Applications/Brave Browser.app/Contents/MacOS/Brave Browser',
    '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  ],
  win32: [
    path.join(
      process.env['PROGRAMFILES'] || 'C:\\Program Files',
      'Google\\Chrome\\Application\\chrome.exe',
    ),
    path.join(
      process.env['PROGRAMFILES(X86)'] || 'C:\\Program Files (x86)',
      'Google\\Chrome\\Application\\chrome.exe',
    ),
    path.join(process.env['LOCALAPPDATA'] || '', 'Google\\Chrome\\Application\\chrome.exe'),
    path.join(
      process.env['PROGRAMFILES(X86)'] || 'C:\\Program Files (x86)',
      'Microsoft\\Edge\\Application\\msedge.exe',
    ),
    path.join(
      process.env['PROGRAMFILES'] || 'C:\\Program Files',
      'BraveSoftware\\Brave-Browser\\Application\\brave.exe',
    ),
  ],
  linux: [
    '/usr/bin/google-chrome',
    '/usr/bin/google-chrome-stable',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
    '/usr/bin/brave-browser',
    '/usr/bin/microsoft-edge',
  ],
};

export function findSystemChrome(): string | null {
  for (const p of CANDIDATES[process.platform] || []) {
    try {
      if (p && fs.existsSync(p)) return p;
    } catch {}
  }
  return null;
}

const CHALLENGE_TITLE =
  /just a moment|attention required|un momento|un instant|einen moment|momentje|security checkpoint|checking your browser|verify(ing)? you are human|access denied|access to this page has been denied|pardon our interruption|are you a robot|captcha/i;

interface TabInfo {
  type: string;
  title: string;
  url: string;
}

async function listTabs(port: number): Promise<TabInfo[]> {
  const res = await fetch(`http://127.0.0.1:${port}/json/list`, {
    signal: AbortSignal.timeout(3_000),
  });
  return (await res.json()) as TabInfo[];
}

function hostKey(u: string): string {
  try {
    return new URL(u).hostname.replace(/^www\./, '');
  } catch {
    return '';
  }
}

export class SystemChromeUnblock {
  private conn: CdpConnection | null = null;
  private exited = false;

  private constructor(
    private readonly child: ChildProcess,
    private readonly port: number,
    private readonly targetHost: string,
  ) {
    child.on('exit', () => {
      this.exited = true;
    });
  }

  static async open(url: string): Promise<SystemChromeUnblock | null> {
    const exe = findSystemChrome();
    if (!exe) return null;
    const profile = path.join(app.getPath('userData'), 'webcapture-browser');
    fs.mkdirSync(profile, { recursive: true });
    const portFile = path.join(profile, 'DevToolsActivePort');
    try {
      fs.rmSync(portFile, { force: true });
    } catch {}
    const child = spawn(
      exe,
      [
        `--user-data-dir=${profile}`,
        '--remote-debugging-port=0',
        '--remote-allow-origins=http://127.0.0.1',
        '--no-first-run',
        '--no-default-browser-check',
        '--disable-default-apps',
        '--disable-sync',
        '--window-size=1440,960',
        '--new-window',
        url,
      ],
      { stdio: 'ignore', detached: false },
    );
    child.on('error', () => {});
    const t0 = Date.now();
    while (Date.now() - t0 < 15_000) {
      try {
        const [line] = fs.readFileSync(portFile, 'utf8').split('\n');
        const port = Number(line);
        if (port > 0) return new SystemChromeUnblock(child, port, hostKey(url));
      } catch {}
      await new Promise((r) => setTimeout(r, 250));
    }
    try {
      child.kill();
    } catch {}
    return null;
  }

  private async connectQuietly(): Promise<CdpConnection | null> {
    try {
      const ver = (await (
        await fetch(`http://127.0.0.1:${this.port}/json/version`, {
          signal: AbortSignal.timeout(2_000),
        })
      ).json()) as {
        webSocketDebuggerUrl: string;
      };
      return await CdpConnection.connect(ver.webSocketDebuggerUrl);
    } catch {
      return null;
    }
  }

  focus(): void {
    // The window belongs to another app: the user brings it forward themselves.
  }

  get isClosed(): boolean {
    return this.exited;
  }

  // Resolves true once a tab of the target site shows a non-challenge title for
  // a couple of seconds; false when the browser is closed or the time runs out.
  async waitUntilUnblocked({
    timeoutMs,
    signal,
  }: {
    timeoutMs: number;
    signal?: AbortSignal;
  }): Promise<boolean> {
    const t0 = Date.now();
    let clearSince = 0;
    let lastTitle = '';
    while (!this.exited && !signal?.aborted && Date.now() - t0 < timeoutMs) {
      try {
        const tabs = (await listTabs(this.port)).filter(
          (t) => t.type === 'page' && hostKey(t.url) === this.targetHost,
        );
        const tab = tabs[0];
        const challenge =
          !tab ||
          CHALLENGE_TITLE.test(tab.title || '') ||
          /\/cdn-cgi\//.test(tab.url) ||
          !tab.title;
        if (!challenge && tab.title === lastTitle) {
          if (!clearSince) clearSince = Date.now();
          if (Date.now() - clearSince > 2_500) return true;
        } else clearSince = 0;
        lastTitle = tab?.title || '';
      } catch {
        if (this.exited) return false;
      }
      await new Promise((r) => setTimeout(r, 800));
    }
    return false;
  }

  // Attach to the same browser + profile with a raw, low-footprint DevTools
  // connection (no Playwright: its Runtime.enable on every page is exactly what
  // anti-bot scripts detect, and the clearance would be revoked on reload).
  async attach(): Promise<SiteSession> {
    const ver = (await (await fetch(`http://127.0.0.1:${this.port}/json/version`)).json()) as {
      webSocketDebuggerUrl: string;
      'User-Agent'?: string;
    };
    this.conn = await CdpConnection.connect(ver.webSocketDebuggerUrl);
    const tab = (await listTabs(this.port)).find(
      (t) => t.type === 'page' && hostKey(t.url) === this.targetHost,
    ) as (TabInfo & { id?: string }) | undefined;
    return new SystemChromeSession(
      this.conn,
      ver['User-Agent'] || '',
      tab?.id || null,
      this.targetHost,
    );
  }

  async close(): Promise<void> {
    try {
      const conn = this.conn || (await this.connectQuietly());
      await conn?.send('Browser.close').catch(() => {});
      conn?.close();
    } catch {}
    if (!this.exited) {
      try {
        this.child.kill();
      } catch {}
    }
  }
}

// ─── Raw DevTools connection (flat sessions) ─────────────────────────────────

class CdpConnection {
  private seq = 0;
  private pending = new Map<
    number,
    { resolve: (v: unknown) => void; reject: (e: Error) => void }
  >();
  private listeners = new Set<
    (sessionId: string | undefined, method: string, params: Record<string, unknown>) => void
  >();

  private constructor(private readonly ws: WebSocket) {
    ws.on('message', (data) => {
      let msg: {
        id?: number;
        result?: unknown;
        error?: { message: string };
        method?: string;
        params?: Record<string, unknown>;
        sessionId?: string;
      };
      try {
        msg = JSON.parse(String(data));
      } catch {
        return;
      }
      if (msg.id !== undefined) {
        const p = this.pending.get(msg.id);
        if (!p) return;
        this.pending.delete(msg.id);
        if (msg.error) p.reject(new Error(msg.error.message));
        else p.resolve(msg.result);
      } else if (msg.method) {
        for (const l of this.listeners) l(msg.sessionId, msg.method, msg.params || {});
      }
    });
    ws.on('close', () => {
      for (const p of this.pending.values()) p.reject(new Error('DevTools connection closed'));
      this.pending.clear();
    });
  }

  static connect(url: string): Promise<CdpConnection> {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url, { perMessageDeflate: false, maxPayload: 512 * 1024 * 1024 });
      ws.once('open', () => resolve(new CdpConnection(ws)));
      ws.once('error', reject);
    });
  }

  send<T = Record<string, unknown>>(
    method: string,
    params?: Record<string, unknown>,
    sessionId?: string,
  ): Promise<T> {
    const id = ++this.seq;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      this.ws.send(
        JSON.stringify({ id, method, params: params || {}, ...(sessionId ? { sessionId } : {}) }),
      );
    });
  }

  on(
    listener: (
      sessionId: string | undefined,
      method: string,
      params: Record<string, unknown>,
    ) => void,
  ): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  close(): void {
    try {
      this.ws.close();
    } catch {}
  }
}

// Capture session on the user's real Chrome: one foreground tab per page, one
// page at a time (background tabs are throttled), the window sized so the
// viewport is 1440 CSS px wide.
class SystemChromeSession implements SiteSession {
  readonly engine = 'chrome' as const;
  readonly maxConcurrency = 1;

  private page: CdpPage | null = null;

  constructor(
    private readonly conn: CdpConnection,
    readonly userAgent: string,
    // The tab where the user passed the check: captured in place, then reused
    // for the inner pages (a fresh tab gets challenged again on some sites).
    private readonly tabId: string | null,
    private readonly host: string,
  ) {}

  async newPage(): Promise<PageDriver> {
    if (this.page) return this.page;
    const targetId =
      this.tabId ||
      (await this.conn.send<{ targetId: string }>('Target.createTarget', { url: 'about:blank' }))
        .targetId;
    const { sessionId } = await this.conn.send<{ sessionId: string }>('Target.attachToTarget', {
      targetId,
      flatten: true,
    });
    const transport: CdpTransport = {
      send: (method, params) => this.conn.send(method, params, sessionId),
      onEvent: (listener) =>
        this.conn.on((sid, method, params) => {
          if (sid === sessionId) listener(method, params);
        }),
    };
    await this.fitWindow(targetId, transport);
    const page = new CdpPage(
      transport,
      {
        stealth: true,
        fetchBlocking: false,
        reusable: true,
        adoptHost: this.tabId ? this.host : undefined,
      },
      'chrome',
    );
    await page.init();
    this.page = page;
    return page;
  }

  // Size the browser window so the page viewport is 1440 CSS px wide and ~900
  // tall, without device emulation (an emulated screen is itself a bot signal).
  private async fitWindow(targetId: string, t: CdpTransport): Promise<void> {
    try {
      const { windowId } = await this.conn.send<{ windowId: number }>(
        'Browser.getWindowForTarget',
        { targetId },
      );
      let width = VIEWPORT.width;
      let height = VIEWPORT.height + 90;
      for (let i = 0; i < 3; i++) {
        await this.conn
          .send('Browser.setWindowBounds', { windowId, bounds: { windowState: 'normal' } })
          .catch(() => {});
        await this.conn.send('Browser.setWindowBounds', { windowId, bounds: { width, height } });
        await new Promise((r) => setTimeout(r, 250));
        const res = await t.send<{ result?: { value?: { w: number; h: number } } }>(
          'Runtime.evaluate',
          {
            expression: '({ w: innerWidth, h: innerHeight })',
            returnByValue: true,
          },
        );
        const v = res.result?.value;
        if (!v) break;
        const dw = VIEWPORT.width - v.w;
        const dh = VIEWPORT.height - v.h;
        if (Math.abs(dw) <= 1 && Math.abs(dh) <= 2) break;
        width += dw;
        height += dh;
      }
    } catch {
      /* keep the window as it is */
    }
  }

  async close(): Promise<void> {
    /* the browser is closed by SystemChromeUnblock.close() */
  }
}
