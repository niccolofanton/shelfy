// Web-reference capture engine — PLAYWRIGHT backend (replaces the Electron
// offscreen-rendering path for WebGL/canvas-heavy reference sites).
//
// WHY: Electron `offscreen:true` cannot reliably composite WebGL into the surface
// that `Page.captureScreenshot` reads (electron#39859, #11439). Sites like
// lusion.co render their ENTIRE hero + project thumbnails in WebGL (OGL/Three.js),
// so the OSR path produced screenshots where every visual was blank white while
// only the DOM text/CSS captured. A real headless Chromium (Playwright) composites
// WebGL via ANGLE/SwiftShader and captures it correctly.
//
// CONTRACT PARITY: capturePage() here returns the SAME live pageCtx shape as
// webcapture.capturePage(), so weborchestrator.js and web-enrich.js consume it
// unchanged:
//   { screenshotPath, width, height, finalUrl, title, capped, html, headers,
//     evaluate(codeString) -> Promise<json>, dispose() }
//
// It REUSES webcapture.js internals (ffmpeg encode, on-disk path layout, and the
// in-page DOM-prep scripts for cookie dismissal / animation kill / virtual-scroll
// neutralization / fixed-element flattening) so both engines behave identically
// off the GPU axis.
//
// SSRF: every navigation target is gated by net-safety.assertSafeUrl, exactly like
// the OSR engine. Auth: cookies from the persist:social Electron session are
// mirrored into the Playwright context so logged-in IG/X reference pages render.

import path from 'path';
import fs from 'fs';
import os from 'os';
import { execFile } from 'child_process';
import type { ChildProcess } from 'child_process';
import { session } from 'electron';
import type { Page, Route, Request as PwRequest } from 'playwright-core';
import { assertSafeUrl, isBlockedHostname } from './net-safety';
import * as webcapture from './webcapture';
import { attachAdblock } from './adblock';
// The shared headless-Chromium lifecycle moved to webcap/browser.ts (SPIKE-11),
// so the capture service can launch a browser without this v1 engine. Re-exported
// below; the desktop keeps one shared browser process.
import { getBrowser, closeBrowser, getChromium } from './webcap/browser';

// One vertical band of a captured page (encodeImageChunks / encodeFrames output).
interface Chunk {
  screenshotPath: string;
  width: number;
  height: number;
}

// Per-capture options (parity with the OSR engine's capturePage). All optional.
interface CapturePageOptions {
  partition?: string;
  maxHeightPx?: number;
  format?: string;
  quality?: number;
  signal?: AbortSignal;
  onStep?: (label: string, delta: number, tot: number) => void;
  settleBeforeShotMs?: number;
  readyMaxMs?: number;
  captureStamp?: number;
}

// The live pageCtx contract returned by capturePage (mirror of webcapture's).
interface PageCtx {
  screenshotPath: string | null;
  chunks: Chunk[];
  width: number;
  height: number;
  finalUrl: string;
  title: string;
  capped: boolean;
  html: string;
  headers: Record<string, string>;
  webglHeavy: boolean;
  evaluate: (code: string) => Promise<unknown>;
  dispose: () => Promise<void>;
}

// Shape of webcapture.js' _internals bag reused here. Declared locally so the
// destructured bindings carry real types regardless of how webcapture types it.
interface WebcaptureInternals {
  encodeImageChunks: (
    srcPng: string,
    finalUrl: string,
    format: string,
    quality: number,
    opts?: { stamp?: number; chunkHeight?: number; signal?: AbortSignal },
  ) => Promise<Chunk[]>;
  encodeFrames: (
    framePngs: string[],
    finalUrl: string,
    format: string,
    quality: number,
    opts?: { stamp?: number; signal?: AbortSignal },
  ) => Promise<Chunk[]>;
  resolveFfmpeg: () => string;
  PARTITION: string;
  DESKTOP_UA: string;
  VIEWPORT_W: number;
  VIEWPORT_H: number;
  DEFAULT_MAX_HEIGHT: number;
  JS_DISMISS_COOKIES: string;
  JS_DISABLE_ANIMATIONS: string;
  JS_NEUTRALIZE_VIRTUAL_SCROLL: string;
  JS_FORCE_SCROLL_ANIM_END: string;
  JS_NEUTRALIZE_FIXED: string;
  JS_DETECT_CANVAS: string;
  jsAutoScroll: (maxHeightPx: number) => string;
}

const {
  encodeImageChunks,
  encodeFrames,
  resolveFfmpeg,
  PARTITION,
  DESKTOP_UA,
  VIEWPORT_W,
  VIEWPORT_H,
  DEFAULT_MAX_HEIGHT,
  JS_DISMISS_COOKIES,
  JS_DISABLE_ANIMATIONS,
  JS_NEUTRALIZE_VIRTUAL_SCROLL,
  JS_FORCE_SCROLL_ANIM_END,
  JS_NEUTRALIZE_FIXED,
  JS_DETECT_CANVAS,
  jsAutoScroll,
} = webcapture._internals as WebcaptureInternals;

// ─── Constants ──────────────────────────────────────────────────────────────

const NAV_TIMEOUT_MS = 45_000;
const NETWORK_IDLE_MS = 20_000; // best-effort settle on quiet network
const SETTLE_MS = 600;
const SCROLL_EVAL_TIMEOUT_MS = 30_000;
// Wait out a full-screen loading/preloader overlay before capturing (so we never
// shoot the intro counter — common on WebGL/awwwards sites, e.g. lusion's
// #preloader runs ~10s, longer under parallel SwiftShader contention). Capped.
const READY_MAX_MS = 30_000;
const READY_POLL_MS = 500;
const SHOT_TIMEOUT_MS = 30_000;

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
function sleepAbortable(ms: number, signal?: AbortSignal): Promise<void> {
  if (!signal) return sleep(ms);
  const sig = signal;
  if (sig.aborted) return Promise.resolve();
  return new Promise((resolve) => {
    const t = setTimeout(done, ms);
    function done(): void {
      clearTimeout(t);
      sig.removeEventListener('abort', done);
      resolve();
    }
    sig.addEventListener('abort', done, { once: true });
  });
}
function throwIfAborted(signal?: AbortSignal): void {
  if (signal && signal.aborted) {
    throw Object.assign(new Error('AbortError'), { name: 'AbortError' });
  }
}

// ─── Cookie bridge: persist:social → Playwright context ──────────────────────

// A cookie shaped for Playwright's context.addCookies().
interface PwCookie {
  name: string;
  value: string;
  domain: string;
  path: string;
  httpOnly: boolean;
  secure: boolean;
  sameSite: 'Strict' | 'Lax' | 'None';
  expires?: number;
}

// Electron sameSite ('no_restriction'|'lax'|'strict'|'unspecified') → Playwright
// ('None'|'Lax'|'Strict'). Playwright requires Strict/Lax/None; default to Lax.
function mapSameSite(s: string | undefined): 'Strict' | 'Lax' | 'None' {
  switch (String(s || '').toLowerCase()) {
    case 'no_restriction':
      return 'None';
    case 'strict':
      return 'Strict';
    case 'lax':
      return 'Lax';
    default:
      return 'Lax';
  }
}

// Pull cookies from the logged-in Electron session and shape them for
// context.addCookies(). Best-effort: any malformed cookie is skipped, never
// fatal — an unauthenticated capture still renders public reference sites.
async function cookiesForPlaywright(partition: string): Promise<PwCookie[]> {
  try {
    const ses = session.fromPartition(partition);
    const all = await ses.cookies.get({});
    const out: PwCookie[] = [];
    for (const c of all) {
      if (!c || !c.name || !c.domain) continue;
      const sameSite = mapSameSite(c.sameSite);
      // SameSite=None cookies MUST be Secure or Chromium rejects addCookies().
      const secure = sameSite === 'None' ? true : !!c.secure;
      const entry: PwCookie = {
        name: c.name,
        value: c.value || '',
        domain: c.domain,
        path: c.path || '/',
        httpOnly: !!c.httpOnly,
        secure,
        sameSite,
      };
      if (typeof c.expirationDate === 'number' && c.expirationDate > 0) {
        entry.expires = Math.floor(c.expirationDate);
      }
      out.push(entry);
    }
    return out;
  } catch {
    return [];
  }
}

// ─── Best-effort in-page eval (mirror of webcapture.safeEval) ────────────────
async function safeEval<T>(
  page: Page,
  code: string,
  { ms = 15_000, fallback }: { ms?: number; fallback?: T } = {},
): Promise<T> {
  let t: ReturnType<typeof setTimeout> | undefined;
  try {
    return (await Promise.race([
      page.evaluate(code),
      new Promise<never>((_, rej) => {
        t = setTimeout(() => rej(new Error('eval timeout')), ms);
      }),
    ])) as T;
  } catch {
    return fallback as T;
  } finally {
    clearTimeout(t);
  }
}

// True while a full-screen loading/preloader/splash overlay is still visible. Matches
// the common naming conventions (load/preload/loader/splash in class or id) AND
// requires the element to be a fixed/absolute overlay covering most of the viewport,
// so a small inline spinner or a content section never counts. Used to wait out the
// intro on WebGL/awwwards sites instead of shooting the loading counter.
const JS_LOADER_VISIBLE = `(() => {
  try {
    const vw = innerWidth, vh = innerHeight;
    const sel = '[class*=preload i],[id*=preload i],[class*=loader i],[id*=loader i],[class*=loading i],[id*=loading i],[class*=splash i],[id*=splash i]';
    for (const el of document.querySelectorAll(sel)) {
      const s = getComputedStyle(el);
      if (s.display === 'none' || s.visibility === 'hidden' || parseFloat(s.opacity || '1') === 0) continue;
      if (s.position !== 'fixed' && s.position !== 'absolute') continue;
      const r = el.getBoundingClientRect();
      if (r.width >= vw * 0.8 && r.height >= vh * 0.8) return true;
    }
    return false;
  } catch (e) { return false; }
})()`;

// Poll until no full-screen loader overlay remains (or maxMs elapses). Returns the
// time spent waiting. Resolves immediately on a normal page (no loader → no wait).
async function waitForReady(
  page: Page,
  { maxMs = READY_MAX_MS, signal }: { maxMs?: number; signal?: AbortSignal } = {},
): Promise<number> {
  const t0 = Date.now();
  for (;;) {
    if (signal && signal.aborted) return Date.now() - t0;
    let present: unknown = false;
    try {
      present = await page.evaluate(JS_LOADER_VISIBLE);
    } catch {
      present = false;
    }
    if (!present || Date.now() - t0 >= maxMs) return Date.now() - t0;
    await sleep(READY_POLL_MS);
  }
}

// ─── Scroll-jacked WebGL experiences (filmstrip capture) ─────────────────────
//
// Some award-winning sites (lusion.co et al.) are a SINGLE locked viewport: the
// document has overflow:hidden / height:100vh, a fixed full-screen WebGL canvas,
// and the whole "scroll" is a wheel listener that advances an animation timeline
// INSIDE the canvas. There is no tall static DOM to tile (documentElement.scrollHeight
// stays ≈ viewport) and no readable scroll progress — so fullPage captures only the
// hero. The ONLY way to see the rest is to DRIVE the wheel and grab a frame per step,
// then stack the frames (a "filmstrip" of the scroll journey). Each frame becomes a
// chunk, so the existing lightbox/AI pipeline consumes it unchanged.

// Cap the journey: lusion's experience never reaches a static end (it's open/looping),
// so a frame cap bounds it. ~12 viewport frames cover hero → about → reel → featured work.
const JOURNEY_MAX_FRAMES = 12;
const JOURNEY_SETTLE_MS = 950; // let the snapped section ease to rest before the shot
const JOURNEY_WHEEL_DELTA = 1500; // ≈1.6× viewport: ~one section per step, low redundancy
// Mean per-pixel gray diff (0-255) of a 16×16 signature below which two frames are
// "the same". Used to (a) bail if the FIRST wheel changes nothing → not a journey, a
// genuinely static page misdetected as jacked; (b) stop once the experience clamps.
const JOURNEY_SAME_DIFF = 6;

// Whether the document is LOCKED from scrolling (overflow hidden/clip AND no tall
// content). Read EARLY — before JS_DISMISS_COOKIES, which force-sets html/body
// overflow:auto to unlock a banner and would otherwise erase this signal.
const JS_DOC_LOCKED = `(() => {
  try {
    const d = document.documentElement, b = document.body;
    const ov = (getComputedStyle(d).overflowY + ' ' + (b ? getComputedStyle(b).overflowY : '')).toLowerCase();
    const docScrolls = d.scrollHeight > innerHeight * 1.5;
    return /hidden|clip/.test(ov) && !docScrolls;
  } catch (e) { return false; }
})()`;

// A large fixed/absolute <canvas> covering most of the viewport AND no tall scrollable
// content — the WebGL-experience fingerprint. Combined with the early lock signal, this
// identifies a scroll-jacked single-viewport experience (lusion-type).
const JS_HAS_BIG_FIXED_CANVAS = `(() => {
  try {
    const d = document.documentElement;
    if (d.scrollHeight > innerHeight * 1.5) return false; // tall DOM ⇒ normal scroll
    for (const c of document.querySelectorAll('canvas')) {
      const r = c.getBoundingClientRect(), s = getComputedStyle(c);
      if (r.width >= innerWidth * 0.6 && r.height >= innerHeight * 0.6 &&
          (s.position === 'fixed' || s.position === 'absolute')) return true;
    }
    return false;
  } catch (e) { return false; }
})()`;

// 16×16 grayscale signature of a PNG via ffmpeg, for a cheap perceptual frame diff
// (the WebGL canvas isn't reliably readable in-page, so we diff the screenshots).
function frameSignature(png: string, signal?: AbortSignal): Promise<Buffer | null> {
  return new Promise((resolve) => {
    const child: ChildProcess = execFile(
      resolveFfmpeg(),
      [
        '-hide_banner',
        '-loglevel',
        'error',
        '-i',
        png,
        '-vf',
        'scale=16:16,format=gray',
        '-f',
        'rawvideo',
        '-',
      ],
      { maxBuffer: 1 << 20, encoding: 'buffer' },
      (err, stdout) => resolve(err ? null : (stdout as Buffer)),
    );
    if (signal) {
      signal.addEventListener(
        'abort',
        () => {
          try {
            child.kill();
          } catch {}
          resolve(null);
        },
        { once: true },
      );
    }
  });
}
function meanDiff(a: Buffer | null, b: Buffer | null): number {
  if (!a || !b || a.length === 0 || a.length !== b.length) return 255;
  let s = 0;
  for (let i = 0; i < a.length; i++) s += Math.abs(a[i] - b[i]);
  return s / a.length;
}

// Drive the wheel and grab a viewport frame per step, stopping when the experience
// clamps (two near-identical frames) or the cap is hit. Returns ORDERED png paths
// (caller encodes them to chunks). Bails to a single frame if the first wheel does
// nothing (page wasn't actually a journey). Best-effort: a failed shot ends the run.
async function captureScrollJourney(
  page: Page,
  tmpDir: string,
  {
    maxFrames = JOURNEY_MAX_FRAMES,
    settleMs = JOURNEY_SETTLE_MS,
    deltaY = JOURNEY_WHEEL_DELTA,
    signal,
    mark,
  }: {
    maxFrames?: number;
    settleMs?: number;
    deltaY?: number;
    signal?: AbortSignal;
    mark?: (label: string) => void;
  } = {},
): Promise<string[]> {
  const frames: string[] = [];
  let prevSig: Buffer | null = null;
  // Center the cursor so the wheel lands on the experience, not a corner widget.
  try {
    await page.mouse.move(Math.floor(VIEWPORT_W / 2), Math.floor(VIEWPORT_H / 2));
  } catch {}
  for (let i = 0; i < maxFrames; i++) {
    throwIfAborted(signal);
    const png = path.join(tmpDir, `journey-${String(i).padStart(2, '0')}.png`);
    try {
      await page.screenshot({
        path: png,
        type: 'png',
        animations: 'disabled',
        timeout: SHOT_TIMEOUT_MS,
      });
    } catch {
      break;
    }
    const sig = await frameSignature(png, signal);
    const diff = prevSig ? meanDiff(prevSig, sig) : 999;
    if (prevSig && diff < JOURNEY_SAME_DIFF) {
      // First wheel did nothing → static page, not a journey: keep only the hero.
      if (i === 1) {
        frames.length = 1;
        mark?.(`journey-static(1)`);
        break;
      }
      // Otherwise the experience clamped (reached its end): stop, the dup tail is dropped.
      mark?.(`journey-end(${frames.length})`);
      break;
    }
    frames.push(png);
    prevSig = sig;
    if (i < maxFrames - 1) {
      try {
        await page.mouse.wheel(0, deltaY);
      } catch {
        break;
      }
      await sleepAbortable(settleMs, signal);
    }
  }
  mark?.(`journey(${frames.length} frames)`);
  return frames;
}

// ─── F2: capture (Playwright) ────────────────────────────────────────────────

/**
 * Render the URL in a headless Chromium and return a LIVE pageCtx (parity with
 * webcapture.capturePage). The page/context stay alive until dispose().
 */
async function capturePage(
  url: string,
  {
    partition = PARTITION,
    maxHeightPx = DEFAULT_MAX_HEIGHT,
    format = 'webp',
    quality = 82,
    signal,
    onStep,
    // Post-ready WebGL settle. The heavy lifting is now waitForReady() (waits out the
    // loading/preloader overlay), so this is just a short grace for the hero's first
    // good frame after the intro clears. The orchestrator raises it on a re-capture.
    settleBeforeShotMs = 4_000,
    // Cap for waiting out a loading overlay before the shot (raised on re-capture).
    readyMaxMs = READY_MAX_MS,
    captureStamp,
  }: CapturePageOptions = {},
): Promise<PageCtx> {
  throwIfAborted(signal);
  assertSafeUrl(url); // reject non-http(s) / blocked host before navigating

  const browser = await getBrowser();
  const cookies = await cookiesForPlaywright(partition);

  const context = await browser.newContext({
    viewport: { width: VIEWPORT_W, height: VIEWPORT_H },
    deviceScaleFactor: 1, // 1× output (DPR-independent), like the OSR clip.scale:1
    userAgent: DESKTOP_UA,
    // Deliberately NO reducedMotion:'reduce': motion-heavy reference sites honor
    // prefers-reduced-motion by DISABLING the very animations/WebGL we want in the
    // reference shot (some even swap to an alternate static layout) — the capture
    // would show a degraded, unrepresentative site. The intro/preloader is waited
    // out by waitForReady() instead.
    ignoreHTTPSErrors: false,
  });
  if (cookies.length) {
    try {
      await context.addCookies(cookies);
    } catch {
      /* a bad cookie set shouldn't block a public capture */
    }
  }

  // Ad/tracker blocking (faster, cleaner screenshots, fewer consent overlays).
  const adblock = await attachAdblock(context);

  const page = await context.newPage();
  page.setDefaultTimeout(NAV_TIMEOUT_MS);

  // SSRF: gate EVERY request — navigations (including the post-redirect chain) AND
  // sub-resources — on the blocklist, mirroring webcapture.fetchText's validation.
  // The initial assertSafeUrl above only vetted the pasted URL; a public host that
  // 30x-redirects to an internal/metadata IP would otherwise be fully rendered, and
  // a page could probe loopback/private hosts via <img>/fetch/script sub-resources.
  // isBlockedHostname only matches loopback/private/link-local/metadata hosts, so
  // legitimate public CDNs/assets are never affected.
  // Registered after newPage() but before any goto(), so it gates the very first
  // navigation and every redirect hop in the chain. This handler is registered
  // AFTER attachAdblock's, and Playwright runs route handlers in reverse order
  // (LIFO): we must defer with route.fallback() (NOT route.continue(), which would
  // resolve the route and bypass the adblock handler entirely for every request).
  try {
    await context.route('**/*', (route: Route, request: PwRequest) => {
      try {
        if (isBlockedHostname(new URL(request.url()).hostname))
          return route.abort('blockedbyclient');
      } catch {
        /* never let the guard itself break a navigation */
      }
      return route.fallback();
    });
  } catch {
    /* routing unavailable → fall back to the post-navigation host check below */
  }

  // Never let a stray asset/manifest trigger a download (no "Save As" possible in
  // headless, but it would still error the navigation). Abort download responses.
  page.on('download', (d) => {
    try {
      d.cancel();
    } catch {}
  });

  let disposed = false;
  const dispose = async (): Promise<void> => {
    if (disposed) return;
    disposed = true;
    try {
      await context.close();
    } catch {
      /* already closed */
    }
  };

  // Abort plumbing: tear the context down if the caller cancels.
  const onAbort = (): void => {
    dispose().catch(() => {});
  };
  if (signal) {
    if (signal.aborted) {
      await dispose();
      throw Object.assign(new Error('AbortError'), { name: 'AbortError' });
    }
    signal.addEventListener('abort', onAbort, { once: true });
  }

  const T0 = Date.now();
  let lastMs = 0;
  const mark = (label: string): void => {
    const ms = Date.now() - T0;
    try {
      onStep?.(label, ms - lastMs, ms);
    } catch {}
    lastMs = ms;
  };

  try {
    mark('start');

    // 1) LOAD — capture the main response headers for tech detection.
    let navHeaders: Record<string, string> = {};
    const resp = await page.goto(url, { waitUntil: 'load', timeout: NAV_TIMEOUT_MS });
    if (resp) {
      try {
        navHeaders = resp.headers() || {};
      } catch {}
    }
    mark('loaded');
    throwIfAborted(signal);

    // Best-effort: let the network go quiet (lazy chunks/textures) before prepping.
    await page.waitForLoadState('networkidle', { timeout: NETWORK_IDLE_MS }).catch(() => {});
    mark(`networkidle(adblock:${adblock.blockedCount()})`);

    const finalUrl = page.url() || url;
    // Belt-and-suspenders SSRF check: even with the route guard, re-validate the
    // RESOLVED host before harvesting DOM/headers/screenshot — a redirect that
    // landed on an internal/metadata host must never be captured.
    try {
      if (isBlockedHostname(new URL(finalUrl).hostname)) {
        throw new Error(`Blocked redirect host: ${new URL(finalUrl).hostname}`);
      }
    } catch (e) {
      if (
        /Blocked redirect host/.test((e as { message?: string } | null | undefined)?.message || '')
      )
        throw e;
      /* unparsable finalUrl → fall through; the route guard already gated nav */
    }
    await sleep(SETTLE_MS);

    // Snapshot the document's scroll-lock NOW — JS_DISMISS_COOKIES (next) force-unlocks
    // overflow, which would erase the signal that flags a scroll-jacked experience.
    const earlyLocked = await safeEval<boolean>(page, JS_DOC_LOCKED, {
      fallback: false,
      ms: 5_000,
    });

    // 2) DISMISS COOKIE BANNER (reuse the OSR engine's 40+ selector script).
    await safeEval(page, JS_DISMISS_COOKIES);
    mark('cookies-dismissed');
    await sleep(SETTLE_MS);

    // 3) WAIT OUT a loading/preloader overlay (intro counter on WebGL/awwwards
    // sites) — with transitions still ALIVE: this must run BEFORE the animation
    // kill, because `transition:none` suppresses transitionend/animationend and
    // preloaders dismissed on those events (a common CSS pattern) then never
    // leave → the wait runs to its cap and the shot shows the loader. A fixed 9s
    // settle was unreliable (lusion's #preloader runs ~10s, and longer under
    // parallel contention). No loader → returns immediately.
    const readyWaited = await waitForReady(page, { maxMs: readyMaxMs, signal });
    mark(`ready(${readyWaited}ms)`);
    throwIfAborted(signal);

    // 3a) KILL ANIMATIONS / TRANSITIONS, freeze autoplay media. Videos have had
    // load+ready time to advance past their (often black) first frame, and the
    // script nudges currentTime before pausing.
    await safeEval(page, JS_DISABLE_ANIMATIONS);
    mark('animations-killed');

    // Cheap canvas/WebGL probe, exposed on the pageCtx: the orchestrator lowers
    // per-site parallelism on WebGL-heavy sites (N concurrent Three.js/OGL scenes
    // contend for one GPU: slower intros, timeouts, GPU-process crash risk).
    const canvasInfo = await safeEval<{ canvas?: boolean; webgl?: boolean; big?: boolean }>(
      page,
      JS_DETECT_CANVAS,
      {
        fallback: { canvas: false },
        ms: 5_000,
      },
    );
    const webglHeavy = !!(canvasInfo && canvasInfo.canvas && (canvasInfo.webgl || canvasInfo.big));

    // 3a-bis) SCROLL-JACKED WebGL EXPERIENCE? (lusion-type: locked viewport, fixed
    // canvas, wheel-driven timeline with no scrollable DOM). Detect BEFORE the
    // virtual-scroll neutralizer (which forces overflow:visible and would mask the
    // signal). If so, capture a FILMSTRIP of the scroll journey instead of a single
    // hero — driving the wheel is the only way to reveal the rest of the experience.
    const bigCanvas = await safeEval<boolean>(page, JS_HAS_BIG_FIXED_CANVAS, {
      fallback: false,
      ms: 5_000,
    });
    const jacked = earlyLocked && bigCanvas;
    if (jacked) {
      mark('scroll-jacked');
      // Grab the LIVE DOM now (the journey mutates wheel state, not the DOM, but the
      // hero DOM is the richest text for tagging).
      const html = await safeEval<string>(page, 'document.documentElement.outerHTML', {
        fallback: '',
      });
      const title = (await safeEval<string>(page, 'document.title', { fallback: '' })) || '';
      mark(`html(${(html || '').length})`);

      const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-pwj-'));
      try {
        const framePngs = await captureScrollJourney(page, tmpDir, { signal, mark });
        // Two discovered URLs can redirect to the SAME finalUrl, which would make
        // concurrent captures resolve to identical on-disk frame paths and race.
        // The per-capture tmpDir is unique, so fold its basename into the key (same
        // scheme as webcapture.js' single-shot fix). Transparent: callers consume
        // chunks[].screenshotPath directly, never re-deriving it from finalUrl.
        const chunks = await encodeFrames(
          framePngs,
          `${finalUrl}#cap=${path.basename(tmpDir)}`,
          format,
          quality,
          { stamp: captureStamp, signal },
        );
        const screenshotPath = chunks[0]?.screenshotPath || null;
        const width = chunks[0]?.width || VIEWPORT_W;
        const height = chunks.reduce((sum, c) => sum + (c.height || 0), 0);
        mark(`encoded(${width}x${height}, ${chunks.length} frame${chunks.length > 1 ? 's' : ''})`);
        return {
          screenshotPath,
          chunks,
          width,
          height,
          finalUrl,
          title,
          capped: false,
          html,
          headers: navHeaders,
          webglHeavy,
          evaluate: (code: string): Promise<unknown> => {
            if (disposed) return Promise.reject(new Error('pageCtx disposed'));
            if (typeof code !== 'string')
              return Promise.reject(new Error('evaluate expects a code string'));
            return page.evaluate(code);
          },
          dispose,
        };
      } finally {
        try {
          fs.rmSync(tmpDir, { recursive: true, force: true });
        } catch {}
      }
    }

    // 3b) NEUTRALIZE VIRTUALIZED SCROLL (GSAP ScrollSmoother / Locomotive / bespoke)
    //     so the page height is real and window.scrollTo actually moves content.
    const vscroll = await safeEval<{ libs?: string[] }>(page, JS_NEUTRALIZE_VIRTUAL_SCROLL, {
      fallback: { libs: [] },
    });
    mark(`virtual-scroll(${(vscroll && vscroll.libs && vscroll.libs.join(',')) || 'none'})`);
    await sleep(SETTLE_MS);

    // 4) SCROLL for lazy-load + on-scroll WebGL scenes, then back to top.
    await safeEval(page, jsAutoScroll(maxHeightPx), { ms: SCROLL_EVAL_TIMEOUT_MS });
    mark('scrolled');
    // 4a) GSAP/ScrollTrigger reveals REVERSED on the return to top (neither the
    // CSS kill nor screenshot animations:'disabled' touches GSAP, and the
    // full-page grab performs no real scrolling) → force them to their END state
    // so below-the-fold sections aren't captured at opacity:0.
    const fin = await safeEval<{ st?: number; tweens?: number }>(page, JS_FORCE_SCROLL_ANIM_END, {
      fallback: { st: 0, tweens: 0 },
    });
    if (fin && (fin.st || fin.tweens)) mark(`gsap-finished(st=${fin.st},tw=${fin.tweens})`);
    await sleep(SETTLE_MS);
    throwIfAborted(signal);

    // 4b) PRE-SHOT WAIT — fixed grace for slow WebGL heroes / hydration / lazy media.
    if (settleBeforeShotMs > 0) {
      await sleepAbortable(settleBeforeShotMs, signal);
      mark(`pre-shot-wait(${settleBeforeShotMs}ms)`);
      throwIfAborted(signal);
    }

    // Grab the LIVE rendered DOM (post-hydration) for content/tech extraction.
    const html = await safeEval<string>(page, 'document.documentElement.outerHTML', {
      fallback: '',
    });
    const title = (await safeEval<string>(page, 'document.title', { fallback: '' })) || '';
    mark(`html(${(html || '').length})`);

    // 5) FLATTEN fixed/sticky (so a sticky header isn't repeated down the full page),
    //    return to the top, then capture.
    await safeEval(page, JS_NEUTRALIZE_FIXED);
    await safeEval(page, 'window.scrollTo(0, 0)');
    await sleep(SETTLE_MS);
    throwIfAborted(signal);
    mark('pre-capture');

    // Page height decision. documentElement.scrollHeight is the HONEST flowed-content
    // height: after JS_NEUTRALIZE_VIRTUAL_SCROLL a Locomotive/ScrollSmoother site
    // reports its full height (→ full capture), while a bespoke virtual-scroll site
    // like lusion.co reports ~viewport because the rest is a scroll-DRIVEN WebGL
    // experience with no static tall DOM (its 50k "JS_MEASURE" height is just the
    // virtual scrollbar SPACER — capturing it would yield a mostly-blank tail). So we
    // trust documentElement here and let Playwright's fullPage tile the real content.
    const docHeight = Number(
      await safeEval<number>(page, 'Math.ceil(document.documentElement.scrollHeight)', {
        fallback: VIEWPORT_H,
      }),
    );
    const scrollHeight = Math.max(VIEWPORT_H, docHeight);
    const capped = scrollHeight > maxHeightPx;
    mark(`measured(h=${scrollHeight}${capped ? `→${maxHeightPx}` : ''})`);

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-pw-'));
    const tmpPng = path.join(tmpDir, 'capture.png');
    try {
      try {
        if (capped) {
          // Tall page beyond the app cap: fullPage TILES the real content with the
          // layout viewport untouched, and the clip crops the tiling to the top
          // maxHeightPx (verified: fullPage+clip → 1280×maxHeightPx; clip alone is
          // clamped to the viewport). The legacy approach — growing the viewport to
          // 12000px — re-rendered every 100vh section at 12000px (stretched hero,
          // all scroll triggers fired at once, broken WebGL camera aspect): exactly
          // the vh distortion the OSR engine's captureBeyondViewport was chosen to
          // avoid. Kept only as fallback if the tall tiling fails (e.g. SwiftShader
          // refusing large surfaces).
          try {
            await page.screenshot({
              path: tmpPng,
              type: 'png',
              fullPage: true,
              animations: 'disabled',
              timeout: SHOT_TIMEOUT_MS,
              clip: { x: 0, y: 0, width: VIEWPORT_W, height: maxHeightPx },
            });
          } catch {
            await page.setViewportSize({ width: VIEWPORT_W, height: maxHeightPx });
            await sleep(SETTLE_MS);
            await page.screenshot({
              path: tmpPng,
              type: 'png',
              animations: 'disabled',
              timeout: SHOT_TIMEOUT_MS,
              clip: { x: 0, y: 0, width: VIEWPORT_W, height: maxHeightPx },
            });
          }
        } else {
          // fullPage tiles internally and is bounded by documentElement.scrollHeight —
          // exactly the honest flowed height we want.
          await page.screenshot({
            path: tmpPng,
            type: 'png',
            fullPage: true,
            animations: 'disabled',
            timeout: SHOT_TIMEOUT_MS,
          });
        }
        mark(`captured(pw${capped ? ',capped' : ''})`);
      } catch (err) {
        mark(`pw-shot-failed(${(err as { message?: string } | null | undefined)?.message || err})`);
        throw err;
      }

      // 6) ENCODE → WebP/PNG, SLICED into ≤CHUNK_HEIGHT vertical bands so the renderer
      // shows several light images instead of one heavyweight full-page frame (the
      // report-modal lightbox would otherwise decode a single 1280×12000 image). A short
      // page yields a single chunk at the canonical path — no change for normal sites.
      // Per-capture nonce (unique tmpDir basename) so concurrent captures that
      // redirect to the same finalUrl don't race on identical chunk paths — see
      // the encodeFrames call above and webcapture.js' single-shot fix.
      const chunks = await encodeImageChunks(
        tmpPng,
        `${finalUrl}#cap=${path.basename(tmpDir)}`,
        format,
        quality,
        { stamp: captureStamp, signal },
      );
      const screenshotPath = chunks[0]?.screenshotPath || null;
      const width = chunks[0]?.width || VIEWPORT_W;
      const height = chunks.reduce((sum, c) => sum + (c.height || 0), 0);
      mark(`encoded(${width}x${height}, ${chunks.length} chunk${chunks.length > 1 ? 's' : ''})`);

      return {
        screenshotPath,
        chunks,
        width,
        height,
        finalUrl,
        title,
        capped,
        html,
        headers: navHeaders,
        webglHeavy,
        // Live-page eval for F4 (palette/fonts/tech runtime probes). Stays usable
        // until dispose(). page.evaluate(string) runs the string as an expression —
        // the F4 scripts are IIFEs returning JSON, which is exactly compatible.
        evaluate: (code: string): Promise<unknown> => {
          if (disposed) return Promise.reject(new Error('pageCtx disposed'));
          if (typeof code !== 'string')
            return Promise.reject(new Error('evaluate expects a code string'));
          return page.evaluate(code);
        },
        dispose,
      };
    } finally {
      try {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      } catch {}
    }
  } catch (err) {
    if (signal) signal.removeEventListener('abort', onAbort);
    await dispose();
    throw err;
  } finally {
    if (signal) signal.removeEventListener('abort', onAbort);
  }
}

export { capturePage, closeBrowser, getBrowser, getChromium };
