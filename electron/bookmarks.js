'use strict';

// Manual bookmarks: the user adds local files (images/videos/pdf/any) with a
// note + tags, surfaced in the gallery as platform='manual' posts so they flow
// through the same card/modal/search/AI rendering as social and web items.
//
// The renderer (src/lib/bookmarkFiles.js) does all decoding and hands us, per
// file, the original bytes plus an always-present webp preview (downscaled image
// / video frame / pdf page 1 / drawn icon). Here we only persist bytes to the
// userData asset dirs and build the posts + post_media rows. Mirrors downloader's
// asset layout so the asset:// protocol serves these files unchanged.

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { app } = require('electron');
const db = require('./db');

const VALID_KINDS = ['image', 'video', 'pdf', 'file'];
const DEFAULT_EXT = { image: 'jpg', video: 'mp4', pdf: 'pdf', file: 'bin' };

function assetDir(sub) {
  const dir = path.join(app.getPath('userData'), 'assets', sub);
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

// Sanitised, length-capped extension from a filename; falls back per kind.
function safeExt(name, kind) {
  const m = /\.([a-z0-9]+)$/i.exec(String(name || '').trim());
  const raw = m ? m[1].toLowerCase() : '';
  return raw && raw.length <= 8 ? raw : DEFAULT_EXT[kind] || 'bin';
}

// Coerce the IPC-transferred value (Uint8Array / ArrayBuffer / number[] / Buffer)
// into a Node Buffer, or null if there's nothing usable.
function toBuffer(v) {
  if (!v) return null;
  if (Buffer.isBuffer(v)) return v.length ? v : null;
  if (v instanceof Uint8Array) return v.length ? Buffer.from(v) : null;
  if (v instanceof ArrayBuffer) return v.byteLength ? Buffer.from(v) : null;
  if (Array.isArray(v)) return v.length ? Buffer.from(v) : null;
  if (v.buffer instanceof ArrayBuffer) return Buffer.from(v.buffer);
  return null;
}

// Persist one manual bookmark. `files` is the renderer payload:
//   [{ name, mime, kind, original: Uint8Array, preview: Uint8Array|null }]
// Returns { id }. Throws on no usable files.
async function addManualBookmark({ note = '', tags = [], files = [] } = {}) {
  if (!Array.isArray(files) || files.length === 0) {
    throw new Error('Nessun file selezionato.');
  }
  const id = 'manual:' + crypto.randomUUID();
  const slug = id.replace(/[^a-z0-9]/gi, '').slice(0, 24);

  const media = [];
  let primaryKind = null;
  let thumbnailPath = null; // card/modal thumbnail (always a preview image when present)
  let imagePath = null; // a real image for the AI analyzer (image/pdf only)
  let videoPath = null; // enables card hover-play

  // Files are written to disk before the DB row exists; if a later write (or the
  // DB insert) fails, the bytes already on disk have no post row pointing at them
  // and would never be reclaimed by the normal delete/GC paths. Track every path
  // we write so we can unlink them on failure before rethrowing.
  const written = [];
  let mediaType = 'image';

  try {
    for (let i = 0; i < files.length; i++) {
      const f = files[i];
      const kind = VALID_KINDS.includes(f && f.kind) ? f.kind : 'file';
      const origBuf = toBuffer(f && f.original);
      if (!origBuf) continue; // skip empty/garbage entries

      const sub = kind === 'video' ? 'videos' : kind === 'image' ? 'images' : 'files';
      const origPath = path.join(assetDir(sub), `${slug}-${i}.${safeExt(f.name, kind)}`);
      await fs.promises.writeFile(origPath, origBuf);
      written.push(origPath);

      let previewPath = null;
      const prevBuf = toBuffer(f && f.preview);
      if (prevBuf) {
        previewPath = path.join(assetDir('thumbnails'), `${slug}-${i}.webp`);
        await fs.promises.writeFile(previewPath, prevBuf);
        written.push(previewPath);
      }

      // Slide rendering: videos play from the original file; images render the
      // original (full quality for zoom); pdf/file render their preview webp.
      // source_url carries the original path so the modal can open the real file.
      media.push({
        kind,
        type: kind === 'video' ? 'video' : kind === 'file' ? 'file' : 'image',
        localPath: kind === 'image' || kind === 'video' ? origPath : previewPath || origPath,
        sourcePath: origPath,
      });

      if (!primaryKind) {
        primaryKind = kind;
        thumbnailPath = previewPath || (kind === 'image' ? origPath : null);
        imagePath = kind === 'image' ? origPath : kind === 'pdf' ? previewPath : null;
      }
      if (kind === 'video' && !videoPath) videoPath = origPath;
    }

    if (media.length === 0) throw new Error('Nessun file valido.');

    // Post-level media_type: video wins, multiple items are a carousel (matching
    // the social pipeline), a single pdf/generic file is 'file' so the card shows
    // the document icon (PostCard's MediaTypeIcon) instead of the image one.
    mediaType =
      primaryKind === 'video'
        ? 'video'
        : media.length > 1
          ? 'carousel'
          : primaryKind === 'pdf' || primaryKind === 'file'
            ? 'file'
            : 'image';
  } catch (err) {
    // Cleanup covers ONLY the on-disk blobs written above: at this point no DB row
    // references them yet, so unlinking is safe.
    await Promise.all(written.map((p) => fs.promises.unlink(p).catch(() => {})));
    throw err;
  }

  // The DB insert is OUTSIDE the cleanup scope on purpose: addManualBookmark
  // commits the posts/post_media rows and THEN runs a second transaction (user
  // content). If that second step throws, the row may already be committed —
  // blanket-unlinking the files would orphan a live DB row pointing at deleted
  // blobs. The written files match whatever the insert committed, so leave them.
  return db.addManualBookmark({
    id,
    note: typeof note === 'string' ? note : '',
    tags: Array.isArray(tags) ? tags : [],
    mediaType,
    thumbnailPath,
    imagePath,
    videoPath,
    media,
  });
}

module.exports = { addManualBookmark };
