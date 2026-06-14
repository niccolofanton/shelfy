import React, { useEffect, useRef, useState } from 'react';
import {
  X,
  Sparkles,
  Loader2,
  Download,
  RotateCw,
  Pencil,
  Trash2,
  MoreHorizontal,
  StickyNote,
  Plus,
  Tags,
} from 'lucide-react';
import { useT } from '../../i18n';
import { useAnalysis } from '../../hooks/useAnalysis';
import Popover from '../Popover';
import type { ApplyAiFilter, PostUpdated } from './MetaColumn';

// ── Local shapes for the analyzer context (file-internal runtime records the IPC
// layer types as `unknown`; the consumers across the app narrow them locally). ──

// The live analyzer job surfaced by useAnalysis().jobFor(). Display-only.
interface AnalyzeJob {
  key: string;
  status: 'pending' | 'extracting' | 'analyzing' | 'done' | 'error' | 'cancelled';
  description?: string | null;
  tags?: string[] | null;
  authorUsername?: string | null;
  error?: string | null;
}

// VLM model availability / download progress (analyzer-internal shapes).
interface ModelStatus {
  ready?: boolean;
}
interface ModelProgress {
  progress?: number;
}

// Optimistic snapshot of the AI fields after a manual edit / clear.
interface AiOverride {
  description: string;
  tags: string[];
  saveReason: string;
}
// Optimistic snapshot of the user-authored layer (note + manual tags).
interface UserOverride {
  note: string;
  tags: string[];
}

// Maps an analyze-job status to its i18n key (resolved at render with `t`).
const ANALYZE_STATUS_KEY: Partial<Record<AnalyzeJob['status'], string>> = {
  pending: 'statusPending',
  extracting: 'statusExtracting',
  analyzing: 'statusAnalyzing',
};

interface SectionProps {
  children: React.ReactNode;
  action?: React.ReactNode;
}

// Card wrapper for the AI panel. Defined at module scope (not inside AiPanel) so
// its component identity stays stable across renders — otherwise every re-render
// (e.g. the frequent `jobs` updates while an analysis runs in the background)
// would remount the whole subtree and replay the u-fade-in / u-pop-in animations,
// causing a constant flicker.
function Section({ children, action }: SectionProps) {
  const t = useT('postModal');
  return (
    <div className="u-fade-in rounded-lg border border-[#2a2a2a] bg-[#121212] px-3 py-2.5 space-y-2">
      <div className="flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-wide text-[#7a7a7a]">
        <Sparkles size={12} className="text-violet-400" />
        {t('aiSection')}
        {action && <div className="ml-auto">{action}</div>}
      </div>
      {children}
    </div>
  );
}

interface UserLayerProps {
  note: string;
  manualTags: string[];
  onApplyAiFilter?: ApplyAiFilter;
  onChangeNote: (text: string) => Promise<void>;
  onChangeManualTags: (next: string[]) => Promise<void>;
}

// User-authored layer rendered inside the AI panel: the user's own manual tags
// (emerald, distinct from the violet AI tags, removable + inline-addable) and a
// free-text personal note (inline-editable). Both persist independently of the
// AI fields and survive an analysis regeneration. Defined at module scope (like
// Section) so its identity stays stable across the panel's frequent re-renders.
// Presentational: it owns only transient UI state; persistence happens in the
// parent via onChangeNote / onChangeManualTags.
function UserLayer({
  note,
  manualTags,
  onApplyAiFilter,
  onChangeNote,
  onChangeManualTags,
}: UserLayerProps) {
  const t = useT('postModal');
  const tc = useT('common');
  const tags = Array.isArray(manualTags) ? manualTags : [];

  // ── Manual tags: inline add / remove ──────────────────────────────────────
  const [tagInput, setTagInput] = useState<string>('');
  const [savingTags, setSavingTags] = useState<boolean>(false);

  const commitTags = async (next: string[]): Promise<void> => {
    setSavingTags(true);
    try {
      await onChangeManualTags(next);
    } catch (err) {
      console.error('[PostModal] onChangeManualTags error:', err);
    } finally {
      setSavingTags(false);
    }
  };

  const addTag = (raw: string): void => {
    const t = String(raw || '')
      .trim()
      .replace(/^#+/, '')
      .trim();
    setTagInput('');
    if (!t) return;
    if (tags.some((x) => x.toLowerCase() === t.toLowerCase())) return; // already present
    commitTags([...tags, t]);
  };
  const removeTag = (t: string): Promise<void> => commitTags(tags.filter((x) => x !== t));
  const handleTagKeyDown = (e: React.KeyboardEvent<HTMLInputElement>): void => {
    if (e.key === 'Enter' || e.key === ',') {
      e.preventDefault();
      addTag(tagInput);
    } else if (e.key === 'Backspace' && !tagInput && tags.length) {
      removeTag(tags[tags.length - 1]);
    }
  };

  // ── Personal note: inline edit ────────────────────────────────────────────
  const [editingNote, setEditingNote] = useState<boolean>(false);
  const [draftNote, setDraftNote] = useState<string>('');
  const [savingNote, setSavingNote] = useState<boolean>(false);
  const startNote = (): void => {
    setDraftNote(note || '');
    setEditingNote(true);
  };
  const saveNote = async (): Promise<void> => {
    setSavingNote(true);
    try {
      await onChangeNote(draftNote.trim());
      setEditingNote(false);
    } catch (err) {
      console.error('[PostModal] onChangeNote error:', err);
    } finally {
      setSavingNote(false);
    }
  };

  return (
    <div className="space-y-3 pt-2.5 mt-0.5 border-t border-[#242424]">
      {/* Manual tags — the user's own, distinct (emerald) from the AI tags */}
      <div className="space-y-1.5">
        <span className="flex items-center gap-1.5 text-[10px] uppercase tracking-wide text-[#6a6a6a]">
          <Tags size={11} className="text-emerald-400/80" /> {t('yourTags')}
        </span>
        <div className="flex flex-wrap items-center gap-1.5 rounded-md bg-[#161616] border border-[#242424] px-2 py-2">
          {tags.map((tag) => (
            <span
              key={tag}
              className="u-pop-in flex items-center gap-1 px-2 py-0.5 rounded-full bg-emerald-500/12 text-emerald-300 text-[11px]"
            >
              <button
                onClick={() => onApplyAiFilter?.({ tag: tag })}
                title={t('filterByTag')}
                className="u-press hover:text-emerald-100"
              >
                #{tag}
              </button>
              <button
                onClick={() => removeTag(tag)}
                title={t('removeTag')}
                disabled={savingTags}
                className="u-press text-emerald-300/60 hover:text-white disabled:opacity-50"
              >
                <X size={10} />
              </button>
            </span>
          ))}
          <input
            data-testid="post-modal-manual-tag-input"
            value={tagInput}
            onChange={(e) => setTagInput(e.target.value)}
            onKeyDown={handleTagKeyDown}
            onBlur={() => addTag(tagInput)}
            disabled={savingTags}
            placeholder={tags.length ? t('addTagShort') : t('addYourTag')}
            className="min-w-[120px] flex-1 bg-transparent text-[12px] text-emerald-100 placeholder:text-[#5a5a5a] focus:outline-none"
          />
        </div>
      </div>

      {/* Personal note — free text, inline editable */}
      <div className="space-y-1.5">
        <span className="flex items-center gap-1.5 text-[10px] uppercase tracking-wide text-[#6a6a6a]">
          <StickyNote size={11} className="text-emerald-400/80" /> {t('note')}
        </span>
        {editingNote ? (
          <div className="space-y-1.5">
            <textarea
              data-testid="post-modal-note-input"
              value={draftNote}
              onChange={(e) => setDraftNote(e.target.value)}
              rows={3}
              autoFocus
              placeholder={t('writeNote')}
              className="w-full rounded-md bg-[#1a1a1a] border border-[#2a2a2a] px-2.5 py-2 text-[13px] leading-relaxed text-[#e0e0e0] resize-y focus:outline-none focus:border-emerald-500/50"
            />
            <div className="flex items-center gap-2">
              <button
                data-testid="post-modal-note-save"
                onClick={saveNote}
                disabled={savingNote}
                className="u-press flex items-center gap-1.5 px-3 h-7 rounded-md text-[11px] bg-emerald-500/20 text-emerald-100 hover:bg-emerald-500/30 disabled:opacity-50"
              >
                {savingNote && <Loader2 size={12} className="animate-spin" />}
                {tc('save')}
              </button>
              <button
                onClick={() => setEditingNote(false)}
                disabled={savingNote}
                className="u-press px-3 h-7 rounded-md text-[11px] text-[#9a9a9a] hover:text-white"
              >
                {tc('cancel')}
              </button>
            </div>
          </div>
        ) : note ? (
          <p
            data-testid="post-modal-note"
            onClick={startNote}
            title={t('editNote')}
            className="u-press cursor-text whitespace-pre-wrap break-words rounded-md -mx-1 px-1 py-0.5 text-[13px] leading-relaxed text-[#d8d8d8] hover:bg-[#1a1a1a]"
          >
            {note}
          </p>
        ) : (
          <button
            data-testid="post-modal-note-add"
            onClick={startNote}
            className="u-press flex items-center gap-1.5 text-[12px] text-[#7a7a7a] hover:text-emerald-300"
          >
            <Plus size={12} /> {t('addNote')}
          </button>
        )}
      </div>
    </div>
  );
}

interface AiPanelProps {
  post: Shelfy.Post;
  onApplyAiFilter?: ApplyAiFilter;
  onPostUpdated?: PostUpdated;
  onEditingChange?: (editing: boolean) => void;
}

// Local VLM categorization for downloaded videos: shows the generated
// description + tags, or the controls to (download the model and) analyze.
export default function AiPanel({
  post,
  onApplyAiFilter,
  onPostUpdated,
  onEditingChange,
}: AiPanelProps) {
  const t = useT('postModal');
  const tc = useT('common');
  const {
    jobFor,
    modelStatus,
    modelProgress,
    analyzePost,
    retryJob,
    cancelJob,
    downloadModel,
    updatePostAiAnalysis,
    updatePostUserContent,
    clearPostDescriptions,
  } = useAnalysis();

  // Shown for any post with a local visual asset on disk (video, image,
  // thumbnail, or a downloaded carousel slide) — mirrors analyzer.canAnalyze.
  const hasLocalAsset =
    post.videoPath ||
    post.imagePath ||
    post.thumbnailPath ||
    (Array.isArray(post.media) && post.media.some((m) => m && m.localPath));

  const job = (jobFor(post.id) as AnalyzeJob | null) ?? null;
  const status = job?.status;
  const busy = status === 'pending' || status === 'extracting' || status === 'analyzing';

  // Local overrides applied after a manual edit, so the UI reflects saved values
  // without reloading the post from the DB.
  const [override, setOverride] = useState<AiOverride | null>(null);
  // Separate override for the user-authored layer (note + manual tags), so it can
  // be edited independently of the AI fields.
  const [userOverride, setUserOverride] = useState<UserOverride | null>(null);
  // Reset overrides when switching to a different post.
  useEffect(() => {
    setOverride(null);
    setUserOverride(null);
  }, [post.id]);

  const liveDescription = (status === 'done' && job?.description) || post.aiDescription;
  const liveTags = status === 'done' && job?.tags?.length ? job.tags : post.aiTags;
  const liveSaveReason = post.aiSaveReason;

  const description = override ? override.description : liveDescription;
  const tags = override ? override.tags : liveTags;
  const saveReason = override ? override.saveReason : liveSaveReason;
  const modelStatusTyped = modelStatus as ModelStatus | null;
  const modelProgressTyped = modelProgress as ModelProgress | null;
  const modelReady = modelStatusTyped?.ready;

  // User-authored layer values (with the same optimistic-override pattern).
  const userNote = userOverride ? userOverride.note : (post.userNote ?? '');
  const manualTags = userOverride ? userOverride.tags : post.userTags || [];

  // Functional override updates so a concurrent note/tags save preserves the
  // sibling field's *latest* value instead of clobbering it with a stale
  // render-time closure. The backend (updateUserContent) already merges per-field.
  const handleChangeNote = async (text: string): Promise<void> => {
    await updatePostUserContent(post.id, { note: text });
    setUserOverride((prev) => ({
      ...(prev ?? { note: post.userNote ?? '', tags: post.userTags || [] }),
      note: text,
    }));
    onPostUpdated?.(post.id, { userNote: text });
  };
  const handleChangeManualTags = async (arr: string[]): Promise<void> => {
    await updatePostUserContent(post.id, { manualTags: arr });
    setUserOverride((prev) => ({
      ...(prev ?? { note: post.userNote ?? '', tags: post.userTags || [] }),
      tags: arr,
    }));
    onPostUpdated?.(post.id, { userTags: arr });
  };

  // ── Manual edit mode ──────────────────────────────────────────────────────
  const [editing, setEditing] = useState<boolean>(false);
  const [draftDescription, setDraftDescription] = useState<string>('');
  const [draftTags, setDraftTags] = useState<string[]>([]);
  const [draftSaveReason, setDraftSaveReason] = useState<string>('');
  const [tagInput, setTagInput] = useState<string>('');
  // Tags currently playing their exit (u-pop-out) animation before removal.
  const [removingTags, setRemovingTags] = useState<Set<string>>(() => new Set());
  const [saving, setSaving] = useState<boolean>(false);
  // Two-step confirmation for deleting the AI description.
  const [confirmDeleteDesc, setConfirmDeleteDesc] = useState<boolean>(false);
  const [deletingDesc, setDeletingDesc] = useState<boolean>(false);
  // Two-step confirmation for clearing the AI tags (sibling of the description one,
  // mirrors the gallery's "Rimuovi tag AI" bulk action for a single post).
  const [confirmDeleteTags, setConfirmDeleteTags] = useState<boolean>(false);
  const [deletingTags, setDeletingTags] = useState<boolean>(false);
  // Overflow (⋯) menu holding the destructive AI actions, keeping the action row
  // uncluttered.
  const moreRef = useRef<HTMLDivElement | null>(null);
  const [moreOpen, setMoreOpen] = useState<boolean>(false);
  const closeMore = (): void => {
    setMoreOpen(false);
    setConfirmDeleteDesc(false);
    setConfirmDeleteTags(false);
  };
  // Reset the confirm prompts + overflow menu when switching post.
  useEffect(() => {
    setConfirmDeleteDesc(false);
    setConfirmDeleteTags(false);
    setMoreOpen(false);
  }, [post.id]);

  // Report to the parent whether an inline editor is active (with possible unsaved
  // drafts) so a backdrop click / Escape can guard against silently discarding it.
  // Mirror unmount as "not editing" so a stale dirty flag never blocks the next close.
  useEffect(() => {
    onEditingChange?.(editing);
  }, [editing, onEditingChange]);
  useEffect(() => () => onEditingChange?.(false), [onEditingChange]);

  const startEditing = (): void => {
    setDraftDescription(description || '');
    setDraftTags(Array.isArray(tags) ? [...tags] : []);
    setDraftSaveReason(saveReason || '');
    setTagInput('');
    setEditing(true);
  };

  const addTag = (raw: string): void => {
    const t = raw.trim();
    if (!t) return;
    // Re-adding a tag that is mid-removal-animation must keep it: cancel the
    // pending finalize first, otherwise onAnimationEnd -> finalizeRemoveTag would
    // later delete the chip the user just intended to keep.
    setRemovingTags((prev) => {
      if (!prev.has(t)) return prev;
      const next = new Set(prev);
      next.delete(t);
      return next;
    });
    setDraftTags((prev) => (prev.includes(t) ? prev : [...prev, t]));
    setTagInput('');
  };

  const handleTagKeyDown = (e: React.KeyboardEvent<HTMLInputElement>): void => {
    if (e.key === 'Enter' || e.key === ',') {
      e.preventDefault();
      addTag(tagInput);
    } else if (e.key === 'Backspace' && !tagInput && draftTags.length) {
      setDraftTags((prev) => prev.slice(0, -1));
    }
  };

  // Play the exit animation first, then drop the tag once it ends. Backspace
  // removal stays instant (no chip to animate by the time the key fires).
  const removeTag = (t: string): void => setRemovingTags((prev) => new Set(prev).add(t));
  const finalizeRemoveTag = (t: string): void => {
    setDraftTags((prev) => prev.filter((x) => x !== t));
    setRemovingTags((prev) => {
      if (!prev.has(t)) return prev;
      const next = new Set(prev);
      next.delete(t);
      return next;
    });
  };

  const handleSave = async (): Promise<void> => {
    setSaving(true);
    const fields: AiOverride = {
      description: draftDescription.trim(),
      tags: draftTags,
      saveReason: draftSaveReason.trim(),
    };
    try {
      await updatePostAiAnalysis(post.id, fields);
      // Optimistically reflect saved values in the local UI without mutating the
      // `post` prop; let the parent update its store immutably via onPostUpdated.
      setOverride(fields);
      onPostUpdated?.(post.id, {
        aiDescription: fields.description,
        aiTags: fields.tags,
        aiSaveReason: fields.saveReason,
      });
      setEditing(false);
    } catch (err) {
      console.error('[PostModal] updatePostAiAnalysis error:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleDeleteDescription = async (): Promise<void> => {
    if (typeof clearPostDescriptions !== 'function') {
      console.error(
        '[PostModal] clearPostDescriptions non disponibile — riavvia `npm run dev` (preload/main non fanno hot-reload).',
      );
      setConfirmDeleteDesc(false);
      return;
    }
    setDeletingDesc(true);
    try {
      await clearPostDescriptions([post.id]);
      // Reflect the cleared description locally without mutating the prop; keep
      // the current tags / save-reason intact.
      setOverride({
        description: '',
        tags: Array.isArray(tags) ? tags : [],
        saveReason: saveReason || '',
      });
      // Mirror the DB: description gone + status reset to "not analyzed", so the
      // post is eligible for "Analizza mancanti" regeneration.
      onPostUpdated?.(post.id, { aiDescription: '', aiStatus: null });
    } catch (err) {
      console.error('[PostModal] clearPostDescriptions error:', err);
    } finally {
      // Always leave the confirm prompt, even on failure, so the button never
      // gets stuck in the "Conferma/Annulla" state.
      setConfirmDeleteDesc(false);
      setDeletingDesc(false);
      setMoreOpen(false);
    }
  };

  const handleClearAiTags = async (): Promise<void> => {
    if (typeof window.electronAPI.clearPostAiTags !== 'function') {
      console.error(
        '[PostModal] clearPostAiTags non disponibile — riavvia `npm run dev` (preload/main non fanno hot-reload).',
      );
      setConfirmDeleteTags(false);
      return;
    }
    setDeletingTags(true);
    try {
      await window.electronAPI.clearPostAiTags([post.id]);
      // Reflect the cleared tags locally without mutating the prop; keep the
      // current description / save-reason intact.
      setOverride({
        description: description || '',
        tags: [],
        saveReason: saveReason || '',
      });
      // Mirror the DB (clearAiTags also resets ai_status → NULL): tags gone +
      // status back to "not analyzed", same as handleDeleteDescription above.
      onPostUpdated?.(post.id, { aiTags: [], aiStatus: null });
    } catch (err) {
      console.error('[PostModal] clearPostAiTags error:', err);
    } finally {
      setConfirmDeleteTags(false);
      setDeletingTags(false);
      setMoreOpen(false);
    }
  };

  // Also show the panel when the post already carries (or is producing) an AI
  // analysis, even without a local asset — mirrors the backend's text-only path
  // in canAnalyze(): a not-downloaded post can be tagged from its caption alone,
  // and those tags must stay visible here rather than be hidden by the gate.
  const hasExistingAnalysis =
    !!(description && description.trim()) ||
    (Array.isArray(tags) && tags.length > 0) ||
    !!post.aiStatus ||
    busy;

  // A not-downloaded video can't be (re)analyzed — this mirrors the backend's
  // canAnalyze(), which skips videos without their file on disk (cover thumbnail
  // / caption alone misrepresent a clip). Frontend proxy: no videoPath = not
  // downloaded. We still surface the panel to SHOW tags it may already carry,
  // but never offer the analyze/regenerate action for it.
  const videoNeedsDownload = post.mediaType === 'video' && !post.videoPath;

  if (!hasLocalAsset && !hasExistingAnalysis) return null;
  if (videoNeedsDownload && !hasExistingAnalysis) return null;

  const handleAnalyze = async (): Promise<void> => {
    // Drop any manual-edit/description-delete override so the regenerated analysis
    // (which lands via the live job/post values) is visible immediately — otherwise
    // the stale override would mask the fresh result until the user navigates away.
    setOverride(null);
    if (!modelReady) await downloadModel();
    analyzePost(post.id);
  };

  if (modelProgressTyped) {
    return (
      <Section>
        <div className="flex items-center gap-2 text-xs text-[#bdbdbd]">
          <Loader2 size={13} className="animate-spin text-violet-400" />
          {t('downloadingModel', {
            percent: Math.round((modelProgressTyped.progress || 0) * 100),
          })}
        </div>
        <div className="h-1 rounded-full bg-[#2a2a2a] overflow-hidden">
          <div
            className="h-full bg-violet-500 transition-all"
            style={{ width: `${Math.round((modelProgressTyped.progress || 0) * 100)}%` }}
          />
        </div>
      </Section>
    );
  }

  if (busy) {
    return (
      <Section>
        <div className="flex items-center justify-between gap-2">
          <span className="flex items-center gap-2 text-xs text-[#bdbdbd]">
            <Loader2 size={13} className="animate-spin text-violet-400" />
            {status && ANALYZE_STATUS_KEY[status] ? t(ANALYZE_STATUS_KEY[status]) : t('processing')}
          </span>
          <button
            onClick={() => job && cancelJob(job.key)}
            className="u-press text-[11px] text-[#9a9a9a] hover:text-white"
          >
            {tc('cancel')}
          </button>
        </div>
      </Section>
    );
  }

  if (description || tags?.length || editing) {
    const entities = post.aiEntities?.filter(Boolean) || [];
    const keywords = post.aiKeywords?.filter(Boolean) || [];

    // Manual edit mode: description textarea, chip tag editor, save_reason.
    if (editing) {
      return (
        <Section>
          <div className="space-y-1.5">
            <label className="text-[10px] uppercase tracking-wide text-[#6a6a6a]">
              {t('description')}
            </label>
            <textarea
              data-testid="post-modal-edit-description"
              value={draftDescription}
              onChange={(e) => setDraftDescription(e.target.value)}
              rows={4}
              className="w-full rounded-md bg-[#1a1a1a] border border-[#2a2a2a] px-2.5 py-2 text-[13px] text-[#e0e0e0] resize-y focus:outline-none focus:border-violet-500/60"
            />
          </div>

          <div className="space-y-1.5">
            <label className="text-[10px] uppercase tracking-wide text-[#6a6a6a]">
              {t('tags')}
            </label>
            <div className="flex flex-wrap gap-1.5 rounded-md bg-[#1a1a1a] border border-[#2a2a2a] px-2 py-2">
              {draftTags.map((tag) => {
                const leaving = removingTags.has(tag);
                return (
                  <span
                    key={tag}
                    onAnimationEnd={leaving ? () => finalizeRemoveTag(tag) : undefined}
                    className={
                      (leaving ? 'u-pop-out' : 'u-pop-in') +
                      ' flex items-center gap-1 px-2 py-0.5 rounded-full bg-violet-500/15 text-violet-200 text-[11px]'
                    }
                  >
                    #{tag}
                    <button
                      onClick={() => removeTag(tag)}
                      title={t('removeTag')}
                      className="u-press text-violet-300/70 hover:text-white"
                    >
                      <X size={11} />
                    </button>
                  </span>
                );
              })}
              <input
                data-testid="post-modal-edit-tag-input"
                value={tagInput}
                onChange={(e) => setTagInput(e.target.value)}
                onKeyDown={handleTagKeyDown}
                onBlur={() => addTag(tagInput)}
                placeholder={t('addTag')}
                className="flex-1 min-w-[100px] bg-transparent text-[12px] text-[#e0e0e0] placeholder:text-[#5a5a5a] focus:outline-none"
              />
            </div>
          </div>

          <div className="space-y-1.5">
            <label className="text-[10px] uppercase tracking-wide text-[#6a6a6a]">
              {t('whySave')}
            </label>
            <textarea
              data-testid="post-modal-edit-reason"
              value={draftSaveReason}
              onChange={(e) => setDraftSaveReason(e.target.value)}
              rows={2}
              className="w-full rounded-md bg-[#1a1a1a] border border-[#2a2a2a] px-2.5 py-2 text-[12px] text-[#cfcfcf] resize-y focus:outline-none focus:border-violet-500/60"
            />
          </div>

          <div className="flex items-center gap-2 pt-0.5">
            <button
              data-testid="post-modal-save"
              onClick={handleSave}
              disabled={saving}
              className="u-press flex items-center gap-1.5 px-3 h-7 rounded-md text-[11px] bg-violet-500/20 text-violet-100 hover:bg-violet-500/30 disabled:opacity-50"
            >
              {saving && <Loader2 size={12} className="animate-spin" />}
              {tc('save')}
            </button>
            <button
              data-testid="post-modal-cancel"
              onClick={() => setEditing(false)}
              disabled={saving}
              className="u-press px-3 h-7 rounded-md text-[11px] text-[#9a9a9a] hover:text-white"
            >
              {tc('cancel')}
            </button>
          </div>
        </Section>
      );
    }

    // Single menu item for a destructive AI action: first click arms the confirm
    // (red), second click runs it — keeps the two-step safety inside the menu.
    const menuItemBase =
      'u-press flex items-center gap-2.5 px-3 py-2 text-xs text-left disabled:opacity-50 transition-colors';
    return (
      <Section
        action={
          <div className="flex items-center gap-0.5">
            <button
              data-testid="post-modal-edit"
              onClick={startEditing}
              title={t('editAi')}
              className="u-press flex items-center justify-center w-6 h-6 rounded-md text-[#8a8a8a] hover:text-white hover:bg-[#2a2a2a]"
            >
              <Pencil size={13} />
            </button>
            {(description || (tags?.length ?? 0) > 0) && (
              <div ref={moreRef}>
                <button
                  data-testid="post-modal-ai-more"
                  onClick={() => (moreOpen ? closeMore() : setMoreOpen(true))}
                  aria-haspopup="menu"
                  aria-expanded={moreOpen}
                  title={t('moreActions')}
                  className="u-press flex items-center justify-center w-6 h-6 rounded-md text-[#8a8a8a] hover:text-white hover:bg-[#2a2a2a]"
                >
                  <MoreHorizontal size={15} />
                </button>
                <Popover
                  anchorRef={moreRef}
                  open={moreOpen}
                  onRequestClose={closeMore}
                  align="right"
                  placement="bottom"
                  className="min-w-[210px] bg-[#1f1f1f] border border-[#2e2e2e] rounded-lg shadow-2xl py-1 flex flex-col"
                >
                  <div role="menu" data-testid="post-modal-ai-menu">
                    {description && (
                      <button
                        data-testid="post-modal-delete-description"
                        onClick={() =>
                          confirmDeleteDesc ? handleDeleteDescription() : setConfirmDeleteDesc(true)
                        }
                        disabled={deletingDesc}
                        className={[
                          menuItemBase,
                          'w-full',
                          confirmDeleteDesc
                            ? 'bg-red-500/15 text-red-300 hover:bg-red-500/25'
                            : 'text-[#cfcfcf] hover:bg-[#2a2a2a] hover:text-white',
                        ].join(' ')}
                      >
                        {deletingDesc ? (
                          <Loader2 size={14} className="animate-spin shrink-0" />
                        ) : (
                          <Trash2 size={14} className="shrink-0" />
                        )}
                        {confirmDeleteDesc ? t('confirmDeleteDescription') : t('deleteDescription')}
                      </button>
                    )}
                    {(tags?.length ?? 0) > 0 && (
                      <button
                        data-testid="post-modal-clear-tags"
                        onClick={() =>
                          confirmDeleteTags ? handleClearAiTags() : setConfirmDeleteTags(true)
                        }
                        disabled={deletingTags}
                        className={[
                          menuItemBase,
                          'w-full',
                          confirmDeleteTags
                            ? 'bg-red-500/15 text-red-300 hover:bg-red-500/25'
                            : 'text-[#cfcfcf] hover:bg-[#2a2a2a] hover:text-white',
                        ].join(' ')}
                      >
                        {deletingTags ? (
                          <Loader2 size={14} className="animate-spin shrink-0" />
                        ) : (
                          <Tags size={14} className="shrink-0" />
                        )}
                        {confirmDeleteTags ? t('confirmRemoveAiTags') : t('removeAiTags')}
                      </button>
                    )}
                  </div>
                </Popover>
              </div>
            )}
          </div>
        }
      >
        {description && (
          <p className="text-[14px] leading-[1.65] text-[#dcdcdc] whitespace-pre-wrap break-words">
            {description}
          </p>
        )}

        {/* Tags — clickable */}
        {(tags?.length ?? 0) > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {(tags ?? []).map((tag, i) => (
              <button
                key={tag}
                onClick={() => onApplyAiFilter?.({ tag: tag })}
                title={t('filterByTag')}
                style={{ animationDelay: i * 30 + 'ms' }}
                className="u-pop-in u-press px-2 py-0.5 rounded-full bg-violet-500/12 text-violet-300 text-[11px] hover:bg-violet-500/25"
              >
                #{tag}
              </button>
            ))}
          </div>
        )}

        {/* Entities — non-clickable chips */}
        {entities.length > 0 && (
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="text-[10px] uppercase tracking-wide text-[#6a6a6a]">
              {t('entities')}
            </span>
            {entities.map((e) => (
              <span
                key={e}
                className="px-2 py-0.5 rounded-full bg-[#1f1f1f] text-[#aaa] text-[11px]"
              >
                {e}
              </span>
            ))}
          </div>
        )}

        {/* Keywords — discreet text */}
        {keywords.length > 0 && (
          <p className="text-[12px] leading-relaxed text-[#7a7a7a] break-words">
            <span className="text-[#6a6a6a]">{t('searchAlso')}</span>
            {keywords.join(', ')}
          </p>
        )}

        {/* Why save it */}
        {saveReason && (
          <p className="text-[12.5px] leading-relaxed text-[#9a9a9a] break-words">
            <span className="text-[#6a6a6a]">{t('whySavePrefix')}</span>
            {saveReason}
          </p>
        )}

        {!videoNeedsDownload && (
          <div className="flex items-center gap-3 pt-0.5">
            <button
              data-testid="post-modal-regenerate"
              onClick={handleAnalyze}
              className="u-press flex items-center gap-1.5 px-2.5 h-7 rounded-md text-[11px] bg-violet-500/15 text-violet-200 hover:bg-violet-500/25"
            >
              <RotateCw size={12} /> {t('regenerate')}
            </button>
          </div>
        )}

        <UserLayer
          note={userNote}
          manualTags={manualTags}
          onApplyAiFilter={onApplyAiFilter}
          onChangeNote={handleChangeNote}
          onChangeManualTags={handleChangeManualTags}
        />
      </Section>
    );
  }

  return (
    <Section>
      {status === 'error' && job?.error && (
        <p className="text-[11px] text-red-400/90 break-words">
          {t('errorPrefix', { error: job.error })}
        </p>
      )}
      <button
        data-testid="post-modal-analyze"
        onClick={status === 'error' ? () => job && retryJob(job.key) : handleAnalyze}
        className="u-press flex items-center gap-1.5 px-2.5 h-8 rounded-md text-xs bg-violet-500/15 text-violet-200 hover:bg-violet-500/25"
      >
        {modelReady ? <Sparkles size={14} /> : <Download size={14} />}
        {status === 'error'
          ? tc('retry')
          : modelReady
            ? t('analyzeVideo')
            : t('downloadModelAndAnalyze')}
      </button>

      {/* Manual tags + personal note are available even before any AI analysis. */}
      <UserLayer
        note={userNote}
        manualTags={manualTags}
        onApplyAiFilter={onApplyAiFilter}
        onChangeNote={handleChangeNote}
        onChangeManualTags={handleChangeManualTags}
      />
    </Section>
  );
}
