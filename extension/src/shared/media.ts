// Media URL helpers: the signed-CDN expiry of IG/FB URLs and what a media URL points at (used
// by compare.ts's export format and, later, by the extension's upload tasks, P2-17).

/** Instagram/Facebook CDN hosts sign URLs with an `oe` expiry (plan §2.13: archive by expiry). */
const SIGNED_CDN_HOST = /(?:^|\.)(?:cdninstagram\.com|fbcdn\.net)$/;
const MIN_PLAUSIBLE_EXPIRY = Date.UTC(2015, 0, 1);
const MAX_PLAUSIBLE_EXPIRY = Date.UTC(2100, 0, 1);

/** Expiry (unix ms) of a signed IG/FB CDN URL from its hex `oe` parameter, else null. */
export function parseCdnExpiry(url: string): number | null {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  if (!SIGNED_CDN_HOST.test(parsed.hostname)) return null;
  const oe = parsed.searchParams.get('oe');
  if (!oe || !/^[0-9a-f]{1,12}$/i.test(oe)) return null;
  const ms = parseInt(oe, 16) * 1000;
  return ms >= MIN_PLAUSIBLE_EXPIRY && ms <= MAX_PLAUSIBLE_EXPIRY ? ms : null;
}

/**
 * What a media URL points at. The parsers type a slide `video` but, for IG and X, keep only its
 * poster image (`video_versions` / `video_info.variants` are dropped); Pinterest keeps the
 * progressive MP4 or HLS URL itself.
 */
export type MediaUrlKind = 'image' | 'poster' | 'video';

const VIDEO_PATH = /\.(?:mp4|m3u8|mov|webm)$/i;

export function classifyMediaUrl(type: 'image' | 'video', url: string): MediaUrlKind {
  if (type !== 'video') return 'image';
  let pathname = '';
  try {
    pathname = new URL(url).pathname;
  } catch {
    return 'poster';
  }
  return VIDEO_PATH.test(pathname) ? 'video' : 'poster';
}

export function urlHost(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return '';
  }
}
