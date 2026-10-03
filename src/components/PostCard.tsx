import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  HardDrive,
  Link,
  Video,
  Image,
  Layers,
  AlignLeft,
  Check,
  Sparkles,
  Globe,
  Award,
  FileText,
} from 'lucide-react';
import type { MediaUrls } from '../api/ShelfyClient';
import { useShelfy } from '../api/ShelfyProvider';
import { useLongPress } from '../hooks/useLongPress';

// Target box for grid-tile images. Local files are often full-resolution
// originals (multi-MB); the asset protocol serves a cached fit-in-640px copy
// instead so a scroll-burst of tiles doesn't stall on huge decodes (the web
// serves its 480px rendition). The modal still loads the original. Keep in
// sync with main.js PREWARM_TILE_WIDTH.
const TILE_WIDTH = 640;
import SourceIcon, { PLATFORM_COLORS } from './SourceIcon';
import { useT, useLang, localeTag } from '../i18n';

// `title` is a valid global SVG attribute (native hover tooltip) that lucide
// spreads onto its <svg>, but React's SVGAttributes typings omit it. Declare it
// so the icon tooltips below (and elsewhere) stay typed without `any`.
//
// `fetchpriority` (lowercase): @types/react 18.3's `ImgHTMLAttributes` already
// declares the camelCase `fetchPriority`, but react-dom 18.3.1's own attribute
// table doesn't special-case it yet, so that spelling only warns at runtime
// ("React does not recognize the `fetchPriority` prop…") and never reaches the
// DOM. The plain lowercase HTML attribute name passes straight through
// react-dom's generic (unrecognized-prop) path instead, which is what the
// card's cover <img> below actually uses.
declare module 'react' {
  // React's declaration uses this generic name; keep it for interface merging.
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  interface SVGAttributes<T> {
    title?: string;
  }
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  interface ImgHTMLAttributes<T> {
    fetchpriority?: 'high' | 'low' | 'auto';
  }
}

// Media overscan: the cover image / video preview render 6px wider & taller than
// the tile and shift up-left 3px, so the element bleeds 3px past every edge of the
// card's rounded `overflow-hidden` clip. The clip then always cuts through solid
// image INTERIOR — never the element's own antialiased (and, once `u-media-zoom`
// promotes it, composited) border — so no pale seam shows at rest, mid-zoom, or on
// a static video. `maxWidth:'none'` defeats the img preflight `max-width:100%` cap.
// Kept as an inline style (not a Tailwind arbitrary value) because `calc(100%+6px)`
// without spaces around `+` is INVALID CSS and silently dropped — which is exactly
// what made the earlier overscan a no-op, leaving the seam visible until hover.
const MEDIA_OVERSCAN: React.CSSProperties = {
  top: '-3px',
  left: '-3px',
  width: 'calc(100% + 6px)',
  height: 'calc(100% + 6px)',
  maxWidth: 'none',
};

type Translate = (key: string, vars?: Record<string, unknown>) => string;

// web_palette_json may be hex strings OR { hex, role } objects (F4 is loose);
// normalise to a list of hex strings, dropping anything unusable.
function paletteHexes(palette: unknown): string[] {
  if (!Array.isArray(palette)) return [];
  return palette
    .map((c): string | null =>
      typeof c === 'string'
        ? c
        : c &&
            typeof c === 'object' &&
            'hex' in c &&
            typeof (c as { hex: unknown }).hex === 'string'
          ? (c as { hex: string }).hex
          : null,
    )
    .filter((h): h is string => typeof h === 'string' && /^#?[0-9a-fA-F]{3,8}$/.test(h.trim()))
    .map((h) => (h.trim().startsWith('#') ? h.trim() : `#${h.trim()}`));
}

// Per-platform brand glyph for the bottom-left identity chip, delegating to the
// shared SourceIcon (single source of truth) at the card's 11px size.
function PlatformIcon({ platform }: { platform: Shelfy.Platform }): React.JSX.Element {
  return <SourceIcon platform={platform} size={11} className="text-white/85" />;
}

// Site favicon with a lucide Globe fallback. Prefers the copy stored at
// capture time (plan §1.2 #10: the SPA makes zero third-party requests);
// `faviconPath` is absent for every post today on the desktop (it has no
// capture-time favicon field yet), so `capabilities.localFiles` there falls
// back to the site's own /favicon.ico exactly as before — unchanged desktop
// behavior. On the web (no `localFiles`), a missing stored favicon falls back
// to the neutral glyph instead, never a live per-domain request. Shared
// between the rest-state domain chip and the no-screenshot fallback.
function Favicon({
  domain,
  faviconPath,
  size = 11,
}: {
  domain: string | null;
  faviconPath?: string | null;
  size?: number;
}): React.JSX.Element {
  const { media, capabilities } = useShelfy();
  const [iconFailed, setIconFailed] = useState<boolean>(false);
  const stored = faviconPath ? media.file(faviconPath) : null;
  let src: string | null = stored;
  if (!src && capabilities.localFiles) {
    try {
      if (domain) src = new URL('/favicon.ico', `https://${domain}`).href;
    } catch {
      /* malformed domain → Globe fallback below */
    }
  }
  if (iconFailed || !src) {
    return <Globe size={size} className="text-white/85 shrink-0" />;
  }
  return (
    <img
      src={src}
      alt=""
      width={size}
      height={size}
      style={{ width: size, height: size }}
      className="rounded-sm shrink-0"
      onError={() => setIconFailed(true)}
      draggable={false}
    />
  );
}

// For web posts the bottom-left identity is a favicon + domain chip instead of
// a social platform glyph.
function WebDomainBadge({
  domain,
  faviconPath,
}: {
  domain: string | null;
  faviconPath?: string | null;
}): React.JSX.Element {
  if (!domain) return <Globe size={11} className="text-white/85" />;
  return (
    <div className="flex items-center gap-1 min-w-0">
      <Favicon domain={domain} faviconPath={faviconPath} size={11} />
      <span className="text-white/85 text-[10px] truncate max-w-[110px]">{domain}</span>
    </div>
  );
}

function MediaTypeIcon({
  mediaType,
  mediaCount = 1,
}: {
  mediaType: Shelfy.MediaType | null;
  mediaCount?: number;
}): React.JSX.Element {
  const isMulti = mediaType === 'carousel' || mediaType === 'images';
  const cls = 'text-white/85';

  if (mediaType === 'video') {
    return <Video size={11} className={cls} />;
  }
  if (isMulti) {
    return (
      <div className="flex items-center gap-0.5">
        <Layers size={10} className={cls} />
        {mediaCount > 1 && (
          <span className="text-white/70 text-[9px] font-medium leading-none">{mediaCount}</span>
        )}
      </div>
    );
  }
  if (mediaType === 'text') {
    return <AlignLeft size={10} className={cls} />;
  }
  if (mediaType === 'file') {
    return <FileText size={10} className={cls} />;
  }
  if (mediaType === 'website') {
    return (
      <div className="flex items-center gap-0.5">
        <Globe size={10} className={cls} />
        {mediaCount > 1 && (
          <span className="text-white/70 text-[9px] font-medium leading-none">{mediaCount}</span>
        )}
      </div>
    );
  }
  return <Image size={10} className={cls} />;
}

function OfflineIcon({
  isDownloaded,
  t,
}: {
  isDownloaded: boolean;
  t: Translate;
}): React.JSX.Element {
  if (isDownloaded) {
    return <HardDrive size={10} className="text-white/85" title={t('savedOffline')} />;
  }
  return <Link size={10} className="text-white/40" title={t('linkOnly')} />;
}

// A text post has its own editorial layout. Keep the author visible at rest and
// reserve the top for the selection checkbox, so neither chrome nor the footer
// can cover the excerpt in a small square tile.
function TextCard({
  post,
  selectable,
  t,
}: {
  post: Shelfy.Post;
  selectable: boolean;
  t: Translate;
}): React.JSX.Element {
  const accent = PLATFORM_COLORS[post.platform];
  return (
    <div
      data-testid="text-card"
      className="relative w-full h-full overflow-hidden"
      style={{
        background: `radial-gradient(circle at 0% 0%, ${accent}25, transparent 68%), #17191f`,
      }}
    >
      <div
        className="absolute inset-y-0 left-0 w-[2px] opacity-70"
        style={{ backgroundColor: accent }}
      />
      {!selectable && (
        <span
          aria-hidden="true"
          className="absolute top-1 left-3 font-serif text-[30px] leading-none text-white/30"
        >
          “
        </span>
      )}
      <span className="absolute top-3 right-3 text-[9px] font-semibold tracking-[0.16em] text-white/40">
        {t('textLabel')}
      </span>
      <div className="absolute inset-x-3 top-8 bottom-9 flex items-center overflow-hidden">
        <p className="max-h-full text-[12px] font-medium leading-[1.4] text-[#f0f1f5] break-words line-clamp-6">
          {post.text}
        </p>
      </div>
      <div className="absolute inset-x-3 bottom-3 flex items-center gap-1.5 border-t border-white/10 pt-2 text-[10px] leading-none text-white/55">
        <SourceIcon
          platform={post.platform}
          size={11}
          className="shrink-0"
          style={{ color: accent }}
        />
        <span className="min-w-0 truncate">@{post.authorUsername || t('unknownAuthor')}</span>
      </div>
    </div>
  );
}

// Web post without a usable screenshot: favicon + page title + domain, so the
// card still answers "what site is this?" at a glance.
function WebFallback({ post, t }: { post: Shelfy.Post; t: Translate }): React.JSX.Element {
  const rawTitle = post.webMeta?.title || post.webMeta?.siteName || null;
  const title = typeof rawTitle === 'string' ? rawTitle : null;
  return (
    <div
      data-testid="web-fallback"
      className="w-full h-full flex flex-col items-center justify-center gap-1.5 px-4 text-center"
      style={{ backgroundColor: '#161618' }}
    >
      <Favicon domain={post.webDomain} faviconPath={post.webFaviconPath} size={22} />
      {title ? (
        <p className="text-xs text-gray-200 leading-snug line-clamp-2 break-words">{title}</p>
      ) : (
        <p className="text-xs text-gray-400">{post.webDomain || t('website')}</p>
      )}
      {title && post.webDomain && (
        <p className="text-[10px] text-gray-500 truncate max-w-full">{post.webDomain}</p>
      )}
    </div>
  );
}

// Manual bookmark without a generated preview: file glyph + the user's own note
// (or the generic bookmark label) instead of an empty thumbnail.
function ManualFallback({ post, t }: { post: Shelfy.Post; t: Translate }): React.JSX.Element {
  return (
    <div
      data-testid="manual-fallback"
      className="w-full h-full flex flex-col items-center justify-center gap-1.5 px-4 text-center"
      style={{ backgroundColor: '#161618' }}
    >
      <FileText size={22} className="text-gray-500" />
      {post.userNote ? (
        <p className="text-xs text-gray-300 leading-snug line-clamp-2 break-words">
          {post.userNote}
        </p>
      ) : (
        <p className="text-[10px] text-gray-500">{t('manualBookmark')}</p>
      )}
    </div>
  );
}

// Social post whose media can't be shown (no local file and its remote thumbnail
// has expired — Instagram/Twitter CDN URLs are signed and short-lived), or which
// simply has no media: platform glyph + author handle. When the post DID have
// media (so the image is genuinely gone, not absent), a muted "media unavailable"
// line says so — instead of dumping the caption as if it were a text post.
function SocialFallback({ post, t }: { post: Shelfy.Post; t: Translate }): React.JSX.Element {
  const hadMedia =
    !!(
      post.thumbnailUrl ||
      post.thumbnailPath ||
      post.previewPath ||
      post.imagePath ||
      post.videoPath
    ) ||
    (Array.isArray(post.media) && post.media.length > 0);
  return (
    <div
      data-testid="social-fallback"
      className="w-full h-full flex flex-col items-center justify-center gap-1 px-4"
      style={{ backgroundColor: '#161618' }}
    >
      <SourceIcon platform={post.platform} size={20} className="text-gray-500" />
      <p className="text-[11px] text-gray-400 truncate max-w-full">
        @{post.authorUsername || t('unknownAuthor')}
      </p>
      {hadMedia && (
        <p className="text-[10px] text-gray-600 truncate max-w-full">{t('mediaUnavailable')}</p>
      )}
    </div>
  );
}

function formatTimestamp(timestamp: string | null, locale: string): string {
  if (!timestamp) return '';
  try {
    const date = new Date(timestamp);
    if (isNaN(date.getTime())) return timestamp;
    return date.toLocaleDateString(locale, { year: 'numeric', month: 'short', day: 'numeric' });
  } catch {
    return timestamp;
  }
}

// The ordered image sources to cycle through on hover. Prefers the downloaded
// file for each carousel slide, falling back to its remote URL.
function buildSlideshowImages(post: Shelfy.Post, media: MediaUrls): string[] {
  if (!Array.isArray(post.media)) return [];
  return post.media
    .filter((m): m is Shelfy.PostMedia => !!m && m.type === 'image')
    .map((m) => (m.localPath ? media.tile(m.localPath, TILE_WIDTH) : m.url))
    .filter((src): src is string => Boolean(src));
}

const SLIDESHOW_INTERVAL_MS = 800;

// Tap-to-preview (P1-02, touch only): there is no hover on touch, so the FIRST
// tap on a card previews it (the same overlay/video hover already shows) and
// the SECOND tap opens it — mirroring "hover, then click" with two taps. A
// window-wide event lets one card's tap close any other card's open preview
// (at most one previews at a time), without lifting state through the grid
// components that render PostCard (VirtualPostGrid, InfiniteCanvas) and are
// not this task's to edit.
const TOUCH_PREVIEW_EVENT = 'shelfy:postcard-touch-preview';

interface PostCardProps {
  post: Shelfy.Post;
  onOpen: (post: Shelfy.Post, event?: React.SyntheticEvent) => void;
  selectable?: boolean;
  selected?: boolean;
  onQuickSelect?: (post: Shelfy.Post, event: React.SyntheticEvent) => void;
  // Viewport-first loading (plan §2.19): the rows a grid paints on its very
  // first frame set this so their cover competes for bandwidth/decode ahead of
  // everything scrolled in afterward. Threads straight to the <img>'s
  // `fetchpriority`; default 'auto' matches the previous (unprioritized)
  // behavior for every other row.
  priority?: boolean;
}

function PostCard({
  post,
  onOpen,
  selectable = false,
  selected = false,
  onQuickSelect,
  priority = false,
}: PostCardProps): React.JSX.Element {
  const t = useT('postCard');
  const { lang } = useLang();
  // Local file references resolve through the client (asset:// on the desktop,
  // /media on the web).
  const { media, capabilities } = useShelfy();
  const isWeb = post.platform === 'web';
  const isManual = post.platform === 'manual';
  const localImage = post.thumbnailPath || post.imagePath || post.previewPath;
  const imageSrc = localImage ? media.tile(localImage, TILE_WIDTH) : post.thumbnailUrl || null;
  const isDownloaded = !!(post.thumbnailPath || post.imagePath || post.videoPath);

  // Web-only extras — all render-conditional so a freshly-added (raw) site that
  // only has a screenshot + domain shows nothing else.
  const awards = isWeb && Array.isArray(post.webAwards) ? post.webAwards.filter(Boolean) : [];
  const swatches = useMemo(
    () => (isWeb ? paletteHexes(post.webPalette).slice(0, 5) : []),
    [isWeb, post.webPalette],
  );

  // Marks posts carrying a local AI analysis: both a generated description and
  // at least one generated tag.
  const hasAiAnalysis =
    !!(post.aiDescription && post.aiDescription.trim()) &&
    Array.isArray(post.aiTags) &&
    post.aiTags.length > 0;

  // Hover recall: up to 3 tags (AI first, the user's own as fallback) shown as
  // micro-chips in the hover overlay; when there are none, the caption's first
  // line steps in so the overlay always says *something* about the content.
  const hoverTags = useMemo<string[]>(() => {
    const src =
      Array.isArray(post.aiTags) && post.aiTags.length > 0
        ? post.aiTags
        : Array.isArray(post.userTags)
          ? post.userTags
          : [];
    return src.filter(Boolean).slice(0, 3);
  }, [post.aiTags, post.userTags]);
  const hasText = typeof post.text === 'string' && post.text.trim().length > 0;
  // Memoized: split/map/find ran on every card mount before, even though it's only
  // read inside the (now lazy) hover overlay.
  const firstTextLine = useMemo<string | null>(
    () =>
      hasText && post.text
        ? (post.text
            .split('\n')
            .map((l) => l.trim())
            .find(Boolean) ?? null)
        : null,
    [hasText, post.text],
  );

  // Hover preview: a downloaded single-video post autoplays muted; a multi-image
  // carousel runs a slideshow; a single image does nothing.
  const localVideoSrc = post.videoPath ? media.file(post.videoPath) : null;
  // Depend on a stable key (id + media length) rather than the whole post object,
  // so a new post reference with unchanged media doesn't rebuild the array.
  const slideshowImages = useMemo<string[]>(
    () => buildSlideshowImages(post, media),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [post.id, Array.isArray(post.media) ? post.media.length : 0, media],
  );

  const [hovering, setHovering] = useState<boolean>(false);
  // Lazy hover chrome: the gradient overlay (author/tags/swatches/timestamp +
  // lucide SVGs), the <video> preview and the quick-select checkbox are only ever
  // seen on hover/focus, yet they are ~20 of the card's ~35 DOM nodes. Mounting
  // them only after the first pointer-enter / focus (one-way: they stay mounted
  // so a re-hover still animates) makes the at-rest mount — the cost the
  // virtualizer pays for every row it reveals during a scroll — far cheaper.
  const [everHovered, setEverHovered] = useState<boolean>(false);
  const [slide, setSlide] = useState<number>(0);
  const [videoReady, setVideoReady] = useState<boolean>(false);
  // Falls back to the informative no-image block when the primary thumbnail 404s
  // / is blocked / its local file was moved out from under the DB.
  const [imageFailed, setImageFailed] = useState<boolean>(false);
  // Blur-up reveal: `imageLoaded` drives the tile's fade-in over the blurred
  // placeholder (post.thumbBlur, a ~24px data URI shipped with the post row);
  // `imageSettled` unmounts the placeholder once the fade has finished, so at
  // most one blurred layer per still-loading card is ever composited.
  const [imageLoaded, setImageLoaded] = useState<boolean>(false);
  const [imageSettled, setImageSettled] = useState<boolean>(false);
  // Memory-cached images (a virtualized row scrolling back in) are complete
  // before onLoad can attach: show them instantly — no blur flash, no re-fade.
  const handleImageRef = useCallback((el: HTMLImageElement | null) => {
    if (el && el.complete && el.naturalWidth > 0) {
      setImageLoaded(true);
      setImageSettled(true);
    }
  }, []);
  const videoRef = useRef<HTMLVideoElement | null>(null);

  // ── Touch: tap-to-preview + long-press to select (P1-02) ────────────────────
  // A mouse keeps today's hover/click/quick-select exactly as they are; only a
  // touch/pen pointer takes this path. Tracked in a ref (not state) so the
  // click/hover handlers below can read it synchronously without waiting for
  // a render, and so a mouse-only test/user never pays for it.
  const lastPointerTypeRef = useRef<string>('mouse');
  const [touchPreviewing, setTouchPreviewing] = useState<boolean>(false);
  const touchPreviewingRef = useRef<boolean>(false);
  touchPreviewingRef.current = touchPreviewing;
  const longPress = useLongPress((e) => {
    // Mirrors handleQuickSelectClick's own guard below: already selecting, no-op.
    if (!selectable) onQuickSelect?.(post, e);
  });

  // At most one card previews at a time: another card's tap drops this one's
  // (closes the overlay, stops any playing video/slideshow).
  useEffect(() => {
    function onOtherPreview(e: Event): void {
      const id = (e as CustomEvent<string>).detail;
      if (id === post.id || !touchPreviewingRef.current) return;
      setTouchPreviewing(false);
      setHovering(false);
      setSlide(0);
      setVideoReady(false);
    }
    window.addEventListener(TOUCH_PREVIEW_EVENT, onOtherPreview);
    return () => window.removeEventListener(TOUCH_PREVIEW_EVENT, onOtherPreview);
  }, [post.id]);

  useEffect(() => {
    if (!hovering || slideshowImages.length < 2) return undefined;
    const id = setInterval(
      () => setSlide((s) => (s + 1) % slideshowImages.length),
      SLIDESHOW_INTERVAL_MS,
    );
    return () => clearInterval(id);
  }, [hovering, slideshowImages.length]);

  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    v.muted = true;
    if (hovering) {
      // play() returns a Promise in browsers but `undefined` in some
      // environments (jsdom under test, very old engines) — guard the .catch.
      const p = v.play();
      if (p && typeof p.catch === 'function') p.catch(() => {});
    } else {
      v.pause();
      try {
        v.currentTime = 0;
      } catch {
        /* not seekable yet */
      }
    }
    // No teardown here: the src binding (src={hovering ? … : undefined}) already
    // drops on hover-out, so we must not removeAttribute/load() on every toggle.
  }, [hovering]);

  // Unmount-only teardown: if the card unmounts (e.g. virtualized out while
  // hovering), stop playback and release the source so a detached <video> isn't
  // left decoding in the background. Reading the live ref at unmount is intentional
  // (we want the element as it exists then), hence the lint suppression.
  useEffect(() => {
    return () => {
      // eslint-disable-next-line react-hooks/exhaustive-deps
      const el = videoRef.current;
      if (el) {
        el.pause();
        el.removeAttribute('src');
        el.load();
      }
    };
  }, []);

  function handleEnter(): void {
    // Touch/pen fire a synthetic hover sequence after a tap too; that preview
    // is driven explicitly from handleClick below instead.
    if (lastPointerTypeRef.current !== 'mouse') return;
    setHovering(true);
    setEverHovered(true);
  }

  function handleLeave(): void {
    if (lastPointerTypeRef.current !== 'mouse') return;
    setHovering(false);
    setSlide(0);
    setVideoReady(false);
  }

  // Tracks the pointer that is about to press/click/long-press, and feeds the
  // long-press timer (a mouse pointerdown is a no-op there).
  function handlePointerDown(e: React.PointerEvent): void {
    lastPointerTypeRef.current = e.pointerType;
    longPress.onPointerDown(e);
  }

  // A touch/pen long-press opens its own native menu (iOS "Save Image", a
  // context menu on Android/desktop touch screens) that would otherwise race
  // the long-press-to-select gesture above.
  function handleContextMenu(e: React.MouseEvent): void {
    if (lastPointerTypeRef.current !== 'mouse') e.preventDefault();
  }

  function handleClick(e: React.MouseEvent): void {
    // The long-press above already acted (entered selection) — swallow the
    // trailing click the browser still dispatches once the finger lifts.
    if (longPress.consumeFired()) return;
    if (!selectable && lastPointerTypeRef.current !== 'mouse' && !touchPreviewingRef.current) {
      // First tap on touch/pen: preview instead of opening — there is no
      // hover to do it for us. A second tap on this same card (now that
      // touchPreviewingRef is true) falls through and opens it below.
      setEverHovered(true);
      setHovering(true);
      setTouchPreviewing(true);
      window.dispatchEvent(new CustomEvent(TOUCH_PREVIEW_EVENT, { detail: post.id }));
      return;
    }
    // Forward the original click event so callers can read modifier keys
    // (e.g. shift-click range selection in the Gallery).
    onOpen(post, e);
  }

  // Keyboard activation: Enter / Space opens the post (or toggles selection in
  // select mode). Forwards shiftKey so range-select works from the keyboard too.
  function handleKeyDown(e: React.KeyboardEvent): void {
    if (e.key === 'Enter' || e.key === ' ' || e.key === 'Spacebar') {
      e.preventDefault();
      onOpen(post, e);
    }
  }

  // Hover quick-select (Google-Photos-like): selecting from the checkbox must
  // never open the modal nor reach the grid's drag-select handlers.
  function handleQuickSelectClick(e: React.MouseEvent): void {
    e.stopPropagation();
    onQuickSelect?.(post, e);
  }

  function handleQuickSelectKeyDown(e: React.KeyboardEvent): void {
    if (e.key === 'Enter' || e.key === ' ' || e.key === 'Spacebar') {
      e.preventDefault();
      e.stopPropagation();
      onQuickSelect?.(post, e);
    }
  }

  // Web cards show only the hero screenshot at rest (no on-hover page slideshow
  // in the POC — the other pages live in the modal carousel).
  const slideshowActive = !isWeb && hovering && slideshowImages.length >= 2;
  const displayedImage = slideshowActive ? slideshowImages[slide] : imageSrc;
  const imageShowable = !!displayedImage && !imageFailed;

  // Whether this post is a MEDIA post at all (it has, or once had, a thumbnail /
  // image / video / carousel). A signed remote thumbnail that has since expired
  // still counts — the post is a media post whose image is merely gone, not a
  // text post. Used to keep such posts OUT of the typographic text treatment.
  const hasMediaSource =
    !!(
      post.thumbnailPath ||
      post.previewPath ||
      post.imagePath ||
      post.videoPath ||
      post.thumbnailUrl
    ) ||
    (Array.isArray(post.media) && post.media.length > 0);

  // Only GENUINE text posts get the typographic quote-card: an explicit text
  // mediaType, or a social post that truly carries no media (just a caption).
  // A media post whose image failed to load no longer masquerades as a text card
  // — it drops to the informative SocialFallback ("media unavailable") instead.
  const isTextCard =
    post.mediaType === 'text' || (!hasMediaSource && hasText && !isWeb && !isManual);

  // A new image source (post change, or hover-slideshow frame) clears a stale
  // failure flag so a later valid src isn't permanently hidden behind the fallback.
  useEffect(() => {
    setImageFailed(false);
  }, [displayedImage]);

  // Settle fallback: ALWAYS arm the timer, not only after onLoad. transitionend can
  // be missed (reduced-motion), but more importantly a cover whose <img> fires
  // NEITHER load NOR error — e.g. a video post with an expired/hanging remote
  // thumbnail or no saved local cover — would otherwise leave the pixelated blur-up
  // placeholder composited on top of the card forever. Loaded → quick settle as the
  // fade ends; not loaded → a longer failsafe, then drop the blur regardless.
  useEffect(() => {
    if (imageSettled) return undefined;
    const t = setTimeout(() => setImageSettled(true), imageLoaded ? 450 : 1200);
    return () => clearTimeout(t);
  }, [imageLoaded, imageSettled]);

  return (
    <div
      data-testid="post-card"
      data-selected={selected ? 'true' : 'false'}
      // Button-like so keyboard / assistive-tech users can reach and activate the
      // primary surface of the app. In select mode it conveys toggle state via
      // aria-pressed; otherwise it just opens the post modal.
      role="button"
      tabIndex={0}
      aria-pressed={selectable ? selected : undefined}
      aria-label={
        post.text ||
        (isWeb ? post.webDomain : post.authorUsername && `@${post.authorUsername}`) ||
        t('post')
      }
      className={[
        // `isolate` confines the card's internal z-10/z-20 layers (gradient, badges,
        // checkbox) to its own stacking context, so they can't paint over the
        // gallery's sticky filter bar / dropdown panel above the grid.
        // No lift/shadow and no resting hairline: the hover treatment is a pure
        // INTERNAL media zoom (see `u-media-zoom`) + the gradient overlay fading in,
        // so a card never casts the stray pale halo the old ring+lift produced.
        'group relative isolate aspect-square overflow-hidden rounded-sm u-clip-aa cursor-pointer outline-none',
        // Only the selected state draws a ring (accent); at rest the card edge is
        // defined by its own fill against the darker grid, with no hairline border.
        selected ? 'ring-2 ring-[#7B5CFF] ring-inset' : '',
      ].join(' ')}
      style={{ backgroundColor: '#1a1a1a' }}
      onClick={handleClick}
      onKeyDown={handleKeyDown}
      onMouseEnter={handleEnter}
      onMouseLeave={handleLeave}
      onPointerDown={handlePointerDown}
      onPointerMove={longPress.onPointerMove}
      onPointerUp={longPress.onPointerUp}
      onPointerCancel={longPress.onPointerCancel}
      onContextMenu={handleContextMenu}
      // Keyboard users reach the hover chrome too: focusing the card (or any
      // child) mounts the lazy overlay / quick-select so they're reachable.
      onFocus={() => setEverHovered(true)}
    >
      {/* Media slot: typographic card, image / slideshow frame, or an informative
        per-platform fallback (never a mute gray box). */}
      {isTextCard ? (
        <TextCard post={post} selectable={selectable} t={t} />
      ) : imageShowable ? (
        <>
          {/* Blur-up placeholder: paints in the same frame the card mounts (data
            URI — no fetch), so a cold tile reads as a soft preview of the artwork
            instead of a black square while the real thumbnail loads/generates. */}
          {post.thumbBlur && !imageSettled && (
            <img
              data-testid="blur-placeholder"
              src={post.thumbBlur}
              alt=""
              aria-hidden="true"
              draggable={false}
              className={`absolute inset-0 w-full h-full object-cover ${isWeb ? 'object-top' : ''}`}
              // scale hides the blur's transparent edge halo inside the crop.
              style={{ filter: 'blur(12px)', transform: 'scale(1.08)' }}
            />
          )}
          <img
            ref={handleImageRef}
            // Stable hook for the perf harness (e2e/perf-gallery.spec.ts): marks
            // THE cover image so its load/decode can be timed without mistaking a
            // favicon (web domain chip / fallback) for the thumbnail. Inert in prod.
            data-testid="card-image"
            src={displayedImage ?? undefined}
            alt={post.text || (isWeb ? post.webDomain : post.authorUsername) || ''}
            // Eager on purpose: the virtualizer already windows which cards exist,
            // and mounts overscan rows precisely so their media is ready before
            // they scroll into view. `loading="lazy"` would defer those fetches
            // until near the viewport, defeating the pre-loading.
            loading="eager"
            decoding="async"
            fetchpriority={priority ? 'high' : 'auto'}
            // Cover image FILLS the square (object-cover) and overscans 3px past every
            // edge via MEDIA_OVERSCAN, so the rounded clip cuts image interior, never the
            // element's antialiased/composited border → no pale seam at rest, mid-zoom, or
            // settled. Painted above the blur (later in DOM order). Web shots anchor to top.
            className={`absolute object-cover u-media-zoom ${isWeb ? 'object-top' : ''} ${
              touchPreviewing ? 'scale-105' : ''
            } ${
              imageLoaded ? (selectable && !selected ? 'opacity-80' : 'opacity-100') : 'opacity-0'
            }`}
            style={MEDIA_OVERSCAN}
            draggable={false}
            onLoad={() => setImageLoaded(true)}
            onTransitionEnd={() => setImageSettled(true)}
            // A 404 / blocked remote thumbnail or a moved/deleted local asset falls
            // back to the informative block instead of the browser broken-image glyph.
            onError={() => {
              setImageFailed(true);
              // The desktop re-fetches the preview into its local cache.
              if (capabilities.localFiles && !isWeb && !isManual && post.thumbnailUrl) {
                void window.electronAPI?.repairPreview?.(post.id)?.catch(() => {});
              }
            }}
          />
        </>
      ) : isWeb ? (
        <WebFallback post={post} t={t} />
      ) : isManual ? (
        <ManualFallback post={post} t={t} />
      ) : (
        <SocialFallback post={post} t={t} />
      )}

      {/* Local video preview: lazily loaded and played only while hovering */}
      {everHovered && localVideoSrc && (
        <video
          ref={videoRef}
          src={hovering ? localVideoSrc : undefined}
          muted
          loop
          playsInline
          preload="none"
          onPlaying={() => setVideoReady(true)}
          className={`absolute object-cover u-media-zoom ${
            hovering && videoReady ? 'opacity-100' : 'opacity-0'
          }`}
          style={MEDIA_OVERSCAN}
          draggable={false}
        />
      )}

      {/* Selection checkbox — presentational (the parent card owns the click), but
        carries checkbox semantics so assistive tech announces the toggle state. */}
      {selectable && (
        <div
          data-testid="select-checkbox"
          role="checkbox"
          aria-checked={selected}
          aria-label={selected ? t('deselectPost') : t('selectPost')}
          className={[
            'absolute top-1.5 left-1.5 z-20 flex items-center justify-center w-5 h-5 rounded-md border u-press u-fade-in u-scale-in',
            selected ? 'bg-[#7B5CFF] border-[#7B5CFF]' : 'bg-black/50 border-white/60',
          ].join(' ')}
        >
          {selected && <Check size={13} className="text-white u-pop-in" strokeWidth={3} />}
        </div>
      )}

      {/* Quick-select checkbox — only outside select mode, revealed on hover (or
        keyboard focus); same dress as the select-mode checkbox but interactive,
        so one click flips the surface into selection without the toolbar. */}
      {everHovered && !selectable && typeof onQuickSelect === 'function' && (
        <div
          data-testid="quick-select-checkbox"
          role="checkbox"
          aria-checked={false}
          aria-label={t('selectPost')}
          title={t('selectPost')}
          tabIndex={0}
          onClick={handleQuickSelectClick}
          onKeyDown={handleQuickSelectKeyDown}
          // Swallow mousedown so the grid's drag-select machinery never sees a
          // press that starts on the checkbox.
          onMouseDown={(e) => e.stopPropagation()}
          className={[
            'absolute top-1.5 left-1.5 z-20 flex items-center justify-center w-5 h-5 rounded-md border bg-black/50 border-white/60 opacity-0 group-hover:opacity-100 focus-visible:opacity-100 transition-opacity u-transition u-press',
            touchPreviewing ? '!opacity-100' : '',
          ].join(' ')}
        />
      )}

      {/* Award badge (web only) — top-right, won't collide with the top-left checkbox */}
      {awards.length > 0 && (
        <div
          data-testid="web-award-badge"
          className="absolute top-1.5 right-1.5 z-20 flex items-center gap-0.5 rounded-full bg-amber-400/90 text-black text-[9px] font-semibold px-1.5 py-0.5 u-pop-in"
          title={t('awards')}
        >
          <Award size={9} strokeWidth={2.5} />
          {awards[0]?.level || (awards.length > 1 ? awards.length : null)}
        </div>
      )}

      {/* Hover overlay: gradiente più profondo + info autore (slide-up leggero).
        Montato solo dopo il primo hover/focus (everHovered): a riposo è ~metà dei
        nodi DOM della card e quasi tutti gli SVG lucide — tenerlo fuori dal mount
        path è ciò che alleggerisce le righe rivelate durante lo scroll. */}
      {everHovered && !isTextCard && (
        <div
          data-testid="post-card-overlay"
          className={[
            'absolute inset-0 opacity-0 group-hover:opacity-100 transition-opacity u-transition z-10 flex flex-col justify-end',
            touchPreviewing ? '!opacity-100' : '',
          ].join(' ')}
        >
          {/* Darkening gradient, deliberately LARGER than the card (-inset-2) so it
            still covers the hover-zoomed image (scale 1.05) right up to the edges:
            pushing the layer's own antialiased edge OUTSIDE the card removes the ~1px
            unscrimmed seam the edge-to-edge overlay left against the scaled image's
            compositing layer. The card's overflow-hidden clips the overflow. Kept
            separate from the text so the copy below isn't scaled with it. */}
          <div
            aria-hidden="true"
            className="pointer-events-none absolute -inset-2"
            style={{
              background:
                'linear-gradient(to top, rgba(0,0,0,0.94) 0%, rgba(0,0,0,0.72) 15%, rgba(0,0,0,0.46) 30%, rgba(0,0,0,0.20) 50%, rgba(0,0,0,0.05) 70%, transparent 100%)',
            }}
          />
          <div
            className={[
              'relative px-2 pb-8 space-y-1 translate-y-1 group-hover:translate-y-0 transition-transform u-transition',
              touchPreviewing ? '!translate-y-0' : '',
            ].join(' ')}
          >
            <p className="text-white text-xs font-bold leading-tight truncate font-display">
              {isWeb
                ? post.webDomain || post.authorName || t('website')
                : isManual
                  ? post.userNote || t('manualBookmark')
                  : `@${post.authorUsername || t('unknownAuthor')}`}
            </p>
            {/* Palette swatches (web only) — presentational; copy lives in the modal */}
            {swatches.length > 0 && (
              <div className="flex items-center gap-1" data-testid="web-palette-swatches">
                {swatches.map((hex, i) => (
                  <span
                    key={`${hex}-${i}`}
                    className="w-2.5 h-2.5 rounded-[2px] ring-1 ring-white/15"
                    style={{ backgroundColor: hex }}
                  />
                ))}
              </div>
            )}
            {/* Content recall: tag micro-chips, or the caption's first line */}
            {hoverTags.length > 0 ? (
              <div className="flex items-center gap-1 overflow-hidden" data-testid="hover-tags">
                {hoverTags.map((tag, i) => (
                  <span
                    key={`${tag}-${i}`}
                    className="bg-white/10 text-white/85 text-[10px] leading-tight rounded-full px-1.5 py-px truncate max-w-[90px]"
                  >
                    {tag}
                  </span>
                ))}
              </div>
            ) : firstTextLine ? (
              <p className="text-white/60 text-[10px] leading-tight truncate">{firstTextLine}</p>
            ) : null}
            {/* Timestamp + status icons (AI / offline) share the overlay's last row */}
            <div className="flex items-center justify-between gap-2">
              <p className="text-white/45 text-[10px] leading-tight truncate">
                {post.timestamp ? formatTimestamp(post.timestamp, localeTag(lang)) : ''}
              </p>
              <div className="flex items-center gap-1.5 shrink-0">
                {hasAiAnalysis && (
                  <Sparkles size={11} className="text-[#b9a6ff]" title={t('aiGenerated')} />
                )}
                <OfflineIcon isDownloaded={isDownloaded} t={t} />
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Bottom identity row — compact rest-state chips, legible on any artwork:
        platform / domain on the left, media type (+ count) on the right.
        NB: NO `backdrop-blur` here. These chips render at rest on every card, so
        under the compositor scroll path (real wheel/trackpad) a backdrop-filter
        forces the backdrop to be re-sampled+blurred every frame for ~200 regions
        at once — it pinned native scrolling at ~25ms/frame (~40fps). A slightly
        more opaque solid (`bg-black/65`) keeps the chips legible at ~95-110fps.
        See VirtualPostGrid + e2e/perf-gallery.spec.ts. */}
      {!isTextCard && (
        <div className="absolute bottom-1.5 left-1.5 right-1.5 flex items-center justify-between gap-2 z-20 u-fade-in">
          <div
            data-testid="platform-chip"
            className="flex items-center rounded bg-black/65 px-1.5 py-0.5 min-w-0"
          >
            {isWeb ? (
              <WebDomainBadge domain={post.webDomain} faviconPath={post.webFaviconPath} />
            ) : (
              <PlatformIcon platform={post.platform} />
            )}
          </div>
          <div
            data-testid="mediatype-chip"
            className="flex items-center rounded bg-black/65 px-1.5 py-0.5 shrink-0"
          >
            <MediaTypeIcon mediaType={post.mediaType} mediaCount={post.mediaCount} />
          </div>
        </div>
      )}
    </div>
  );
}

export default React.memo(PostCard);
