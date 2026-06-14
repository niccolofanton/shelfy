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

const RESULT_LIMIT = 60;

function initialState() {
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

let state = initialState();
const listeners = new Set();

function getSnapshot() {
  return state;
}

function subscribe(listener) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

// Apply a patch (or updater fn) and notify subscribers with a fresh object.
function setState(patch) {
  const next = typeof patch === 'function' ? patch(state) : patch;
  state = { ...state, ...next };
  listeners.forEach((l) => l());
}

let idCounter = 0;
function nextId() {
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
let activeChatRunId = null;

// A single module-level subscription to streamed chat tokens. Registered lazily
// the first time the hook is used and never torn down — matching the contract
// ("registra una sola subscription a livello modulo nell'hook").
let chatTokenUnsub = null;
function ensureChatTokenSubscription() {
  if (chatTokenUnsub) return;
  if (!window.electronAPI?.onChatToken) return;
  chatTokenUnsub = window.electronAPI.onChatToken((payload) => {
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
  tagsArg = state.activeTags,
  keywordsArg = state.activeKeywords,
  modeArg = state.tagMode,
  sourceArg = state.source,
) {
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

  let searchFn;
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
function dedupeTags(tags = []) {
  const seen = new Set();
  const out = [];
  for (const t of tags) {
    if (!t || seen.has(t)) continue;
    seen.add(t);
    out.push(t);
  }
  return out;
}

// Replace the active filters with exactly the tags AND keywords proposed by one
// assistant message (no accumulation across the conversation), then re-run.
function applyMessageTags(messageId) {
  const msg = state.messages.find((m) => m.id === messageId && m.role === 'assistant');
  if (!msg) return;
  const nextTags = dedupeTags(msg.proposedTags);
  const nextKeywords = dedupeTags(msg.proposedKeywords);
  setState({ activeTags: nextTags, activeKeywords: nextKeywords, appliedMessageId: messageId });
  runGallerySearch(nextTags, nextKeywords, state.tagMode);
}

// ── Actions ─────────────────────────────────────────────────────────────────

async function sendMessage(text) {
  const trimmed = (text || '').trim();
  if (!trimmed || state.chatLoading) return;

  ensureChatTokenSubscription();

  const userMsg = { id: nextId(), role: 'user', content: trimmed };
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
    const tagGroups = res?.tagGroups && typeof res.tagGroups === 'object' ? res.tagGroups : null;
    const keywordsToAdd = Array.isArray(res?.keywordsToAdd)
      ? res.keywordsToAdd
      : Array.isArray(tagGroups?.keywords)
        ? tagGroups.keywords
        : [];

    const assistantMsg = {
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
    const errMsg = {
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
      error: err?.message ?? 'Errore durante la ricerca',
    }));
  }
}

// Manual tweaks below clear appliedMessageId: once the user hand-edits the set
// it no longer matches any single message, so no message should read as applied.
function toggleTag(tag) {
  if (!tag) return;
  const has = state.activeTags.includes(tag);
  const next = has ? state.activeTags.filter((t) => t !== tag) : [...state.activeTags, tag];
  setState({ activeTags: next, appliedMessageId: null });
  runGallerySearch(next, state.activeKeywords, state.tagMode);
}

function removeTag(tag) {
  if (!state.activeTags.includes(tag)) return;
  const next = state.activeTags.filter((t) => t !== tag);
  setState({ activeTags: next, appliedMessageId: null });
  runGallerySearch(next, state.activeKeywords, state.tagMode);
}

function toggleKeyword(kw) {
  if (!kw) return;
  const has = state.activeKeywords.includes(kw);
  const next = has ? state.activeKeywords.filter((k) => k !== kw) : [...state.activeKeywords, kw];
  setState({ activeKeywords: next, appliedMessageId: null });
  runGallerySearch(state.activeTags, next, state.tagMode);
}

function removeKeyword(kw) {
  if (!state.activeKeywords.includes(kw)) return;
  const next = state.activeKeywords.filter((k) => k !== kw);
  setState({ activeKeywords: next, appliedMessageId: null });
  runGallerySearch(state.activeTags, next, state.tagMode);
}

function setTagMode(mode) {
  if (mode !== 'or' && mode !== 'and') return;
  if (mode === state.tagMode) return;
  setState({ tagMode: mode });
  runGallerySearch(state.activeTags, state.activeKeywords, mode, state.source);
}

// Scope the gallery results to all sources, only web sites, or only social.
function setSource(source) {
  if (source !== 'all' && source !== 'web' && source !== 'social') return;
  if (source === state.source) return;
  setState({ source });
  runGallerySearch(state.activeTags, state.activeKeywords, state.tagMode, source);
}

// "Pulisci" — drop every active filter (both tags and keywords) at once.
function clearFilters() {
  if (state.activeTags.length === 0 && state.activeKeywords.length === 0) return;
  setState({ activeTags: [], activeKeywords: [], appliedMessageId: null });
  runGallerySearch([], [], state.tagMode);
}

async function stopStreaming() {
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

function reset() {
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

async function refreshModelStatus() {
  if (!window.electronAPI?.getModelStatus) return;
  try {
    const s = await window.electronAPI.getModelStatus();
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

async function downloadModel() {
  if (!window.electronAPI?.downloadModel) return;
  setState({ modelStatus: { ...state.modelStatus, downloading: true } });
  try {
    await window.electronAPI.downloadModel();
  } catch (err) {
    console.error('[useAiSearch] downloadModel error:', err);
  } finally {
    stopModelStatusPoll();
    refreshModelStatus();
  }
}

// One-time module init: load model status and subscribe to optional progress.
let initialized = false;
let modelProgressUnsub = null;
let modelPollTimer = null;
// While a download is in flight, poll the authoritative status. analyzer's
// downloadModel emits NO terminal progress=1 on failure/pause/cancel (and a
// download may be started from the Settings picker, not via our own action), so
// without this the banner could spin on "Download in corso…" forever. getStatus
// returns the real downloading=false once the job ends.
function startModelStatusPoll() {
  if (modelPollTimer) return;
  modelPollTimer = setInterval(async () => {
    await refreshModelStatus();
    // state is reassigned by refreshModelStatus via setState; once the backend
    // reports it's no longer downloading, stop polling.
    if (!state.modelStatus?.downloading) stopModelStatusPoll();
  }, 2000);
}
function stopModelStatusPoll() {
  if (modelPollTimer) {
    clearInterval(modelPollTimer);
    modelPollTimer = null;
  }
}
function ensureInit() {
  if (initialized) return;
  initialized = true;
  refreshModelStatus();
  if (window.electronAPI?.onModelProgress) {
    modelProgressUnsub = window.electronAPI.onModelProgress((p) => {
      if (p && p.progress >= 1) {
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

// Stable actions object — created once so consumers can rely on referential
// equality (no need for useCallback at the call site).
const actions = {
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
export function useAiSearch() {
  ensureInit();
  ensureChatTokenSubscription();
  const snapshot = useSyncExternalStore(subscribe, getSnapshot);
  return useMemo(() => ({ ...snapshot, actions }), [snapshot]);
}

export default useAiSearch;
