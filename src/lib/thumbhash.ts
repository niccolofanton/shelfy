// Client-decoded ThumbHash blur-up placeholder (T8 carry-over; plan §2.19,
// §2.13). The web API serves `post.thumbhash` — the standard-base64 ThumbHash
// of the cover, ≤25 bytes (crates/media/src/render.rs) — instead of the
// desktop's own JPEG-based `thumb_blur` data URI. This module decodes it into
// something PostCard can show exactly the same way: a small image `src` for
// the existing blur-up `<img>` (see PostCard's `post.thumbBlur` handling), so
// no rendering code has to tell the two placeholder kinds apart.
//
// Deliberately canvas-free: a browser's <img> decodes BMP natively, and a
// 32bpp BMP is cheap to build by hand from the decoded RGBA buffer (fixed
// 54-byte header, 4-byte-aligned rows — no padding math, no color table).
// That keeps this module pure (same code path in the browser and under
// vitest/jsdom, with no canvas polyfill), and skips a decode→paint→readback
// round trip for an image this tiny (ThumbHash renders at ≤32px a side).
//
// `thumbhash` (npm) ships no types and no `.d.ts` (plain ESM with JSDoc), so
// the shape used here is declared below.
declare module 'thumbhash' {
  export function thumbHashToRGBA(hash: Uint8Array): { w: number; h: number; rgba: Uint8Array };
}

import { thumbHashToRGBA } from 'thumbhash';

// `atob`/`btoa` exist in every browser and in vitest's jsdom (this module's
// only two runtimes): no Node `Buffer` fallback needed, which keeps this file
// typecheckable under the browser-only renderer/web tsconfigs (no `@types/node`).
function base64ToBytes(base64: string): Uint8Array {
  const bin = atob(base64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function bytesToBase64(bytes: Uint8Array): string {
  let bin = '';
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}

const FILE_HEADER = 14;
const DIB_HEADER = 40; // BITMAPINFOHEADER
const PIXELS_OFFSET = FILE_HEADER + DIB_HEADER;

// Packs a decoded RGBA buffer into an uncompressed 32bpp BMP (bottom-up, the
// traditional row order every decoder supports) and returns it as a data URI.
function rgbaToBmpDataUrl(w: number, h: number, rgba: Uint8Array): string {
  const rowSize = w * 4; // 32bpp rows are always a multiple of 4 bytes: no padding
  const pixelDataSize = rowSize * h;
  const fileSize = PIXELS_OFFSET + pixelDataSize;
  const buf = new Uint8Array(fileSize);
  const view = new DataView(buf.buffer);

  // BITMAPFILEHEADER
  buf[0] = 0x42; // 'B'
  buf[1] = 0x4d; // 'M'
  view.setUint32(2, fileSize, true);
  view.setUint32(6, 0, true); // reserved
  view.setUint32(10, PIXELS_OFFSET, true);

  // BITMAPINFOHEADER
  view.setUint32(14, DIB_HEADER, true);
  view.setInt32(18, w, true);
  view.setInt32(22, h, true); // positive height = bottom-up
  view.setUint16(26, 1, true); // planes
  view.setUint16(28, 32, true); // bit count
  view.setUint32(30, 0, true); // BI_RGB (uncompressed)
  view.setUint32(34, pixelDataSize, true);
  view.setInt32(38, 2835, true); // ~72 DPI, not read by any consumer here
  view.setInt32(42, 2835, true);
  view.setUint32(46, 0, true);
  view.setUint32(50, 0, true);

  // Pixel data, bottom-up, BGRA (BMP's native channel order).
  let offset = PIXELS_OFFSET;
  for (let y = h - 1; y >= 0; y--) {
    let src = y * rowSize;
    for (let x = 0; x < w; x++) {
      buf[offset] = rgba[src + 2]; // B
      buf[offset + 1] = rgba[src + 1]; // G
      buf[offset + 2] = rgba[src]; // R
      buf[offset + 3] = rgba[src + 3]; // A
      offset += 4;
      src += 4;
    }
  }

  return `data:image/bmp;base64,${bytesToBase64(buf)}`;
}

/**
 * Decodes a standard-base64 ThumbHash (`post.thumbhash`) into a tiny image
 * `src` usable wherever the desktop's `thumbBlur` data URI is used today.
 * Returns `null` on anything malformed instead of throwing: a blur-up
 * placeholder is a cosmetic nicety, never worth crashing a card over.
 */
export function thumbHashToDataURL(base64Hash: string | null | undefined): string | null {
  if (!base64Hash) return null;
  try {
    const hash = base64ToBytes(base64Hash);
    // The format's shortest valid header is 5 bytes (no alpha channel); see
    // render.rs / the reference decoder.
    if (hash.length < 5) return null;
    const { w, h, rgba } = thumbHashToRGBA(hash);
    if (!(w > 0) || !(h > 0) || rgba.length < w * h * 4) return null;
    return rgbaToBmpDataUrl(w, h, rgba);
  } catch {
    return null;
  }
}
