import { describe, it, expect, beforeAll, afterAll, beforeEach } from 'vitest';
import { createRequire } from 'module';
import { tmpdir } from 'os';
import { join } from 'path';
import { existsSync, unlinkSync, writeFileSync } from 'fs';

// Inject a mock for `electron` into Node's require cache before db.js is
// loaded. db.js lives in a CommonJS package so vi.mock() hoisting does not
// intercept its synchronous require('electron') call — patching the cache
// directly is the reliable cross-version solution.
const req = createRequire(import.meta.url);
const electronMockPath = req.resolve('electron');
req.cache[electronMockPath] = {
  id: electronMockPath,
  filename: electronMockPath,
  loaded: true,
  exports: { app: { getPath: () => tmpdir() } },
  children: [],
  paths: [],
};

// ABI guard: better-sqlite3 may be compiled for Electron's ABI rather than the
// current Node's. When that happens the native require throws; skip the suites
// that need the DB instead of failing the whole run.
let db,
  dbInstance,
  loadErr = null;
try {
  db = req('../../electron/db.js');
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

const dbPath = join(tmpdir(), 'shelfy.sqlite');

const igPost = {
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

const twPost = {
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
  if (!dbInstance) dbInstance = db.initialize();
});

afterAll(() => {
  if (dbInstance) dbInstance.close();
  if (existsSync(dbPath)) unlinkSync(dbPath);
});

beforeEach(() => {
  if (!dbInstance) return;
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
    expect(typeof dbInstance.prepare).toBe('function');
  });

  it('returns the same instance on a second call (singleton)', () => {
    const second = db.initialize();
    expect(second).toBe(dbInstance);
  });
});

// ---------------------------------------------------------------------------
// upsertPost / getPost
// ---------------------------------------------------------------------------

d('upsertPost / getPost', () => {
  it('inserts a post and retrieves it by id', () => {
    db.upsertPost(igPost);
    const result = db.getPost('ig-1');
    expect(result).not.toBeNull();
    expect(result.id).toBe('ig-1');
  });

  it('maps camelCase fields correctly to/from snake_case columns', () => {
    db.upsertPost(igPost);
    const result = db.getPost('ig-1');
    expect(result.platform).toBe('instagram');
    expect(result.shortcode).toBe('ABC123');
    expect(result.postUrl).toBe('https://www.instagram.com/p/ABC123/');
    expect(result.profileUrl).toBe('https://www.instagram.com/testuser/');
    expect(result.authorUsername).toBe('testuser');
    expect(result.authorName).toBe('');
    expect(result.text).toBe('Test caption');
    expect(result.thumbnailUrl).toBe('https://cdn.example.com/thumb.jpg');
    expect(result.mediaType).toBe('image');
    expect(result.timestamp).toBe('2024-01-15T10:00:00.000Z');
  });

  it('returns null for an unknown id', () => {
    const result = db.getPost('does-not-exist');
    expect(result).toBeNull();
  });

  it('upsertPost replaces an existing post', () => {
    db.upsertPost(igPost);
    db.upsertPost({ ...igPost, text: 'Updated caption' });
    const result = db.getPost('ig-1');
    expect(result.text).toBe('Updated caption');
  });
});

// ---------------------------------------------------------------------------
// getPosts with filters
// ---------------------------------------------------------------------------

d('getPosts()', () => {
  beforeEach(() => {
    db.upsertPost(igPost);
    db.upsertPost(twPost);
  });

  it('returns { posts, total } shape', () => {
    const result = db.getPosts();
    expect(result).toHaveProperty('posts');
    expect(result).toHaveProperty('total');
    expect(Array.isArray(result.posts)).toBe(true);
  });

  it('returns all posts when no filters are given', () => {
    const { posts, total } = db.getPosts();
    expect(posts).toHaveLength(2);
    expect(total).toBe(2);
  });

  it('filters by platform: instagram', () => {
    const { posts, total } = db.getPosts({ platform: 'instagram' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('ig-1');
    expect(total).toBe(1);
  });

  it('filters by platform: twitter', () => {
    const { posts, total } = db.getPosts({ platform: 'twitter' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('tw-1');
    expect(total).toBe(1);
  });

  it('filters by mediaType: video', () => {
    db.upsertPost({
      ...igPost,
      id: 'ig-vid',
      mediaType: 'video',
      timestamp: '2024-01-20T00:00:00.000Z',
    });
    const { posts, total } = db.getPosts({ mediaType: 'video' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('ig-vid');
    expect(total).toBe(1);
  });

  it('filters by mediaType: image', () => {
    const { posts } = db.getPosts({ mediaType: 'image' });
    expect(posts).toHaveLength(2);
  });

  it('search filters on text (LIKE %search%)', () => {
    const { posts } = db.getPosts({ search: 'caption' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('ig-1');
  });

  it('search filters on author_username (LIKE %search%)', () => {
    const { posts } = db.getPosts({ search: 'tweetuser' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('tw-1');
  });

  it('search filters on shortcode (LIKE %search%)', () => {
    const { posts } = db.getPosts({ search: 'ABC123' });
    expect(posts).toHaveLength(1);
    expect(posts[0].id).toBe('ig-1');
  });

  it('respects limit but total reflects full count', () => {
    const { posts, total } = db.getPosts({ limit: 1 });
    expect(posts).toHaveLength(1);
    expect(total).toBe(2);
  });

  it('respects offset', () => {
    const { posts: all } = db.getPosts();
    const { posts: withOffset } = db.getPosts({ limit: 50, offset: 1 });
    expect(withOffset).toHaveLength(1);
    expect(withOffset[0].id).not.toBe(all[0].id);
  });

  it('orders by timestamp DESC', () => {
    const { posts } = db.getPosts();
    expect(posts[0].timestamp > posts[1].timestamp).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// bulkUpsert()
// ---------------------------------------------------------------------------

d('bulkUpsert()', () => {
  it('inserts new posts', () => {
    db.bulkUpsert([igPost, twPost]);
    const { posts } = db.getPosts();
    expect(posts).toHaveLength(2);
  });

  it('INSERT OR IGNORE: does not overwrite an existing post that has a download path set', () => {
    db.upsertPost({ ...igPost, text: 'Original text' });
    db.updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    db.bulkUpsert([{ ...igPost, text: 'Should not overwrite' }]);
    const result = db.getPost('ig-1');
    expect(result.text).toBe('Original text');
    expect(result.thumbnailPath).toBe('/local/thumb.jpg');
  });

  it('updates metadata for an existing post WITHOUT download paths', () => {
    db.upsertPost({
      ...igPost,
      text: 'Old text',
      profileUrl: 'https://www.instagram.com/olduser/',
    });
    db.bulkUpsert([
      { ...igPost, text: 'New text', profileUrl: 'https://www.instagram.com/newuser/' },
    ]);
    const result = db.getPost('ig-1');
    expect(result.text).toBe('New text');
    expect(result.profileUrl).toBe('https://www.instagram.com/newuser/');
  });

  it('inserts all posts in a single transaction', () => {
    const posts = [
      { ...igPost, id: 'bulk-1', timestamp: '2024-02-01T00:00:00.000Z' },
      { ...igPost, id: 'bulk-2', timestamp: '2024-02-02T00:00:00.000Z' },
      { ...igPost, id: 'bulk-3', timestamp: '2024-02-03T00:00:00.000Z' },
    ];
    db.bulkUpsert(posts);
    const { posts: results } = db.getPosts();
    expect(results).toHaveLength(3);
  });
});

// ---------------------------------------------------------------------------
// getStats()
// ---------------------------------------------------------------------------

d('getStats()', () => {
  it('returns total count of 0 when no posts exist', () => {
    const stats = db.getStats();
    expect(stats.total).toBe(0);
  });

  it('returns correct total count', () => {
    db.upsertPost(igPost);
    db.upsertPost(twPost);
    const stats = db.getStats();
    expect(stats.total).toBe(2);
  });

  it('byPlatform counts instagram and twitter correctly', () => {
    db.upsertPost(igPost);
    db.upsertPost({ ...igPost, id: 'ig-2', timestamp: '2024-01-16T00:00:00.000Z' });
    db.upsertPost(twPost);
    const stats = db.getStats();
    expect(stats.byPlatform.instagram).toBe(2);
    expect(stats.byPlatform.twitter).toBe(1);
  });

  it('byPlatform defaults to 0 for platforms with no posts', () => {
    const stats = db.getStats();
    expect(stats.byPlatform.instagram).toBe(0);
    expect(stats.byPlatform.twitter).toBe(0);
  });

  it('byMediaType counts correctly', () => {
    db.upsertPost(igPost);
    db.upsertPost({
      ...igPost,
      id: 'ig-vid',
      mediaType: 'video',
      timestamp: '2024-01-20T00:00:00.000Z',
    });
    const stats = db.getStats();
    expect(stats.byMediaType.image).toBe(1);
    expect(stats.byMediaType.video).toBe(1);
  });

  it('downloaded count includes only posts with at least one path set', () => {
    db.upsertPost(igPost);
    db.upsertPost(twPost);
    db.updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    const stats = db.getStats();
    expect(stats.downloaded).toBe(1);
  });

  it('downloaded count is 0 when no paths are set', () => {
    db.upsertPost(igPost);
    db.upsertPost(twPost);
    const stats = db.getStats();
    expect(stats.downloaded).toBe(0);
  });

  it('downloadedByType counts each path column independently', () => {
    db.upsertPost(igPost);
    db.upsertPost(twPost);
    db.upsertPost({ ...igPost, id: 'ig-3', timestamp: '2024-01-17T00:00:00.000Z' });
    db.updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg', imagePath: '/local/img.jpg' });
    db.updatePaths('tw-1', { videoPath: '/local/vid.mp4' });
    const stats = db.getStats();
    expect(stats.downloaded).toBe(2);
    expect(stats.downloadedByType).toEqual({ thumbnails: 1, images: 1, videos: 1 });
  });

  it('memoizes the result until a write invalidates it', () => {
    db.upsertPost(igPost);
    expect(db.getStats().total).toBe(1);
    // Raw delete bypasses db.js's write paths: the memoized stats survive.
    dbInstance.exec('DELETE FROM posts;');
    expect(db.getStats().total).toBe(1);
    db.invalidateGlobalCaches();
    expect(db.getStats().total).toBe(0);
  });

  it('updatePaths invalidates the memoized stats', () => {
    db.upsertPost(igPost);
    expect(db.getStats().downloaded).toBe(0);
    db.updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    expect(db.getStats().downloaded).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// termIdfWeights() — IDF cache
// ---------------------------------------------------------------------------

d('termIdfWeights() cache', () => {
  const cols = ['text'];

  it('weights rarer terms higher than ubiquitous ones', () => {
    for (let i = 0; i < 10; i++) {
      db.upsertPost({
        ...igPost,
        id: `idf-${i}`,
        text: i === 0 ? 'common rareword' : 'common filler',
      });
    }
    const w = db.__termIdfWeights(['common', 'rareword'], cols);
    expect(w.common).toBe(1);
    expect(w.rareword).toBeGreaterThan(w.common);
  });

  it('memoizes per-term weights until invalidateGlobalCaches', () => {
    for (let i = 0; i < 10; i++) {
      db.upsertPost({
        ...igPost,
        id: `idf-${i}`,
        text: i === 0 ? 'common rareword' : 'common filler',
      });
    }
    const before = db.__termIdfWeights(['rareword'], cols).rareword;
    expect(before).toBeGreaterThan(1);
    // Raw update bypasses db.js's write paths: the cached weight survives.
    dbInstance.exec("UPDATE posts SET text = 'common rareword';");
    expect(db.__termIdfWeights(['rareword'], cols).rareword).toBe(before);
    db.invalidateGlobalCaches();
    expect(db.__termIdfWeights(['rareword'], cols).rareword).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// updatePaths()
// ---------------------------------------------------------------------------

d('updatePaths()', () => {
  beforeEach(() => {
    db.upsertPost(igPost);
  });

  it('sets thumbnailPath for a post', () => {
    db.updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    const result = db.getPost('ig-1');
    expect(result.thumbnailPath).toBe('/local/thumb.jpg');
  });

  it('sets imagePath for a post', () => {
    db.updatePaths('ig-1', { imagePath: '/local/image.jpg' });
    const result = db.getPost('ig-1');
    expect(result.imagePath).toBe('/local/image.jpg');
  });

  it('sets videoPath for a post', () => {
    db.updatePaths('ig-1', { videoPath: '/local/video.mp4' });
    const result = db.getPost('ig-1');
    expect(result.videoPath).toBe('/local/video.mp4');
  });

  it('COALESCE: passing null does not overwrite an existing thumbnailPath', () => {
    db.updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    db.updatePaths('ig-1', { thumbnailPath: null });
    const result = db.getPost('ig-1');
    expect(result.thumbnailPath).toBe('/local/thumb.jpg');
  });

  it('COALESCE: can update one path without clearing others', () => {
    db.updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg', imagePath: '/local/image.jpg' });
    db.updatePaths('ig-1', { videoPath: '/local/video.mp4' });
    const result = db.getPost('ig-1');
    expect(result.thumbnailPath).toBe('/local/thumb.jpg');
    expect(result.imagePath).toBe('/local/image.jpg');
    expect(result.videoPath).toBe('/local/video.mp4');
  });

  it('persists thumbBlur alongside the paths, COALESCE-protected like them', () => {
    db.updatePaths('ig-1', {
      thumbnailPath: '/local/thumb.jpg',
      thumbBlur: 'data:image/jpeg;base64,AAAA',
    });
    expect(db.getPost('ig-1').thumbBlur).toBe('data:image/jpeg;base64,AAAA');
    // A later path-only update must not clear the stored placeholder.
    db.updatePaths('ig-1', { videoPath: '/local/video.mp4' });
    expect(db.getPost('ig-1').thumbBlur).toBe('data:image/jpeg;base64,AAAA');
  });
});

// ---------------------------------------------------------------------------
// Blur-up placeholders (thumb_blur)
// ---------------------------------------------------------------------------

d('thumb_blur helpers', () => {
  beforeEach(() => {
    db.upsertPost(igPost);
    db.upsertPost(twPost);
  });

  it('listPostsMissingThumbBlur returns posts with a local cover and no placeholder', () => {
    db.updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    const rows = db.listPostsMissingThumbBlur();
    expect(rows).toEqual([{ id: 'ig-1', src: '/local/thumb.jpg' }]);
  });

  it('listPostsMissingThumbBlur prefers thumbnail_path over image_path as src', () => {
    db.updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg', imagePath: '/local/image.jpg' });
    expect(db.listPostsMissingThumbBlur()).toEqual([{ id: 'ig-1', src: '/local/thumb.jpg' }]);
  });

  it("setThumbBlur removes the post from the missing list, '' sentinel included", () => {
    db.updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    db.updatePaths('tw-1', { imagePath: '/local/image.jpg' });
    db.setThumbBlur('ig-1', 'data:image/jpeg;base64,AAAA');
    db.setThumbBlur('tw-1', ''); // tried, ineligible — must not be rescanned
    expect(db.listPostsMissingThumbBlur()).toEqual([]);
  });

  it("rowToPost maps the '' sentinel back to null for the renderer", () => {
    db.updatePaths('ig-1', { thumbnailPath: '/local/thumb.jpg' });
    db.setThumbBlur('ig-1', '');
    expect(db.getPost('ig-1').thumbBlur).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// importFromJSON()
// ---------------------------------------------------------------------------

d('importFromJSON()', () => {
  const tempJsonPath = join(tmpdir(), 'test-import.json');

  it('reads a JSON array, inserts posts, and returns { imported: N }', async () => {
    writeFileSync(tempJsonPath, JSON.stringify([igPost, twPost]), 'utf8');
    const result = await db.importFromJSON(tempJsonPath);
    expect(result).toMatchObject({ imported: 2 });
    expect(db.getPost('ig-1')).not.toBeNull();
    expect(db.getPost('tw-1')).not.toBeNull();
  });

  it('reads a JSON object with a posts array', async () => {
    writeFileSync(tempJsonPath, JSON.stringify({ posts: [igPost] }), 'utf8');
    const result = await db.importFromJSON(tempJsonPath);
    expect(result).toMatchObject({ imported: 1 });
  });

  it('returns { imported: 0 } on a second import of the same file (INSERT OR IGNORE)', async () => {
    writeFileSync(tempJsonPath, JSON.stringify([igPost, twPost]), 'utf8');
    await db.importFromJSON(tempJsonPath);
    const second = await db.importFromJSON(tempJsonPath);
    expect(second).toMatchObject({ imported: 0 });
  });
});

// ---------------------------------------------------------------------------
// AI analysis helpers
// ---------------------------------------------------------------------------

// Insert a base post then run updateAiAnalysis with the given AI fields.
function seedAnalyzed(id, overrides = {}, ai = {}) {
  db.upsertPost({
    ...igPost,
    id,
    shortcode: id,
    text: overrides.text ?? `caption ${id}`,
    mediaType: overrides.mediaType ?? 'image',
    timestamp: overrides.timestamp ?? '2024-03-01T00:00:00.000Z',
    ...overrides,
  });
  db.updateAiAnalysis(id, { status: 'done', model: 'test-model', ...ai });
}

// ---------------------------------------------------------------------------
// updateAiAnalysis() — new fields round-trip via rowToPost
// ---------------------------------------------------------------------------

d('updateAiAnalysis()', () => {
  it('persists all new AI fields and rowToPost reads them back', () => {
    db.upsertPost(igPost);
    db.updateAiAnalysis('ig-1', {
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

    const p = db.getPost('ig-1');
    expect(p.aiDescription).toBe('una ricetta di pasta');
    expect(p.aiTags).toEqual(['pasta', 'cucina']);
    expect(p.aiStatus).toBe('done');
    expect(p.aiModel).toBe('test-model');
    expect(p.aiCategory).toBe('Cibo & Ricette');
    expect(p.aiContentType).toBe('ricetta');
    expect(p.aiEntities).toEqual(['Carbonara', 'Roma']);
    expect(p.aiKeywords).toEqual(['come fare la carbonara', 'pasta romana']);
    expect(p.aiLanguage).toBe('it');
    expect(p.aiSaveReason).toBe('da provare nel weekend');
    expect(typeof p.aiAnalyzedAt).toBe('number');
  });

  it('defaults array fields to [] when never set', () => {
    db.upsertPost(igPost);
    const p = db.getPost('ig-1');
    expect(p.aiEntities).toEqual([]);
    expect(p.aiKeywords).toEqual([]);
    expect(p.aiTags).toEqual([]);
    expect(p.aiCategory).toBeNull();
  });

  it('COALESCE: a status-only update does not clear existing fields', () => {
    db.upsertPost(igPost);
    db.updateAiAnalysis('ig-1', { tags: ['x'], category: 'Altro', status: 'done' });
    db.updateAiAnalysis('ig-1', { status: 'analyzing' });
    const p = db.getPost('ig-1');
    expect(p.aiStatus).toBe('analyzing');
    expect(p.aiTags).toEqual(['x']);
    expect(p.aiCategory).toBe('Altro');
  });

  it('explicit { status: null } clears ai_status (cancelled job) without touching other fields', () => {
    db.upsertPost(igPost);
    db.updateAiAnalysis('ig-1', { tags: ['x'], category: 'Altro', status: 'analyzing' });
    db.updateAiAnalysis('ig-1', { status: null });
    const p = db.getPost('ig-1');
    expect(p.aiStatus).toBeNull();
    expect(p.aiTags).toEqual(['x']);
    expect(p.aiCategory).toBe('Altro');
  });

  it('an empty update is a no-op and leaves existing fields intact', () => {
    db.upsertPost(igPost);
    db.updateAiAnalysis('ig-1', { tags: ['x'], status: 'done' });
    db.updateAiAnalysis('ig-1', {});
    const p = db.getPost('ig-1');
    expect(p.aiStatus).toBe('done');
    expect(p.aiTags).toEqual(['x']);
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
    const tags = db.getFrequentTags(10);
    expect(tags[0].toLowerCase()).toBe('cucina');
    expect(tags).toContain('pasta');
    expect(tags).toContain('viaggio');
  });

  it('respects the limit', () => {
    seedAnalyzed('p1', {}, { tags: ['a', 'b', 'c', 'd'] });
    expect(db.getFrequentTags(2)).toHaveLength(2);
  });

  it('returns the most common casing as display form', () => {
    seedAnalyzed('p1', {}, { tags: ['Cucina'] });
    seedAnalyzed('p2', {}, { tags: ['cucina'] });
    seedAnalyzed('p3', {}, { tags: ['cucina'] });
    expect(db.getFrequentTags(5)[0]).toBe('cucina');
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
    db.upsertPost({
      ...igPost,
      id: 'raw',
      shortcode: 'raw',
      timestamp: '2024-03-03T00:00:00.000Z',
    });
  });

  it('filters by single tag', () => {
    const { posts } = db.getPosts({ tag: 'pasta' });
    expect(posts.map((p) => p.id)).toEqual(['food']);
  });

  it('filters by tags with tagMode "or"', () => {
    const { posts } = db.getPosts({ tags: ['pasta', 'gadget'], tagMode: 'or' });
    expect(posts.map((p) => p.id).sort()).toEqual(['food', 'tech']);
  });

  it('filters by tags with tagMode "and"', () => {
    const { posts } = db.getPosts({ tags: ['pasta', 'cucina'], tagMode: 'and' });
    expect(posts.map((p) => p.id)).toEqual(['food']);
    const none = db.getPosts({ tags: ['pasta', 'gadget'], tagMode: 'and' });
    expect(none.posts).toHaveLength(0);
  });

  it('filters by category', () => {
    const { posts } = db.getPosts({ category: 'Tech & Strumenti' });
    expect(posts.map((p) => p.id)).toEqual(['tech']);
  });

  it('filters by contentType', () => {
    const { posts } = db.getPosts({ contentType: 'ricetta' });
    expect(posts.map((p) => p.id)).toEqual(['food']);
  });

  it('filters by entity', () => {
    const { posts } = db.getPosts({ entity: 'Apple' });
    expect(posts.map((p) => p.id)).toEqual(['tech']);
  });

  it('filters by analyzedStatus "analyzed"', () => {
    const { posts } = db.getPosts({ analyzedStatus: 'analyzed' });
    expect(posts.map((p) => p.id).sort()).toEqual(['food', 'tech']);
  });

  it('filters by analyzedStatus "unanalyzed"', () => {
    const { posts } = db.getPosts({ analyzedStatus: 'unanalyzed' });
    expect(posts.map((p) => p.id)).toEqual(['raw']);
  });

  it('filters by aiTagged "tagged"', () => {
    const { posts } = db.getPosts({ aiTagged: 'tagged' });
    expect(posts.map((p) => p.id).sort()).toEqual(['food', 'tech']);
  });

  it('filters by aiTagged "untagged"', () => {
    const { posts } = db.getPosts({ aiTagged: 'untagged' });
    expect(posts.map((p) => p.id)).toEqual(['raw']);
  });

  it('escapes LIKE wildcards: a tag query with % / _ does not match literal tags', () => {
    seedAnalyzed('wild', { timestamp: '2024-03-06T00:00:00.000Z' }, { tags: ['100%off', 'a_b'] });
    // '%' must not act as a wildcard: 'p%a' should NOT match 'pasta'.
    expect(db.getPosts({ tag: 'p%a' }).posts).toHaveLength(0);
    // '_' must not act as a single-char wildcard: 'a_b' must only match the literal.
    expect(db.getPosts({ tag: 'a_b' }).posts.map((p) => p.id)).toEqual(['wild']);
    // Literal '%' still matches when present verbatim.
    expect(db.getPosts({ tag: '100%off' }).posts.map((p) => p.id)).toEqual(['wild']);
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
    expect(db.getPosts({ search: 'p%a' }).posts).toHaveLength(0);
    // literal '%' in text is found.
    expect(db.getPosts({ search: '50%' }).posts.map((p) => p.id)).toEqual(['pct']);
    // entity '_' is literal, not a single-char wildcard.
    expect(db.getPosts({ entity: 'C_3PO' }).posts.map((p) => p.id)).toEqual(['pct']);
    expect(db.getPosts({ entity: 'CX3PO' }).posts).toHaveLength(0);
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
    db.upsertPost({ ...igPost, id: 'c', shortcode: 'c' });

    const ov = db.getAiOverview();
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

    const stats = db.getTagStats();
    const cucina = stats.find((s) => s.tag.toLowerCase() === 'cucina');
    expect(cucina.count).toBe(2);
    expect(cucina.lastUsed).toBe('2024-03-10T00:00:00.000Z');
    const catMap = Object.fromEntries(cucina.categories.map((c) => [c.category, c.count]));
    expect(catMap).toEqual({ 'Cibo & Ricette': 1, 'Tech & Strumenti': 1 });
  });

  it('counts a tag once per post even if duplicated within it', () => {
    seedAnalyzed('a', {}, { tags: ['cucina', 'Cucina'] });
    const cucina = db.getTagStats().find((s) => s.tag.toLowerCase() === 'cucina');
    expect(cucina.count).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// getEntityStats()
// ---------------------------------------------------------------------------

d('getEntityStats()', () => {
  it('counts entities case-insensitively, preserving casing', () => {
    seedAnalyzed('a', {}, { tags: ['x'], entities: ['Roma', 'Apple'] });
    seedAnalyzed('b', {}, { tags: ['x'], entities: ['roma'] });
    const stats = db.getEntityStats();
    const roma = stats.find((s) => s.entity.toLowerCase() === 'roma');
    expect(roma.count).toBe(2);
    expect(roma.entity).toBe('Roma'); // most common casing
    expect(stats.find((s) => s.entity === 'Apple').count).toBe(1);
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

    const co = db.getTagCooccurrence('cucina');
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

    const res = db.searchTagsByText('devo fare un moodboard di cuffie');
    const map = Object.fromEntries(res.map((r) => [r.tag.toLowerCase(), r.count]));
    expect(map.cuffie).toBe(2);
    expect(map.raymarching).toBeUndefined(); // unrelated tag not pulled in
    expect(res[0].tag.toLowerCase()).toBe('cuffie'); // most-used first
  });

  it('matches on substrings of the tag, not just whole words', () => {
    seedAnalyzed('a', {}, { tags: ['headphones'] });
    const res = db.searchTagsByText('phone');
    expect(res.map((r) => r.tag.toLowerCase())).toContain('headphones');
  });

  it('ignores tokens shorter than minTermLen and empty queries', () => {
    seedAnalyzed('a', {}, { tags: ['cuffie'] });
    expect(db.searchTagsByText('di un', { minTermLen: 3 })).toEqual([]);
    expect(db.searchTagsByText('')).toEqual([]);
  });

  it('respects the limit', () => {
    seedAnalyzed('a', {}, { tags: ['audio-uno', 'audio-due', 'audio-tre'] });
    expect(db.searchTagsByText('audio', { limit: 2 }).length).toBe(2);
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

    const groups = await db.getTagCandidateGroups({ minTagCount: 2, minJaccard: 0.1 });
    expect(groups.length).toBeGreaterThanOrEqual(1);
    const g = groups.find((x) => x.tags.includes('cucina') && x.tags.includes('pasta'));
    expect(g).toBeTruthy();
    expect(g.neighbors).toBeTruthy();
    // candidate tags are normalized keys
    expect(g.tags).toEqual(expect.arrayContaining(['cucina', 'pasta']));
  });

  it('does NOT fuse two unrelated dense groups bridged only by a hub tag', async () => {
    // Two tight themes, each co-occurring strongly within itself...
    for (const id of ['s1', 's2', 's3']) seedAnalyzed(id, {}, { tags: ['glsl', 'shader', 'hub'] });
    for (const id of ['t1', 't2', 't3']) seedAnalyzed(id, {}, { tags: ['blender', '3d', 'hub'] });
    // ...'hub' co-occurs with everything but, being frequent, gets a LOW Jaccard
    // weight and must not merge shaders with 3d into one blob.
    const groups = await db.getTagCandidateGroups({ minTagCount: 2, minJaccard: 0.3 });
    const withShader = groups.find((g) => g.tags.includes('glsl'));
    const withBlender = groups.find((g) => g.tags.includes('blender'));
    expect(withShader).toBeTruthy();
    expect(withBlender).toBeTruthy();
    expect(withShader).not.toBe(withBlender);
    expect(withShader.tags).not.toContain('blender');
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
    const a = db.buildTagCommunities(freq, edges, { minJaccard: 0.3 });
    const b = db.buildTagCommunities(freq, edges, { minJaccard: 0.3 });
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

    const { count } = db.saveClusterRun([{ label: 'Shader & GPU', tags: ['glsl', 'shader'] }]);
    expect(count).toBe(1);

    const clusters = db.getTagClusters();
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

    db.saveClusterRun([{ label: 'Shader', tags: ['glsl', 'shader'] }]);
    const accepted = db.getTagClusters()[0];
    db.setClusterStatus(accepted.id, 'accepted');

    // Re-run proposes a different grouping; accepted 'glsl/shader' stays put.
    db.saveClusterRun([
      { label: 'Shader', tags: ['glsl', 'shader'] }, // these are owned → skipped
      { label: '3D', tags: ['blender', '3d'] },
    ]);

    const clusters = db.getTagClusters();
    const acceptedNow = clusters.find((c) => c.status === 'accepted');
    expect(acceptedNow.topTag).toBe('Shader');
    expect(acceptedNow.tags.map((t) => t.toLowerCase())).toEqual(
      expect.arrayContaining(['glsl', 'shader']),
    );
    const proposed = clusters.find((c) => c.status === 'proposed');
    expect(proposed.tags.map((t) => t.toLowerCase())).toEqual(
      expect.arrayContaining(['blender', '3d']),
    );
    expect(proposed.tags.map((t) => t.toLowerCase())).not.toContain('glsl');
  });

  it('dismiss deletes the cluster; rename and removeTag mutate it', () => {
    seedAnalyzed('a', {}, { tags: ['glsl', 'shader', 'webgl'] });
    seedAnalyzed('b', {}, { tags: ['glsl', 'shader', 'webgl'] });
    db.saveClusterRun([{ label: 'Shader', tags: ['glsl', 'shader', 'webgl'] }]);
    const c = db.getTagClusters()[0];

    db.renameCluster(c.id, 'Shader & GPU');
    expect(db.getTagClusters()[0].topTag).toBe('Shader & GPU');

    db.removeTagFromCluster('webgl', c.id);
    expect(db.getTagClusters()[0].tags.map((t) => t.toLowerCase())).not.toContain('webgl');

    db.setClusterStatus(c.id, 'dismissed');
    expect(db.getTagClusters()).toHaveLength(0);
  });

  it('mergeTags remaps cluster membership: target inherits, sources dropped', () => {
    seedAnalyzed('a', {}, { tags: ['glslang', 'shader'] });
    seedAnalyzed('b', {}, { tags: ['glslang', 'shader'] });
    db.saveClusterRun([{ label: 'Shader', tags: ['glslang', 'shader'] }]);

    // Merge 'glslang' → 'glsl'. The cluster must now contain 'glsl', not 'glslang'.
    db.mergeTags(['glslang'], 'glsl');
    const tags = db.getTagClusters()[0].tags.map((t) => t.toLowerCase());
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

    const suggestions = db.getTagMergeSuggestions();

    // 'ai' / 'AI' normalize to the same key and are within edit distance of 'a.i.'.
    const aiGroup = suggestions.find((g) =>
      [g.canonical, ...g.variants].some((t) => t.toLowerCase() === 'ai'),
    );
    expect(aiGroup).toBeTruthy();
    const aiForms = [aiGroup.canonical, ...aiGroup.variants].map((t) => t.toLowerCase());
    expect(aiForms).toContain('a.i.');

    // 'citta' / 'città' differ only by diacritics → same normalized key.
    const cittaGroup = suggestions.find((g) =>
      [g.canonical, ...g.variants].some((t) => t.toLowerCase() === 'città'),
    );
    expect(cittaGroup).toBeTruthy();
    expect([cittaGroup.canonical, ...cittaGroup.variants].map((t) => t.toLowerCase())).toEqual(
      expect.arrayContaining(['citta', 'città']),
    );
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
    db.upsertPost({ ...igPost, id: 'd', shortcode: 'd' });

    const health = db.getTagHealth();
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
    const res = db.renameTag('cucina', 'cibo');
    expect(res.updated).toBe(2);
    expect(db.getPost('a').aiTags).toContain('cibo');
    expect(db.getPost('a').aiTags).not.toContain('Cucina');
    expect(db.getPost('b').aiTags).toEqual(['cibo']);
  });

  it('mergeTags collapses several sources into one target, de-duplicating', () => {
    seedAnalyzed('a', {}, { tags: ['pasta', 'spaghetti'] });
    const res = db.mergeTags(['pasta', 'spaghetti'], 'primo');
    expect(res.updated).toBe(1);
    expect(db.getPost('a').aiTags).toEqual(['primo']);
  });

  it('mergeTags throws without a target', () => {
    expect(() => db.mergeTags(['x'], '')).toThrow();
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
    expect(db.getPostIds()).toEqual(['food', 'tech']);
  });

  it('honors the same filters as getPosts', () => {
    expect(db.getPostIds({ category: 'Cibo & Ricette' })).toEqual(['food']);
    expect(db.getPostIds({ tag: 'gadget' })).toEqual(['tech']);
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
    const posts = db.getPostsByIds(['z', 'x', 'y']);
    expect(posts.map((p) => p.id)).toEqual(['z', 'x', 'y']);
  });

  it('skips unknown ids and returns [] for empty input', () => {
    seedAnalyzed('x', {}, { tags: ['a'] });
    expect(db.getPostsByIds(['x', 'missing']).map((p) => p.id)).toEqual(['x']);
    expect(db.getPostsByIds([])).toEqual([]);
  });

  it('handles more ids than one batch (> 500) preserving order', () => {
    const ids = [];
    for (let i = 0; i < 600; i++) {
      const id = `b${String(i).padStart(4, '0')}`;
      db.upsertPost({ ...igPost, id, shortcode: id });
      ids.push(id);
    }
    const shuffled = [ids[599], ids[0], ids[250], ids[500]];
    const posts = db.getPostsByIds(shuffled);
    expect(posts.map((p) => p.id)).toEqual(shuffled);
  });
});

// ---------------------------------------------------------------------------
// Derived index tables (post_tags / post_entities)
// ---------------------------------------------------------------------------

// Convenience reads of the derived rows for a post, lowercased norms sorted.
function tagNorms(id) {
  return dbInstance
    .prepare('SELECT tag_norm FROM post_tags WHERE post_id = ? ORDER BY tag_norm')
    .all(id)
    .map((r) => r.tag_norm);
}
function entNorms(id) {
  return dbInstance
    .prepare('SELECT ent_norm FROM post_entities WHERE post_id = ? ORDER BY ent_norm')
    .all(id)
    .map((r) => r.ent_norm);
}

d('derived index sync (updateAiAnalysis)', () => {
  it('populates post_tags / post_entities when tags+entities are provided', () => {
    db.upsertPost(igPost);
    db.updateAiAnalysis('ig-1', { tags: ['Pasta', 'Cucina'], entities: ['Roma'], status: 'done' });
    expect(tagNorms('ig-1')).toEqual(['cucina', 'pasta']);
    expect(entNorms('ig-1')).toEqual(['roma']);
    // tag_form preserves the original casing.
    const forms = dbInstance
      .prepare('SELECT tag_form FROM post_tags WHERE post_id = ? ORDER BY tag_form')
      .all('ig-1')
      .map((r) => r.tag_form);
    expect(forms).toEqual(['Cucina', 'Pasta']);
  });

  it('resyncs post_tags on a subsequent tags update (replace, not append)', () => {
    db.upsertPost(igPost);
    db.updateAiAnalysis('ig-1', { tags: ['a', 'b'] });
    expect(tagNorms('ig-1')).toEqual(['a', 'b']);
    db.updateAiAnalysis('ig-1', { tags: ['b', 'c'] });
    expect(tagNorms('ig-1')).toEqual(['b', 'c']);
  });

  it('clears post_tags when tags is [] or null', () => {
    db.upsertPost(igPost);
    db.updateAiAnalysis('ig-1', { tags: ['x', 'y'], entities: ['Z'] });
    expect(tagNorms('ig-1')).toEqual(['x', 'y']);
    db.updateAiAnalysis('ig-1', { tags: [] });
    expect(tagNorms('ig-1')).toEqual([]);
    expect(entNorms('ig-1')).toEqual(['z']); // entities untouched (not provided)
    db.updateAiAnalysis('ig-1', { entities: null });
    expect(entNorms('ig-1')).toEqual([]);
  });

  it('does NOT touch the derived tables when the field is undefined', () => {
    db.upsertPost(igPost);
    db.updateAiAnalysis('ig-1', { tags: ['keep'], entities: ['Keep'] });
    db.updateAiAnalysis('ig-1', { status: 'analyzing' }); // neither tags nor entities provided
    expect(tagNorms('ig-1')).toEqual(['keep']);
    expect(entNorms('ig-1')).toEqual(['keep']);
  });

  it('dedupes case-variant tags within one post (one row per norm)', () => {
    db.upsertPost(igPost);
    db.updateAiAnalysis('ig-1', { tags: ['Cucina', 'cucina', '  CUCINA '] });
    expect(tagNorms('ig-1')).toEqual(['cucina']);
  });
});

d('migrate() backfill of derived tables', () => {
  it('backfills post_tags / post_entities from pre-existing ai_tags JSON', () => {
    // Seed a post and write ai_tags DIRECTLY (bypassing updateAiAnalysis), then
    // clear the derived rows to simulate a DB created before the index existed.
    db.upsertPost(igPost);
    dbInstance
      .prepare("UPDATE posts SET ai_tags = ?, ai_entities = ? WHERE id = 'ig-1'")
      .run(JSON.stringify(['Pasta', 'cucina']), JSON.stringify(['Roma']));
    dbInstance.exec('DELETE FROM post_tags; DELETE FROM post_entities;');
    expect(tagNorms('ig-1')).toEqual([]);

    // The backfill (run inside migrate) must repopulate from the JSON source.
    db.__backfillDerivedTags();
    expect(tagNorms('ig-1')).toEqual(['cucina', 'pasta']);
    expect(entNorms('ig-1')).toEqual(['roma']);
  });

  it('is idempotent: a second backfill does not duplicate rows', () => {
    db.upsertPost(igPost);
    dbInstance
      .prepare("UPDATE posts SET ai_tags = ? WHERE id = 'ig-1'")
      .run(JSON.stringify(['x', 'y']));
    dbInstance.exec('DELETE FROM post_tags;');
    db.__backfillDerivedTags();
    db.__backfillDerivedTags();
    expect(tagNorms('ig-1')).toEqual(['x', 'y']);
  });
});

d('getPostsForAnalysis()', () => {
  it('returns lightweight posts for explicit ids in input order, media attached', () => {
    db.upsertPost({ ...igPost, id: 'p1', shortcode: 's1' });
    db.upsertPost({ ...twPost, id: 'p2', shortcode: 's2' });
    const posts = db.getPostsForAnalysis({ ids: ['p2', 'p1'] });
    expect(posts.map((p) => p.id)).toEqual(['p2', 'p1']);
    expect(posts[1]).toMatchObject({ id: 'p1', shortcode: 's1', authorUsername: 'testuser' });
    expect(Array.isArray(posts[1].media)).toBe(true);
    // No collections attached for the analysis shape.
    expect(posts[1].collectionIds).toBeUndefined();
  });

  it('missingOnly returns only un-analyzed posts', () => {
    seedAnalyzed('done1', {}, { tags: ['x'] }); // ai_status = 'done'
    db.upsertPost({ ...igPost, id: 'raw1', shortcode: 'raw1' }); // never analyzed
    const posts = db.getPostsForAnalysis({ missingOnly: true });
    expect(posts.map((p) => p.id)).toEqual(['raw1']);
  });

  it('returns all posts when no ids and not missingOnly', () => {
    db.upsertPost({ ...igPost, id: 'a', shortcode: 'a' });
    db.upsertPost({ ...igPost, id: 'b', shortcode: 'b' });
    expect(db.getPostsForAnalysis().length).toBe(2);
  });

  it('returns [] for an empty ids array', () => {
    db.upsertPost(igPost);
    expect(db.getPostsForAnalysis({ ids: [] })).toEqual([]);
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
    expect(db.getPosts({ tag: 'pasta' }).posts.map((p) => p.id)).toEqual(['food']);
    expect(db.getPosts({ tag: 'PASTA' }).posts.map((p) => p.id)).toEqual(['food']);
  });

  it('entity filter is case-insensitive', () => {
    expect(db.getPosts({ entity: 'apple' }).posts.map((p) => p.id)).toEqual(['tech']);
  });

  it('tags AND / OR via the index', () => {
    expect(
      db.getPosts({ tags: ['pasta', 'cucina'], tagMode: 'and' }).posts.map((p) => p.id),
    ).toEqual(['food']);
    expect(
      db
        .getPosts({ tags: ['pasta', 'gadget'], tagMode: 'or' })
        .posts.map((p) => p.id)
        .sort(),
    ).toEqual(['food', 'tech']);
    expect(db.getPosts({ tags: ['pasta', 'gadget'], tagMode: 'and' }).posts).toHaveLength(0);
  });

  it('getPostIdsByTags hits the index, case-insensitive', () => {
    expect(db.getPostIdsByTags(['CUCINA'], 'or').sort()).toEqual(['food', 'tech']);
    expect(db.getPostIdsByTags(['pasta', 'cucina'], 'and')).toEqual(['food']);
    expect(db.getPostIdsByTags(['pasta', 'gadget'], 'and')).toEqual([]);
  });
});

d('mergeTags keeps the derived index in sync', () => {
  it('rebuilds post_tags after a merge', () => {
    seedAnalyzed('a', {}, { tags: ['pasta', 'spaghetti'] });
    db.mergeTags(['pasta', 'spaghetti'], 'primo');
    expect(tagNorms('a')).toEqual(['primo']);
    // Filtering by the new tag works; the old ones no longer match.
    expect(db.getPosts({ tag: 'primo' }).posts.map((p) => p.id)).toEqual(['a']);
    expect(db.getPosts({ tag: 'pasta' }).posts).toHaveLength(0);
  });
});
