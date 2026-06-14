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
import { assetThumbUrl, assetUrl } from '../lib/asset';

// Target box for grid-tile images. Local files are often full-resolution
// originals (multi-MB); the asset protocol serves a cached fit-in-640px copy
// instead so a scroll-burst of tiles doesn't stall on huge decodes. The modal
// still loads the original. Keep in sync with main.js PREWARM_TILE_WIDTH.
const TILE_WIDTH = 640;
import SourceIcon from './SourceIcon';
import { useT, useLang, localeTag } from '../i18n';

// `title` is a valid global SVG attribute (native hover tooltip) that lucide
// spreads onto its <svg>, but React's SVGAttributes typings omit it. Declare it
// so the icon tooltips below (and elsewhere) stay typed without `any`.
declare module 'react' {
  interface SVGAttributes<T> {
    title?: string;
  }
}

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

// Site favicon with a lucide Globe fallback. Comes from the site's own
// /favicon.ico (the user already visited it at capture time — Google s2 would
// leak every saved domain to a third party); onError falls back to Globe.
// Shared between the rest-state domain chip and the no-screenshot fallback.
function Favicon({
  domain,
  size = 11,
}: {
  domain: string | null;
  size?: number;
}): React.JSX.Element {
  const [iconFailed, setIconFailed] = useState<boolean>(false);
  let faviconSrc: string | null = null;
  try {
    if (domain) faviconSrc = new URL('/favicon.ico', `https://${domain}`).href;
  } catch {
    /* malformed domain → Globe fallback below */
  }
  if (iconFailed || !faviconSrc) {
    return <Globe size={size} className="text-white/85 shrink-0" />;
  }
  return (
    <img
      src={faviconSrc}
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
function WebDomainBadge({ domain }: { domain: string | null }): React.JSX.Element {
  if (!domain) return <Globe size={11} className="text-white/85" />;
  return (
    <div className="flex items-center gap-1 min-w-0">
      <Favicon domain={domain} size={11} />
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

// Typographic body for text posts (or posts whose media is gone but whose text
// survives): a deliberate quote-card instead of a mute missing-image gray. The
// platform glyph sits in the corner as a low-opacity watermark.
function TextCard({ post }: { post: Shelfy.Post }): React.JSX.Element {
  return (
    <div
      data-testid="text-card"
      className="relative w-full h-full p-3 overflow-hidden"
      style={{ backgroundColor: '#161618' }}
    >
      <p className="text-[13px] text-gray-200 leading-snug line-clamp-6 break-words">{post.text}</p>
      <SourceIcon
        platform={post.platform}
        size={32}
        className="absolute bottom-2 right-2 text-white/[0.07] pointer-events-none"
        aria-hidden="true"
      />
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
      <Favicon domain={post.webDomain} size={22} />
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

// Social post with neither an image nor text: platform glyph + author handle.
function SocialFallback({ post, t }: { post: Shelfy.Post; t: Translate }): React.JSX.Element {
  return (
    <div
      data-testid="social-fallback"
      className="w-full h-full flex flex-col items-center justify-center gap-1.5 px-4"
      style={{ backgroundColor: '#161618' }}
    >
      <SourceIcon platform={post.platform} size={20} className="text-gray-500" />
      <p className="text-[11px] text-gray-400 truncate max-w-full">
        @{post.authorUsername || t('unknownAuthor')}
      </p>
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
function buildSlideshowImages(post: Shelfy.Post): string[] {
  if (!Array.isArray(post.media)) return [];
  return post.media
    .filter((m): m is Shelfy.PostMedia => !!m && m.type === 'image')
    .map((m) => (m.localPath ? assetThumbUrl(m.localPath, TILE_WIDTH) : m.url))
    .filter((src): src is string => Boolean(src));
}

const SLIDESHOW_INTERVAL_MS = 800;

interface PostCardProps {
  post: Shelfy.Post;
  onOpen: (post: Shelfy.Post, event?: React.SyntheticEvent) => void;
  selectable?: boolean;
  selected?: boolean;
  onQuickSelect?: (post: Shelfy.Post, event: React.SyntheticEvent) => void;
}

function PostCard({
  post,
  onOpen,
  selectable = false,
  selected = false,
  onQuickSelect,
}: PostCardProps): React.JSX.Element {
  const t = useT('postCard');
  const { lang } = useLang();
  const isWeb = post.platform === 'web';
  const isManual = post.platform === 'manual';
  const localImage = post.thumbnailPath || post.imagePath;
  const imageSrc = localImage ? assetThumbUrl(localImage, TILE_WIDTH) : post.thumbnailUrl || null;
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
  const localVideoSrc = post.videoPath ? assetUrl(post.videoPath) : null;
  // Depend on a stable key (id + media length) rather than the whole post object,
  // so a new post reference with unchanged media doesn't rebuild the array.
  const slideshowImages = useMemo<string[]>(
    () => buildSlideshowImages(post),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [post.id, Array.isArray(post.media) ? post.media.length : 0],
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
    setHovering(true);
    setEverHovered(true);
  }

  function handleLeave(): void {
    setHovering(false);
    setSlide(0);
    setVideoReady(false);
  }

  function handleClick(e: React.MouseEvent): void {
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

  // Text posts always get the typographic treatment; other social posts fall
  // back to it only when their media is unavailable but the caption survives
  // (web/manual have their own informative fallbacks below).
  const isTextCard =
    post.mediaType === 'text' || (!imageShowable && hasText && !isWeb && !isManual);

  // A new image source (post change, or hover-slideshow frame) clears a stale
  // failure flag so a later valid src isn't permanently hidden behind the fallback.
  useEffect(() => {
    setImageFailed(false);
  }, [displayedImage]);

  // Settle fallback: transitionend doesn't fire when transitions are disabled
  // (prefers-reduced-motion) or the event is missed — never leave the blurred
  // placeholder composited under an already-opaque image.
  useEffect(() => {
    if (!imageLoaded || imageSettled) return undefined;
    const t = setTimeout(() => setImageSettled(true), 450);
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
        'group relative isolate aspect-square overflow-hidden rounded-sm cursor-pointer u-lift outline-none focus-visible:ring-2 focus-visible:ring-[#7B5CFF] focus-visible:ring-inset',
        // A hairline ring defines the card edge against the grid at rest; the
        // accent ring replaces (not stacks on) it while selected.
        selected ? 'ring-2 ring-[#7B5CFF] ring-inset' : 'ring-1 ring-white/[0.06]',
      ].join(' ')}
      style={{ backgroundColor: '#1a1a1a' }}
      onClick={handleClick}
      onKeyDown={handleKeyDown}
      onMouseEnter={handleEnter}
      onMouseLeave={handleLeave}
      // Keyboard users reach the hover chrome too: focusing the card (or any
      // child) mounts the lazy overlay / quick-select so they're reachable.
      onFocus={() => setEverHovered(true)}
    >
      {/* Media slot: typographic card, image / slideshow frame, or an informative
        per-platform fallback (never a mute gray box). */}
      {isTextCard ? (
        <TextCard post={post} />
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
            // `relative` keeps this image painting ABOVE the absolutely-positioned
            // blur layer (static elements would paint below positioned siblings).
            // Full-page web screenshots are anchored to the top so the hero /
            // above-the-fold stays visible in the square crop.
            className={`relative ${isWeb ? 'object-top' : ''} object-cover w-full h-full u-transition ${
              imageLoaded ? (selectable && !selected ? 'opacity-80' : 'opacity-100') : 'opacity-0'
            }`}
            draggable={false}
            onLoad={() => setImageLoaded(true)}
            onTransitionEnd={() => setImageSettled(true)}
            // A 404 / blocked remote thumbnail or a moved/deleted local asset falls
            // back to the informative block instead of the browser broken-image glyph.
            onError={() => setImageFailed(true)}
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
          className={`absolute inset-0 object-cover w-full h-full transition-opacity u-transition ${
            hovering && videoReady ? 'opacity-100' : 'opacity-0'
          }`}
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
          className="absolute top-1.5 left-1.5 z-20 flex items-center justify-center w-5 h-5 rounded-md border bg-black/50 border-white/60 opacity-0 group-hover:opacity-100 focus-visible:opacity-100 transition-opacity u-transition u-press"
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
      {everHovered && (
        <div
          className="absolute inset-0 opacity-0 group-hover:opacity-100 transition-opacity u-transition z-10 flex flex-col justify-end"
          style={{
            background:
              'linear-gradient(to top, rgba(0,0,0,0.94) 0%, rgba(0,0,0,0.72) 15%, rgba(0,0,0,0.46) 30%, rgba(0,0,0,0.20) 50%, rgba(0,0,0,0.05) 70%, transparent 100%)',
          }}
        >
          <div className="px-2 pb-8 space-y-1 translate-y-1 group-hover:translate-y-0 transition-transform u-transition">
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
      <div className="absolute bottom-1.5 left-1.5 right-1.5 flex items-center justify-between gap-2 z-20 u-fade-in">
        <div
          data-testid="platform-chip"
          className="flex items-center rounded bg-black/65 px-1.5 py-0.5 min-w-0"
        >
          {isWeb ? (
            <WebDomainBadge domain={post.webDomain} />
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
    </div>
  );
}

export default React.memo(PostCard);
