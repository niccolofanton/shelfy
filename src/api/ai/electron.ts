// The desktop AiApi: every operation sends the exact IPC channel and
// arguments the pre-seam hooks used (src/hooks/useAnalysis.tsx, useAiTags.ts,
// useAiSearch.ts, useDictation.ts), so the desktop's behavior is unchanged
// (P3-08 acceptance).
import type { ElectronAPI } from '../../../types/electron-api';
import type { AiApi } from './index';
type DesktopAiApi = Required<Omit<AiApi, 'webQueue'>>;
import type { AiQueueApi } from './queue';
import type { AiTagsApi } from './tags';
import type { AiSearchApi } from './search';
import type { AiSuggestApi } from './suggest';
import type { AiDictationApi } from './dictation';

// `bridge` is read on every call, never captured — same contract as
// createElectronClient (electron.ts / electronClient.ts share this pattern
// because the preload may install `window.electronAPI` after module load).
export function createElectronAiApi(bridge: () => ElectronAPI): DesktopAiApi {
  const queue: AiQueueApi = {
    getStatus: () => bridge().getAnalyzeStatus(),
    getIsPaused: () => bridge().getAnalyzeIsPaused(),
    getConcurrency: () => bridge().getAnalyzeConcurrency(),
    onProgress: (cb) => {
      const off = bridge().onAnalyzeProgress(cb);
      return () => off?.();
    },

    getModelStatus: () => bridge().getModelStatus(),
    onModelProgress: (cb) => {
      const off = bridge().onModelProgress(cb);
      return () => off?.();
    },
    // No model id: the analyzer downloads the currently-selected/default
    // model. The bridge types `id` as required, but the IPC handler defaults
    // it to undefined, so calling with no argument is the intended runtime
    // behavior (mirrors the pre-seam useAnalysis.downloadModel).
    downloadModel: () => (bridge().downloadModel as () => Promise<unknown>)(),

    analyzePost: (postId) => bridge().analyzePost(postId),
    analyzeAll: () => bridge().analyzeAll(),
    analyzeMissing: () => bridge().analyzeMissing(),

    cancelJob: (key) => bridge().cancelAnalyzeJob(key),
    cancelAll: () => bridge().cancelAllAnalyze(),
    retryJob: (key) => bridge().retryAnalyzeJob(key),
    pauseAll: () => bridge().pauseAnalyze(),
    resumeAll: () => bridge().resumeAnalyze(),
    clearAll: () => bridge().clearAllAnalyze(),
    clearCompleted: () => bridge().clearCompletedAnalyze(),

    updateManual: (id, fields) => bridge().updatePostAiAnalysis(id, fields),
    clearDescriptions: (ids) => bridge().clearPostDescriptions(ids),
  };

  const tags: AiTagsApi = {
    getOverview: () => bridge().getAiOverview(),
    getTagStats: (args) => bridge().getTagStats(args),
    getEntityStats: (args) => bridge().getEntityStats(args),
    getTagCooccurrence: (tag, limit) => bridge().getTagCooccurrence(tag, limit),
    getHealth: () => bridge().getTagHealth(),
    getMergeSuggestions: (args) => bridge().getTagMergeSuggestions(args),

    renameTag: (from, to) => bridge().renameTag(from, to),
    mergeTags: (sources, target) => bridge().mergeTags(sources, target),

    getClusters: (args) => bridge().getTagClusters(args),
    regenerateClusters: async (onProgress) => {
      const off = bridge().onClusterProgress((p) => onProgress?.(p));
      try {
        return await bridge().regenerateClusters();
      } finally {
        off?.();
      }
    },
    cancelClusters: () => bridge().cancelClusters(),
    acceptCluster: (id) => bridge().acceptCluster(id),
    dismissCluster: (id) => bridge().dismissCluster(id),
    renameCluster: (id, label) => bridge().renameCluster(id, label),
    removeTagFromCluster: (tag, clusterId) => bridge().removeTagFromCluster(tag, clusterId),

    getAliases: (args) => bridge().getTagAliases(args),
    proposeAliases: async (onProgress) => {
      const off = bridge().onAliasProgress((p) => onProgress?.(p));
      try {
        return await bridge().proposeAliases();
      } finally {
        off?.();
      }
    },
    cancelAliases: () => bridge().cancelAliases(),
    acceptAlias: (aliasNorm) => bridge().acceptAlias(aliasNorm),
    dismissAlias: (aliasNorm) => bridge().dismissAlias(aliasNorm),

    getPostIdsByTags: (tagList, mode) => bridge().getPostIdsByTags(tagList, mode),
    getPosts: (filters) => bridge().getPosts(filters),
  };

  const search: AiSearchApi = {
    byTags: (tagList, mode, limit, offset, source) =>
      bridge().searchByTags(tagList, mode, limit, offset, source),
    hybrid: (tagList, textQuery, mode, limit, offset, source) =>
      bridge().searchHybrid(tagList, textQuery, mode, limit, offset, source),
    byText: (query, limit, offset, source) => bridge().searchByText(query, limit, offset, source),

    chat: (messages, activeTags) => bridge().chatSearch(messages, activeTags),
    cancelChat: () => bridge().cancelChatSearch(),
    onToken: (cb) => {
      const off = bridge().onChatToken(cb);
      return () => off?.();
    },

    onResultsStale: (cb) => {
      const off = bridge().onNewPosts((raw) => {
        const source = (raw as { source?: unknown } | null)?.source;
        if (source === 'preview-cache' || source === 'preview-repair') cb();
      });
      return () => off?.();
    },

    getProviders: () => bridge().getSearchProviders(),
    selectProvider: (id) => bridge().selectSearchProvider(id),
    getModelStatus: () => bridge().getModelStatus(),
    onModelProgress: (cb) => {
      const off = bridge().onModelProgress(cb);
      return () => off?.();
    },
    downloadModel: () => (bridge().downloadModel as () => Promise<unknown>)(),
  };

  const suggest: AiSuggestApi = {
    suggest: (query) => bridge().suggestSearch(query),
  };

  const dictation: AiDictationApi = {
    status: () => bridge().sttStatus(),
    onModelProgress: (cb) => {
      const off = bridge().onSttModelProgress(cb);
      return () => off?.();
    },
    // No model id, same runtime contract as queue/search's downloadModel above.
    downloadModel: () => (bridge().sttDownloadModel as () => Promise<unknown>)(),
    ensure: async () => {
      await bridge().sttEnsure();
    },
    transcribe: (wav, opts) => bridge().sttTranscribe(wav, opts),
  };

  return { queue, tags, search, suggest, dictation };
}
