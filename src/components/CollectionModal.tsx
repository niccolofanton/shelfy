import React, { useEffect, useRef, useState } from 'react';
import { X, Plus, Trash2 } from 'lucide-react';
import { useT } from '../i18n';

// Curated palette so custom sources stay visually consistent with the app.
export const COLLECTION_COLORS: string[] = [
  '#7B5CFF',
  '#e91e63',
  '#ff9800',
  '#4caf50',
  '#9c27b0',
  '#00bcd4',
  '#f44336',
  '#ffc107',
  '#8bc34a',
  '#607d8b',
];

// Lowercased palette for case-insensitive membership checks (the swatch list is
// stored mixed-case, e.g. '#7B5CFF'). Used consistently by both the "is this an
// off-palette custom colour?" check and the per-swatch selected check.
const COLLECTION_COLORS_LC: string[] = COLLECTION_COLORS.map((c) => c.toLowerCase());

type RadioTone = 'accent' | 'danger';

interface RadioDotProps {
  selected: boolean;
  tone?: RadioTone;
}

// Radio indicator for the delete-mode choice. `tone` colors it to match the
// option's severity (accent = safe, danger = destructive).
function RadioDot({ selected, tone = 'accent' }: RadioDotProps) {
  const ring = tone === 'danger' ? 'border-red-500' : 'border-[#7B5CFF]';
  const fill = tone === 'danger' ? 'bg-red-500' : 'bg-[#7B5CFF]';
  return (
    <span
      className={`mt-0.5 flex items-center justify-center w-4 h-4 rounded-full border shrink-0 u-transition ${
        selected ? ring : 'border-[#454545]'
      }`}
    >
      {selected && <span className={`w-2 h-2 rounded-full ${fill}`} />}
    </span>
  );
}

type DeleteMode = 'label' | 'posts';

// What the save handler consumes (mirrors App's CollectionDraft).
interface CollectionDraft {
  name: string;
  color: string;
}

interface CollectionModalProps {
  initial?: Shelfy.Collection;
  onClose: () => void;
  onSave: (draft: CollectionDraft) => Promise<unknown> | void;
  onDelete?: (
    id: number,
    opts: { deletePosts: boolean },
  ) => Promise<{ errors?: unknown[] } | void> | void;
  collections?: Shelfy.Collection[];
}

export default function CollectionModal({
  initial,
  onClose,
  onSave,
  onDelete,
  collections = [],
}: CollectionModalProps) {
  const t = useT('collectionModal');
  const tc = useT('common');
  const [name, setName] = useState<string>(initial?.name ?? '');
  const [color, setColor] = useState<string>(initial?.color ?? COLLECTION_COLORS[0]);
  const [saving, setSaving] = useState<boolean>(false);
  const [confirmingDelete, setConfirmingDelete] = useState<boolean>(false);
  // 'label' = remove only the tag (posts stay); 'posts' = also delete the posts.
  const [deleteMode, setDeleteMode] = useState<DeleteMode>('label');
  const [deleting, setDeleting] = useState<boolean>(false);
  // Surfaced feedback for failed save/delete (otherwise silently swallowed) and a
  // non-blocking warning when a delete partially fails (some files left on disk).
  const [error, setError] = useState<string>('');
  const [warning, setWarning] = useState<string>('');
  const inputRef = useRef<HTMLInputElement | null>(null);
  const postCount = initial?.count ?? 0;
  // Platform-tied collections (e.g. Instagram saved folders) are auto-derived and
  // recreated on the next sync, so deleting their posts is a hidden hazard.
  const isPlatform = !!(
    initial?.platform || (initial as { external_id?: string | null } | undefined)?.external_id
  );

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  const trimmed = name.trim();
  const isCustom = !COLLECTION_COLORS_LC.includes(color.toLowerCase());

  // Duplicate-name guard for manual (non-platform) collections: block creating a
  // second folder with a name that already exists (case-insensitive), and warn on
  // rename collisions too. Skipped for platform-tied collections (deduped by sync).
  const duplicate =
    !isPlatform &&
    !!trimmed &&
    collections.some(
      (c) =>
        c.id !== initial?.id &&
        !c.platform &&
        (c.name ?? '').trim().toLowerCase() === trimmed.toLowerCase(),
    );
  const canSave = trimmed.length > 0 && !saving && !duplicate;

  async function handleSave(): Promise<void> {
    if (!canSave) return;
    setError('');
    setSaving(true);
    try {
      await onSave({ name: trimmed, color });
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : t('saveError'));
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(): Promise<void> {
    if (!initial || deleting) return;
    setError('');
    setWarning('');
    setDeleting(true);
    try {
      const res = await onDelete?.(initial.id, { deletePosts: deleteMode === 'posts' });
      // The IPC handler returns { ok, deletedPosts, errors }; the collection is
      // already gone, but if some on-disk files couldn't be removed we keep the
      // modal open to surface a warning (the confirm button becomes "Chiudi").
      if (res?.errors?.length) {
        setWarning(t('deletePartial', { count: res.errors.length }));
        setDeleting(false);
        return;
      }
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : t('deleteError'));
      setDeleting(false);
    }
  }

  return (
    <div
      data-testid="collection-modal"
      className="fixed inset-0 bg-black/70 flex items-center justify-center z-[60] p-6 u-backdrop-in"
      onClick={onClose}
    >
      <div
        className="bg-[#1a1a1a] border border-[#2e2e2e] rounded-xl shadow-2xl w-full max-w-sm overflow-hidden u-dialog-in"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between px-5 h-12 border-b border-[#2e2e2e]">
          <span className="text-white text-sm font-semibold font-display">
            {initial ? t('titleEdit') : t('titleNew')}
          </span>
          <button
            onClick={onClose}
            title={tc('close')}
            className="flex items-center justify-center w-8 h-8 -mr-2 rounded-md text-[#a0a0a0] hover:text-white hover:bg-[#2a2a2a] transition-colors u-press"
          >
            <X size={16} />
          </button>
        </div>

        <div className="px-5 py-5 space-y-5">
          {/* Live preview of how the source appears in the sidebar */}
          <div className="flex items-center gap-2.5 px-3 py-2.5 rounded-lg bg-[#0f0f0f] border border-[#2e2e2e]">
            <span
              className="w-3 h-3 rounded-full shrink-0 u-transition"
              style={{ backgroundColor: color }}
            />
            <span className={`text-sm truncate ${trimmed ? 'text-gray-200' : 'text-gray-600'}`}>
              {trimmed || t('previewPlaceholder')}
            </span>
          </div>

          <div>
            <label className="block text-xs font-medium text-gray-400 mb-1.5">
              {t('nameLabel')}
            </label>
            <input
              ref={inputRef}
              data-testid="collection-name-input"
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') handleSave();
              }}
              placeholder={t('namePlaceholder')}
              maxLength={60}
              aria-invalid={duplicate}
              className={`w-full bg-[#0f0f0f] border rounded-md px-3 py-2 text-sm text-gray-200 placeholder-gray-600 outline-none transition-colors ${
                duplicate
                  ? 'border-amber-500/70 focus:border-amber-500'
                  : 'border-[#2e2e2e] focus:border-[#7B5CFF]'
              }`}
            />
            {duplicate && (
              <p data-testid="collection-name-duplicate" className="text-xs text-amber-400 mt-1.5">
                {t('duplicate')}
              </p>
            )}
          </div>

          <div>
            <label className="block text-xs font-medium text-gray-400 mb-2.5">
              {t('colorLabel')}
            </label>
            <div className="grid grid-cols-5 gap-y-3 justify-items-center">
              {COLLECTION_COLORS.map((c) => {
                const selected = color.toLowerCase() === c.toLowerCase();
                return (
                  <button
                    key={c}
                    data-testid={`collection-color-${c}`}
                    onClick={() => setColor(c)}
                    title={c}
                    aria-label={c}
                    aria-pressed={selected}
                    className={`relative w-7 h-7 rounded-full u-press u-transition ${selected ? 'scale-105' : 'hover:scale-110'}`}
                    style={{ backgroundColor: c }}
                  >
                    {selected && (
                      <span className="u-pop-in absolute inset-0 rounded-full ring-2 ring-inset ring-white" />
                    )}
                  </button>
                );
              })}

              {/* Custom color — opens the native picker; selected when off-palette */}
              <label
                data-testid="collection-color-custom"
                title={t('customColor')}
                className={`relative w-7 h-7 rounded-full cursor-pointer flex items-center justify-center u-press ${isCustom ? 'ring-2 ring-inset ring-white scale-105' : 'hover:scale-110'}`}
                style={{
                  background: isCustom
                    ? color
                    : 'conic-gradient(from 0deg, #f44336, #ff9800, #ffc107, #4caf50, #00bcd4, #7B5CFF, #9c27b0, #f44336)',
                }}
              >
                {!isCustom && (
                  <Plus size={14} className="text-white drop-shadow-[0_1px_1px_rgba(0,0,0,0.6)]" />
                )}
                <input
                  type="color"
                  value={color}
                  onChange={(e) => setColor(e.target.value)}
                  className="absolute inset-0 w-full h-full opacity-0 cursor-pointer"
                  aria-label={t('customColor')}
                />
              </label>
            </div>
          </div>
        </div>

        {/* Footer. The delete flow swaps the whole footer for a confirmation panel
            with an explicit choice (tag only vs. tag + posts) so a destructive
            delete can never be triggered by accident. */}
        {confirmingDelete && initial ? (
          <div
            className="px-5 py-4 border-t border-[#2e2e2e] u-fade-in"
            data-testid="collection-delete-confirm-panel"
          >
            <p className="text-sm font-medium text-gray-200">
              {t('deleteVerb')} <span className="text-white">«{initial.name}»</span>
            </p>
            <p className="text-xs text-gray-500 mt-0.5 mb-3">
              {isPlatform ? t('deletePlatformDesc') : t('deleteDesc')}
            </p>

            {/* Mutually-exclusive delete-mode choices exposed as a real radio group
                for screen readers + arrow-key navigation. The destructive
                "delete posts too" option is hidden for synced (platform) folders,
                whose posts are auto-recreated and must not be nuked from a view. */}
            <div role="radiogroup" aria-label={t('whatToRemove')} className="space-y-2">
              <button
                type="button"
                role="radio"
                aria-checked={deleteMode === 'label'}
                data-testid="collection-delete-mode-label"
                onClick={() => setDeleteMode('label')}
                onKeyDown={(e) => {
                  if (
                    (e.key === 'ArrowDown' || e.key === 'ArrowRight') &&
                    postCount > 0 &&
                    !isPlatform
                  )
                    setDeleteMode('posts');
                }}
                className={`w-full flex items-start gap-3 px-3 py-2.5 rounded-lg border text-left u-press u-transition ${
                  deleteMode === 'label'
                    ? 'border-[#7B5CFF] bg-[#7B5CFF]/10'
                    : 'border-[#2e2e2e] bg-[#0f0f0f] hover:border-[#3a3a3a]'
                }`}
              >
                <RadioDot selected={deleteMode === 'label'} tone="accent" />
                <span className="min-w-0">
                  <span className="block text-sm text-gray-200">{t('onlyLabel')}</span>
                  <span className="block text-xs text-gray-500">
                    {postCount > 0 ? t('postsRemain', { count: postCount }) : t('noLinkedPosts')}
                  </span>
                </span>
              </button>

              {postCount > 0 && !isPlatform && (
                <button
                  type="button"
                  role="radio"
                  aria-checked={deleteMode === 'posts'}
                  data-testid="collection-delete-mode-posts"
                  onClick={() => setDeleteMode('posts')}
                  onKeyDown={(e) => {
                    if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') setDeleteMode('label');
                  }}
                  className={`w-full flex items-start gap-3 px-3 py-2.5 rounded-lg border text-left u-press u-transition ${
                    deleteMode === 'posts'
                      ? 'border-red-500/70 bg-red-500/10'
                      : 'border-[#2e2e2e] bg-[#0f0f0f] hover:border-[#3a3a3a]'
                  }`}
                >
                  <RadioDot selected={deleteMode === 'posts'} tone="danger" />
                  <span className="min-w-0">
                    <span className="block text-sm text-gray-200">
                      {t('labelAndPosts', { count: postCount })}
                    </span>
                    <span className="block text-xs text-gray-500">{t('labelAndPostsDesc')}</span>
                  </span>
                </button>
              )}
            </div>

            {error && (
              <p
                data-testid="collection-delete-error"
                role="alert"
                className="text-xs text-red-400 mt-3"
              >
                {error}
              </p>
            )}
            {warning && (
              <p
                data-testid="collection-delete-warning"
                role="alert"
                className="text-xs text-amber-400 mt-3"
              >
                {warning}
              </p>
            )}

            <div className="flex items-center justify-end gap-2 mt-4">
              <button
                onClick={() => (warning ? onClose() : setConfirmingDelete(false))}
                disabled={deleting}
                className="px-3.5 py-1.5 rounded-md text-sm text-gray-300 hover:text-white hover:bg-[#2a2a2a] disabled:opacity-50 transition-colors u-press"
              >
                {tc('cancel')}
              </button>
              {warning ? (
                // The delete already happened; only the post-deletion partially
                // failed. Don't let the user re-trigger a delete on a gone row.
                <button
                  data-testid="collection-delete-done"
                  onClick={onClose}
                  className="px-4 py-1.5 rounded-md text-sm font-medium text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] transition-colors u-press"
                >
                  {tc('close')}
                </button>
              ) : (
                <button
                  data-testid="collection-delete-confirm"
                  onClick={handleDelete}
                  disabled={deleting}
                  className={`px-4 py-1.5 rounded-md text-sm font-medium text-white disabled:opacity-50 transition-colors u-press ${
                    deleteMode === 'posts'
                      ? 'bg-red-600 hover:bg-red-500'
                      : 'bg-[#7B5CFF] hover:bg-[#5A3DDE]'
                  }`}
                >
                  {deleting
                    ? t('deletingPosts')
                    : deleteMode === 'posts'
                      ? t('deleteLabelAndPosts', { count: postCount })
                      : t('deleteLabel')}
                </button>
              )}
            </div>
          </div>
        ) : (
          <>
            {error && (
              <p
                data-testid="collection-save-error"
                role="alert"
                className="px-5 -mt-1 mb-1 text-xs text-red-400"
              >
                {error}
              </p>
            )}
            <div className="flex items-center justify-end px-5 py-3 border-t border-[#2e2e2e]">
              {initial && onDelete && (
                <button
                  data-testid="collection-delete"
                  onClick={() => {
                    setDeleteMode('label');
                    setError('');
                    setWarning('');
                    setConfirmingDelete(true);
                  }}
                  title={t('deleteSourceTitle')}
                  className="flex items-center gap-1.5 mr-auto px-3 py-1.5 rounded-md text-sm text-gray-400 hover:text-red-400 hover:bg-[#2a2a2a] transition-colors u-press"
                >
                  <Trash2 size={14} />
                  {tc('delete')}
                </button>
              )}
              <button
                onClick={onClose}
                className="px-3.5 py-1.5 rounded-md text-sm text-gray-300 hover:text-white hover:bg-[#2a2a2a] transition-colors u-press"
              >
                {tc('cancel')}
              </button>
              <button
                data-testid="collection-save"
                onClick={handleSave}
                disabled={!canSave}
                className="ml-2 px-4 py-1.5 rounded-md text-sm font-medium text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-40 disabled:cursor-not-allowed transition-[background-color,opacity] duration-200 u-press"
              >
                {initial ? tc('save') : t('create')}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
