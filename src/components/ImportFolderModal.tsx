import React, { useEffect, useMemo, useRef, useState } from 'react';
import { X, Plus, Instagram, Tag, Loader, AlertCircle } from 'lucide-react';
import { COLLECTION_COLORS } from './CollectionModal';
import PinterestIcon from './PinterestIcon';
import { useT } from '../i18n';

type ImportMode = 'tag' | 'platform';

// onConfirm receives one of these payloads (see the JSDoc below).
export type ImportFolderResult =
  | { mode: 'platform' }
  | { mode: 'tag'; collectionId: number } // existing tag
  | { mode: 'tag'; name: string; color: string }; // create a new tag

interface ImportFolderModalProps {
  folderName?: string;
  igCollections?: Shelfy.Collection[];
  matchedId?: number | string | null;
  actionLabel?: string;
  platform?: Shelfy.Platform;
  onClose: () => void;
  onConfirm: (payload: ImportFolderResult) => Promise<unknown> | void;
}

// Shown when the user starts an import (full sync or "scarica selezionati") while
// browsing INSIDE an Instagram saved folder. Lets them choose the destination:
//   - "platform": import into Instagram with no tag;
//   - "tag": add the imported posts to a folder-tag — either an EXISTING IG
//     folder-tag (matched by folder id, rename-safe) or a NEW one whose name/color
//     are edited with the same UI as the sidebar's tag creation.
//
// onConfirm receives one of:
//   { mode: 'platform' }
//   { mode: 'tag', collectionId }                 // existing tag
//   { mode: 'tag', name, color }                  // create a new tag
//
// `igCollections` are the existing Instagram folder-tags (for the dropdown);
// `matchedId` is the tag already linked to THIS folder (preselected when present).
export default function ImportFolderModal({
  folderName,
  igCollections = [],
  matchedId = null,
  actionLabel,
  platform = 'instagram',
  onClose,
  onConfirm,
}: ImportFolderModalProps) {
  const t = useT('importFolder');
  const tc = useT('common');
  const isPin = platform === 'pinterest';
  const platformLabel = isPin ? 'Pinterest' : 'Instagram';
  const resolvedActionLabel = actionLabel ?? t('defaultAction');
  const suggestedName =
    (folderName || '').trim() || (isPin ? t('boardFallback') : t('folderFallback'));
  const [mode, setMode] = useState<ImportMode>('tag');
  // Existing-tag selection: the matched tag if any, else "new" (create).
  const [existingId, setExistingId] = useState<string>(
    matchedId != null ? String(matchedId) : 'new',
  );
  const [name, setName] = useState<string>(suggestedName);
  const [color, setColor] = useState<string>(COLLECTION_COLORS[0]);
  const [saving, setSaving] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const nameRef = useRef<HTMLInputElement | null>(null);

  const isNew = existingId === 'new';

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  // Focus the name field when creating a brand-new tag.
  useEffect(() => {
    if (mode === 'tag' && isNew) nameRef.current?.focus();
  }, [mode, isNew]);

  const selectedExisting = useMemo(
    () => igCollections.find((c) => String(c.id) === existingId) || null,
    [igCollections, existingId],
  );

  // Case-insensitive membership check: COLLECTION_COLORS is stored mixed-case
  // (e.g. '#7B5CFF'), so a strict includes() against color.toLowerCase() would
  // wrongly mark palette colours as custom.
  const isCustomColor = !COLLECTION_COLORS.some((c) => c.toLowerCase() === color.toLowerCase());
  const trimmed = name.trim();
  const canConfirm =
    !saving && (mode === 'platform' || (isNew ? trimmed.length > 0 : !!selectedExisting));

  async function handleConfirm(): Promise<void> {
    if (!canConfirm) return;
    setSaving(true);
    setError(null);
    try {
      let payload: ImportFolderResult;
      if (mode === 'platform') {
        payload = { mode: 'platform' };
      } else if (isNew) {
        payload = { mode: 'tag', name: trimmed, color };
      } else {
        payload = { mode: 'tag', collectionId: selectedExisting!.id };
      }
      await onConfirm(payload);
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : t('importFailed'));
    } finally {
      setSaving(false);
    }
  }

  // Preview label for the chosen destination (shown in the confirm button).
  const destLabel =
    mode === 'platform'
      ? platformLabel
      : isNew
        ? trimmed || suggestedName
        : selectedExisting?.name || t('tagFallback');

  return (
    <div
      data-testid="import-folder-modal"
      className="fixed inset-0 bg-black/70 flex items-center justify-center z-[60] p-6 u-backdrop-in"
      onClick={onClose}
    >
      <div
        className="bg-[#1a1a1a] border border-[#2e2e2e] rounded-xl shadow-2xl w-full max-w-sm overflow-hidden u-dialog-in"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between px-5 h-12 border-b border-[#2e2e2e]">
          <span className="text-white text-sm font-semibold font-display">
            {isPin ? t('titlePinterest') : t('titleInstagram')}
          </span>
          <button
            onClick={onClose}
            title={tc('close')}
            className="flex items-center justify-center w-8 h-8 -mr-2 rounded-md text-[#a0a0a0] hover:text-white hover:bg-[#2a2a2a] transition-colors u-press"
          >
            <X size={16} />
          </button>
        </div>

        <div className="px-5 py-5 space-y-4">
          <p className="text-xs text-gray-400 leading-relaxed">
            {t('leadBefore')} <span className="text-gray-200 font-medium">«{suggestedName}»</span>
            {t('leadAfter')}
          </p>

          {/* Destination choice */}
          <div className="space-y-2">
            <button
              data-testid="import-dest-platform"
              onClick={() => setMode('platform')}
              className={[
                'u-press w-full flex items-center gap-2.5 px-3 py-2.5 rounded-lg border text-left transition-colors',
                mode === 'platform'
                  ? 'border-[#7B5CFF] bg-[#7B5CFF]/10'
                  : 'border-[#2e2e2e] bg-[#0f0f0f] hover:border-[#3a3a3a]',
              ].join(' ')}
            >
              <span
                className={`flex items-center justify-center w-4 h-4 rounded-full border-2 shrink-0 transition-colors ${mode === 'platform' ? 'border-[#7B5CFF]' : 'border-[#4a4a4a]'}`}
              >
                {mode === 'platform' && (
                  <span className="u-pop-in w-2 h-2 rounded-full bg-[#7B5CFF]" />
                )}
              </span>
              {isPin ? (
                <PinterestIcon size={15} className="shrink-0" style={{ color: '#e60023' }} />
              ) : (
                <Instagram size={15} className="shrink-0 text-pink-400" />
              )}
              <span className="flex flex-col leading-tight">
                <span className="text-sm text-gray-200">
                  {t('platformOnly', { platform: platformLabel })}
                </span>
                <span className="text-[11px] text-gray-500">{t('platformOnlyHint')}</span>
              </span>
            </button>

            <button
              data-testid="import-dest-tag"
              onClick={() => setMode('tag')}
              className={[
                'u-press w-full flex items-center gap-2.5 px-3 py-2.5 rounded-lg border text-left transition-colors',
                mode === 'tag'
                  ? 'border-[#7B5CFF] bg-[#7B5CFF]/10'
                  : 'border-[#2e2e2e] bg-[#0f0f0f] hover:border-[#3a3a3a]',
              ].join(' ')}
            >
              <span
                className={`flex items-center justify-center w-4 h-4 rounded-full border-2 shrink-0 transition-colors ${mode === 'tag' ? 'border-[#7B5CFF]' : 'border-[#4a4a4a]'}`}
              >
                {mode === 'tag' && <span className="u-pop-in w-2 h-2 rounded-full bg-[#7B5CFF]" />}
              </span>
              <Tag size={15} className="shrink-0 text-[#b9a6ff]" />
              <span className="flex flex-col leading-tight">
                <span className="text-sm text-gray-200">{t('addToTag')}</span>
                <span className="text-[11px] text-gray-500">
                  {t('addToTagHint', { platform: platformLabel })}
                </span>
              </span>
            </button>
          </div>

          {/* Tag editor — only when the tag destination is chosen */}
          {mode === 'tag' && (
            <div className="u-fade-in space-y-4 rounded-lg border border-[#2e2e2e] bg-[#0f0f0f] px-3 py-3">
              {igCollections.length > 0 && (
                <div className="u-fade-in-up" style={{ animationDelay: '0ms' }}>
                  <label className="block text-xs font-medium text-gray-400 mb-1.5">
                    {t('existingTag')}
                  </label>
                  <select
                    data-testid="import-existing-select"
                    value={existingId}
                    onChange={(e) => setExistingId(e.target.value)}
                    className="w-full bg-[#1a1a1a] border border-[#2e2e2e] rounded-md px-3 py-2 text-sm text-gray-200 outline-none focus:border-[#7B5CFF] transition-colors"
                  >
                    <option value="new">{t('createNew')}</option>
                    {igCollections.map((c) => (
                      <option key={c.id} value={String(c.id)}>
                        {c.name}
                        {c.igName && c.igName !== c.name
                          ? ` ${t('wasNamed', { name: c.igName })}`
                          : ''}
                      </option>
                    ))}
                  </select>
                </div>
              )}

              {isNew ? (
                <>
                  {/* Live preview of how the tag appears in the sidebar */}
                  <div
                    className="u-fade-in-up flex items-center gap-2.5 px-3 py-2 rounded-lg bg-[#1a1a1a] border border-[#2e2e2e]"
                    style={{ animationDelay: '40ms' }}
                  >
                    <span
                      className="u-transition w-3 h-3 rounded-full shrink-0"
                      style={{ backgroundColor: color }}
                    />
                    <span
                      key={trimmed || '∅'}
                      className={`u-swap-in text-sm truncate ${trimmed ? 'text-gray-200' : 'text-gray-600'}`}
                    >
                      {trimmed || t('tagPreview')}
                    </span>
                  </div>

                  <div className="u-fade-in-up" style={{ animationDelay: '80ms' }}>
                    <label className="block text-xs font-medium text-gray-400 mb-1.5">
                      {t('tagName')}
                    </label>
                    <input
                      ref={nameRef}
                      data-testid="import-tag-name"
                      type="text"
                      value={name}
                      onChange={(e) => setName(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') handleConfirm();
                      }}
                      placeholder={t('tagNamePlaceholder')}
                      maxLength={60}
                      className="w-full bg-[#1a1a1a] border border-[#2e2e2e] rounded-md px-3 py-2 text-sm text-gray-200 placeholder-gray-600 outline-none focus:border-[#7B5CFF] transition-colors"
                    />
                  </div>

                  <div className="u-fade-in-up" style={{ animationDelay: '120ms' }}>
                    <label className="block text-xs font-medium text-gray-400 mb-2.5">
                      {t('color')}
                    </label>
                    <div className="grid grid-cols-5 gap-y-3 justify-items-center">
                      {COLLECTION_COLORS.map((c) => {
                        const selected = color.toLowerCase() === c.toLowerCase();
                        return (
                          <button
                            key={c}
                            onClick={() => setColor(c)}
                            title={c}
                            aria-label={c}
                            aria-pressed={selected}
                            className={`flex items-center justify-center w-7 h-7 rounded-full u-press u-transition ${selected ? 'ring-2 ring-inset ring-white scale-105' : 'hover:scale-110'}`}
                            style={{ backgroundColor: c }}
                          >
                            {selected && (
                              <span className="u-pop-in w-2 h-2 rounded-full bg-white shadow" />
                            )}
                          </button>
                        );
                      })}
                      <label
                        title={t('customColor')}
                        className={`relative w-7 h-7 rounded-full cursor-pointer flex items-center justify-center u-press ${isCustomColor ? 'ring-2 ring-inset ring-white scale-105' : 'hover:scale-110'}`}
                        style={{
                          background: isCustomColor
                            ? color
                            : 'conic-gradient(from 0deg, #f44336, #ff9800, #ffc107, #4caf50, #00bcd4, #7B5CFF, #9c27b0, #f44336)',
                        }}
                      >
                        {!isCustomColor && (
                          <Plus
                            size={14}
                            className="text-white drop-shadow-[0_1px_1px_rgba(0,0,0,0.6)]"
                          />
                        )}
                        <input
                          type="color"
                          value={isCustomColor ? color : '#ffffff'}
                          onChange={(e) => setColor(e.target.value)}
                          className="absolute inset-0 w-full h-full opacity-0 cursor-pointer"
                          aria-label={t('customColor')}
                        />
                      </label>
                    </div>
                  </div>
                </>
              ) : (
                <div
                  className="u-fade-in-up flex items-center gap-2.5 px-3 py-2 rounded-lg bg-[#1a1a1a] border border-[#2e2e2e]"
                  style={{ animationDelay: '40ms' }}
                >
                  <span
                    className="u-transition w-3 h-3 rounded-full shrink-0"
                    style={{ backgroundColor: selectedExisting?.color || '#7B5CFF' }}
                  />
                  <span
                    key={selectedExisting?.id}
                    className="u-swap-in text-sm text-gray-200 truncate flex-1"
                  >
                    {selectedExisting?.name}
                  </span>
                  <span className="text-xs text-gray-500 tabular-nums">
                    {selectedExisting?.count ?? 0}
                  </span>
                </div>
              )}
            </div>
          )}

          {error && (
            <div className="u-shake flex items-start gap-2 rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2.5">
              <AlertCircle size={16} className="text-red-500 shrink-0 mt-0.5" />
              <p className="text-xs text-red-300 leading-relaxed">{error}</p>
            </div>
          )}
        </div>

        <div className="flex justify-end gap-2 px-5 py-3 border-t border-[#2e2e2e]">
          <button
            onClick={onClose}
            className="px-3.5 py-1.5 rounded-md text-sm text-gray-300 hover:text-white hover:bg-[#2a2a2a] transition-colors u-press"
          >
            {tc('cancel')}
          </button>
          <button
            data-testid="import-folder-confirm"
            onClick={handleConfirm}
            disabled={!canConfirm}
            className={`flex items-center gap-2 px-4 py-1.5 rounded-md text-sm font-medium text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-40 disabled:cursor-not-allowed transition-colors u-press${saving ? ' u-glow' : ''}`}
            style={saving ? { color: '#fff' } : undefined}
          >
            {saving && <Loader size={14} className="u-spin shrink-0" />}
            <span key={`${resolvedActionLabel}-${destLabel}`} className="u-swap-in">
              {t('inDest', { action: resolvedActionLabel, dest: destLabel })}
            </span>
          </button>
        </div>
      </div>
    </div>
  );
}
