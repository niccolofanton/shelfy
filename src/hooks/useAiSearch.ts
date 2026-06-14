import { useSyncExternalStore, useMemo } from 'react';
import { translate, getInitialLang } from '../i18n';

// ────────────────────────────────────────────────────────────────────────────
// Module-scope store
//
// The state lives at module scope (outside any component) so it PERSISTS when
// the AiSearch view unmounts and remounts — the user can switch tabs and come
// back to the same conversation, active tags and gallery results.
//
// Implemented as a tiny subscribe + getSnapshot store consumed via
// useSyncExternalStore. State is treated as immutable: every mutation produces
// a brand-new object so the snapshot identity changes and React re-renders.
// ────────────────────────────────────────────────────────────────────────────

// Tag-grouping payload carried by an assistant reply (mirrors ChatSearchResult
// .tagGroups; null when the model produced none).
type TagGroups = {
  broad: string[];
  specific: string[];
  keywords: string[];
} | null;

// A single chat bubble. User messages carry only id/role/content; assistant
// messages may also carry the proposed tag/keyword/group sets and an error flag.
export interface Message {
  id: string;
  role: 'user' | 'assistant';
  content: string;
  proposedTags?: string[];
  proposedKeywords?: string[];
  proposedGroups?: TagGroups;
  isError?: boolean;
}

// 'or' | 'and' — how active archive tags combine.
type TagMode = 'or' | 'and';
// 'all' | 'web' | 'social' — Siti / Social / Tutto filter.
type SourceScope = 'all' | 'web' | 'social';

// Coarse readiness of the local VLM (model status surfaces as `unknown` on the
// bridge; only ready/downloading are tracked here).
interface ModelStatusFlags {
  ready: boolean;
  downloading: boolean;
}

// The module-scope store snapshot.
export interface AiSearchStoreState {
  messages: Message[];
  activeTags: string[]; // archive tags (filter the tag index)
  activeKeywords: string[]; // free-text keywords from the message (search descriptions)
  appliedMessageId: string | null; // id of the assistant message whose tag+keyword set is the active filter
  tagMode: TagMode;
  source: SourceScope; // Siti / Social / Tutto filter
  results: Shelfy.Post[];
  total: number;
  chatLoading: boolean;
  streamingText: string;
  resultsLoading: boolean;
  error: string | null;
  modelStatus: ModelStatusFlags;
}

const RESULT_LIMIT = 60;

function initialState(): AiSearchStoreState {
  return {
    messages: [], // [{ id, role:'user'|'assistant', content, proposedTags?, proposedKeywords?, isError? }]
    activeTags: [], // string[] — archive tags (filter the tag index)
    activeKeywords: [], // string[] — free-text keywords from the message (search descriptions)
    appliedMessageId: null, // id of the assistant message whose tag+keyword set is the active filter
    tagMode: 'or', // 'or' | 'and'
    source: 'all', // 'all' | 'web' | 'social' — Siti / Social / Tutto filter
    results: [], // Post[]
    total: 0,
    chatLoading: false,
    streamingText: '',
    resultsLoading: false,
    error: null,
    modelStatus: { ready: false, downloading: false },
  };
}

let state: AiSearchStoreState = initialState();
const listeners = new Set<() => void>();

function getSnapshot(): AiSearchStoreState {
  return state;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

type StatePatch = Partial<AiSearchStoreState>;

// Apply a patch (or updater fn) and notify subscribers with a fresh object.
function setState(patch: StatePatch | ((s: AiSearchStoreState) => StatePatch)): void {
  const next = typeof patch === 'function' ? patch(state) : patch;
  state = { ...state, ...next };
  listeners.forEach((l) => l());
}

let idCounter = 0;
function nextId(): string {
  idCounter += 1;
  return `m${Date.now()}_${idCounter}`;
}

// ── Anti-race guards (module-scope so they survive remounts) ────────────────
let resultsReqId = 0; // bumped per gallery fetch; stale responses are dropped
let chatToken = 0; // bumped per sendMessage; ignores stale invoke RESULTS
// Backend run id (stamped by ipc.js) of the run currently allowed to stream
// tokens. null = no adopted run, every token is dropped. ipc.js announces each
// run with { start: true, runId } on the token channel BEFORE its first token
// (IPC delivery is FIFO), so adopting the latest announcement binds the stream
// to the newest run and stragglers from an aborted run carry an older runId.
let activeChatRunId: number | null = null;

// Shape of a streamed-token event on the chat-token channel. Either a run
// announcement ({ start, runId }) or a token chunk ({ token, runId }). The bridge
// delivers this as `unknown`, so it's narrowed before use.
interface ChatTokenPayload {
  start?: boolean;
  runId?: number | null;
  token?: string;
}

function asChatTokenPayload(v: unknown): ChatTokenPayload | null {
  return v && typeof v === 'object' ? (v as ChatTokenPayload) : null;
}

// A single module-level subscription to streamed chat tokens. Registered lazily
// the first time the hook is used and never torn down — matching the contract
// ("registra una sola subscription a livello modulo nell'hook").
let chatTokenUnsub: (() => void) | null = null;
function ensureChatTokenSubscription(): void {
  if (chatTokenUnsub) return;
  if (!window.electronAPI?.onChatToken) return;
  chatTokenUnsub = window.electronAPI.onChatToken((raw) => {
    const payload = asChatTokenPayload(raw);
    // Only accept events while a chat request is in flight.
    if (!state.chatLoading) return;
    if (payload?.start && payload.runId != null) {
      // New run announced: adopt it and clear anything a superseded run may
      // have streamed into the bubble in the meantime.
      activeChatRunId = payload.runId;
      if (state.streamingText) setState({ streamingText: '' });
      return;
    }
    // Drop tokens from a superseded (just-aborted) run — llama-server can still
    // flush a few buffered tokens before the abort propagates, and chatLoading
    // is already true again for the NEW request, so it can't distinguish them.
    if (payload?.runId == null || payload.runId !== activeChatRunId) return;
    const token = payload?.token ?? '';
    if (!token) return;
    setState((s) => ({ streamingText: s.streamingText + token }));
  });
}

// ── Gallery fetching ────────────────────────────────────────────────────────
// Re-fetches whenever activeTags / activeKeywords / tagMode change. Uses the
// reqRef anti-race pattern so a slow earlier request can never clobber a newer
// one. Tags filter the tag index; the active KEYWORDS are joined into the
// free-text query matched against captions + AI descriptions (traditional search).
//
// The active tags/keywords/mode are passed in explicitly by callers (which just
// computed them) rather than re-read from the module-scope `state`. This makes
// the dependency on the freshly-set values explicit and refactor-proof — it no
// longer relies on `setState` having synchronously reassigned `state` first.
function runGallerySearch(
  tagsArg: string[] = state.activeTags,
  keywordsArg: string[] = state.activeKeywords,
  modeArg: TagMode = state.tagMode,
  sourceArg: SourceScope = state.source,
): void {
  const tags = Array.isArray(tagsArg) ? tagsArg : [];
  const keywords = Array.isArray(keywordsArg) ? keywordsArg : [];
  const mode = modeArg;
  const source = sourceArg || 'all';
  const hasTags = tags.length > 0;
  const textQuery = keywords.join(' ').trim();

  if (!hasTags && !textQuery) {
    // Nothing active — clear results (and invalidate any in-flight req).
    resultsReqId += 1;
    setState({ results: [], total: 0, resultsLoading: false });
    return;
  }

  const reqId = ++resultsReqId;
  setState({ resultsLoading: true });

  // All three search paths resolve a { posts, total } envelope.
  let searchFn: Promise<{ posts?: Shelfy.Post[]; total?: number }>;
  if (hasTags && textQuery && window.electronAPI?.searchHybrid) {
    searchFn = window.electronAPI.searchHybrid(tags, textQuery, mode, RESULT_LIMIT, 0, source);
  } else if (hasTags) {
    searchFn = window.electronAPI.searchByTags(tags, mode, RESULT_LIMIT, 0, source);
  } else {
    searchFn = window.electronAPI.searchByText(textQuery, RESULT_LIMIT, 0, source);
  }

  searchFn
    .then((res) => {
      if (reqId !== resultsReqId) return;
      setState({ results: res?.posts ?? [], total: res?.total ?? 0 });
    })
    .catch((err) => {
      if (reqId !== resultsReqId) return;
      console.error('[useAiSearch] gallery search error:', err);
      setState({ results: [], total: 0 });
    })
    .finally(() => {
      if (reqId === resultsReqId) setState({ resultsLoading: false });
    });
}

// De-duplicate a tag list, preserving order. Drops falsy entries.
function dedupeTags(tags: string[] = []): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const t of tags) {
    if (!t || seen.has(t)) continue;
    seen.add(t);
    out.push(t);
  }
  return out;
}

// Replace the active filters with exactly the tags AND keywords proposed by one
// assistant message (no accumulation across the conversation), then re-run.
function applyMessageTags(messageId: string): void {
  const msg = state.messages.find((m) => m.id === messageId && m.role === 'assistant');
  if (!msg) return;
  const nextTags = dedupeTags(msg.proposedTags);
  const nextKeywords = dedupeTags(msg.proposedKeywords);
  setState({ activeTags: nextTags, activeKeywords: nextKeywords, appliedMessageId: messageId });
  runGallerySearch(nextTags, nextKeywords, state.tagMode);
}

// ── Actions ─────────────────────────────────────────────────────────────────

async function sendMessage(text: string): Promise<void> {
  const trimmed = (text || '').trim();
  if (!trimmed || state.chatLoading) return;

  ensureChatTokenSubscription();

  const userMsg: Message = { id: nextId(), role: 'user', content: trimmed };
  // History for the model = full conversation including the new user message.
  const history = [...state.messages, userMsg].map((m) => ({
    role: m.role,
    content: m.content,
  }));

  setState((s) => ({
    messages: [...s.messages, userMsg],
    chatLoading: true,
    streamingText: '',
    error: null,
  }));

  const runId = ++chatToken;
  // No adopted backend run yet: ignore every token until ipc.js announces this
  // run's id on the token channel (which happens before its first token).
  activeChatRunId = null;

  try {
    const res = await window.electronAPI.chatSearch(history, state.activeTags);
    // A newer request (or a cancel) superseded this one — drop the result.
    if (runId !== chatToken) return;

    const reply = res?.reply ?? '';
    const tagsToAdd = Array.isArray(res?.tagsToAdd) ? res.tagsToAdd : [];
    const tagGroups: TagGroups =
      res?.tagGroups && typeof res.tagGroups === 'object' ? res.tagGroups : null;
    const keywordsToAdd = Array.isArray(res?.keywordsToAdd)
      ? res.keywordsToAdd
      : Array.isArray(tagGroups?.keywords)
        ? tagGroups.keywords
        : [];

    const assistantMsg: Message = {
      id: nextId(),
      role: 'assistant',
      content: reply,
      proposedTags: tagsToAdd,
      proposedKeywords: keywordsToAdd,
      proposedGroups: tagGroups,
    };

    // The latest reply REPLACES the active filters with its own suggestions
    // (each message stands on its own — no summing across the conversation).
    // The user can re-apply any earlier message via its "Applica" button.
    const nextTags = dedupeTags(tagsToAdd);
    const nextKeywords = dedupeTags(keywordsToAdd);
    setState((s) => ({
      messages: [...s.messages, assistantMsg],
      chatLoading: false,
      streamingText: '',
      activeTags: nextTags,
      activeKeywords: nextKeywords,
      appliedMessageId: nextTags.length || nextKeywords.length ? assistantMsg.id : null,
    }));

    // Re-run the search with the just-computed filter set (passed explicitly so
    // it never depends on `setState` having already reassigned `state`).
    runGallerySearch(nextTags, nextKeywords, state.tagMode);
  } catch (err) {
    if (runId !== chatToken) return;
    console.error('[useAiSearch] chatSearch error:', err);
    const errMsg: Message = {
      id: nextId(),
      role: 'assistant',
      // Module-scope (no React context here) — resolve against the persisted
      // language. The error bubble's `content` is rendered, so it must localize.
      content: translate(getInitialLang(), 'aiSearch.chatError'),
      isError: true,
      proposedTags: [],
    };
    setState((s) => ({
      messages: [...s.messages, errMsg],
      chatLoading: false,
      streamingText: '',
      error: err instanceof Error ? err.message : 'Errore durante la ricerca',
    }));
  }
}

// Manual tweaks below clear appliedMessageId: once the user hand-edits the set
// it no longer matches any single message, so no message should read as applied.
function toggleTag(tag: string): void {
  if (!tag) return;
  const has = state.activeTags.includes(tag);
  const next = has ? state.activeTags.filter((t) => t !== tag) : [...state.activeTags, tag];
  setState({ activeTags: next, appliedMessageId: null });
  runGallerySearch(next, state.activeKeywords, state.tagMode);
}

function removeTag(tag: string): void {
  if (!state.activeTags.includes(tag)) return;
  const next = state.activeTags.filter((t) => t !== tag);
  setState({ activeTags: next, appliedMessageId: null });
  runGallerySearch(next, state.activeKeywords, state.tagMode);
}

function toggleKeyword(kw: string): void {
  if (!kw) return;
  const has = state.activeKeywords.includes(kw);
  const next = has ? state.activeKeywords.filter((k) => k !== kw) : [...state.activeKeywords, kw];
  setState({ activeKeywords: next, appliedMessageId: null });
  runGallerySearch(state.activeTags, next, state.tagMode);
}

function removeKeyword(kw: string): void {
  if (!state.activeKeywords.includes(kw)) return;
  const next = state.activeKeywords.filter((k) => k !== kw);
  setState({ activeKeywords: next, appliedMessageId: null });
  runGallerySearch(state.activeTags, next, state.tagMode);
}

function setTagMode(mode: TagMode): void {
  if (mode !== 'or' && mode !== 'and') return;
  if (mode === state.tagMode) return;
  setState({ tagMode: mode });
  runGallerySearch(state.activeTags, state.activeKeywords, mode, state.source);
}

// Scope the gallery results to all sources, only web sites, or only social.
function setSource(source: SourceScope): void {
  if (source !== 'all' && source !== 'web' && source !== 'social') return;
  if (source === state.source) return;
  setState({ source });
  runGallerySearch(state.activeTags, state.activeKeywords, state.tagMode, source);
}

// "Pulisci" — drop every active filter (both tags and keywords) at once.
function clearFilters(): void {
  if (state.activeTags.length === 0 && state.activeKeywords.length === 0) return;
  setState({ activeTags: [], activeKeywords: [], appliedMessageId: null });
  runGallerySearch([], [], state.tagMode);
}

async function stopStreaming(): Promise<void> {
  // Invalidate the in-flight chat run so a late resolve is ignored, then ask
  // the backend to cancel.
  chatToken += 1;
  activeChatRunId = null;
  setState({ chatLoading: false, streamingText: '' });
  try {
    await window.electronAPI?.cancelChatSearch?.();
  } catch (err) {
    console.error('[useAiSearch] cancelChatSearch error:', err);
  }
}

function reset(): void {
  // Invalidate any in-flight work, then wipe the conversation & gallery.
  chatToken += 1;
  activeChatRunId = null;
  resultsReqId += 1;
  const ms = state.modelStatus;
  state = { ...initialState(), modelStatus: ms };
  listeners.forEach((l) => l());
  try {
    window.electronAPI?.cancelChatSearch?.();
  } catch {
    /* ignore */
  }
}

// VLM model status surfaces as `unknown` on the bridge; only ready/downloading
// are read here.
interface RawModelStatus {
  ready?: boolean;
  downloading?: boolean;
  [key: string]: unknown;
}
function asRawModelStatus(v: unknown): RawModelStatus | null {
  return v && typeof v === 'object' ? (v as RawModelStatus) : null;
}

async function refreshModelStatus(): Promise<void> {
  if (!window.electronAPI?.getModelStatus) return;
  try {
    const s = asRawModelStatus(await window.electronAPI.getModelStatus());
    setState({
      modelStatus: {
        ready: !!s?.ready,
        downloading: !!s?.downloading,
      },
    });
  } catch (err) {
    console.error('[useAiSearch] getModelStatus error:', err);
  }
}

async function downloadModel(): Promise<void> {
  if (!window.electronAPI?.downloadModel) return;
  setState({ modelStatus: { ...state.modelStatus, downloading: true } });
  try {
    // The bridge types `downloadModel` with a required `id`, but at runtime the
    // id is optional (preload/ipc default it to undefined → resume the pending
    // download). Call it with no argument to preserve that behavior; cast to the
    // no-arg signature so the frozen type's required param doesn't force one.
    await (window.electronAPI.downloadModel as () => Promise<unknown>)();
  } catch (err) {
    console.error('[useAiSearch] downloadModel error:', err);
  } finally {
    stopModelStatusPoll();
    refreshModelStatus();
  }
}

// One-time module init: load model status and subscribe to optional progress.
let initialized = false;
let modelProgressUnsub: (() => void) | null = null;
let modelPollTimer: ReturnType<typeof setInterval> | null = null;
// While a download is in flight, poll the authoritative status. analyzer's
// downloadModel emits NO terminal progress=1 on failure/pause/cancel (and a
// download may be started from the Settings picker, not via our own action), so
// without this the banner could spin on "Download in corso…" forever. getStatus
// returns the real downloading=false once the job ends.
function startModelStatusPoll(): void {
  if (modelPollTimer) return;
  modelPollTimer = setInterval(async () => {
    await refreshModelStatus();
    // state is reassigned by refreshModelStatus via setState; once the backend
    // reports it's no longer downloading, stop polling.
    if (!state.modelStatus?.downloading) stopModelStatusPoll();
  }, 2000);
}
function stopModelStatusPoll(): void {
  if (modelPollTimer) {
    clearInterval(modelPollTimer);
    modelPollTimer = null;
  }
}

// Model-download progress event (onModelProgress payload is `unknown`).
interface ModelProgressPayload {
  progress?: number;
  [key: string]: unknown;
}
function asModelProgressPayload(v: unknown): ModelProgressPayload | null {
  return v && typeof v === 'object' ? (v as ModelProgressPayload) : null;
}

function ensureInit(): void {
  if (initialized) return;
  initialized = true;
  refreshModelStatus();
  if (window.electronAPI?.onModelProgress) {
    modelProgressUnsub = window.electronAPI.onModelProgress((raw) => {
      const p = asModelProgressPayload(raw);
      if (p && typeof p.progress === 'number' && p.progress >= 1) {
        stopModelStatusPoll();
        refreshModelStatus();
      } else {
        setState({ modelStatus: { ...state.modelStatus, downloading: true } });
        // Guard against a stuck banner if the download fails/pauses/cancels
        // without ever emitting a terminal progress event.
        startModelStatusPoll();
      }
    });
  }
}

// The stable actions object exposed by the store.
export interface AiSearchActions {
  sendMessage: (text: string) => Promise<void>;
  toggleTag: (tag: string) => void;
  removeTag: (tag: string) => void;
  toggleKeyword: (kw: string) => void;
  removeKeyword: (kw: string) => void;
  applyMessageTags: (messageId: string) => void;
  setTagMode: (mode: TagMode) => void;
  setSource: (source: SourceScope) => void;
  clearFilters: () => void;
  stopStreaming: () => Promise<void>;
  reset: () => void;
  downloadModel: () => Promise<void>;
  refreshModelStatus: () => Promise<void>;
  refresh: typeof runGallerySearch;
}

// Stable actions object — created once so consumers can rely on referential
// equality (no need for useCallback at the call site).
const actions: AiSearchActions = {
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
  reset,
  downloadModel,
  refreshModelStatus,
  refresh: runGallerySearch,
};

// What the hook returns: the persisted snapshot plus the stable actions.
export type UseAiSearchResult = AiSearchStoreState & { actions: AiSearchActions };

/**
 * useAiSearch — subscribes the component to the module-scope AI-search store.
 *
 * @returns the persisted state plus the stable `actions` (also spread at the
 *          top level for convenience):
 *   state: { messages, activeTags, activeKeywords, appliedMessageId, tagMode,
 *            results, total, chatLoading, streamingText, resultsLoading, error,
 *            modelStatus }
 *   actions: { sendMessage, toggleTag, removeTag, toggleKeyword, removeKeyword,
 *              applyMessageTags, setTagMode, clearFilters, stopStreaming, reset,
 *              downloadModel, refreshModelStatus }
 */
export function useAiSearch(): UseAiSearchResult {
  ensureInit();
  ensureChatTokenSubscription();
  const snapshot = useSyncExternalStore(subscribe, getSnapshot);
  return useMemo(() => ({ ...snapshot, actions }), [snapshot]);
}

export default useAiSearch;
