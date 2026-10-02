// Electron capture driver: a Chromium window of the app driven over the
// DevTools protocol (webContents.debugger). Used as the fallback when the
// Playwright browser cannot launch (hidden window). Single tab: pages are
// captured one at a time (maxConcurrency = 1).
//
// Isolation: a dedicated partition (never the user's social session), sandboxed
// renderer, no preload, no node; popups/downloads/permission prompts refused;
// every request SSRF-gated and content-blocked in session.webRequest.

import { BrowserWindow, session as electronSession } from 'electron';
import type { Session } from 'electron';
import { isBlockedHostname } from '../net-safety';
import { getEngine, shouldBlock } from './blocker';
import { CdpPage, CHALLENGE_HOSTS, type CdpTransport } from './cdp-page';
import { VIEWPORT, acceptLanguage, type PageDriver, type SiteSession } from './driver';

export const CAPTURE_PARTITION = 'persist:webcapture';

type TaggedWindow = BrowserWindow & { __shelfyCapture?: boolean };

let partitionReady: Promise<Session> | null = null;

function preparePartition(): Promise<Session> {
  if (!partitionReady) {
    partitionReady = (async () => {
      const ses = electronSession.fromPartition(CAPTURE_PARTITION);
      // A real Chrome UA: no "Electron"/app tokens, reduced version (Chrome/126.0.0.0).
      const ua = ses
        .getUserAgent()
        .replace(/\sElectron\/\S+/i, '')
        .replace(/\s(?!Chrome|Safari|AppleWebKit|Mozilla)[A-Za-z][\w.-]*\/\d[\w.-]*(?=\s)/g, '')
        .replace(/Chrome\/(\d+)\.[\d.]+/, 'Chrome/$1.0.0.0');
      ses.setUserAgent(ua, acceptLanguage());
      ses.setPermissionRequestHandler((_wc, _perm, cb) => cb(false));
      ses.on('will-download', (e) => e.preventDefault());
      const engine = await getEngine();
      ses.webRequest.onBeforeRequest((details, cb) => {
        try {
          const u = new URL(details.url);
          if (
            (u.protocol === 'http:' || u.protocol === 'https:') &&
            isBlockedHostname(u.hostname)
          ) {
            return cb({ cancel: true });
          }
          if (CHALLENGE_HOSTS.test(u.hostname) || u.pathname.startsWith('/cdn-cgi/')) return cb({});
          const isMain = details.resourceType === 'mainFrame';
          const source = details.referrer || (details.frame ? details.frame.url : '') || '';
          if (shouldBlock(engine, details.url, source, details.resourceType, isMain))
            return cb({ cancel: true });
        } catch {
          /* never break navigation on a guard error */
        }
        cb({});
      });
      return ses;
    })();
  }
  return partitionReady;
}

export class ElectronSession implements SiteSession {
  readonly engine = 'electron' as const;
  readonly maxConcurrency = 1;
  private page: CdpPage | null = null;
  private closed = false;

  private constructor(
    readonly window: BrowserWindow,
    readonly userAgent: string,
  ) {
    window.on('closed', () => {
      this.closed = true;
    });
  }

  static async create({
    visible,
    url,
    title,
  }: {
    visible: boolean;
    url?: string;
    title?: string;
  }): Promise<ElectronSession> {
    const ses = await preparePartition();
    const win = new BrowserWindow({
      width: VIEWPORT.width,
      height: VIEWPORT.height,
      useContentSize: true,
      show: visible,
      paintWhenInitiallyHidden: true,
      title: title || 'Shelfy',
      autoHideMenuBar: true,
      webPreferences: {
        session: ses,
        sandbox: true,
        contextIsolation: true,
        nodeIntegration: false,
        webSecurity: true,
        backgroundThrottling: false,
        spellcheck: false,
      },
    }) as TaggedWindow;
    win.__shelfyCapture = true;
    // Popups never leave the window (the global handler would open them in the
    // user's default browser).
    win.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
    win.on('page-title-updated', (e) => e.preventDefault());
    const s = new ElectronSession(win, ses.getUserAgent());
    if (url) await win.loadURL(url).catch(() => {});
    return s;
  }

  get isClosed(): boolean {
    return this.closed || this.window.isDestroyed();
  }

  async newPage(): Promise<PageDriver> {
    if (this.isClosed) throw new Error('Finestra di cattura chiusa');
    if (!this.page) {
      const dbg = this.window.webContents.debugger;
      if (!dbg.isAttached()) dbg.attach('1.3');
      const transport: CdpTransport = {
        send: (method, params) => dbg.sendCommand(method, params) as never,
        onEvent: (listener) => {
          const h = (_e: unknown, method: string, params: Record<string, unknown>): void =>
            listener(method, params || {});
          dbg.on('message', h);
          return () => dbg.removeListener('message', h);
        },
      };
      this.page = new CdpPage(
        transport,
        { stealth: false, fetchBlocking: false, reusable: true },
        'electron',
      );
      await this.page.init();
    }
    return this.page;
  }

  // Polls the window until the anti-bot interstitial is gone (the user solved
  // it) — resolves true — or the window is closed / the deadline passes.
  async waitUntilUnblocked(
    detectScript: string,
    { timeoutMs }: { timeoutMs: number },
  ): Promise<boolean> {
    const t0 = Date.now();
    let clearSince = 0;
    while (!this.isClosed && Date.now() - t0 < timeoutMs) {
      try {
        const res = (await this.window.webContents.executeJavaScript(detectScript, true)) as {
          blocked?: boolean;
        };
        if (res && !res.blocked && !this.window.webContents.isLoading()) {
          if (!clearSince) clearSince = Date.now();
          if (Date.now() - clearSince > 1_500) return true;
        } else clearSince = 0;
      } catch {
        clearSince = 0;
      }
      await new Promise((r) => setTimeout(r, 700));
    }
    return false;
  }

  async close(): Promise<void> {
    try {
      if (this.window.webContents.debugger.isAttached()) this.window.webContents.debugger.detach();
    } catch {}
    if (!this.window.isDestroyed()) this.window.destroy();
  }
}
