// Shared pure helpers for the PostModal shell and its subcomponents — slide
// building, media picking, URL/label utilities and small persisted prefs.

import { assetUrl } from '../../lib/asset';

// Translator returned by useT — namespaced key + optional interpolation vars.
type Translate = (key: string, vars?: Record<string, string | number>) => string;

// What pickSlideMedia decides to render for a single slide: a local/remote image,
// a video, or the authenticated webview fallback. `src` is null only when assetUrl
// is handed a null path (guarded against at the call sites).
export type SlideMediaKind = 'image' | 'video' | 'webview';
export interface SlideMedia {
  kind: SlideMediaKind;
  src: string | null;
}

// A single slide as the modal consumes it. Persisted slides are full
// Shelfy.PostMedia rows (assignable to this); legacy in-memory posts synthesize a
// minimal slide without a `position`, so only the fields actually read here are
// required.
export type PostSlide = Pick<Shelfy.PostMedia, 'type' | 'url' | 'localPath'>;

// X/twitter redirects /<user>/status/<id> to the canonical URL, but a post captured
// without a username yields a broken "x.com//status/<id>" — fall back to the
// username-independent /i/web/status form in that case.
export function resolveUrl(post: Shelfy.Post): string | null {
  if (post.platform === 'twitter') {
    if (!post.postUrl || post.postUrl.includes('//status/')) {
      return `https://x.com/i/web/status/${post.id}`;
    }
  }
  return post.postUrl;
}

// Only http(s) URLs may be loaded into the authenticated webview fallback —
// post.postUrl is intercepted data (clamped in length only), so anything else
// (file:, javascript:, …) must never reach the persist:social session.
export function isHttpUrl(value: unknown): value is string {
  if (typeof value !== 'string') return false;
  try {
    const p = new URL(value).protocol;
    return p === 'http:' || p === 'https:';
  } catch {
    return false;
  }
}

export function formatTimestamp(timestamp: string | null, locale: string): string | null {
  if (!timestamp) return null;
  const date = new Date(timestamp);
  if (isNaN(date.getTime())) return timestamp;
  return date.toLocaleString(locale, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

// Videos start muted by default; once the user unmutes one, that choice sticks
// for subsequent videos (persisted across sessions).
const VIDEO_MUTED_KEY = 'postModal:videoMuted';
export function getVideoMutedPref(): boolean {
  return localStorage.getItem(VIDEO_MUTED_KEY) !== 'false';
}
export function setVideoMutedPref(muted: boolean): void {
  localStorage.setItem(VIDEO_MUTED_KEY, muted ? 'true' : 'false');
}

// Maps a media-type enum to its i18n key (resolved at render with the active `t`).
export const MEDIA_TYPE_KEY: Record<Shelfy.MediaType, string> = {
  image: 'mediaImage',
  images: 'mediaImages',
  carousel: 'mediaCarousel',
  video: 'mediaVideo',
  text: 'mediaText',
  website: 'mediaWebsite',
  file: 'mediaFile',
};

// Is this slide the site homepage? (url === final/root, or path is "/")
export function isHomepageUrl(rawUrl: string | null | undefined, finalUrl: string | null): boolean {
  if (!rawUrl) return false;
  try {
    const u = new URL(rawUrl);
    const path = u.pathname.replace(/\/+$/, '');
    if (!path) return true;
    if (finalUrl) {
      const fu = new URL(finalUrl);
      if (path === fu.pathname.replace(/\/+$/, '')) return true;
    }
  } catch {
    /* opaque url */
  }
  return false;
}

// Human-readable chip label for a captured web page: "Home" for the root, else a
// prettified last path segment (dashes → spaces, capitalised), capped in length.
// `t` is the postModal translator (passed in since this lives at module scope).
export function webPageLabel(
  rawUrl: string | null | undefined,
  finalUrl: string | null,
  index: number,
  t: Translate,
): string {
  if (isHomepageUrl(rawUrl, finalUrl)) return t('home');
  try {
    const u = new URL(rawUrl as string);
    const segs = u.pathname.split('/').filter(Boolean);
    const last = decodeURIComponent(segs[segs.length - 1] || '');
    const pretty = last
      .replace(/\.[a-z0-9]+$/i, '')
      .replace(/[-_]+/g, ' ')
      .trim();
    if (!pretty) return t('home');
    const capped = pretty.length > 28 ? pretty.slice(0, 27) + '…' : pretty;
    return capped.charAt(0).toUpperCase() + capped.slice(1);
  } catch {
    return t('page', { n: index + 1 });
  }
}

// The ordered slides to show for a post. Prefer the persisted media array;
// fall back to a single synthesized slide for legacy in-memory posts. For web
// references the captured pages follow the sitemap order, but we always pin the
// homepage as the first slide (item: "homepage sempre come prima slide").
export function buildSlides(post: Shelfy.Post): PostSlide[] {
  if (Array.isArray(post.media) && post.media.length > 0) {
    if (post.platform !== 'web') return post.media;
    const finalUrl = post.webFinalUrl || post.postUrl || post.webUrl;
    const homeIdx = post.media.findIndex((m) => isHomepageUrl(m?.url, finalUrl));
    if (homeIdx > 0) {
      const reordered = [...post.media];
      const [home] = reordered.splice(homeIdx, 1);
      reordered.unshift(home);
      return reordered;
    }
    return post.media;
  }
  if (post.videoPath) return [{ type: 'video', url: null, localPath: post.videoPath }];
  const localImage = post.imagePath || post.thumbnailPath;
  if (localImage) return [{ type: 'image', url: post.thumbnailUrl, localPath: localImage }];
  if (post.thumbnailUrl) return [{ type: 'image', url: post.thumbnailUrl, localPath: null }];
  return [];
}

// Decide what to render for a single slide. Local downloads win; otherwise show
// the remote image directly, falling back to the live page only when there's
// nothing else (e.g. carousel videos we don't download per-slide).
export function pickSlideMedia(
  post: Shelfy.Post,
  slide: PostSlide | undefined,
  slideCount: number,
): SlideMedia {
  if (!slide) return { kind: 'webview', src: resolveUrl(post) };
  if (slide.type === 'video') {
    // Per-slide video isn't downloaded; only a single-video post has a local file.
    if (post.videoPath && slideCount === 1) return { kind: 'video', src: assetUrl(post.videoPath) };
    if (slide.localPath) return { kind: 'video', src: assetUrl(slide.localPath) };
    return { kind: 'webview', src: resolveUrl(post) };
  }
  if (slide.localPath) return { kind: 'image', src: assetUrl(slide.localPath) };
  if (slide.url) return { kind: 'image', src: slide.url };
  return { kind: 'webview', src: resolveUrl(post) };
}
