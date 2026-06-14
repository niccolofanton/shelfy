import React, { useEffect, useMemo, useRef, useState } from 'react';
import {
  X,
  Bookmark,
  UploadCloud,
  FileText,
  Film,
  Image as ImageIcon,
  File as FileIcon,
  Loader,
  AlertCircle,
} from 'lucide-react';
import { useT } from '../i18n';
import {
  classifyFile,
  prepareFile,
  MAX_FILES,
  MAX_FILE_BYTES,
  MAX_TOTAL_BYTES,
} from '../lib/bookmarkFiles';

// Human-readable byte size for the file rows.
function fmtSize(bytes) {
  if (!bytes && bytes !== 0) return '';
  const u = ['B', 'KB', 'MB', 'GB'];
  let n = bytes;
  let i = 0;
  while (n >= 1024 && i < u.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(n >= 10 || i === 0 ? 0 : 1)} ${u[i]}`;
}

function KindIcon({ kind, size = 16 }) {
  const cls = 'text-[#b9a6ff] shrink-0';
  if (kind === 'image') return <ImageIcon size={size} className={cls} />;
  if (kind === 'video') return <Film size={size} className={cls} />;
  if (kind === 'pdf') return <FileText size={size} className={cls} />;
  return <FileIcon size={size} className={cls} />;
}

// One selected file, with a cheap inline preview (object URL for images, a kind
// glyph otherwise — full previews are generated only on submit).
function FileRow({ item, onRemove, removeLabel }) {
  return (
    <div className="flex items-center gap-2.5 rounded-md bg-[#0f0f0f] border border-[#262626] px-2.5 py-2">
      {item.thumbUrl ? (
        <img
          src={item.thumbUrl}
          alt=""
          className="w-9 h-9 rounded object-cover shrink-0 bg-[#1a1a1a]"
          draggable={false}
        />
      ) : (
        <div className="w-9 h-9 rounded bg-[#1a1a1a] flex items-center justify-center shrink-0">
          <KindIcon kind={item.kind} />
        </div>
      )}
      <div className="min-w-0 flex-1">
        <p className="text-[13px] text-gray-200 truncate">{item.file.name}</p>
        <p className="text-[11px] text-gray-500">{fmtSize(item.file.size)}</p>
      </div>
      <button
        onClick={onRemove}
        title={removeLabel}
        className="flex items-center justify-center w-7 h-7 rounded-md text-gray-500 hover:text-white hover:bg-[#2a2a2a] transition-colors u-press shrink-0"
      >
        <X size={14} />
      </button>
    </div>
  );
}

/**
 * AddBookmarkModal — add a manual bookmark: one or more local files
 * (images / videos / pdf / any) + a description and tags. Mirrors AddSiteModal's
 * chrome and fire-and-forget flow: on submit it builds each file's preview in the
 * renderer (src/lib/bookmarkFiles.js), ships bytes to the main process, and the
 * gallery picks the new post up via the shared `interceptor:newPosts` event.
 *
 * Props:
 *   onClose()        — dismiss the modal.
 *   onAdded(postId?) — fired once the bookmark is persisted.
 */
export default function AddBookmarkModal({ onClose, onAdded }) {
  const t = useT('addBookmark');
  const tc = useT('common');
  const [status, setStatus] = useState('idle'); // idle | working | error
  const [items, setItems] = useState([]); // [{ id, file, kind, thumbUrl }]
  const [note, setNote] = useState('');
  const [tags, setTags] = useState([]);
  const [tagDraft, setTagDraft] = useState('');
  const [dragging, setDragging] = useState(false);
  const [error, setError] = useState(null);
  const fileInputRef = useRef(null);
  const seq = useRef(0);
  const itemsRef = useRef(items);
  itemsRef.current = items;

  // Revoke the inline image preview URLs on unmount to avoid leaks. Read the
  // current list via the ref so the cleanup sees the final items, not the
  // empty array captured on first render.
  useEffect(() => {
    return () =>
      itemsRef.current.forEach((it) => it.thumbUrl && URL.revokeObjectURL(it.thumbUrl));
  }, []);

  useEffect(() => {
    const onKey = (e) => {
      if (e.key === 'Escape' && status !== 'working') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose, status]);

  function addFiles(fileList) {
    const incoming = Array.from(fileList || []);
    if (incoming.length === 0) return;
    setError(null);
    setItems((prev) => {
      const room = MAX_FILES - prev.length;
      if (room <= 0) {
        setError(t('tooManyFiles', { max: MAX_FILES }));
        return prev;
      }
      const next = [...prev];
      // Cumulative cap: all originals ship in a single IPC payload, so bound the
      // sum, not just each file (the main process enforces the same limit).
      let totalBytes = prev.reduce((sum, it) => sum + (it.file.size || 0), 0);
      for (const file of incoming.slice(0, room)) {
        if (file.size > MAX_FILE_BYTES) {
          setError(t('fileTooBig', { name: file.name }));
          continue;
        }
        if (totalBytes + file.size > MAX_TOTAL_BYTES) {
          setError(t('totalTooBig'));
          continue;
        }
        totalBytes += file.size;
        const kind = classifyFile(file);
        next.push({
          id: ++seq.current,
          file,
          kind,
          thumbUrl: kind === 'image' ? URL.createObjectURL(file) : null,
        });
      }
      if (incoming.length > room) setError(t('tooManyFiles', { max: MAX_FILES }));
      return next;
    });
  }

  function removeItem(id) {
    // Editing the selection clears any prior validation message (e.g. "too many
    // files"): leaving it up after the user removes items reads as a stale error.
    setError(null);
    setItems((prev) => {
      const target = prev.find((it) => it.id === id);
      if (target?.thumbUrl) URL.revokeObjectURL(target.thumbUrl);
      return prev.filter((it) => it.id !== id);
    });
  }

  function commitTag(raw) {
    const v = (raw || '').trim().replace(/,$/, '').trim();
    if (!v) return;
    setTags((prev) =>
      prev.some((t2) => t2.toLowerCase() === v.toLowerCase()) ? prev : [...prev, v],
    );
    setTagDraft('');
  }

  function onTagKeyDown(e) {
    if (e.key === 'Enter' || e.key === ',') {
      e.preventDefault();
      commitTag(tagDraft);
    } else if (e.key === 'Backspace' && !tagDraft && tags.length) {
      setTags((prev) => prev.slice(0, -1));
    }
  }

  function onDrop(e) {
    e.preventDefault();
    setDragging(false);
    if (status === 'working') return;
    addFiles(e.dataTransfer?.files);
  }

  const canSubmit = items.length > 0 && status !== 'working';

  async function handleSubmit() {
    if (!canSubmit) return;
    setStatus('working');
    setError(null);
    try {
      // Fold any uncommitted tag draft in so it isn't silently dropped.
      const finalTags = tagDraft.trim()
        ? [...tags, tagDraft.trim()].filter((v, i, a) => a.indexOf(v) === i)
        : tags;
      const files = [];
      for (const it of items) files.push(await prepareFile(it.file));
      const res = await window.electronAPI.addManualBookmark({
        note: note.trim(),
        tags: finalTags,
        files,
      });
      onAdded?.(res?.id);
    } catch (err) {
      console.error('[AddBookmarkModal] add error:', err);
      setError(err?.message || t('errorFallback'));
      setStatus('error');
    }
  }

  const dropLabel = useMemo(
    () => (items.length ? t('dropMore') : t('dropHint')),
    [items.length, t],
  );

  return (
    <div
      data-testid="add-bookmark-modal"
      className="fixed inset-0 bg-black/70 flex items-center justify-center z-[60] p-6 u-backdrop-in"
      onClick={() => status !== 'working' && onClose()}
    >
      <div
        className="bg-[#1a1a1a] border border-[#2e2e2e] rounded-xl shadow-2xl w-full max-w-md overflow-hidden u-dialog-in"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between px-5 h-12 border-b border-[#2e2e2e]">
          <span className="flex items-center gap-2 text-white text-sm font-semibold font-display">
            <Bookmark size={16} className="text-[#7B5CFF]" />
            {t('title')}
          </span>
          <button
            onClick={onClose}
            disabled={status === 'working'}
            title={tc('close')}
            className="flex items-center justify-center w-8 h-8 -mr-2 rounded-md text-[#a0a0a0] hover:text-white hover:bg-[#2a2a2a] transition-colors u-press disabled:opacity-40"
          >
            <X size={16} />
          </button>
        </div>

        <div key={status} className="u-swap-in">
          {status !== 'error' ? (
            <div className="px-5 py-5 space-y-4 max-h-[70vh] overflow-y-auto">
              <p className="text-[#888] text-sm leading-relaxed">{t('intro')}</p>

              {/* Non-fatal validation feedback from file selection/drop — kept
                  inline so the user can still adjust the selection. */}
              {error && status !== 'error' && (
                <p className="text-[13px] text-red-400 flex items-center gap-1.5">
                  <AlertCircle size={14} className="shrink-0" />
                  {error}
                </p>
              )}

              {/* Dropzone + browse */}
              <button
                type="button"
                onClick={() => fileInputRef.current?.click()}
                onDragOver={(e) => {
                  e.preventDefault();
                  setDragging(true);
                }}
                onDragLeave={() => setDragging(false)}
                onDrop={onDrop}
                className={[
                  'w-full flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed px-4 py-6 text-center transition-colors u-press',
                  dragging
                    ? 'border-[#7B5CFF] bg-[#7B5CFF]/10'
                    : 'border-[#3a3a3a] bg-[#0f0f0f] hover:border-[#555]',
                ].join(' ')}
              >
                <UploadCloud size={22} className="text-[#7B5CFF]" />
                <span className="text-sm text-gray-300">{dropLabel}</span>
                <span className="text-[11px] text-gray-600">{t('formats')}</span>
              </button>
              <input
                ref={fileInputRef}
                type="file"
                multiple
                onChange={(e) => {
                  addFiles(e.target.files);
                  e.target.value = '';
                }}
                className="hidden"
              />

              {items.length > 0 && (
                <div className="space-y-2">
                  {items.map((it) => (
                    <FileRow
                      key={it.id}
                      item={it}
                      removeLabel={tc('remove')}
                      onRemove={() => removeItem(it.id)}
                    />
                  ))}
                </div>
              )}

              {/* Description */}
              <div>
                <label className="block text-xs font-medium text-gray-400 mb-1.5">
                  {t('descLabel')}
                </label>
                <textarea
                  value={note}
                  onChange={(e) => setNote(e.target.value)}
                  rows={3}
                  placeholder={t('descPlaceholder')}
                  className="w-full resize-none bg-[#0f0f0f] border border-[#2e2e2e] rounded-md px-3 py-2 text-sm text-gray-200 placeholder-gray-600 outline-none focus:border-[#7B5CFF] transition-colors"
                />
              </div>

              {/* Tags */}
              <div>
                <label className="block text-xs font-medium text-gray-400 mb-1.5">
                  {t('tagsLabel')}
                </label>
                <div className="flex flex-wrap items-center gap-1.5 bg-[#0f0f0f] border border-[#2e2e2e] rounded-md px-2 py-1.5 focus-within:border-[#7B5CFF] transition-colors">
                  {tags.map((tag) => (
                    <span
                      key={tag}
                      className="inline-flex items-center gap-1 rounded bg-emerald-500/15 text-emerald-300 text-xs px-1.5 py-0.5"
                    >
                      {tag}
                      <button
                        onClick={() => setTags((prev) => prev.filter((x) => x !== tag))}
                        className="hover:text-white"
                      >
                        <X size={11} />
                      </button>
                    </span>
                  ))}
                  <input
                    value={tagDraft}
                    onChange={(e) => setTagDraft(e.target.value)}
                    onKeyDown={onTagKeyDown}
                    onBlur={() => commitTag(tagDraft)}
                    placeholder={tags.length ? '' : t('tagsPlaceholder')}
                    className="flex-1 min-w-[80px] bg-transparent text-sm text-gray-200 placeholder-gray-600 outline-none py-0.5"
                  />
                </div>
              </div>
            </div>
          ) : (
            <div className="px-5 py-5">
              <div className="flex flex-col items-center gap-3 py-3 mb-4 u-shake">
                <AlertCircle size={36} className="text-red-500 u-pop-in" />
                <p className="text-[#ccc] text-sm text-center">{error}</p>
              </div>
              <div className="flex flex-col gap-2">
                <button
                  onClick={() => {
                    setStatus('idle');
                    setError(null);
                  }}
                  className="w-full px-4 py-2 rounded-lg bg-white text-black text-sm font-medium hover:bg-[#e0e0e0] transition-colors u-press"
                >
                  {tc('retry')}
                </button>
                <button
                  onClick={onClose}
                  className="w-full px-4 py-2 rounded-lg text-[#888] hover:text-white transition-colors text-sm u-press"
                >
                  {tc('cancel')}
                </button>
              </div>
            </div>
          )}
        </div>

        {status !== 'error' && (
          <div className="flex items-center justify-between gap-2 px-5 py-3 border-t border-[#2e2e2e]">
            <span className="text-[11px] text-gray-600">
              {items.length > 0 ? t('fileCount', { count: items.length }) : ''}
            </span>
            <div className="flex items-center gap-2">
              <button
                onClick={onClose}
                disabled={status === 'working'}
                className="px-3.5 py-1.5 rounded-md text-sm text-gray-300 hover:text-white hover:bg-[#2a2a2a] transition-colors u-press disabled:opacity-40"
              >
                {tc('cancel')}
              </button>
              <button
                data-testid="add-bookmark-submit"
                onClick={handleSubmit}
                disabled={!canSubmit}
                className="flex items-center gap-2 px-4 py-1.5 rounded-md text-sm font-medium text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-40 disabled:cursor-not-allowed transition-[background-color,opacity] duration-200 u-press"
              >
                {status === 'working' && <Loader size={14} className="animate-spin" />}
                {status === 'working' ? t('working') : tc('add')}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
