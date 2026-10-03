import React, { useEffect, useRef, useState } from 'react';
import { ChevronLeft, ChevronRight, ExternalLink, Globe, Quote, RotateCw } from 'lucide-react';
import { useT } from '../../i18n';
import { useShelfy } from '../../api/ShelfyProvider';
import { useReducedMotion } from '../../hooks/useReducedMotion';
import { NARROW_QUERY, useMediaQuery } from '../ui/useMediaQuery';
import { isHttpUrl, getVideoMutedPref, setVideoMutedPref, webPageLabel } from './helpers';
import type { PostSlide, SlideMedia } from './helpers';

// A platform glyph: a lucide icon or the hand-rolled PinterestIcon. `ElementType`
// (not a precise props shape) so both assign to it despite lucide and Pinterest
// typing `size` differently; the fallback only ever passes size/className/style.
type PlatformIcon = React.ElementType;

interface MediaFallbackProps {
  PlatformIcon: PlatformIcon;
  accent: string;
  url: string | null;
  // Present only when the fallback stands in for a media file that failed to
  // decode (an expired CDN URL, a missing object): offer a retry. Absent for a
  // post that simply has no media to show.
  onRetry?: () => void;
}

// Shown in the media pane when there is nothing to render (MOD-2, MOD-3,
// finding 6): a failed/missing image, or a web reference without the live
// webview. The post's own platform glyph (not a generic globe), a short label,
// [Open original] and — for a load failure — [Retry]. Replaces the browser's
// broken-image icon with the caption spilled over it as alt text.
function MediaFallback({
  PlatformIcon,
  accent,
  url,
  onRetry,
}: MediaFallbackProps): React.JSX.Element {
  const t = useT('postModal');
  const client = useShelfy();
  return (
    <div
      data-testid="post-modal-no-media"
      className="u-fade-in flex h-full min-h-[40vh] flex-col items-center justify-center gap-4 px-6 text-center"
    >
      <PlatformIcon size={36} style={{ color: accent }} />
      <p className="text-sm text-muted">{t('mediaUnavailable')}</p>
      <div className="flex flex-wrap items-center justify-center gap-2">
        {onRetry && (
          <button
            type="button"
            data-testid="post-modal-media-retry"
            onClick={(e) => {
              e.stopPropagation();
              onRetry();
            }}
            className="u-press inline-flex h-8 narrow:h-11 items-center gap-1.5 rounded-md border border-strong px-3 text-xs text-primary hover:bg-hover"
          >
            <RotateCw size={14} /> {t('retryMedia')}
          </button>
        )}
        {isHttpUrl(url) && (
          <button
            type="button"
            data-testid="post-modal-open-original"
            onClick={(e) => {
              e.stopPropagation();
              client.openExternal(url);
            }}
            className="u-press inline-flex h-8 narrow:h-11 items-center gap-1.5 rounded-md bg-elevated px-3 text-xs font-medium text-primary hover:bg-hover"
          >
            <ExternalLink size={14} />
            {t('openOriginal')}
          </button>
        )}
      </div>
    </div>
  );
}

interface TextCardProps {
  text: string;
}

// A text-only post (a tweet without media) used to leave the media pane empty
// but for a globe and "Open original" (MOD-3). Instead, render the post's own
// words as the hero: a quote mark and the text at a comfortable reading size.
// The caption is suppressed in MetaColumn then, so it shows once, here.
function TextCard({ text }: TextCardProps): React.JSX.Element {
  return (
    <div
      data-testid="post-modal-text"
      className="u-fade-in flex h-full min-h-[40vh] flex-col items-center justify-center overflow-y-auto px-6 py-8 scrollbar-thin scrollbar-thumb-[#2e2e2e]"
    >
      <div className="mx-auto max-w-[640px]">
        <Quote size={28} className="mb-3 text-[#4a4a4a]" aria-hidden="true" />
        <p className="whitespace-pre-wrap break-words text-xl leading-relaxed text-[#ececec]">
          {text}
        </p>
      </div>
    </div>
  );
}

interface MediaCarouselProps {
  post: Shelfy.Post;
  isWeb: boolean;
  isTextOnly: boolean;
  media: SlideMedia;
  current: PostSlide | undefined;
  slides: PostSlide[];
  clampedSlide: number;
  slideCount: number;
  hasMultiple: boolean;
  // The post's platform glyph and accent, for the media fallback (MOD-2).
  PlatformIcon: PlatformIcon;
  accent: string;
  // The original page URL (for [Open original] in the fallback).
  postUrl: string | null;
  onSlidePrev: () => void;
  onSlideNext: () => void;
  onSelectSlide: (index: number) => void;
  onOpenLightbox: () => void;
  onOpenLightboxKey: (e: React.KeyboardEvent<HTMLImageElement>) => void;
  // Reports whether the media currently shown failed to load, so the shell can
  // disable the download/open actions that would fail the same way (MOD-13).
  onMediaFailedChange?: (failed: boolean) => void;
  // A horizontal swipe on the media (narrow only): step slide-first, then post
  // at the edges — the same order as the keyboard arrows (MOD-5).
  onSwipeNavigate: (dir: 'prev' | 'next') => void;
}

// How far a touch must travel horizontally (and how much more than vertically)
// to count as a swipe rather than a tap or a vertical scroll.
const SWIPE_PX = 50;

// LEFT column of the modal: the post media (or web screenshot) plus the
// within-post slide navigation (arrows / counter / dots / web page chip).
// The shell owns the slide index (the modal-level keyboard handler steps it);
// this component owns the video element + persisted mute preference, the
// per-slide media-failure state, and the horizontal swipe between posts.
export default function MediaCarousel({
  post,
  isWeb,
  isTextOnly,
  media,
  current,
  slides,
  clampedSlide,
  slideCount,
  hasMultiple,
  PlatformIcon,
  accent,
  postUrl,
  onSlidePrev,
  onSlideNext,
  onSelectSlide,
  onOpenLightbox,
  onOpenLightboxKey,
  onMediaFailedChange,
  onSwipeNavigate,
}: MediaCarouselProps) {
  const t = useT('postModal');
  const { webviewFallback } = useShelfy().capabilities;
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const reducedMotion = useReducedMotion();
  const narrow = useMediaQuery(NARROW_QUERY);

  // A media file that failed to decode (expired URL, missing object): show the
  // same fallback a post without media shows, with a retry. Keyed on the src, so
  // navigating to another slide/post clears it; `reloadNonce` forces the <img> to
  // re-request on retry even when the URL is unchanged.
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const [reloadNonce, setReloadNonce] = useState(0);
  const retry = (): void => {
    setFailedSrc(null);
    setReloadNonce((n) => n + 1);
  };
  const didFail = media.src != null && failedSrc === media.src;

  // Tell the shell whether the current media failed (drives MOD-13).
  useEffect(() => {
    onMediaFailedChange?.(didFail);
  }, [didFail, onMediaFailedChange]);

  // Apply the persisted mute preference once the video element is mounted.
  useEffect(() => {
    if (media.kind === 'video' && videoRef.current) {
      videoRef.current.muted = getVideoMutedPref();
    }
  }, [media.kind, media.src]);

  // Short alt text: the author (or domain) and nothing more — never the whole
  // caption, which used to spill over a broken image as its alt (MOD-2).
  const mediaAlt = isWeb
    ? post.webDomain || post.authorName || ''
    : post.authorName || (post.authorUsername ? `@${post.authorUsername}` : '');

  // Horizontal swipe → navigate (narrow only). `touch-action: pan-y` keeps
  // vertical scrolling of a tall web screenshot working.
  const touchStart = useRef<{ x: number; y: number } | null>(null);
  const onTouchStart = (e: React.TouchEvent): void => {
    if (!narrow || e.touches.length !== 1) {
      touchStart.current = null;
      return;
    }
    touchStart.current = { x: e.touches[0].clientX, y: e.touches[0].clientY };
  };
  const onTouchEnd = (e: React.TouchEvent): void => {
    const start = touchStart.current;
    touchStart.current = null;
    if (!start) return;
    const touch = e.changedTouches[0];
    const dx = touch.clientX - start.x;
    const dy = touch.clientY - start.y;
    if (Math.abs(dx) < SWIPE_PX || Math.abs(dx) < Math.abs(dy) * 1.5) return;
    onSwipeNavigate(dx < 0 ? 'next' : 'prev');
  };
  const swipeProps = {
    onTouchStart,
    onTouchEnd,
    style: narrow ? ({ touchAction: 'pan-y' } as React.CSSProperties) : undefined,
  };

  const video = (extra: string): React.JSX.Element => (
    <video
      key={`video-${clampedSlide}`}
      ref={videoRef}
      data-testid="post-modal-video"
      src={media.src ?? undefined}
      controls
      muted
      playsInline
      // Autoplay only without a reduced-motion request (MOD-7); always muted so
      // a post never plays sound on open.
      autoPlay={!reducedMotion}
      onVolumeChange={(e) => setVideoMutedPref(e.currentTarget.muted)}
      className={extra}
    />
  );

  if (isWeb) {
    /* ── Web reference: scrollable full-width screenshot ──────────────────── */
    return (
      <div
        data-testid="post-modal-media"
        className="relative flex-1 min-w-0 bg-[#0f0f0f] overflow-hidden"
        {...swipeProps}
      >
        {/* The current page screenshot, shown full-width and scrolled
            vertically like a real website rather than fit-to-frame. */}
        <div
          key={`scroll-${clampedSlide}`}
          data-testid="post-modal-web-scroll"
          className="absolute inset-0 overflow-y-auto overflow-x-hidden scrollbar-thin scrollbar-thumb-[#2e2e2e]"
        >
          {media.kind === 'image' && !didFail ? (
            <img
              key={`img-${clampedSlide}-${reloadNonce}`}
              data-testid="post-modal-image"
              src={media.src ?? undefined}
              alt={mediaAlt}
              onClick={onOpenLightbox}
              onKeyDown={onOpenLightboxKey}
              onError={() => setFailedSrc(media.src)}
              role="button"
              tabIndex={0}
              aria-label={t('zoomFullscreen')}
              title={t('clickToZoomFullscreen')}
              className="w-full h-auto block cursor-zoom-in focus:outline-none"
              draggable={false}
            />
          ) : media.kind === 'video' && !didFail ? (
            <div className="min-h-full flex items-center justify-center">
              {video('u-fade-in max-w-full')}
            </div>
          ) : didFail ? (
            <MediaFallback
              PlatformIcon={PlatformIcon}
              accent={accent}
              url={postUrl}
              onRetry={retry}
            />
          ) : !webviewFallback ? (
            <MediaFallback PlatformIcon={PlatformIcon} accent={accent} url={media.src} />
          ) : isHttpUrl(media.src) ? (
            <webview
              key={`webview-${clampedSlide}`}
              src={media.src}
              partition="persist:social"
              className="u-fade-in"
              style={{ width: '100%', height: '100%' }}
            />
          ) : (
            <MediaFallback PlatformIcon={PlatformIcon} accent={accent} url={postUrl} />
          )}
        </div>

        {/* Page chip — which page this slide is (item: chip per slide). */}
        <div
          data-testid="post-modal-page-chip"
          className="absolute top-3 left-3 z-raised flex items-center gap-1.5 px-2.5 py-1 rounded-full bg-black/65 backdrop-blur text-white text-caption font-medium max-w-[70%]"
        >
          <Globe size={12} className="shrink-0 text-[#b9a6ff]" />
          <span className="truncate">
            {webPageLabel(current?.url, post.webFinalUrl || post.postUrl, clampedSlide, t)}
          </span>
        </div>

        {hasMultiple && (
          <>
            {/* Always present (disabled at the edges) so a click can never
                fall through to the screenshot behind; no scale punch. */}
            <button
              data-testid="post-modal-slide-prev"
              onClick={(e) => {
                e.stopPropagation();
                onSlidePrev();
              }}
              disabled={clampedSlide === 0}
              aria-label={t('prevPage')}
              title={t('prevPage')}
              className="absolute left-2 top-1/2 -translate-y-1/2 z-raised flex items-center justify-center w-9 h-9 narrow:w-11 narrow:h-11 rounded-full bg-black/55 text-white/80 transition-colors hover:bg-black/75 hover:text-white disabled:opacity-25 disabled:cursor-not-allowed disabled:hover:bg-black/55"
            >
              <ChevronLeft size={20} />
            </button>
            <button
              data-testid="post-modal-slide-next"
              onClick={(e) => {
                e.stopPropagation();
                onSlideNext();
              }}
              disabled={clampedSlide === slideCount - 1}
              aria-label={t('nextPage')}
              title={t('nextPage')}
              className="absolute right-2 top-1/2 -translate-y-1/2 z-raised flex items-center justify-center w-9 h-9 narrow:w-11 narrow:h-11 rounded-full bg-black/55 text-white/80 transition-colors hover:bg-black/75 hover:text-white disabled:opacity-25 disabled:cursor-not-allowed disabled:hover:bg-black/55"
            >
              <ChevronRight size={20} />
            </button>

            <div
              data-testid="post-modal-slide-counter"
              className="absolute top-3 right-3 z-raised px-2 py-0.5 rounded-full bg-black/65 text-white text-caption font-medium tabular-nums"
            >
              {clampedSlide + 1}/{slideCount}
            </div>

            <div className="absolute bottom-2 left-1/2 -translate-x-1/2 z-raised flex items-center gap-1.5">
              {slides.map((_, i) => (
                <button
                  key={i}
                  onClick={(e) => {
                    e.stopPropagation();
                    onSelectSlide(i);
                  }}
                  aria-label={t('goToPage', { n: i + 1 })}
                  aria-current={i === clampedSlide ? 'true' : undefined}
                  title={t('goToPage', { n: i + 1 })}
                  className={
                    'h-1.5 rounded-full transition-all ' +
                    (i === clampedSlide ? 'w-4 bg-white' : 'w-1.5 bg-white/40 hover:bg-white/70')
                  }
                />
              ))}
            </div>
          </>
        )}
      </div>
    );
  }

  /* ── Social post: media centered and fit-to-frame ─────────────────────────── */
  return (
    <div
      data-testid="post-modal-media"
      className="relative flex-1 min-w-0 bg-[#0f0f0f] flex items-center justify-center overflow-hidden"
      {...swipeProps}
    >
      {isTextOnly ? (
        <TextCard text={post.text || ''} />
      ) : media.kind === 'video' && !didFail ? (
        video('u-fade-in max-w-full max-h-full')
      ) : media.kind === 'image' && !didFail ? (
        <img
          key={`img-${clampedSlide}-${reloadNonce}`}
          data-testid="post-modal-image"
          src={media.src ?? undefined}
          alt={mediaAlt}
          onClick={onOpenLightbox}
          onKeyDown={onOpenLightboxKey}
          onError={() => setFailedSrc(media.src)}
          role="button"
          tabIndex={0}
          aria-label={t('zoom')}
          title={t('clickToZoom')}
          className="u-fade-in max-w-full max-h-full object-contain cursor-zoom-in focus:outline-none"
        />
      ) : didFail ? (
        <MediaFallback PlatformIcon={PlatformIcon} accent={accent} url={postUrl} onRetry={retry} />
      ) : media.kind === 'webview' && webviewFallback && isHttpUrl(media.src) ? (
        <webview
          key={`webview-${clampedSlide}`}
          src={media.src}
          partition="persist:social"
          className="u-fade-in"
          style={{ width: '100%', height: '100%' }}
        />
      ) : (
        <MediaFallback PlatformIcon={PlatformIcon} accent={accent} url={postUrl} />
      )}

      {/* Slide navigation — within the post's own media */}
      {hasMultiple && !isTextOnly && (
        <>
          {clampedSlide > 0 && (
            <button
              data-testid="post-modal-slide-prev"
              onClick={onSlidePrev}
              aria-label={t('prevImage')}
              title={t('prevImage')}
              className="u-press absolute left-2 top-1/2 -translate-y-1/2 z-raised flex items-center justify-center w-9 h-9 narrow:w-11 narrow:h-11 rounded-full bg-black/50 text-white/80 hover:bg-black/70 hover:text-white"
            >
              <ChevronLeft size={20} />
            </button>
          )}
          {clampedSlide < slideCount - 1 && (
            <button
              data-testid="post-modal-slide-next"
              onClick={onSlideNext}
              aria-label={t('nextImage')}
              title={t('nextImage')}
              className="u-press absolute right-2 top-1/2 -translate-y-1/2 z-raised flex items-center justify-center w-9 h-9 narrow:w-11 narrow:h-11 rounded-full bg-black/50 text-white/80 hover:bg-black/70 hover:text-white"
            >
              <ChevronRight size={20} />
            </button>
          )}

          {/* Counter */}
          <div
            key={`counter-${clampedSlide}`}
            data-testid="post-modal-slide-counter"
            className="u-scale-in absolute top-2 right-2 z-raised px-2 py-0.5 rounded-full bg-black/60 text-white text-caption font-medium"
          >
            {clampedSlide + 1}/{slideCount}
          </div>

          {/* Dot indicators */}
          <div className="absolute bottom-2 left-1/2 -translate-x-1/2 z-raised flex items-center gap-1.5">
            {slides.map((_, i) => (
              <button
                key={i}
                onClick={() => onSelectSlide(i)}
                aria-label={t('goToImage', { n: i + 1 })}
                aria-current={i === clampedSlide ? 'true' : undefined}
                title={t('goToImage', { n: i + 1 })}
                className={
                  'u-press h-1.5 rounded-full transition-all ' +
                  (i === clampedSlide ? 'w-4 bg-white' : 'w-1.5 bg-white/40 hover:bg-white/70')
                }
              />
            ))}
          </div>
        </>
      )}
    </div>
  );
}
