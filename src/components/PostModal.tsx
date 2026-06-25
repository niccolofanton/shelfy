import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  X,
  Instagram,
  Twitter,
  HardDriveDownload,
  ChevronLeft,
  ChevronRight,
  Globe,
  Bookmark,
} from 'lucide-react';
import { assetUrl, isAssetUrl } from '../lib/asset';
import { useT } from '../i18n';
import ImageLightbox, { LightboxImage } from './ImageLightbox';
import PinterestIcon from './PinterestIcon';
import CollectionModal from './CollectionModal';
import { resolveUrl, webPageLabel, buildSlides, pickSlideMedia } from './postmodal/helpers';
import MediaCarousel from './postmodal/MediaCarousel';
import MetaColumn, { ApplyAiFilter, PostUpdated } from './postmodal/MetaColumn';
import ActionsMenu from './postmodal/ActionsMenu';
import CollectionsMenu from './postmodal/CollectionsMenu';

interface PostModalProps {
  post: Shelfy.Post;
  onClose: () => void;
  onPrev?: () => void;
  onNext?: () => void;
  hasPrev?: boolean;
  hasNext?: boolean;
  onApplyAiFilter?: ApplyAiFilter;
  onLocalFilesDeleted?: (postId: string) => void;
  onPostDeleted?: (postId: string) => void;
  onPostUpdated?: PostUpdated;
  onOpenInWebsites?: () => void;
  onReanalyzeWeb?: (post: Shelfy.Post) => void;
  onAssigned?: () => void;
}

// Shell/orchestrator: owns the shared state (slide index, lightbox, layer flags,
// collections membership, keyboard + focus handling) and composes the postmodal/
// subcomponents — MediaCarousel | MetaColumn under a header with CollectionsMenu
// and ActionsMenu.
export default function PostModal({
  post,
  onClose,
  onPrev,
  onNext,
  hasPrev = false,
  hasNext = false,
  onApplyAiFilter,
  onLocalFilesDeleted,
  onPostDeleted,
  onPostUpdated,
  onOpenInWebsites,
  onReanalyzeWeb,
  onAssigned,
}: PostModalProps): React.JSX.Element {
  const t = useT('postModal');
  const tc = useT('common');
  // Slides only change when the post itself changes; recomputing per render
  // would churn the slide-dependent effects below.
  // eslint-disable-next-line react-hooks/exhaustive-deps -- preexisting: keyed on post.id/post.media on purpose
  const slides = useMemo(() => buildSlides(post), [post.id, post.media]);
  const [slide, setSlide] = useState<number>(0);
  // The dialog panel — focused on open so keyboard/screen-reader users land inside
  // it, and used to trap Tab so focus can't escape into the obscured grid behind.
  const panelRef = useRef<HTMLDivElement | null>(null);
  // Always-fresh id of the post currently shown. The shell isn't remounted on post
  // switch (callers don't pass a key), so an in-flight assign closure must read the
  // CURRENT post from here — not its captured `post` — to detect a navigation.
  const postIdRef = useRef<string>(post.id);
  useEffect(() => {
    postIdRef.current = post.id;
  }, [post.id]);

  // AiPanel reports whether its inline editor is open with unsaved drafts, so a
  // backdrop click / Escape doesn't silently discard the user's in-progress edits.
  const aiEditingRef = useRef<boolean>(false);
  const handleAiEditingChange = useCallback((editing: boolean) => {
    aiEditingRef.current = editing;
  }, []);
  const requestClose = (): void => {
    if (aiEditingRef.current) {
      const ok = window.confirm(t('unsavedConfirm'));
      if (!ok) return;
    }
    onClose();
  };

  // ── Add-to-source (collection) — single-post mirror of the gallery bulk action ─
  // The modal loads its own collection list so the action works from every mount
  // point (gallery, search, tags, browser) without threading props through each.
  const [collections, setCollections] = useState<Shelfy.Collection[]>([]);
  const [assignOpen, setAssignOpen] = useState<boolean>(false);
  const [showCreateCollection, setShowCreateCollection] = useState<boolean>(false);
  // Collections this post already belongs to — seeded from the post, kept fresh
  // optimistically as the user assigns. Drives the green check in the picker.
  const [assignedIds, setAssignedIds] = useState<Set<number>>(
    () => new Set(post.collectionIds || []),
  );

  useEffect(() => {
    let alive = true;
    window.electronAPI
      .getCollections()
      .then((list) => {
        if (alive) setCollections(list || []);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  // Re-seed membership when switching to a different post.
  useEffect(() => {
    setAssignedIds(new Set(post.collectionIds || []));
    // eslint-disable-next-line react-hooks/exhaustive-deps -- preexisting: re-seed only on post switch
  }, [post.id]);

  // Reset to the first slide whenever a different post is opened.
  useEffect(() => {
    setSlide(0);
  }, [post.id]);

  const slideCount = slides.length;
  const hasMultiple = slideCount > 1;
  const clampedSlide = Math.min(slide, Math.max(0, slideCount - 1));
  const current = slides[clampedSlide];

  const goSlidePrev = (): void => setSlide((s) => (s > 0 ? s - 1 : s));
  const goSlideNext = (): void => setSlide((s) => (s < slideCount - 1 ? s + 1 : s));

  // Full-screen image viewer (click-to-zoom). Built from the image slides only, so
  // a full-page web screenshot can be scrolled at full width and pages navigated.
  const [lightboxIndex, setLightboxIndex] = useState<number | null>(null);
  const imageSlides = useMemo(
    () =>
      slides
        .map((s, i) => ({ s, i }))
        .filter(({ s }) => s && s.type !== 'video' && (s.localPath || s.url)),
    [slides],
  );
  const openLightbox = (): void => {
    const pos = imageSlides.findIndex(({ i }) => i === clampedSlide);
    setLightboxIndex(pos >= 0 ? pos : 0);
  };
  // Keyboard activation for the click-to-zoom media (Enter / Space), so the
  // lightbox is reachable without a mouse.
  const openLightboxOnKey = (e: React.KeyboardEvent): void => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      openLightbox();
    }
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      // While a layer is open above the modal it owns the keyboard (Esc/arrows):
      // the full-screen lightbox, the "Aggiungi a source" popover, or the
      // create-source dialog. Ignore here so Escape only dismisses the topmost
      // layer (not the whole modal) and an arrow doesn't step the background
      // slide/post in parallel.
      if (lightboxIndex != null || assignOpen || showCreateCollection) return;
      if (e.key === 'Escape') requestClose();
      else if (e.key === 'ArrowLeft') {
        // Within a multi-slide post, arrows step through slides first; once at an
        // edge they move between posts.
        if (hasMultiple && clampedSlide > 0) goSlidePrev();
        else if (hasPrev) onPrev?.();
      } else if (e.key === 'ArrowRight') {
        if (hasMultiple && clampedSlide < slideCount - 1) goSlideNext();
        else if (hasNext) onNext?.();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- preexisting: goSlideNext/requestClose are stable per render snapshot
  }, [
    onClose,
    onPrev,
    onNext,
    hasPrev,
    hasNext,
    hasMultiple,
    clampedSlide,
    slideCount,
    lightboxIndex,
    assignOpen,
    showCreateCollection,
  ]);

  // Move focus into the dialog on open and trap Tab within it, so keyboard /
  // screen-reader users are placed inside the dialog and can't Tab out into the
  // obscured grid behind. The lightbox owns focus while open, so skip then.
  useEffect(() => {
    const panel = panelRef.current;
    if (!panel) return;
    panel.focus({ preventScroll: true });
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== 'Tab' || lightboxIndex != null) return;
      const focusables = panel.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
      if (!focusables.length) {
        e.preventDefault();
        panel.focus({ preventScroll: true });
        return;
      }
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      const active = document.activeElement;
      if (e.shiftKey && (active === first || active === panel)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    panel.addEventListener('keydown', onKey);
    return () => panel.removeEventListener('keydown', onKey);
  }, [post.id, lightboxIndex]);

  const url = resolveUrl(post);
  const isWeb = post.platform === 'web';
  const isManual = post.platform === 'manual';
  const Icon = isWeb
    ? Globe
    : isManual
      ? Bookmark
      : post.platform === 'instagram'
        ? Instagram
        : post.platform === 'pinterest'
          ? PinterestIcon
          : Twitter;
  const accent =
    isWeb || isManual
      ? '#7B5CFF'
      : post.platform === 'instagram'
        ? '#c2185b'
        : post.platform === 'pinterest'
          ? '#e60023'
          : '#1565c0';
  const platformLabel = isWeb
    ? t('website')
    : isManual
      ? t('manualBookmark')
      : post.platform === 'instagram'
        ? 'Instagram'
        : post.platform === 'pinterest'
          ? 'Pinterest'
          : t('platformX');

  const media = pickSlideMedia(post, current, slideCount);
  const isLocal = media.kind === 'image' || media.kind === 'video' ? isAssetUrl(media.src) : false;

  // Primary downloaded file to reveal/open with one click (most "complete" asset
  // first). For manual bookmarks the current slide's source_url carries the
  // ORIGINAL file path (a pdf/file slide renders a preview, so localPath would
  // point at the webp preview, not the real file) — reveal that instead.
  const primaryLocalPath =
    (isManual && current?.url) ||
    post.videoPath ||
    current?.localPath ||
    post.imagePath ||
    post.thumbnailPath ||
    null;

  // Add this post to a source. Optimistic (green check flips instantly); roll the
  // membership back if the IPC write fails. INSERT OR IGNORE on the backend means
  // re-adding an already-member post is harmless. The pid snapshot guards against
  // navigating to another post mid-flight: a late success/failure must not touch
  // the now-current post's checkmarks (which the [post.id] effect already re-seeded).
  async function assignToCollection(cid: number): Promise<void> {
    const pid = post.id;
    setAssignedIds((prev) => new Set(prev).add(cid));
    try {
      await window.electronAPI.addPostsToCollections([pid], [cid]);
      onAssigned?.(); // refresh sidebar source counts where the parent wires it
    } catch (err) {
      console.error('[PostModal] addPostsToCollections error:', err);
      if (postIdRef.current !== pid) return; // navigated away — don't corrupt the new post
      setAssignedIds((prev) => {
        const next = new Set(prev);
        next.delete(cid);
        return next;
      });
    }
  }

  async function handleCreateAndAssign({
    name,
    color,
  }: {
    name: string;
    color: string;
  }): Promise<void> {
    try {
      const created = await window.electronAPI.createCollection(name, color);
      // Refetch so the new source lands in the picker with the same ordering the
      // sidebar uses, then assign this post to it.
      try {
        const list = await window.electronAPI.getCollections();
        setCollections(list || []);
      } catch {
        /* best-effort list refresh */
      }
      if (created?.id) await assignToCollection(created.id);
    } catch (err) {
      console.error('[PostModal] createCollection error:', err);
    } finally {
      setShowCreateCollection(false);
    }
  }

  return (
    <>
      <div
        data-testid="post-modal"
        className="u-backdrop-in fixed inset-0 bg-black/70 flex items-center justify-center z-50 p-6"
        onClick={requestClose}
      >
        {/* Post navigation — outside the panel, pinned to the screen edges */}
        {hasPrev && (
          <button
            data-testid="post-modal-prev"
            onClick={(e) => {
              e.stopPropagation();
              onPrev?.();
            }}
            title={t('prevPost')}
            className="u-press u-lift absolute left-3 top-1/2 -translate-y-1/2 z-50 flex items-center justify-center w-11 h-11 rounded-full bg-[#1a1a1a]/80 border border-[#2e2e2e] text-white/70 hover:text-white hover:bg-[#2a2a2a]"
          >
            <ChevronLeft size={24} />
          </button>
        )}
        {hasNext && (
          <button
            data-testid="post-modal-next"
            onClick={(e) => {
              e.stopPropagation();
              onNext?.();
            }}
            title={t('nextPost')}
            className="u-press u-lift absolute right-3 top-1/2 -translate-y-1/2 z-50 flex items-center justify-center w-11 h-11 rounded-full bg-[#1a1a1a]/80 border border-[#2e2e2e] text-white/70 hover:text-white hover:bg-[#2a2a2a]"
          >
            <ChevronRight size={24} />
          </button>
        )}

        <div
          ref={panelRef}
          role="dialog"
          aria-modal="true"
          aria-label={
            isWeb
              ? post.webDomain || post.authorName || t('website')
              : post.authorName || post.authorUsername || t('post')
          }
          tabIndex={-1}
          className="select-text u-dialog-in bg-[#1a1a1a] border border-[#2e2e2e] rounded-xl shadow-2xl flex flex-col w-full max-w-5xl h-[88vh] overflow-hidden focus:outline-none"
          onClick={(e) => e.stopPropagation()}
        >
          {/* Header */}
          <div className="flex items-center gap-2 px-4 h-12 flex-shrink-0 border-b border-[#2e2e2e]">
            <Icon size={16} style={{ color: accent }} title={platformLabel} className="shrink-0" />
            <div className="flex items-baseline gap-1.5 min-w-0">
              {isWeb ? (
                <>
                  <span className="text-white text-sm font-medium truncate">
                    {post.webDomain || post.authorName || t('website')}
                  </span>
                  {post.authorName && post.authorName !== post.webDomain && (
                    <span className="text-[#888] text-xs truncate shrink-0">{post.authorName}</span>
                  )}
                </>
              ) : (
                <>
                  {post.authorName && (
                    <span className="text-white text-sm font-medium truncate">
                      {post.authorName}
                    </span>
                  )}
                  <span
                    className={
                      post.authorName
                        ? 'text-[#888] text-xs truncate shrink-0'
                        : 'text-white text-sm font-medium truncate'
                    }
                  >
                    @{post.authorUsername || t('unknownAuthor')}
                  </span>
                </>
              )}
            </div>
            {isLocal && (
              <span
                className="u-pop-in flex items-center gap-1 text-[10px] text-green-400 bg-green-500/10 rounded px-1.5 py-0.5"
                title={t('viewingLocal')}
              >
                <HardDriveDownload size={11} />
                {t('local')}
              </span>
            )}
            <div className="flex-1" />

            <CollectionsMenu
              collections={collections}
              assignedIds={assignedIds}
              open={assignOpen}
              onToggle={() => setAssignOpen((o) => !o)}
              onRequestClose={() => setAssignOpen(false)}
              onAssign={assignToCollection}
              onCreateNew={() => {
                setAssignOpen(false);
                setShowCreateCollection(true);
              }}
            />

            <ActionsMenu
              post={post}
              url={url}
              primaryLocalPath={primaryLocalPath}
              isManual={isManual}
              onLocalFilesDeleted={onLocalFilesDeleted}
              onPostDeleted={onPostDeleted}
              onPostUpdated={onPostUpdated}
              onClose={onClose}
            />

            <button
              data-testid="post-modal-close"
              onClick={requestClose}
              title={tc('close')}
              className="u-press flex items-center justify-center w-8 h-8 rounded-md text-[#a0a0a0] hover:text-white hover:bg-[#2a2a2a]"
            >
              <X size={16} />
            </button>
          </div>

          {/* ── Two columns — media / web screenshot | written content ───────── */}
          <div className="flex-1 min-h-0 flex overflow-hidden">
            <MediaCarousel
              post={post}
              isWeb={isWeb}
              media={media}
              current={current}
              slides={slides}
              clampedSlide={clampedSlide}
              slideCount={slideCount}
              hasMultiple={hasMultiple}
              onSlidePrev={goSlidePrev}
              onSlideNext={goSlideNext}
              onSelectSlide={setSlide}
              onOpenLightbox={openLightbox}
              onOpenLightboxKey={openLightboxOnKey}
            />

            <MetaColumn
              post={post}
              isWeb={isWeb}
              slideCount={slideCount}
              hasMultiple={hasMultiple}
              onApplyAiFilter={onApplyAiFilter}
              onPostUpdated={onPostUpdated}
              onOpenInWebsites={onOpenInWebsites}
              onReanalyzeWeb={onReanalyzeWeb}
              onAiEditingChange={handleAiEditingChange}
            />
          </div>
        </div>
      </div>

      {lightboxIndex != null && imageSlides.length > 0 && (
        <ImageLightbox
          images={imageSlides.map(
            ({ s }, i): LightboxImage => ({
              src: (s.localPath ? assetUrl(s.localPath) : s.url) || '',
              // Web captures store tall pages as vertical chunks; match this slide's
              // page by URL and hand the lightbox the band list so it lazy-stacks them.
              chunks: isWeb
                ? (() => {
                    const pg = (Array.isArray(post.webPages) ? post.webPages : []).find(
                      (p) => p && p.url === s.url && Array.isArray(p.chunks) && p.chunks.length > 1,
                    );
                    return pg
                      ? pg.chunks
                          ?.map((c) => assetUrl(c.screenshotPath ?? null))
                          .filter((src): src is string => Boolean(src))
                      : undefined;
                  })()
                : undefined,
              label: isWeb
                ? [post.webDomain, webPageLabel(s.url, post.webFinalUrl || post.postUrl, i, t)]
                    .filter(Boolean)
                    .join(' · ')
                : post.authorUsername
                  ? `@${post.authorUsername}`
                  : '',
              href: isWeb ? (s.url ?? undefined) : undefined,
            }),
          )}
          index={lightboxIndex}
          onClose={() => setLightboxIndex(null)}
          onIndexChange={setLightboxIndex}
        />
      )}

      {showCreateCollection && (
        <CollectionModal
          collections={collections}
          onClose={() => setShowCreateCollection(false)}
          onSave={handleCreateAndAssign}
        />
      )}
    </>
  );
}
