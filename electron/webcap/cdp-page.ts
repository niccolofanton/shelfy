// A PageDriver over a raw DevTools-protocol transport. Two transports exist:
//   • Electron webContents.debugger (electron-driver.ts);
//   • a flat-session WebSocket to a real Chrome (system-chrome.ts).
//
// "Stealth" mode is used on the user's real Chrome after an anti-bot check:
// anti-bot scripts detect automation through Runtime.enable side effects and
// patched natives, which would invalidate the clearance on the next page load.
// So in stealth mode this driver never enables the Runtime domain (evaluate
// works without it), injects no init scripts and does not emulate the screen.

import { isBlockedHostname } from '../net-safety';
import { getEngine, shouldBlock, cosmeticCss, JS_COSMETIC_FEATURES } from './blocker';
import { INIT_SCRIPT } from './scripts';
import {
  VIEWPORT,
  autoconsentScript,
  type NavResult,
  type NetEntry,
  type PageDriver,
  type ScreenshotOptions,
} from './driver';

export interface CdpTransport {
  send<T = Record<string, unknown>>(method: string, params?: Record<string, unknown>): Promise<T>;
  onEvent(listener: (method: string, params: Record<string, unknown>) => void): () => void;
}

export interface CdpPageOptions {
  stealth: boolean; // no Runtime.enable, no init scripts, no emulation
  fetchBlocking: boolean; // SSRF guard + content blocking through the Fetch domain
  reusable?: boolean; // one tab reused for every page: close() only resets per-page state
  // The tab already shows a page of this host (the user just passed an anti-bot
  // check there): the first goto() to that host adopts it instead of reloading.
  adoptHost?: string;
  onClose?: () => Promise<void>;
}

// Anti-bot / CAPTCHA providers: never content-blocked, the check must load intact.
export const CHALLENGE_HOSTS =
  /(^|\.)(challenges\.cloudflare\.com|hcaptcha\.com|recaptcha\.net|google\.com|gstatic\.com|captcha-delivery\.com|datadome\.co|px-cloud\.net|perimeterx\.net|px-cdn\.net|arkoselabs\.com|funcaptcha\.com)$/i;

const RESOURCE_TYPES: Record<string, string> = {
  Document: 'document',
  Stylesheet: 'stylesheet',
  Image: 'image',
  Media: 'media',
  Font: 'font',
  Script: 'script',
  XHR: 'xhr',
  Fetch: 'fetch',
  EventSource: 'eventsource',
  WebSocket: 'websocket',
  Manifest: 'manifest',
  Ping: 'ping',
  CSPViolationReport: 'cspreport',
  Other: 'other',
};

export class CdpPage implements PageDriver {
  readonly engine: 'playwright' | 'electron' | 'chrome';
  readonly viewport = { ...VIEWPORT };
  private net: NetEntry[] = [];
  private inflight = new Set<string>();
  private lastNetActivity = Date.now();
  private loadFired = false;
  private mainFrameId: string | null = null;
  private currentUrl = '';
  private mainResponse: { status: number; headers: Record<string, string>; mime: string } | null =
    null;
  private listeners = new Set<(method: string, params: Record<string, unknown>) => void>();
  private off: (() => void) | null = null;

  constructor(
    private readonly t: CdpTransport,
    private readonly opts: CdpPageOptions,
    engine: 'playwright' | 'electron' | 'chrome',
  ) {
    this.engine = engine;
  }

  async init(): Promise<void> {
    this.off = this.t.onEvent((method, params) => {
      this.track(method, params);
      for (const l of this.listeners) l(method, params);
    });
    await this.t.send('Page.enable');
    // Stealth: no Network domain — enabling it on a real Chrome is enough for
    // Cloudflare to revoke a fresh clearance (verified). Request data then comes
    // from the page's own Performance API.
    if (!this.opts.stealth) await this.t.send('Network.enable');
    const tree = await this.t.send<{ frameTree: { frame: { id: string; url: string } } }>(
      'Page.getFrameTree',
    );
    this.mainFrameId = tree.frameTree.frame.id;
    this.currentUrl = tree.frameTree.frame.url;
    if (!this.opts.stealth) {
      await this.t.send('Page.addScriptToEvaluateOnNewDocument', { source: INIT_SCRIPT });
      const consent = autoconsentScript();
      if (consent) await this.t.send('Page.addScriptToEvaluateOnNewDocument', { source: consent });
      await this.t.send('Emulation.setDeviceMetricsOverride', {
        width: VIEWPORT.width,
        height: VIEWPORT.height,
        deviceScaleFactor: VIEWPORT.scale,
        mobile: false,
      });
      await this.t.send('Page.setBypassCSP', { enabled: true }).catch(() => {});
    }
    if (this.opts.fetchBlocking && !this.opts.stealth) {
      const engine = await getEngine();
      this.listeners.add((method, params) => {
        if (method !== 'Fetch.requestPaused') return;
        const p = params as {
          requestId: string;
          request: { url: string };
          resourceType: string;
          frameId?: string;
        };
        let block = false;
        try {
          const u = new URL(p.request.url);
          if ((u.protocol === 'http:' || u.protocol === 'https:') && isBlockedHostname(u.hostname))
            block = true;
          else if (!CHALLENGE_HOSTS.test(u.hostname) && !u.pathname.startsWith('/cdn-cgi/')) {
            const isMain = p.resourceType === 'Document' && p.frameId === this.mainFrameId;
            block = shouldBlock(
              engine,
              p.request.url,
              this.currentUrl,
              RESOURCE_TYPES[p.resourceType] || 'other',
              isMain,
            );
          }
        } catch {}
        const cmd = block
          ? this.t.send('Fetch.failRequest', {
              requestId: p.requestId,
              errorReason: 'BlockedByClient',
            })
          : this.t.send('Fetch.continueRequest', { requestId: p.requestId });
        cmd.catch(() => {});
      });
      await this.t.send('Fetch.enable', {
        patterns: [{ urlPattern: '*', requestStage: 'Request' }],
      });
    }
  }

  private track(method: string, params: Record<string, unknown>): void {
    if (method === 'Network.requestWillBeSent') {
      this.inflight.add(String(params.requestId));
      this.lastNetActivity = Date.now();
    } else if (method === 'Network.loadingFinished' || method === 'Network.loadingFailed') {
      this.inflight.delete(String(params.requestId));
      this.lastNetActivity = Date.now();
    } else if (method === 'Network.responseReceived') {
      const r = (params.response || {}) as {
        url?: string;
        status?: number;
        mimeType?: string;
        headers?: Record<string, string>;
        encodedDataLength?: number;
      };
      const type = String(params.type || 'Other');
      if (type === 'Document' && !this.mainResponse && params.frameId === this.mainFrameId) {
        const headers: Record<string, string> = {};
        for (const [k, v] of Object.entries(r.headers || {})) headers[k.toLowerCase()] = String(v);
        this.mainResponse = {
          status: Number(r.status) || 0,
          headers,
          mime: String(r.mimeType || ''),
        };
      }
      if (this.net.length < 3000) {
        this.net.push({
          url: String(r.url || ''),
          type: RESOURCE_TYPES[type] || 'other',
          status: Number(r.status) || 0,
          mime: String(r.mimeType || ''),
          bytes: Number(r.encodedDataLength) || 0,
        });
      }
    } else if (method === 'Page.loadEventFired') {
      this.loadFired = true;
    } else if (method === 'Page.frameNavigated') {
      const f = (params.frame || {}) as { id?: string; parentId?: string; url?: string };
      if (!f.parentId && f.url) this.currentUrl = f.url;
    }
  }

  url(): string {
    return this.currentUrl;
  }

  async goto(url: string, timeoutMs: number): Promise<NavResult> {
    if (this.opts.adoptHost) {
      const host = this.opts.adoptHost;
      this.opts.adoptHost = undefined;
      const same = (u: string): string => {
        try {
          return new URL(u).hostname.replace(/^www\./, '');
        } catch {
          return '';
        }
      };
      if (same(this.currentUrl) === host && same(url) === host) {
        this.loadFired = true;
        const nav = await this.evaluate<{ status: number; type: string } | null>(
          `(() => { const n = performance.getEntriesByType('navigation')[0]; return { status: (n && n.responseStatus) || 200, type: document.contentType || 'text/html' }; })()`,
          { fallback: null, timeoutMs: 5_000 },
        );
        return {
          status: nav?.status || 200,
          headers: {},
          contentType: nav?.type || 'text/html',
          finalUrl: this.currentUrl,
        };
      }
    }
    this.loadFired = false;
    this.mainResponse = null;
    this.net = [];
    let resolveDom!: () => void;
    const domReady = new Promise<void>((r) => (resolveDom = r));
    const l = (m: string): void => {
      if (m === 'Page.domContentEventFired') resolveDom();
    };
    this.listeners.add(l);
    const timer = setTimeout(() => resolveDom(), timeoutMs);
    try {
      const res = await this.t.send<{ errorText?: string }>('Page.navigate', { url });
      if (res.errorText) throw new Error(res.errorText);
      await domReady;
    } finally {
      clearTimeout(timer);
      this.listeners.delete(l);
    }
    let r = this.mainResponse as {
      status: number;
      headers: Record<string, string>;
      mime: string;
    } | null;
    if (this.opts.stealth) {
      const nav = await this.evaluate<{ status: number; type: string } | null>(
        `(() => { const n = performance.getEntriesByType('navigation')[0]; return n ? { status: n.responseStatus || 0, type: (document.contentType || '') } : null; })()`,
        { fallback: null, timeoutMs: 5_000 },
      );
      if (nav) r = { status: nav.status || 200, headers: {}, mime: nav.type };
    }
    return {
      status: r ? r.status : null,
      headers: r ? r.headers : {},
      contentType: r ? r.mime.toLowerCase() : '',
      finalUrl: this.currentUrl || url,
    };
  }

  async waitForLoad(ms: number): Promise<void> {
    const t0 = Date.now();
    while (!this.loadFired && Date.now() - t0 < ms) await new Promise((r) => setTimeout(r, 150));
  }

  async waitForNetworkIdle(ms: number): Promise<void> {
    const t0 = Date.now();
    if (this.opts.stealth) {
      let last = -1;
      let stableSince = Date.now();
      while (Date.now() - t0 < ms) {
        const n = await this.evaluate<number>('performance.getEntriesByType("resource").length', {
          fallback: 0,
          timeoutMs: 2_000,
        });
        if (n !== last) {
          last = n;
          stableSince = Date.now();
        } else if (Date.now() - stableSince > 700) return;
        await new Promise((r) => setTimeout(r, 200));
      }
      return;
    }
    while (Date.now() - t0 < ms) {
      if (this.inflight.size <= 1 && Date.now() - this.lastNetActivity > 500) return;
      await new Promise((r) => setTimeout(r, 150));
    }
  }

  async evaluate<T>(
    expression: string,
    { timeoutMs = 15_000, fallback }: { timeoutMs?: number; fallback?: T } = {},
  ): Promise<T> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const res = await Promise.race([
        this.t.send<{ result?: { value?: unknown }; exceptionDetails?: unknown }>(
          'Runtime.evaluate',
          {
            expression,
            awaitPromise: true,
            returnByValue: true,
            userGesture: true,
          },
        ),
        new Promise<never>((_, rej) => {
          timer = setTimeout(() => rej(new Error('timeout')), timeoutMs);
        }),
      ]);
      if (res.exceptionDetails) return fallback as T;
      return (res.result?.value ?? fallback) as T;
    } catch {
      return fallback as T;
    } finally {
      clearTimeout(timer);
    }
  }

  async screenshot({ clip, fullPage = false }: ScreenshotOptions): Promise<Buffer> {
    const res = await this.t.send<{ data: string }>('Page.captureScreenshot', {
      format: 'png',
      fromSurface: true,
      captureBeyondViewport: !!fullPage,
      ...(clip ? { clip: { ...clip, scale: 1 } } : {}),
    });
    return Buffer.from(res.data, 'base64');
  }

  async wheel(deltaY: number): Promise<void> {
    await this.t.send('Input.dispatchMouseEvent', {
      type: 'mouseWheel',
      x: Math.round(VIEWPORT.width / 2),
      y: Math.round(VIEWPORT.height / 2),
      deltaX: 0,
      deltaY,
    });
  }

  async mouseMove(x: number, y: number): Promise<void> {
    await this.t.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
  }

  async pressKey(key: string): Promise<void> {
    for (const type of ['keyDown', 'keyUp']) {
      await this.t
        .send('Input.dispatchKeyEvent', {
          type,
          key,
          code: key,
          windowsVirtualKeyCode: key === 'Escape' ? 27 : 0,
        })
        .catch(() => {});
    }
  }

  async startScreencast(
    onFrame: (jpeg: Buffer, tsSec: number) => void,
    { maxWidth, maxHeight, quality }: { maxWidth: number; maxHeight: number; quality: number },
  ): Promise<() => Promise<void>> {
    const l = (method: string, params: Record<string, unknown>): void => {
      if (method !== 'Page.screencastFrame') return;
      const p = params as { data: string; sessionId: number; metadata?: { timestamp?: number } };
      try {
        onFrame(Buffer.from(p.data, 'base64'), p.metadata?.timestamp ?? Date.now() / 1000);
      } catch {}
      this.t.send('Page.screencastFrameAck', { sessionId: p.sessionId }).catch(() => {});
    };
    this.listeners.add(l);
    await this.t.send('Page.startScreencast', {
      format: 'jpeg',
      quality,
      maxWidth,
      maxHeight,
      everyNthFrame: 1,
    });
    return async () => {
      await this.t.send('Page.stopScreencast').catch(() => {});
      this.listeners.delete(l);
    };
  }

  async applyCosmetics(): Promise<number> {
    const engine = await getEngine();
    if (!engine) return 0;
    const features = await this.evaluate<{
      ids: string[];
      classes: string[];
      hrefs: string[];
    } | null>(JS_COSMETIC_FEATURES, {
      fallback: null,
    });
    const css = cosmeticCss(engine, this.url(), features);
    if (!css) return 0;
    const ok = await this.evaluate<boolean>(
      `(() => { try { const s = document.createElement('style'); s.setAttribute('data-shelfy', 'cosmetic'); s.textContent = ${JSON.stringify(css)}; (document.head || document.documentElement).appendChild(s); return true; } catch (e) { return false; } })()`,
      { fallback: false },
    );
    return ok ? 1 : 0;
  }

  async network(): Promise<NetEntry[]> {
    if (!this.opts.stealth) return this.net.slice();
    const entries = await this.evaluate<
      { url: string; type: string; status: number; bytes: number }[]
    >(
      `(() => performance.getEntriesByType('resource').slice(0, 3000).map((e) => ({ url: e.name, type: /\\.(woff2?|ttf|otf)(\\?|$)/i.test(e.name) ? 'font' : e.initiatorType === 'script' ? 'script' : e.initiatorType === 'css' || e.initiatorType === 'link' ? (/\\.css(\\?|$)/i.test(e.name) ? 'stylesheet' : 'other') : e.initiatorType, status: e.responseStatus || 0, bytes: e.transferSize || 0 })))()`,
      { fallback: [], timeoutMs: 8_000 },
    );
    return entries.map((e) => ({ ...e, mime: '' }));
  }

  async close(): Promise<void> {
    if (this.opts.reusable) {
      this.net = [];
      return;
    }
    this.off?.();
    this.off = null;
    this.listeners.clear();
    await this.opts.onClose?.();
  }
}
