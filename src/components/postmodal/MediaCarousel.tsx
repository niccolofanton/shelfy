import React, { useEffect, useRef } from 'react';
import { ChevronLeft, ChevronRight, Globe } from 'lucide-react';
import { useT } from '../../i18n';
import { isHttpUrl, getVideoMutedPref, setVideoMutedPref, webPageLabel } from './helpers';
import type { PostSlide, SlideMedia } from './helpers';

interface MediaCarouselProps {
  post: Shelfy.Post;
  isWeb: boolean;
  media: SlideMedia;
  current: PostSlide | undefined;
  slides: PostSlide[];
  clampedSlide: number;
  slideCount: number;
  hasMultiple: boolean;
  onSlidePrev: () => void;
  onSlideNext: () => void;
  onSelectSlide: (index: number) => void;
  onOpenLightbox: () => void;
  onOpenLightboxKey: (e: React.KeyboardEvent<HTMLImageElement>) => void;
}

// LEFT column of the modal: the post media (or web screenshot) plus the
// within-post slide navigation (arrows / counter / dots / web page chip).
// The shell owns the slide index (the modal-level keyboard handler steps it);
// this component owns only the video element + persisted mute preference.
export default function MediaCarousel({
  post,
  isWeb,
  media,
  current,
  slides,
  clampedSlide,
  slideCount,
  hasMultiple,
  onSlidePrev,
  onSlideNext,
  onSelectSlide,
  onOpenLightbox,
  onOpenLightboxKey,
}: MediaCarouselProps) {
  const t = useT('postModal');
  const videoRef = useRef<HTMLVideoElement | null>(null);

  // Apply the persisted mute preference once the video element is mounted.
  useEffect(() => {
    if (media.kind === 'video' && videoRef.current) {
      videoRef.current.muted = getVideoMutedPref();
    }
  }, [media.kind, media.src]);

  if (isWeb) {
    /* ── Web reference: scrollable full-width screenshot ──────────────────── */
    return (
      <div className="relative flex-1 min-w-0 bg-[#0f0f0f] overflow-hidden">
        {/* The current page screenshot, shown full-width and scrolled
            vertically like a real website rather than fit-to-frame. */}
        <div
          key={`scroll-${clampedSlide}`}
          data-testid="post-modal-web-scroll"
          className="absolute inset-0 overflow-y-auto overflow-x-hidden scrollbar-thin scrollbar-thumb-[#2e2e2e]"
        >
          {media.kind === 'image' ? (
            <img
              key={`img-${clampedSlide}`}
              data-testid="post-modal-image"
              src={media.src ?? undefined}
              alt={post.text || post.webDomain || ''}
              onClick={onOpenLightbox}
              onKeyDown={onOpenLightboxKey}
              role="button"
              tabIndex={0}
              aria-label={t('zoomFullscreen')}
              title={t('clickToZoomFullscreen')}
              className="w-full h-auto block cursor-zoom-in focus:outline-none focus-visible:ring-2 focus-visible:ring-violet-500/60"
              draggable={false}
            />
          ) : media.kind === 'video' ? (
            <div className="min-h-full flex items-center justify-center">
              <video
                key={`video-${clampedSlide}`}
                ref={videoRef}
                data-testid="post-modal-video"
                src={media.src ?? undefined}
                controls
                autoPlay
                onVolumeChange={(e) => setVideoMutedPref(e.currentTarget.muted)}
                className="u-fade-in max-w-full"
              />
            </div>
          ) : isHttpUrl(media.src) ? (
            <webview
              key={`webview-${clampedSlide}`}
              src={media.src}
              partition="persist:social"
              className="u-fade-in"
              style={{ width: '100%', height: '100%' }}
            />
          ) : (
            <div className="flex items-center justify-center h-full min-h-[40vh] text-gray-600">
              <Globe size={32} />
            </div>
          )}
        </div>

        {/* Page chip — which page this slide is (item: chip per slide). */}
        <div
          data-testid="post-modal-page-chip"
          className="absolute top-3 left-3 z-10 flex items-center gap-1.5 px-2.5 py-1 rounded-full bg-black/65 backdrop-blur text-white text-[11px] font-medium max-w-[70%]"
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
              title={t('prevPage')}
              className="absolute left-2 top-1/2 -translate-y-1/2 z-20 flex items-center justify-center w-9 h-9 rounded-full bg-black/55 text-white/80 transition-colors hover:bg-black/75 hover:text-white disabled:opacity-25 disabled:cursor-not-allowed disabled:hover:bg-black/55"
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
              title={t('nextPage')}
              className="absolute right-2 top-1/2 -translate-y-1/2 z-20 flex items-center justify-center w-9 h-9 rounded-full bg-black/55 text-white/80 transition-colors hover:bg-black/75 hover:text-white disabled:opacity-25 disabled:cursor-not-allowed disabled:hover:bg-black/55"
            >
              <ChevronRight size={20} />
            </button>

            <div
              data-testid="post-modal-slide-counter"
              className="absolute top-3 right-3 z-10 px-2 py-0.5 rounded-full bg-black/65 text-white text-[11px] font-medium tabular-nums"
            >
              {clampedSlide + 1}/{slideCount}
            </div>

            <div className="absolute bottom-2 left-1/2 -translate-x-1/2 z-20 flex items-center gap-1.5">
              {slides.map((_, i) => (
                <button
                  key={i}
                  onClick={(e) => {
                    e.stopPropagation();
                    onSelectSlide(i);
                  }}
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
    <div className="relative flex-1 min-w-0 bg-[#0f0f0f] flex items-center justify-center overflow-hidden">
      {media.kind === 'video' && (
        <video
          key={`video-${clampedSlide}`}
          ref={videoRef}
          data-testid="post-modal-video"
          src={media.src ?? undefined}
          controls
          autoPlay
          onVolumeChange={(e) => setVideoMutedPref(e.currentTarget.muted)}
          className="u-fade-in max-w-full max-h-full"
        />
      )}
      {media.kind === 'image' && (
        <img
          key={`img-${clampedSlide}`}
          data-testid="post-modal-image"
          src={media.src ?? undefined}
          alt={post.text || post.authorUsername || ''}
          onClick={onOpenLightbox}
          onKeyDown={onOpenLightboxKey}
          role="button"
          tabIndex={0}
          aria-label={t('zoom')}
          title={t('clickToZoom')}
          className="u-fade-in max-w-full max-h-full object-contain cursor-zoom-in focus:outline-none focus-visible:ring-2 focus-visible:ring-violet-500/60"
        />
      )}
      {media.kind === 'webview' &&
        (isHttpUrl(media.src) ? (
          <webview
            key={`webview-${clampedSlide}`}
            src={media.src}
            partition="persist:social"
            className="u-fade-in"
            style={{ width: '100%', height: '100%' }}
          />
        ) : (
          <Globe size={32} className="text-gray-600" />
        ))}

      {/* Slide navigation — within the post's own media */}
      {hasMultiple && (
        <>
          {clampedSlide > 0 && (
            <button
              data-testid="post-modal-slide-prev"
              onClick={onSlidePrev}
              title={t('prevImage')}
              className="u-press absolute left-2 top-1/2 -translate-y-1/2 z-10 flex items-center justify-center w-9 h-9 rounded-full bg-black/50 text-white/80 hover:bg-black/70 hover:text-white"
            >
              <ChevronLeft size={20} />
            </button>
          )}
          {clampedSlide < slideCount - 1 && (
            <button
              data-testid="post-modal-slide-next"
              onClick={onSlideNext}
              title={t('nextImage')}
              className="u-press absolute right-2 top-1/2 -translate-y-1/2 z-10 flex items-center justify-center w-9 h-9 rounded-full bg-black/50 text-white/80 hover:bg-black/70 hover:text-white"
            >
              <ChevronRight size={20} />
            </button>
          )}

          {/* Counter */}
          <div
            key={`counter-${clampedSlide}`}
            data-testid="post-modal-slide-counter"
            className="u-scale-in absolute top-2 right-2 z-10 px-2 py-0.5 rounded-full bg-black/60 text-white text-[11px] font-medium"
          >
            {clampedSlide + 1}/{slideCount}
          </div>

          {/* Dot indicators */}
          <div className="absolute bottom-2 left-1/2 -translate-x-1/2 z-10 flex items-center gap-1.5">
            {slides.map((_, i) => (
              <button
                key={i}
                onClick={() => onSelectSlide(i)}
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
