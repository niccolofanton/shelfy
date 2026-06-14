import { describe, it, expect, beforeEach, vi } from 'vitest';
import { createRequire } from 'module';
import os from 'os';
import fs from 'fs';
import path from 'path';

// ─── Require cache patching ────────────────────────────────────────────────────
// vi.mock() hoisting only intercepts ESM imports; ipc.js is CJS
// (electron/package.json: {"type":"commonjs"}) so we patch the require cache
// directly before loading the module via createRequire.

const req = createRequire(import.meta.url);

function patchModule(id, exports) {
  const resolved = req.resolve(id);
  req.cache[resolved] = {
    id: resolved,
    filename: resolved,
    loaded: true,
    exports,
    children: [],
    paths: [],
  };
}

// Mock: electron — ipcMain.handle stores each handler so tests can invoke them
// directly, the way the renderer's ipcRenderer.invoke would.
const handlers = new Map();
const ipcMainMock = { handle: vi.fn((channel, fn) => handlers.set(channel, fn)) };
const dialogMock = {
  showSaveDialog: vi.fn(),
  showOpenDialog: vi.fn(),
  showErrorBox: vi.fn(),
};
patchModule('electron', {
  app: { getPath: vi.fn(() => os.tmpdir()), getVersion: vi.fn(() => '0.0.0-test') },
  ipcMain: ipcMainMock,
  dialog: dialogMock,
  shell: { openExternal: vi.fn() },
});

// Mock: local modules required by ipc.js. Only the functions touched at
// registration time (the progress-emitter setters) plus the ones exercised by
// the handlers under test need real stubs.
const dbMock = {
  getLocalFilePaths: vi.fn(),
  deletePosts: vi.fn(),
  getPostIds: vi.fn(),
  getPostsByIds: vi.fn(),
  exportAllPosts: vi.fn(),
  getCollectionsForExport: vi.fn(),
};
patchModule('../../electron/db.js', dbMock);

const downloaderMock = { setProgressEmitter: vi.fn(), enqueueMany: vi.fn() };
patchModule('../../electron/downloader.js', downloaderMock);
patchModule('../../electron/analyzer.js', { setProgressEmitter: vi.fn() });
patchModule('../../electron/weborchestrator.js', {
  setProgressEmitter: vi.fn(),
  setListRefreshEmitter: vi.fn(),
});
patchModule('../../electron/stt.js', {});
patchModule('../../electron/embeddings.js', {});
patchModule('../../electron/updater.js', {});
patchModule('../../electron/binaries.js', {});
patchModule('../../electron/feedback.js', {});
patchModule('../../electron/net-safety.js', {});

const { registerIpcHandlers } = req('../../electron/ipc.js');
registerIpcHandlers({ isDestroyed: () => false, webContents: { send: vi.fn() } });

const invoke = (channel, payload) => handlers.get(channel)(null, payload);

function makeTmpDir() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-ipc-test-'));
}

beforeEach(() => {
  vi.clearAllMocks();
  dbMock.getLocalFilePaths.mockImplementation(() => []);
  dbMock.deletePosts.mockImplementation((ids) => ({ deleted: ids.length }));
  dbMock.getPostIds.mockImplementation(() => []);
  dbMock.getPostsByIds.mockImplementation((ids) => ids.map((id) => ({ id })));
  dbMock.exportAllPosts.mockImplementation(() => []);
  dbMock.getCollectionsForExport.mockImplementation(() => []);
  downloaderMock.enqueueMany.mockReturnValue(undefined);
});

// ─── db:deletePosts ────────────────────────────────────────────────────────────

describe('db:deletePosts', () => {
  it('unlinks every local file then deletes the DB rows', async () => {
    const dir = makeTmpDir();
    const f1 = path.join(dir, 'a.jpg');
    const f2 = path.join(dir, 'b.mp4');
    fs.writeFileSync(f1, 'x');
    fs.writeFileSync(f2, 'x');
    dbMock.getLocalFilePaths.mockImplementation((id) => (id === 'p1' ? [f1, f2] : []));

    const res = await invoke('db:deletePosts', { ids: ['p1', 'p2'] });

    expect(res).toEqual({ ok: true, deleted: 2, errors: [] });
    expect(fs.existsSync(f1)).toBe(false);
    expect(fs.existsSync(f2)).toBe(false);
    expect(dbMock.deletePosts).toHaveBeenCalledWith(['p1', 'p2']);
  });

  it('treats already-missing files (ENOENT) as success, collects other errors', async () => {
    dbMock.getLocalFilePaths.mockReturnValue([
      path.join(os.tmpdir(), 'shelfy-ipc-test-missing', 'nope.bin'),
    ]);

    const res = await invoke('db:deletePosts', { ids: ['p1'] });

    expect(res.ok).toBe(true);
    expect(res.errors).toEqual([]);
    expect(dbMock.deletePosts).toHaveBeenCalledWith(['p1']);
  });

  it('rejects renderer-supplied id arrays above MAX_BULK_ITEMS', async () => {
    const ids = new Array(100001).fill('x');
    await expect(invoke('db:deletePosts', { ids })).rejects.toThrow(/too many ids/);
    expect(dbMock.deletePosts).not.toHaveBeenCalled();
  });
});

// ─── db:exportJSON ─────────────────────────────────────────────────────────────

describe('db:exportJSON', () => {
  it('writes chunked output that round-trips as { posts, collections }', async () => {
    const out = path.join(makeTmpDir(), 'export.json');
    dialogMock.showSaveDialog.mockResolvedValue({ canceled: false, filePath: out });
    // >1 chunk (CHUNK=500) with awkward strings to catch separator/escaping bugs.
    const posts = Array.from({ length: 1203 }, (_, i) => ({
      id: `p${i}`,
      caption: `line"1\nline,2 — ${i}`,
    }));
    const collections = [{ name: 'A', color: '#fff', platform: 'instagram' }];
    dbMock.exportAllPosts.mockReturnValue(posts);
    dbMock.getCollectionsForExport.mockReturnValue(collections);

    const res = await invoke('db:exportJSON', {});

    expect(res).toEqual({ canceled: false, count: 1203, filePath: out });
    const parsed = JSON.parse(fs.readFileSync(out, 'utf8'));
    expect(parsed.posts).toEqual(posts);
    expect(parsed.collections).toEqual(collections);
  });

  it('produces valid JSON for an empty library', async () => {
    const out = path.join(makeTmpDir(), 'empty.json');
    dialogMock.showSaveDialog.mockResolvedValue({ canceled: false, filePath: out });

    const res = await invoke('db:exportJSON', {});

    expect(res).toEqual({ canceled: false, count: 0, filePath: out });
    expect(JSON.parse(fs.readFileSync(out, 'utf8'))).toEqual({ posts: [], collections: [] });
  });
});

// ─── download:all / download:posts queued counts ───────────────────────────────

describe('download queued counts', () => {
  it('download:posts sums the real queued counts returned by enqueueMany', async () => {
    downloaderMock.enqueueMany.mockReturnValue({ queued: 3 });
    const res = await invoke('download:posts', { ids: ['a', 'b', 'c', 'd', 'e'] });
    expect(res.queued).toBe(3);
  });

  it('download:posts falls back to the batch size when enqueueMany returns nothing', async () => {
    downloaderMock.enqueueMany.mockReturnValue(undefined);
    const res = await invoke('download:posts', { ids: ['a', 'b'] });
    expect(res.queued).toBe(2);
  });

  it('download:all accumulates queued across batches', async () => {
    dbMock.getPostIds.mockReturnValue(Array.from({ length: 450 }, (_, i) => `p${i}`));
    downloaderMock.enqueueMany.mockImplementation((posts) => ({
      queued: Math.max(posts.length - 1, 0),
    }));
    const res = await invoke('download:all', {});
    // Batches of 200/200/50, one post skipped per batch.
    expect(res.queued).toBe(199 + 199 + 49);
  });
});
