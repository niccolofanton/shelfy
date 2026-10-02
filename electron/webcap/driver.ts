// Capture drivers for the web-reference pipeline (v2).
//
// The capture logic (capture.ts) is written once against PageDriver and runs on:
//   • PlaywrightSession — the default: a sandboxed headless Chromium, ONE browser
//     context per site (shared HTTP cache and consent state across its pages);
//   • ElectronSession (electron-driver.ts) — a real Electron window driven over
//     CDP, used when a site needs the user to pass an anti-bot check in a visible
//     window, and as the fallback when Playwright cannot launch.
//
// Security: every request is SSRF-gated (loopback/private/metadata hosts are
// aborted) BEFORE content blocking; no cookies from the user's social sessions
// are ever imported; service workers are blocked; dialogs, downloads and popups
// are refused.

import fs from 'fs';
import os from 'os';
import path from 'path';
import { app } from 'electron';
import type {
  Browser,
  BrowserContext,
  CDPSession,
  Page,
  Route,
  Request as PwRequest,
} from 'playwright-core';
import { isBlockedHostname } from '../net-safety';
import { getBrowser } from '../webcapture-playwright';
import { getEngine, shouldBlock, cosmeticCss, JS_COSMETIC_FEATURES } from './blocker';
import { INIT_SCRIPT } from './scripts';

export const VIEWPORT = { width: 1440, height: 900, scale: 2 };

export interface NavResult {
  status: number | null;
  headers: Record<string, string>;
  contentType: string;
  finalUrl: string;
}

export interface NetEntry {
  url: string;
  type: string;
  status: number;
  mime: string;
  bytes: number;
}

export interface ScreenshotOptions {
  clip?: { x: number; y: number; width: number; height: number };
  fullPage?: boolean;
  animations?: 'allow' | 'disabled';
  timeoutMs?: number;
}

export interface PageDriver {
  readonly engine: 'playwright' | 'electron' | 'chrome';
  readonly viewport: { width: number; height: number; scale: number };
  goto(url: string, timeoutMs: number): Promise<NavResult>;
  waitForLoad(ms: number): Promise<void>;
  waitForNetworkIdle(ms: number): Promise<void>;
  url(): string;
  evaluate<T>(expression: string, opts?: { timeoutMs?: number; fallback?: T }): Promise<T>;
  screenshot(opts: ScreenshotOptions): Promise<Buffer>;
  wheel(deltaY: number): Promise<void>;
  mouseMove(x: number, y: number): Promise<void>;
  pressKey(key: string): Promise<void>;
  startScreencast(
    onFrame: (jpeg: Buffer, tsSec: number) => void,
    opts: { maxWidth: number; maxHeight: number; quality: number },
  ): Promise<() => Promise<void>>;
  applyCosmetics(): Promise<number>;
  network(): Promise<NetEntry[]>;
  close(): Promise<void>;
}

export interface SiteSession {
  readonly engine: 'playwright' | 'electron' | 'chrome';
  readonly userAgent: string;
  readonly maxConcurrency?: number; // pages captured at once (single-tab drivers: 1)
  newPage(): Promise<PageDriver>;
  close(): Promise<void>;
}

// ─── Shared helpers ──────────────────────────────────────────────────────────

let autoconsentSource: string | null = null;
// DuckDuckGo autoconsent, self-contained build: detects the consent platform and
// clicks through the OPT-OUT path (reject non-essential), recording what it did
// in window.autoconsentStandalone.messages.
export function autoconsentScript(): string {
  if (autoconsentSource === null) {
    try {
      // The package only exports its main entry: resolve that and take the
      // standalone bundle next to it.
      const file = path.join(
        path.dirname(require.resolve('@duckduckgo/autoconsent')),
        'autoconsent.standalone.js',
      );
      autoconsentSource = fs.readFileSync(file, 'utf8');
    } catch (err) {
      console.warn('[webcap] autoconsent bundle unavailable:', (err as Error)?.message || err);
      autoconsentSource = '';
    }
  }
  return autoconsentSource;
}

// A real desktop Chrome UA for the actual engine version (the old hard-coded
// Chrome/124 on a Chromium 148 engine was itself a bot signal).
export function desktopUserAgent(chromeVersion: string): string {
  const major = String(chromeVersion || '').split('.')[0] || '130';
  const platform =
    process.platform === 'win32'
      ? 'Windows NT 10.0; Win64; x64'
      : process.platform === 'linux'
        ? 'X11; Linux x86_64'
        : 'Macintosh; Intel Mac OS X 10_15_7';
  return `Mozilla/5.0 (${platform}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/${major}.0.0.0 Safari/537.36`;
}

export function acceptLanguage(): string {
  let loc = 'en-US';
  try {
    loc = app.getLocale() || loc;
  } catch {}
  const base = loc.split('-')[0];
  return base === 'en' ? `${loc},en;q=0.9` : `${loc},${base};q=0.9,en;q=0.8`;
}

function withTimeout<T>(p: Promise<T>, ms: number, label: string): Promise<T> {
  let t: ReturnType<typeof setTimeout> | undefined;
  return Promise.race([
    p,
    new Promise<never>((_, rej) => {
      t = setTimeout(
        () => rej(Object.assign(new Error(`timeout: ${label}`), { name: 'TimeoutError' })),
        ms,
      );
    }),
  ]).finally(() => clearTimeout(t));
}

// ─── Playwright ──────────────────────────────────────────────────────────────

class PlaywrightPage implements PageDriver {
  readonly engine = 'playwright' as const;
  readonly viewport = { ...VIEWPORT };
  private net: NetEntry[] = [];
  private cdp: CDPSession | null = null;

  constructor(
    private readonly page: Page,
    private readonly context: BrowserContext,
    cdp?: CDPSession,
  ) {
    if (cdp) this.cdp = cdp;
    page.on('response', (res) => {
      try {
        if (this.net.length >= 3000) return;
        const req = res.request();
        const h = res.headers();
        this.net.push({
          url: res.url(),
          type: req.resourceType(),
          status: res.status(),
          mime: (h['content-type'] || '').split(';')[0].trim(),
          bytes: Number(h['content-length']) || 0,
        });
      } catch {}
    });
    page.on('dialog', (d) => {
      d.dismiss().catch(() => {});
    });
    page.on('download', (d) => {
      d.cancel().catch(() => {});
    });
  }

  url(): string {
    return this.page.url();
  }

  async goto(url: string, timeoutMs: number): Promise<NavResult> {
    const resp = await this.page.goto(url, { waitUntil: 'domcontentloaded', timeout: timeoutMs });
    let headers: Record<string, string> = {};
    try {
      headers = (resp && (await resp.allHeaders())) || {};
    } catch {
      headers = resp?.headers() || {};
    }
    return {
      status: resp ? resp.status() : null,
      headers,
      contentType: String(headers['content-type'] || '')
        .split(';')[0]
        .trim()
        .toLowerCase(),
      finalUrl: this.page.url() || url,
    };
  }

  async waitForLoad(ms: number): Promise<void> {
    await this.page.waitForLoadState('load', { timeout: ms }).catch(() => {});
  }

  async waitForNetworkIdle(ms: number): Promise<void> {
    await this.page.waitForLoadState('networkidle', { timeout: ms }).catch(() => {});
  }

  async evaluate<T>(
    expression: string,
    { timeoutMs = 15_000, fallback }: { timeoutMs?: number; fallback?: T } = {},
  ): Promise<T> {
    try {
      return (await withTimeout(this.page.evaluate(expression), timeoutMs, 'evaluate')) as T;
    } catch {
      return fallback as T;
    }
  }

  async screenshot({
    clip,
    fullPage = false,
    animations = 'allow',
    timeoutMs = 45_000,
  }: ScreenshotOptions): Promise<Buffer> {
    return this.page.screenshot({
      type: 'png',
      fullPage,
      clip,
      animations,
      caret: 'hide',
      scale: 'device',
      timeout: timeoutMs,
    });
  }

  async wheel(deltaY: number): Promise<void> {
    await this.page.mouse.wheel(0, deltaY);
  }

  async mouseMove(x: number, y: number): Promise<void> {
    await this.page.mouse.move(x, y);
  }

  async pressKey(key: string): Promise<void> {
    await this.page.keyboard.press(key).catch(() => {});
  }

  async startScreencast(
    onFrame: (jpeg: Buffer, tsSec: number) => void,
    { maxWidth, maxHeight, quality }: { maxWidth: number; maxHeight: number; quality: number },
  ): Promise<() => Promise<void>> {
    if (!this.cdp) this.cdp = await this.context.newCDPSession(this.page);
    const cdp = this.cdp;
    const handler = (ev: {
      data: string;
      metadata: { timestamp?: number };
      sessionId: number;
    }): void => {
      try {
        onFrame(Buffer.from(ev.data, 'base64'), ev.metadata?.timestamp ?? Date.now() / 1000);
      } catch {}
      cdp.send('Page.screencastFrameAck', { sessionId: ev.sessionId }).catch(() => {});
    };
    cdp.on('Page.screencastFrame', handler);
    await cdp.send('Page.startScreencast', {
      format: 'jpeg',
      quality,
      maxWidth,
      maxHeight,
      everyNthFrame: 1,
    });
    return async () => {
      try {
        await cdp.send('Page.stopScreencast');
      } catch {}
      cdp.off('Page.screencastFrame', handler);
    };
  }

  async applyCosmetics(): Promise<number> {
    const engine = await getEngine();
    if (!engine) return 0;
    let injected = 0;
    for (const frame of this.page.frames()) {
      try {
        const url = frame.url();
        if (!/^https?:/.test(url)) continue;
        const features = (await withTimeout(
          frame.evaluate(JS_COSMETIC_FEATURES),
          5_000,
          'features',
        )) as {
          ids: string[];
          classes: string[];
          hrefs: string[];
        };
        const css = cosmeticCss(engine, url, features);
        if (css) {
          await frame.addStyleTag({ content: css }).catch(() => {});
          injected++;
        }
      } catch {}
    }
    return injected;
  }

  async network(): Promise<NetEntry[]> {
    return this.net.slice();
  }

  async close(): Promise<void> {
    try {
      await this.cdp?.detach();
    } catch {}
    await this.page.close().catch(() => {});
  }
}

// Init scripts, SSRF guard + content blocking, popup refusal: applied to any
// context the capture drives (fresh headless context or an attached real Chrome).
export async function configureContext(context: BrowserContext): Promise<void> {
  await context.addInitScript({ content: INIT_SCRIPT });
  const consent = autoconsentScript();
  if (consent) await context.addInitScript({ content: consent });
  const engine = await getEngine();
  await context.route('**/*', (route: Route, request: PwRequest) => {
    try {
      const u = new URL(request.url());
      if ((u.protocol === 'http:' || u.protocol === 'https:') && isBlockedHostname(u.hostname)) {
        return route.abort('blockedbyclient');
      }
      const frame = request.frame();
      const isMain = request.isNavigationRequest() && !!frame && frame.parentFrame() === null;
      const source = frame ? frame.url() : '';
      if (shouldBlock(engine, request.url(), source, request.resourceType(), isMain)) {
        return route.abort('blockedbyclient');
      }
    } catch {
      /* never let the guard itself break a navigation */
    }
    return route.fallback();
  });
  // window.open popups never get their own capture.
  context.on('page', (p) => {
    p.opener()
      .then((opener) => (opener ? p.close() : undefined))
      .catch(() => {});
  });
}

export class PlaywrightSession implements SiteSession {
  readonly engine = 'playwright' as const;
  constructor(
    private readonly context: BrowserContext,
    readonly userAgent: string,
    // Attached real browser: the context has no viewport emulation of its own,
    // so each page gets 1440×900 @2× through CDP; the context is not ours to close.
    private readonly attached = false,
    readonly maxConcurrency?: number,
  ) {}

  static async create(): Promise<PlaywrightSession> {
    const browser: Browser = await getBrowser();
    const userAgent = desktopUserAgent(browser.version());
    let locale = 'en-US';
    try {
      locale = app.getLocale() || locale;
    } catch {}
    const context = await browser.newContext({
      viewport: { width: VIEWPORT.width, height: VIEWPORT.height },
      deviceScaleFactor: VIEWPORT.scale,
      userAgent,
      locale,
      extraHTTPHeaders: { 'Accept-Language': acceptLanguage() },
      serviceWorkers: 'block',
      bypassCSP: true,
      ignoreHTTPSErrors: false,
      acceptDownloads: false,
      // Deliberately NOT reducedMotion: motion-heavy reference sites disable the
      // very animations/WebGL the reference is about when it is set.
    });
    await configureContext(context);
    return new PlaywrightSession(context, userAgent);
  }

  async newPage(): Promise<PageDriver> {
    const page = await this.context.newPage();
    page.setDefaultTimeout(45_000);
    if (this.attached) {
      const cdp = await this.context.newCDPSession(page);
      await cdp.send('Emulation.setDeviceMetricsOverride', {
        width: VIEWPORT.width,
        height: VIEWPORT.height,
        deviceScaleFactor: VIEWPORT.scale,
        mobile: false,
      });
      // Emulation lives as long as this CDP session: the page keeps it.
      return new PlaywrightPage(page, this.context, cdp);
    }
    return new PlaywrightPage(page, this.context);
  }

  async close(): Promise<void> {
    if (!this.attached) await this.context.close().catch(() => {});
  }
}

export async function createPlaywrightSession(): Promise<SiteSession> {
  return PlaywrightSession.create();
}

// Scratch dir for screencast frames and intermediate PNG bands.
export function scratchDir(prefix: string): string {
  return fs.mkdtempSync(`${os.tmpdir()}/${prefix}`);
}
