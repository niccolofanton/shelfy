// Web-reference capture (v2): one page and one whole site.
//
// Per page, in this order (the order matters — each step reads the page in the
// state a visitor would see it before the next one changes it):
//   1. navigate (domcontentloaded) → load (≤15 s) → short network-idle grace;
//   2. anti-bot / challenge / login-wall detection (waits briefly for auto-pass);
//   3. consent opt-out (autoconsent), cosmetic filters, leftover pop-ups removed;
//   4. loader/preloader wait, fonts + above-the-fold images, visual stability;
//   5. HERO: the untouched first viewport at 2× (pixel QC, one retry);
//   6. primary page only: smooth wheel-scroll SCREENCAST → MP4 + hover preview;
//   7. reveal mode (scroll-triggered content forced visible) + page probe;
//   8. FULL PAGE in ≤2000 CSS-px bands at 2× (filmstrip for scroll-jacked WebGL
//      experiences), FOOTER crop, SECTION crops.
// Assets are written under <userData>/assets/web; intermediate PNGs live in a
// per-page scratch dir that is always removed.

import fs from 'fs';
import path from 'path';
import type { PageDriver, SiteSession, NavResult, NetEntry } from './driver';
import { createPlaywrightSession, scratchDir, VIEWPORT } from './driver';
import {
  JS_DETECT_BLOCKED,
  JS_LOADER_VISIBLE,
  JS_VIEWPORT_CONTENT,
  JS_REMOVE_OVERLAYS,
  JS_DOC_LOCKED,
  JS_HAS_BIG_FIXED_CANVAS,
  JS_SCROLL_STATE,
  JS_REVEAL,
  JS_NEUTRALIZE_VIRTUAL_SCROLL,
  JS_HIDE_FLOATING_DECOR,
  JS_PAGE_PROBE,
  jsWaitAssets,
} from './scripts';
import {
  assetPath,
  encodeWebp,
  cropAcross,
  signature,
  meanDiff,
  imageStats,
  encodeVideo,
  encodePreview,
  createPool,
  pngSize,
  type ImageAsset,
} from './encode';
import { pickPages, classifyUrl, type PageType } from './discover';
import { discoverPages } from './sitefetch';

// ─── Public types ────────────────────────────────────────────────────────────

// A capture event is a stable snake_case `code` plus scalar params (P4 lane rule
// 10), never UI prose: the capture service streams the code and the SPA/desktop
// render the string. shared/capture/codes.json is the contract; the desktop maps
// codes to Italian in webcap/codes.ts.
export type CaptureEventKind = 'read' | 'artifact' | 'info' | 'error';
export type CaptureEventParams = Record<string, string | number | boolean | null>;
export interface CaptureEvent {
  kind: CaptureEventKind;
  code: string;
  params?: CaptureEventParams;
}

export interface CaptureHooks {
  onEvent?: (e: CaptureEvent) => void;
  onStage?: (stage: string, frac: number) => void;
  onPage?: (page: CapturedPage, index: number, total: number) => void;
}

export interface BandAsset extends ImageAsset {
  top: number; // CSS px from the top of the page
  cssHeight: number;
}

export interface SectionAsset extends ImageAsset {
  kind: string;
  heading: string;
  top: number;
  cssHeight: number;
}

export interface VideoAsset {
  path: string;
  preview: string | null;
  poster: string | null;
  width: number;
  height: number;
  duration: number;
}

export type QcStatus = 'ok' | 'blank' | 'black' | 'loader' | 'sparse';

// Raw JS_PAGE_PROBE output (typed loosely: it is untrusted page data).
export type PageProbe = Record<string, unknown> & {
  head?: Record<string, unknown>;
  links?: { href: string; text: string; region: string; visible: boolean; top: number | null }[];
  sections?: { kind: string; top: number; height: number; heading: string }[];
  docHeight?: number;
};

export interface CapturedPage {
  requestedUrl: string;
  url: string;
  pageType: PageType;
  status: number | null;
  headers: Record<string, string>;
  title: string;
  hero: ImageAsset | null;
  bands: BandAsset[];
  footer: ImageAsset | null;
  sections: SectionAsset[];
  video: VideoAsset | null;
  heightCss: number;
  capped: boolean;
  jacked: boolean;
  probe: PageProbe;
  network: NetEntry[];
  qc: { status: QcStatus; reason: string };
  consent: { cmp: string | null; result: string | null };
  overlaysRemoved: number;
  reveal: Record<string, number> | null;
  timings: Record<string, number>;
}

export interface SiteCapture {
  engine: 'playwright' | 'electron' | 'chrome';
  userAgent: string;
  primary: CapturedPage;
  pages: CapturedPage[]; // primary first
  skipped: { url: string; reason: string }[];
  discoverySource: 'nav' | 'nav+sitemap' | 'single-page';
}

export class BlockedError extends Error {
  readonly blocked = true;
  constructor(
    readonly vendor: string,
    readonly url: string,
    readonly reason: string,
  ) {
    super(`Il sito ha mostrato una verifica anti-bot (${vendor})`);
    this.name = 'BlockedError';
  }
}

export class PageError extends Error {
  constructor(
    message: string,
    readonly code: 'http' | 'not-html' | 'navigation' | 'empty' | 'timeout',
  ) {
    super(message);
    this.name = 'PageError';
  }
}

// ─── Constants ───────────────────────────────────────────────────────────────

const NAV_TIMEOUT_MS = 35_000;
const LOAD_WAIT_MS = 15_000;
const IDLE_GRACE_MS = 5_000;
const CHALLENGE_WAIT_MS = 16_000;
const CONSENT_MAX_MS = 7_000;
const LOADER_MAX_MS = 25_000;
const BAND_CSS = 2000;
const MAX_HEIGHT_CSS = 30_000;
const VIDEO_MAX_MS = 24_000;
const VIDEO_STEP_PX = 64;
const VIDEO_STEP_MS = 45;
const JOURNEY_MAX_FRAMES = 12;
const JOURNEY_DELTA = 1500;
const JOURNEY_SETTLE_MS = 950;
const SECTION_MAX = 14;
const SECTION_MIN_CSS = 160;
const SECTION_MAX_CSS = 2400;
const HERO_QUALITY = 90;
const BAND_QUALITY = 86;

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal?.aborted) return resolve();
    const t = setTimeout(done, ms);
    function done(): void {
      clearTimeout(t);
      signal?.removeEventListener('abort', done);
      resolve();
    }
    signal?.addEventListener('abort', done, { once: true });
  });
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) throw Object.assign(new Error('AbortError'), { name: 'AbortError' });
}

function shortPath(u: string): string {
  try {
    const x = new URL(u);
    return (x.pathname || '/') + (x.search || '');
  } catch {
    return u;
  }
}

// ─── Page steps ──────────────────────────────────────────────────────────────

async function detectBlocked(
  page: PageDriver,
  nav: NavResult,
): Promise<{ blocked: boolean; vendor?: string; reason?: string }> {
  const res = await page.evaluate<{ blocked: boolean; vendor?: string; reason?: string }>(
    JS_DETECT_BLOCKED,
    {
      fallback: { blocked: false },
    },
  );
  if (res.blocked) return res;
  const server = String(nav.headers['server'] || '').toLowerCase();
  if (
    (nav.status === 403 || nav.status === 429 || nav.status === 503) &&
    (nav.headers['cf-mitigated'] || /cloudflare|akamai|datadome/.test(server))
  ) {
    return {
      blocked: true,
      vendor: server.includes('cloudflare') ? 'cloudflare' : server || 'firewall',
      reason: `HTTP ${nav.status}`,
    };
  }
  return { blocked: false };
}

interface ConsentMessage {
  type?: string;
  cmp?: string;
  result?: unknown;
}

async function settleConsent(
  page: PageDriver,
  signal?: AbortSignal,
): Promise<{ cmp: string | null; result: string | null }> {
  const t0 = Date.now();
  let cmp: string | null = null;
  for (;;) {
    const msgs = await page.evaluate<ConsentMessage[]>(
      `(() => { const s = window.autoconsentStandalone; return s && Array.isArray(s.messages) ? s.messages.slice(-40).map((m) => ({ type: m.type, cmp: m.cmp, result: m.result })) : []; })()`,
      { fallback: [], timeoutMs: 3_000 },
    );
    for (const m of msgs) if (m.cmp && !cmp) cmp = String(m.cmp);
    const done = msgs.find((m) => m.type === 'autoconsentDone' || m.type === 'optOutResult');
    if (done) return { cmp, result: done.type === 'optOutResult' ? String(done.result) : 'done' };
    const elapsed = Date.now() - t0;
    if (!cmp && elapsed > 2_500) return { cmp: null, result: null };
    if (elapsed > CONSENT_MAX_MS) return { cmp, result: 'timeout' };
    await sleep(400, signal);
    throwIfAborted(signal);
  }
}

async function waitLoader(page: PageDriver, maxMs: number, signal?: AbortSignal): Promise<number> {
  const t0 = Date.now();
  for (;;) {
    const present = await page.evaluate<boolean>(JS_LOADER_VISIBLE, {
      fallback: false,
      timeoutMs: 4_000,
    });
    if (!present || Date.now() - t0 >= maxMs) return Date.now() - t0;
    await sleep(500, signal);
    throwIfAborted(signal);
  }
}

// Viewport shots until two consecutive frames look the same (or the cap): the
// last frame is the hero. WebGL scenes never fully settle — the cap bounds them.
async function stableViewport(
  page: PageDriver,
  maxMs: number,
  signal?: AbortSignal,
): Promise<Buffer> {
  const t0 = Date.now();
  let prev: Buffer | null = null;
  let shot = await page.screenshot({ animations: 'allow' });
  for (;;) {
    const sig = await signature(shot, signal);
    if (prev && meanDiff(prev, sig) < 2.5) return shot;
    if (Date.now() - t0 >= maxMs) return shot;
    prev = sig;
    await sleep(700, signal);
    throwIfAborted(signal);
    shot = await page.screenshot({ animations: 'allow' });
  }
}

async function heroQc(
  page: PageDriver,
  png: Buffer,
  signal?: AbortSignal,
): Promise<{ status: QcStatus; reason: string }> {
  const stats = await imageStats(png, signal);
  const content = await page.evaluate<{ chars: number; images: number; media: number }>(
    JS_VIEWPORT_CONTENT,
    {
      fallback: { chars: 0, images: 0, media: 0 },
    },
  );
  const loader = await page.evaluate<boolean>(JS_LOADER_VISIBLE, { fallback: false });
  if (loader) return { status: 'loader', reason: 'loader overlay still visible' };
  if (stats) {
    if (stats.std < 4 && content.media === 0) {
      return {
        status: stats.mean < 30 ? 'black' : 'blank',
        reason: `uniform frame (mean ${stats.mean.toFixed(0)}, std ${stats.std.toFixed(1)})`,
      };
    }
    if (stats.edges < 0.003 && content.chars < 15 && content.images === 0 && content.media === 0) {
      return { status: 'sparse', reason: 'almost nothing painted in the first viewport' };
    }
  }
  return { status: 'ok', reason: '' };
}

// Smooth wheel scroll from top to bottom while the compositor is screencast.
// Wheel events (not scrollTo) so smooth-scroll libraries and scroll-jacked
// experiences animate exactly as for a visitor.
async function recordScroll(
  page: PageDriver,
  dir: string,
  jacked: boolean,
  signal?: AbortSignal,
): Promise<{ file: string; ts: number }[]> {
  const frames: { file: string; ts: number }[] = [];
  let n = 0;
  const stop = await page.startScreencast(
    (jpeg, ts) => {
      // The compositor can push ~100 fps: keep ≤ 40 fps (the video is 30 fps).
      if (frames.length >= 1600 || (frames.length && ts - frames[frames.length - 1].ts < 1 / 40))
        return;
      const file = path.join(dir, `f${String(n++).padStart(5, '0')}.jpg`);
      try {
        fs.writeFileSync(file, jpeg);
        frames.push({ file, ts });
      } catch {}
    },
    { maxWidth: VIEWPORT.width, maxHeight: VIEWPORT.height, quality: 82 },
  );
  try {
    await page.mouseMove(Math.round(VIEWPORT.width / 2), Math.round(VIEWPORT.height / 2));
    await sleep(1_400, signal); // hold on the hero
    const t0 = Date.now();
    let lastY = -1;
    let stuckSince = 0;
    let i = 0;
    while (Date.now() - t0 < VIDEO_MAX_MS && !signal?.aborted) {
      await page.wheel(VIDEO_STEP_PX);
      await sleep(VIDEO_STEP_MS, signal);
      if (++i % 8 !== 0) continue;
      const st = await page.evaluate<{ y: number; max: number }>(JS_SCROLL_STATE, {
        fallback: { y: 0, max: 0 },
        timeoutMs: 2_000,
      });
      if (!jacked && st.max > 0 && st.y >= st.max - 2) {
        await sleep(1_200, signal); // hold on the footer
        break;
      }
      if (st.y === lastY) {
        if (!stuckSince) stuckSince = Date.now();
        if (!jacked && Date.now() - stuckSince > 2_500) break;
      } else {
        stuckSince = 0;
        lastY = st.y;
      }
    }
  } finally {
    await stop();
  }
  return frames.sort((a, b) => a.ts - b.ts);
}

async function returnToTop(page: PageDriver, jacked: boolean, signal?: AbortSignal): Promise<void> {
  if (jacked) {
    for (let i = 0; i < 60; i++) {
      await page.wheel(-VIDEO_STEP_PX * 6);
      await sleep(30, signal);
    }
    await sleep(1_200, signal);
  }
  await page.evaluate('window.scrollTo(0, 0)', { fallback: null });
  await sleep(500, signal);
}

// Trigger lazy content on pages without a video pass: stepwise native scroll.
async function quickScroll(page: PageDriver, signal?: AbortSignal): Promise<void> {
  const st0 = await page.evaluate<{ max: number }>(JS_SCROLL_STATE, { fallback: { max: 0 } });
  const max = Math.min(st0.max, MAX_HEIGHT_CSS);
  for (let y = 0; y <= max && !signal?.aborted; y += Math.round(VIEWPORT.height * 0.85)) {
    await page.evaluate(`window.scrollTo(0, ${y})`, { fallback: null, timeoutMs: 3_000 });
    await sleep(260, signal);
  }
  await page.evaluate('window.scrollTo(0, 0)', { fallback: null });
  await sleep(300, signal);
}

async function filmstrip(page: PageDriver, dir: string, signal?: AbortSignal): Promise<string[]> {
  const files: string[] = [];
  let prev: Buffer | null = null;
  await page.mouseMove(Math.round(VIEWPORT.width / 2), Math.round(VIEWPORT.height / 2));
  for (let i = 0; i < JOURNEY_MAX_FRAMES && !signal?.aborted; i++) {
    let png: Buffer;
    try {
      png = await page.screenshot({ animations: 'allow' });
    } catch {
      break;
    }
    const sig = await signature(png, signal);
    if (prev && meanDiff(prev, sig) < 6) break; // the experience clamped
    const file = path.join(dir, `journey-${String(i).padStart(2, '0')}.png`);
    fs.writeFileSync(file, png);
    files.push(file);
    prev = sig;
    await page.wheel(JOURNEY_DELTA);
    await sleep(JOURNEY_SETTLE_MS, signal);
  }
  return files;
}

// ─── One page ────────────────────────────────────────────────────────────────

export interface PageOptions {
  primary: boolean;
  video: boolean;
  stamp: number;
  pageType?: PageType;
  extraSettleMs?: number;
  signal?: AbortSignal;
  emit?: (e: CaptureEvent) => void;
  encodePool: <T>(task: () => Promise<T>) => Promise<T>;
}

export async function capturePage(
  session: SiteSession,
  requestedUrl: string,
  opts: PageOptions,
): Promise<CapturedPage> {
  const { signal, emit, stamp, encodePool } = opts;
  const timings: Record<string, number> = {};
  const T0 = Date.now();
  let last = T0;
  const mark = (label: string): void => {
    const now = Date.now();
    timings[label] = now - last;
    last = now;
  };
  const page = await session.newPage();
  const dir = scratchDir('shelfy-cap-');
  const onAbort = (): void => {
    page.close().catch(() => {});
  };
  signal?.addEventListener('abort', onAbort, { once: true });
  try {
    throwIfAborted(signal);
    // 1. navigate
    let nav: NavResult;
    try {
      nav = await page.goto(requestedUrl, NAV_TIMEOUT_MS);
    } catch (err) {
      throwIfAborted(signal);
      throw new PageError(
        `Pagina non raggiungibile: ${(err as Error)?.message?.split('\n')[0] || err}`,
        'navigation',
      );
    }
    await page.waitForLoad(LOAD_WAIT_MS);
    await page.waitForNetworkIdle(IDLE_GRACE_MS);
    mark('load');
    throwIfAborted(signal);

    // 2. anti-bot / challenge
    let blocked = await detectBlocked(page, nav);
    if (blocked.blocked && blocked.vendor !== 'login') {
      emit?.({
        kind: 'info',
        code: 'antibot.waiting',
        params: { vendor: blocked.vendor || 'unknown', path: shortPath(requestedUrl) },
      });
      const tB = Date.now();
      while (blocked.blocked && Date.now() - tB < CHALLENGE_WAIT_MS) {
        await sleep(1_500, signal);
        throwIfAborted(signal);
        blocked = await detectBlocked(page, { ...nav, status: 200 });
      }
    }
    if (blocked.blocked)
      throw new BlockedError(
        blocked.vendor || 'unknown',
        page.url() || requestedUrl,
        blocked.reason || '',
      );
    if (nav.contentType && !/html|xml/.test(nav.contentType)) {
      throw new PageError(`Non è una pagina HTML (${nav.contentType})`, 'not-html');
    }
    if (nav.status !== null && nav.status >= 400) throw new PageError(`HTTP ${nav.status}`, 'http');
    mark('checks');

    // 3. consent + pop-ups
    const consent = await settleConsent(page, signal);
    await page.applyCosmetics();
    let overlaysRemoved = await page.evaluate<number>(JS_REMOVE_OVERLAYS, { fallback: 0 });
    if (consent.cmp)
      emit?.({
        kind: 'info',
        code: 'consent.optout',
        params: { cmp: consent.cmp, result: consent.result || 'ok' },
      });
    mark('consent');

    // 4. readiness
    const loaderMs = await waitLoader(page, LOADER_MAX_MS, signal);
    if (loaderMs > 1500)
      emit?.({
        kind: 'info',
        code: 'preloader.waited',
        params: { seconds: Math.round(loaderMs / 100) / 10 },
      });
    await page.evaluate(jsWaitAssets(6_000), { fallback: true, timeoutMs: 8_000 });
    if (opts.extraSettleMs) await sleep(opts.extraSettleMs, signal);
    const earlyLocked = await page.evaluate<boolean>(JS_DOC_LOCKED, { fallback: false });
    const bigCanvas = await page.evaluate<boolean>(JS_HAS_BIG_FIXED_CANVAS, { fallback: false });
    const jacked = earlyLocked && bigCanvas;
    overlaysRemoved += await page.evaluate<number>(JS_REMOVE_OVERLAYS, { fallback: 0 });
    mark('ready');

    // 5. hero (untouched first viewport)
    let heroPng = await stableViewport(page, 4_000, signal);
    let qc = await heroQc(page, heroPng, signal);
    if (qc.status !== 'ok') {
      emit?.({
        kind: 'info',
        code: 'hero.retry',
        params: { status: qc.status, path: shortPath(requestedUrl) },
      });
      await waitLoader(page, 15_000, signal);
      await sleep(6_000, signal);
      throwIfAborted(signal);
      heroPng = await stableViewport(page, 5_000, signal);
      qc = await heroQc(page, heroPng, signal);
    }
    const finalUrl = page.url() || requestedUrl;
    const heroPromise = encodePool(() =>
      encodeWebp(heroPng, assetPath(`${finalUrl}#hero`, 'webp', stamp), {
        quality: HERO_QUALITY,
        signal,
      }),
    );
    mark('hero');

    // 6. scroll video (primary page)
    let videoFrames: { file: string; ts: number }[] = [];
    if (opts.video) {
      try {
        videoFrames = await recordScroll(page, dir, jacked, signal);
        emit?.({ kind: 'info', code: 'video.recorded', params: { frames: videoFrames.length } });
      } catch (err) {
        throwIfAborted(signal);
        emit?.({
          kind: 'info',
          code: 'video.failed',
          params: { detail: String((err as Error)?.message || err).slice(0, 200) },
        });
      }
      await returnToTop(page, jacked, signal);
      mark('video');
    }
    const videoPromise: Promise<VideoAsset | null> =
      videoFrames.length > 10
        ? encodePool(async () => {
            const out = assetPath(`${finalUrl}#video`, 'mp4', stamp);
            const v = await encodeVideo(videoFrames, out, { width: 1280, crf: 27, signal });
            if (!v) return null;
            const preview = await encodePreview(
              out,
              assetPath(`${finalUrl}#preview`, 'mp4', stamp),
              { signal },
            );
            return {
              path: v.path,
              preview,
              poster: null,
              width: v.width,
              height: v.height,
              duration: v.duration,
            };
          }).catch((err) => {
            if ((err as Error)?.name === 'AbortError') throw err;
            emit?.({
              kind: 'info',
              code: 'video.encode_failed',
              params: { detail: String((err as Error)?.message || err).slice(0, 200) },
            });
            return null;
          })
        : Promise.resolve(null);

    // 7. reveal + probe
    let reveal: Record<string, number> | null = null;
    if (!jacked) {
      if (!opts.video) await quickScroll(page, signal);
      await page.evaluate(JS_NEUTRALIZE_VIRTUAL_SCROLL, { fallback: null });
      reveal = await page.evaluate<Record<string, number>>(JS_REVEAL, {
        fallback: null as unknown as Record<string, number>,
        timeoutMs: 12_000,
      });
      await page.applyCosmetics();
      overlaysRemoved += await page.evaluate<number>(JS_REMOVE_OVERLAYS, { fallback: 0 });
      await page.evaluate(JS_HIDE_FLOATING_DECOR, { fallback: 0 });
      await page.evaluate('window.scrollTo(0, 0)', { fallback: null });
      await sleep(600, signal);
    }
    const probe = await page.evaluate<PageProbe>(JS_PAGE_PROBE, {
      fallback: {} as PageProbe,
      timeoutMs: 30_000,
    });
    mark('probe');
    throwIfAborted(signal);

    // 8. full page
    const bands: BandAsset[] = [];
    const bandFiles: { file: string; top: number; height: number }[] = [];
    let heightCss = VIEWPORT.height;
    let capped = false;
    const pending: Promise<unknown>[] = [];
    if (jacked) {
      const frames = await filmstrip(page, dir, signal);
      frames.forEach((file, i) => {
        const top = i * VIEWPORT.height;
        bandFiles.push({ file, top, height: VIEWPORT.height });
        pending.push(
          encodePool(() =>
            encodeWebp(file, assetPath(`${finalUrl}#band${i}`, 'webp', stamp), {
              quality: BAND_QUALITY,
              signal,
            }),
          ).then((a) => {
            bands[i] = { ...a, top, cssHeight: VIEWPORT.height };
          }),
        );
      });
      heightCss = frames.length * VIEWPORT.height;
    } else {
      const doc = Number(probe.docHeight) || VIEWPORT.height;
      heightCss = Math.max(VIEWPORT.height, Math.min(doc, MAX_HEIGHT_CSS));
      capped = doc > MAX_HEIGHT_CSS;
      for (let i = 0, top = 0; top < heightCss; i++, top += BAND_CSS) {
        throwIfAborted(signal);
        const h = Math.min(BAND_CSS, heightCss - top);
        let png: Buffer;
        try {
          png = await page.screenshot({
            fullPage: true,
            clip: { x: 0, y: top, width: VIEWPORT.width, height: h },
            animations: 'disabled',
            timeoutMs: 60_000,
          });
        } catch (err) {
          throwIfAborted(signal);
          emit?.({
            kind: 'info',
            code: 'band.failed',
            params: {
              band: i + 1,
              detail: String((err as Error)?.message?.split('\n')[0] || err).slice(0, 200),
            },
          });
          break;
        }
        const file = path.join(dir, `band-${String(i).padStart(2, '0')}.png`);
        fs.writeFileSync(file, png);
        bandFiles.push({ file, top, height: h });
        const idx = i;
        pending.push(
          encodePool(() =>
            encodeWebp(file, assetPath(`${finalUrl}#band${idx}`, 'webp', stamp), {
              quality: BAND_QUALITY,
              signal,
            }),
          ).then((a) => {
            bands[idx] = { ...a, top, cssHeight: h };
          }),
        );
      }
      if (bandFiles.length) {
        const total = bandFiles[bandFiles.length - 1].top + bandFiles[bandFiles.length - 1].height;
        if (total < heightCss) heightCss = total;
      }
    }
    mark('bands');

    // Device-pixel scale of the PNG bands (2× normally; frames are viewport shots).
    const scale = bandFiles.length
      ? pngSize(fs.readFileSync(bandFiles[0].file)).width / VIEWPORT.width || VIEWPORT.scale
      : VIEWPORT.scale;

    // Footer crop: the last viewport of the page.
    let footer: ImageAsset | null = null;
    if (bandFiles.length && heightCss > VIEWPORT.height * 1.5) {
      const h = Math.min(VIEWPORT.height, heightCss);
      pending.push(
        encodePool(() =>
          cropAcross(
            bandFiles,
            { y: heightCss - h, h },
            scale,
            assetPath(`${finalUrl}#footer`, 'webp', stamp),
            {
              quality: HERO_QUALITY,
              signal,
            },
          ),
        ).then((a) => {
          footer = a;
        }),
      );
    }

    // Section crops.
    const sections: SectionAsset[] = [];
    if (!jacked && bandFiles.length && Array.isArray(probe.sections)) {
      const list = probe.sections
        .filter((s) => s && s.height >= SECTION_MIN_CSS && s.top < heightCss)
        .slice(0, SECTION_MAX);
      list.forEach((s, i) => {
        const h = Math.min(s.height, SECTION_MAX_CSS, heightCss - s.top);
        if (h < SECTION_MIN_CSS) return;
        pending.push(
          encodePool(() =>
            cropAcross(
              bandFiles,
              { y: s.top, h },
              scale,
              assetPath(`${finalUrl}#sec${i}`, 'webp', stamp),
              {
                quality: BAND_QUALITY,
                maxWidth: 2880,
                signal,
              },
            ),
          ).then((a) => {
            if (a)
              sections.push({
                ...a,
                kind: s.kind,
                heading: s.heading || '',
                top: s.top,
                cssHeight: h,
              });
          }),
        );
      });
    }

    const settled = await Promise.allSettled(pending);
    for (const r of settled) {
      if (r.status === 'rejected' && (r.reason as Error)?.name === 'AbortError') throw r.reason;
    }
    let hero: ImageAsset | null = null;
    try {
      hero = await heroPromise;
    } catch (err) {
      if ((err as Error)?.name === 'AbortError') throw err;
    }
    const video = await videoPromise;
    mark('encode');

    const compactBands = bands.filter(Boolean);
    const network = await page.network();
    if (!hero && !compactBands.length) throw new PageError('Nessuna immagine prodotta', 'empty');
    const head = (probe.head || {}) as { title?: string };
    return {
      requestedUrl,
      url: finalUrl,
      pageType: opts.pageType || classifyUrl(finalUrl),
      status: nav.status,
      headers: nav.headers,
      title: String(head.title || ''),
      hero,
      bands: compactBands,
      footer,
      sections: sections.sort((a, b) => a.top - b.top),
      video,
      heightCss,
      capped,
      jacked,
      probe,
      network: network.filter(
        (n) =>
          n.type === 'font' ||
          n.type === 'script' ||
          n.type === 'stylesheet' ||
          n.type === 'document',
      ),
      qc,
      consent,
      overlaysRemoved,
      reveal,
      timings: { ...timings, total: Date.now() - T0 },
    };
  } finally {
    signal?.removeEventListener('abort', onAbort);
    await page.close().catch(() => {});
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

// ─── Whole site ──────────────────────────────────────────────────────────────

const PRIMARY_DEADLINE_MS = 5 * 60_000; // includes the scroll video
const INNER_DEADLINE_MS = 3 * 60_000;

// Runs one page capture under its own deadline, linked to the job signal: a page
// stuck in any step can never hold the whole job. A deadline hit surfaces as a
// PageError('timeout'); a job cancel stays an AbortError.
async function withPageDeadline<T>(
  ms: number,
  parent: AbortSignal | undefined,
  run: (signal: AbortSignal) => Promise<T>,
): Promise<T> {
  const ac = new AbortController();
  const onParent = (): void => ac.abort();
  if (parent?.aborted) ac.abort();
  parent?.addEventListener('abort', onParent, { once: true });
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    ac.abort();
  }, ms);
  try {
    return await run(ac.signal);
  } catch (err) {
    if (timedOut && !parent?.aborted)
      throw new PageError(`Tempo scaduto dopo ${Math.round(ms / 60_000)} minuti`, 'timeout');
    throw err;
  } finally {
    clearTimeout(timer);
    parent?.removeEventListener('abort', onParent);
  }
}

export interface SiteOptions {
  maxPages: number;
  singlePage: boolean;
  stamp: number;
  video: boolean;
  signal?: AbortSignal;
  hooks?: CaptureHooks;
  session?: SiteSession; // provided by the unblock flow (visible window)
  // ─── Injectable knobs (desktop uses the defaults; the capture service, SPIKE-11,
  // runs 1 page at a time at 1×, with the server budgets) ─────────────────────
  pagesParallel?: number; // inner pages captured at once (default: v2's min(3, …))
  encodePoolSize?: number; // ffmpeg encode-pool size (default 3)
  deviceScale?: number; // default session device scale (default 2×; server 1×)
  sessionLocale?: string; // context locale for the default session
  primaryDeadlineMs?: number; // default 5 min (service: 180 s)
  innerDeadlineMs?: number; // default 3 min (service: 150 s)
  // When set, inner pages stop being started once the wall-clock budget or the
  // artifact-byte cap is reached, and the pages already captured are KEPT
  // (SPIKE-11: a timed-out capture is `partial`, not discarded). The remaining
  // pages are reported in `skipped` with reason `budget_exceeded`.
  siteBudgetMs?: number;
  maxArtifactBytes?: number;
  // Override the default Playwright session factory (service injects none → uses
  // the default). The fallback runs when the primary factory throws; the desktop
  // passes its visible-window ElectronSession, the service passes none so a launch
  // failure fails the capture (no Electron fallback on the server).
  createSession?: () => Promise<SiteSession>;
  fallbackSession?: () => Promise<SiteSession>;
}

// Bytes already written for a captured page (hero + bands + footer + sections +
// video), summed from the files on disk for the artifact-budget check.
function pageArtifactBytes(p: CapturedPage): number {
  let total = 0;
  const add = (f: string | null | undefined): void => {
    if (!f) return;
    try {
      total += fs.statSync(f).size;
    } catch {}
  };
  add(p.hero?.path);
  for (const b of p.bands) add(b.path);
  add(p.footer?.path);
  for (const s of p.sections) add(s.path);
  add(p.video?.path);
  add(p.video?.preview);
  return total;
}

export async function captureSite(url: string, opts: SiteOptions): Promise<SiteCapture> {
  // The site budget includes session setup and the primary page. Keep a finished
  // primary even when that budget is exhausted, but do not start inner pages.
  const siteStart = Date.now();
  const { signal, hooks } = opts;
  const emit = (e: CaptureEvent): void => {
    try {
      hooks?.onEvent?.(e);
    } catch {}
  };
  const ownSession = !opts.session;
  let session: SiteSession;
  if (opts.session) session = opts.session;
  else {
    const createPrimary =
      opts.createSession ??
      ((): Promise<SiteSession> =>
        createPlaywrightSession({
          deviceScale: opts.deviceScale,
          maxConcurrency: opts.pagesParallel,
          locale: opts.sessionLocale,
        }));
    try {
      session = await createPrimary();
    } catch (err) {
      // No fallback (the capture service) → a launch failure fails the capture.
      if (!opts.fallbackSession) throw err;
      emit({
        kind: 'info',
        code: 'engine.fallback',
        params: { detail: String((err as Error)?.message?.split('\n')[0] || err).slice(0, 200) },
      });
      session = await opts.fallbackSession();
    }
  }
  const encodePool = createPool(opts.encodePoolSize ?? 3);
  const primaryDeadlineMs = opts.primaryDeadlineMs ?? PRIMARY_DEADLINE_MS;
  const innerDeadlineMs = opts.innerDeadlineMs ?? INNER_DEADLINE_MS;
  try {
    // The sitemap is only a supplement: fetch it in parallel with the primary page.
    const sitemapPromise: Promise<string[]> = opts.singlePage
      ? Promise.resolve([])
      : discoverPages(url, { maxPages: 8, signal })
          .then((d) => (d && d.source === 'sitemap' ? d.pages.map((p) => p.url) : []))
          .catch(() => []);

    hooks?.onStage?.('primary', 0);
    emit({ kind: 'read', code: 'site.opening', params: { url } });
    const primary = await withPageDeadline(primaryDeadlineMs, signal, (s) =>
      capturePage(session, url, {
        primary: true,
        video: opts.video,
        stamp: opts.stamp,
        signal: s,
        emit,
        encodePool,
      }),
    );
    emit({
      kind: 'artifact',
      code: 'page.primary_captured',
      params: {
        hero: primary.hero ? `${primary.hero.width}×${primary.hero.height}` : '—',
        bands: primary.bands.length,
        sections: primary.sections.length,
      },
    });
    const total = opts.singlePage ? 1 : Math.max(1, opts.maxPages);
    hooks?.onPage?.(primary, 0, total);

    const pages: CapturedPage[] = [primary];
    const skipped: { url: string; reason: string }[] = [];
    let discoverySource: SiteCapture['discoverySource'] = 'single-page';
    if (!opts.singlePage && opts.maxPages > 1) {
      const sitemap = await Promise.race([sitemapPromise, sleep(4_000).then(() => [] as string[])]);
      const head = (primary.probe.head || {}) as { hreflang?: { lang?: string; href?: string }[] };
      const picked = pickPages(primary.url, primary.probe.links || [], {
        max: opts.maxPages - 1,
        hreflang: head.hreflang || [],
        sitemap,
      });
      discoverySource = picked.some((p) => p.source === 'sitemap') ? 'nav+sitemap' : 'nav';
      emit({
        kind: 'info',
        code: 'pages.selected',
        params: {
          count: picked.length,
          pages: picked.map((p) => `${shortPath(p.url)} (${p.pageType})`).join(', '),
        },
      });
      const ctx = (primary.probe.canvasContexts || {}) as Record<string, number>;
      const webgl = !!(ctx.webgl || ctx.webgl2 || ctx.webgpu);
      // The capture service forces 1 page at a time (SPIKE-11); otherwise v2's own
      // choice (2 for WebGL / scroll-jacked, else 3), capped by the session.
      const concurrency =
        opts.pagesParallel ??
        Math.min(session.maxConcurrency ?? 3, primary.jacked || webgl ? 2 : 3);
      let next = 0;
      let done = 1;
      const results: (CapturedPage | null)[] = new Array(picked.length).fill(null);
      // Site budget / artifact cap: once reached, stop STARTING new inner pages and
      // keep the ones already captured (SPIKE-11 partial-persistence). A running
      // page finishes under its own deadline.
      const budgetReached = (): boolean => {
        if (opts.siteBudgetMs && Date.now() - siteStart >= opts.siteBudgetMs) return true;
        if (opts.maxArtifactBytes) {
          let bytes = pageArtifactBytes(primary);
          for (const r of results) if (r) bytes += pageArtifactBytes(r);
          if (bytes >= opts.maxArtifactBytes) return true;
        }
        return false;
      };
      const worker = async (): Promise<void> => {
        for (;;) {
          throwIfAborted(signal);
          const i = next++;
          if (i >= picked.length) return;
          if (budgetReached()) {
            next = picked.length; // stop every worker; unstarted pages → budget_exceeded below
            return;
          }
          const p = picked[i];
          hooks?.onStage?.('pages', done / (picked.length + 1));
          try {
            const cap = await withPageDeadline(innerDeadlineMs, signal, (s) =>
              capturePage(session, p.url, {
                primary: false,
                video: false,
                stamp: opts.stamp,
                pageType: p.pageType,
                signal: s,
                emit,
                encodePool,
              }),
            );
            results[i] = cap;
            emit({
              kind: 'artifact',
              code: 'page.captured',
              params: {
                path: shortPath(cap.url),
                bands: cap.bands.length,
                sections: cap.sections.length,
              },
            });
          } catch (err) {
            if ((err as Error)?.name === 'AbortError') throw err;
            // Reason is a stable code, not prose (P4 lane rule 10 / manifest skip
            // reasons): anti-bot → capture_blocked, PageError → its code, else error.
            const reason =
              err instanceof BlockedError
                ? 'capture_blocked'
                : err instanceof PageError
                  ? err.code
                  : 'error';
            // On a session that already needed a human check, a second challenge
            // means the site re-checks every navigation: stop instead of piling up
            // flagged requests (the primary page is already captured).
            if (
              (err instanceof BlockedError || reason === 'http') &&
              session.engine !== 'playwright'
            )
              next = picked.length;
            skipped.push({ url: p.url, reason });
            emit({
              kind: 'info',
              code: 'page.skipped',
              params: { path: shortPath(p.url), reason },
            });
          } finally {
            done++;
            const ready = results.filter((r): r is CapturedPage => !!r);
            hooks?.onPage?.(ready[ready.length - 1] || primary, done - 1, total);
          }
        }
      };
      const runners = Array.from({ length: Math.min(concurrency, picked.length) }, () => worker());
      const outcome = await Promise.allSettled(runners);
      const aborted = outcome.find((o) => o.status === 'rejected');
      if (aborted && aborted.status === 'rejected') throw aborted.reason;
      // Pages never started because the site budget / artifact cap ran out: keep
      // what we have, report the rest as budget_exceeded (SPIKE-11 partial result).
      for (let i = 0; i < picked.length; i++) {
        if (!results[i] && !skipped.some((s) => s.url === picked[i].url)) {
          skipped.push({ url: picked[i].url, reason: 'budget_exceeded' });
          emit({
            kind: 'info',
            code: 'page.skipped',
            params: { path: shortPath(picked[i].url), reason: 'budget_exceeded' },
          });
        }
      }
      // Drop near-duplicates of an already captured page (same final URL).
      for (const r of results) {
        if (r && !pages.some((p) => p.url === r.url)) pages.push(r);
      }
    }
    return {
      engine: session.engine,
      userAgent: session.userAgent,
      primary,
      pages,
      skipped,
      discoverySource,
    };
  } finally {
    if (ownSession) await session.close().catch(() => {});
  }
}
