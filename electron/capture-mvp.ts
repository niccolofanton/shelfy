// SPIKE / MVP — "capture-on-view": save the bytes the browser already downloaded
// to render the feed, with ZERO extra network requests, immune to the CORS
// canvas-taint (validated separately in scripts/capture-spike/).
//
// Two gates:
//   SHELFY_CAPTURE_MVP=1   → attach CDP, capture image bytes, dump to
//                            <userData>/capture-mvp/ for inspection (debug).
//   SHELFY_CAPTURE_WRITE=1 → ALSO persist into the real DB + assets/: correlate
//                            each captured image to its post by file basename and
//                            write paths under the completeness rule —
//                              single image  → image_path + thumbnail_path (COMPLETE)
//                              carousel/video → thumbnail (cover) only (not complete)
//
// Reuses db.updatePaths/updateMediaPath (COALESCE/upsert — never destroys existing
// data), db.captureFindSlot for correlation (the LIVE rw connection, so it sees
// posts the sync just inserted), and thumbs.microThumbDataUri for the blur-up.
// Images that arrive before their post is upserted are retried, not dropped.
// Delete this file + its call site in main.ts (+ db.captureFindSlot) to remove it.

import { app } from 'electron';
import type { WebContents } from 'electron';
import path from 'path';
import fs from 'fs';
import * as db from './db';
import { microThumbDataUri } from './thumbs';

const ENABLED = process.env.SHELFY_CAPTURE_MVP === '1';
const WRITE = process.env.SHELFY_CAPTURE_WRITE === '1';

// Media CDNs we care about (keeps favicons / UI chrome out of the debug sample).
const MEDIA_HOST_RE = /(^|\.)(cdninstagram\.com|fbcdn\.net|pbs\.twimg\.com|pinimg\.com)$/i;
const MAX_SAVE = 60; // cap debug files written to capture-mvp/

const attached = new WeakSet<WebContents>();

// ── helpers ────────────────────────────────────────────────────────────────
function outDir(): string {
  const dir = path.join(app.getPath('userData'), 'capture-mvp');
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}
function hostOf(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return '';
  }
}
function safeUrl(url: string): string {
  try {
    const u = new URL(url);
    return `${u.origin}${u.pathname}`;
  } catch {
    return String(url).split(/[?#]/)[0].slice(0, 200);
  }
}
function basenameNoExt(url: string): string {
  try {
    return (new URL(url).pathname.split('/').pop() || '').replace(/\.[a-z0-9]+$/i, '');
  } catch {
    return '';
  }
}
function imgExtFromMime(mime: string): string {
  if (/png/i.test(mime)) return 'png';
  if (/webp/i.test(mime)) return 'webp';
  if (/gif/i.test(mime)) return 'gif';
  return 'jpg';
}

// Which platform, and is this a POST media image (vs avatar / UI icon)?
function platformOf(url: string): 'instagram' | 'twitter' | 'pinterest' | 'other' {
  if (/cdninstagram\.com|fbcdn\.net/.test(url)) return 'instagram';
  if (/twimg\.com/.test(url)) return 'twitter';
  if (/pinimg\.com/.test(url)) return 'pinterest';
  return 'other';
}
function isPostMedia(url: string): boolean {
  const p = platformOf(url);
  if (p === 'instagram') return /\/t51\.\d+-15\//.test(url); // -19 = profile pics
  if (p === 'twitter')
    return /\/(media|amplify_video_thumb)\//.test(url) && !/profile_images/.test(url);
  return false;
}

// Filename sanitization mirroring downloader.ts so files line up with what the
// on-demand downloader would write (no double-download later).
function safeIdent(v: unknown): string {
  return (
    String(v ?? '')
      .replace(/[^A-Za-z0-9_-]/g, '_')
      .slice(0, 128) || 'unknown'
  );
}
const KNOWN_PLATFORMS = new Set(['instagram', 'twitter', 'pinterest', 'web', 'manual']);
function safePlatform(p: string): string {
  return KNOWN_PLATFORMS.has(p) ? p : safeIdent(p);
}
function assetDir(sub: string): string {
  const d = path.join(app.getPath('userData'), 'assets', sub);
  fs.mkdirSync(d, { recursive: true });
  return d;
}

// ── correlation ──────────────────────────────────────────────────────────────
// Slot shape mirrors db.captureFindSlot's return (kept local for isSingleImage).
type Slot = {
  postId: string;
  platform: string;
  shortcode: string | null;
  mediaType: string;
  mediaCount: number;
  position: number;
  thumbPath: string | null;
  imgPath: string | null;
};

// ── persistence ──────────────────────────────────────────────────────────────
const stats = { complete: 0, thumbOnly: 0, skipNoPost: 0, alreadyLocal: 0 };

function isSingleImage(s: Slot): boolean {
  return s.mediaCount <= 1 && (s.mediaType === 'image' || s.mediaType === 'images');
}

type PersistResult = 'saved' | 'already' | 'no-post';

function persistCapture(url: string, mime: string, buf: Buffer): PersistResult {
  // Correlate via the LIVE rw connection (db.captureFindSlot) so it sees posts the
  // sync just inserted; a separate read-only connection lagged and mis-skipped.
  const slot = db.captureFindSlot(basenameNoExt(url));
  if (!slot) return 'no-post'; // image arrived before its data — caller retries
  // Skip only if this post ALREADY has the local file we'd write — checked against
  // the DB (requisito #5: re-save when the thumb isn't local / the link expired).
  // After a delete the paths are NULL again, so this correctly re-captures.
  const alreadyLocal = isSingleImage(slot) ? !!slot.imgPath : !!slot.thumbPath;
  if (alreadyLocal) {
    stats.alreadyLocal += 1;
    return 'already';
  }

  const ext = imgExtFromMime(mime);
  const ident = safeIdent(slot.shortcode || slot.postId);
  const plat = safePlatform(slot.platform);

  if (isSingleImage(slot)) {
    // Single image: the captured file IS the post — image + thumbnail → COMPLETE.
    const dest = path.join(assetDir('images'), `${plat}-${ident}-${slot.position || 0}.${ext}`);
    fs.writeFileSync(dest, buf);
    db.updateMediaPath(slot.postId, slot.position || 0, dest, 'image');
    db.updatePaths(slot.postId, { imagePath: dest, thumbnailPath: dest });
    stats.complete += 1;
    console.log(
      `[capture-mvp] SAVED complete  ${slot.platform}:${slot.postId} → ${path.basename(dest)}`,
    );
    blurLater(slot.postId, dest);
  } else {
    // Carousel/video: cover only → thumbnail. Post stays incomplete (rest is on-demand).
    const dest = path.join(assetDir('thumbnails'), `${plat}-${ident}.${ext}`);
    fs.writeFileSync(dest, buf);
    db.updatePaths(slot.postId, { thumbnailPath: dest });
    stats.thumbOnly += 1;
    console.log(
      `[capture-mvp] SAVED cover     ${slot.platform}:${slot.postId} (${slot.mediaType}) → ${path.basename(dest)}`,
    );
    blurLater(slot.postId, dest);
  }
  return 'saved';
}

// Images can arrive over CDP slightly BEFORE the sync has upserted the post's JSON
// (race). Instead of dropping them, buffer no-post captures and retry — a moment
// later captureFindSlot finds the post and we persist. This is what closes the gap
// to ~100% on a clean sync.
type Pending = { url: string; mime: string; buf: Buffer; attempts: number };
const retryQueue: Pending[] = [];
const RETRY_MAX = 8; // give up after this many retries (post never appeared)
const RETRY_INTERVAL_MS = 1200;
const RETRY_CAP = 800; // bound memory
let retryTimer: ReturnType<typeof setInterval> | null = null;

function tryPersist(url: string, mime: string, buf: Buffer): void {
  if (persistCapture(url, mime, buf) !== 'no-post') return;
  if (retryQueue.length < RETRY_CAP) retryQueue.push({ url, mime, buf, attempts: 0 });
  if (!retryTimer) {
    retryTimer = setInterval(() => {
      if (!retryQueue.length) return;
      const batch = retryQueue.splice(0, retryQueue.length);
      for (const it of batch) {
        if (persistCapture(it.url, it.mime, it.buf) !== 'no-post') continue;
        if (++it.attempts < RETRY_MAX) retryQueue.push(it);
        else stats.skipNoPost += 1; // gave up: the post never showed up in the DB
      }
    }, RETRY_INTERVAL_MS);
  }
}

// Blur-up placeholder (mirrors the downloader): fire-and-forget; the startup
// backfill catches any we miss.
function blurLater(postId: string, src: string): void {
  microThumbDataUri(src)
    .then((uri) => {
      if (uri) db.updatePaths(postId, { thumbBlur: uri });
    })
    .catch(() => {});
}

// ── CDP attach ───────────────────────────────────────────────────────────────
export function attachCaptureSpike(wc: WebContents): void {
  if (!ENABLED || attached.has(wc)) return;
  attached.add(wc);

  const id = wc.id;
  const dbg = wc.debugger;
  try {
    dbg.attach('1.3');
  } catch (err) {
    console.warn('[capture-mvp] debugger.attach failed:', String((err as Error)?.message || err));
    return;
  }

  const pending = new Map<string, { url: string; mime: string }>();
  let saved = 0;
  let seen = 0;
  let misses = 0;

  dbg.on('message', (_event, method, params) => {
    if (method === 'Network.responseReceived') {
      const p = params as {
        requestId: string;
        type?: string;
        response?: { url?: string; mimeType?: string };
      };
      const url = p.response?.url || '';
      const mime = p.response?.mimeType || '';
      const isImage = p.type === 'Image' || /^image\//i.test(mime);
      if (!isImage || !/^https?:/i.test(url) || !MEDIA_HOST_RE.test(hostOf(url))) return;
      pending.set(p.requestId, { url, mime });
      return;
    }

    if (method === 'Network.loadingFinished') {
      const p = params as { requestId: string };
      const meta = pending.get(p.requestId);
      if (!meta) return;
      pending.delete(p.requestId);
      seen += 1;

      dbg
        .sendCommand('Network.getResponseBody', { requestId: p.requestId })
        .then((res: { body: string; base64Encoded: boolean }) => {
          const buf = Buffer.from(res.body, res.base64Encoded ? 'base64' : 'utf8');
          if (saved < MAX_SAVE) {
            const name = `${String(saved).padStart(3, '0')}-${hostOf(meta.url)}.${imgExtFromMime(meta.mime)}`;
            try {
              fs.writeFileSync(path.join(outDir(), name), buf);
              saved += 1;
            } catch {}
          }
          try {
            fs.appendFileSync(
              path.join(outDir(), 'urls.txt'),
              `${meta.mime}\t${buf.length}\t${safeUrl(meta.url)}\n`,
            );
          } catch {}

          // The real thing: persist into DB + assets (with retry for the race).
          if (WRITE && isPostMedia(meta.url)) {
            try {
              tryPersist(meta.url, meta.mime, buf);
            } catch (e) {
              console.warn('[capture-mvp] persist error:', String((e as Error)?.message || e));
            }
          }
        })
        .catch((err: unknown) => {
          misses += 1;
          const reason = String((err as Error)?.message || err);
          try {
            fs.appendFileSync(
              path.join(outDir(), 'misses.txt'),
              `${safeUrl(meta.url)}\t${reason}\n`,
            );
          } catch {}
          console.warn(
            `[capture-mvp] getResponseBody MISS (#${misses}) ${safeUrl(meta.url)} — ${reason}`,
          );
        });
      return;
    }
  });

  dbg
    .sendCommand('Network.enable', {
      maxTotalBufferSize: 256 * 1024 * 1024,
      maxResourceBufferSize: 32 * 1024 * 1024,
    })
    .then(() => {
      console.log(
        `[capture-mvp] attached on webview ${id} (write=${WRITE}) — debug dir ${outDir()}`,
      );
    })
    .catch((err: unknown) =>
      console.warn('[capture-mvp] Network.enable failed:', String((err as Error)?.message || err)),
    );

  wc.once('destroyed', () => {
    try {
      dbg.detach();
    } catch {}
    console.log(
      `[capture-mvp] webview ${id} closed — seen=${seen} saved=${saved} misses=${misses} | ` +
        `DB writes: complete=${stats.complete} cover=${stats.thumbOnly} ` +
        `skip(no-post)=${stats.skipNoPost} skip(already-local)=${stats.alreadyLocal}`,
    );
  });
}
