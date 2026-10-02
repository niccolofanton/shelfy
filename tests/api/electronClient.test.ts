import { describe, it, expect, vi } from 'vitest';
import { createElectronClient, desktopCapabilities } from '../../src/api/electronClient';
import type { ElectronAPI, PostSearchResult } from '../../types/electron-api';

const page = (ids: string[], total: number): PostSearchResult =>
  ({ posts: ids.map((id) => ({ id })), total }) as unknown as PostSearchResult;

describe('electronClient — posts over the bridge', () => {
  it('pages by offset: the cursor is the offset of the next page', async () => {
    vi.mocked(window.electronAPI.getPosts)
      .mockResolvedValueOnce(page(['a', 'b'], 5))
      .mockResolvedValueOnce(page(['c', 'd'], 5))
      .mockResolvedValueOnce(page(['e'], 5));
    const client = createElectronClient();

    const first = await client.listPosts({ platform: 'instagram' }, { limit: 2 });
    expect(window.electronAPI.getPosts).toHaveBeenLastCalledWith({
      platform: 'instagram',
      limit: 2,
      offset: 0,
    });
    expect(first).toMatchObject({ total: 5, nextCursor: '2' });
    expect(first.posts.map((p) => p.id)).toEqual(['a', 'b']);

    const second = await client.listPosts({}, { limit: 2, cursor: first.nextCursor });
    expect(window.electronAPI.getPosts).toHaveBeenLastCalledWith({ limit: 2, offset: 2 });
    expect(second.nextCursor).toBe('4');

    const last = await client.listPosts({}, { limit: 2, cursor: second.nextCursor });
    expect(last.nextCursor).toBeNull();
  });

  it('reads a malformed result as an empty last page', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue(
      undefined as unknown as PostSearchResult,
    );
    const result = await createElectronClient().listPosts({}, { limit: 50 });
    expect(result).toEqual({ posts: [], total: 0, nextCursor: null });
  });

  it('fetches posts by id, stats and folders as the views did', async () => {
    vi.mocked(window.electronAPI.getPostsByIds).mockResolvedValue([
      { id: 'p1' } as unknown as Shelfy.Post,
    ]);
    const client = createElectronClient();
    expect((await client.getPostsByIds(['p1'])).map((p) => p.id)).toEqual(['p1']);
    expect(window.electronAPI.getPostsByIds).toHaveBeenCalledWith(['p1']);
    await client.getStats();
    expect(window.electronAPI.getStats).toHaveBeenCalled();
    await client.listCollections();
    expect(window.electronAPI.getCollections).toHaveBeenCalled();
  });
});

describe('electronClient — events', () => {
  it('turns finished download and analyze jobs into post events, not progress ticks', () => {
    const client = createElectronClient();
    const stored = vi.fn();
    const analyzed = vi.fn();
    const offStored = client.on('post.stored', stored);
    client.on('post.analyzed', analyzed);
    const downloadCb = vi.mocked(window.electronAPI.onDownloadProgress).mock.calls[0][0];
    const analyzeCb = vi.mocked(window.electronAPI.onAnalyzeProgress).mock.calls[0][0];

    downloadCb({ status: 'downloading', postId: 'p1' });
    downloadCb({ status: 'done', postId: 'p1' });
    downloadCb({ status: 'done' });
    analyzeCb({ status: 'analyzing', postId: 'p2' });
    analyzeCb({ status: 'done', postId: 'p2' });

    expect(stored.mock.calls).toEqual([
      [{ type: 'post.stored', postId: 'p1' }],
      [{ type: 'post.stored', postId: null }],
    ]);
    expect(analyzed.mock.calls).toEqual([[{ type: 'post.analyzed', postId: 'p2' }]]);
    expect(typeof offStored).toBe('function');
  });

  it('describes a batch of new posts only when the payload does', () => {
    const unsubscribe = vi.fn();
    vi.mocked(window.electronAPI.onNewPosts).mockReturnValue(unsubscribe);
    const listener = vi.fn();
    const off = createElectronClient().on('posts.changed', listener);
    const cb = vi.mocked(window.electronAPI.onNewPosts).mock.calls[0][0];

    cb({ count: 3, platform: 'twitter' });
    cb({ reason: 'thumb-blur' });
    cb(undefined);

    expect(listener.mock.calls).toEqual([
      [{ type: 'posts.changed', count: 3, platform: 'twitter' }],
      [{ type: 'posts.changed', count: undefined, platform: undefined }],
      [{ type: 'posts.changed', count: undefined, platform: undefined }],
    ]);
    off();
    expect(unsubscribe).toHaveBeenCalled();
  });

  it('reports moved counters on every new-posts push, as the app assumed before', () => {
    const unsubscribe = vi.fn();
    vi.mocked(window.electronAPI.onNewPosts).mockReturnValue(unsubscribe);
    const listener = vi.fn();
    const off = createElectronClient().on('stats.changed', listener);
    const cb = vi.mocked(window.electronAPI.onNewPosts).mock.calls[0][0];

    cb({ count: 2, platform: 'instagram' });
    cb({ reason: 'thumb-blur' });

    expect(listener.mock.calls).toEqual([[{ type: 'stats.changed' }], [{ type: 'stats.changed' }]]);
    off();
    expect(unsubscribe).toHaveBeenCalled();
  });

  it('never resyncs: the IPC loses no event', () => {
    const client = createElectronClient();
    const off = client.on('resync', vi.fn());
    expect(window.electronAPI.onNewPosts).not.toHaveBeenCalled();
    expect(window.electronAPI.onDownloadProgress).not.toHaveBeenCalled();
    expect(window.electronAPI.onAnalyzeProgress).not.toHaveBeenCalled();
    expect(() => off()).not.toThrow();
  });

  it('logs the reports of the error boundaries', () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});
    const crash = new TypeError('boom');
    createElectronClient().reportError({ view: 'gallery', error: crash, componentStack: 'at X' });
    expect(error).toHaveBeenCalledWith('[ErrorBoundary] gallery:', crash, 'at X');
    error.mockRestore();
  });
});

describe('electronClient — capabilities and media', () => {
  it('has every capability; the window chrome follows the OS', () => {
    expect(desktopCapabilities('darwin')).toMatchObject({
      windowControls: false,
      trafficLights: true,
      localFiles: true,
      libraryEdit: true,
      bulkActions: true,
    });
    expect(desktopCapabilities('win32')).toMatchObject({
      windowControls: true,
      trafficLights: false,
    });
    const bridge = { platform: 'darwin' } as unknown as ElectronAPI;
    expect(createElectronClient(() => bridge).capabilities.trafficLights).toBe(true);
  });

  it('serves local files through the asset protocol', () => {
    const { media } = createElectronClient();
    expect(media.file('/lib/a.jpg')).toBe('asset://media/%2Flib%2Fa.jpg');
    expect(media.tile('/lib/a.jpg', 640)).toBe('asset://media/%2Flib%2Fa.jpg?w=640');
    expect(media.file(null)).toBeNull();
    expect(media.isStored('asset://media/x')).toBe(true);
    expect(media.isStored('https://cdn.example.test/a.jpg')).toBe(false);
  });

  it('opens links through the bridge', () => {
    const openExternal = vi.fn().mockResolvedValue(undefined);
    const bridge = { openExternal } as unknown as ElectronAPI;
    createElectronClient(() => bridge).openExternal('https://example.test/p/1');
    expect(openExternal).toHaveBeenCalledWith('https://example.test/p/1');
  });
});
