// Persist small gallery covers as soon as sync gives us fresh CDN URLs. These
// files are deliberately separate from downloaded originals: an offline preview
// must not make a carousel or video appear fully archived.
import { app, nativeImage } from 'electron';
import fs from 'fs';
import path from 'path';
import crypto from 'crypto';
import { assertSafeMediaUrl } from './net-safety';
import { SOCIAL_UA } from './interceptor';
import { fetchAnonymousMedia } from './anonymous-media';
import * as db from './db';

export type PreviewCandidate = { id: string; platform: string; thumbnailUrl: string };

const MAX_BYTES = 8 * 1024 * 1024;
const WIDTH = 640;
const CONCURRENCY = 3;
const queue: Array<{ post: PreviewCandidate; onSaved?: () => void }> = [];
const queued = new Set<string>();
let running = 0;
let failed = 0;

function allowedHost(platform: string, host: string): boolean {
  if (platform === 'instagram')
    return host.endsWith('.cdninstagram.com') || host.endsWith('.fbcdn.net');
  if (platform === 'twitter') return host === 'pbs.twimg.com';
  if (platform === 'pinterest') return host === 'i.pinimg.com' || host.endsWith('.pinimg.com');
  return false;
}

function checkedUrl(platform: string, raw: string): URL {
  const url = assertSafeMediaUrl(raw);
  if (url.protocol !== 'https:' || !allowedHost(platform, url.hostname.toLowerCase())) {
    throw new Error('Unsupported preview host');
  }
  return url;
}

export function isSupportedPreviewUrl(platform: string, raw: string): boolean {
  try {
    checkedUrl(platform, raw);
    return true;
  } catch {
    return false;
  }
}

async function fetchPreview(post: PreviewCandidate): Promise<Buffer> {
  let url = checkedUrl(post.platform, post.thumbnailUrl);
  const referer =
    post.platform === 'instagram'
      ? 'https://www.instagram.com/'
      : post.platform === 'twitter'
        ? 'https://x.com/'
        : 'https://www.pinterest.com/';
  const ac = new AbortController();
  const timeout = setTimeout(() => ac.abort(), 15_000);
  try {
    for (let redirect = 0; redirect < 4; redirect++) {
      const res = await fetchAnonymousMedia(url.toString(), {
        redirect: 'manual',
        signal: ac.signal,
        headers: {
          'User-Agent': SOCIAL_UA,
          Referer: referer,
          Accept: 'image/avif,image/webp,image/apng,image/*,*/*;q=0.8',
        },
      });
      if (res.status >= 300 && res.status < 400) {
        const location = res.headers.get('location');
        if (!location) throw new Error('Redirect without location');
        url = checkedUrl(post.platform, new URL(location, url).href);
        continue;
      }
      if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
      const len = Number(res.headers.get('content-length'));
      if (len > MAX_BYTES) throw new Error('Preview too large');
      const type = res.headers.get('content-type') || '';
      if (!type.toLowerCase().startsWith('image/')) throw new Error('Not an image');
      const chunks: Buffer[] = [];
      let bytes = 0;
      for await (const part of res.body as unknown as AsyncIterable<Uint8Array>) {
        bytes += part.byteLength;
        if (bytes > MAX_BYTES) throw new Error('Preview too large');
        chunks.push(Buffer.from(part));
      }
      const image = nativeImage.createFromBuffer(Buffer.concat(chunks));
      if (image.isEmpty()) throw new Error('Image decode failed');
      const size = image.getSize();
      const tile = size.width > WIDTH ? image.resize({ width: WIDTH, quality: 'good' }) : image;
      return tile.toJPEG(82);
    }
    throw new Error('Too many redirects');
  } finally {
    clearTimeout(timeout);
  }
}

async function save(post: PreviewCandidate): Promise<boolean> {
  const data = await fetchPreview(post);
  const dir = path.join(app.getPath('userData'), 'assets', 'previews');
  await fs.promises.mkdir(dir, { recursive: true });
  const key = crypto.createHash('sha256').update(`${post.id}\n${post.thumbnailUrl}`).digest('hex');
  const dest = path.join(dir, `${key}.jpg`);
  const tmp = `${dest}.${process.pid}.part`;
  try {
    await fs.promises.writeFile(tmp, data);
    await fs.promises.rename(tmp, dest);
    if (db.setPreviewPath(post.id, post.thumbnailUrl, dest)) return true;
    await fs.promises.unlink(dest).catch(() => {});
    return false;
  } finally {
    await fs.promises.unlink(tmp).catch(() => {});
  }
}

function drain(): void {
  while (running < CONCURRENCY && queue.length > 0) {
    const item = queue.shift()!;
    running++;
    void save(item.post)
      .then((saved) => {
        if (saved) item.onSaved?.();
      })
      .catch((err: unknown) => {
        failed++;
        if (failed <= 3 || failed % 100 === 0) {
          const cause = (err as Error & { cause?: { code?: string } })?.cause?.code;
          console.warn(
            `[preview-cache] ${failed} failed; latest: ${String((err as Error)?.message || err)}${cause ? ` (${cause})` : ''}`,
          );
        }
      })
      .finally(() => {
        queued.delete(`${item.post.id}\n${item.post.thumbnailUrl}`);
        running--;
        drain();
      });
  }
}

export function enqueuePreviews(posts: PreviewCandidate[], onSaved?: () => void): number {
  let added = 0;
  for (const post of posts) {
    try {
      checkedUrl(post.platform, post.thumbnailUrl);
    } catch {
      continue;
    }
    const key = `${post.id}\n${post.thumbnailUrl}`;
    if (queued.has(key)) continue;
    queued.add(key);
    queue.push({ post, onSaved });
    added++;
  }
  drain();
  return added;
}
