// On-demand recovery for a gallery cover whose old CDN URL has expired. Fetch
// only the canonical social post page, extract its fresh og:image, then hand the
// image to the normal local preview cache. Both requests use an isolated,
// cookie-free session; posts requiring login cannot be repaired this way.
import * as db from './db';
import { SOCIAL_UA } from './interceptor';
import { fetchAnonymousMedia } from './anonymous-media';
import { enqueuePreviews, isSupportedPreviewUrl } from './preview-cache';

const MAX_HTML_BYTES = 2 * 1024 * 1024;
const CONCURRENCY = 2;
const MAX_PENDING = 80;
const RETRY_MS = 15 * 60_000;
const BLOCK_MS = 30 * 60_000;
const pending: Array<{ id: string; onSaved: () => void }> = [];
const attempted = new Map<string, number>();
let active = 0;
let blockedUntil = 0;

function socialPostUrl(post: Shelfy.Post): URL | null {
  if (!post.postUrl) return null;
  try {
    const u = new URL(post.postUrl);
    if (u.protocol !== 'https:') return null;
    const host = u.hostname.toLowerCase();
    if (post.platform === 'instagram') {
      if (host !== 'www.instagram.com' && host !== 'instagram.com') return null;
      if (!/^\/(?:p|reel|tv)\/[A-Za-z0-9_-]+\/?$/.test(u.pathname)) return null;
    } else if (post.platform === 'twitter') {
      if (host !== 'x.com' && host !== 'twitter.com' && host !== 'www.x.com') return null;
      if (!/^\/[^/]+\/status\/\d+\/?$/.test(u.pathname)) return null;
    } else if (post.platform === 'pinterest') {
      if (!['www.pinterest.com', 'pinterest.com'].includes(host)) return null;
      if (!/^\/pin\/\d+\/?$/.test(u.pathname)) return null;
    } else return null;
    return u;
  } catch {
    return null;
  }
}

function decodeEntities(value: string): string {
  return value.replace(/&(#x[0-9a-f]+|#\d+|amp|quot|apos|lt|gt);/gi, (_whole, entity: string) => {
    const e = entity.toLowerCase();
    if (e === 'amp') return '&';
    if (e === 'quot') return '"';
    if (e === 'apos') return "'";
    if (e === 'lt') return '<';
    if (e === 'gt') return '>';
    if (e.startsWith('#x')) return String.fromCodePoint(parseInt(e.slice(2), 16));
    if (e.startsWith('#')) return String.fromCodePoint(parseInt(e.slice(1), 10));
    return '';
  });
}

export function metaContent(html: string, property: string): string | null {
  for (const tag of html.match(/<meta\b[^>]*>/gi) || []) {
    const attrs = new Map<string, string>();
    for (const match of tag.matchAll(/([\w:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')/g)) {
      attrs.set(match[1].toLowerCase(), decodeEntities(match[2] ?? match[3] ?? ''));
    }
    if ((attrs.get('property') || attrs.get('name'))?.toLowerCase() === property) {
      return attrs.get('content') || null;
    }
  }
  return null;
}

async function fetchPostHtml(post: Shelfy.Post): Promise<string> {
  const initialUrl = socialPostUrl(post);
  if (!initialUrl) throw new Error('Unsupported social post URL');
  let url: URL = initialUrl;
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), 12_000);
  try {
    for (let redirect = 0; redirect < 4; redirect++) {
      const res = await fetchAnonymousMedia(url.toString(), {
        redirect: 'manual',
        signal: ac.signal,
        headers: { 'User-Agent': SOCIAL_UA, Accept: 'text/html,application/xhtml+xml' },
      });
      if (res.status >= 300 && res.status < 400) {
        const location = res.headers.get('location');
        if (!location) throw new Error('Redirect without location');
        const next = new URL(location, url);
        if (!socialPostUrl({ ...post, postUrl: next.href })) throw new Error('Unexpected redirect');
        url = next;
        continue;
      }
      if (res.status === 429) {
        blockedUntil = Date.now() + BLOCK_MS;
        throw new Error('Source rate limited');
      }
      if (!res.ok || !res.body) throw new Error(`Source HTTP ${res.status}`);
      if (!(res.headers.get('content-type') || '').toLowerCase().includes('text/html')) {
        throw new Error('Source did not return HTML');
      }
      const parts: Uint8Array[] = [];
      let bytes = 0;
      for await (const part of res.body as unknown as AsyncIterable<Uint8Array>) {
        bytes += part.byteLength;
        if (bytes > MAX_HTML_BYTES) throw new Error('Source page too large');
        parts.push(part);
      }
      const joined = new Uint8Array(bytes);
      let at = 0;
      for (const part of parts) {
        joined.set(part, at);
        at += part.byteLength;
      }
      return new TextDecoder().decode(joined);
    }
    throw new Error('Too many redirects');
  } finally {
    clearTimeout(timer);
  }
}

async function repairOne(id: string, onSaved: () => void): Promise<void> {
  if (Date.now() < blockedUntil) return;
  db.clearMissingLocalPaths(id);
  const post = db.getPost(id);
  if (!post || post.previewPath || post.thumbnailPath || post.imagePath || !post.thumbnailUrl)
    return;
  if (!socialPostUrl(post)) return;
  const html = await fetchPostHtml(post);
  const canonical = metaContent(html, 'og:url');
  if (canonical) {
    const expected = new URL(post.postUrl!).pathname.split('/').filter(Boolean).pop();
    if (!expected || !new URL(canonical).pathname.split('/').includes(expected)) return;
  }
  const fresh = metaContent(html, 'og:image');
  if (!fresh || !isSupportedPreviewUrl(post.platform, fresh)) return;
  if (fresh !== post.thumbnailUrl && !db.refreshPreviewUrl(id, post.thumbnailUrl, fresh)) return;
  enqueuePreviews([{ id, platform: post.platform, thumbnailUrl: fresh }], onSaved);
}

function drain(): void {
  while (active < CONCURRENCY && pending.length && Date.now() >= blockedUntil) {
    const item = pending.shift()!;
    active++;
    void repairOne(item.id, item.onSaved)
      .catch((err: unknown) => {
        console.warn('[preview-repair]', String((err as Error)?.message || err));
      })
      .finally(() => {
        active--;
        drain();
      });
  }
  if (Date.now() < blockedUntil) pending.length = 0;
}

export function requestPreviewRepair(id: string, onSaved: () => void): boolean {
  if (!id || id.length > 256 || Date.now() < blockedUntil || pending.length >= MAX_PENDING)
    return false;
  const last = attempted.get(id) || 0;
  if (Date.now() - last < RETRY_MS) return false;
  attempted.set(id, Date.now());
  pending.push({ id, onSaved });
  drain();
  return true;
}
