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
import { useShelfy } from '../../api/ShelfyProvider';
import Popover from '../Popover';
import IconButton from '../ui/IconButton';
import type { PostUpdated } from './MetaColumn';

// A download:progress event for a single asset (downloader-internal runtime shape;
// the IPC layer types it `unknown`, so the fields read here are narrowed locally).
interface DownloadProgress {
  postId?: string | null;
  status?: string;
}
function asDownloadProgress(value: unknown): DownloadProgress {
  return (value ?? {}) as DownloadProgress;
}

interface ActionsMenuProps {
  post: Shelfy.Post;
  url: string | null;
  primaryLocalPath: string | null;
  // The media shown in the modal failed to load, so the download/open actions
  // that target the same stored object would fail too — disable them (MOD-13).
  mediaUnavailable?: boolean;
  isManual: boolean;
  onLocalFilesDeleted?: (id: string) => void;
  // `deletedAt` is the bulk seam's undo handle (P1-14: `null` when nothing
  // could be undone — the desktop's permanent delete, or F11's "nothing
  // moved"); callers that don't offer an undo affordance can ignore it.
  onPostDeleted?: (id: string, deletedAt: number | null) => void;
  onPostUpdated?: PostUpdated;
  onClose: () => void;
}

// The header "more" (⋮) menu: open file / open original / download / delete
// local files / delete post. Owns all the per-post download + delete state —
// nothing here is needed by the rest of the modal beyond the parent callbacks.
// Each entry exists only where the client backs it: the web shows "open
// original" alone for now (capabilities `localFiles` and `bulkActions`).
export default function ActionsMenu({
  post,
  url,
  primaryLocalPath,
  mediaUnavailable = false,
  isManual,
  onLocalFilesDeleted,
  onPostDeleted,
  onPostUpdated,
  onClose,
}: ActionsMenuProps) {
  const t = useT('postModal');
  const client = useShelfy();
  const { localFiles, bulkActions } = client.capabilities;
  const [deleteConfirm, setDeleteConfirm] = useState<boolean>(false);
  const [deleting, setDeleting] = useState<boolean>(false);
  // Separate two-step confirm + in-flight flag for the destructive "delete whole
  // post" action (distinct from "delete local files only" above).
  const [deletePostConfirm, setDeletePostConfirm] = useState<boolean>(false);
  const [deletingPost, setDeletingPost] = useState<boolean>(false);
  const [menuOpen, setMenuOpen] = useState<boolean>(false);
  const [downloadQueued, setDownloadQueued] = useState<boolean>(false);
  // Surfaced when a download was requested but the queue produced no jobs (e.g. a
  // web/text post with no downloadable asset type) — otherwise the spinner would
  // hang on "In coda…" forever since no progress event ever arrives.
  const [downloadEmpty, setDownloadEmpty] = useState<boolean>(false);
  // Surfaced when a destructive/download action throws (or a download job reports
  // 'error'), so the button no longer just silently snaps back with no explanation.
  const [actionError, setActionError] = useState<string>('');
  // Cleared when a real progress event lands; fires the fallback otherwise.
  const downloadTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Debounce for "queue settled" — see the progress handler below.
  const settleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // The trigger the menu anchors to (Popover reads it; outside-click, Escape and
  // the bottom-sheet-on-narrow presentation are the Popover's now).
  const triggerRef = useRef<HTMLButtonElement | null>(null);

  // Equivalent of "any local asset on disk" (thumbnail / image / video).
  const hasLocalFiles = Boolean(post.thumbnailPath || post.imagePath || post.videoPath);

  // Reset both delete confirmations when the menu closes, so a primed state
  // never lingers across re-opens.
  useEffect(() => {
    if (!menuOpen) {
      setDeleteConfirm(false);
      setDeletePostConfirm(false);
      setActionError('');
    }
  }, [menuOpen]);

  // Reset the download state whenever a different post is opened.
  useEffect(() => {
    setDownloadQueued(false);
    setDownloadEmpty(false);
    clearTimeout(downloadTimerRef.current ?? undefined);
    clearTimeout(settleTimerRef.current ?? undefined);
  }, [post.id]);

  // Clear any pending timers on unmount.
  useEffect(
    () => () => {
      clearTimeout(downloadTimerRef.current ?? undefined);
      clearTimeout(settleTimerRef.current ?? undefined);
    },
    [],
  );

  // Re-fetch this post from the DB and patch it in place, so menu actions
  // (download finished, local files removed) reflect without closing the modal.
  async function refreshPost(): Promise<void> {
    try {
      const [updated] = await client.getPostsByIds([post.id]);
      if (updated) onPostUpdated?.(post.id, updated);
    } catch {
      /* best-effort refresh */
    }
  }

  // Queue a one-off download of this single post's assets. The download runs in
  // the background queue; the progress subscription below clears the flag and
  // refreshes the post once the assets land.
  async function handleDownload(): Promise<void> {
    setDownloadEmpty(false);
    setActionError('');
    setDownloadQueued(true);
    try {
      const res = await window.electronAPI.downloadPost(post.id);
      // If the backend reports the number of enqueued jobs and it's 0 (e.g. a web
      // or text-only post with no eligible asset type), nothing will ever emit a
      // download:progress event — surface "nothing to download" instead of hanging.
      if (res && typeof res.queued === 'number' && res.queued === 0) {
        clearTimeout(downloadTimerRef.current ?? undefined);
        setDownloadQueued(false);
        setDownloadEmpty(true);
        return;
      }
      // Fallback for backends that don't return a count: if no progress event for
      // this post arrives within a grace window, assume nothing was enqueued and
      // clear the spinner rather than leaving it stuck on "In coda…" forever.
      clearTimeout(downloadTimerRef.current ?? undefined);
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
      clearTimeout(downloadTimerRef.current ?? undefined);
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
  const onProgressRef = useRef<((job: DownloadProgress) => void) | null>(null);
  useEffect(() => {
    onProgressRef.current = (job: DownloadProgress): void => {
      // Only this post's assets matter. The per-event refresh below is NOT gated
      // on `downloadQueued`: a slow-tail asset (e.g. a video landing seconds after
      // a fast thumbnail) can arrive after the settle timer has already cleared the
      // spinner — gating here would silently drop its refresh, leaving the modal on
      // the remote URL and the menu without the "local" actions. The queued flag is
      // only used to drive the spinner (via settle).
      if (job.postId !== post.id) return;
      // Real activity for this post: cancel the "nothing was enqueued" fallback.
      clearTimeout(downloadTimerRef.current ?? undefined);
      const settle = (): void => {
        clearTimeout(settleTimerRef.current ?? undefined);
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
    // Downloads are local: nothing to follow without the capability.
    if (!localFiles) return undefined;
    const unsub = window.electronAPI.onDownloadProgress((job) =>
      onProgressRef.current?.(asDownloadProgress(job)),
    );
    return () => unsub?.();
  }, [localFiles]);

  async function handleDeleteLocal(): Promise<void> {
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

  // Deletes the whole post, through the bulk seam (P1-14): a soft, undoable
  // move to the trash on the web; the desktop's electronClient still runs its
  // existing permanent deletePosts (no undo — `deletedAt` then stays null).
  async function handleDeletePost(): Promise<void> {
    if (!deletePostConfirm) {
      setDeletePostConfirm(true);
      setDeleteConfirm(false);
      return;
    }
    setDeletingPost(true);
    setActionError('');
    try {
      const { deletedAt } = await client.bulkAction({ keys: [post.id] }, 'delete');
      onPostDeleted?.(post.id, deletedAt);
      onClose();
    } catch (err) {
      // The bulk action can reject (network, a server error). Without a catch
      // this discarded onClick promise would raise an unhandled rejection and the
      // destructive action would fail silently; log it and leave the modal open so
      // the user can retry rather than assuming the post was removed.
      console.error('[PostModal] delete (bulkAction) error:', err);
      setActionError(t('actionFailed'));
    } finally {
      setDeletingPost(false);
      setDeletePostConfirm(false);
    }
  }

  // Nothing this client can do with the post: no menu at all.
  if (!url && !localFiles && !bulkActions && !primaryLocalPath) return null;

  // One row of the menu. On narrow the Popover renders as a bottom sheet (O8),
  // where `.u-sheet [role=menuitem]` grows every row to 48px for the thumb.
  const row =
    'u-press flex w-full items-center gap-2.5 px-3 py-2 text-xs text-left transition-colors';

  /* Actions collapsed under a "more" menu, keeping the header uncluttered. A
     shared Popover: anchored on desktop, a bottom sheet on narrow, with focus,
     arrow-key menu navigation and Escape handled for us (MOD-6, MOD-8, O8). */
  return (
    <>
      <IconButton
        ref={triggerRef}
        data-testid="post-modal-more"
        icon={MoreVertical}
        label={t('moreActions')}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        onClick={() => setMenuOpen((o) => !o)}
      />

      <Popover
        anchorRef={triggerRef}
        open={menuOpen}
        onRequestClose={() => setMenuOpen(false)}
        presentation="auto"
        align="right"
        role="menu"
        aria-label={t('moreActions')}
        data-testid="post-modal-menu"
        className="u-fade-in-down origin-top min-w-[210px] bg-[#1f1f1f] border border-[#2e2e2e] rounded-lg shadow-2xl py-1 flex flex-col"
        sheetClassName="flex flex-col"
      >
        {localFiles && primaryLocalPath && (
          <button
            role="menuitem"
            data-testid="post-modal-openfile"
            onClick={() => {
              window.electronAPI.showItemInFolder(primaryLocalPath);
              setMenuOpen(false);
            }}
            className={`${row} text-[#cfcfcf] hover:bg-[#2a2a2a] hover:text-white`}
          >
            <FolderOpen size={14} className="shrink-0" />
            {t('openFile')}
          </button>
        )}
        {/* The web has no local disk to open/reveal: a stored object is
          downloaded instead, through a plain same-origin <a download> (no
          server change needed). When that object itself is missing (the modal's
          media failed to load), the download would 404 — disable it with a
          reason instead of handing out a broken file (MOD-13). "Open original"
          below still covers a post whose media isn't archived yet. */}
        {!localFiles &&
          primaryLocalPath &&
          (mediaUnavailable ? (
            <button
              role="menuitem"
              data-testid="post-modal-download-original"
              disabled
              title={t('noStoredFile')}
              className={`${row} text-[#cfcfcf] disabled:opacity-50`}
            >
              <HardDriveDownload size={14} className="shrink-0" />
              {t('downloadOriginal')}
            </button>
          ) : (
            <a
              role="menuitem"
              data-testid="post-modal-download-original"
              href={client.media.file(primaryLocalPath) ?? undefined}
              download
              onClick={() => setMenuOpen(false)}
              className={`${row} text-[#cfcfcf] hover:bg-[#2a2a2a] hover:text-white`}
            >
              <HardDriveDownload size={14} className="shrink-0" />
              {t('downloadOriginal')}
            </a>
          ))}
        {/* Manual bookmarks (and any post without a captured URL) have no
          original page to open — hide the entry instead of failing silently. */}
        {url && (
          <button
            role="menuitem"
            data-testid="post-modal-external"
            onClick={() => {
              client.openExternal(url);
              setMenuOpen(false);
            }}
            className={`${row} text-[#cfcfcf] hover:bg-[#2a2a2a] hover:text-white`}
          >
            <ExternalLink size={14} className="shrink-0" />
            {t('openOriginal')}
          </button>
        )}
        {/* Manual bookmarks have no remote source: nothing to (re)download,
          and "delete local files" would destroy the only copy of the
          original (image/video) or leave a broken preview (pdf/file).
          Only "Elimina post" (below) applies to them. */}
        {localFiles && !hasLocalFiles && !isManual && (
          <button
            role="menuitem"
            data-testid="post-modal-download"
            onClick={handleDownload}
            disabled={downloadQueued || downloadEmpty}
            title={downloadEmpty ? t('noDownloadableFiles') : t('downloadFilesTitle')}
            className={`${row} text-[#cfcfcf] hover:bg-[#2a2a2a] hover:text-white disabled:opacity-50`}
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
        {localFiles && hasLocalFiles && !isManual && (
          <button
            role="menuitem"
            data-testid="post-modal-delete-local"
            onClick={handleDeleteLocal}
            disabled={deleting}
            title={deleteConfirm ? t('clickAgainToConfirm') : t('deleteLocalFilesTitle')}
            className={[
              row,
              'disabled:opacity-50',
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

        {bulkActions && (
          <>
            <div className="my-1 border-t border-[#2e2e2e]" />

            {/* Destructive: removes the post entirely (DB record + files). */}
            <button
              role="menuitem"
              data-testid="post-modal-delete-post"
              onClick={handleDeletePost}
              disabled={deletingPost}
              title={deletePostConfirm ? t('clickAgainToConfirm') : t('deletePostTitle')}
              className={[
                row,
                'disabled:opacity-50',
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
          </>
        )}

        {actionError && (
          <div
            role="alert"
            data-testid="action-error"
            className="px-3 py-2 text-xs text-red-300 border-t border-[#2e2e2e] u-fade-in"
          >
            {actionError}
          </div>
        )}
      </Popover>
    </>
  );
}
