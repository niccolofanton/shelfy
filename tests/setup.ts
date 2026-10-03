import { vi, beforeEach, beforeAll, type Mock } from 'vitest';
import '@testing-library/jest-dom';
import type { ElectronAPI } from '../types/electron-api';
import { loadMessages, ALL_NAMESPACES } from '../src/i18n';

// Many tests render one view or modal in isolation (no App.tsx), so none of
// its withMessages()-wrapped lazy() factories ever run (F14: most i18n
// namespaces load lazily in the real app — see src/i18n/index.tsx). Load
// every namespace once per test file, up front, so a test never sees an
// untranslated "ns.key" just because of how it chose to mount its tree.
beforeAll(() => loadMessages(ALL_NAMESPACES));

// jsdom has no IntersectionObserver; the Gallery's infinite-scroll sentinel
// needs it to exist. A no-op stub is enough for component tests.
if (typeof global.IntersectionObserver === 'undefined') {
  global.IntersectionObserver = class {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
    takeRecords(): IntersectionObserverEntry[] {
      return [];
    }
  } as unknown as typeof IntersectionObserver;
}

// jsdom has no PointerEvent constructor: `fireEvent.pointerDown/Move/Up` (and
// a bare `new PointerEvent(...)`) silently fall back to a plain Event that
// carries none of clientX/clientY/pointerType/pointerId — the fields PostCard
// (useLongPress, tap-to-preview) and InfiniteCanvas (pan/pinch) read directly
// off the event (P1-02). A minimal polyfill — MouseEvent for the client
// coordinates, plus the PointerEvent-only fields — is enough for tests.
// `extension/tests/**` and other node-environment suites share this setup
// file but have no `MouseEvent` at all, hence the guard.
if (typeof MouseEvent !== 'undefined' && typeof global.PointerEvent === 'undefined') {
  class PointerEventPolyfill extends MouseEvent {
    readonly pointerId: number;
    readonly pointerType: string;
    readonly isPrimary: boolean;
    constructor(type: string, params: PointerEventInit = {}) {
      super(type, params);
      this.pointerId = params.pointerId ?? 0;
      this.pointerType = params.pointerType ?? '';
      this.isPrimary = params.isPrimary ?? false;
    }
  }
  global.PointerEvent = PointerEventPolyfill as unknown as typeof PointerEvent;
}

// Default resolved/return values for every electronAPI method, applied both at
// initial mock creation and after each test's reset. Adding a new IPC method
// only requires one entry here. Tests override specific methods as needed.
const API_DEFAULTS: Record<string, unknown> = {
  getPosts: { posts: [], total: 0 },
  getStats: { total: 0, byPlatform: { instagram: 0, twitter: 0 }, byMediaType: {}, downloaded: 0 },
  importJSON: { imported: 0 },
  downloadPost: {},
  downloadPosts: { queued: 0 },
  downloadAll: undefined,
  getDownloadStatus: [],
  cancelDownload: undefined,
  saveInterceptedPosts: undefined,
  openFile: null,
  openPath: undefined,
  getCollections: [],
  createCollection: { id: 1, name: '', color: '#3d5afe', count: 0 },
  updateCollection: undefined,
  deleteCollection: undefined,
  addPostsToCollections: { added: 0 },
  removePostFromCollection: undefined,
  // Manual edits (P1-06 seam: ShelfyClient.updatePost)
  updatePostUserContent: undefined,
  updatePostAiAnalysis: undefined,
  // Bulk / id helpers
  getPostIds: [],
  getPostsByIds: [],
  // P1-14 bulk seam (electronClient.bulkAction's desktop cases)
  deletePosts: { deleted: 0, errors: [] },
  clearPostDescriptions: 0,
  clearPostAiTags: 0,
  repairPreview: true,
  getSearchProviders: [{ id: 'local', name: 'Locale', selected: true }],
  selectSearchProvider: [{ id: 'local', name: 'Locale', selected: true }],
  // Analysis (local VLM)
  analyzePost: { queued: true },
  analyzePosts: { queued: 0 },
  // Default undefined: the gallery's analyze handler then falls back to enqueuing
  // the whole selection. Tests of the local/remote split override this explicitly.
  splitForAnalysis: undefined,
  analyzeAll: { queued: 0 },
  analyzeMissing: { queued: 0 },
  getAnalyzeStatus: [],
  getAnalyzeIsPaused: false,
  getAnalyzeConcurrency: { value: 1, max: 1 },
  cancelAnalyzeJob: undefined,
  cancelAllAnalyze: undefined,
  clearAllAnalyze: undefined,
  retryAnalyzeJob: undefined,
  clearCompletedAnalyze: undefined,
  getModelStatus: {
    ready: true,
    downloading: false,
    files: { model: true, mmproj: true },
    name: 'test-model',
  },
  downloadModel: { ready: true },
  // AI setup / onboarding — the defaults paint a fully-configured pipeline so the
  // App onboarding gate stays closed unless a test opts into the incomplete state.
  listModels: [
    {
      id: 'qwen3vl-8b',
      name: 'Qwen3-VL 8B',
      tier: 'Bilanciato',
      note: '',
      sizeGB: 6.2,
      minRamGB: 16,
      recommended: true,
      ready: true,
      partial: false,
      active: true,
      downloading: false,
    },
  ],
  sttListModels: [
    {
      id: 'whisper-turbo-q5',
      name: 'Whisper Large v3 Turbo (q5)',
      tier: 'Qualità leggera',
      note: '',
      sizeGB: 0.55,
      sizeLabel: '547 MB',
      recommended: true,
      ready: true,
      partial: false,
      active: true,
      downloading: false,
    },
  ],
  embListModels: [
    {
      id: 'e5-small',
      name: 'multilingual-e5-small',
      tier: 'Embedding',
      note: '',
      sizeGB: 0.12,
      sizeLabel: '120 MB',
      recommended: true,
      ready: true,
      partial: false,
      active: true,
      downloading: false,
    },
  ],
  sttDownloadModel: undefined,
  embDownloadModel: undefined,
  getBinariesStatus: { ready: true, present: {}, missing: 0, variant: 'metal' },
  ensureBinaries: undefined,
  getVariantState: {
    variant: 'metal',
    explicit: false,
    failed: [],
    effective: 'metal',
    recommended: 'metal',
  },
  getHardwareInfo: {
    hardware: {
      platform: 'darwin',
      arch: 'arm64',
      appleSilicon: true,
      totalRamGB: 16,
      cpu: { logical: 8, physical: 8, perf: 4 },
      gpu: { vendor: 'apple', name: 'Apple GPU', vramGB: 11, cuda: false, unified: true },
      recommendedVariant: 'metal',
    },
    tuning: {},
    recommendedModelId: 'qwen3vl-8b',
    recommendedVariant: 'metal',
  },
  // AI Tags tab
  getTaxonomy: { categories: [], contentTypes: [] },
  getAiOverview: {
    total: 0,
    analyzed: 0,
    unanalyzed: 0,
    byCategory: [],
    byContentType: [],
    languages: [],
    uniqueTags: 0,
    taggedPosts: 0,
  },
  getTagStats: [],
  getEntityStats: [],
  getTagCooccurrence: [],
  getTagClusters: [],
  getTagMergeSuggestions: [],
  getTagHealth: { orphanTags: [], rareTags: 0, unanalyzedPosts: 0, untaggedPosts: 0 },
  renameTag: { updated: 0 },
  mergeTags: { updated: 0 },
  getPostIdsByTags: [],
  // Cluster review (aitags:cluster:*)
  regenerateClusters: {},
  cancelClusters: { cancelled: false },
  acceptCluster: { updated: 0 },
  dismissCluster: { updated: 0 },
  renameCluster: { updated: 0 },
  removeTagFromCluster: { removed: 0 },
  // Alias review (aitags:alias(es):*)
  getTagAliases: [],
  proposeAliases: { ok: true, proposed: 0 },
  cancelAliases: { cancelled: false },
  acceptAlias: { ok: true, rewritten: 0 },
  dismissAlias: { ok: true, rewritten: 0 },
  // AI ▸ Search (chat + tag/text/hybrid search)
  chatSearch: {
    reply: '',
    tagsToAdd: [],
    tagsToRemove: [],
    keywordsToAdd: [],
    tagGroups: { broad: [], specific: [], keywords: [] },
    modelUsed: false,
  },
  cancelChatSearch: { ok: true },
  searchByTags: { posts: [], total: 0 },
  searchHybrid: { posts: [], total: 0 },
  searchByText: { posts: [], total: 0 },
  suggestSearch: { tags: [] },
  // AI ▸ Search ▸ dictation (local whisper.cpp)
  sttStatus: { modelReady: true, binaryReady: true, ready: true, downloading: false },
  sttEnsure: { ok: true },
  sttTranscribe: { text: '' },
};

// Event subscribers return an unsubscribe function.
const EVENT_SUBS: string[] = [
  'onNewPosts',
  'onDownloadProgress',
  'onAnalyzeProgress',
  'onModelProgress',
  'onSttModelProgress',
  'onEmbModelProgress',
  'onBinariesProgress',
  'onClusterProgress',
  'onAliasProgress',
  'onChatToken',
];

// Provide a window.electronAPI mock for all jsdom-environment tests.
if (typeof window !== 'undefined') {
  // Pin the UI language to Italian so text-based assertions match the (verbatim)
  // Italian locale regardless of the host's navigator.language. Production
  // auto-detects; tests must be deterministic.
  try {
    window.localStorage.setItem('app:language', 'it');
  } catch {
    /* storage unavailable */
  }

  // The assembled mock is addressed by string method name; a loose record keeps
  // the dynamic build/reset readable while it is exposed as a typed ElectronAPI.
  window.electronAPI = {} as unknown as ElectronAPI;
  const api = window.electronAPI as unknown as Record<string, Mock>;

  const applyDefaults = (): void => {
    for (const [name, value] of Object.entries(API_DEFAULTS)) {
      api[name].mockResolvedValue(value);
    }
    for (const name of EVENT_SUBS) {
      api[name].mockReturnValue(() => {});
    }
  };

  for (const name of Object.keys(API_DEFAULTS)) api[name] = vi.fn();
  for (const name of EVENT_SUBS) api[name] = vi.fn();
  applyDefaults();

  // Reset all electronAPI mocks between tests so state doesn't bleed, then
  // re-apply the defaults declared above.
  beforeEach(() => {
    try {
      window.localStorage.setItem('app:language', 'it');
    } catch {
      /* storage unavailable */
    }
    Object.values(api).forEach((fn) => {
      if (typeof fn?.mockReset === 'function') fn.mockReset();
    });
    applyDefaults();
  });
}
