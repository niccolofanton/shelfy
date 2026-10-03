'use strict';

// Web reference capture (POC "siti web come reference", wave 2 — F1 + F2).
//
// Two responsibilities, both pure-main-process, zero new npm deps:
//
//  • discoverPages(url)  — F1. From a single pasted URL, produce an ordered,
//    deduplicated list of 5-8 representative pages to capture. robots.txt →
//    sitemap.xml (sitemapindex + .gz via zlib core) → fallback home crawl
//    (regex on href, no cheerio) → normalize → rank by IT/EN path keywords →
//    dedup per template → cap. Never throws on a missing sitemap/dead host;
//    degrades to 'seed-only' (home only). Throws only on invalid/blocked input.
//
//  • capturePage(url)    — F2. Render the URL in a HIDDEN BrowserWindow that
//    reuses the persist:social session (auth cookies), dismiss cookie banners,
//    kill animations, scroll for lazy-load, neutralize fixed/sticky, resize the
//    window to the content height and capturePage() one full-page frame (or
//    slice + ffmpeg vstack past the texture cap). Returns a LIVE pageCtx whose
//    .evaluate()/.html/.headers stay usable until the caller calls .dispose().
//
// SSRF: every fetch/navigation is gated by net-safety.assertSafeUrl /
// isBlockedHostname (shared with downloader.js).
//
// IMPORTANT (wave 3, main.js): the hidden capture window has webContents type
// 'window', so the `will-navigate` guard in main.js/hardenWebContents would
// preventDefault() its initial loadURL to an external host. The window is tagged
// `win.__shelfyCapture = true`; main.js must let navigation through for tagged
// windows. Until that flag is honored, capturePage loads nothing. See report.

import { session, BrowserWindow } from 'electron';
import type {
  WebContents,
  DownloadItem,
  Event as ElectronEvent,
  OnHeadersReceivedListenerDetails,
  HeadersReceivedResponse,
} from 'electron';
import path from 'path';
import fs from 'fs';
import os from 'os';
import { spawn } from 'child_process';
import type { ChildProcessByStdio } from 'child_process';
import type { Readable } from 'stream';
import { assertSafeUrl } from './net-safety';
// Site discovery, HTTP fetch, image download/encode and asset paths moved to
// webcap/sitefetch.ts (SPIKE-11): the capture service bundles webcap/* without
// this v1 engine. Re-exported below so the desktop's public API is unchanged.
import {
  discoverPages,
  normalizeInputUrl,
  normalizeUrl,
  scorePath,
  selectRepresentative,
  extractLinks,
  parseSitemapXml,
  screenshotPathForUrl,
  fetchImageToWebp,
  getCaptureDir,
  resolveFfmpeg,
  encodeImage,
  safeInputPath,
  spawnAsync,
} from './webcap/sitefetch';

// Custom marker the capture window is tagged with so main.js/hardenWebContents
// can let it navigate to an external host (its webContents type is 'window').
// BrowserWindow has no index signature, so augment via an intersection type.
type TaggedCaptureWindow = BrowserWindow & { __shelfyCapture?: boolean };

// ─── Constants ──────────────────────────────────────────────────────────────

const PARTITION = 'persist:social'; // reuse the logged-in IG/X session
const DESKTOP_UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36';

// Capture (F2)
const VIEWPORT_W = 1280,
  VIEWPORT_H = 900;
const DEFAULT_MAX_HEIGHT = 12000; // app-level cap; beyond → capped:true
const SLICE_OVERLAP = 80; // px overlap between slices (cropped out before stitch)
const NAV_TIMEOUT_MS = 30_000;
const SETTLE_MS = 600; // post-scroll settle for lazy-load/fonts
const SETTLE_INNER_MS = 220; // per-scroll-step settle
const SCROLL_ITER_CAP = 40; // anti-infinite-scroll hard cap
const EVAL_TIMEOUT_MS = 15_000; // cap any single in-page eval (headless safety)
const SCROLL_EVAL_TIMEOUT_MS = 30_000; // autoScroll does real work; allow longer
const RAF_TIMEOUT_MS = 2_500; // rAF may never fire on a non-painting window
const CAPTURE_TIMEOUT_MS = 20_000; // capturePage() can hang on a headless surface

// ─── Small utilities ────────────────────────────────────────────────────────

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

// Like sleep, but resolves early if the signal aborts (so a long pre-capture wait
// doesn't make cancel feel unresponsive). Never rejects — the caller's next
// throwIfAborted(signal) turns an abort into the proper AbortError.
function sleepAbortable(ms: number, signal?: AbortSignal): Promise<void> {
  if (!signal) return sleep(ms);
  if (signal.aborted) return Promise.resolve();
  const sig = signal;
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

// Reject a promise if it doesn't settle within `ms`. Caps operations that can
// hang on a non-visible/headless window (capturePage, in-page rAF/fonts.ready).
function withTimeout<T>(promise: Promise<T>, ms: number, label?: string): Promise<T> {
  let t: ReturnType<typeof setTimeout>;
  const timeout = new Promise<never>((_, reject) => {
    t = setTimeout(
      () => reject(Object.assign(new Error(`timeout: ${label || 'op'}`), { name: 'TimeoutError' })),
      ms,
    );
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(t));
}

// Best-effort in-page eval: never hangs, never throws. Returns `fallback` on
// timeout/error so a wedged script (rAF that never fires, fonts.ready that never
// resolves) degrades gracefully instead of blocking the whole capture.
async function safeEval<T>(
  wc: WebContents,
  code: string,
  { ms = EVAL_TIMEOUT_MS, fallback }: { ms?: number; fallback?: T } = {},
): Promise<T | undefined> {
  try {
    if (wc.isDestroyed()) return fallback;
    return (await withTimeout(wc.executeJavaScript(code, true), ms, 'eval')) as T;
  } catch {
    return fallback;
  }
}

// One encoded screenshot band: its on-disk path plus the real pixel dimensions.
interface ScreenshotChunk {
  screenshotPath: string;
  width: number;
  height: number;
}

// Max stored height per screenshot chunk. A full-page capture can be 1280×12000;
// rendering that as ONE <img> forces the renderer to decode a single heavyweight
// frame (and the report-modal lightbox to hold it all in memory). Slicing it into
// vertical bands lets the UI stack several light images, each lazy-loaded.
const CHUNK_HEIGHT = 2000;

// Crop + encode a tall PNG into vertical chunks of ≤ chunkHeight each (top→bottom),
// returning [{ screenshotPath, width, height }]. chunk[0] is the page top, so it
// doubles as the lightweight thumbnail. A short page yields a single chunk written
// to the canonical screenshot path (so nothing downstream changes for normal pages).
// Best-effort: if cropping fails it falls back to one full-page encode.
async function encodeImageChunks(
  srcPng: string,
  finalUrl: string,
  format: string,
  quality: number,
  {
    stamp,
    chunkHeight = CHUNK_HEIGHT,
    signal,
  }: { stamp?: number; chunkHeight?: number; signal?: AbortSignal } = {},
): Promise<ScreenshotChunk[]> {
  const { width: srcW, height: srcH } = await probeImageSize(srcPng, signal);
  const W = srcW || VIEWPORT_W;
  const H = srcH || 0;

  const singleEncode = async (): Promise<ScreenshotChunk[]> => {
    const outPath = screenshotPathForUrl(finalUrl, format, stamp);
    await encodeImage(srcPng, outPath, format, quality, signal);
    const sz = await probeImageSize(outPath, signal);
    return [{ screenshotPath: outPath, width: sz.width || W, height: sz.height || H }];
  };

  // Short page → one image, canonical path (no behavioural change for normal sites).
  if (!H || H <= chunkHeight) return singleEncode();

  const ffmpeg = resolveFfmpeg();
  const n = Math.ceil(H / chunkHeight);
  const chunks: ScreenshotChunk[] = [];
  for (let i = 0; i < n; i++) {
    throwIfAborted(signal);
    const y = i * chunkHeight;
    const h = Math.min(chunkHeight, H - y);
    // Distinct, deterministic path per chunk (hash on "<url>#c<i>").
    const outPath = screenshotPathForUrl(`${finalUrl}#c${i}`, format, stamp);
    fs.mkdirSync(path.dirname(outPath), { recursive: true });
    const args = ['-hide_banner', '-protocol_whitelist', 'file', '-i', safeInputPath(srcPng)];
    if (format === 'png') {
      args.push('-vf', `crop=${W}:${h}:0:${y}`, '-frames:v', '1', '-y', outPath);
    } else {
      args.push(
        '-vf',
        `crop=${W}:${h}:0:${y},scale='min(iw,${VIEWPORT_W})':-2`,
        '-frames:v',
        '1',
        '-c:v',
        'libwebp',
        '-quality',
        String(quality),
        '-y',
        outPath,
      );
    }
    try {
      await spawnAsync(ffmpeg, args, signal);
      if (fs.existsSync(outPath)) {
        const sz = await probeImageSize(outPath, signal);
        chunks.push({ screenshotPath: outPath, width: sz.width || W, height: sz.height || h });
      }
    } catch (err) {
      if ((err as { name?: string })?.name === 'AbortError') throw err;
      /* skip this band; keep the others */
    }
  }
  // Trim TRAILING fully-flat bands. Forcing scroll-driven animations to their end
  // state compacts content upward while pin spacers keep documentElement tall, so
  // the document tail can be pure dead space. Ultra-conservative: only a band with
  // stddev < FLAT_TAIL_STDDEV (literally one flat color — even sparse small text
  // measures ~2) is dropped, never a middle band, and chunk[0] always survives.
  while (chunks.length > 1) {
    const last = chunks[chunks.length - 1];
    const flat = await probeImageFlatness(last.screenshotPath, signal);
    if (!flat || flat.stddev >= FLAT_TAIL_STDDEV) break;
    try {
      fs.unlinkSync(last.screenshotPath);
    } catch {}
    chunks.pop();
  }
  return chunks.length ? chunks : singleEncode();
}

// Stddev of a 16×16 grayscale signature — cheap "is this band one flat color"
// probe (same downscale trick as the journey frame dedup). Null on any failure.
const FLAT_TAIL_STDDEV = 1;
function probeImageFlatness(
  file: string,
  signal?: AbortSignal,
): Promise<{ mean: number; stddev: number } | null> {
  let ffmpeg: string;
  try {
    ffmpeg = resolveFfmpeg();
  } catch {
    return Promise.resolve(null);
  }
  return new Promise((resolve) => {
    let child: ChildProcessByStdio<null, Readable, null>;
    try {
      child = spawn(
        ffmpeg,
        [
          '-hide_banner',
          '-loglevel',
          'error',
          '-i',
          safeInputPath(file),
          '-vf',
          'scale=16:16,format=gray',
          '-f',
          'rawvideo',
          '-',
        ],
        { stdio: ['ignore', 'pipe', 'ignore'] },
      );
    } catch {
      return resolve(null);
    }
    const bufs: Buffer[] = [];
    child.stdout.on('data', (d: Buffer) => bufs.push(d));
    const onAbort = (): void => {
      try {
        child.kill('SIGKILL');
      } catch {}
      resolve(null);
    };
    const settle = (v: { mean: number; stddev: number } | null): void => {
      if (signal) signal.removeEventListener('abort', onAbort);
      resolve(v);
    };
    if (signal) {
      if (signal.aborted) return onAbort();
      signal.addEventListener('abort', onAbort, { once: true });
    }
    child.on('error', () => settle(null));
    child.on('close', () => {
      const buf = Buffer.concat(bufs);
      if (!buf.length) return settle(null);
      let sum = 0;
      for (const b of buf) sum += b;
      const mean = sum / buf.length;
      let v = 0;
      for (const b of buf) v += (b - mean) * (b - mean);
      settle({ mean, stddev: Math.sqrt(v / buf.length) });
    });
  });
}

// Encode an ORDERED list of already-cropped viewport PNG frames into webp/png
// chunks, returning [{ screenshotPath, width, height }] in the same shape as
// encodeImageChunks. Used by the FILMSTRIP path: a scroll-jacked WebGL experience
// (lusion-type) has no tall static DOM to slice, so we capture one viewport frame
// per scroll step and stack the frames as chunks. chunk[0] is the hero (thumbnail).
async function encodeFrames(
  framePngs: string[],
  finalUrl: string,
  format: string,
  quality: number,
  { stamp, signal }: { stamp?: number; signal?: AbortSignal } = {},
): Promise<ScreenshotChunk[]> {
  const chunks: ScreenshotChunk[] = [];
  for (let i = 0; i < framePngs.length; i++) {
    throwIfAborted(signal);
    const src = framePngs[i];
    if (!src || !fs.existsSync(src)) continue;
    // Distinct deterministic path per frame (hash on "<url>#f<i>").
    const outPath = screenshotPathForUrl(`${finalUrl}#f${i}`, format, stamp);
    try {
      await encodeImage(src, outPath, format, quality, signal);
      if (fs.existsSync(outPath)) {
        const sz = await probeImageSize(outPath, signal);
        chunks.push({
          screenshotPath: outPath,
          width: sz.width || VIEWPORT_W,
          height: sz.height || VIEWPORT_H,
        });
      }
    } catch (err) {
      if ((err as { name?: string })?.name === 'AbortError') throw err;
      /* skip this frame; keep the others */
    }
  }
  return chunks;
}

// Vertically concatenate already-overlap-cropped PNG slices with ffmpeg vstack,
// then encode to the final format. N is small (the height cap bounds slice count).
async function stitchSlices(
  slicePaths: string[],
  outPath: string,
  format: string,
  quality: number,
  signal?: AbortSignal,
): Promise<void> {
  const ffmpeg = resolveFfmpeg();
  const tmpMid = path.join(os.tmpdir(), `shelfy-web-stitch-${process.pid}-${Date.now()}.png`);
  const inputs: string[] = [];
  for (const p of slicePaths) {
    inputs.push('-i', safeInputPath(p));
  }
  const filter = slicePaths.length > 1 ? `vstack=inputs=${slicePaths.length}` : 'null';
  await spawnAsync(
    ffmpeg,
    [
      '-hide_banner',
      '-protocol_whitelist',
      'file',
      ...inputs,
      '-filter_complex',
      filter,
      '-frames:v',
      '1',
      '-y',
      tmpMid,
    ],
    signal,
  );
  if (!fs.existsSync(tmpMid)) throw new Error('vstack produced no output');
  await encodeImage(tmpMid, outPath, format, quality, signal);
  try {
    fs.unlinkSync(tmpMid);
  } catch {}
}

// Read the real WxH of an encoded image from ffmpeg's banner (no ffprobe ships).
async function probeImageSize(
  file: string,
  signal?: AbortSignal,
): Promise<{ width: number; height: number }> {
  const ffmpeg = resolveFfmpeg();
  const { stderr } = await spawnAsync(
    ffmpeg,
    ['-hide_banner', '-protocol_whitelist', 'file', '-i', safeInputPath(file)],
    signal,
  );
  const m = stderr.match(/,\s*(\d+)x(\d+)[, ]/);
  return m ? { width: Number(m[1]), height: Number(m[2]) } : { width: 0, height: 0 };
}

// ─── F2: in-page injected routines (run via executeJavaScript on untrusted page) ─

// Each returns a string of JS evaluated in the page. They are idempotent DOM
// manipulations; no preload, no privileged bridge into the untrusted page.

const JS_DISMISS_COOKIES = `(() => {
  const ACCEPT_SELECTORS = [
    '#onetrust-accept-btn-handler',
    'button#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll',
    '#CybotCookiebotDialogBodyButtonAccept',
    '.qc-cmp2-summary-buttons button[mode="primary"]',
    '#didomi-notice-agree-button',
    'button[data-testid="uc-accept-all-button"]',
    '.osano-cm-accept-all', '.cmplz-accept', '.brlbs-cmpnt-btn-accept-all',
    '[aria-label*="accept" i]',
  ];
  for (const sel of ACCEPT_SELECTORS) {
    try { const el = document.querySelector(sel); if (el) { el.click(); } } catch (e) {}
  }
  const ACCEPT_TEXT = ['accept all','accept cookies','accept','agree','i agree','allow all',
    'got it','ok','consenti','accetta tutto','accetta','ho capito','va bene','acconsento'];
  const clickable = Array.from(document.querySelectorAll('button, a, [role="button"], input[type="button"], input[type="submit"]'));
  for (const el of clickable) {
    const label = (el.innerText || el.textContent || el.value || el.getAttribute('aria-label') || '').trim().toLowerCase();
    if (!label || label.length > 30) continue;
    if (ACCEPT_TEXT.includes(label)) { try { el.click(); break; } catch (e) {} }
  }
  // Remove residual high-z fixed overlays that mention consent and cover the page.
  const vw = window.innerWidth, vh = window.innerHeight;
  for (const el of Array.from(document.querySelectorAll('div, section, aside'))) {
    try {
      const s = getComputedStyle(el);
      if (s.position !== 'fixed' && s.position !== 'sticky') continue;
      const r = el.getBoundingClientRect();
      if (r.width * r.height < vw * vh * 0.4) continue;
      const t = (el.id + ' ' + el.className + ' ' + (el.innerText || '')).toLowerCase();
      if (/cookie|consent|gdpr|privacy|cmp|banner/.test(t)) el.style.setProperty('display', 'none', 'important');
    } catch (e) {}
  }
  // Re-enable VERTICAL scrolling the banner may have locked — only when it IS
  // locked, and only overflow-y: overflow-x:hidden is load-bearing on motion
  // sites (it hides off-canvas marquee / translateX(-100%) elements; forcing it
  // visible widens the full-page shot and the encode then shrinks the whole page).
  try {
    const d = document.documentElement, b = document.body;
    const ovY = getComputedStyle(d).overflowY + ' ' + (b ? getComputedStyle(b).overflowY : '');
    if (/hidden|clip/.test(ovY)) {
      d.style.setProperty('overflow-y', 'auto', 'important');
      if (b) {
        b.style.setProperty('overflow-y', 'auto', 'important');
        if (getComputedStyle(b).position === 'fixed') b.style.setProperty('position', 'static', 'important');
      }
    }
  } catch (e) {}
  return true;
})()`;

const JS_DISABLE_ANIMATIONS = `(() => {
  const id = '__shelfy_no_anim__';
  if (!document.getElementById(id)) {
    const st = document.createElement('style');
    st.id = id;
    st.textContent = '*,*::before,*::after{animation:none!important;transition:none!important;' +
      'scroll-behavior:auto!important;caret-color:transparent!important;' +
      'animation-duration:0s!important;transition-duration:0s!important}';
    (document.head || document.documentElement).appendChild(st);
  }
  // Freeze autoplay media on a stable frame. Pausing at currentTime≈0 freezes a
  // background hero video on its FIRST frame — often solid black (pre-poster) —
  // so nudge into the clip before pausing; the settles downstream give the
  // seeked frame time to decode.
  for (const v of Array.from(document.querySelectorAll('video'))) {
    try {
      v.autoplay = false;
      if (v.readyState >= 2 && (v.currentTime || 0) < 0.5) {
        const d = Number.isFinite(v.duration) && v.duration > 0 ? v.duration : 0;
        try { v.currentTime = Math.min(2, d > 0 ? d * 0.25 : 1); } catch (e) {}
      }
      v.pause();
    } catch (e) {}
  }
  return true;
})()`;

// Reference sites rarely use native scroll. GSAP ScrollSmoother and Locomotive
// (v3 classic) VIRTUALIZE it: the real document is pinned (overflow:hidden, ~viewport
// height) and the content lives inside a transform-translated wrapper. That makes
// documentElement.scrollHeight ≈ viewport, so captureBeyondViewport grabs only the
// hero (and the QC model sees a valid hero → no re-capture → silent truncation). We
// neutralize the virtualizer — kill the instance when reachable, else strip the
// transform — and restore native scroll BEFORE autoscroll/measure, so window.scrollTo
// works and the height is real. Lenis uses NATIVE scroll (wrapper.scrollTo), so we
// deliberately leave it: its smooth animation is already handled by JS_DISABLE_ANIMATIONS.
const JS_NEUTRALIZE_VIRTUAL_SCROLL = `(() => {
  const out = { libs: [] };
  const restore = (el) => {
    if (!el) return;
    try {
      el.style.setProperty('transform', 'none', 'important');
      el.style.setProperty('translate', 'none', 'important');
      el.style.setProperty('position', 'static', 'important');
      el.style.setProperty('height', 'auto', 'important');
      el.style.setProperty('min-height', '0', 'important');
      el.style.setProperty('overflow', 'visible', 'important');
    } catch (e) {}
  };
  // 1) GSAP ScrollSmoother — kill the live instance if exposed (#smooth-content matrix3d).
  try {
    if (window.ScrollSmoother && typeof window.ScrollSmoother.get === 'function') {
      const s = window.ScrollSmoother.get();
      if (s) { try { s.scrollTop(0); } catch (e) {} try { s.kill(); } catch (e) {} out.libs.push('scrollsmoother'); }
    }
  } catch (e) {}
  const sc = document.querySelector('#smooth-content, [data-smooth-content], .smooth-content');
  if (sc) {
    restore(sc);
    const w = document.querySelector('#smooth-wrapper, [data-smooth-wrapper], .smooth-wrapper');
    if (w) restore(w);
    if (!out.libs.includes('scrollsmoother')) out.libs.push('scrollsmoother-dom');
  }
  // 2) Locomotive Scroll (v3 classic) — [data-scroll-container] translated up.
  const lc = document.querySelector('[data-scroll-container]');
  if (lc) { restore(lc); out.libs.push('locomotive'); }
  // 3) Generic virtual scroller: a tall body child translated while the document
  //    itself doesn't scroll. Catches bespoke implementations.
  try {
    const bodyOv = getComputedStyle(document.body).overflow;
    const htmlOv = getComputedStyle(document.documentElement).overflow;
    const docScrolls = document.documentElement.scrollHeight > window.innerHeight + 4;
    if (!docScrolls || /hidden|clip/.test(bodyOv + ' ' + htmlOv)) {
      for (const el of Array.from(document.body.children)) {
        try {
          const t = getComputedStyle(el).transform;
          if (el.scrollHeight > window.innerHeight * 1.2 && t && t !== 'none' && /matrix|translate/.test(t)) {
            restore(el);
            if (!out.libs.length) out.libs.push('generic-virtual');
          }
        } catch (e) {}
      }
    }
  } catch (e) {}
  // 4) Lenis is native; just make sure a 'stopped/locked' instance isn't clipping.
  try {
    const h = document.documentElement;
    if (h.classList.contains('lenis')) { out.libs.push('lenis-native'); h.classList.remove('lenis-stopped', 'lenis-locked'); }
  } catch (e) {}
  // 5) Restore VERTICAL scrollability — only when a virtualizer was found or the
  // document is genuinely locked, and only overflow-y: overflow-x:hidden is
  // load-bearing on motion sites (off-canvas marquee / translateX(-100%) elements
  // would widen the full-page shot and the encode would shrink the whole page).
  try {
    const d = document.documentElement, b = document.body;
    const lockedY = /hidden|clip/.test(getComputedStyle(d).overflowY + ' ' + (b ? getComputedStyle(b).overflowY : ''));
    if (out.libs.length || lockedY) {
      for (const el of [d, b]) {
        if (!el) continue;
        el.style.setProperty('overflow-y', 'visible', 'important');
        el.style.setProperty('overflow-x', 'hidden', 'important');
        el.style.setProperty('height', 'auto', 'important');
      }
      if (b && getComputedStyle(b).position === 'fixed') b.style.setProperty('position', 'static', 'important');
    }
  } catch (e) {}
  return out;
})()`;

// Detect <canvas> (WebGL/Three.js) so we can give the offscreen compositor an extra
// repaint+settle before the grab. getContext() is non-destructive when a context of
// that type already exists; on a context-less canvas it may allocate one (harmless).
const JS_DETECT_CANVAS = `(() => {
  try {
    const cs = Array.from(document.querySelectorAll('canvas'));
    if (!cs.length) return { canvas: false };
    let webgl = false, big = false;
    const vw = window.innerWidth, vh = window.innerHeight;
    for (const c of cs) {
      const r = c.getBoundingClientRect();
      if (r.width * r.height >= vw * vh * 0.5) big = true;
      try { if (c.getContext('webgl2') || c.getContext('webgl') || c.getContext('experimental-webgl')) webgl = true; } catch (e) {}
    }
    return { canvas: true, webgl, big };
  } catch (e) { return { canvas: false }; }
})()`;

// Incrementally scroll to trigger lazy-load / IntersectionObserver, forcing
// lazy <img> to load, then scroll back to top. Capped against infinite scroll.
function jsAutoScroll(maxHeightPx?: number): string {
  return `(async () => {
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const cap = ${Number(maxHeightPx) || DEFAULT_MAX_HEIGHT};
    const step = Math.max(200, Math.floor(window.innerHeight * 0.8));
    for (const img of Array.from(document.querySelectorAll('img'))) {
      try {
        img.loading = 'eager';
        const ds = img.getAttribute('data-src'); if (ds && !img.src) img.src = ds;
        const dss = img.getAttribute('data-srcset'); if (dss && !img.srcset) img.srcset = dss;
      } catch (e) {}
    }
    let y = 0, iters = 0, stable = 0, lastH = document.documentElement.scrollHeight;
    while (iters < ${SCROLL_ITER_CAP}) {
      window.scrollTo(0, y);
      await sleep(${SETTLE_INNER_MS});
      const h = document.documentElement.scrollHeight;
      if (h <= lastH) { stable++; } else { stable = 0; lastH = h; }
      y += step;
      iters++;
      if (y >= Math.min(h, cap) && stable >= 2) break;
      if (y >= cap) break;
    }
    window.scrollTo(0, 0);
    try { if (document.fonts && document.fonts.ready) await document.fonts.ready; } catch (e) {}
    // Wait for images revealed during the scroll to actually decode, so the grab
    // doesn't race ahead of half-painted media.
    try {
      await Promise.all(Array.from(document.images)
        .filter((im) => !im.complete && typeof im.decode === 'function')
        .map((im) => im.decode().catch(() => {})));
    } catch (e) {}
    return true;
  })()`;
}

// GSAP runs on rAF + inline styles, so neither the CSS kill in
// JS_DISABLE_ANIMATIONS nor Playwright's animations:'disabled' touches it. Worse:
// scroll-driven animations RESET when the autoscroll returns to top — scrubbed
// timelines map scroll→progress directly, and the widespread
// `toggleActions:'play none none reverse'` pattern rewinds on leave-back — and
// the full-page grab (captureBeyondViewport) performs NO real scrolling, so every
// below-the-fold section would be shot in its pre-animation state
// (opacity:0 / transform offset), i.e. blank bands. Run AFTER the
// autoscroll: force each trigger's animation to its END state and disable the
// trigger WITHOUT reverting, so the page is captured fully revealed. Pinned /
// scrubbed timelines land on their final frame too (a horizontal pin section
// still shows only its last panel, but content beats emptiness). Top-level
// entrance tweens still mid-flight are completed as well; infinite loops
// (marquees) are left alone. Best-effort + idempotent; no GSAP → zeros.
const JS_FORCE_SCROLL_ANIM_END = `(() => {
  const out = { st: 0, tweens: 0 };
  try {
    const ST = window.ScrollTrigger;
    if (ST && typeof ST.getAll === 'function') {
      for (const t of ST.getAll()) {
        try {
          const anim = t.animation;
          if (anim) { try { anim.progress(1); anim.pause(); } catch (e) {} }
          // disable(reset=false): stop tracking scroll WITHOUT reverting inline
          // styles, so the end state above sticks for the screenshot.
          try { t.disable(false); } catch (e) {}
          out.st++;
        } catch (e) {}
      }
    }
  } catch (e) {}
  try {
    const g = window.gsap;
    if (g && g.globalTimeline && typeof g.globalTimeline.getChildren === 'function') {
      for (const tw of g.globalTimeline.getChildren(false, true, true)) {
        try {
          if (typeof tw.repeat === 'function' && tw.repeat() === -1) continue; // marquee/loop
          if (typeof tw.isActive === 'function' && tw.isActive()) { tw.progress(1); tw.pause(); out.tweens++; }
        } catch (e) {}
      }
    }
  } catch (e) {}
  return out;
})()`;

// Convert fixed/sticky elements to static so they neither cover content nor get
// duplicated across stitched slices. Idempotent (guarded by a data attribute).
const JS_NEUTRALIZE_FIXED = `(() => {
  const vw = window.innerWidth, vh = window.innerHeight;
  for (const el of Array.from(document.querySelectorAll('*'))) {
    try {
      const s = getComputedStyle(el);
      if (s.position !== 'fixed' && s.position !== 'sticky') continue;
      // Keep full-screen hero backgrounds and any element holding a <canvas>
      // (WebGL/Three.js): flattening them to static collapses or moves the scene
      // right before the shot. captureBeyondViewport renders a fixed element once
      // at its position anyway, so leaving these put is the correct full-page result.
      const r = el.getBoundingClientRect();
      const coversViewport = r.width >= vw * 0.9 && r.height >= vh * 0.9;
      const hasCanvas = el.tagName === 'CANVAS' || !!el.querySelector('canvas');
      if (hasCanvas || coversViewport) continue;
      // Small FIXED widgets — custom cursors (ubiquitous on awwwards-style sites),
      // cursor followers, badges — must be HIDDEN, not staticized: switching them
      // to static drops them into the flow at the top of the page as artifacts.
      // Sticky elements are in-flow content (section labels etc.) → keep flattening.
      if (s.position === 'fixed' && r.width < 160 && r.height < 160) {
        el.style.setProperty('display', 'none', 'important');
        continue;
      }
      if (!el.hasAttribute('data-shelfy-pos')) el.setAttribute('data-shelfy-pos', s.position);
      el.style.setProperty('position', 'static', 'important');
    } catch (e) {}
  }
  return true;
})()`;

const JS_MEASURE = `(() => {
  const d = document.documentElement, b = document.body;
  let scrollHeight = Math.max(
    d.scrollHeight, b ? b.scrollHeight : 0,
    d.offsetHeight, b ? b.offsetHeight : 0,
    d.clientHeight,
  );
  // Belt-and-suspenders for a virtual-scroll wrapper that survived neutralization:
  // take the tallest known scroll/translate container, plus the lowest body-child
  // bottom edge (handles absolutely-positioned content the document height misses).
  try {
    for (const el of Array.from(document.querySelectorAll('#smooth-content, [data-scroll-container], [data-smooth-content]'))) {
      scrollHeight = Math.max(scrollHeight, el.scrollHeight || 0, Math.ceil(el.getBoundingClientRect().height) || 0);
    }
    for (const el of Array.from(document.body.children)) {
      scrollHeight = Math.max(scrollHeight, Math.ceil(el.getBoundingClientRect().bottom + (window.scrollY || 0)));
    }
  } catch (e) {}
  return { scrollHeight, viewportH: window.innerHeight, title: document.title, dpr: window.devicePixelRatio || 1 };
})()`;

// ─── F2: capture ────────────────────────────────────────────────────────────

// Wait for the first load to "settle" (or timeout), then return. `did-finish-load`
// alone is unreliable: heavy/animated sites keep long-lived connections open so
// the load event can lag for tens of seconds (or never fire), which previously
// stalled every capture for the full NAV_TIMEOUT. So we settle on the EARLIEST
// strong signal — did-finish-load / did-stop-loading — and, failing those, on
// `dom-ready` plus a short grace (the page has a usable DOM; autoscroll + settles
// downstream still give lazy content time). Rejects only on a hard main-frame
// navigation failure. NAV_TIMEOUT_MS is the absolute hard cap.
function waitForLoad(wc: WebContents): Promise<void> {
  return new Promise((resolve, reject) => {
    let done = false;
    let graceTimer: ReturnType<typeof setTimeout> | null = null;
    const finish = <A>(fn: (arg: A) => void, arg: A): void => {
      if (done) return;
      done = true;
      cleanup();
      fn(arg);
    };
    const onStop = (): void => finish(resolve, undefined); // did-finish-load / did-stop-loading
    const onDomReady = (): void => {
      // DOM is parsed; give sync sub-resources a brief grace, then settle even if
      // the full load event never arrives.
      if (!graceTimer) graceTimer = setTimeout(() => finish(resolve, undefined), 2_000);
    };
    const onFail = (
      _e: ElectronEvent,
      code: number,
      desc: string,
      _url: string,
      isMainFrame: boolean,
    ): void => {
      // Ignore sub-frame failures and benign aborts (-3 ERR_ABORTED on redirects).
      if (isMainFrame && code !== -3)
        finish(reject, new Error(`Navigation failed (${code}): ${desc}`));
    };
    const timer = setTimeout(() => finish(resolve, undefined), NAV_TIMEOUT_MS); // settle on what loaded
    function cleanup(): void {
      clearTimeout(timer);
      if (graceTimer) clearTimeout(graceTimer);
      wc.removeListener('did-finish-load', onStop);
      wc.removeListener('did-stop-loading', onStop);
      wc.removeListener('dom-ready', onDomReady);
      wc.removeListener('did-fail-load', onFail);
    }
    wc.on('did-finish-load', onStop);
    wc.on('did-stop-loading', onStop);
    wc.on('dom-ready', onDomReady);
    wc.on('did-fail-load', onFail);
  });
}

// Full-page screenshot via the DevTools protocol. captureBeyondViewport renders
// the whole scrollable page WITHOUT enlarging the layout viewport (so vh-based
// sections aren't stretched), and clip.scale:1 yields a 1× (DPR-independent)
// PNG buffer. Attaches/detaches the debugger around the single call.
async function captureFullPage(
  wc: WebContents,
  width: number,
  height: number,
): Promise<Buffer | null> {
  let attached = false;
  try {
    try {
      wc.debugger.attach('1.3');
      attached = true;
    } catch {
      // Already attached (e.g. devtools) → reuse the existing session.
      attached = false;
    }
    const res = (await withTimeout(
      wc.debugger.sendCommand('Page.captureScreenshot', {
        format: 'png',
        captureBeyondViewport: true,
        fromSurface: true,
        clip: { x: 0, y: 0, width, height, scale: 1 },
      }),
      CAPTURE_TIMEOUT_MS,
      'cdp-capture',
    )) as { data?: string };
    return res && res.data ? Buffer.from(res.data, 'base64') : null;
  } finally {
    if (attached) {
      try {
        wc.debugger.detach();
      } catch {}
    }
  }
}

// Options accepted by capturePage(). All optional; each fills its own default.
interface CapturePageOptions {
  partition?: string;
  maxHeightPx?: number;
  format?: string;
  quality?: number;
  signal?: AbortSignal;
  onStep?: (label: string, delta: number, tot: number) => void;
  settleBeforeShotMs?: number;
  captureStamp?: number;
}

// The live pageCtx contract returned by capturePage(). `evaluate`/`dispose` stay
// usable until dispose() is called; the metadata fields describe the captured shot.
interface PageCtx {
  screenshotPath: string;
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

// Shapes returned by the in-page JS_* probe scripts (run via executeJavaScript).
interface VirtualScrollResult {
  libs: string[];
}
interface ScrollAnimResult {
  st: number;
  tweens: number;
}
interface MeasureResult {
  scrollHeight: number;
  viewportH: number;
  title: string;
  dpr: number;
}
interface CanvasResult {
  canvas: boolean;
  webgl?: boolean;
  big?: boolean;
}

/**
 * F2 entry point. Render the URL and return a LIVE pageCtx.
 * @param {string} url
 * @param {{ partition?: string, maxHeightPx?: number, format?: 'webp'|'png',
 *           quality?: number, signal?: AbortSignal }} [opts]
 * @returns {Promise<{
 *   screenshotPath: string, width: number, height: number, finalUrl: string,
 *   title: string, capped: boolean, html: string, headers: object,
 *   evaluate: (code: string) => Promise<any>, dispose: () => Promise<void>
 * }>}
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
    // ALWAYS wait this long after the page settles, before grabbing the frame, so
    // slow heroes (WebGL/Three.js), hydration and lazy media have time to render.
    // The orchestrator raises it (e.g. 30s) when re-capturing a page the QC model
    // flagged as black/loading.
    settleBeforeShotMs = 15_000,
    // Epoch prefixed onto the screenshot filename so repeated captures of a site
    // don't overwrite each other's files (version history). The orchestrator
    // passes the capture's epoch (== web_captured_at).
    captureStamp,
  }: CapturePageOptions = {},
): Promise<PageCtx> {
  throwIfAborted(signal);
  assertSafeUrl(url); // reject non-http(s) / blocked host before navigating

  const ses = session.fromPartition(partition);
  // Create the temp dir BEFORE the offscreen window: a mkdtempSync throw (EACCES /
  // EMFILE / disk full) must not happen AFTER `new BrowserWindow`, or the window
  // would leak (dispose() isn't defined yet at that point).
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-web-'));
  const win: TaggedCaptureWindow = new BrowserWindow({
    show: false,
    width: VIEWPORT_W,
    height: VIEWPORT_H,
    webPreferences: {
      session: ses,
      // Offscreen rendering: Chromium paints the page into an off-screen buffer
      // WITHOUT ever mapping a real window into the OS window server. This is
      // what makes requestAnimationFrame fire, document.fonts.ready resolve and
      // webContents.capturePage() return real frames on a non-visible window —
      // a plain show:false window is never composited, so the capture stalls to
      // its timeouts and every page is skipped. Crucially, an offscreen window
      // can NEVER overlay the app or steal mouse/keyboard input (which a mapped
      // showInactive() window can, freezing the UI). See report.
      offscreen: true,
      sandbox: true,
      contextIsolation: true,
      nodeIntegration: false,
      images: true,
      javascript: true,
      backgroundThrottling: false,
    },
  });
  // main.js/hardenWebContents must honor this flag to let the capture window
  // navigate to an external host (its webContents type is 'window'). See report.
  win.__shelfyCapture = true;

  const wc = win.webContents;
  wc.setAudioMuted(true);
  // Drive the offscreen compositor: without an active frame rate Chromium may
  // not paint a non-visible offscreen surface, which would leave capturePage()
  // empty. A modest rate is plenty (we grab a single still per page).
  try {
    wc.setFrameRate(30);
  } catch {}
  // Subscribing to 'paint' (even as a no-op) keeps the offscreen pipeline
  // actively producing frames on all platforms.
  try {
    wc.on('paint', () => {});
  } catch {}

  // Some pages (or a stray manifest/asset URL) trigger a download during capture,
  // which would pop the native "Save As" dialog. Cancel any download started by
  // THIS capture window — we only ever want to render, never to save files.
  const onWillDownload = (
    _e: ElectronEvent,
    item: DownloadItem,
    webContents: WebContents,
  ): void => {
    if (webContents === wc) {
      try {
        item.cancel();
      } catch {}
    }
  };

  // Capture the main navigation response headers (best-effort; F4 tech detect).
  // onHeadersReceived is a BLOCKING webRequest handler: Electron holds every
  // response until the listener invokes `callback`. The previous version never
  // called it, so EVERY request on this session (including the capture's own
  // page load) hung forever → did-finish-load/dom-ready never fired → the whole
  // capture timed out and every page was skipped. We now always call back,
  // passing the headers through unchanged.
  let navHeaders: Record<string, string> = {};
  const onHeaders = (
    details: OnHeadersReceivedListenerDetails,
    callback: (response: HeadersReceivedResponse) => void,
  ): void => {
    try {
      if (details.resourceType === 'mainFrame' && details.responseHeaders) {
        navHeaders = {};
        for (const [k, v] of Object.entries(details.responseHeaders)) {
          navHeaders[k.toLowerCase()] = Array.isArray(v) ? v.join(', ') : v;
        }
      }
    } catch {
      /* never let header bookkeeping break the response */
    }
    callback({ cancel: false, responseHeaders: details.responseHeaders });
  };

  // Abort plumbing: tear the window down if the caller cancels.
  const onAbort = (): void => {
    try {
      if (!win.isDestroyed()) win.destroy();
    } catch {}
  };

  // Define dispose() BEFORE attaching any session / window / signal listener: with
  // it defined first, the listeners are registered inside the main try block where
  // the existing catch→dispose path cleans everything up (window, header hook, temp
  // dir) on any failure. (tmpDir itself is created above, before the window, so a
  // mkdtemp throw can't leak the window either.)
  let disposed = false;
  const dispose = async (): Promise<void> => {
    if (disposed) return;
    disposed = true;
    try {
      ses.webRequest.onHeadersReceived(null);
    } catch {}
    try {
      ses.removeListener('will-download', onWillDownload);
    } catch {}
    if (signal) signal.removeEventListener('abort', onAbort);
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch {}
    try {
      if (!win.isDestroyed()) win.destroy();
    } catch {}
  };

  // Optional timing trace (SHELFY_CAPTURE_DEBUG=1) to pinpoint which capture
  // step stalls on a given environment/site.
  const T0 = Date.now();
  const dbg = !!process.env.SHELFY_CAPTURE_DEBUG;
  let lastMs = 0;
  const mark = (label: string): void => {
    const ms = Date.now() - T0;
    if (dbg) console.log(`[webcapture] +${ms}ms ${label} :: ${url}`);
    // Report the DELTA since the previous step so a stall is obvious (which step
    // ate the time), straight into the caller's progress timeline.
    try {
      onStep?.(label, ms - lastMs, ms);
    } catch {}
    lastMs = ms;
  };

  try {
    mark('start');

    // Register the session / window / abort listeners INSIDE the try so any
    // throw from here on (or from the work below) routes through catch→dispose
    // and never leaks the offscreen window or the session-global header hook.
    try {
      ses.on('will-download', onWillDownload);
    } catch {}
    try {
      ses.webRequest.onHeadersReceived(onHeaders);
    } catch {}
    if (signal) {
      if (signal.aborted) {
        throw Object.assign(new Error('AbortError'), { name: 'AbortError' });
      }
      signal.addEventListener('abort', onAbort, { once: true });
    }

    // 1) LOAD
    const loadPromise = waitForLoad(wc);
    wc.loadURL(url, { userAgent: DESKTOP_UA });
    await loadPromise;
    mark('loaded');
    throwIfAborted(signal);
    const finalUrl = wc.getURL() || url;
    await sleep(SETTLE_MS);

    // 2) DISMISS COOKIE BANNER (best-effort, idempotent)
    await safeEval(wc, JS_DISMISS_COOKIES);
    mark('cookies-dismissed');
    await sleep(SETTLE_MS);

    // 3) KILL ANIMATIONS / TRANSITIONS, freeze autoplay media
    await safeEval(wc, JS_DISABLE_ANIMATIONS);
    mark('animations-killed');

    // 3b) NEUTRALIZE VIRTUALIZED SCROLL (GSAP ScrollSmoother / Locomotive / bespoke).
    // Must run BEFORE the autoscroll+measure: it restores native scroll so the next
    // window.scrollTo actually moves the page and documentElement.scrollHeight reflects
    // the real content height (otherwise captureBeyondViewport grabs only the hero).
    // Lenis is native and deliberately left intact.
    const vscroll = await safeEval<VirtualScrollResult>(wc, JS_NEUTRALIZE_VIRTUAL_SCROLL, {
      fallback: { libs: [] },
    });
    mark(`virtual-scroll(${(vscroll && vscroll.libs && vscroll.libs.join(',')) || 'none'})`);
    await sleep(SETTLE_MS);

    // 4) SCROLL for lazy-load + on-scroll animations, then return to top
    await safeEval(wc, jsAutoScroll(maxHeightPx), { ms: SCROLL_EVAL_TIMEOUT_MS });
    mark('scrolled');
    // 4a) GSAP/ScrollTrigger reveals REVERSED on the return to top (and the CSS
    // kill can't touch GSAP) → force them to their END state so below-the-fold
    // sections aren't captured at opacity:0.
    const fin = await safeEval<ScrollAnimResult>(wc, JS_FORCE_SCROLL_ANIM_END, {
      fallback: { st: 0, tweens: 0 },
    });
    if (fin && (fin.st || fin.tweens)) mark(`gsap-finished(st=${fin.st},tw=${fin.tweens})`);
    await sleep(SETTLE_MS);
    throwIfAborted(signal);

    // 4b) PRE-SHOT WAIT — always give the page a fixed grace period to finish
    // loading/animating before the grab (slow WebGL heroes, hydration, lazy media).
    if (settleBeforeShotMs > 0) {
      await sleepAbortable(settleBeforeShotMs, signal);
      mark(`pre-shot-wait(${settleBeforeShotMs}ms)`);
      throwIfAborted(signal);
    }

    // Grab the LIVE rendered DOM (post-hydration) for F3/F7 SPA support.
    const html = (await safeEval<string>(wc, 'document.documentElement.outerHTML', {
      fallback: '',
    })) as string;
    mark(`html(${(html || '').length})`);

    // 5) MEASURE + decide single-frame vs slice/stitch
    const measure = (await safeEval<MeasureResult>(wc, JS_MEASURE, {
      fallback: { scrollHeight: VIEWPORT_H, viewportH: VIEWPORT_H, title: '', dpr: 1 },
    })) as MeasureResult;
    const scrollHeight = Math.max(VIEWPORT_H, Number(measure.scrollHeight) || VIEWPORT_H);
    const viewportH = Number(measure.viewportH) || VIEWPORT_H;
    const title = typeof measure.title === 'string' ? measure.title : '';
    // Output is 1× logical pixels (clip.scale:1 below), so the only ceiling is the
    // WebP per-side limit (16383); maxHeightPx (12000) is comfortably under it.
    const targetH = Math.min(scrollHeight, maxHeightPx);
    const capped = scrollHeight > maxHeightPx;
    mark(`measured(h=${scrollHeight}→${targetH})`);

    const tmpPng = path.join(tmpDir, 'capture.png');

    // Flatten fixed/sticky → static (so a sticky header isn't repeated down the
    // full-page shot) and return to the top before the grab.
    await safeEval(wc, JS_NEUTRALIZE_FIXED);
    await safeEval(wc, 'window.scrollTo(0, 0)');
    await sleep(SETTLE_MS);
    throwIfAborted(signal);
    mark('pre-capture');

    // WebGL/Three.js heroes: Electron offscreen rendering can leave the canvas stuck
    // on its first frame (electron#39859) → a black/empty grab. Force a compositor
    // repaint and wait two animation frames so a fresh composited surface exists for
    // captureBeyondViewport (which reads the surface, not the WebGL drawing buffer).
    const vis = await safeEval<CanvasResult>(wc, JS_DETECT_CANVAS, { fallback: { canvas: false } });
    if (vis && vis.canvas) {
      try {
        wc.invalidate();
      } catch {}
      await safeEval(wc, 'new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))', {
        ms: RAF_TIMEOUT_MS,
      });
      await sleep(SETTLE_MS);
      mark(`canvas(webgl=${!!vis.webgl})`);
    }

    // PRIMARY — full-page capture via the DevTools protocol. captureBeyondViewport
    // keeps the LAYOUT viewport at the window size (1280×900), so `100vh` sections
    // render at their true height instead of being stretched to the whole page;
    // clip.scale:1 forces a DPR-independent 1× output (no retina 2× doubling).
    let ok = false;
    try {
      const buf = await captureFullPage(wc, VIEWPORT_W, Math.ceil(targetH));
      if (buf && buf.length) {
        fs.writeFileSync(tmpPng, buf);
        ok = true;
        mark('captured(cdp)');
      }
    } catch (err) {
      mark(`cdp-failed(${(err as Error)?.message || err})`);
    }

    if (!ok) {
      // FALLBACK — slice + ffmpeg vstack at the REAL viewport height (so vh layouts
      // stay correct), then downscale on encode. Best-effort if CDP is unavailable.
      await safeEval(wc, 'window.scrollTo(0, 0)');
      const slicePaths: string[] = [];
      const stepH = viewportH - SLICE_OVERLAP;
      let i = 0;
      for (let y = 0; y < targetH; y += stepH) {
        throwIfAborted(signal);
        await safeEval(wc, `window.scrollTo(0, ${y})`);
        await sleep(SETTLE_INNER_MS);
        await safeEval(wc, 'new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))', {
          ms: RAF_TIMEOUT_MS,
        });
        const image = await withTimeout(wc.capturePage(), CAPTURE_TIMEOUT_MS, 'capturePage');
        let png = image;
        const size = image.getSize();
        const scale = size.height / viewportH || 1; // device px per CSS px for THIS image
        const topCrop = i === 0 ? 0 : Math.round(Math.min(SLICE_OVERLAP, viewportH - 1) * scale);
        const remaining = Math.round((Math.ceil(targetH) - y) * scale) - topCrop;
        const cropH = Math.max(1, Math.min(size.height - topCrop, remaining));
        try {
          png = image.crop({ x: 0, y: topCrop, width: size.width, height: cropH });
        } catch {
          png = image;
        }
        const sp = path.join(tmpDir, `slice_${i}.png`);
        fs.writeFileSync(sp, png.toPNG());
        slicePaths.push(sp);
        i++;
        if (i > SCROLL_ITER_CAP) break;
      }
      const stitched = path.join(tmpDir, 'stitched.png');
      await stitchSlices(slicePaths, stitched, 'png', quality, signal);
      fs.renameSync(stitched, tmpPng);
      mark('captured(slices)');
    }

    // 6) ENCODE → final WebP/PNG at a per-URL path made collision-safe under
    // concurrency. The orchestrator passes the SAME captureStamp to every page
    // of a job and runs several capturePage() in parallel; two DISTINCT
    // discovered URLs can redirect to the SAME finalUrl (e.g. /work and /work/,
    // or a locale redirect), so screenshotPathForUrl(finalUrl, stamp) — which is
    // deterministic from (finalUrl, stamp) — would resolve to the SAME file for
    // both. The two encodeImage()/probeImageSize() calls would then race on one
    // output (torn read, 0×0 size, loser's metadata kept). The per-call tmpDir
    // is unique per capture, so folding its basename into the hashed key gives
    // each concurrent capture its own on-disk path. The caller consumes
    // ctx.screenshotPath directly (it never re-derives it from finalUrl), so
    // this stays transparent to the orchestrator.
    const pathNonce = `${finalUrl}#cap=${path.basename(tmpDir)}`;
    const screenshotPath = screenshotPathForUrl(pathNonce, format, captureStamp);
    await encodeImage(tmpPng, screenshotPath, format, quality, signal);
    const { width, height } = await probeImageSize(screenshotPath, signal);

    return {
      screenshotPath,
      width,
      height,
      finalUrl,
      title,
      capped,
      html,
      headers: navHeaders,
      // Canvas/WebGL probe result — the orchestrator lowers per-site capture
      // parallelism on WebGL-heavy sites (GPU contention).
      webglHeavy: !!(vis && vis.canvas && (vis.webgl || vis.big)),
      // F4 uses this for computed styles / window globals. Stays usable until
      // dispose(). Always passes userGesture=true so click()-style probes work.
      evaluate: (code: string): Promise<unknown> => {
        if (disposed || win.isDestroyed()) return Promise.reject(new Error('pageCtx disposed'));
        if (typeof code !== 'string')
          return Promise.reject(new Error('evaluate expects a code string'));
        return wc.executeJavaScript(code, true);
      },
      dispose,
    };
  } catch (err) {
    await dispose();
    throw err;
  }
}

// Options accepted by captureMany(): the per-page hooks plus the capturePage opts.
interface CaptureManyOptions extends CapturePageOptions {
  onPage?: (ctx: PageCtx) => unknown | Promise<unknown>;
  keepAlive?: boolean;
}

/**
 * Optional convenience: capture pages sequentially (one window at a time, to
 * cap RAM/GPU).
 *
 * keepAlive contract — DISPOSE BY DEFAULT. Each capturePage holds an offscreen
 * BrowserWindow + temp dir + session handler, so a forgotten ctx leaks a hidden
 * window. By default every ctx is disposed before returning (after `onPage` runs,
 * if given), and the returned array carries only the post-dispose metadata — the
 * live `evaluate`/`dispose` handles are no longer usable. Pass `keepAlive:true`
 * ONLY if you take ownership of disposing every returned ctx yourself.
 * @param {Array<{url:string}>|string[]} pages
 */
async function captureMany(
  pages: (string | { url: string })[],
  opts: CaptureManyOptions = {},
): Promise<PageCtx[]> {
  const { onPage, keepAlive = false, ...captureOpts } = opts;
  const list = (pages || [])
    .map((p) => (typeof p === 'string' ? p : p && p.url))
    .filter(Boolean) as string[];
  const results: PageCtx[] = [];
  for (const u of list) {
    throwIfAborted(captureOpts.signal);
    const ctx = await capturePage(u, captureOpts);
    try {
      if (typeof onPage === 'function') await onPage(ctx);
    } finally {
      if (!keepAlive) await ctx.dispose();
    }
    results.push(ctx);
  }
  return results;
}

export {
  // F1
  discoverPages,
  normalizeInputUrl,
  normalizeUrl,
  scorePath,
  selectRepresentative,
  extractLinks,
  parseSitemapXml,
  // F2
  capturePage,
  captureMany,
  screenshotPathForUrl,
  fetchImageToWebp,
  getCaptureDir,
};

// Internals reused by the Playwright capture engine (webcapture-playwright.js)
// so the two engines share the exact same on-disk layout, ffmpeg encode and
// battle-tested in-page DOM-prep scripts (cookies/animations/virtual-scroll/fixed).
export const _internals = {
  encodeImage,
  encodeImageChunks,
  encodeFrames,
  probeImageSize,
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
  JS_MEASURE,
  jsAutoScroll,
};
