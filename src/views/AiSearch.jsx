import React, { useState, useEffect, useRef, useCallback } from 'react';
import VirtualPostGrid from '../components/VirtualPostGrid';
import GridSizeControl from '../components/GridSizeControl';
import PostGridSkeleton from '../components/PostGridSkeleton';
import PostModal from '../components/PostModal';
import CollectionModal from '../components/CollectionModal';
import Chip from '../components/Chip';
import { useAiSearch } from '../hooks/useAiSearch';
import { useDictation } from '../hooks/useDictation';
import { useToast } from '../hooks/useToast';
import { postsToMarkdown, downloadMarkdown, copyPostLinks } from '../lib/exportMarkdown';
import { useT, useLang, localeTag } from '../i18n';
import {
  Sparkles,
  Send,
  Square,
  RefreshCw,
  X,
  Download,
  FolderPlus,
  ClipboardCopy,
  FileDown,
  Check,
  Search as SearchIcon,
  Mic,
  Loader2,
  SquarePen,
  ListFilter,
} from 'lucide-react';

const ACCENT = '#7B5CFF';

// Suggestion-chip example queries — resolved per-language at render time via the
// `aiSearch` namespace (seed1…seed6). These are UI suggestions, not LLM prompts.
const SEED_PROMPT_KEYS = ['seed1', 'seed2', 'seed3', 'seed4', 'seed5', 'seed6'];

// ────────────────────────────────────────────────────────────────────────────
// Leaf components
// ────────────────────────────────────────────────────────────────────────────

function Toast({ children, closing }) {
  return (
    <div
      data-testid="aisearch-toast"
      role="status"
      aria-live="polite"
      className={[
        'fixed bottom-6 left-1/2 -translate-x-1/2 z-50 flex items-center gap-2 rounded-lg border border-[#2e2e2e] bg-[#1a1a1a] px-4 py-2.5 text-sm text-gray-200 shadow-2xl',
        closing ? 'u-fade-out' : 'u-fade-in-up',
      ].join(' ')}
    >
      <Check size={15} className="text-green-400" />
      {children}
    </div>
  );
}

function UserBubble({ content }) {
  return (
    <div data-testid="chat-message-user" className="flex justify-end u-fade-in-right">
      <div className="max-w-[85%] rounded-2xl bg-[#1a1a1a] px-3 py-2 text-sm text-gray-100 whitespace-pre-wrap break-words">
        {content}
      </div>
    </div>
  );
}

function TagChipRow({ tags, activeTags, onToggleTag }) {
  return (
    <div className="flex flex-wrap gap-1.5">
      {tags.map((t, i) => (
        <Chip
          key={t}
          data-testid="proposed-tag-chip"
          label={t}
          className="u-pop-in"
          style={{ animationDelay: `${i * 40}ms` }}
          active={activeTags.includes(t)}
          onClick={() => onToggleTag(t)}
        />
      ))}
    </div>
  );
}

function AssistantBubble({
  messageId,
  content,
  proposedTags,
  proposedGroups,
  isError,
  activeTags,
  activeKeywords,
  applied,
  onToggleTag,
  onToggleKeyword,
  onApply,
}) {
  const t = useT('aiSearch');
  // Prefer the two-tier layout (broad themes spaced apart from fine-grained
  // tags); fall back to a single flat row when groups aren't available.
  const broad = Array.isArray(proposedGroups?.broad) ? proposedGroups.broad : [];
  const specific = Array.isArray(proposedGroups?.specific) ? proposedGroups.specific : [];
  // Free-text keywords extracted from the user message — applied as text filters
  // (sky tone), toggled like tags.
  const keywords = Array.isArray(proposedGroups?.keywords) ? proposedGroups.keywords : [];
  const hasGroups = broad.length > 0 || specific.length > 0;
  const flat = Array.isArray(proposedTags) ? proposedTags : [];
  const hasContent = hasGroups || flat.length > 0 || keywords.length > 0;

  return (
    <div data-testid="chat-message-assistant" className="flex gap-2 u-fade-in-up">
      <Sparkles size={16} style={{ color: ACCENT }} className="mt-0.5 shrink-0" />
      <div className="min-w-0 space-y-2">
        <p
          className={[
            'text-sm whitespace-pre-wrap break-words',
            isError ? 'text-red-400' : 'text-gray-200',
          ].join(' ')}
        >
          {content}
        </p>
        {/* TAGS (violet) — archive tags that filter the tag index. */}
        {hasGroups ? (
          <div className="space-y-3">
            {broad.length > 0 && (
              <div className="space-y-1">
                <span className="block text-[10px] uppercase tracking-wide font-medium text-[#9b85ff]">
                  {t('broadTags')}
                </span>
                <TagChipRow tags={broad} activeTags={activeTags} onToggleTag={onToggleTag} />
              </div>
            )}
            {specific.length > 0 && (
              <div className="space-y-1">
                <span className="block text-[10px] uppercase tracking-wide font-medium text-[#9b85ff]">
                  {t('specificTags')}
                </span>
                <TagChipRow tags={specific} activeTags={activeTags} onToggleTag={onToggleTag} />
              </div>
            )}
          </div>
        ) : (
          flat.length > 0 && (
            <TagChipRow tags={flat} activeTags={activeTags} onToggleTag={onToggleTag} />
          )
        )}

        {/* KEYWORDS (sky) — free-text terms extracted from the message, matched
            against captions + AI descriptions. Toggled like tags, distinct colour. */}
        {keywords.length > 0 && (
          <div className="space-y-1">
            <span className="block text-[10px] uppercase tracking-wide font-medium text-[#7FD3F7]">
              {t('keywords')}
            </span>
            <div className="flex flex-wrap gap-1.5">
              {keywords.map((k, i) => (
                <Chip
                  key={k}
                  data-testid="proposed-keyword-chip"
                  label={k}
                  tone="sky"
                  className="u-pop-in"
                  style={{ animationDelay: `${i * 40}ms` }}
                  active={activeKeywords.includes(k)}
                  onClick={() => onToggleKeyword(k)}
                />
              ))}
            </div>
          </div>
        )}

        {/* Apply THIS message's tags + keywords as the active filter set (replaces, no sum) */}
        {hasContent &&
          (applied ? (
            <span
              data-testid="message-tags-applied"
              className="inline-flex items-center gap-1 text-[11px] text-green-400 font-medium select-none"
            >
              <Check size={12} /> {t('appliedToFilters')}
            </span>
          ) : (
            <button
              type="button"
              data-testid="apply-message-tags-btn"
              onClick={() => onApply(messageId)}
              title={t('applyToFiltersTitle')}
              className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md text-[11px] text-white bg-[#7B5CFF] hover:bg-[#6347e0] cursor-pointer u-press"
            >
              <ListFilter size={12} /> {t('applyToFilters')}
            </button>
          ))}
      </div>
    </div>
  );
}

// Skeleton placeholder pills shown while the model is still choosing tags. The
// widths are fixed so the shimmer reads as "tags loading here", not real chips.
const TAG_SKELETON_WIDTHS = [72, 104, 60, 88];

function StreamingBubble({ text }) {
  const t = useT('aiSearch');
  return (
    <div data-testid="chat-message-streaming" className="flex gap-2">
      <Sparkles size={16} style={{ color: ACCENT }} className="mt-0.5 shrink-0" />
      <div className="min-w-0 space-y-2">
        <p className="text-sm text-gray-200 whitespace-pre-wrap break-words">
          {text || t('searching')}
          <span className="streaming-cursor animate-pulse">▍</span>
        </p>
        <div data-testid="tag-skeletons" aria-hidden="true" className="flex flex-wrap gap-1.5">
          {TAG_SKELETON_WIDTHS.map((w, i) => (
            <span key={i} className="tag-skeleton" style={{ width: w }} />
          ))}
        </div>
      </div>
    </div>
  );
}

// Settled conversation history. Memoized so the per-token streamingText renders
// of the parent don't re-render every bubble: during a stream only the
// StreamingBubble leaf changes, while `messages` and the filter props keep
// their identities (the store only replaces them on real changes).
const MessageList = React.memo(function MessageList({
  messages,
  activeTags,
  activeKeywords,
  appliedMessageId,
  onToggleTag,
  onToggleKeyword,
  onApply,
}) {
  return messages.map((m) =>
    m.role === 'user' ? (
      <UserBubble key={m.id} content={m.content} />
    ) : (
      <AssistantBubble
        key={m.id}
        messageId={m.id}
        content={m.content}
        proposedTags={m.proposedTags}
        proposedGroups={m.proposedGroups}
        isError={m.isError}
        activeTags={activeTags}
        activeKeywords={activeKeywords}
        applied={appliedMessageId === m.id}
        onToggleTag={onToggleTag}
        onToggleKeyword={onToggleKeyword}
        onApply={onApply}
      />
    ),
  );
});

// Voice-model status banner — same flow as the classification model: an explicit
// download button with a percentage progress bar. Distinguishes "binary missing"
// (a dev/build prerequisite) from "model not downloaded yet".
function VoiceModelBanner({ modelStatus, modelProgress, onDownload }) {
  const t = useT('aiSearch');
  if (!modelStatus || modelStatus.ready) return null;

  if (modelProgress) {
    const pct = Math.round((modelProgress.progress || 0) * 100);
    return (
      <div
        data-testid="stt-model-downloading"
        className="flex items-start gap-2 border-b border-[#2e2e2e] bg-[#141414] px-3 py-2.5 u-fade-in-down"
      >
        <div className="flex-1 min-w-0">
          <p className="text-xs text-gray-300">{t('sttDownloading', { pct })}</p>
          <div className="mt-1.5 h-1 rounded-full bg-[#2e2e2e] overflow-hidden">
            <div className="h-full bg-[#7B5CFF] u-progress" style={{ width: `${pct}%` }} />
          </div>
        </div>
        <RefreshCw size={14} className="text-[#7B5CFF] animate-spin mt-0.5" />
      </div>
    );
  }

  if (modelStatus.binaryReady === false) {
    return (
      <div
        data-testid="stt-binary-missing"
        className="flex items-start gap-2 border-b border-[#2e2e2e] bg-[#141414] px-3 py-2.5 u-fade-in-down"
      >
        <div className="flex-1 min-w-0">
          <p className="text-xs text-gray-300">{t('sttUnavailable')}</p>
          <p className="text-[11px] text-gray-500 mt-0.5">
            {t('sttBinaryMissingPre')}
            <code className="text-gray-400">whisper-server</code>
            {t('sttBinaryMissingPost')}
            <code className="text-gray-400">.vlm/whisper/</code>.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div
      data-testid="stt-model-notready"
      className="flex items-start gap-2 border-b border-[#2e2e2e] bg-[#141414] px-3 py-2.5 u-fade-in-down"
    >
      <div className="flex-1 min-w-0">
        <p className="text-xs text-gray-300">{t('sttModelUnavailable')}</p>
        <p className="text-[11px] text-gray-500 mt-0.5">{t('sttDownloadHint')}</p>
      </div>
      <button
        data-testid="stt-download-btn"
        onClick={onDownload}
        className="shrink-0 flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] u-press"
      >
        <Download size={13} /> {t('sttDownloadBtn')}
      </button>
    </div>
  );
}

// Mic button with an audio-reactive glow. It subscribes to the dictation audio
// level (a ref-backed stream, ~10Hz) IN ISOLATION so the high-frequency updates
// re-render only this tiny leaf — not the whole AiSearch view and its results
// grid. When not recording it ignores the stream entirely.
function MicButton({ dictation, isRecording, isBusyDict, micDisabled, modelReadyForVoice }) {
  const t = useT('aiSearch');
  const [level, setLevel] = useState(0);
  useEffect(() => {
    if (!isRecording) {
      setLevel(0);
      return undefined;
    }
    setLevel(dictation.getAudioLevel?.() || 0);
    return dictation.subscribeAudioLevel?.((l) => setLevel(l));
  }, [isRecording, dictation]);

  const spread = isRecording ? Math.round(Math.min(7, level * 45)) : 0;
  const glowAlpha = isRecording ? Math.min(0.5, level * 3).toFixed(2) : '0';
  const iconScale = isRecording ? (1 + Math.min(0.4, level * 2)).toFixed(2) : '1';

  return (
    <button
      data-testid="chat-mic-btn"
      onClick={dictation.toggle}
      disabled={micDisabled}
      aria-label={isRecording ? t('micStop') : t('micStart')}
      title={
        !modelReadyForVoice && !isRecording
          ? t('micModelUnavailable')
          : isRecording
            ? t('micStop')
            : t('micStart')
      }
      className={[
        'shrink-0 flex items-center justify-center w-7 h-7 rounded-lg disabled:opacity-30 u-press',
        isRecording
          ? 'bg-[#7B5CFF]/20 text-[#7B5CFF]'
          : isBusyDict
            ? 'text-gray-600 bg-transparent'
            : 'text-gray-500 hover:text-gray-200 hover:bg-[#222]',
      ].join(' ')}
      style={
        isRecording
          ? {
              boxShadow: `0 0 0 ${spread}px rgba(123,92,255,${glowAlpha})`,
              transition: 'box-shadow 60ms ease-out',
            }
          : undefined
      }
    >
      {isBusyDict ? (
        <Loader2 size={13} className="animate-spin" />
      ) : (
        <Mic
          size={13}
          style={
            isRecording
              ? {
                  transform: `scale(${iconScale})`,
                  transition: 'transform 60ms ease-out',
                }
              : undefined
          }
        />
      )}
    </button>
  );
}

// Right column: filter toolbar + virtualized results grid. Memoized so the
// per-token streamingText renders of the parent don't touch it (every prop —
// store arrays, stable actions, useCallback handlers — keeps its identity
// while a reply streams), keeping the chat stream free of grid reconciliation
// and of the forced reflows its layout effects would trigger.
const ResultsPane = React.memo(function ResultsPane({
  results,
  total,
  resultsLoading,
  activeTags,
  activeKeywords,
  tagMode,
  source,
  setTagMode,
  setSource,
  removeTag,
  removeKeyword,
  clearFilters,
  onPromoteCollection,
  onCopyLinks,
  onExportMarkdown,
  onOpen,
  scrollRef,
}) {
  const t = useT('aiSearch');
  const { lang } = useLang();
  const hasActiveFilters = activeTags.length > 0 || activeKeywords.length > 0;

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      {/* Sticky toolbar */}
      <div className="sticky top-0 z-10 border-b border-[#2e2e2e] bg-[#0f0f0f] px-3 py-2 flex flex-col justify-center gap-2 min-h-[52px]">
        <div className="flex items-center gap-2 flex-wrap">
          {/* AND/OR toggle — disabled with fewer than 2 tags */}
          <div className="inline-flex rounded-md border border-[#2e2e2e] overflow-hidden">
            {['and', 'or'].map((m) => (
              <button
                key={m}
                onClick={() => setTagMode(m)}
                disabled={activeTags.length < 2}
                className={[
                  'px-2.5 py-1 text-xs uppercase u-press disabled:opacity-40',
                  tagMode === m ? 'bg-[#7B5CFF] text-white' : 'text-gray-400 hover:bg-[#1a1a1a]',
                ].join(' ')}
              >
                {m}
              </button>
            ))}
          </div>

          {/* Source scope — Tutto / Siti / Social */}
          <div className="inline-flex rounded-md border border-[#2e2e2e] overflow-hidden">
            {[
              ['all', t('sourceAll')],
              ['web', t('sourceWeb')],
              ['social', t('sourceSocial')],
            ].map(([val, label]) => (
              <button
                key={val}
                data-testid={`source-${val}`}
                onClick={() => setSource(val)}
                className={[
                  'px-2.5 py-1 text-xs u-press',
                  source === val ? 'bg-[#7B5CFF] text-white' : 'text-gray-400 hover:bg-[#1a1a1a]',
                ].join(' ')}
              >
                {label}
              </button>
            ))}
          </div>

          {/* Active tags (violet) */}
          {activeTags.map((t) => (
            <Chip
              key={t}
              data-testid="active-tag-chip"
              label={t}
              active
              onRemove={() => removeTag(t)}
            />
          ))}

          {/* Active keywords (sky) */}
          {activeKeywords.map((k) => (
            <Chip
              key={`kw-${k}`}
              data-testid="active-keyword-chip"
              label={k}
              tone="sky"
              active
              onRemove={() => removeKeyword(k)}
            />
          ))}

          {hasActiveFilters && (
            <button
              onClick={clearFilters}
              className="text-xs text-gray-500 hover:text-gray-200 u-press"
            >
              {t('clear')}
            </button>
          )}

          <div className="flex-1" />

          <span data-testid="search-result-count" className="text-xs text-gray-400 tabular-nums">
            {hasActiveFilters
              ? // Only the first RESULT_LIMIT posts are loaded (no pagination yet,
                // see crossFile note). Show the loaded count honestly when capped
                // rather than implying all `total` are reachable.
                total > results.length
                ? t('firstNofTotal', {
                    n: results.length,
                    total: total.toLocaleString(localeTag(lang)),
                  })
                : t('nResults', { total: total.toLocaleString(localeTag(lang)) })
              : t('noActiveFilter')}
          </span>

          {results.length > 0 && <GridSizeControl />}
        </div>

        {/* Result actions — these operate on the loaded slice (capped at
            RESULT_LIMIT), not the full matched `total`. When the set is capped
            the label says so to avoid silently acting on fewer posts than the
            advertised count. */}
        {results.length > 0 && (
          <div className="flex items-center gap-2 flex-wrap">
            <button
              data-testid="search-promote-collection-btn"
              onClick={onPromoteCollection}
              className="flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-50 u-press"
            >
              <FolderPlus size={13} /> {t('createCollection')}
            </button>
            <button
              onClick={onCopyLinks}
              className="flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs text-gray-300 bg-[#1a1a1a] hover:bg-[#222] u-press"
            >
              <ClipboardCopy size={13} /> {t('copyLinks')}
            </button>
            <button
              onClick={onExportMarkdown}
              className="flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs text-gray-300 bg-[#1a1a1a] hover:bg-[#222] u-press"
            >
              <FileDown size={13} /> {t('exportMarkdown')}
            </button>
            {total > results.length && (
              <span className="text-[11px] text-gray-500 tabular-nums">
                {t('nOfTotalShown', {
                  n: results.length,
                  total: total.toLocaleString(localeTag(lang)),
                })}
              </span>
            )}
          </div>
        )}
      </div>

      {/* Gallery body */}
      <div
        ref={scrollRef}
        className="flex-1 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e]"
      >
        {resultsLoading && results.length === 0 && (
          <div data-testid="search-loading">
            <PostGridSkeleton />
          </div>
        )}

        {!resultsLoading && !hasActiveFilters && results.length === 0 && (
          <div
            data-testid="search-empty-state"
            className="flex flex-col items-center justify-center h-full min-h-[40vh] gap-2 text-center px-6 u-fade-in"
          >
            <SearchIcon size={32} className="text-[#333]" strokeWidth={1} />
            <p className="text-[#555] text-sm max-w-xs">{t('galleryEmpty')}</p>
          </div>
        )}

        {!resultsLoading && hasActiveFilters && results.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full min-h-[40vh] gap-2 text-center px-6 u-fade-in">
            <X size={32} className="text-[#333]" strokeWidth={1} />
            <p className="text-[#555] text-sm">{t('noResults')}</p>
          </div>
        )}

        {results.length > 0 && (
          <VirtualPostGrid
            testId="search-gallery-grid"
            posts={results}
            scrollRef={scrollRef}
            onOpen={onOpen}
          />
        )}
      </div>
    </div>
  );
});

// ────────────────────────────────────────────────────────────────────────────
// Main view — self-contained; the only props are the App-level web-post hooks
// (jump to the Websites panel / re-run a web capture) threaded to the PostModal.
// ────────────────────────────────────────────────────────────────────────────

export default function AiSearch({ onOpenInWebsites, onReanalyzeWeb }) {
  const t = useT('aiSearch');
  const td = useT('dictation');
  const {
    messages,
    activeTags,
    activeKeywords,
    appliedMessageId,
    tagMode,
    source,
    results,
    total,
    chatLoading,
    streamingText,
    resultsLoading,
    modelStatus,
    actions,
  } = useAiSearch();

  const {
    sendMessage,
    toggleTag,
    removeTag,
    toggleKeyword,
    removeKeyword,
    applyMessageTags,
    setTagMode,
    setSource,
    clearFilters,
    stopStreaming,
    downloadModel,
    refresh,
    reset,
  } = actions;

  const [draft, setDraft] = useState('');
  const { toast, toastClosing, showToast } = useToast();
  const [activePost, setActivePost] = useState(null);
  // Collection-creation modal (replaces window.prompt): holds the post ids
  // captured when the user pressed "Crea collection da questi".
  const [collectionModal, setCollectionModal] = useState(null); // { ids } | null
  // Scroll container for the (row-virtualized) results grid.
  const resultsScrollRef = useRef(null);

  // ── Voice dictation: transcript populates the draft; user reviews & sends ──
  const handleDictationResult = useCallback((text) => {
    const clean = (text || '').replace(/\n+/g, ' ').trim();
    if (!clean) return;
    setDraft((d) => (d.trim() ? `${d.trim()} ${clean}` : clean));
  }, []);
  const dictation = useDictation({ onResult: handleDictationResult, language: 'it' });

  // ── Detail modal: open on card click + step through the current results ────
  const goPrevPost = useCallback(() => {
    setActivePost((cur) => {
      if (!cur) return cur;
      const i = results.findIndex((p) => p.id === cur.id);
      return i > 0 ? results[i - 1] : cur;
    });
  }, [results]);
  const goNextPost = useCallback(() => {
    setActivePost((cur) => {
      if (!cur) return cur;
      const i = results.findIndex((p) => p.id === cur.id);
      return i >= 0 && i < results.length - 1 ? results[i + 1] : cur;
    });
  }, [results]);

  // ── Auto-scroll the chat log to the bottom on new content ──────────────────
  // Coalesced per animation frame: streamed tokens can land several times per
  // frame, and reading scrollHeight forces a layout each time — one rAF batches
  // the read+write to at most once per painted frame.
  const logRef = useRef(null);
  const logScrollRaf = useRef(0);
  useEffect(() => {
    if (logScrollRaf.current) return;
    logScrollRaf.current = requestAnimationFrame(() => {
      logScrollRaf.current = 0;
      const el = logRef.current;
      if (el) el.scrollTop = el.scrollHeight;
    });
  }, [messages, streamingText, chatLoading]);
  useEffect(() => () => cancelAnimationFrame(logScrollRaf.current), []);

  const modelReady = !!modelStatus?.ready;
  const modelDownloading = !!modelStatus?.downloading;

  // ── Chat input handlers ────────────────────────────────────────────────────
  // Sending while dictating stops the recording, folds the final transcript into
  // the draft, and sends in one step — no need to press the stop button first.
  const handleSend = useCallback(async () => {
    if (chatLoading) return;
    let text = draft.trim();
    if (dictation.status === 'recording') {
      const dictated = await dictation.stop({ silent: true });
      text = [text, (dictated || '').trim()].filter(Boolean).join(' ');
    }
    if (!text) return;
    setDraft('');
    sendMessage(text);
  }, [draft, chatLoading, sendMessage, dictation]);

  const handleReset = useCallback(() => {
    if (dictation.isActive) dictation.stop({ silent: true });
    reset();
    setDraft('');
  }, [dictation, reset]);

  const handleSeed = useCallback(
    (text) => {
      if (chatLoading) return;
      sendMessage(text);
    },
    [chatLoading, sendMessage],
  );

  const onInputKeyDown = useCallback(
    (e) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        handleSend();
      }
    },
    [handleSend],
  );

  // ── Result actions (mirrors AiTags) ────────────────────────────────────────
  const handleCopyLinks = useCallback(async () => {
    const { status, n } = await copyPostLinks(results);
    if (status === 'empty') showToast(t('noLinksToCopy'));
    else if (status === 'copied') showToast(t('linksCopied', { n }));
    else showToast(t('copyFailed'));
  }, [results, showToast, t]);

  const handleExportMarkdown = useCallback(() => {
    if (results.length === 0) {
      showToast(t('nothingToExport'));
      return;
    }
    downloadMarkdown(postsToMarkdown(results), `shelfy-aisearch-${Date.now()}.md`);
    showToast(t('exported', { n: results.length }));
  }, [results, showToast, t]);

  // Opens the app's collection modal seeded with the loaded result ids. NOTE:
  // this operates on the loaded `results` slice (capped at RESULT_LIMIT), not the
  // full matched `total` — the label below makes that explicit.
  const handlePromoteCollection = useCallback(() => {
    const ids = results.map((p) => p.id);
    if (ids.length === 0) {
      showToast(t('nothingToCollect'));
      return;
    }
    setCollectionModal({ ids });
  }, [results, showToast, t]);

  const handleCreateCollection = useCallback(
    async ({ name, color }) => {
      const ids = collectionModal?.ids ?? [];
      // createCollection is positional (name, color) — see preload.js.
      const created = await window.electronAPI.createCollection(name, color || ACCENT);
      if (!created?.id) {
        showToast(t('collectionError'));
        return;
      }
      const res = await window.electronAPI.addPostsToCollections(ids, [created.id]);
      // Trust the backend count; show the number only when it's a real value.
      showToast(
        typeof res?.added === 'number'
          ? t('collectionCreatedWithPosts', { name, n: res.added })
          : t('collectionCreated', { name }),
      );
    },
    [collectionModal, showToast, t],
  );

  const hasMessages = messages.length > 0;

  // ── Textarea auto-resize (1 row → max 6 rows) ────────────────────────────
  const textareaRef = useRef(null);
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, 168)}px`;
  }, [draft, dictation.liveText]);

  // ── Dictation state shorthands ─────────────────────────────────────────────
  const isRecording = dictation.status === 'recording';
  const isBusyDict = dictation.status === 'requesting' || dictation.status === 'transcribing';
  // The composer value is forced to the live overlay while recording OR busy, so
  // lock editing in both states to keep the affordance honest.
  const inputLocked = isRecording || isBusyDict;
  const modelReadyForVoice = !!dictation.modelStatus?.ready;
  const micDisabled = chatLoading || isBusyDict || (!isRecording && !modelReadyForVoice);
  // While recording OR transcribing, overlay the live text so the content
  // never disappears during the gap between stop and handleDictationResult firing.
  // Newlines from whisper are collapsed to spaces.
  const cleanLive = (dictation.liveText || '').replace(/\n+/g, ' ').trim();
  const displayDraft =
    isRecording || isBusyDict ? (draft.trim() ? `${draft.trim()} ${cleanLive}` : cleanLive) : draft;

  return (
    <div data-testid="aisearch-view" className="flex h-full overflow-hidden">
      {/* ── Left: chat panel ──────────────────────────────────────────────── */}
      <div className="w-[420px] min-w-[420px] border-r border-[#2e2e2e] flex flex-col bg-[#0f0f0f]">
        {/* Header */}
        <div className="flex items-center gap-2 px-4 h-[52px] shrink-0 border-b border-[#2e2e2e]">
          <Sparkles size={16} style={{ color: ACCENT }} />
          <h1 className="text-white text-sm font-semibold tracking-tight">{t('chatTitle')}</h1>
          <div className="flex-1" />
          {hasMessages && (
            <button
              data-testid="chat-reset-btn"
              onClick={handleReset}
              aria-label={t('newConversation')}
              title={t('newConversation')}
              className="shrink-0 flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg text-xs text-gray-500 hover:text-gray-200 hover:bg-[#1a1a1a] border border-transparent hover:border-[#2e2e2e] u-press"
            >
              <SquarePen size={13} /> {t('newConversationShort')}
            </button>
          )}
        </div>

        {/* Model status banner */}
        {!modelReady && (
          <div
            data-testid={modelDownloading ? 'model-status-downloading' : 'model-status-notready'}
            className="flex items-start gap-2 border-b border-[#2e2e2e] bg-[#141414] px-3 py-2.5 u-fade-in-down"
          >
            <div className="flex-1 min-w-0">
              <p className="text-xs text-gray-300">
                {modelDownloading ? t('modelDownloading') : t('modelUnavailable')}
              </p>
              <p className="text-[11px] text-gray-500 mt-0.5">{t('modelTextFallback')}</p>
            </div>
            {!modelDownloading && (
              <button
                data-testid="model-download-btn"
                onClick={downloadModel}
                className="shrink-0 flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] u-press"
              >
                <Download size={13} /> {t('downloadModel')}
              </button>
            )}
            {modelDownloading && (
              <RefreshCw size={14} className="text-[#7B5CFF] animate-spin mt-0.5" />
            )}
          </div>
        )}

        {/* Voice model status (same flow as the classification model) */}
        <VoiceModelBanner
          modelStatus={dictation.modelStatus}
          modelProgress={dictation.modelProgress}
          onDownload={dictation.downloadModel}
        />

        {/* Message log */}
        <div
          ref={logRef}
          data-testid="chat-log"
          role="log"
          aria-live="polite"
          className="flex-1 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] px-3 py-4 space-y-4"
        >
          {!hasMessages && !chatLoading && (
            <div className="flex flex-col items-center text-center gap-5 pt-8 px-4 u-fade-in-up">
              <div className="flex items-center justify-center w-10 h-10 rounded-2xl bg-[#7B5CFF]/10 border border-[#7B5CFF]/20">
                <Sparkles size={20} style={{ color: ACCENT }} strokeWidth={1.5} />
              </div>
              <div className="space-y-1.5">
                <p className="text-gray-200 text-sm font-medium">{t('emptyTitle')}</p>
                <p className="text-gray-500 text-xs leading-relaxed max-w-[280px]">
                  {t('emptyHint')}
                </p>
              </div>
              <div className="flex flex-wrap justify-center gap-1.5 pt-1">
                {SEED_PROMPT_KEYS.map((key) => {
                  const label = t(key);
                  return <Chip key={key} label={label} onClick={() => handleSeed(label)} />;
                })}
              </div>
            </div>
          )}

          <MessageList
            messages={messages}
            activeTags={activeTags}
            activeKeywords={activeKeywords}
            appliedMessageId={appliedMessageId}
            onToggleTag={toggleTag}
            onToggleKeyword={toggleKeyword}
            onApply={applyMessageTags}
          />

          {chatLoading && <StreamingBubble text={streamingText} />}
        </div>

        {/* Composer */}
        <div className="border-t border-[#2e2e2e] px-3 pt-3 pb-3">
          {/* Unified input box — border turns violet when focused or recording */}
          <div
            className={[
              'rounded-xl border bg-[#141414] transition-colors',
              isRecording
                ? 'border-[#7B5CFF]/50'
                : 'border-[#2a2a2a] focus-within:border-[#7B5CFF]/50',
            ].join(' ')}
          >
            <textarea
              ref={textareaRef}
              data-testid="chat-input"
              value={displayDraft}
              // While recording OR transcribing the value is forced to the live
              // overlay (displayDraft), so lock the field to match — otherwise
              // keystrokes during transcription would call setDraft but vanish.
              onChange={inputLocked ? undefined : (e) => setDraft(e.target.value)}
              readOnly={inputLocked}
              onKeyDown={inputLocked ? undefined : onInputKeyDown}
              rows={1}
              placeholder={
                isBusyDict
                  ? t('placeholderTranscribing')
                  : isRecording
                    ? t('placeholderListening')
                    : t('placeholderDefault')
              }
              className={[
                'w-full bg-transparent px-3.5 pt-3 pb-1.5 text-sm outline-none resize-none overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e]',
                inputLocked
                  ? 'text-gray-400 placeholder-[#7B5CFF]/50 cursor-default'
                  : 'text-gray-100 placeholder-gray-600',
              ].join(' ')}
              style={{ minHeight: '40px', maxHeight: '168px' }}
            />
            {/* Toolbar row inside the box */}
            <div className="flex items-center justify-end gap-1.5 px-2.5 pb-2.5">
              {/* Mic: idle → icon; requesting/transcribing → spinner; recording → audio-reactive.
                  Isolated leaf so the ~10Hz audio level doesn't re-render the view. */}
              <MicButton
                dictation={dictation}
                isRecording={isRecording}
                isBusyDict={isBusyDict}
                micDisabled={micDisabled}
                modelReadyForVoice={modelReadyForVoice}
              />

              {chatLoading ? (
                <button
                  key="stop"
                  data-testid="chat-stop-btn"
                  onClick={stopStreaming}
                  aria-label={t('stopTitle')}
                  title={t('stopTitle')}
                  className="u-swap-in shrink-0 flex items-center justify-center gap-1.5 h-7 px-2.5 rounded-lg text-xs text-gray-300 bg-[#2a2a2a] hover:bg-[#333] border border-[#3a3a3a] u-press"
                >
                  <Square size={11} fill="currentColor" /> {t('stop')}
                </button>
              ) : (
                <button
                  key="send"
                  data-testid="chat-send-btn"
                  onClick={handleSend}
                  disabled={!draft.trim() && !isRecording}
                  aria-label={t('send')}
                  title={t('sendTitle')}
                  className="u-swap-in shrink-0 flex items-center justify-center gap-1.5 h-7 px-2.5 rounded-lg text-xs text-white bg-[#7B5CFF] hover:bg-[#6347e0] disabled:opacity-30 disabled:hover:bg-[#7B5CFF] u-press"
                >
                  <Send size={12} /> {t('send')}
                </button>
              )}
            </div>
          </div>

          {/* Keyboard hint / dictation error */}
          {dictation.status === 'error' ? (
            <p className="mt-1.5 text-[11px] text-red-500/80 select-none">
              {dictation.error || td('errorGeneric')}
            </p>
          ) : (
            <p className="mt-1.5 text-[11px] text-gray-700 text-right select-none">
              {t('newlineHint')}
            </p>
          )}
        </div>
      </div>

      {/* ── Right: gallery ─────────────────────────────────────────────────── */}
      <ResultsPane
        results={results}
        total={total}
        resultsLoading={resultsLoading}
        activeTags={activeTags}
        activeKeywords={activeKeywords}
        tagMode={tagMode}
        source={source}
        setTagMode={setTagMode}
        setSource={setSource}
        removeTag={removeTag}
        removeKeyword={removeKeyword}
        clearFilters={clearFilters}
        onPromoteCollection={handlePromoteCollection}
        onCopyLinks={handleCopyLinks}
        onExportMarkdown={handleExportMarkdown}
        onOpen={setActivePost}
        scrollRef={resultsScrollRef}
      />

      {activePost && (
        <PostModal
          post={activePost}
          onClose={() => setActivePost(null)}
          onPrev={goPrevPost}
          onNext={goNextPost}
          hasPrev={results.findIndex((p) => p.id === activePost.id) > 0}
          hasNext={(() => {
            const i = results.findIndex((p) => p.id === activePost.id);
            return i >= 0 && i < results.length - 1;
          })()}
          onApplyAiFilter={({ tag } = {}) => {
            if (tag) toggleTag(String(tag).toLowerCase());
            setActivePost(null);
          }}
          onLocalFilesDeleted={() => refresh()}
          onOpenInWebsites={
            onOpenInWebsites
              ? () => {
                  setActivePost(null);
                  onOpenInWebsites();
                }
              : undefined
          }
          onReanalyzeWeb={
            onReanalyzeWeb
              ? (p) => {
                  setActivePost(null);
                  onReanalyzeWeb(p);
                }
              : undefined
          }
        />
      )}

      {collectionModal && (
        // No `initial` → the modal renders in "Nuova source / Crea" mode (passing
        // a truthy `initial` would flip it to edit wording). The user types the
        // name; the default is offered via the placeholder of the modal itself.
        <CollectionModal onClose={() => setCollectionModal(null)} onSave={handleCreateCollection} />
      )}

      {toast && <Toast closing={toastClosing}>{toast}</Toast>}
    </div>
  );
}
