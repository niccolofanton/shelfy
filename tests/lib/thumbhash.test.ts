import { describe, it, expect } from 'vitest';
import { rgbaToThumbHash } from 'thumbhash';
import { thumbHashToDataURL } from '../../src/lib/thumbhash';

// `thumbhash` ships no types; src/lib/thumbhash.ts declares the one export it
// uses (thumbHashToRGBA). This merges in the encoder, used only to build a
// real fixture below instead of hand-rolling ThumbHash bytes.
declare module 'thumbhash' {
  export function rgbaToThumbHash(w: number, h: number, rgba: Uint8Array): Uint8Array;
}

// A tiny 4x4 solid-ish image, encoded with the package's own encoder so the
// fixture is a real ThumbHash (not a hand-rolled byte string), then passed
// through our decoder — the same round trip the server/client do across the
// wire (crates/media encodes, this module decodes).
function fourByFourHash(): string {
  const w = 4;
  const h = 4;
  const rgba = new Uint8Array(w * h * 4);
  for (let i = 0; i < w * h; i++) {
    rgba[i * 4] = 200; // R
    rgba[i * 4 + 1] = 100; // G
    rgba[i * 4 + 2] = 50; // B
    rgba[i * 4 + 3] = 255; // A
  }
  const hash = rgbaToThumbHash(w, h, rgba);
  let bin = '';
  for (let i = 0; i < hash.length; i++) bin += String.fromCharCode(hash[i]);
  return btoa(bin);
}

describe('thumbHashToDataURL', () => {
  it('decodes a real ThumbHash into a BMP data URI', () => {
    const url = thumbHashToDataURL(fourByFourHash());
    expect(url).toMatch(/^data:image\/bmp;base64,/);
  });

  it('is deterministic for the same hash', () => {
    const hash = fourByFourHash();
    expect(thumbHashToDataURL(hash)).toBe(thumbHashToDataURL(hash));
  });

  it('returns null for null, undefined and empty input', () => {
    expect(thumbHashToDataURL(null)).toBeNull();
    expect(thumbHashToDataURL(undefined)).toBeNull();
    expect(thumbHashToDataURL('')).toBeNull();
  });

  it('returns null instead of throwing on malformed base64 or a too-short hash', () => {
    expect(thumbHashToDataURL('not-valid-base64!!!')).toBeNull();
    expect(thumbHashToDataURL(btoa('ab'))).toBeNull(); // 2 bytes, below the 5-byte header
  });

  it('produces a well-formed BMP: header size, pixel count and bit depth match the decode', () => {
    const url = thumbHashToDataURL(fourByFourHash());
    const base64 = url!.slice(url!.indexOf(',') + 1);
    const bytes = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
    const view = new DataView(bytes.buffer);
    expect(String.fromCharCode(bytes[0], bytes[1])).toBe('BM');
    expect(view.getUint32(2, true)).toBe(bytes.length); // file size field matches
    expect(view.getUint32(10, true)).toBe(54); // pixel data offset (14 + 40)
    expect(view.getUint16(28, true)).toBe(32); // 32bpp
    const w = view.getInt32(18, true);
    const h = view.getInt32(22, true);
    expect(w).toBeGreaterThan(0);
    expect(h).toBeGreaterThan(0);
    expect(bytes.length).toBe(54 + w * h * 4);
  });
});
