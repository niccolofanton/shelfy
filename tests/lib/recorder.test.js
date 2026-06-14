import { describe, it, expect } from 'vitest';
import { encodeWav, SAMPLE_RATE } from '../../src/lib/dictation/recorder';

const readStr = (view, off, len) =>
  Array.from({ length: len }, (_, i) => String.fromCharCode(view.getUint8(off + i))).join('');

describe('encodeWav', () => {
  it('writes a valid 16-bit mono PCM WAV header', () => {
    const samples = new Float32Array([0, 0.5, -0.5, 1, -1]);
    const buf = encodeWav(samples, SAMPLE_RATE);
    const view = new DataView(buf);

    // Total size = 44-byte header + 2 bytes per sample.
    expect(buf.byteLength).toBe(44 + samples.length * 2);

    expect(readStr(view, 0, 4)).toBe('RIFF');
    expect(readStr(view, 8, 4)).toBe('WAVE');
    expect(readStr(view, 12, 4)).toBe('fmt ');
    expect(readStr(view, 36, 4)).toBe('data');

    expect(view.getUint32(16, true)).toBe(16);          // fmt chunk size
    expect(view.getUint16(20, true)).toBe(1);           // PCM
    expect(view.getUint16(22, true)).toBe(1);           // mono
    expect(view.getUint32(24, true)).toBe(SAMPLE_RATE); // sample rate
    expect(view.getUint32(28, true)).toBe(SAMPLE_RATE * 2); // byte rate
    expect(view.getUint16(32, true)).toBe(2);           // block align
    expect(view.getUint16(34, true)).toBe(16);          // bits/sample
    expect(view.getUint32(40, true)).toBe(samples.length * 2); // data size
  });

  it('converts float samples to clamped int16', () => {
    const samples = new Float32Array([0, 1, -1, 2, -2]); // 2/-2 must clamp to 1/-1
    const view = new DataView(encodeWav(samples));
    expect(view.getInt16(44, true)).toBe(0);
    expect(view.getInt16(46, true)).toBe(0x7fff);
    expect(view.getInt16(48, true)).toBe(-0x8000);
    expect(view.getInt16(50, true)).toBe(0x7fff);  // clamped from 2
    expect(view.getInt16(52, true)).toBe(-0x8000); // clamped from -2
  });

  it('produces a header-only buffer for empty input', () => {
    expect(encodeWav(new Float32Array(0)).byteLength).toBe(44);
  });
});
