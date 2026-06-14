import { vi, beforeEach } from 'vitest';
import '@testing-library/jest-dom';

// jsdom has no IntersectionObserver; the Gallery's infinite-scroll sentinel
// needs it to exist. A no-op stub is enough for component tests.
if (typeof global.IntersectionObserver === 'undefined') {
  global.IntersectionObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
    takeRecords() {
      return [];
    }
  };
}

// Default resolved/return values for every electronAPI method, applied both at
// initial mock creation and after each test's reset. Adding a new IPC method
// only requires one entry here. Tests override specific methods as needed.
const API_DEFAULTS = {
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
  // Bulk / id helpers
  getPostIds: [],
  getPostsByIds: [],
  // Analysis (local VLM)
  analyzePost: { queued: true },
  analyzePosts: { queued: 0 },
  // Default undefined: the gallery's analyze handler then falls back to enqueuing
  // the whole selection. Tests of the local/remote split override this explicitly.
  splitForAnalysis: undefined,
  analyzeAll: { queued: 0 },
  analyzeMissing: { queued: 0 },
  getAnalyzeStatus: [],
  cancelAnalyzeJob: undefined,
  cancelAllAnalyze: undefined,
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
};

// Event subscribers return an unsubscribe function.
const EVENT_SUBS = [
  'onNewPosts',
  'onDownloadProgress',
  'onAnalyzeProgress',
  'onModelProgress',
  'onSttModelProgress',
  'onEmbModelProgress',
  'onBinariesProgress',
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

  const applyDefaults = () => {
    for (const [name, value] of Object.entries(API_DEFAULTS)) {
      window.electronAPI[name].mockResolvedValue(value);
    }
    for (const name of EVENT_SUBS) {
      window.electronAPI[name].mockReturnValue(() => {});
    }
  };

  window.electronAPI = {};
  for (const name of Object.keys(API_DEFAULTS)) window.electronAPI[name] = vi.fn();
  for (const name of EVENT_SUBS) window.electronAPI[name] = vi.fn();
  applyDefaults();

  // Reset all electronAPI mocks between tests so state doesn't bleed, then
  // re-apply the defaults declared above.
  beforeEach(() => {
    try {
      window.localStorage.setItem('app:language', 'it');
    } catch {
      /* storage unavailable */
    }
    Object.values(window.electronAPI).forEach((fn) => {
      if (typeof fn?.mockReset === 'function') fn.mockReset();
    });
    applyDefaults();
  });
}
