// ── Grid thumbnails ──────────────────────────────────────────────────────────
// The "thumbnails" on disk are often the full-resolution CDN originals (multi-MB
// JPEGs, up to 100 megapixel): decoding dozens of those for ~200px grid tiles is
// what makes the gallery slow, regardless of how the grid is virtualized. An
// `?w=N` query on an asset:// image URL serves a downscaled copy instead, cached
// on disk keyed by (path, mtime, size, width), so each image pays the resize a
// single time ever.
//
// Generation goes through nativeImage.createThumbnailFromPath — the OS
// thumbnailer (QuickLook / Windows shell). It is ASYNC, so the main process
// event loop never blocks on a decode (a synchronous decode here serializes
// every pending asset response and IPC call behind ~350ms per image — the grid
// visibly loads "in chunks"). It also decodes formats nativeImage can't (webp).
// gif/avif/svg stay original: resizing would drop animation / lacks a decoder.
//
// This module also owns the per-post blur-up placeholder (posts.thumb_blur): a
// ~24px JPEG data URI persisted in the DB and shipped inside the getPosts
// payload, so a card can paint a recognizable preview in the same frame it
// mounts — no IPC, no protocol round-trip, no black tile while the real
// thumbnail loads.

import { app, nativeImage, screen } from 'electron';
import path from 'path';
import fs from 'fs';
import crypto from 'crypto';
import * as db from './db';

// Resolved cache location + format metadata for one (source file, width) tile.
interface ThumbCacheEntry {
  png: boolean;
  key: string;
  mime: string;
  cachePath: string;
}

// A served downscaled tile: the decoded bytes plus the headers the protocol
// handler needs (content-type + ETag).
interface ThumbResult {
  data: Buffer;
  mime: string;
  etag: string;
}

const THUMB_EXTS = new Set<string>(['jpg', 'jpeg', 'png', 'webp']);
const thumbCacheDir = (): string => path.join(app.getPath('userData'), 'thumb-cache');

// Resolve symlinks on an absolute path, falling back to the input unchanged
// when it can't be resolved (e.g. doesn't exist yet). Mirrors main.js's
// safeRealpath: the asset protocol handler keys the on-disk tile cache by the
// FULLY symlink-resolved path (main.js:110), so the pre-warm and blur writers
// here must normalize identically — otherwise, when userData is reached through
// a symlink, path.resolve() and realpath() differ and the SHA-1 cache keys
// never match (prewarmed/probed tiles become unreachable).
async function safeRealpath(p: string): Promise<string> {
  try {
    return await fs.promises.realpath(p);
  } catch {
    return p;
  }
}
// SHELFY_THUMB_NO_CACHE (set only by the gallery perf harness' "cacheless" mode)
// makes thumbnailFor never read, write, or dedup the on-disk tile cache — every
// request regenerates+decodes the tile from scratch, the steady-state worst
// case. Inert in normal runs.
const NO_THUMB_CACHE = process.env.SHELFY_THUMB_NO_CACHE === '1';
// Dedupes a burst of concurrent requests for the same (file, width) so a
// thumbnail is generated once, not once per request.
const inflightThumbs = new Map<string, Promise<ThumbResult | null>>();

// Cap concurrent generations. The OS thumbnailer is async, but a cold first
// scroll can request hundreds at once — bound the outstanding system calls so
// the freshest requests (what the user is looking at) aren't starved. On Linux
// there is no OS thumbnailer and generation falls back to a synchronous decode
// on the main thread, so keep the cap tighter there.
let thumbSlots = process.platform === 'linux' ? 4 : 8;
const thumbWaiters: Array<() => void> = [];
const takeThumbSlot = (): Promise<void> =>
  new Promise<void>((resolve) => {
    if (thumbSlots > 0) {
      thumbSlots -= 1;
      resolve();
    } else {
      thumbWaiters.push(resolve);
    }
  });
const releaseThumbSlot = (): void => {
  // LIFO on purpose: during a cold-cache fling hundreds of generations queue
  // up, and the tiles the user is currently looking at are the ones requested
  // LAST. Serving the newest request first means the visible viewport always
  // wins; the scrolled-past tiles still complete eventually and warm the cache.
  const next = thumbWaiters.pop();
  if (next) next();
  else thumbSlots += 1;
};

// Cache file path + mime for a given source file/size. PNG keeps its alpha
// (web screenshots / logos on the dark theme); the other formats flatten to
// JPEG. Shared by the protocol handler and the startup pre-warm. `key` doubles
// as the HTTP ETag for the tile response: it changes whenever the source file
// (mtime/size) or the requested width changes.
function thumbCacheEntry(
  resolved: string,
  stat: fs.Stats,
  width: number,
  ext: string,
): ThumbCacheEntry {
  const png = ext === 'png';
  const key = crypto
    .createHash('sha1')
    .update(`${resolved}|${stat.mtimeMs}|${stat.size}|${width}`)
    .digest('hex');
  return {
    png,
    key,
    mime: png ? 'image/png' : 'image/jpeg',
    cachePath: path.join(thumbCacheDir(), `${key}.${png ? 'png' : 'jpg'}`),
  };
}

// Produce the downscaled bytes, or null when the original should be served
// as-is (undecodable / already small enough).
async function generateThumb(
  resolved: string,
  width: number,
  png: boolean,
): Promise<Buffer | null> {
  // OS thumbnailer first: async, broad format support, QuickLook-cached.
  // The output preserves aspect ratio (fits within the box, no padding), but
  // getSize() reports the requested box in DIPs — trust the encoded bytes, not
  // getSize(). On macOS the size is interpreted as DIPs and the bitmap comes
  // out at scaleFactor× that (verified: 320 DIP → 640px on a 2x display), so
  // divide to make `width` mean device pixels everywhere.
  try {
    const scale = process.platform === 'darwin' ? screen.getPrimaryDisplay()?.scaleFactor || 1 : 1;
    const dip = Math.max(1, Math.round(width / scale));
    const fitted = await nativeImage.createThumbnailFromPath(resolved, {
      width: dip,
      height: dip,
    });
    if (!fitted.isEmpty()) return png ? fitted.toPNG() : fitted.toJPEG(80);
  } catch {
    /* unsupported platform/format — fall through to the sync decode */
  }
  // Fallback (Linux, exotic formats): synchronous decode. Blocks the main
  // process for the decode, but it's rare and bounded by the slot cap.
  const img = nativeImage.createFromPath(resolved);
  if (img.isEmpty()) return null;
  const { width: ow, height: oh } = img.getSize();
  if (ow <= width && oh <= width) return null;
  const scale = Math.min(width / ow, width / oh);
  const resized = img.resize({ width: Math.max(1, Math.round(ow * scale)), quality: 'good' });
  return png ? resized.toPNG() : resized.toJPEG(80);
}

// Resolve to { data, mime, etag } for a downscaled copy, or null when the
// original should be served as-is.
async function thumbnailFor(
  resolved: string,
  width: number,
  ext: string,
): Promise<ThumbResult | null> {
  let stat: fs.Stats;
  try {
    stat = await fs.promises.stat(resolved);
  } catch {
    return null;
  }
  const { png, mime, key, cachePath } = thumbCacheEntry(resolved, stat, width, ext);
  // Cacheless: no read / no write / no inflight-dedup — regenerate every time.
  if (NO_THUMB_CACHE) {
    await takeThumbSlot();
    try {
      const data = await generateThumb(resolved, width, png);
      return data ? { data, mime, etag: key } : null;
    } finally {
      releaseThumbSlot();
    }
  }
  try {
    return { data: await fs.promises.readFile(cachePath), mime, etag: key };
  } catch {
    /* not cached yet */
  }
  const existing = inflightThumbs.get(cachePath);
  if (existing) return existing;
  const job = (async (): Promise<ThumbResult | null> => {
    await takeThumbSlot();
    try {
      const data = await generateThumb(resolved, width, png);
      if (!data) return null;
      // The tile is already decoded in memory: persisting it is a best-effort
      // optimization, never a precondition for serving it. A cache-write failure
      // (disk full, EACCES, EXDEV cross-device rename, AV lock) must NOT void an
      // otherwise valid downscaled buffer — otherwise the protocol handler falls
      // back to streaming the multi-MB original, the exact cost this avoids.
      try {
        await fs.promises.mkdir(thumbCacheDir(), { recursive: true });
        // tmp + rename: a concurrent reader must never see a half-written file.
        const tmp = `${cachePath}.${process.pid}.tmp`;
        try {
          await fs.promises.writeFile(tmp, data);
          await fs.promises.rename(tmp, cachePath);
        } catch (err) {
          // Don't leak the orphaned .tmp if the rename (or write) failed.
          await fs.promises.unlink(tmp).catch(() => {});
          throw err;
        }
      } catch {
        /* cache persist failed — serve the in-memory tile anyway */
      }
      return { data, mime, etag: key };
    } finally {
      releaseThumbSlot();
    }
  })()
    .catch(() => null) // any decode/cache failure → fall back to the original
    .finally(() => inflightThumbs.delete(cachePath));
  inflightThumbs.set(cachePath, job);
  return job;
}

// Cheap ETag probe for a tile: stat + hash, no read, no generation. Lets the
// protocol handler answer If-None-Match with a 304 before touching the cache.
async function thumbETag(resolved: string, width: number, ext: string): Promise<string | null> {
  try {
    const stat = await fs.promises.stat(resolved);
    return thumbCacheEntry(resolved, stat, width, ext).key;
  } catch {
    return null;
  }
}

// One-time background warm of the tile cache: walk the downloaded asset dirs
// and pre-generate every missing grid thumbnail, so even the FIRST scroll
// through the library lands on cached files. Sequential on purpose — it
// occupies at most one generation slot, leaving the rest for what the user is
// actually looking at; on later launches it reduces to a fast stat/access scan.
// `web` holds the website screenshots (webp): they are the heaviest sources
// (full-page captures) and used to be the slowest on-demand tiles.
const PREWARM_TILE_WIDTH = 640; // keep in sync with PostCard's TILE_WIDTH
async function prewarmThumbCache(): Promise<void> {
  let generated = 0;
  for (const dir of ['thumbnails', 'images', 'web']) {
    const root = path.join(app.getPath('userData'), 'assets', dir);
    let names: string[];
    try {
      names = await fs.promises.readdir(root);
    } catch {
      continue; // dir not created yet (fresh install)
    }
    for (const name of names) {
      const ext = path.extname(name).slice(1).toLowerCase();
      if (!THUMB_EXTS.has(ext)) continue;
      // Resolve symlinks so the cache key matches the protocol handler's.
      const resolved = await safeRealpath(path.join(root, name));
      try {
        const stat = await fs.promises.stat(resolved);
        const { cachePath } = thumbCacheEntry(resolved, stat, PREWARM_TILE_WIDTH, ext);
        await fs.promises.access(cachePath);
      } catch {
        const thumb = await thumbnailFor(resolved, PREWARM_TILE_WIDTH, ext);
        if (thumb) generated += 1;
      }
    }
  }
  if (generated > 0) console.info(`[thumb-cache] pre-warmed ${generated} tiles`);
}

// ── Blur-up placeholders ─────────────────────────────────────────────────────
// A ~24px JPEG weighs ~300-600 bytes as a data URI: small enough to persist in
// the posts row and ship with every getPosts page, big enough to read as a
// blurred preview of the artwork once the card upscales it.
const BLUR_WIDTH = 24;
const BLUR_JPEG_QUALITY = 55;

// Build the micro placeholder for a local image file. Prefers decoding the
// cached 640px grid tile (a ~50KB JPEG — milliseconds) over re-decoding the
// multi-MB original; falls back to the OS thumbnailer when the tile isn't
// cached yet. Returns a data URI string, or null when the file can't be decoded.
async function microThumbDataUri(sourcePath: string): Promise<string | null> {
  try {
    // realpath (not just resolve): the cached grid tile is keyed by the
    // protocol handler on the symlink-resolved path, so the fast tile-decode
    // lookup below must use the same normalization or it always misses.
    const resolved = await safeRealpath(path.resolve(sourcePath));
    const ext = path.extname(resolved).slice(1).toLowerCase();
    let small: Buffer | null = null;
    if (THUMB_EXTS.has(ext)) {
      try {
        const stat = await fs.promises.stat(resolved);
        const { cachePath } = thumbCacheEntry(resolved, stat, PREWARM_TILE_WIDTH, ext);
        const tile = nativeImage.createFromPath(cachePath);
        if (!tile.isEmpty()) {
          small = tile.resize({ width: BLUR_WIDTH, quality: 'good' }).toJPEG(BLUR_JPEG_QUALITY);
        }
      } catch {
        /* tile not cached yet — generate from the original below */
      }
    }
    if (!small) small = await generateThumb(resolved, BLUR_WIDTH, false);
    if (!small) {
      // generateThumb returns null for originals already ≤ BLUR_WIDTH: re-encode
      // the original itself (it's tiny by definition).
      const orig = nativeImage.createFromPath(resolved);
      if (!orig.isEmpty()) small = orig.toJPEG(BLUR_JPEG_QUALITY);
    }
    if (!small || !small.length) return null;
    return `data:image/jpeg;base64,${small.toString('base64')}`;
  } catch {
    return null;
  }
}

// Startup backfill: populate thumb_blur for every post that has a local cover
// but no placeholder yet. Runs after prewarmThumbCache so nearly every source
// resolves through the fast tile-decode path. Posts whose cover can't be
// decoded are stamped with '' (sentinel: tried, ineligible) so they aren't
// rescanned on every boot; rowToPost maps '' back to null for the renderer.
async function backfillThumbBlurs(): Promise<number> {
  let rows: Array<{ id: string; src: string }>;
  try {
    rows = db.listPostsMissingThumbBlur();
  } catch (err) {
    console.warn(`[thumb-blur] backfill scan failed: ${(err as Error)?.message || err}`);
    return 0;
  }
  let written = 0;
  for (const { id, src } of rows) {
    const uri = await microThumbDataUri(src);
    try {
      db.setThumbBlur(id, uri || '');
    } catch {
      /* post deleted mid-sweep — skip */
    }
    if (uri) written += 1;
  }
  if (written > 0) console.info(`[thumb-blur] backfilled ${written} placeholders`);
  return written;
}

export {
  THUMB_EXTS,
  PREWARM_TILE_WIDTH,
  thumbnailFor,
  thumbETag,
  prewarmThumbCache,
  microThumbDataUri,
  backfillThumbBlurs,
};
