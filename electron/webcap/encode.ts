// ffmpeg-backed image/video encoding and pixel sampling for the web-reference
// capture (v2). Inputs are PNG buffers or files produced by the capture driver;
// outputs land under <userData>/assets/web with the same naming scheme as the v1
// engine (stamp-host-hash.ext), so version history and asset:// serving work
// unchanged. Every spawn is abortable and never touches the network
// (-protocol_whitelist file,pipe).

import fs from 'fs';
import os from 'os';
import path from 'path';
import { spawn } from 'child_process';
import { resolveFfmpeg, screenshotPathForUrl } from './sitefetch';

// ffmpeg thread cap. The desktop leaves it unset (ffmpeg's own default); the
// capture service sets 1 (plan §2.18 budget: `ffmpeg -threads 1`) so an encode
// never competes with Chromium for the 1.5 shared vCPUs (SPIKE-11).
let ffmpegThreads: number | null = null;
export function setFfmpegThreads(n: number | null): void {
  ffmpegThreads = n && n > 0 ? Math.floor(n) : null;
}

export interface ImageAsset {
  path: string;
  width: number;
  height: number;
}

function abortError(): Error {
  return Object.assign(new Error('AbortError'), { name: 'AbortError' });
}

// Run ffmpeg with an optional stdin buffer; resolves with stdout (Buffer) or
// rejects on a non-zero exit. Abort kills the child.
export function runFfmpeg(
  args: string[],
  {
    input,
    signal,
    timeoutMs = 120_000,
  }: { input?: Buffer; signal?: AbortSignal; timeoutMs?: number } = {},
): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(abortError());
    const threadArgs = ffmpegThreads != null ? ['-threads', String(ffmpegThreads)] : [];
    const child = spawn(
      resolveFfmpeg(),
      ['-hide_banner', '-loglevel', 'error', ...threadArgs, ...args],
      {
        stdio: [input ? 'pipe' : 'ignore', 'pipe', 'pipe'],
      },
    );
    const out: Buffer[] = [];
    let err = '';
    child.stdout!.on('data', (d: Buffer) => out.push(d));
    child.stderr!.setEncoding('utf8');
    child.stderr!.on('data', (d: string) => {
      if (err.length < 4000) err += d;
    });
    const timer = setTimeout(() => {
      try {
        child.kill('SIGKILL');
      } catch {}
    }, timeoutMs);
    const onAbort = (): void => {
      try {
        child.kill('SIGKILL');
      } catch {}
    };
    signal?.addEventListener('abort', onAbort, { once: true });
    child.on('error', (e) => {
      clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
      reject(e);
    });
    child.on('close', (code) => {
      clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
      if (signal?.aborted) return reject(abortError());
      if (code === 0) resolve(Buffer.concat(out));
      else reject(new Error(`ffmpeg exited ${code}: ${err.trim().slice(0, 300)}`));
    });
    if (input && child.stdin) {
      child.stdin.on('error', () => {});
      child.stdin.end(input);
    }
  });
}

export function assetPath(key: string, ext: string, stamp?: number): string {
  const p = screenshotPathForUrl(key, ext, stamp);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  return p;
}

// PNG (buffer or file) → WebP. `maxWidth` downscales (never upscales); `crop`
// is in SOURCE pixels. Lossy WebP at a high quality: at 2× device scale the
// chroma subsampling of thin coloured text is no longer visible.
export async function encodeWebp(
  src: Buffer | string,
  outPath: string,
  {
    quality = 88,
    maxWidth,
    crop,
    signal,
  }: {
    quality?: number;
    maxWidth?: number;
    crop?: { x: number; y: number; w: number; h: number };
    signal?: AbortSignal;
  } = {},
): Promise<ImageAsset> {
  const filters: string[] = [];
  if (crop)
    filters.push(
      `crop=${Math.round(crop.w)}:${Math.round(crop.h)}:${Math.round(crop.x)}:${Math.round(crop.y)}`,
    );
  if (maxWidth) filters.push(`scale='if(gt(iw,${maxWidth}),${maxWidth},iw)':-1:flags=lanczos`);
  const input = typeof src === 'string' ? ['-i', src] : ['-f', 'png_pipe', '-i', '-'];
  await runFfmpeg(
    [
      '-protocol_whitelist',
      'file,pipe,fd',
      ...input,
      ...(filters.length ? ['-vf', filters.join(',')] : []),
      '-frames:v',
      '1',
      '-c:v',
      'libwebp',
      '-quality',
      String(quality),
      '-compression_level',
      '5',
      '-y',
      outPath,
    ],
    { input: typeof src === 'string' ? undefined : src, signal },
  );
  if (!fs.existsSync(outPath)) throw new Error('WebP encode produced no output');
  const size = await probeSize(outPath, signal);
  return { path: outPath, ...size };
}

// Pixel size of an image/video file. PNG and WebP are read from their headers
// (no spawn); anything else falls back to ffmpeg's stream info.
export async function probeSize(
  file: string,
  signal?: AbortSignal,
): Promise<{ width: number; height: number }> {
  try {
    const fd = await fs.promises.open(file, 'r');
    try {
      const buf = Buffer.alloc(64);
      await fd.read(buf, 0, 64, 0);
      const png = pngSize(buf);
      if (png.width) return png;
      const webp = webpSize(buf);
      if (webp.width) return webp;
    } finally {
      await fd.close();
    }
  } catch {
    /* fall back to ffmpeg */
  }
  return new Promise((resolve) => {
    if (signal?.aborted) return resolve({ width: 0, height: 0 });
    const child = spawn(resolveFfmpeg(), ['-hide_banner', '-i', file], {
      stdio: ['ignore', 'ignore', 'pipe'],
    });
    let err = '';
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (d: string) => {
      if (err.length < 20000) err += d;
    });
    child.on('error', () => resolve({ width: 0, height: 0 }));
    child.on('close', () => {
      const m = /Video:[^\n]*?,\s*(\d{2,6})x(\d{2,6})[, ]/.exec(err);
      resolve(m ? { width: Number(m[1]), height: Number(m[2]) } : { width: 0, height: 0 });
    });
  });
}

// WebP canvas size from the RIFF header (VP8X / VP8L / VP8).
export function webpSize(buf: Buffer): { width: number; height: number } {
  if (
    buf.length < 30 ||
    buf.toString('ascii', 0, 4) !== 'RIFF' ||
    buf.toString('ascii', 8, 12) !== 'WEBP'
  ) {
    return { width: 0, height: 0 };
  }
  const chunk = buf.toString('ascii', 12, 16);
  if (chunk === 'VP8X') {
    return { width: 1 + buf.readUIntLE(24, 3), height: 1 + buf.readUIntLE(27, 3) };
  }
  if (chunk === 'VP8L') {
    const b = buf.readUInt32LE(21);
    return { width: (b & 0x3fff) + 1, height: ((b >> 14) & 0x3fff) + 1 };
  }
  if (chunk === 'VP8 ') {
    return { width: buf.readUInt16LE(26) & 0x3fff, height: buf.readUInt16LE(28) & 0x3fff };
  }
  return { width: 0, height: 0 };
}

// PNG dimensions straight from the IHDR chunk (no spawn).
export function pngSize(buf: Buffer): { width: number; height: number } {
  if (buf.length > 24 && buf.readUInt32BE(12) === 0x49484452) {
    return { width: buf.readUInt32BE(16), height: buf.readUInt32BE(20) };
  }
  return { width: 0, height: 0 };
}

// 16×16 grayscale signature, for cheap frame-difference checks.
export async function signature(
  src: Buffer | string,
  signal?: AbortSignal,
): Promise<Buffer | null> {
  try {
    const input = typeof src === 'string' ? ['-i', src] : ['-f', 'png_pipe', '-i', '-'];
    const out = await runFfmpeg(
      [
        '-protocol_whitelist',
        'file,pipe,fd',
        ...input,
        '-vf',
        'scale=16:16,format=gray',
        '-f',
        'rawvideo',
        '-',
      ],
      { input: typeof src === 'string' ? undefined : src, signal, timeoutMs: 20_000 },
    );
    return out.length === 256 ? out : null;
  } catch {
    return null;
  }
}

export function meanDiff(a: Buffer | null, b: Buffer | null): number {
  if (!a || !b || a.length !== b.length || !a.length) return 255;
  let s = 0;
  for (let i = 0; i < a.length; i++) s += Math.abs(a[i] - b[i]);
  return s / a.length;
}

// Luminance statistics of a downscaled image: mean, standard deviation and the
// share of strong edges. Used by pixel-based QC (blank / black / loader frames).
export async function imageStats(
  src: Buffer | string,
  signal?: AbortSignal,
): Promise<{ mean: number; std: number; edges: number } | null> {
  try {
    const input = typeof src === 'string' ? ['-i', src] : ['-f', 'png_pipe', '-i', '-'];
    const W = 96;
    const H = 60;
    const raw = await runFfmpeg(
      [
        '-protocol_whitelist',
        'file,pipe,fd',
        ...input,
        '-vf',
        `scale=${W}:${H},format=gray`,
        '-f',
        'rawvideo',
        '-',
      ],
      { input: typeof src === 'string' ? undefined : src, signal, timeoutMs: 20_000 },
    );
    if (raw.length !== W * H) return null;
    let sum = 0;
    for (let i = 0; i < raw.length; i++) sum += raw[i];
    const mean = sum / raw.length;
    let v = 0;
    let edges = 0;
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        const p = raw[y * W + x];
        v += (p - mean) * (p - mean);
        if (x + 1 < W && Math.abs(p - raw[y * W + x + 1]) > 24) edges++;
        if (y + 1 < H && Math.abs(p - raw[(y + 1) * W + x]) > 24) edges++;
      }
    }
    return { mean, std: Math.sqrt(v / raw.length), edges: edges / (W * H * 2) };
  } catch {
    return null;
  }
}

// Down-sampled RGB pixels of an image (row-major rgb24), for palette clustering.
export async function samplePixels(
  file: string,
  width: number,
  signal?: AbortSignal,
): Promise<{ data: Buffer; width: number; height: number } | null> {
  try {
    const { width: w0, height: h0 } = await probeSize(file, signal);
    if (!w0 || !h0) return null;
    const height = Math.max(1, Math.round((h0 / w0) * width));
    const data = await runFfmpeg(
      [
        '-protocol_whitelist',
        'file',
        '-i',
        file,
        '-vf',
        `scale=${width}:${height}:flags=area,format=rgb24`,
        '-f',
        'rawvideo',
        '-',
      ],
      { signal, timeoutMs: 30_000 },
    );
    if (data.length !== width * height * 3) return null;
    return { data, width, height };
  } catch {
    return null;
  }
}

// Stack two or more PNG files vertically and crop a region of the result.
// Used for crops (sections, footer) that span a band boundary.
export async function cropAcross(
  bands: { file: string; top: number; height: number }[],
  region: { y: number; h: number },
  pxScale: number,
  outPath: string,
  {
    quality = 88,
    maxWidth,
    signal,
  }: { quality?: number; maxWidth?: number; signal?: AbortSignal } = {},
): Promise<ImageAsset | null> {
  const y0 = region.y;
  const y1 = region.y + region.h;
  const used = bands.filter((b) => b.top < y1 && b.top + b.height > y0);
  if (!used.length) return null;
  if (used.length === 1) {
    const b = used[0];
    const local = Math.max(0, y0 - b.top);
    const h = Math.min(b.height - local, y1 - Math.max(y0, b.top));
    const { width } = await probeSize(b.file, signal);
    return encodeWebp(b.file, outPath, {
      quality,
      maxWidth,
      signal,
      crop: { x: 0, y: local * pxScale, w: width, h: h * pxScale },
    });
  }
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-crop-'));
  try {
    const stacked = path.join(tmp, 'stack.png');
    const inputs: string[] = [];
    for (const b of used) inputs.push('-i', b.file);
    await runFfmpeg(
      [
        '-protocol_whitelist',
        'file',
        ...inputs,
        '-filter_complex',
        `vstack=inputs=${used.length}`,
        '-frames:v',
        '1',
        '-y',
        stacked,
      ],
      { signal },
    );
    const top = used[0].top;
    const { width } = await probeSize(stacked, signal);
    const local = Math.max(0, y0 - top);
    const total = used.reduce((s, b) => s + b.height, 0);
    const h = Math.min(total - local, region.h);
    return await encodeWebp(stacked, outPath, {
      quality,
      maxWidth,
      signal,
      crop: { x: 0, y: local * pxScale, w: width, h: h * pxScale },
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

// Screencast frames (JPEG files + wall-clock timestamps in seconds) → H.264 MP4
// at a constant frame rate. Frames are duplicated/dropped to the target rate
// from their real timestamps, so the result plays at real-world speed.
export async function encodeVideo(
  frames: { file: string; ts: number }[],
  outPath: string,
  {
    fps = 30,
    crf = 25,
    width = 1440,
    signal,
  }: { fps?: number; crf?: number; width?: number; signal?: AbortSignal } = {},
): Promise<{ path: string; width: number; height: number; duration: number } | null> {
  if (frames.length < 2) return null;
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-vid-'));
  try {
    const list = path.join(tmp, 'frames.txt');
    const lines: string[] = [];
    for (let i = 0; i < frames.length; i++) {
      const dur =
        i + 1 < frames.length ? Math.max(1 / fps, frames[i + 1].ts - frames[i].ts) : 1 / fps;
      lines.push(`file '${frames[i].file.replace(/'/g, "'\\''")}'`);
      lines.push(`duration ${Math.min(dur, 2).toFixed(4)}`);
    }
    // concat demuxer: the last file must be repeated for its duration to apply.
    lines.push(`file '${frames[frames.length - 1].file.replace(/'/g, "'\\''")}'`);
    fs.writeFileSync(list, lines.join('\n'));
    await runFfmpeg(
      [
        '-protocol_whitelist',
        'file',
        '-f',
        'concat',
        '-safe',
        '0',
        '-i',
        list,
        '-vf',
        `fps=${fps},scale='min(iw,${width})':-2:flags=lanczos,format=yuv420p`,
        '-c:v',
        'libx264',
        '-preset',
        'medium',
        '-crf',
        String(crf),
        '-movflags',
        '+faststart',
        '-an',
        '-y',
        outPath,
      ],
      { signal, timeoutMs: 300_000 },
    );
    if (!fs.existsSync(outPath)) return null;
    const size = await probeSize(outPath, signal);
    const duration = frames[frames.length - 1].ts - frames[0].ts;
    return { path: outPath, ...size, duration: Math.round(duration * 10) / 10 };
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

// Short, light hover preview from the scroll video: sped up, smaller, capped.
export async function encodePreview(
  video: string,
  outPath: string,
  {
    speed = 2.5,
    width = 720,
    maxSeconds = 9,
    signal,
  }: { speed?: number; width?: number; maxSeconds?: number; signal?: AbortSignal } = {},
): Promise<string | null> {
  try {
    await runFfmpeg(
      [
        '-protocol_whitelist',
        'file',
        '-i',
        video,
        '-vf',
        `setpts=PTS/${speed},fps=24,scale=${width}:-2:flags=lanczos,format=yuv420p`,
        '-t',
        String(maxSeconds),
        '-c:v',
        'libx264',
        '-preset',
        'medium',
        '-crf',
        '30',
        '-movflags',
        '+faststart',
        '-an',
        '-y',
        outPath,
      ],
      { signal, timeoutMs: 180_000 },
    );
    return fs.existsSync(outPath) ? outPath : null;
  } catch (e) {
    if ((e as Error)?.name === 'AbortError') throw e;
    return null;
  }
}

// Bounded parallelism for encode jobs (ffmpeg processes are CPU-heavy).
export function createPool(limit: number): <T>(task: () => Promise<T>) => Promise<T> {
  let active = 0;
  const queue: (() => void)[] = [];
  const next = (): void => {
    if (active >= limit) return;
    const run = queue.shift();
    if (run) run();
  };
  return <T>(task: () => Promise<T>): Promise<T> =>
    new Promise<T>((resolve, reject) => {
      queue.push(() => {
        active++;
        task()
          .then(resolve, reject)
          .finally(() => {
            active--;
            next();
          });
      });
      next();
    });
}
