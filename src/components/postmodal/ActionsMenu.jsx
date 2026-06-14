import { useEffect, useRef, useState } from 'react';
import {
  ExternalLink,
  HardDriveDownload,
  FolderOpen,
  Loader2,
  Trash2,
  MoreVertical,
} from 'lucide-react';
import { useT } from '../../i18n';

// The header "more" (⋮) menu: open file / open original / download / delete
// local files / delete post. Owns all the per-post download + delete state —
// nothing here is needed by the rest of the modal beyond the parent callbacks.
export default function ActionsMenu({
  post,
  url,
  primaryLocalPath,
  isManual,
  onLocalFilesDeleted,
  onPostDeleted,
  onPostUpdated,
  onClose,
}) {
  const t = useT('postModal');
  const [deleteConfirm, setDeleteConfirm] = useState(false);
  const [deleting, setDeleting] = useState(false);
  // Separate two-step confirm + in-flight flag for the destructive "delete whole
  // post" action (distinct from "delete local files only" above).
  const [deletePostConfirm, setDeletePostConfirm] = useState(false);
  const [deletingPost, setDeletingPost] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [downloadQueued, setDownloadQueued] = useState(false);
  // Surfaced when a download was requested but the queue produced no jobs (e.g. a
  // web/text post with no downloadable asset type) — otherwise the spinner would
  // hang on "In coda…" forever since no progress event ever arrives.
  const [downloadEmpty, setDownloadEmpty] = useState(false);
  // Surfaced when a destructive/download action throws (or a download job reports
  // 'error'), so the button no longer just silently snaps back with no explanation.
  const [actionError, setActionError] = useState('');
  // Cleared when a real progress event lands; fires the fallback otherwise.
  const downloadTimerRef = useRef(null);
  // Debounce for "queue settled" — see the progress handler below.
  const settleTimerRef = useRef(null);
  const menuRef = useRef(null);

  // Equivalent of "any local asset on disk" (thumbnail / image / video).
  const hasLocalFiles = Boolean(post.thumbnailPath || post.imagePath || post.videoPath);

  // Close the actions menu on outside click; also reset both delete
  // confirmations so a primed state never lingers across re-opens.
  useEffect(() => {
    if (!menuOpen) {
      setDeleteConfirm(false);
      setDeletePostConfirm(false);
      setActionError('');
      return;
    }
    const onDocClick = (e) => {
      if (menuRef.current && !menuRef.current.contains(e.target)) setMenuOpen(false);
    };
    document.addEventListener('mousedown', onDocClick);
    return () => document.removeEventListener('mousedown', onDocClick);
  }, [menuOpen]);

  // Reset the download state whenever a different post is opened.
  useEffect(() => {
    setDownloadQueued(false);
    setDownloadEmpty(false);
    clearTimeout(downloadTimerRef.current);
    clearTimeout(settleTimerRef.current);
  }, [post.id]);

  // Clear any pending timers on unmount.
  useEffect(
    () => () => {
      clearTimeout(downloadTimerRef.current);
      clearTimeout(settleTimerRef.current);
    },
    [],
  );

  // Re-fetch this post from the DB and patch it in place, so menu actions
  // (download finished, local files removed) reflect without closing the modal.
  async function refreshPost() {
    try {
      const [updated] = await window.electronAPI.getPostsByIds([post.id]);
      if (updated) onPostUpdated?.(post.id, updated);
    } catch {
      /* best-effort refresh */
    }
  }

  // Queue a one-off download of this single post's assets. The download runs in
  // the background queue; the progress subscription below clears the flag and
  // refreshes the post once the assets land.
  async function handleDownload() {
    setDownloadEmpty(false);
    setActionError('');
    setDownloadQueued(true);
    try {
      const res = await window.electronAPI.downloadPost(post.id);
      // If the backend reports the number of enqueued jobs and it's 0 (e.g. a web
      // or text-only post with no eligible asset type), nothing will ever emit a
      // download:progress event — surface "nothing to download" instead of hanging.
      if (res && typeof res.queued === 'number' && res.queued === 0) {
        clearTimeout(downloadTimerRef.current);
        setDownloadQueued(false);
        setDownloadEmpty(true);
        return;
      }
      // Fallback for backends that don't return a count: if no progress event for
      // this post arrives within a grace window, assume nothing was enqueued and
      // clear the spinner rather than leaving it stuck on "In coda…" forever.
      clearTimeout(downloadTimerRef.current);
      downloadTimerRef.current = setTimeout(() => {
        setDownloadQueued((q) => {
          if (q) setDownloadEmpty(true);
          return false;
        });
      }, 8000);
    } catch (err) {
      // The download IPC rejected (e.g. "post not found"): log it and reset the
      // spinner so the failure isn't indistinguishable from a successful no-op.
      console.error('[PostModal] downloadPost error:', err);
      clearTimeout(downloadTimerRef.current);
      setDownloadQueued(false);
      setActionError(t('downloadFailed'));
    }
  }

  // While a download for this post is queued, reflect completion in place:
  // refresh as its assets land so the menu swaps to the "local" actions.
  //
  // A multi-asset post (thumbnail + image slides + video) emits one
  // download:progress 'done' per asset, all carrying the same postId. The first
  // 'done' (often the fast thumbnail) is NOT overall completion: refreshing on it
  // and unsubscribing would show partial local files and prematurely flip the menu
  // to "Elimina file locali". Instead we refresh on every terminal event (so each
  // asset shows as it lands) but keep the subscription open, and only clear the
  // "In coda…" spinner once the per-post stream has gone quiet for a short window
  // (queue settled) — a within-file approximation of "all expected jobs done".
  //
  // The IPC subscription itself is registered ONCE per mount (empty deps):
  // re-subscribing on every post switch / render duplicated the listener and
  // leaked stale handlers. The handler reads the latest closure through a ref,
  // so the per-post filter happens inside the handler instead of by recreating
  // the subscription.
  const onProgressRef = useRef(null);
  useEffect(() => {
    onProgressRef.current = (job) => {
      // Only this post's assets matter. The per-event refresh below is NOT gated
      // on `downloadQueued`: a slow-tail asset (e.g. a video landing seconds after
      // a fast thumbnail) can arrive after the settle timer has already cleared the
      // spinner — gating here would silently drop its refresh, leaving the modal on
      // the remote URL and the menu without the "local" actions. The queued flag is
      // only used to drive the spinner (via settle).
      if (job.postId !== post.id) return;
      // Real activity for this post: cancel the "nothing was enqueued" fallback.
      clearTimeout(downloadTimerRef.current);
      const settle = () => {
        clearTimeout(settleTimerRef.current);
        settleTimerRef.current = setTimeout(() => setDownloadQueued(false), 1500);
      };
      if (job.status === 'done') {
        setActionError('');
        refreshPost();
        onLocalFilesDeleted?.(post.id); // reuse parent hook to refresh grid/stats
        settle();
      } else if (job.status === 'error') {
        // A failed download must not just silently stop the spinner: surface it.
        setActionError(t('downloadFailed'));
        settle();
      }
    };
  });
  useEffect(() => {
    const unsub = window.electronAPI.onDownloadProgress((job) => onProgressRef.current?.(job));
    return () => unsub?.();
  }, []);

  async function handleDeleteLocal() {
    if (!deleteConfirm) {
      setDeleteConfirm(true);
      setDeletePostConfirm(false);
      return;
    }
    setDeleting(true);
    setActionError('');
    try {
      await window.electronAPI.deleteLocalFiles(post.id);
      onLocalFilesDeleted?.(post.id);
      await refreshPost();
    } catch (err) {
      // A rejecting IPC (DB error, missing paths) would otherwise surface as an
      // unhandled rejection on this discarded onClick promise; log it so the
      // silently-reset button has a visible cause.
      console.error('[PostModal] deleteLocalFiles error:', err);
      setActionError(t('actionFailed'));
    } finally {
      setDeleting(false);
      setDeleteConfirm(false);
    }
  }

  // Permanently delete the whole post (DB record + downloaded files).
  async function handleDeletePost() {
    if (!deletePostConfirm) {
      setDeletePostConfirm(true);
      setDeleteConfirm(false);
      return;
    }
    setDeletingPost(true);
    setActionError('');
    try {
      await window.electronAPI.deletePosts([post.id]);
      onPostDeleted?.(post.id);
      onClose();
    } catch (err) {
      // deletePosts can reject (DB error, > MAX_BULK_ITEMS guard). Without a catch
      // this discarded onClick promise would raise an unhandled rejection and the
      // destructive action would fail silently; log it and leave the modal open so
      // the user can retry rather than assuming the post was removed.
      console.error('[PostModal] deletePosts error:', err);
      setActionError(t('actionFailed'));
    } finally {
      setDeletingPost(false);
      setDeletePostConfirm(false);
    }
  }

  /* Actions collapsed under a "more" menu, keeping the header uncluttered.
     Toggles on click; closes on outside click (see effect above). */
  return (
    <div ref={menuRef} className="relative">
      <button
        data-testid="post-modal-more"
        onClick={() => setMenuOpen((o) => !o)}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        title={t('moreActions')}
        className="u-press flex items-center justify-center w-8 h-8 rounded-md text-[#a0a0a0] hover:text-white hover:bg-[#2a2a2a]"
      >
        <MoreVertical size={16} />
      </button>

      {menuOpen && (
        <div className="absolute right-0 top-full pt-1 z-50">
          <div
            data-testid="post-modal-menu"
            role="menu"
            className="u-fade-in-down origin-top min-w-[190px] bg-[#1f1f1f] border border-[#2e2e2e] rounded-lg shadow-2xl py-1 flex flex-col"
          >
            {primaryLocalPath && (
              <button
                data-testid="post-modal-openfile"
                onClick={() => {
                  window.electronAPI.showItemInFolder(primaryLocalPath);
                  setMenuOpen(false);
                }}
                className="u-press flex items-center gap-2.5 px-3 py-2 text-xs text-[#cfcfcf] hover:bg-[#2a2a2a] hover:text-white text-left"
              >
                <FolderOpen size={14} className="shrink-0" />
                {t('openFile')}
              </button>
            )}
            {/* Manual bookmarks (and any post without a captured URL) have no
              original page to open — hide the entry instead of failing silently. */}
            {url && (
              <button
                data-testid="post-modal-external"
                onClick={() => {
                  window.electronAPI.openExternal(url);
                  setMenuOpen(false);
                }}
                className="u-press flex items-center gap-2.5 px-3 py-2 text-xs text-[#cfcfcf] hover:bg-[#2a2a2a] hover:text-white text-left"
              >
                <ExternalLink size={14} className="shrink-0" />
                {t('openOriginal')}
              </button>
            )}
            {/* Manual bookmarks have no remote source: nothing to (re)download,
              and "delete local files" would destroy the only copy of the
              original (image/video) or leave a broken preview (pdf/file).
              Only "Elimina post" (below) applies to them. */}
            {!hasLocalFiles && !isManual && (
              <button
                data-testid="post-modal-download"
                onClick={handleDownload}
                disabled={downloadQueued || downloadEmpty}
                title={downloadEmpty ? t('noDownloadableFiles') : t('downloadFilesTitle')}
                className="u-press flex items-center gap-2.5 px-3 py-2 text-xs text-[#cfcfcf] hover:bg-[#2a2a2a] hover:text-white text-left disabled:opacity-50"
              >
                {downloadQueued ? (
                  <Loader2 size={14} className="animate-spin shrink-0" />
                ) : (
                  <HardDriveDownload size={14} className="shrink-0" />
                )}
                <span
                  key={downloadEmpty ? 'empty' : downloadQueued ? 'queued' : 'idle'}
                  className="u-fade-in"
                >
                  {downloadEmpty
                    ? t('nothingToDownload')
                    : downloadQueued
                      ? t('queued')
                      : t('downloadLocal')}
                </span>
              </button>
            )}
            {hasLocalFiles && !isManual && (
              <button
                data-testid="post-modal-delete-local"
                onClick={handleDeleteLocal}
                disabled={deleting}
                title={deleteConfirm ? t('clickAgainToConfirm') : t('deleteLocalFilesTitle')}
                className={[
                  'u-press flex items-center gap-2.5 px-3 py-2 text-xs text-left disabled:opacity-50 transition-colors',
                  deleteConfirm
                    ? 'bg-red-500/15 text-red-300 hover:bg-red-500/25'
                    : 'text-[#cfcfcf] hover:bg-[#2a2a2a] hover:text-red-300',
                ].join(' ')}
              >
                {deleting ? (
                  <Loader2 size={14} className="animate-spin shrink-0" />
                ) : (
                  <Trash2 size={14} className="shrink-0" />
                )}
                <span key={deleteConfirm ? 'confirm' : 'idle'} className="u-fade-in">
                  {deleteConfirm ? t('confirmDeletion') : t('deleteLocalFiles')}
                </span>
              </button>
            )}

            <div className="my-1 border-t border-[#2e2e2e]" />

            {/* Destructive: removes the post entirely (DB record + files). */}
            <button
              data-testid="post-modal-delete-post"
              onClick={handleDeletePost}
              disabled={deletingPost}
              title={deletePostConfirm ? t('clickAgainToConfirm') : t('deletePostTitle')}
              className={[
                'u-press flex items-center gap-2.5 px-3 py-2 text-xs text-left disabled:opacity-50 transition-colors',
                deletePostConfirm
                  ? 'bg-red-500/20 text-red-300 hover:bg-red-500/30'
                  : 'text-red-400/90 hover:bg-red-500/10 hover:text-red-300',
              ].join(' ')}
            >
              {deletingPost ? (
                <Loader2 size={14} className="animate-spin shrink-0" />
              ) : (
                <Trash2 size={14} className="shrink-0" />
              )}
              <span key={deletePostConfirm ? 'confirm' : 'idle'} className="u-fade-in">
                {deletePostConfirm ? t('confirmDeletePost') : t('deletePost')}
              </span>
            </button>

            {actionError && (
              <div
                role="alert"
                data-testid="action-error"
                className="px-3 py-2 text-xs text-red-300 border-t border-[#2e2e2e] u-fade-in"
              >
                {actionError}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
