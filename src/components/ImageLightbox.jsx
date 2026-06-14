import React, { useEffect, useRef, useState } from 'react';
import { X, ChevronLeft, ChevronRight, ExternalLink } from 'lucide-react';
import { useT } from '../i18n';

// Full-screen screenshot viewer used both in the Websites panel (review captures
// live) and the gallery PostModal. Full-page screenshots are tall, so the image
// is shown at viewport width and scrolls vertically; ←/→ switch page, Esc closes.
// `images`: [{ src, chunks?: string[], label?, href? }]. When `chunks` is present
// (a tall capture sliced into vertical bands) they're stacked and lazy-loaded, so
// the renderer never decodes one heavyweight full-page frame. Controlled index.
export default function ImageLightbox({ images = [], index = 0, onClose, onIndexChange }) {
  const t = useT('lightbox');
  const scrollRef = useRef(null);
  const containerRef = useRef(null);
  const list = Array.isArray(images) ? images.filter((im) => im && im.src) : [];
  const safeIndex = Math.max(0, Math.min(index, list.length - 1));
  const cur = list[safeIndex];
  const [failed, setFailed] = useState(false);

  const go = (delta) => {
    if (!list.length) return;
    const next = (safeIndex + delta + list.length) % list.length;
    onIndexChange?.(next);
  };

  useEffect(() => {
    setFailed(false);
    if (scrollRef.current) scrollRef.current.scrollTop = 0; // reset scroll per page
  }, [safeIndex]);

  useEffect(() => {
    const onKey = (e) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onClose?.();
      } else if (e.key === 'ArrowRight') {
        // Stop propagation (like the Escape branch) so the arrow consumed here
        // doesn't also reach PostModal's window-level handler and step the
        // background slide/post in parallel (the two indices would drift).
        e.preventDefault();
        e.stopPropagation();
        go(1);
      } else if (e.key === 'ArrowLeft') {
        e.preventDefault();
        e.stopPropagation();
        go(-1);
      }
    };
    // Capture phase so we intercept before the underlying modal's own handler.
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
    // `go` closes over safeIndex / list.length / onIndexChange — depend on those
    // (plus onClose) instead of re-registering on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [onClose, onIndexChange, safeIndex, list.length]);

  // Move focus into the lightbox on open so it owns the keyboard (and Tab stays in
  // the overlay rather than reaching the dialog/grid behind).
  useEffect(() => {
    containerRef.current?.focus({ preventScroll: true });
  }, []);

  if (!cur) return null;

  return (
    <div
      ref={containerRef}
      data-testid="image-lightbox"
      role="dialog"
      aria-modal="true"
      aria-label={cur.label || t('dialogLabel')}
      tabIndex={-1}
      className="fixed inset-0 z-[120] flex flex-col bg-black/90 u-backdrop-in focus:outline-none"
      onClick={onClose}
    >
      {/* Top bar */}
      <div
        className="flex items-center gap-3 px-4 h-12 shrink-0 text-gray-300"
        onClick={(e) => e.stopPropagation()}
      >
        <span className="text-xs tabular-nums text-gray-400">
          {safeIndex + 1} / {list.length}
        </span>
        {cur.label && <span className="text-sm truncate text-gray-200">{cur.label}</span>}
        <div className="flex-1" />
        {cur.href && (
          <button
            onClick={() => window.electronAPI?.openExternal?.(cur.href)}
            className="u-press flex items-center gap-1.5 px-2.5 h-8 rounded-md text-xs text-gray-200 bg-white/10 hover:bg-white/15"
            title={t('openPageTitle')}
          >
            <ExternalLink size={13} /> {t('openPage')}
          </button>
        )}
        <button
          data-testid="lightbox-close"
          onClick={onClose}
          className="u-press flex items-center justify-center w-8 h-8 rounded-md text-gray-300 hover:text-white hover:bg-white/10"
          title={t('closeEsc')}
        >
          <X size={18} />
        </button>
      </div>

      {/* Scrollable image stage */}
      <div
        ref={scrollRef}
        className="flex-1 overflow-y-auto overflow-x-hidden scrollbar-thin scrollbar-thumb-[#3a3a3a] px-4 pb-6"
        onClick={onClose}
      >
        <div className="mx-auto max-w-[1100px]" onClick={(e) => e.stopPropagation()}>
          {failed ? (
            <div className="flex items-center justify-center h-[60vh] text-sm text-gray-500">
              {t('loadFailed')}
            </div>
          ) : Array.isArray(cur.chunks) && cur.chunks.length > 1 ? (
            // Tall capture: stack the vertical bands, lazy-loading each so only the
            // visible portion is ever decoded (no single 1280×12000 frame in memory).
            <div className="overflow-hidden rounded-md">
              {cur.chunks.map((src, ci) => (
                <img
                  key={ci}
                  src={src}
                  alt=""
                  className="w-full h-auto block align-top"
                  draggable={false}
                  loading="lazy"
                  decoding="async"
                  onError={ci === 0 ? () => setFailed(true) : undefined}
                />
              ))}
            </div>
          ) : (
            <img
              src={cur.src}
              alt={cur.label || ''}
              className="w-full h-auto block rounded-md"
              draggable={false}
              onError={() => setFailed(true)}
            />
          )}
        </div>
      </div>

      {/* Prev / next (only with more than one image) */}
      {list.length > 1 && (
        <>
          <button
            data-testid="lightbox-prev"
            onClick={(e) => {
              e.stopPropagation();
              go(-1);
            }}
            className="u-press fixed left-3 top-1/2 -translate-y-1/2 flex items-center justify-center w-10 h-10 rounded-full bg-white/10 hover:bg-white/20 text-white"
            title={t('prev')}
          >
            <ChevronLeft size={20} />
          </button>
          <button
            data-testid="lightbox-next"
            onClick={(e) => {
              e.stopPropagation();
              go(1);
            }}
            className="u-press fixed right-3 top-1/2 -translate-y-1/2 flex items-center justify-center w-10 h-10 rounded-full bg-white/10 hover:bg-white/20 text-white"
            title={t('next')}
          >
            <ChevronRight size={20} />
          </button>
        </>
      )}
    </div>
  );
}
