import { describe, it, expect, beforeAll, afterAll, beforeEach, vi } from 'vitest';
import { tmpdir } from 'os';
import { join } from 'path';
import { existsSync, unlinkSync, writeFileSync } from 'fs';
import type { Database as DatabaseType } from 'better-sqlite3';

// Loose camelCase post-input shape the upsert/import helpers accept. db.js's own
// PostInput type is file-internal, so the fixtures use a minimal mirror.
interface PostInput {
  id: string;
  platform?: string;
  shortcode?: string | null;
  postUrl?: string | null;
  profileUrl?: string | null;
  authorUsername?: string | null;
  authorName?: string | null;
  text?: string | null;
  thumbnailUrl?: string | null;
  mediaType?: string | null;
  timestamp?: string | null;
}

interface SeedOverrides {
  text?: string;
  mediaType?: string;
  timestamp?: string;
}
interface SeedAi {
  description?: string;
  tags?: string[];
  status?: string;
  model?: string;
  category?: string;
  contentType?: string;
  entities?: string[];
  keywords?: string[];
  language?: string;
  saveReason?: string;
}

// Public surface of db.js exercised by this suite. Domain return types reuse the
// global Shelfy.* shapes; file-internal helpers are typed against the shapes the
// assertions read.
interface DbModule {
  initialize: () => DatabaseType;
  invalidateGlobalCaches: () => void;
  upsertPost: (post: PostInput) => Shelfy.UpsertResult;
  bulkUpsert: (posts: PostInput[]) => unknown;
  getPost: (id: string) => Shelfy.Post | null;
  getPosts: (filters?: Record<string, unknown>) => { posts: Shelfy.Post[]; total: number };
  getPostIds: (filters?: Record<string, unknown>) => string[];
  getPostsByIds: (ids: string[]) => Shelfy.Post[];
  getPostsForAnalysis: (args?: { ids?: string[]; missingOnly?: boolean }) => Shelfy.AnalysisPost[];
  getStats: () => Shelfy.Stats;
  getAiOverview: () => Shelfy.AiOverview;
  getTagStats: (args?: { limit?: number }) => Shelfy.Tag[];
  getEntityStats: (args?: { limit?: number }) => Shelfy.Entity[];
  getTagCooccurrence: (tag: string, limit?: number) => Shelfy.TagCount[];
  getTagClusters: (args?: { maxClusters?: number }) => Shelfy.TagCluster[];
  getTagMergeSuggestions: (args?: { limit?: number }) => Shelfy.TagMergeSuggestion[];
  getTagHealth: () => Shelfy.TagHealth;
  getTagCandidateGroups: (args?: {
    minTagCount?: number;
    minJaccard?: number;
  }) => Promise<Shelfy.TagCandidateGroup[]>;
  buildTagCommunities: (
    freq: Map<string, number>,
    edges: Array<{ a: string; b: string; c: number }>,
    opts?: { minJaccard?: number },
  ) => string[][];
  searchTagsByText: (
    query: string,
    opts?: { limit?: number; minTermLen?: number },
  ) => Shelfy.TagCount[];
  getFrequentTags: (limit?: number) => string[];
  getPostIdsByTags: (tags: string[], mode?: 'and' | 'or') => string[];
  updatePaths: (
    id: string,
    fields: {
      thumbnailPath?: string | null;
      imagePath?: string | null;
      videoPath?: string | null;
      thumbBlur?: string | null;
    },
  ) => void;
  setThumbBlur: (id: string, thumbBlur: string) => void;
  listPostsMissingThumbBlur: () => Array<{ id: string; src: string }>;
  updateAiAnalysis: (id: string, fields: Record<string, unknown>) => void;
  saveClusterRun: (clusters: Array<{ label?: string; tags?: string[] }>) => {
    runId: number;
    count: number;
  };
  setClusterStatus: (id: number, status: string) => { updated: number };
  renameCluster: (id: number, label: string) => { updated: number };
  removeTagFromCluster: (tag: string, clusterId: number) => { removed: number };
  renameTag: (from: string, to: string) => { updated: number };
  mergeTags: (sources: string[], target: string) => { updated: number };
  importFromJSON: (filePath: string) => Promise<Shelfy.ImportResult>;
  __termIdfWeights: (terms: string[], cols: string[]) => Record<string, number>;
  __backfillDerivedTags: () => void;
}

// db.ts imports `{ app } from 'electron'`; mocking the bare module makes its
// app.getPath('userData') resolve to tmpdir() so the test DB lands there.
// vi.mock with no factory auto-resolves to __mocks__/electron.ts at the repo
// root (export const app = { getPath: () => tmpdir() }).
vi.mock('electron');

// ABI guard: better-sqlite3 may be compiled for Electron's ABI rather than the
// current Node's. When that happens the native binding throws on the first
// `new Database()`; skip the suites that need the DB instead of failing the run.
// db.ts itself imports cleanly (it loads the native binding lazily), so the
// module namespace is available for pure helpers even when initialize() fails.
let db: DbModule | undefined;
let dbInstance: DatabaseType | undefined;
let loadErr: unknown = null;
try {
  db = (await import('../../electron/db')) as unknown as DbModule;
  // Force the native .node to load now (better-sqlite3 loads it lazily on the
  // first `new Database()`), so an ABI mismatch is caught here, not in beforeAll.
  dbInstance = db.initialize();
} catch (e) {
  loadErr = e;
}
const nativeMismatch = !!loadErr && /NODE_MODULE_VERSION|ERR_DLOPEN_FAILED/.test(String(loadErr));
const d = loadErr ? describe.skip : describe;
// Pure helpers don't touch the native binding, so they run even when only
// initialize() (new Database) failed the ABI guard, as long as the module loaded.
const pure = db ? describe : describe.skip;

// Non-null views for use inside the (skipped-on-error) suites below, where db and
// dbInstance are guaranteed loaded.
const DB = (): DbModule => db as DbModule;
const SQL = (): DatabaseType => dbInstance as DatabaseType;

const dbPath = join(tmpdir(), 'shelfy.sqlite');

const igPost: PostInput = {
  id: 'ig-1',
  platform: 'instagram',
  shortcode: 'ABC123',
  postUrl: 'https://www.instagram.com/p/ABC123/',
  profileUrl: 'https://www.instagram.com/testuser/',
  authorUsername: 'testuser',
  authorName: '',
  text: 'Test caption',
  thumbnailUrl: 'https://cdn.example.com/thumb.jpg',
  mediaType: 'image',
  timestamp: '2024-01-15T10:00:00.000Z',
};

const twPost: PostInput = {
  id: 'tw-1',
  platform: 'twitter',
  shortcode: '',
  postUrl: 'https://x.com/tweetuser/status/tw-1',
  profileUrl: 'https://x.com/tweetuser',
  authorUsername: 'tweetuser',
  authorName: 'Tweet User',
  text: 'Hello world',
  thumbnailUrl: 'https://pbs.twimg.com/media/test.jpg',
  mediaType: 'image',
  timestamp: '2024-01-10T08:00:00.000Z',
};

beforeAll(() => {
  if (loadErr) {
    console.warn(
      `[db.test] skipping DB suites: native module not loadable (${nativeMismatch ? 'ABI mismatch' : String(loadErr)})`,
    );
    return;
  }
  if (!dbInstance && db) dbInstance = db.initialize();
});

afterAll(() => {
  if (dbInstance) dbInstance.close();
  if (existsSync(dbPath)) unlinkSync(dbPath);
});

beforeEach(() => {
  if (!dbInstance || !db) return;
  dbInstance.exec(
    'DELETE FROM tag_cluster; DELETE FROM downloads; DELETE FROM post_collections; DELETE FROM post_media; DELETE FROM post_tags; DELETE FROM post_entities; DELETE FROM posts;',
  );
  // The raw exec above bypasses db.js's write paths, so drop its memoized
  // aggregates (stats, IDF weights, tag counts) explicitly.
  db.invalidateGlobalCaches();
});

// ---------------------------------------------------------------------------
// initialize()
// ---------------------------------------------------------------------------

d('initialize()', () => {
  it('returns a Database instance with a prepare method', () => {
    expect(typeof SQL().prepare).toBe('function');
  });

  it('returns the same instance on a second call (singleton)', () => {
    const second = DB().initialize();
    expect(second).toBe(dbInstance);
  });
});

// ---------------------------------------------------------------------------
// upsertPost / getPost
// ---------------------------------------------------------------------------

d('upsertPost / getPost', () => {
  it('inserts a post and retrieves it by id', () => {
    DB().upsertPost(igPost);
    const result = DB().getPost('ig-1');
    expect(result).not.toBeNull();
    expect(result?.id).toBe('ig-1');
  });

  it('maps camelCase fields correctly to/from snake_case columns', () => {
    DB().upsertPost(igPost);
    const result = DB().getPost('ig-1');
    expect(result?.platform).toBe('instagram');
    expect(result?.shortcode).toBe('ABC123');
    expect(result?.postUrl).toBe('https://www.instagram.com/p/ABC123/');
    expect(result?.profileUrl).toBe('https://www.instagram.com/testuser/');
    expect(result?.authorUsername).toBe('testuser');
    expect(result?.authorName).toBe('');
    expect(result?.text).toBe('Test caption');
    expect(result?.thumbnailUrl).toBe('https://cdn.example.com/thumb.jpg');
    expect(result?.mediaType).toBe('image');
    expect(result?.timestamp).toBe('2024-01-15T10:00:00.000Z');
  });

  it('returns null for an unknown id', () => {
    const result = DB().getPost('does-not-exist');
    expect(result).toBeNull();
  });

  it('upsertPost replaces an existing post', () => {
    DB().upsertPost(igPost);
    DB().upsertPost({ ...igPost, text: 'Updated caption' });
    const result = DB().getPost('ig-1');
    expect(result?.text).toBe('Updated caption');
  });
});

// ---------------------------------------------------------------------------
// getPosts with filters
// ---------------------------------------------------------------------------

d('getPosts()', () => {
  beforeEach(() => {
    DB().upsertPost(igPost);
    DB().upsertPost(twPost);
  });

  it('returns { posts, total } shape', () => {
    const result = DB().getPosts();
    expect(result).toHaveProperty('posts');
    expect(result).toHaveProperty('total');
    expect(Array.isArray(result.posts)).toBe(true);
  });

  it('returns all posts when no filters are given', () => {
    const { posts, total } = DB().getPosts();
    expect(posts).toHaveLength(2);
    expect(total).toBe(2);
  });

  it('filters by platform: instagram', () => {
    const { posts, total } = DB().getPosts({ platform: 'instagram' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('ig-1');
    expect(total).toBe(1);
  });

  it('filters by platform: twitter', () => {
    const { posts, total } = DB().getPosts({ platform: 'twitter' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('tw-1');
    expect(total).toBe(1);
  });

  it('filters by mediaType: video', () => {
    DB().upsertPost({
      ...igPost,
      id: 'ig-vid',
      mediaType: 'video',
      timestamp: '2024-01-20T00:00:00.000Z',
    });
    const { posts, total } = DB().getPosts({ mediaType: 'video' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('ig-vid');
    expect(total).toBe(1);
  });

  it('filters by mediaType: image', () => {
    const { posts } = DB().getPosts({ mediaType: 'image' });
    expect(posts).toHaveLength(2);
  });

  it('search filters on text (LIKE %search%)', () => {
    const { posts } = DB().getPosts({ search: 'caption' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('ig-1');
  });

  it('search filters on author_username (LIKE %search%)', () => {
    const { posts } = DB().getPosts({ search: 'tweetuser' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('tw-1');
  });

  it('search filters on shortcode (LIKE %search%)', () => {
    const { posts } = DB().getPosts({ search: 'ABC123' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('ig-1');
  });

  it('respects limit but total reflects full count', () => {
    const { posts, total } = DB().getPosts({ limit: 1 });
    expect(posts).toHaveLength(1);
    expect(total).toBe(2);
  });

  it('respects offset', () => {
    const { posts: all } = DB().getPosts();
    const { posts: withOffset } = DB().getPosts({ limit: 50, offset: 1 });
    expect(withOffset).toHaveLength(1);
    expect(withOffset[0].id).not.toBe(all[0].id);
  });

  it('orders by timestamp DESC', () => {
    const { posts } = DB().getPosts();
    expect((posts[0].timestamp ?? '') > (posts[1].timestamp ?? '')).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// bulkUpsert()
// ---------------------------------------------------------------------------

d('bulkUpsert()', () => {
  it('inserts new posts', () => {
    DB().bulkUpsert([igPost, twPost]);
    const { posts } = DB().getPosts();
    expect(posts).toHaveLength(2);
  });

  it('INSERT OR IGNORE: does not overwrite an existing post that has a download path set', () => {
    DB().upsertPost({ ...igPost, text: 'Original text' });
    DB().updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    DB().bulkUpsert([{ ...igPost, text: 'Should not overwrite' }]);
    const result = DB().getPost('ig-1');
    expect(result?.text).toBe('Original text');
    expect(result?.thumbnailPath).toBe('/local/thumb.jpg');
  });

  it('updates metadata for an existing post WITHOUT download paths', () => {
    DB().upsertPost({
      ...igPost,
      text: 'Old text',
      profileUrl: 'https://www.instagram.com/olduser/',
    });
    DB().bulkUpsert([
      { ...igPost, text: 'New text', profileUrl: 'https://www.instagram.com/newuser/' },
    ]);
    const result = DB().getPost('ig-1');
    expect(result?.text).toBe('New text');
    expect(result?.profileUrl).toBe('https://www.instagram.com/newuser/');
  });

  it('inserts all posts in a single transaction', () => {
    const posts: PostInput[] = [
      { ...igPost, id: 'bulk-1', timestamp: '2024-02-01T00:00:00.000Z' },
      { ...igPost, id: 'bulk-2', timestamp: '2024-02-02T00:00:00.000Z' },
      { ...igPost, id: 'bulk-3', timestamp: '2024-02-03T00:00:00.000Z' },
    ];
    DB().bulkUpsert(posts);
    const { posts: results } = DB().getPosts();
    expect(results).toHaveLength(3);
  });
});

// ---------------------------------------------------------------------------
// getStats()
// ---------------------------------------------------------------------------

d('getStats()', () => {
  it('returns total count of 0 when no posts exist', () => {
    const stats = DB().getStats();
    expect(stats.total).toBe(0);
  });

  it('returns correct total count', () => {
    DB().upsertPost(igPost);
    DB().upsertPost(twPost);
    const stats = DB().getStats();
    expect(stats.total).toBe(2);
  });

  it('byPlatform counts instagram and twitter correctly', () => {
    DB().upsertPost(igPost);
    DB().upsertPost({ ...igPost, id: 'ig-2', timestamp: '2024-01-16T00:00:00.000Z' });
    DB().upsertPost(twPost);
    const stats = DB().getStats();
    expect(stats.byPlatform.instagram).toBe(2);
    expect(stats.byPlatform.twitter).toBe(1);
  });

  it('byPlatform defaults to 0 for platforms with no posts', () => {
    const stats = DB().getStats();
    expect(stats.byPlatform.instagram).toBe(0);
    expect(stats.byPlatform.twitter).toBe(0);
  });

  it('byMediaType counts correctly', () => {
    DB().upsertPost(igPost);
    DB().upsertPost({
      ...igPost,
      id: 'ig-vid',
      mediaType: 'video',
      timestamp: '2024-01-20T00:00:00.000Z',
    });
    const stats = DB().getStats();
    expect(stats.byMediaType.image).toBe(1);
    expect(stats.byMediaType.video).toBe(1);
  });

  it('downloaded count includes only posts with at least one path set', () => {
    DB().upsertPost(igPost);
    DB().upsertPost(twPost);
    DB().updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    const stats = DB().getStats();
    expect(stats.downloaded).toBe(1);
  });

  it('downloaded count is 0 when no paths are set', () => {
    DB().upsertPost(igPost);
    DB().upsertPost(twPost);
    const stats = DB().getStats();
    expect(stats.downloaded).toBe(0);
  });

  it('downloadedByType counts each path column independently', () => {
    DB().upsertPost(igPost);
    DB().upsertPost(twPost);
    DB().upsertPost({ ...igPost, id: 'ig-3', timestamp: '2024-01-17T00:00:00.000Z' });
    DB().updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg', imagePath: '/local/img.jpg' });
    DB().updatePaths('tw-1', { videoPath: '/local/vid.mp4' });
    const stats = DB().getStats();
    expect(stats.downloaded).toBe(2);
    expect(stats.downloadedByType).toEqual({ thumbnails: 1, images: 1, videos: 1 });
  });

  it('memoizes the result until a write invalidates it', () => {
    DB().upsertPost(igPost);
    expect(DB().getStats().total).toBe(1);
    // Raw delete bypasses db.js's write paths: the memoized stats survive.
    SQL().exec('DELETE FROM posts;');
    expect(DB().getStats().total).toBe(1);
    DB().invalidateGlobalCaches();
    expect(DB().getStats().total).toBe(0);
  });

  it('updatePaths invalidates the memoized stats', () => {
    DB().upsertPost(igPost);
    expect(DB().getStats().downloaded).toBe(0);
    DB().updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    expect(DB().getStats().downloaded).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// termIdfWeights() — IDF cache
// ---------------------------------------------------------------------------

d('termIdfWeights() cache', () => {
  const cols = ['text'];

  it('weights rarer terms higher than ubiquitous ones', () => {
    for (let i = 0; i < 10; i++) {
      DB().upsertPost({
        ...igPost,
        id: `idf-${i}`,
        text: i === 0 ? 'common rareword' : 'common filler',
      });
    }
    const w = DB().__termIdfWeights(['common', 'rareword'], cols);
    expect(w.common).toBe(1);
    expect(w.rareword).toBeGreaterThan(w.common);
  });

  it('memoizes per-term weights until invalidateGlobalCaches', () => {
    for (let i = 0; i < 10; i++) {
      DB().upsertPost({
        ...igPost,
        id: `idf-${i}`,
        text: i === 0 ? 'common rareword' : 'common filler',
      });
    }
    const before = DB().__termIdfWeights(['rareword'], cols).rareword;
    expect(before).toBeGreaterThan(1);
    // Raw update bypasses db.js's write paths: the cached weight survives.
    SQL().exec("UPDATE posts SET text = 'common rareword';");
    expect(DB().__termIdfWeights(['rareword'], cols).rareword).toBe(before);
    DB().invalidateGlobalCaches();
    expect(DB().__termIdfWeights(['rareword'], cols).rareword).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// updatePaths()
// ---------------------------------------------------------------------------

d('updatePaths()', () => {
  beforeEach(() => {
    DB().upsertPost(igPost);
  });

  it('sets thumbnailPath for a post', () => {
    DB().updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    const result = DB().getPost('ig-1');
    expect(result?.thumbnailPath).toBe('/local/thumb.jpg');
  });

  it('sets imagePath for a post', () => {
    DB().updatePaths('ig-1', { imagePath: '/local/image.jpg' });
    const result = DB().getPost('ig-1');
    expect(result?.imagePath).toBe('/local/image.jpg');
  });

  it('sets videoPath for a post', () => {
    DB().updatePaths('ig-1', { videoPath: '/local/video.mp4' });
    const result = DB().getPost('ig-1');
    expect(result?.videoPath).toBe('/local/video.mp4');
  });

  it('COALESCE: passing null does not overwrite an existing thumbnailPath', () => {
    DB().updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    DB().updatePaths('ig-1', { thumbnailPath: null });
    const result = DB().getPost('ig-1');
    expect(result?.thumbnailPath).toBe('/local/thumb.jpg');
  });

  it('COALESCE: can update one path without clearing others', () => {
    DB().updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg', imagePath: '/local/image.jpg' });
    DB().updatePaths('ig-1', { videoPath: '/local/video.mp4' });
    const result = DB().getPost('ig-1');
    expect(result?.thumbnailPath).toBe('/local/thumb.jpg');
    expect(result?.imagePath).toBe('/local/image.jpg');
    expect(result?.videoPath).toBe('/local/video.mp4');
  });

  it('persists thumbBlur alongside the paths, COALESCE-protected like them', () => {
    DB().updatePaths('ig-1', {
      thumbnailPath: '/local/thumb.jpg',
      thumbBlur: 'data:image/jpeg;base64,AAAA',
    });
    expect(DB().getPost('ig-1')?.thumbBlur).toBe('data:image/jpeg;base64,AAAA');
    // A later path-only update must not clear the stored placeholder.
    DB().updatePaths('ig-1', { videoPath: '/local/video.mp4' });
    expect(DB().getPost('ig-1')?.thumbBlur).toBe('data:image/jpeg;base64,AAAA');
  });
});

// ---------------------------------------------------------------------------
// Blur-up placeholders (thumb_blur)
// ---------------------------------------------------------------------------

d('thumb_blur helpers', () => {
  beforeEach(() => {
    DB().upsertPost(igPost);
    DB().upsertPost(twPost);
  });

  it('listPostsMissingThumbBlur returns posts with a local cover and no placeholder', () => {
    DB().updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    const rows = DB().listPostsMissingThumbBlur();
    expect(rows).toEqual([{ id: 'ig-1', src: '/local/thumb.jpg' }]);
  });

  it('listPostsMissingThumbBlur prefers thumbnail_path over image_path as src', () => {
    DB().updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg', imagePath: '/local/image.jpg' });
    expect(DB().listPostsMissingThumbBlur()).toEqual([{ id: 'ig-1', src: '/local/thumb.jpg' }]);
  });

  it("setThumbBlur removes the post from the missing list, '' sentinel included", () => {
    DB().updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    DB().updatePaths('tw-1', { imagePath: '/local/image.jpg' });
    DB().setThumbBlur('ig-1', 'data:image/jpeg;base64,AAAA');
    DB().setThumbBlur('tw-1', ''); // tried, ineligible — must not be rescanned
    expect(DB().listPostsMissingThumbBlur()).toEqual([]);
  });

  it("rowToPost maps the '' sentinel back to null for the renderer", () => {
    DB().updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    DB().setThumbBlur('ig-1', '');
    expect(DB().getPost('ig-1')?.thumbBlur).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// importFromJSON()
// ---------------------------------------------------------------------------

d('importFromJSON()', () => {
  const tempJsonPath = join(tmpdir(), 'test-import.json');

  it('reads a JSON array, inserts posts, and returns { imported: N }', async () => {
    writeFileSync(tempJsonPath, JSON.stringify([igPost, twPost]), 'utf8');
    const result = await DB().importFromJSON(tempJsonPath);
    expect(result).toMatchObject({ imported: 2 });
    expect(DB().getPost('ig-1')).not.toBeNull();
    expect(DB().getPost('tw-1')).not.toBeNull();
  });

  it('reads a JSON object with a posts array', async () => {
    writeFileSync(tempJsonPath, JSON.stringify({ posts: [igPost] }), 'utf8');
    const result = await DB().importFromJSON(tempJsonPath);
    expect(result).toMatchObject({ imported: 1 });
  });

  it('returns { imported: 0 } on a second import of the same file (INSERT OR IGNORE)', async () => {
    writeFileSync(tempJsonPath, JSON.stringify([igPost, twPost]), 'utf8');
    await DB().importFromJSON(tempJsonPath);
    const second = await DB().importFromJSON(tempJsonPath);
    expect(second).toMatchObject({ imported: 0 });
  });
});

// ---------------------------------------------------------------------------
// AI analysis helpers
// ---------------------------------------------------------------------------

// Insert a base post then run updateAiAnalysis with the given AI fields.
function seedAnalyzed(id: string, overrides: SeedOverrides = {}, ai: SeedAi = {}): void {
  DB().upsertPost({
    ...igPost,
    id,
    shortcode: id,
    text: overrides.text ?? `caption ${id}`,
    mediaType: overrides.mediaType ?? 'image',
    timestamp: overrides.timestamp ?? '2024-03-01T00:00:00.000Z',
    ...overrides,
  });
  DB().updateAiAnalysis(id, { status: 'done', model: 'test-model', ...ai });
}

// ---------------------------------------------------------------------------
// updateAiAnalysis() — new fields round-trip via rowToPost
// ---------------------------------------------------------------------------

d('updateAiAnalysis()', () => {
  it('persists all new AI fields and rowToPost reads them back', () => {
    DB().upsertPost(igPost);
    DB().updateAiAnalysis('ig-1', {
      description: 'una ricetta di pasta',
      tags: ['pasta', 'cucina'],
      status: 'done',
      model: 'test-model',
      category: 'Cibo & Ricette',
      contentType: 'ricetta',
      entities: ['Carbonara', 'Roma'],
      keywords: ['come fare la carbonara', 'pasta romana'],
      language: 'it',
      saveReason: 'da provare nel weekend',
    });

    const p = DB().getPost('ig-1');
    expect(p?.aiDescription).toBe('una ricetta di pasta');
    expect(p?.aiTags).toEqual(['pasta', 'cucina']);
    expect(p?.aiStatus).toBe('done');
    expect(p?.aiModel).toBe('test-model');
    expect(p?.aiCategory).toBe('Cibo & Ricette');
    expect(p?.aiContentType).toBe('ricetta');
    expect(p?.aiEntities).toEqual(['Carbonara', 'Roma']);
    expect(p?.aiKeywords).toEqual(['come fare la carbonara', 'pasta romana']);
    expect(p?.aiLanguage).toBe('it');
    expect(p?.aiSaveReason).toBe('da provare nel weekend');
    expect(typeof p?.aiAnalyzedAt).toBe('number');
  });

  it('defaults array fields to [] when never set', () => {
    DB().upsertPost(igPost);
    const p = DB().getPost('ig-1');
    expect(p?.aiEntities).toEqual([]);
    expect(p?.aiKeywords).toEqual([]);
    expect(p?.aiTags).toEqual([]);
    expect(p?.aiCategory).toBeNull();
  });

  it('COALESCE: a status-only update does not clear existing fields', () => {
    DB().upsertPost(igPost);
    DB().updateAiAnalysis('ig-1', { tags: ['x'], category: 'Altro', status: 'done' });
    DB().updateAiAnalysis('ig-1', { status: 'analyzing' });
    const p = DB().getPost('ig-1');
    expect(p?.aiStatus).toBe('analyzing');
    expect(p?.aiTags).toEqual(['x']);
    expect(p?.aiCategory).toBe('Altro');
  });

  it('explicit { status: null } clears ai_status (cancelled job) without touching other fields', () => {
    DB().upsertPost(igPost);
    DB().updateAiAnalysis('ig-1', { tags: ['x'], category: 'Altro', status: 'analyzing' });
    DB().updateAiAnalysis('ig-1', { status: null });
    const p = DB().getPost('ig-1');
    expect(p?.aiStatus).toBeNull();
    expect(p?.aiTags).toEqual(['x']);
    expect(p?.aiCategory).toBe('Altro');
  });

  it('an empty update is a no-op and leaves existing fields intact', () => {
    DB().upsertPost(igPost);
    DB().updateAiAnalysis('ig-1', { tags: ['x'], status: 'done' });
    DB().updateAiAnalysis('ig-1', {});
    const p = DB().getPost('ig-1');
    expect(p?.aiStatus).toBe('done');
    expect(p?.aiTags).toEqual(['x']);
  });
});

// ---------------------------------------------------------------------------
// getFrequentTags()
// ---------------------------------------------------------------------------

d('getFrequentTags()', () => {
  it('returns tags ordered by frequency (case-insensitive count)', () => {
    seedAnalyzed('p1', {}, { tags: ['cucina', 'pasta'] });
    seedAnalyzed('p2', {}, { tags: ['Cucina', 'viaggio'] });
    seedAnalyzed('p3', {}, { tags: ['cucina'] });
    const tags = DB().getFrequentTags(10);
    expect(tags[0].toLowerCase()).toBe('cucina');
    expect(tags).toContain('pasta');
    expect(tags).toContain('viaggio');
  });

  it('respects the limit', () => {
    seedAnalyzed('p1', {}, { tags: ['a', 'b', 'c', 'd'] });
    expect(DB().getFrequentTags(2)).toHaveLength(2);
  });

  it('returns the most common casing as display form', () => {
    seedAnalyzed('p1', {}, { tags: ['Cucina'] });
    seedAnalyzed('p2', {}, { tags: ['cucina'] });
    seedAnalyzed('p3', {}, { tags: ['cucina'] });
    expect(DB().getFrequentTags(5)[0]).toBe('cucina');
  });
});

// ---------------------------------------------------------------------------
// getPosts() — AI filters
// ---------------------------------------------------------------------------

d('getPosts() AI filters', () => {
  beforeEach(() => {
    seedAnalyzed(
      'food',
      { timestamp: '2024-03-05T00:00:00.000Z' },
      {
        tags: ['pasta', 'cucina'],
        category: 'Cibo & Ricette',
        contentType: 'ricetta',
        entities: ['Roma'],
      },
    );
    seedAnalyzed(
      'tech',
      { timestamp: '2024-03-04T00:00:00.000Z' },
      {
        tags: ['gadget', 'cucina'],
        category: 'Tech & Strumenti',
        contentType: 'prodotto',
        entities: ['Apple'],
      },
    );
    // Unanalyzed post (no updateAiAnalysis).
    DB().upsertPost({
      ...igPost,
      id: 'raw',
      shortcode: 'raw',
      timestamp: '2024-03-03T00:00:00.000Z',
    });
  });

  it('filters by single tag', () => {
    const { posts } = DB().getPosts({ tag: 'pasta' });
    expect(posts.map((p) => p.id)).toEqual(['food']);
  });

  it('filters by tags with tagMode "or"', () => {
    const { posts } = DB().getPosts({ tags: ['pasta', 'gadget'], tagMode: 'or' });
    expect(posts.map((p) => p.id).sort()).toEqual(['food', 'tech']);
  });

  it('filters by tags with tagMode "and"', () => {
    const { posts } = DB().getPosts({ tags: ['pasta', 'cucina'], tagMode: 'and' });
    expect(posts.map((p) => p.id)).toEqual(['food']);
    const none = DB().getPosts({ tags: ['pasta', 'gadget'], tagMode: 'and' });
    expect(none.posts).toHaveLength(0);
  });

  it('filters by category', () => {
    const { posts } = DB().getPosts({ category: 'Tech & Strumenti' });
    expect(posts.map((p) => p.id)).toEqual(['tech']);
  });

  it('filters by contentType', () => {
    const { posts } = DB().getPosts({ contentType: 'ricetta' });
    expect(posts.map((p) => p.id)).toEqual(['food']);
  });

  it('filters by entity', () => {
    const { posts } = DB().getPosts({ entity: 'Apple' });
    expect(posts.map((p) => p.id)).toEqual(['tech']);
  });

  it('filters by analyzedStatus "analyzed"', () => {
    const { posts } = DB().getPosts({ analyzedStatus: 'analyzed' });
    expect(posts.map((p) => p.id).sort()).toEqual(['food', 'tech']);
  });

  it('filters by analyzedStatus "unanalyzed"', () => {
    const { posts } = DB().getPosts({ analyzedStatus: 'unanalyzed' });
    expect(posts.map((p) => p.id)).toEqual(['raw']);
  });

  it('filters by aiTagged "tagged"', () => {
    const { posts } = DB().getPosts({ aiTagged: 'tagged' });
    expect(posts.map((p) => p.id).sort()).toEqual(['food', 'tech']);
  });

  it('filters by aiTagged "untagged"', () => {
    const { posts } = DB().getPosts({ aiTagged: 'untagged' });
    expect(posts.map((p) => p.id)).toEqual(['raw']);
  });

  it('escapes LIKE wildcards: a tag query with % / _ does not match literal tags', () => {
    seedAnalyzed('wild', { timestamp: '2024-03-06T00:00:00.000Z' }, { tags: ['100%off', 'a_b'] });
    // '%' must not act as a wildcard: 'p%a' should NOT match 'pasta'.
    expect(DB().getPosts({ tag: 'p%a' }).posts).toHaveLength(0);
    // '_' must not act as a single-char wildcard: 'a_b' must only match the literal.
    expect(
      DB()
        .getPosts({ tag: 'a_b' })
        .posts.map((p) => p.id),
    ).toEqual(['wild']);
    // Literal '%' still matches when present verbatim.
    expect(
      DB()
        .getPosts({ tag: '100%off' })
        .posts.map((p) => p.id),
    ).toEqual(['wild']);
  });

  it('escapes LIKE wildcards in search and entity filters', () => {
    seedAnalyzed(
      'pct',
      { text: 'discount 50% sale', timestamp: '2024-03-07T00:00:00.000Z' },
      {
        tags: ['x'],
        entities: ['C_3PO'],
      },
    );
    // search: 'p%a' should not match anything via wildcard expansion.
    expect(DB().getPosts({ search: 'p%a' }).posts).toHaveLength(0);
    // literal '%' in text is found.
    expect(
      DB()
        .getPosts({ search: '50%' })
        .posts.map((p) => p.id),
    ).toEqual(['pct']);
    // entity '_' is literal, not a single-char wildcard.
    expect(
      DB()
        .getPosts({ entity: 'C_3PO' })
        .posts.map((p) => p.id),
    ).toEqual(['pct']);
    expect(DB().getPosts({ entity: 'CX3PO' }).posts).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// getAiOverview()
// ---------------------------------------------------------------------------

d('getAiOverview()', () => {
  it('reports totals, category/contentType/language breakdowns and tag counts', () => {
    seedAnalyzed(
      'a',
      {},
      {
        tags: ['pasta', 'cucina'],
        category: 'Cibo & Ricette',
        contentType: 'ricetta',
        language: 'it',
      },
    );
    seedAnalyzed(
      'b',
      {},
      {
        tags: ['pasta'],
        category: 'Cibo & Ricette',
        contentType: 'prodotto',
        language: 'en',
      },
    );
    DB().upsertPost({ ...igPost, id: 'c', shortcode: 'c' });

    const ov = DB().getAiOverview();
    expect(ov.total).toBe(3);
    expect(ov.analyzed).toBe(2);
    expect(ov.unanalyzed).toBe(1);
    expect(ov.taggedPosts).toBe(2);
    expect(ov.uniqueTags).toBe(2); // pasta, cucina
    expect(ov.byCategory[0]).toEqual({ category: 'Cibo & Ricette', count: 2 });
    const langs = Object.fromEntries(ov.languages.map((l) => [l.language, l.count]));
    expect(langs).toEqual({ it: 1, en: 1 });
  });
});

// ---------------------------------------------------------------------------
// getTagStats()
// ---------------------------------------------------------------------------

d('getTagStats()', () => {
  it('returns count, lastUsed and category distribution per tag', () => {
    seedAnalyzed(
      'a',
      { timestamp: '2024-03-01T00:00:00.000Z' },
      {
        tags: ['cucina'],
        category: 'Cibo & Ricette',
      },
    );
    seedAnalyzed(
      'b',
      { timestamp: '2024-03-10T00:00:00.000Z' },
      {
        tags: ['cucina'],
        category: 'Tech & Strumenti',
      },
    );

    const stats = DB().getTagStats();
    const cucina = stats.find((s) => s.tag.toLowerCase() === 'cucina');
    expect(cucina?.count).toBe(2);
    expect(cucina?.lastUsed).toBe('2024-03-10T00:00:00.000Z');
    const catMap = Object.fromEntries((cucina?.categories ?? []).map((c) => [c.category, c.count]));
    expect(catMap).toEqual({ 'Cibo & Ricette': 1, 'Tech & Strumenti': 1 });
  });

  it('counts a tag once per post even if duplicated within it', () => {
    seedAnalyzed('a', {}, { tags: ['cucina', 'Cucina'] });
    const cucina = DB()
      .getTagStats()
      .find((s) => s.tag.toLowerCase() === 'cucina');
    expect(cucina?.count).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// getEntityStats()
// ---------------------------------------------------------------------------

d('getEntityStats()', () => {
  it('counts entities case-insensitively, preserving casing', () => {
    seedAnalyzed('a', {}, { tags: ['x'], entities: ['Roma', 'Apple'] });
    seedAnalyzed('b', {}, { tags: ['x'], entities: ['roma'] });
    const stats = DB().getEntityStats();
    const roma = stats.find((s) => s.entity.toLowerCase() === 'roma');
    expect(roma?.count).toBe(2);
    expect(roma?.entity).toBe('Roma'); // most common casing
    expect(stats.find((s) => s.entity === 'Apple')?.count).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// getTagCooccurrence()
// ---------------------------------------------------------------------------

d('getTagCooccurrence()', () => {
  it('returns tags that co-occur with the target', () => {
    seedAnalyzed('a', {}, { tags: ['cucina', 'pasta', 'roma'] });
    seedAnalyzed('b', {}, { tags: ['cucina', 'pasta'] });
    seedAnalyzed('c', {}, { tags: ['viaggio'] });

    const co = DB().getTagCooccurrence('cucina');
    const map = Object.fromEntries(co.map((c) => [c.tag.toLowerCase(), c.count]));
    expect(map.pasta).toBe(2);
    expect(map.roma).toBe(1);
    expect(map.cucina).toBeUndefined(); // never lists the target itself
    expect(map.viaggio).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// searchTagsByText()
// ---------------------------------------------------------------------------

d('searchTagsByText()', () => {
  it('matches long-tail tags by query token, most-used first', () => {
    seedAnalyzed('a', {}, { tags: ['cuffie', 'audio'] });
    seedAnalyzed('b', {}, { tags: ['cuffie'] });
    seedAnalyzed('c', {}, { tags: ['raymarching'] });

    const res = DB().searchTagsByText('devo fare un moodboard di cuffie');
    const map = Object.fromEntries(res.map((r) => [r.tag.toLowerCase(), r.count]));
    expect(map.cuffie).toBe(2);
    expect(map.raymarching).toBeUndefined(); // unrelated tag not pulled in
    expect(res[0].tag.toLowerCase()).toBe('cuffie'); // most-used first
  });

  it('matches on substrings of the tag, not just whole words', () => {
    seedAnalyzed('a', {}, { tags: ['headphones'] });
    const res = DB().searchTagsByText('phone');
    expect(res.map((r) => r.tag.toLowerCase())).toContain('headphones');
  });

  it('ignores tokens shorter than minTermLen and empty queries', () => {
    seedAnalyzed('a', {}, { tags: ['cuffie'] });
    expect(DB().searchTagsByText('di un', { minTermLen: 3 })).toEqual([]);
    expect(DB().searchTagsByText('')).toEqual([]);
  });

  it('respects the limit', () => {
    seedAnalyzed('a', {}, { tags: ['audio-uno', 'audio-due', 'audio-tre'] });
    expect(DB().searchTagsByText('audio', { limit: 2 }).length).toBe(2);
  });
});

// ---------------------------------------------------------------------------
// getTagClusters()
// ---------------------------------------------------------------------------

d('getTagCandidateGroups()', () => {
  it('groups co-occurring tags into a dense candidate with neighbor context', async () => {
    // 'cucina' + 'pasta' co-occur in 3 posts → a strong Jaccard edge.
    seedAnalyzed('a', {}, { tags: ['cucina', 'pasta'] });
    seedAnalyzed('b', {}, { tags: ['cucina', 'pasta'] });
    seedAnalyzed('c', {}, { tags: ['cucina', 'pasta'] });

    const groups = await DB().getTagCandidateGroups({ minTagCount: 2, minJaccard: 0.1 });
    expect(groups.length).toBeGreaterThanOrEqual(1);
    const g = groups.find((x) => x.tags.includes('cucina') && x.tags.includes('pasta'));
    expect(g).toBeTruthy();
    expect(g?.neighbors).toBeTruthy();
    // candidate tags are normalized keys
    expect(g?.tags).toEqual(expect.arrayContaining(['cucina', 'pasta']));
  });

  it('does NOT fuse two unrelated dense groups bridged only by a hub tag', async () => {
    // Two tight themes, each co-occurring strongly within itself...
    for (const id of ['s1', 's2', 's3']) seedAnalyzed(id, {}, { tags: ['glsl', 'shader', 'hub'] });
    for (const id of ['t1', 't2', 't3']) seedAnalyzed(id, {}, { tags: ['blender', '3d', 'hub'] });
    // ...'hub' co-occurs with everything but, being frequent, gets a LOW Jaccard
    // weight and must not merge shaders with 3d into one blob.
    const groups = await DB().getTagCandidateGroups({ minTagCount: 2, minJaccard: 0.3 });
    const withShader = groups.find((g) => g.tags.includes('glsl'));
    const withBlender = groups.find((g) => g.tags.includes('blender'));
    expect(withShader).toBeTruthy();
    expect(withBlender).toBeTruthy();
    expect(withShader).not.toBe(withBlender);
    expect(withShader?.tags).not.toContain('blender');
  });
});

// ---------------------------------------------------------------------------
// buildTagCommunities() — pure, no DB
// ---------------------------------------------------------------------------

pure('buildTagCommunities() [pure]', () => {
  it('is deterministic and splits hub-bridged themes via Jaccard weighting', () => {
    const freq = new Map([
      ['glsl', 3],
      ['shader', 3],
      ['blender', 3],
      ['3d', 3],
      ['hub', 6],
    ]);
    const edges = [
      { a: 'glsl', b: 'shader', c: 3 },
      { a: 'blender', b: '3d', c: 3 },
      { a: 'glsl', b: 'hub', c: 3 },
      { a: 'shader', b: 'hub', c: 3 },
      { a: 'blender', b: 'hub', c: 3 },
      { a: '3d', b: 'hub', c: 3 },
    ];
    const a = DB().buildTagCommunities(freq, edges, { minJaccard: 0.3 });
    const b = DB().buildTagCommunities(freq, edges, { minJaccard: 0.3 });
    expect(a).toEqual(b); // deterministic
    const shaderGroup = a.find((g) => g.includes('glsl'));
    expect(shaderGroup).toEqual(expect.arrayContaining(['glsl', 'shader']));
    expect(shaderGroup).not.toContain('blender');
  });
});

// ---------------------------------------------------------------------------
// saveClusterRun() / getTagClusters() / status / rename / removeTag
// ---------------------------------------------------------------------------

d('cluster persistence', () => {
  it('round-trips a saved run and reads it back with label/postCount', () => {
    seedAnalyzed('a', {}, { tags: ['glsl', 'shader'] });
    seedAnalyzed('b', {}, { tags: ['glsl', 'shader'] });

    const { count } = DB().saveClusterRun([{ label: 'Shader & GPU', tags: ['glsl', 'shader'] }]);
    expect(count).toBe(1);

    const clusters = DB().getTagClusters();
    expect(clusters.length).toBe(1);
    expect(clusters[0].topTag).toBe('Shader & GPU');
    expect(clusters[0].status).toBe('proposed');
    expect(clusters[0].postCount).toBe(2);
    expect(clusters[0].tags.map((t) => t.toLowerCase())).toEqual(
      expect.arrayContaining(['glsl', 'shader']),
    );
  });

  it('a new run clears prior proposed but keeps accepted clusters and their tags', () => {
    seedAnalyzed('a', {}, { tags: ['glsl', 'shader', 'blender', '3d'] });
    seedAnalyzed('b', {}, { tags: ['glsl', 'shader', 'blender', '3d'] });

    DB().saveClusterRun([{ label: 'Shader', tags: ['glsl', 'shader'] }]);
    const accepted = DB().getTagClusters()[0];
    DB().setClusterStatus(accepted.id, 'accepted');

    // Re-run proposes a different grouping; accepted 'glsl/shader' stays put.
    DB().saveClusterRun([
      { label: 'Shader', tags: ['glsl', 'shader'] }, // these are owned → skipped
      { label: '3D', tags: ['blender', '3d'] },
    ]);

    const clusters = DB().getTagClusters();
    const acceptedNow = clusters.find((c) => c.status === 'accepted');
    expect(acceptedNow?.topTag).toBe('Shader');
    expect((acceptedNow?.tags ?? []).map((t) => t.toLowerCase())).toEqual(
      expect.arrayContaining(['glsl', 'shader']),
    );
    const proposed = clusters.find((c) => c.status === 'proposed');
    expect((proposed?.tags ?? []).map((t) => t.toLowerCase())).toEqual(
      expect.arrayContaining(['blender', '3d']),
    );
    expect((proposed?.tags ?? []).map((t) => t.toLowerCase())).not.toContain('glsl');
  });

  it('dismiss deletes the cluster; rename and removeTag mutate it', () => {
    seedAnalyzed('a', {}, { tags: ['glsl', 'shader', 'webgl'] });
    seedAnalyzed('b', {}, { tags: ['glsl', 'shader', 'webgl'] });
    DB().saveClusterRun([{ label: 'Shader', tags: ['glsl', 'shader', 'webgl'] }]);
    const c = DB().getTagClusters()[0];

    DB().renameCluster(c.id, 'Shader & GPU');
    expect(DB().getTagClusters()[0].topTag).toBe('Shader & GPU');

    DB().removeTagFromCluster('webgl', c.id);
    expect(
      DB()
        .getTagClusters()[0]
        .tags.map((t) => t.toLowerCase()),
    ).not.toContain('webgl');

    DB().setClusterStatus(c.id, 'dismissed');
    expect(DB().getTagClusters()).toHaveLength(0);
  });

  it('mergeTags remaps cluster membership: target inherits, sources dropped', () => {
    seedAnalyzed('a', {}, { tags: ['glslang', 'shader'] });
    seedAnalyzed('b', {}, { tags: ['glslang', 'shader'] });
    DB().saveClusterRun([{ label: 'Shader', tags: ['glslang', 'shader'] }]);

    // Merge 'glslang' → 'glsl'. The cluster must now contain 'glsl', not 'glslang'.
    DB().mergeTags(['glslang'], 'glsl');
    const tags = DB()
      .getTagClusters()[0]
      .tags.map((t) => t.toLowerCase());
    expect(tags).toContain('glsl');
    expect(tags).not.toContain('glslang');
  });
});

// ---------------------------------------------------------------------------
// getTagMergeSuggestions()
// ---------------------------------------------------------------------------

d('getTagMergeSuggestions()', () => {
  it('groups near-duplicate tags (case/accents/typos)', () => {
    seedAnalyzed('a', {}, { tags: ['ai'] });
    seedAnalyzed('b', {}, { tags: ['AI'] });
    seedAnalyzed('c', {}, { tags: ['a.i.'] });
    seedAnalyzed('d', {}, { tags: ['citta'] });
    seedAnalyzed('e', {}, { tags: ['città'] });

    const suggestions = DB().getTagMergeSuggestions();

    // 'ai' / 'AI' normalize to the same key and are within edit distance of 'a.i.'.
    const aiGroup = suggestions.find((g) =>
      [g.canonical, ...g.variants].some((t) => t.toLowerCase() === 'ai'),
    );
    expect(aiGroup).toBeTruthy();
    const aiForms = [aiGroup?.canonical, ...(aiGroup?.variants ?? [])].map((t) =>
      String(t).toLowerCase(),
    );
    expect(aiForms).toContain('a.i.');

    // 'citta' / 'città' differ only by diacritics → same normalized key.
    const cittaGroup = suggestions.find((g) =>
      [g.canonical, ...g.variants].some((t) => t.toLowerCase() === 'città'),
    );
    expect(cittaGroup).toBeTruthy();
    expect(
      [cittaGroup?.canonical, ...(cittaGroup?.variants ?? [])].map((t) => String(t).toLowerCase()),
    ).toEqual(expect.arrayContaining(['citta', 'città']));
  });
});

// ---------------------------------------------------------------------------
// getTagHealth()
// ---------------------------------------------------------------------------

d('getTagHealth()', () => {
  it('reports orphan tags, untagged and unanalyzed posts', () => {
    // 'cucina' appears twice (healthy); 'solo' appears once (orphan).
    seedAnalyzed('a', {}, { tags: ['cucina', 'solo'] });
    seedAnalyzed('b', {}, { tags: ['cucina'] });
    // Analyzed but with no tags → untagged.
    seedAnalyzed('c', {}, { tags: [] });
    // Never analyzed → unanalyzed.
    DB().upsertPost({ ...igPost, id: 'd', shortcode: 'd' });

    const health = DB().getTagHealth();
    const orphanTags = health.orphanTags.map((o) => o.tag.toLowerCase());
    expect(orphanTags).toContain('solo');
    expect(orphanTags).not.toContain('cucina');
    expect(health.untaggedPosts).toBe(1);
    expect(health.unanalyzedPosts).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// renameTag() / mergeTags()
// ---------------------------------------------------------------------------

d('renameTag() / mergeTags()', () => {
  it('renameTag rewrites the tag everywhere (case-insensitive)', () => {
    seedAnalyzed('a', {}, { tags: ['Cucina', 'pasta'] });
    seedAnalyzed('b', {}, { tags: ['cucina'] });
    const res = DB().renameTag('cucina', 'cibo');
    expect(res.updated).toBe(2);
    expect(DB().getPost('a')?.aiTags).toContain('cibo');
    expect(DB().getPost('a')?.aiTags).not.toContain('Cucina');
    expect(DB().getPost('b')?.aiTags).toEqual(['cibo']);
  });

  it('mergeTags collapses several sources into one target, de-duplicating', () => {
    seedAnalyzed('a', {}, { tags: ['pasta', 'spaghetti'] });
    const res = DB().mergeTags(['pasta', 'spaghetti'], 'primo');
    expect(res.updated).toBe(1);
    expect(DB().getPost('a')?.aiTags).toEqual(['primo']);
  });

  it('mergeTags throws without a target', () => {
    expect(() => DB().mergeTags(['x'], '')).toThrow();
  });
});

// ---------------------------------------------------------------------------
// getPostIds() — same filters as getPosts
// ---------------------------------------------------------------------------

d('getPostIds()', () => {
  beforeEach(() => {
    seedAnalyzed(
      'food',
      { timestamp: '2024-03-05T00:00:00.000Z' },
      {
        tags: ['pasta'],
        category: 'Cibo & Ricette',
      },
    );
    seedAnalyzed(
      'tech',
      { timestamp: '2024-03-04T00:00:00.000Z' },
      {
        tags: ['gadget'],
        category: 'Tech & Strumenti',
      },
    );
  });

  it('returns all matching ids ordered by timestamp DESC (no pagination)', () => {
    expect(DB().getPostIds()).toEqual(['food', 'tech']);
  });

  it('honors the same filters as getPosts', () => {
    expect(DB().getPostIds({ category: 'Cibo & Ricette' })).toEqual(['food']);
    expect(DB().getPostIds({ tag: 'gadget' })).toEqual(['tech']);
  });
});

// ---------------------------------------------------------------------------
// getPostsByIds() — order + batching
// ---------------------------------------------------------------------------

d('getPostsByIds()', () => {
  it('returns posts following the input id order', () => {
    seedAnalyzed('x', {}, { tags: ['a'] });
    seedAnalyzed('y', {}, { tags: ['b'] });
    seedAnalyzed('z', {}, { tags: ['c'] });
    const posts = DB().getPostsByIds(['z', 'x', 'y']);
    expect(posts.map((p) => p.id)).toEqual(['z', 'x', 'y']);
  });

  it('skips unknown ids and returns [] for empty input', () => {
    seedAnalyzed('x', {}, { tags: ['a'] });
    expect(
      DB()
        .getPostsByIds(['x', 'missing'])
        .map((p) => p.id),
    ).toEqual(['x']);
    expect(DB().getPostsByIds([])).toEqual([]);
  });

  it('handles more ids than one batch (> 500) preserving order', () => {
    const ids: string[] = [];
    for (let i = 0; i < 600; i++) {
      const id = `b${String(i).padStart(4, '0')}`;
      DB().upsertPost({ ...igPost, id, shortcode: id });
      ids.push(id);
    }
    const shuffled = [ids[599], ids[0], ids[250], ids[500]];
    const posts = DB().getPostsByIds(shuffled);
    expect(posts.map((p) => p.id)).toEqual(shuffled);
  });
});

// ---------------------------------------------------------------------------
// Derived index tables (post_tags / post_entities)
// ---------------------------------------------------------------------------

// Convenience reads of the derived rows for a post, lowercased norms sorted.
function tagNorms(id: string): string[] {
  return (
    SQL()
      .prepare('SELECT tag_norm FROM post_tags WHERE post_id = ? ORDER BY tag_norm')
      .all(id) as Array<{ tag_norm: string }>
  ).map((r) => r.tag_norm);
}
function entNorms(id: string): string[] {
  return (
    SQL()
      .prepare('SELECT ent_norm FROM post_entities WHERE post_id = ? ORDER BY ent_norm')
      .all(id) as Array<{ ent_norm: string }>
  ).map((r) => r.ent_norm);
}

d('derived index sync (updateAiAnalysis)', () => {
  it('populates post_tags / post_entities when tags+entities are provided', () => {
    DB().upsertPost(igPost);
    DB().updateAiAnalysis('ig-1', {
      tags: ['Pasta', 'Cucina'],
      entities: ['Roma'],
      status: 'done',
    });
    expect(tagNorms('ig-1')).toEqual(['cucina', 'pasta']);
    expect(entNorms('ig-1')).toEqual(['roma']);
    // tag_form preserves the original casing.
    const forms = (
      SQL()
        .prepare('SELECT tag_form FROM post_tags WHERE post_id = ? ORDER BY tag_form')
        .all('ig-1') as Array<{ tag_form: string }>
    ).map((r) => r.tag_form);
    expect(forms).toEqual(['Cucina', 'Pasta']);
  });

  it('resyncs post_tags on a subsequent tags update (replace, not append)', () => {
    DB().upsertPost(igPost);
    DB().updateAiAnalysis('ig-1', { tags: ['a', 'b'] });
    expect(tagNorms('ig-1')).toEqual(['a', 'b']);
    DB().updateAiAnalysis('ig-1', { tags: ['b', 'c'] });
    expect(tagNorms('ig-1')).toEqual(['b', 'c']);
  });

  it('clears post_tags when tags is [] or null', () => {
    DB().upsertPost(igPost);
    DB().updateAiAnalysis('ig-1', { tags: ['x', 'y'], entities: ['Z'] });
    expect(tagNorms('ig-1')).toEqual(['x', 'y']);
    DB().updateAiAnalysis('ig-1', { tags: [] });
    expect(tagNorms('ig-1')).toEqual([]);
    expect(entNorms('ig-1')).toEqual(['z']); // entities untouched (not provided)
    DB().updateAiAnalysis('ig-1', { entities: null });
    expect(entNorms('ig-1')).toEqual([]);
  });

  it('does NOT touch the derived tables when the field is undefined', () => {
    DB().upsertPost(igPost);
    DB().updateAiAnalysis('ig-1', { tags: ['keep'], entities: ['Keep'] });
    DB().updateAiAnalysis('ig-1', { status: 'analyzing' }); // neither tags nor entities provided
    expect(tagNorms('ig-1')).toEqual(['keep']);
    expect(entNorms('ig-1')).toEqual(['keep']);
  });

  it('dedupes case-variant tags within one post (one row per norm)', () => {
    DB().upsertPost(igPost);
    DB().updateAiAnalysis('ig-1', { tags: ['Cucina', 'cucina', '  CUCINA '] });
    expect(tagNorms('ig-1')).toEqual(['cucina']);
  });
});

d('migrate() backfill of derived tables', () => {
  it('backfills post_tags / post_entities from pre-existing ai_tags JSON', () => {
    // Seed a post and write ai_tags DIRECTLY (bypassing updateAiAnalysis), then
    // clear the derived rows to simulate a DB created before the index existed.
    DB().upsertPost(igPost);
    SQL()
      .prepare("UPDATE posts SET ai_tags = ?, ai_entities = ? WHERE id = 'ig-1'")
      .run(JSON.stringify(['Pasta', 'cucina']), JSON.stringify(['Roma']));
    SQL().exec('DELETE FROM post_tags; DELETE FROM post_entities;');
    expect(tagNorms('ig-1')).toEqual([]);

    // The backfill (run inside migrate) must repopulate from the JSON source.
    DB().__backfillDerivedTags();
    expect(tagNorms('ig-1')).toEqual(['cucina', 'pasta']);
    expect(entNorms('ig-1')).toEqual(['roma']);
  });

  it('is idempotent: a second backfill does not duplicate rows', () => {
    DB().upsertPost(igPost);
    SQL()
      .prepare("UPDATE posts SET ai_tags = ? WHERE id = 'ig-1'")
      .run(JSON.stringify(['x', 'y']));
    SQL().exec('DELETE FROM post_tags;');
    DB().__backfillDerivedTags();
    DB().__backfillDerivedTags();
    expect(tagNorms('ig-1')).toEqual(['x', 'y']);
  });
});

d('getPostsForAnalysis()', () => {
  it('returns lightweight posts for explicit ids in input order, media attached', () => {
    DB().upsertPost({ ...igPost, id: 'p1', shortcode: 's1' });
    DB().upsertPost({ ...twPost, id: 'p2', shortcode: 's2' });
    const posts = DB().getPostsForAnalysis({ ids: ['p2', 'p1'] });
    expect(posts.map((p) => p.id)).toEqual(['p2', 'p1']);
    expect(posts[1]).toMatchObject({ id: 'p1', shortcode: 's1', authorUsername: 'testuser' });
    expect(Array.isArray(posts[1].media)).toBe(true);
    // No collections attached for the analysis shape.
    expect((posts[1] as Shelfy.Post).collectionIds).toBeUndefined();
  });

  it('missingOnly returns only un-analyzed posts', () => {
    seedAnalyzed('done1', {}, { tags: ['x'] }); // ai_status = 'done'
    DB().upsertPost({ ...igPost, id: 'raw1', shortcode: 'raw1' }); // never analyzed
    const posts = DB().getPostsForAnalysis({ missingOnly: true });
    expect(posts.map((p) => p.id)).toEqual(['raw1']);
  });

  it('returns all posts when no ids and not missingOnly', () => {
    DB().upsertPost({ ...igPost, id: 'a', shortcode: 'a' });
    DB().upsertPost({ ...igPost, id: 'b', shortcode: 'b' });
    expect(DB().getPostsForAnalysis().length).toBe(2);
  });

  it('returns [] for an empty ids array', () => {
    DB().upsertPost(igPost);
    expect(DB().getPostsForAnalysis({ ids: [] })).toEqual([]);
  });
});

d('tag / entity filters use the derived index (case-insensitive)', () => {
  beforeEach(() => {
    seedAnalyzed(
      'food',
      { timestamp: '2024-03-05T00:00:00.000Z' },
      {
        tags: ['Pasta', 'Cucina'],
        entities: ['Roma'],
      },
    );
    seedAnalyzed(
      'tech',
      { timestamp: '2024-03-04T00:00:00.000Z' },
      {
        tags: ['Gadget', 'Cucina'],
        entities: ['Apple'],
      },
    );
  });

  it('tag filter is now case-insensitive (matches regardless of stored casing)', () => {
    // Stored as 'Pasta' but querying lowercase 'pasta' matches via tag_norm.
    expect(
      DB()
        .getPosts({ tag: 'pasta' })
        .posts.map((p) => p.id),
    ).toEqual(['food']);
    expect(
      DB()
        .getPosts({ tag: 'PASTA' })
        .posts.map((p) => p.id),
    ).toEqual(['food']);
  });

  it('entity filter is case-insensitive', () => {
    expect(
      DB()
        .getPosts({ entity: 'apple' })
        .posts.map((p) => p.id),
    ).toEqual(['tech']);
  });

  it('tags AND / OR via the index', () => {
    expect(
      DB()
        .getPosts({ tags: ['pasta', 'cucina'], tagMode: 'and' })
        .posts.map((p) => p.id),
    ).toEqual(['food']);
    expect(
      DB()
        .getPosts({ tags: ['pasta', 'gadget'], tagMode: 'or' })
        .posts.map((p) => p.id)
        .sort(),
    ).toEqual(['food', 'tech']);
    expect(DB().getPosts({ tags: ['pasta', 'gadget'], tagMode: 'and' }).posts).toHaveLength(0);
  });

  it('getPostIdsByTags hits the index, case-insensitive', () => {
    expect(DB().getPostIdsByTags(['CUCINA'], 'or').sort()).toEqual(['food', 'tech']);
    expect(DB().getPostIdsByTags(['pasta', 'cucina'], 'and')).toEqual(['food']);
    expect(DB().getPostIdsByTags(['pasta', 'gadget'], 'and')).toEqual([]);
  });
});

d('mergeTags keeps the derived index in sync', () => {
  it('rebuilds post_tags after a merge', () => {
    seedAnalyzed('a', {}, { tags: ['pasta', 'spaghetti'] });
    DB().mergeTags(['pasta', 'spaghetti'], 'primo');
    expect(tagNorms('a')).toEqual(['primo']);
    // Filtering by the new tag works; the old ones no longer match.
    expect(
      DB()
        .getPosts({ tag: 'primo' })
        .posts.map((p) => p.id),
    ).toEqual(['a']);
    expect(DB().getPosts({ tag: 'pasta' }).posts).toHaveLength(0);
  });
});
