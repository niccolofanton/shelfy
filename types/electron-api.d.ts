// Contratto del bridge preload ⇄ renderer (`window.electronAPI`).
//
// Questa interfaccia è la SINGLE SOURCE OF TRUTH del confine IPC. Viene
// popolata durante l'ondata di conversione di `electron/` leggendo
// `electron/preload.ts` (le firme) e `electron/ipc.ts` (i tipi di ritorno
// reali). `electron/preload.ts` implementa l'oggetto come `ElectronAPI`, così
// implementazione e contratto restano allineati a compile-time.
//
// RETURN TYPES: derived by tracing each ipcRenderer.invoke channel back to its
// ipcMain.handle in electron/ipc.js, then to the underlying db/manager function.
// Where a Shelfy.* domain type fits the traced shape it is used directly. Some
// channels return file-internal runtime shapes from modules not yet converted
// (analyzer/updater/stt/embeddings/binaries job records, model status, tuning,
// hardware info): those have no domain type, so they are typed `Promise<unknown>`
// and the renderer narrows them. Each `unknown` is intentional, not a stand-in
// for `any`.

// ── Bridge-local payload/result shapes ─────────────────────────────────────────
// These describe IPC payloads/results that have no Shelfy.* domain type because
// they are pure boundary shapes (option bags, command acks, progress events).

// Optional fields accepted by createCollection beyond name/color (collections:create).
export interface CreateCollectionOpts {
  platform?: Shelfy.Platform | null;
  externalId?: string | null;
  igName?: string | null;
}

// db:existingIds / db:savedByKeys: the scraper's "already in library" lookups.
export interface SavedByKey {
  key: string;
  id: string;
}

// Generic { ok } / { ok, ... } command acknowledgement returned by many handlers.
export interface OkResult {
  ok: boolean;
}

// db:deleteLocalFiles
export interface DeleteLocalFilesResult {
  ok: boolean;
  deleted: number;
  errors: string[];
}

// db:deletePosts / web:deleteSites
export interface DeletePostsResult {
  ok: boolean;
  deleted: number;
  errors: string[];
}

// db:bulkUpsert (saveInterceptedPosts)
export interface BulkUpsertResult {
  inserted: number;
  skipped: number;
}

// db:exportJSON
export interface ExportResult {
  canceled: boolean;
  count?: number;
  filePath?: string;
  error?: string;
}

// collections:delete
export interface DeleteCollectionResult {
  ok: boolean;
  deletedPosts: number;
  errors: string[];
}

// collections:addPosts
export interface AddPostsResult {
  added: number;
}

// download:* / analyze:* enqueue handlers
export interface QueuedResult {
  queued: number;
}

// analyze:post enqueues a single post, so it returns a boolean ack (queued vs
// skipped) — unlike the analyze:all / download:* family, which return a count.
export interface AnalyzePostResult {
  queued: boolean;
}

// analyze:split
export interface AnalyzeSplitResult {
  analyzable: string[];
  needsDownload: string[];
}

// analyze:getConcurrency
export interface ConcurrencyInfo {
  value: number;
  max: number;
}

// search:suggest
export interface SearchSuggestResult {
  tags: string[];
}

// A multi-turn AI-search chat result (search:chat → analyzer.chatSearch).
export interface ChatSearchResult {
  reply: string;
  tagsToAdd: string[];
  tagsToRemove: string[];
  keywordsToAdd: string[];
  tagGroups: {
    broad: string[];
    specific: string[];
    keywords: string[];
  };
  modelUsed: boolean;
}

// A page of ranked posts (search:byTags / search:hybrid / search:byText).
export interface PostSearchResult {
  posts: Shelfy.Post[];
  total: number;
}

// aitags:cluster:cancel / aitags:aliases:cancel
export interface CancelledResult {
  cancelled: boolean;
}

// aitags:aliases:propose
export interface ProposeAliasesResult {
  ok: boolean;
  proposed: number;
}

// aitags:cluster:setStatus / aitags:cluster:removeTag / aitags:renameTag /
// aitags:mergeTags — DB mutation results that report affected-row counts.
export interface ClusterStatusResult {
  updated: number;
}
export interface RemoveTagResult {
  removed: number;
}
export interface MergeTagsResult {
  updated: number;
}

// aitags:alias:accept / aitags:alias:dismiss
export interface AliasStatusResult {
  ok: boolean;
  rewritten: number;
}

// web:deleteSnapshot
export interface DeleteSnapshotResult {
  ok: boolean;
  errors: string[];
}

// web:deleteLatestReport
export interface DeleteLatestReportResult {
  ok: boolean;
  promoted: number;
  cleared: number;
  errors: string[];
}

// stt:transcribe — either a transcription payload or an error (validation in ipc.js).
export interface SttTranscribeResult {
  text?: string;
  error?: string;
  [key: string]: unknown;
}

// addManualBookmark (bookmark:add → bookmarks.addManualBookmark)
export interface ManualBookmarkResult {
  id: string;
}

// addManualBookmark renderer payload: local files + note + tags. Raw bytes
// travel over IPC because the File API exposes no disk path (see preload).
export interface ManualBookmarkFile {
  name?: string;
  mime?: string;
  kind?: string;
  size?: number;
  original?: Uint8Array | ArrayBuffer | number[] | null;
  preview?: Uint8Array | ArrayBuffer | number[] | null;
}
export interface ManualBookmarkPayload {
  note?: string;
  tags?: string[];
  files?: ManualBookmarkFile[];
}

// feedback:send attachment + payload (mirrors electron/feedback.ts FeedbackResult).
export interface FeedbackAttachment {
  filename?: string;
  content?: string;
}
export interface FeedbackResult {
  ok: boolean;
  error?: string;
  [key: string]: unknown;
}

// stt:transcribe options bag (only `language` crosses the bridge).
export interface SttTranscribeOpts {
  language?: string;
}

// The contextBridge surface exposed on `window.electronAPI`. Arguments mirror
// electron/preload.ts; returns are traced from electron/ipc.js handlers.
export interface ElectronAPI {
  // ── DB ──────────────────────────────────────────────────────────────────────
  getPosts: (filters?: unknown) => Promise<PostSearchResult>;
  getPostIds: (filters?: unknown) => Promise<string[]>;
  getPostsByIds: (ids: string[]) => Promise<Shelfy.Post[]>;
  existingIds: (ids: string[]) => Promise<string[]>;
  savedByKeys: (keys: string[]) => Promise<SavedByKey[]>;
  getStats: () => Promise<Shelfy.Stats>;
  importJSON: (filePath: string) => Promise<Shelfy.ImportResult>;
  exportJSON: (platforms?: Shelfy.Platform[]) => Promise<ExportResult>;
  clearAllData: () => Promise<OkResult>;
  clearAllAiAnalysis: () => Promise<void>;
  clearAllAssets: () => Promise<OkResult>;

  // ── Custom sources (collections) ──────────────────────────────────────────────
  getCollections: () => Promise<Shelfy.Collection[]>;
  createCollection: (
    name: string,
    color: string,
    opts?: CreateCollectionOpts,
  ) => Promise<Shelfy.Collection>;
  updateCollection: (id: number, fields?: { name?: string; color?: string }) => Promise<void>;
  deleteCollection: (
    id: number,
    opts?: { deletePosts?: boolean },
  ) => Promise<DeleteCollectionResult>;
  addPostsToCollections: (postIds: string[], collectionIds: number[]) => Promise<AddPostsResult>;
  removePostFromCollection: (postId: string, collectionId: number) => Promise<void>;

  // ── Downloads — enqueue ───────────────────────────────────────────────────────
  downloadPost: (postId: string, assetTypes?: string[]) => Promise<QueuedResult>;
  downloadPosts: (
    ids: string[],
    assetTypes?: string[],
    missingOnly?: boolean,
  ) => Promise<QueuedResult>;
  downloadAll: (assetTypes?: string[], missingOnly?: boolean) => Promise<QueuedResult>;

  // ── Downloads — status ────────────────────────────────────────────────────────
  // downloader.getJobs() returns runtime job records (electron/downloader.ts
  // DownloadJobRecord, not the DB DownloadJob row) — file-internal, untyped here.
  getDownloadStatus: () => Promise<unknown[]>;
  getDownloadIsPaused: () => Promise<boolean>;

  // ── Downloads — global controls ───────────────────────────────────────────────
  // pause/resume/cancelAll/clearCompleted return manager-internal values; untyped.
  pauseDownloads: () => Promise<unknown>;
  resumeDownloads: () => Promise<unknown>;
  cancelAllDownloads: () => Promise<unknown>;
  clearCompletedDownloads: () => Promise<unknown>;

  // ── Downloads — per-job controls ──────────────────────────────────────────────
  cancelDownloadJob: (key: string) => Promise<unknown>;
  retryDownloadJob: (key: string) => Promise<unknown>;

  // ── Analyze (local VLM categorization) ────────────────────────────────────────
  analyzePost: (postId: string) => Promise<AnalyzePostResult>;
  analyzeAll: () => Promise<QueuedResult>;
  analyzePosts: (postIds: string[]) => Promise<QueuedResult>;
  splitForAnalysis: (postIds: string[]) => Promise<AnalyzeSplitResult>;
  // analyzer.getJobs() returns runtime analyze-job records; file-internal, untyped.
  getAnalyzeStatus: () => Promise<unknown[]>;
  cancelAnalyzeJob: (key: string) => Promise<unknown>;
  cancelAllAnalyze: () => Promise<unknown>;
  clearAllAnalyze: () => Promise<unknown>;
  clearCompletedAnalyze: () => Promise<unknown>;
  pauseAnalyze: () => Promise<unknown>;
  resumeAnalyze: () => Promise<unknown>;
  getAnalyzeIsPaused: () => Promise<boolean>;
  retryAnalyzeJob: (key: string) => Promise<unknown>;
  // model status / list / tuning / hardware: analyzer-internal shapes; untyped.
  getModelStatus: () => Promise<unknown>;
  listModels: () => Promise<unknown[]>;
  setModel: (id: string) => Promise<unknown>;
  getAnalyzeConcurrency: () => Promise<ConcurrencyInfo>;
  setAnalyzeConcurrency: (n: number) => Promise<unknown>;
  getHardwareInfo: () => Promise<unknown>;
  getAnalyzeTuning: () => Promise<unknown>;
  setAnalyzeTuning: (patch: unknown) => Promise<unknown>;
  downloadModel: (id: string) => Promise<unknown>;
  pauseModelDownload: () => Promise<unknown>;
  cancelModelDownload: (id: string) => Promise<unknown>;
  deleteModel: (id: string) => Promise<unknown>;
  // analyzer.getTaxonomy() returns the category/content-type taxonomy; untyped.
  getTaxonomy: () => Promise<unknown>;

  // ── App / updates ─────────────────────────────────────────────────────────────
  getAppVersion: () => Promise<string>;
  // updater.* return channel strings and an updater-internal state object; untyped.
  getUpdateChannel: () => Promise<unknown>;
  setUpdateChannel: (channel: string) => Promise<unknown>;
  getUpdateState: () => Promise<unknown>;
  checkForUpdates: () => Promise<unknown>;
  quitAndInstallUpdate: () => Promise<unknown>;
  openUpdateDownload: () => Promise<unknown>;
  rebuildUpdate: () => Promise<unknown>;
  onUpdaterState: (cb: (data: unknown) => void) => () => void;

  // ── Runtime sidecar binaries ──────────────────────────────────────────────────
  // binaries.* return binaries-internal status/variant shapes; untyped.
  getBinariesStatus: () => Promise<unknown>;
  ensureBinaries: (force?: boolean) => Promise<unknown>;
  getLlamaVariant: () => Promise<unknown>;
  setLlamaVariant: (variant: string) => Promise<unknown>;
  getVariantState: () => Promise<unknown>;
  onBinariesProgress: (cb: (data: unknown) => void) => () => void;
  onVariantFallback: (cb: (data: unknown) => void) => () => void;

  // ── AI Tags ───────────────────────────────────────────────────────────────────
  getAiOverview: () => Promise<Shelfy.AiOverview>;
  getTagStats: (args?: { limit?: number; tier?: Shelfy.TagTier | null }) => Promise<Shelfy.Tag[]>;
  getEntityStats: (args?: { limit?: number }) => Promise<Shelfy.Entity[]>;
  getTagCooccurrence: (tag: string, limit?: number) => Promise<Shelfy.TagCount[]>;
  getTagClusters: (args?: { maxClusters?: number }) => Promise<Shelfy.TagCluster[]>;
  // cluster:regenerate resolves to analyzer.clusterTags()'s run summary; untyped.
  regenerateClusters: () => Promise<unknown>;
  cancelClusters: () => Promise<CancelledResult>;
  acceptCluster: (id: number) => Promise<ClusterStatusResult>;
  dismissCluster: (id: number) => Promise<ClusterStatusResult>;
  renameCluster: (id: number, label: string) => Promise<ClusterStatusResult>;
  removeTagFromCluster: (tag: string, clusterId: number) => Promise<RemoveTagResult>;
  proposeAliases: () => Promise<ProposeAliasesResult>;
  cancelAliases: () => Promise<CancelledResult>;
  getTagAliases: (args?: { status?: Shelfy.AliasStatus | null }) => Promise<Shelfy.TagAlias[]>;
  acceptAlias: (aliasNorm: string) => Promise<AliasStatusResult>;
  dismissAlias: (aliasNorm: string) => Promise<AliasStatusResult>;
  getTagMergeSuggestions: (args?: { limit?: number }) => Promise<Shelfy.TagMergeSuggestion[]>;
  getTagHealth: () => Promise<Shelfy.TagHealth>;
  renameTag: (from: string, to: string) => Promise<MergeTagsResult>;
  mergeTags: (sources: string[], target: string) => Promise<MergeTagsResult>;
  getPostIdsByTags: (tags: string[], mode?: 'and' | 'or') => Promise<string[]>;
  getTagGraph: (args?: { maxNodes?: number; minEdgeWeight?: number }) => Promise<Shelfy.TagGraph>;
  analyzeMissing: () => Promise<QueuedResult>;
  updatePostAiAnalysis: (id: string, fields: unknown) => Promise<void>;
  updatePostUserContent: (
    id: string,
    fields: { note?: string | null; manualTags?: string[] | null },
  ) => Promise<void>;
  clearPostDescriptions: (ids: string[]) => Promise<number>;
  clearPostAiTags: (ids: string[]) => Promise<number>;
  suggestSearch: (query: string) => Promise<SearchSuggestResult>;

  // ── AI ▸ Search (conversational chat + tag/text search) ─────────────────────────
  chatSearch: (messages: unknown[], activeTags?: string[]) => Promise<ChatSearchResult>;
  cancelChatSearch: () => Promise<OkResult>;
  searchByTags: (
    tags: string[],
    mode?: 'and' | 'or',
    limit?: number,
    offset?: number,
    source?: 'all' | 'web' | 'social',
  ) => Promise<PostSearchResult>;
  searchHybrid: (
    tags: string[],
    textQuery: string,
    mode?: 'and' | 'or',
    limit?: number,
    offset?: number,
    source?: 'all' | 'web' | 'social',
  ) => Promise<PostSearchResult>;
  searchByText: (
    query: string,
    limit?: number,
    offset?: number,
    source?: 'all' | 'web' | 'social',
  ) => Promise<PostSearchResult>;

  // ── Web references ──────────────────────────────────────────────────────────────
  // web:add / web:discover resolve to weborchestrator-internal shapes; untyped.
  addWebReference: (
    url: string,
    maxPages?: number,
    overwrite?: boolean,
    singlePage?: boolean,
  ) => Promise<unknown>;
  getWebStatus: () => Promise<unknown[]>;
  getWebIsPaused: () => Promise<boolean>;
  cancelWebJob: (key: string) => Promise<unknown>;
  cancelAllWeb: () => Promise<unknown>;
  pauseWeb: () => Promise<unknown>;
  resumeWeb: () => Promise<unknown>;
  retryWebJob: (key: string) => Promise<unknown>;
  clearCompletedWeb: () => Promise<unknown>;
  discoverWebPages: (url: string, maxPages?: number) => Promise<unknown>;
  addManualBookmark: (payload: ManualBookmarkPayload) => Promise<ManualBookmarkResult>;

  // ── Web snapshots ─────────────────────────────────────────────────────────────
  getWebSnapshots: (postId: string) => Promise<Shelfy.WebSnapshot[]>;
  getWebSnapshotCounts: () => Promise<Record<string, number>>;
  deleteWebSites: (ids: string | string[]) => Promise<DeletePostsResult>;
  deleteWebSnapshot: (id: number) => Promise<DeleteSnapshotResult>;
  deleteWebLatestReport: (ids: string | string[]) => Promise<DeleteLatestReportResult>;

  // ── Speech-to-text (local whisper.cpp) ────────────────────────────────────────
  // stt.* return stt-internal status/model/tuning shapes; untyped.
  sttStatus: () => Promise<unknown>;
  sttListModels: () => Promise<unknown[]>;
  sttSetModel: (id: string) => Promise<unknown>;
  sttDownloadModel: (id: string) => Promise<unknown>;
  sttPauseModelDownload: () => Promise<unknown>;
  sttCancelModelDownload: (id: string) => Promise<unknown>;
  sttDeleteModel: (id: string) => Promise<unknown>;
  sttEnsure: () => Promise<OkResult>;
  sttTranscribe: (
    wav: ArrayBuffer | ArrayBufferView,
    opts?: SttTranscribeOpts,
  ) => Promise<SttTranscribeResult>;
  sttGetTuning: () => Promise<unknown>;
  sttSetTuning: (patch: unknown) => Promise<unknown>;

  // ── Text embeddings (local llama.cpp --embedding) ─────────────────────────────
  // embeddings.* return embeddings-internal status/model shapes; untyped.
  embStatus: () => Promise<unknown>;
  embListModels: () => Promise<unknown[]>;
  embSetModel: (id: string) => Promise<unknown>;
  embDownloadModel: (id: string) => Promise<unknown>;
  embPauseModelDownload: () => Promise<unknown>;
  embCancelModelDownload: (id: string) => Promise<unknown>;
  embDeleteModel: (id: string) => Promise<unknown>;

  // ── Interceptor ───────────────────────────────────────────────────────────────
  saveInterceptedPosts: (posts: unknown[], platform: Shelfy.Platform) => Promise<BulkUpsertResult>;

  // ── File / shell ──────────────────────────────────────────────────────────────
  deleteLocalFiles: (postId: string) => Promise<DeleteLocalFilesResult>;
  deletePosts: (ids: string | string[]) => Promise<DeletePostsResult>;
  openFile: () => Promise<string | null>;
  // shell.openPath resolves to '' on success or an error string; showItemInFolder
  // resolves void; both can short-circuit to { ok: false } when out of userData.
  openPath: (filePath: string) => Promise<string | OkResult>;
  showItemInFolder: (filePath: string) => Promise<void | OkResult>;
  openExternal: (url: string) => Promise<void | OkResult>;

  // ── Feedback ──────────────────────────────────────────────────────────────────
  sendFeedback: (message: string, attachments?: FeedbackAttachment[]) => Promise<FeedbackResult>;

  // ── Push events: main → renderer (return cleanup fn) ──────────────────────────
  onNewPosts: (cb: (data: unknown) => void) => () => void;
  onDownloadProgress: (cb: (data: unknown) => void) => () => void;
  onAnalyzeProgress: (cb: (data: unknown) => void) => () => void;
  onWebProgress: (cb: (data: unknown) => void) => () => void;
  onChatToken: (cb: (data: unknown) => void) => () => void;
  onModelProgress: (cb: (data: unknown) => void) => () => void;
  onSttModelProgress: (cb: (data: unknown) => void) => () => void;
  onEmbModelProgress: (cb: (data: unknown) => void) => () => void;
  onClusterProgress: (cb: (data: unknown) => void) => () => void;
  onAliasProgress: (cb: (data: unknown) => void) => () => void;

  // ── Webview injection scripts ─────────────────────────────────────────────────
  webviewPreloadPath: string;
  getWebviewInjectedScript: () => Promise<string>;
  getWebviewSelectScript: () => Promise<string>;
}

declare global {
  interface Window {
    electronAPI: ElectronAPI;
  }
}
