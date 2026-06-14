import { describe, it, expect, beforeEach, vi, type Mock } from 'vitest';
import os from 'os';
import fs from 'fs';
import path from 'path';

// ─── ESM mocking ────────────────────────────────────────────────────────────────
// ipc.ts is ESM (TypeScript). Its dependencies are mocked via vi.mock(), whose
// factories are hoisted above the imports; vi.hoisted lets each factory and the
// test body share the same mock object. Every dependency is imported in ipc.ts as
// `import * as X from './X'` (namespace import), so each factory returns the
// named-exports object directly.

// An ipcMain.handle handler: (event, payload) => result, registered by ipc.ts.
type IpcHandler = (event: unknown, payload?: unknown) => unknown;

// Mock: electron — ipcMain.handle stores each handler so tests can invoke them
// directly, the way the renderer's ipcRenderer.invoke would.
const electronMock = vi.hoisted(() => {
  const handlers = new Map<string, IpcHandler>();
  return {
    handlers,
    ipcMain: {
      handle: vi.fn((channel: string, fn: IpcHandler) => handlers.set(channel, fn)),
    },
    dialog: {
      showSaveDialog: vi.fn(),
      showOpenDialog: vi.fn(),
      showErrorBox: vi.fn(),
    },
    app: {
      getPath: vi.fn(() => os.tmpdir()),
      getVersion: vi.fn(() => '0.0.0-test'),
    },
    shell: { openExternal: vi.fn() },
  };
});

vi.mock('electron', () => ({
  app: electronMock.app,
  ipcMain: electronMock.ipcMain,
  dialog: electronMock.dialog,
  shell: electronMock.shell,
}));

// Mock: local modules required by ipc.ts. Only the functions touched at
// registration time (the progress-emitter setters) plus the ones exercised by
// the handlers under test need real stubs.
interface DbMock {
  getLocalFilePaths: Mock;
  deletePosts: Mock;
  getPostIds: Mock;
  getPostsByIds: Mock;
  exportAllPosts: Mock;
  getCollectionsForExport: Mock;
}
const dbMock = vi.hoisted<DbMock>(() => ({
  getLocalFilePaths: vi.fn(),
  deletePosts: vi.fn(),
  getPostIds: vi.fn(),
  getPostsByIds: vi.fn(),
  exportAllPosts: vi.fn(),
  getCollectionsForExport: vi.fn(),
}));
vi.mock('../../electron/db', () => dbMock);

interface DownloaderMock {
  setProgressEmitter: Mock;
  enqueueMany: Mock;
}
const downloaderMock = vi.hoisted<DownloaderMock>(() => ({
  setProgressEmitter: vi.fn(),
  enqueueMany: vi.fn(),
}));
vi.mock('../../electron/downloader', () => downloaderMock);

vi.mock('../../electron/analyzer', () => ({ setProgressEmitter: vi.fn() }));
vi.mock('../../electron/weborchestrator', () => ({
  setProgressEmitter: vi.fn(),
  setListRefreshEmitter: vi.fn(),
}));
vi.mock('../../electron/stt', () => ({}));
vi.mock('../../electron/embeddings', () => ({}));
vi.mock('../../electron/updater', () => ({}));
vi.mock('../../electron/binaries', () => ({}));
vi.mock('../../electron/feedback', () => ({}));
vi.mock('../../electron/bookmarks', () => ({}));
vi.mock('../../electron/net-safety', () => ({}));

interface Ipc {
  registerIpcHandlers: (win: {
    isDestroyed: () => boolean;
    webContents: { send: (...args: unknown[]) => void };
  }) => void;
}

const { registerIpcHandlers } = (await import('../../electron/ipc')) as Ipc;
registerIpcHandlers({ isDestroyed: () => false, webContents: { send: vi.fn() } });

const handlers = electronMock.handlers;
const dialogMock = electronMock.dialog;

const invoke = (channel: string, payload?: unknown): unknown => {
  const handler = handlers.get(channel);
  if (!handler) throw new Error(`No handler for channel: ${channel}`);
  return handler(null, payload);
};

function makeTmpDir(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-ipc-test-'));
}

beforeEach(() => {
  vi.clearAllMocks();
  dbMock.getLocalFilePaths.mockImplementation(() => []);
  dbMock.deletePosts.mockImplementation((ids: string[]) => ({ deleted: ids.length }));
  dbMock.getPostIds.mockImplementation(() => []);
  dbMock.getPostsByIds.mockImplementation((ids: string[]) => ids.map((id) => ({ id })));
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
    dbMock.getLocalFilePaths.mockImplementation((id: string) => (id === 'p1' ? [f1, f2] : []));

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

    const res = (await invoke('db:deletePosts', { ids: ['p1'] })) as {
      ok: boolean;
      errors: string[];
    };

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
    const res = (await invoke('download:posts', { ids: ['a', 'b', 'c', 'd', 'e'] })) as {
      queued: number;
    };
    expect(res.queued).toBe(3);
  });

  it('download:posts falls back to the batch size when enqueueMany returns nothing', async () => {
    downloaderMock.enqueueMany.mockReturnValue(undefined);
    const res = (await invoke('download:posts', { ids: ['a', 'b'] })) as { queued: number };
    expect(res.queued).toBe(2);
  });

  it('download:all accumulates queued across batches', async () => {
    dbMock.getPostIds.mockReturnValue(Array.from({ length: 450 }, (_, i) => `p${i}`));
    downloaderMock.enqueueMany.mockImplementation((posts: unknown[]) => ({
      queued: Math.max(posts.length - 1, 0),
    }));
    const res = (await invoke('download:all', {})) as { queued: number };
    // Batches of 200/200/50, one post skipped per batch.
    expect(res.queued).toBe(199 + 199 + 49);
  });
});
