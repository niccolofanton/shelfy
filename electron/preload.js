'use strict';

try {
  const { contextBridge, ipcRenderer } = require('electron');
  const path = require('path');

  contextBridge.exposeInMainWorld('electronAPI', {
    // DB
    getPosts: (filters) => ipcRenderer.invoke('db:getPosts', filters),
    getPostIds: (filters) => ipcRenderer.invoke('db:getPostIds', filters),
    getPostsByIds: (ids) => ipcRenderer.invoke('db:getPostsByIds', { ids }),
    existingIds: (ids) => ipcRenderer.invoke('db:existingIds', { ids }),
    savedByKeys: (keys) => ipcRenderer.invoke('db:savedByKeys', { keys }),
    getStats: () => ipcRenderer.invoke('db:getStats'),
    importJSON: (filePath) => ipcRenderer.invoke('db:importJSON', { filePath }),
    exportJSON: (platforms) => ipcRenderer.invoke('db:exportJSON', { platforms }),
    clearAllData: () => ipcRenderer.invoke('db:clearAll'),
    clearAllAiAnalysis: () => ipcRenderer.invoke('db:clearAiAnalysis'),
    clearAllAssets: () => ipcRenderer.invoke('db:clearAssets'),

    // Custom sources (collections)
    getCollections: () => ipcRenderer.invoke('collections:list'),
    createCollection: (name, color, opts = {}) =>
      ipcRenderer.invoke('collections:create', { name, color, ...opts }),
    updateCollection: (id, { name, color } = {}) =>
      ipcRenderer.invoke('collections:update', { id, name, color }),
    deleteCollection: (id, opts = {}) =>
      ipcRenderer.invoke('collections:delete', { id, deletePosts: !!opts.deletePosts }),
    addPostsToCollections: (postIds, collectionIds) =>
      ipcRenderer.invoke('collections:addPosts', { postIds, collectionIds }),
    removePostFromCollection: (postId, collectionId) =>
      ipcRenderer.invoke('collections:removePost', { postId, collectionId }),

    // Downloads — enqueue
    downloadPost: (postId, assetTypes) =>
      ipcRenderer.invoke('download:post', { postId, assetTypes }),
    downloadPosts: (ids, assetTypes, missingOnly = true) =>
      ipcRenderer.invoke('download:posts', { ids, assetTypes, missingOnly }),
    downloadAll: (assetTypes, missingOnly = false) =>
      ipcRenderer.invoke('download:all', { assetTypes, missingOnly }),

    // Downloads — status
    getDownloadStatus: () => ipcRenderer.invoke('download:status'),
    getDownloadIsPaused: () => ipcRenderer.invoke('download:isPaused'),

    // Downloads — global controls
    pauseDownloads: () => ipcRenderer.invoke('download:pauseAll'),
    resumeDownloads: () => ipcRenderer.invoke('download:resumeAll'),
    cancelAllDownloads: () => ipcRenderer.invoke('download:cancelAll'),
    clearCompletedDownloads: () => ipcRenderer.invoke('download:clearCompleted'),

    // Downloads — per-job controls (key = `${postId}:${assetType}`)
    cancelDownloadJob: (key) => ipcRenderer.invoke('download:cancelJob', { key }),
    retryDownloadJob: (key) => ipcRenderer.invoke('download:retryJob', { key }),

    // Analyze (local VLM categorization)
    analyzePost: (postId) => ipcRenderer.invoke('analyze:post', { postId }),
    analyzeAll: () => ipcRenderer.invoke('analyze:all'),
    analyzePosts: (postIds) => ipcRenderer.invoke('analyze:posts', { postIds }),
    splitForAnalysis: (postIds) => ipcRenderer.invoke('analyze:split', { postIds }),
    getAnalyzeStatus: () => ipcRenderer.invoke('analyze:status'),
    cancelAnalyzeJob: (key) => ipcRenderer.invoke('analyze:cancelJob', { key }),
    cancelAllAnalyze: () => ipcRenderer.invoke('analyze:cancelAll'),
    clearAllAnalyze: () => ipcRenderer.invoke('analyze:clearAll'),
    clearCompletedAnalyze: () => ipcRenderer.invoke('analyze:clearCompleted'),
    pauseAnalyze: () => ipcRenderer.invoke('analyze:pauseAll'),
    resumeAnalyze: () => ipcRenderer.invoke('analyze:resumeAll'),
    getAnalyzeIsPaused: () => ipcRenderer.invoke('analyze:isPaused'),
    retryAnalyzeJob: (key) => ipcRenderer.invoke('analyze:retryJob', { key }),
    getModelStatus: () => ipcRenderer.invoke('analyze:modelStatus'),
    listModels: () => ipcRenderer.invoke('analyze:listModels'),
    setModel: (id) => ipcRenderer.invoke('analyze:setModel', { id }),
    getAnalyzeConcurrency: () => ipcRenderer.invoke('analyze:getConcurrency'),
    setAnalyzeConcurrency: (n) => ipcRenderer.invoke('analyze:setConcurrency', { n }),
    getHardwareInfo: () => ipcRenderer.invoke('analyze:getHardware'),
    getAnalyzeTuning: () => ipcRenderer.invoke('analyze:getTuning'),
    setAnalyzeTuning: (patch) => ipcRenderer.invoke('analyze:setTuning', { patch }),
    downloadModel: (id) => ipcRenderer.invoke('analyze:downloadModel', { id }),
    pauseModelDownload: () => ipcRenderer.invoke('analyze:pauseDownload'),
    cancelModelDownload: (id) => ipcRenderer.invoke('analyze:cancelDownload', { id }),
    deleteModel: (id) => ipcRenderer.invoke('analyze:deleteModel', { id }),
    getTaxonomy: () => ipcRenderer.invoke('analyze:taxonomy'),

    // App / updates
    getAppVersion: () => ipcRenderer.invoke('app:getVersion'),
    getUpdateChannel: () => ipcRenderer.invoke('app:getUpdateChannel'),
    setUpdateChannel: (channel) => ipcRenderer.invoke('app:setUpdateChannel', { channel }),
    getUpdateState: () => ipcRenderer.invoke('updater:getState'),
    checkForUpdates: () => ipcRenderer.invoke('updater:check'),
    quitAndInstallUpdate: () => ipcRenderer.invoke('updater:quitAndInstall'),
    openUpdateDownload: () => ipcRenderer.invoke('updater:openDownload'),
    rebuildUpdate: () => ipcRenderer.invoke('updater:rebuild'),
    onUpdaterState: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('updater:state', handler);
      return () => ipcRenderer.removeListener('updater:state', handler);
    },

    // Runtime sidecar binaries (downloaded into userData)
    getBinariesStatus: () => ipcRenderer.invoke('binaries:status'),
    ensureBinaries: (force) => ipcRenderer.invoke('binaries:ensure', { force: !!force }),
    getLlamaVariant: () => ipcRenderer.invoke('binaries:getVariant'),
    setLlamaVariant: (variant) => ipcRenderer.invoke('binaries:setVariant', { variant }),
    getVariantState: () => ipcRenderer.invoke('binaries:variantState'),
    onBinariesProgress: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('binaries:progress', handler);
      return () => ipcRenderer.removeListener('binaries:progress', handler);
    },
    onVariantFallback: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('ai:variantFallback', handler);
      return () => ipcRenderer.removeListener('ai:variantFallback', handler);
    },

    // AI Tags
    getAiOverview: () => ipcRenderer.invoke('aitags:overview'),
    getTagStats: (args) => ipcRenderer.invoke('aitags:tagStats', args),
    getEntityStats: (args) => ipcRenderer.invoke('aitags:entityStats', args),
    getTagCooccurrence: (tag, limit) => ipcRenderer.invoke('aitags:cooccurrence', { tag, limit }),
    getTagClusters: (args) => ipcRenderer.invoke('aitags:clusters', args),
    regenerateClusters: () => ipcRenderer.invoke('aitags:cluster:regenerate'),
    cancelClusters: () => ipcRenderer.invoke('aitags:cluster:cancel'),
    acceptCluster: (id) =>
      ipcRenderer.invoke('aitags:cluster:setStatus', { id, status: 'accepted' }),
    dismissCluster: (id) =>
      ipcRenderer.invoke('aitags:cluster:setStatus', { id, status: 'dismissed' }),
    renameCluster: (id, label) => ipcRenderer.invoke('aitags:cluster:rename', { id, label }),
    removeTagFromCluster: (tag, clusterId) =>
      ipcRenderer.invoke('aitags:cluster:removeTag', { tag, clusterId }),
    proposeAliases: () => ipcRenderer.invoke('aitags:aliases:propose'),
    cancelAliases: () => ipcRenderer.invoke('aitags:aliases:cancel'),
    getTagAliases: (args) => ipcRenderer.invoke('aitags:aliases:list', args),
    acceptAlias: (aliasNorm) => ipcRenderer.invoke('aitags:alias:accept', { aliasNorm }),
    dismissAlias: (aliasNorm) => ipcRenderer.invoke('aitags:alias:dismiss', { aliasNorm }),
    getTagMergeSuggestions: (args) => ipcRenderer.invoke('aitags:mergeSuggestions', args),
    getTagHealth: () => ipcRenderer.invoke('aitags:health'),
    renameTag: (from, to) => ipcRenderer.invoke('aitags:renameTag', { from, to }),
    mergeTags: (sources, target) => ipcRenderer.invoke('aitags:mergeTags', { sources, target }),
    getPostIdsByTags: (tags, mode) => ipcRenderer.invoke('aitags:postIdsByTags', { tags, mode }),
    getTagGraph: (args) => ipcRenderer.invoke('aitags:tagGraph', args),
    analyzeMissing: () => ipcRenderer.invoke('analyze:missing'),
    updatePostAiAnalysis: (id, fields) =>
      ipcRenderer.invoke('analyze:updateManual', { id, fields }),
    updatePostUserContent: (id, fields) =>
      ipcRenderer.invoke('post:updateUserContent', { id, fields }),
    clearPostDescriptions: (ids) => ipcRenderer.invoke('analyze:clearDescriptions', { ids }),
    clearPostAiTags: (ids) => ipcRenderer.invoke('analyze:clearTags', { ids }),
    suggestSearch: (query) => ipcRenderer.invoke('search:suggest', { query }),

    // AI ▸ Search (conversational chat + tag/text search). `source` scopes to
    // 'all' | 'web' | 'social' (Siti / Social / Tutto filter).
    chatSearch: (messages, activeTags) =>
      ipcRenderer.invoke('search:chat', { messages, activeTags }),
    cancelChatSearch: () => ipcRenderer.invoke('search:chatCancel'),
    searchByTags: (tags, mode, limit, offset, source) =>
      ipcRenderer.invoke('search:byTags', { tags, mode, limit, offset, source }),
    searchHybrid: (tags, textQuery, mode, limit, offset, source) =>
      ipcRenderer.invoke('search:hybrid', { tags, textQuery, mode, limit, offset, source }),
    searchByText: (query, limit, offset, source) =>
      ipcRenderer.invoke('search:byText', { query, limit, offset, source }),

    // Web references (paste a URL → screenshot + category + tags). Mirror of the
    // download/analyze bridges; key = `web:${postId}`.
    addWebReference: (url, maxPages, overwrite, singlePage) =>
      ipcRenderer.invoke('web:add', { url, maxPages, overwrite, singlePage }),
    getWebStatus: () => ipcRenderer.invoke('web:status'),
    getWebIsPaused: () => ipcRenderer.invoke('web:isPaused'),
    cancelWebJob: (key) => ipcRenderer.invoke('web:cancel', { key }),
    cancelAllWeb: () => ipcRenderer.invoke('web:cancelAll'),
    pauseWeb: () => ipcRenderer.invoke('web:pauseAll'),
    resumeWeb: () => ipcRenderer.invoke('web:resumeAll'),
    retryWebJob: (key) => ipcRenderer.invoke('web:retryJob', { key }),
    clearCompletedWeb: () => ipcRenderer.invoke('web:clearCompleted'),
    discoverWebPages: (url, maxPages) => ipcRenderer.invoke('web:discover', { url, maxPages }),
    // Manual bookmark: local files + note + tags. `payload` = { note, tags, files:
    // [{ name, mime, kind, original: Uint8Array, preview: Uint8Array|null }] }.
    //
    // Size contract (mirrors src/lib/bookmarkFiles.js): max 200 MB per file,
    // 500 MB total per invoke. The files come from the File API (picker /
    // drag&drop), so there is no disk path to stat — the raw bytes must travel
    // in the invoke payload. We therefore pre-flight the size HERE, in the
    // renderer process, so an oversized payload is rejected before it is
    // structured-cloned across the IPC boundary into the main process (the
    // main-side check in ipc.js can only run after that copy already exists).
    addManualBookmark: (payload) => {
      const byteLen = (v) => (v ? Number(v.byteLength ?? v.length) || 0 : 0);
      const files = Array.isArray(payload?.files) ? payload.files : [];
      let total = 0;
      for (const f of files) {
        const bytes = byteLen(f?.original) + byteLen(f?.preview);
        if (bytes > 200 * 1024 * 1024) return Promise.reject(new Error('too-large'));
        total += bytes;
      }
      if (total > 500 * 1024 * 1024) return Promise.reject(new Error('too-large'));
      return ipcRenderer.invoke('bookmark:add', payload);
    },
    // Web snapshots (dated version history).
    getWebSnapshots: (postId) => ipcRenderer.invoke('web:getSnapshots', { postId }),
    getWebSnapshotCounts: () => ipcRenderer.invoke('web:snapshotCounts'),
    deleteWebSites: (ids) =>
      ipcRenderer.invoke('web:deleteSites', { ids: Array.isArray(ids) ? ids : [ids] }),
    deleteWebSnapshot: (id) => ipcRenderer.invoke('web:deleteSnapshot', { id }),
    deleteWebLatestReport: (ids) =>
      ipcRenderer.invoke('web:deleteLatestReport', { ids: Array.isArray(ids) ? ids : [ids] }),

    // AI ▸ Search ▸ dettatura vocale (local whisper.cpp speech-to-text)
    sttStatus: () => ipcRenderer.invoke('stt:status'),
    sttListModels: () => ipcRenderer.invoke('stt:listModels'),
    sttSetModel: (id) => ipcRenderer.invoke('stt:setModel', { id }),
    sttDownloadModel: (id) => ipcRenderer.invoke('stt:downloadModel', { id }),
    sttPauseModelDownload: () => ipcRenderer.invoke('stt:pauseDownload'),
    sttCancelModelDownload: (id) => ipcRenderer.invoke('stt:cancelDownload', { id }),
    sttDeleteModel: (id) => ipcRenderer.invoke('stt:deleteModel', { id }),
    sttEnsure: () => ipcRenderer.invoke('stt:ensure'),
    sttTranscribe: (wav, opts) =>
      ipcRenderer.invoke('stt:transcribe', { wav, language: opts?.language }),
    sttGetTuning: () => ipcRenderer.invoke('stt:getTuning'),
    sttSetTuning: (patch) => ipcRenderer.invoke('stt:setTuning', { patch }),

    // AI ▸ embedding di testo (local llama.cpp --embedding) per il clustering tag
    embStatus: () => ipcRenderer.invoke('emb:status'),
    embListModels: () => ipcRenderer.invoke('emb:listModels'),
    embSetModel: (id) => ipcRenderer.invoke('emb:setModel', { id }),
    embDownloadModel: (id) => ipcRenderer.invoke('emb:downloadModel', { id }),
    embPauseModelDownload: () => ipcRenderer.invoke('emb:pauseDownload'),
    embCancelModelDownload: (id) => ipcRenderer.invoke('emb:cancelDownload', { id }),
    embDeleteModel: (id) => ipcRenderer.invoke('emb:deleteModel', { id }),

    // Interceptor
    saveInterceptedPosts: (posts, platform) =>
      ipcRenderer.invoke('db:bulkUpsert', { posts, platform }),

    // File / shell
    deleteLocalFiles: (postId) => ipcRenderer.invoke('db:deleteLocalFiles', { postId }),
    deletePosts: (ids) =>
      ipcRenderer.invoke('db:deletePosts', { ids: Array.isArray(ids) ? ids : [ids] }),
    openFile: () => ipcRenderer.invoke('dialog:openFile'),
    openPath: (filePath) => ipcRenderer.invoke('shell:openPath', { path: filePath }),
    showItemInFolder: (filePath) =>
      ipcRenderer.invoke('shell:showItemInFolder', { path: filePath }),
    openExternal: (url) => ipcRenderer.invoke('shell:openExternal', { url }),

    // Feedback → email allo sviluppatore (invio reale lato main, via Resend).
    sendFeedback: (message, attachments) =>
      ipcRenderer.invoke('feedback:send', { message, attachments }),

    // Push events: main → renderer (return cleanup fn)
    onNewPosts: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('interceptor:newPosts', handler);
      return () => ipcRenderer.removeListener('interceptor:newPosts', handler);
    },
    onDownloadProgress: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('download:progress', handler);
      return () => ipcRenderer.removeListener('download:progress', handler);
    },
    onAnalyzeProgress: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('analyze:progress', handler);
      return () => ipcRenderer.removeListener('analyze:progress', handler);
    },
    onWebProgress: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('web:progress', handler);
      return () => ipcRenderer.removeListener('web:progress', handler);
    },
    onChatToken: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('search:chatToken', handler);
      return () => ipcRenderer.removeListener('search:chatToken', handler);
    },
    onModelProgress: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('analyze:modelProgress', handler);
      return () => ipcRenderer.removeListener('analyze:modelProgress', handler);
    },
    onSttModelProgress: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('stt:modelProgress', handler);
      return () => ipcRenderer.removeListener('stt:modelProgress', handler);
    },
    onEmbModelProgress: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('emb:modelProgress', handler);
      return () => ipcRenderer.removeListener('emb:modelProgress', handler);
    },
    onClusterProgress: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('aitags:clusterProgress', handler);
      return () => ipcRenderer.removeListener('aitags:clusterProgress', handler);
    },
    onAliasProgress: (cb) => {
      const handler = (_, data) => cb(data);
      ipcRenderer.on('aitags:aliasProgress', handler);
      return () => ipcRenderer.removeListener('aitags:aliasProgress', handler);
    },

    webviewPreloadPath: require('url')
      .pathToFileURL(path.join(__dirname, 'webview-preload.js'))
      .toString(),
    // Fetched from the main process (keeps fs out of the preload). The renderer
    // caches the resolved string and injects it into the webview's MAIN world.
    getWebviewInjectedScript: () => ipcRenderer.invoke('getWebviewInjectedScript'),
    getWebviewSelectScript: () => ipcRenderer.invoke('getWebviewSelectScript'),
  });
} catch (err) {
  console.error('[preload] FATAL ERROR:', err.message, err.stack);
}
