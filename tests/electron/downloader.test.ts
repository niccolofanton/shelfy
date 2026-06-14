import { describe, it, expect, beforeEach, vi, type Mock } from 'vitest';

// ─── ESM module mocking ─────────────────────────────────────────────────────────
// downloader.ts is now an ESM module, so its dependencies are intercepted with
// vi.mock (hoisted above the imports). vi.hoisted lets each mock factory and the
// test body share the same mock instance, so assertions can inspect the calls.
//
// How downloader.ts imports each dependency shapes its factory:
//   import { app, session } from 'electron'        -> named exports
//   import fs from 'fs'                             -> default export (+ named for promises)
//   import { spawn } from 'child_process'           -> named exports
//   import * as db from './db'                      -> named exports object
//   import * as jobstore from './jobstore'          -> named exports object
//   import { microThumbDataUri } from './thumbs'    -> named export
//   import { PARTITION, SOCIAL_UA } from './interceptor' -> named exports
// net-safety is intentionally NOT mocked: assertSafeMediaUrl is real behavior the
// tests rely on (the public CDN hosts they use pass the SSRF guard).

// Mock: electron
const appGetPath = vi.hoisted(() => vi.fn(() => '/tmp/vitest-downloader-test'));
vi.mock('electron', () => {
  const app = { getPath: appGetPath };
  const session = {
    fromPartition: vi.fn(() => ({ cookies: { get: vi.fn(async () => []) } })),
  };
  return { app, session, default: { app, session } };
});

// Mock: fs (default import)
// downloadUrl streams res.body to a `<dest>.part` via createWriteStream and
// renames it into place on success, so the mock provides a writable-stream stub
// plus renameSync/unlinkSync alongside the legacy writeFileSync.
function makeWritableStub(): {
  write: Mock;
  end: Mock;
  once: Mock;
  destroy: Mock;
} {
  return {
    write: vi.fn(() => true), // never trigger backpressure (no 'drain' wait)
    end: vi.fn((cb?: () => void) => {
      if (typeof cb === 'function') cb();
    }),
    once: vi.fn(),
    destroy: vi.fn(),
  };
}
const fsMock = vi.hoisted(() => {
  // makeWritableStub is referenced by the createWriteStream default impl; inline
  // it here so the hoisted factory stays self-contained.
  const writableStub = (): {
    write: Mock;
    end: Mock;
    once: Mock;
    destroy: Mock;
  } => ({
    write: vi.fn(() => true),
    end: vi.fn((cb?: () => void) => {
      if (typeof cb === 'function') cb();
    }),
    once: vi.fn(),
    destroy: vi.fn(),
  });
  return {
    existsSync: vi.fn(() => false),
    mkdirSync: vi.fn(),
    writeFileSync: vi.fn(),
    readFileSync: vi.fn(),
    readdirSync: vi.fn(() => [] as string[]),
    createWriteStream: vi.fn(() => writableStub()),
    renameSync: vi.fn(),
    unlinkSync: vi.fn(),
    promises: { rm: vi.fn(async () => {}) },
  };
});
vi.mock('fs', () => ({ ...fsMock, default: fsMock }));

// Mock: child_process — prevent real yt-dlp invocation at module load
const spawnSyncMock = vi.hoisted(() => vi.fn(() => ({ status: 0 })));
const spawnMock = vi.hoisted(() =>
  vi.fn(() => ({
    stdout: { setEncoding: vi.fn(), on: vi.fn() },
    stderr: { setEncoding: vi.fn(), on: vi.fn() },
    on: vi.fn(),
  })),
);
vi.mock('child_process', () => ({
  spawnSync: spawnSyncMock,
  spawn: spawnMock,
  default: { spawnSync: spawnSyncMock, spawn: spawnMock },
}));

// Mock: ./db — downloader imports it as `import * as db from './db'`, so the
// factory returns the named exports object directly.
const dbMock = vi.hoisted(() => ({
  updatePaths: vi.fn(),
  updateMediaPath: vi.fn(),
  getPost: vi.fn(),
}));
vi.mock('../../electron/db', () => dbMock);

// Mock: ./jobstore — the real jobstore pulls in ./db (native better-sqlite3) and
// only serves as the durable mirror here; the tests assert nothing on it, so a
// no-op stub keeps the queue's persistence calls harmless.
const jobstoreMock = vi.hoisted(() => ({
  mirror: vi.fn(),
  mirrorMany: vi.fn(),
  forget: vi.fn(),
  forgetMany: vi.fn(),
  forgetAll: vi.fn(),
  forgetExcept: vi.fn(),
  resumable: vi.fn(() => [] as unknown[]),
  TERMINAL: new Set<string>(['done', 'cancelled', 'error']),
}));
vi.mock('../../electron/jobstore', () => jobstoreMock);

// Mock: ./thumbs — the blur-up generator needs electron.nativeImage, which the
// electron mock above doesn't provide. Resolves null = "no placeholder".
const microThumbMock = vi.hoisted(() => vi.fn(async (): Promise<string | null> => null));
vi.mock('../../electron/thumbs', () => ({ microThumbDataUri: microThumbMock }));

// Mock: ./interceptor — only its two constants are needed; mocking it avoids
// loading the real module (which wires up electron.session). SOCIAL_UA must stay
// a real UA string so the browser-shaped-headers assertions still hold.
vi.mock('../../electron/interceptor', () => ({
  PARTITION: 'persist:social',
  SOCIAL_UA:
    'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36',
}));

// ─── Domain shapes used by the tests ────────────────────────────────────────────

interface DownloadPostInput {
  id: string;
  shortcode?: string;
  thumbnailUrl?: string;
  mediaType?: string;
  media?: Array<{ type: string; url: string }>;
}

interface DownloadJobRecord {
  postId: string;
  assetType: string;
  status: string;
  error?: string;
}

interface Downloader {
  downloadPost: (
    post: DownloadPostInput,
    assetTypes: string[],
    onProgress: (ev: DownloadJobRecord) => void,
  ) => Promise<unknown>;
  getStatus: () => DownloadJobRecord[];
  cancel: (key: string) => void;
  cancelAll: () => void;
  clearCompleted: () => void;
  setProgressEmitter: (emitter: ((ev: DownloadJobRecord) => void) | null) => void;
  enqueueMany: (
    posts: DownloadPostInput[],
    assetTypes: string[],
  ) => { queued: number; skipped: number };
}

// Load downloader as an ESM module; the vi.mock factories above intercept its
// imports.
const downloader = (await import('../../electron/downloader')) as unknown as Downloader;
const { downloadPost, getStatus, cancel, cancelAll, clearCompleted, setProgressEmitter } =
  downloader;

const tick = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

// The downloader keeps a process-wide queue + job history, so tests that assert
// on the exact set of fetches must start from a clean slate. cancelAll marks
// jobs cancelled synchronously but frees concurrency slots only once the aborted
// workers reach their finally block, so wait a few ticks for that to settle.
async function resetQueue(): Promise<void> {
  cancelAll();
  clearCompleted();
  for (let i = 0; i < 5; i++) await tick();
}

// Let the queue drain so every enqueued fetch has actually been issued.
async function waitForIdle(): Promise<void> {
  for (let i = 0; i < 50; i++) {
    await tick();
    if (!getStatus().some((j) => j.status === 'pending' || j.status === 'downloading')) return;
  }
}

// ─── Shared helpers ────────────────────────────────────────────────────────────

interface FetchResponseStub {
  ok: boolean;
  status: number;
  body: Uint8Array[];
  arrayBuffer: Mock;
}

function makeFetchResponse(bytes = new Uint8Array(8)): FetchResponseStub {
  return {
    ok: true,
    status: 200,
    // downloadUrl consumes res.body as an (async-)iterable of chunks.
    body: [bytes],
    arrayBuffer: vi.fn().mockResolvedValue(bytes.buffer ?? bytes),
  };
}

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
  reject: (reason?: unknown) => void;
}

function deferred<T = void>(): Deferred<T> {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

// vi.fn-typed view of the global fetch mock for call inspection.
const fetchMock = (): Mock => global.fetch as unknown as Mock;

// ─── Setup ────────────────────────────────────────────────────────────────────

beforeEach(async () => {
  fsMock.existsSync.mockReturnValue(false);
  fsMock.mkdirSync.mockReset();
  fsMock.writeFileSync.mockReset();
  fsMock.renameSync.mockReset();
  fsMock.unlinkSync.mockReset();
  fsMock.createWriteStream.mockReset();
  fsMock.createWriteStream.mockImplementation(() => makeWritableStub());
  global.fetch = vi.fn().mockResolvedValue(makeFetchResponse()) as unknown as typeof fetch;
  dbMock.updatePaths.mockReset();
  dbMock.updateMediaPath.mockReset();
  dbMock.getPost.mockReset();
  // The downloader keeps a process-wide queue, job history and a single global
  // progress emitter. Clear all of it so each test starts from a clean slate
  // and re-registering an emitter below actually takes effect.
  await resetQueue();
  setProgressEmitter(null);
});

// ─── 1. getStatus() ───────────────────────────────────────────────────────────

describe('getStatus()', () => {
  it('returns an empty array when no downloads are active', () => {
    expect(getStatus()).toEqual([]);
  });

  it('returns an array (not throws) while a download is in progress', async () => {
    const { promise: fetchPromise, resolve: resolveFetch } = deferred();
    global.fetch = vi
      .fn()
      .mockReturnValue(fetchPromise.then(() => makeFetchResponse())) as unknown as typeof fetch;

    const post: DownloadPostInput = {
      id: 'status-post',
      thumbnailUrl: 'https://cdn.example.com/thumb.jpg',
      mediaType: 'image',
    };

    const downloadPromise = downloadPost(post, ['thumbnail'], vi.fn());
    await Promise.resolve();

    expect(Array.isArray(getStatus())).toBe(true);

    resolveFetch();
    await downloadPromise;
    await waitForIdle();

    // The queue keeps finished jobs as history (cleared only via clearCompleted),
    // so once the download settles the job remains with status "done".
    const job = getStatus().find((j) => j.postId === 'status-post');
    expect(job).toBeDefined();
    expect(job?.status).toBe('done');
  });
});

// ─── 2. cancel(postId) ────────────────────────────────────────────────────────

describe('cancel()', () => {
  it('does not throw for a non-existent postId', () => {
    expect(() => cancel('non-existent-id')).not.toThrow();
  });

  it('removes the job from getStatus() after cancel', async () => {
    const { promise: fetchPromise, resolve: resolveFetch } = deferred();
    global.fetch = vi
      .fn()
      .mockReturnValue(fetchPromise.then(() => makeFetchResponse())) as unknown as typeof fetch;

    const post: DownloadPostInput = {
      id: 'cancel-post',
      thumbnailUrl: 'https://cdn.example.com/thumb.jpg',
      mediaType: 'image',
    };

    const downloadPromise = downloadPost(post, ['thumbnail'], vi.fn());
    await Promise.resolve();

    // cancel() now takes the job key (`${postId}:${assetType}`), not a bare
    // postId. Cancelling keeps the job in the history list but flips it to the
    // terminal "cancelled" state rather than removing it outright.
    cancel('cancel-post:thumbnail');

    const remaining = getStatus().filter((j) => j.postId === 'cancel-post');
    expect(remaining).toHaveLength(1);
    expect(remaining[0].status).toBe('cancelled');

    resolveFetch();
    await downloadPromise;
    await waitForIdle();
  });
});

// ─── 3. downloadPost — thumbnail download ────────────────────────────────────

describe('downloadPost — thumbnail download', () => {
  it('calls onProgress with "downloading" then "done" and calls db.updatePaths', async () => {
    const post: DownloadPostInput = {
      id: 'thumb-post-1',
      shortcode: 'ABC123',
      thumbnailUrl: 'https://cdn.instagram.com/thumb.jpg',
      mediaType: 'image',
    };
    const onProgress = vi.fn();

    await downloadPost(post, ['thumbnail'], onProgress);
    await waitForIdle();

    expect(onProgress).toHaveBeenCalledWith(
      expect.objectContaining({
        postId: 'thumb-post-1',
        assetType: 'thumbnail',
        status: 'downloading',
      }),
    );
    expect(onProgress).toHaveBeenCalledWith(
      expect.objectContaining({ postId: 'thumb-post-1', assetType: 'thumbnail', status: 'done' }),
    );
    expect(dbMock.updatePaths).toHaveBeenCalledWith(
      'thumb-post-1',
      expect.objectContaining({ thumbnailPath: expect.any(String) }),
    );
  });

  it('generates the blur-up placeholder from the fresh cover and persists it with the paths', async () => {
    microThumbMock.mockResolvedValueOnce('data:image/jpeg;base64,AAAA');
    const post: DownloadPostInput = {
      id: 'thumb-post-blur',
      shortcode: 'BLUR1',
      thumbnailUrl: 'https://cdn.instagram.com/thumb.jpg',
      mediaType: 'image',
    };

    await downloadPost(post, ['thumbnail'], vi.fn());
    await waitForIdle();

    expect(microThumbMock).toHaveBeenCalledWith(expect.stringContaining('BLUR1'));
    expect(dbMock.updatePaths).toHaveBeenCalledWith(
      'thumb-post-blur',
      expect.objectContaining({
        thumbnailPath: expect.any(String),
        thumbBlur: 'data:image/jpeg;base64,AAAA',
      }),
    );
  });

  it('fetches using the thumbnailUrl', async () => {
    const post: DownloadPostInput = {
      id: 'thumb-post-2',
      thumbnailUrl: 'https://cdn.instagram.com/photo.jpg',
      mediaType: 'image',
    };

    await downloadPost(post, ['thumbnail'], vi.fn());

    expect(global.fetch).toHaveBeenCalledWith(
      'https://cdn.instagram.com/photo.jpg',
      expect.any(Object),
    );
  });

  it('sends browser-shaped headers (UA + Sec-Fetch) to look like a real image request', async () => {
    const post: DownloadPostInput = {
      id: 'hdr-post-1',
      thumbnailUrl: 'https://cdn.instagram.com/photo.jpg',
      mediaType: 'image',
    };

    await downloadPost(post, ['thumbnail'], vi.fn());

    expect(global.fetch).toHaveBeenCalledWith(
      'https://cdn.instagram.com/photo.jpg',
      expect.objectContaining({
        headers: expect.objectContaining({
          'User-Agent': expect.any(String),
          Accept: expect.stringContaining('image/'),
          'Sec-Fetch-Dest': 'image',
        }),
      }),
    );
  });

  it('streams bytes to a .part file and renames it into place', async () => {
    const post: DownloadPostInput = {
      id: 'thumb-post-3',
      thumbnailUrl: 'https://cdn.instagram.com/photo.jpg',
      mediaType: 'image',
    };

    await downloadPost(post, ['thumbnail'], vi.fn());
    await waitForIdle();

    // The downloader streams to "<dest>.part" then atomically renames to <dest>.
    expect(fsMock.createWriteStream).toHaveBeenCalledWith(
      expect.stringMatching(/\.part$/),
      expect.any(Object),
    );
    expect(fsMock.renameSync).toHaveBeenCalledWith(
      expect.stringMatching(/\.part$/),
      expect.any(String),
    );
  });
});

// ─── 4. downloadPost — skip existing file ────────────────────────────────────

describe('downloadPost — skip existing thumbnail', () => {
  it('skips download when file already exists on disk', async () => {
    fsMock.existsSync.mockReturnValue(true);

    const post: DownloadPostInput = {
      id: 'skip-thumb',
      shortcode: 'SC999',
      thumbnailUrl: 'https://cdn.instagram.com/existing.jpg',
      mediaType: 'image',
    };
    const onProgress = vi.fn();

    await downloadPost(post, ['thumbnail'], onProgress);
    await waitForIdle();

    // An already-on-disk thumbnail short-circuits the network fetch and the job
    // lands directly in the "done" state (the current downloader no longer tags
    // such jobs with a separate `skipped` flag).
    expect(global.fetch).not.toHaveBeenCalled();
    expect(onProgress).toHaveBeenCalledWith(
      expect.objectContaining({ postId: 'skip-thumb', assetType: 'thumbnail', status: 'done' }),
    );
  });
});

// ─── 5. downloadPost — image skipped for wrong mediaType ─────────────────────

describe('downloadPost — image fetched per media type', () => {
  it('does not fetch image when post.mediaType is "video"', async () => {
    const post: DownloadPostInput = {
      id: 'video-post',
      thumbnailUrl: 'https://pbs.twimg.com/media/vid.jpg',
      mediaType: 'video',
    };
    await downloadPost(post, ['image'], vi.fn());
    expect(global.fetch).not.toHaveBeenCalled();
  });

  it('fetches the cover when a carousel has no media array', async () => {
    const post: DownloadPostInput = {
      id: 'carousel-post',
      thumbnailUrl: 'https://cdn.instagram.com/c.jpg',
      mediaType: 'carousel',
    };
    await downloadPost(post, ['image'], vi.fn());
    expect(global.fetch).toHaveBeenCalled();
  });

  it('fetches image when post.mediaType is "image"', async () => {
    const post: DownloadPostInput = {
      id: 'image-post',
      thumbnailUrl: 'https://cdn.instagram.com/photo.jpg',
      mediaType: 'image',
    };
    await downloadPost(post, ['image'], vi.fn());
    expect(global.fetch).toHaveBeenCalled();
  });

  it('fetches image when post.mediaType is "images"', async () => {
    const post: DownloadPostInput = {
      id: 'images-post',
      thumbnailUrl: 'https://cdn.instagram.com/c.jpg',
      mediaType: 'images',
    };
    await downloadPost(post, ['image'], vi.fn());
    expect(global.fetch).toHaveBeenCalled();
  });

  it('fetches every slide of a carousel and names files by position', async () => {
    await resetQueue();
    fetchMock().mockClear();
    const post: DownloadPostInput = {
      id: 'multi-carousel',
      shortcode: 'MC1',
      thumbnailUrl: 'https://cdn.instagram.com/0.jpg',
      mediaType: 'carousel',
      media: [
        { type: 'image', url: 'https://cdn.instagram.com/0.jpg' },
        { type: 'image', url: 'https://cdn.instagram.com/1.jpg' },
        { type: 'image', url: 'https://cdn.instagram.com/2.jpg' },
      ],
    };
    await downloadPost(post, ['image'], vi.fn());
    await waitForIdle();

    const fetched = fetchMock().mock.calls.map((c) => c[0]);
    expect(fetched).toEqual(
      expect.arrayContaining([
        'https://cdn.instagram.com/0.jpg',
        'https://cdn.instagram.com/1.jpg',
        'https://cdn.instagram.com/2.jpg',
      ]),
    );
  });

  it('downloads only image slides of a carousel, skipping video slides', async () => {
    await resetQueue();
    fetchMock().mockClear();
    const post: DownloadPostInput = {
      id: 'mixed-carousel',
      shortcode: 'MX1',
      thumbnailUrl: 'https://cdn.instagram.com/a.jpg',
      mediaType: 'carousel',
      media: [
        { type: 'image', url: 'https://cdn.instagram.com/a.jpg' },
        { type: 'video', url: 'https://cdn.instagram.com/b.jpg' },
        { type: 'image', url: 'https://cdn.instagram.com/c.jpg' },
      ],
    };
    await downloadPost(post, ['image'], vi.fn());
    await waitForIdle();

    const fetched = fetchMock().mock.calls.map((c) => c[0]);
    expect(fetched).toContain('https://cdn.instagram.com/a.jpg');
    expect(fetched).toContain('https://cdn.instagram.com/c.jpg');
    expect(fetched).not.toContain('https://cdn.instagram.com/b.jpg');
  });
});

// ─── 6. downloadPost — error handling ─────────────────────────────────────────

describe('downloadPost — error handling', () => {
  it('calls onProgress with status "error" when fetch throws', async () => {
    global.fetch = vi
      .fn()
      .mockRejectedValue(new Error('Network failure')) as unknown as typeof fetch;

    const post: DownloadPostInput = {
      id: 'error-post',
      thumbnailUrl: 'https://cdn.instagram.com/photo.jpg',
      mediaType: 'image',
    };
    const onProgress = vi.fn();

    await expect(downloadPost(post, ['thumbnail'], onProgress)).resolves.toBeDefined();
    await waitForIdle();

    expect(onProgress).toHaveBeenCalledWith(
      expect.objectContaining({
        postId: 'error-post',
        assetType: 'thumbnail',
        status: 'error',
        error: expect.stringContaining('Network failure'),
      }),
    );
  });

  it('does not throw when HTTP response is not ok', async () => {
    global.fetch = vi
      .fn()
      .mockResolvedValue({
        ok: false,
        status: 403,
        arrayBuffer: vi.fn(),
      }) as unknown as typeof fetch;

    const post: DownloadPostInput = {
      id: 'error-post-2',
      thumbnailUrl: 'https://cdn.instagram.com/photo.jpg',
      mediaType: 'image',
    };
    const onProgress = vi.fn();

    await expect(downloadPost(post, ['thumbnail'], onProgress)).resolves.toBeDefined();
    await waitForIdle();

    expect(onProgress).toHaveBeenCalledWith(expect.objectContaining({ status: 'error' }));
  });

  it('does not call db.updatePaths when nothing downloaded', async () => {
    global.fetch = vi
      .fn()
      .mockRejectedValue(new Error('Connection refused')) as unknown as typeof fetch;

    const post: DownloadPostInput = {
      id: 'error-no-db',
      thumbnailUrl: 'https://cdn.instagram.com/photo.jpg',
      mediaType: 'image',
    };
    await downloadPost(post, ['thumbnail'], vi.fn());

    expect(dbMock.updatePaths).not.toHaveBeenCalled();
  });
});

// ─── 7. enqueueMany — real counts ─────────────────────────────────────────────

describe('enqueueMany()', () => {
  it('returns the real queued/skipped counts', async () => {
    const posts: DownloadPostInput[] = [
      {
        id: 'em-queued',
        thumbnailUrl: 'https://cdn.example.com/a.jpg',
        mediaType: 'image',
      },
      // No thumbnailUrl and no media → nothing downloadable for this post.
      { id: 'em-skipped', mediaType: 'image' },
    ];

    const result = downloader.enqueueMany(posts, ['thumbnail']);
    expect(result).toEqual({ queued: 1, skipped: 1 });

    await waitForIdle();
  });
});

// ─── 8. Twitter ?name=orig URL rewriting ──────────────────────────────────────

describe('twitterOriginalUrl — via downloadImage', () => {
  it('replaces query string with ?name=orig for Twitter image downloads', async () => {
    const post: DownloadPostInput = {
      id: 'tw-orig-post',
      thumbnailUrl: 'https://pbs.twimg.com/media/test.jpg?name=small',
      mediaType: 'image',
    };
    await downloadPost(post, ['image'], vi.fn());
    expect(global.fetch).toHaveBeenCalledWith(
      'https://pbs.twimg.com/media/test.jpg?name=orig',
      expect.any(Object),
    );
  });

  it('does NOT rewrite URL for Instagram image downloads', async () => {
    const post: DownloadPostInput = {
      id: 'ig-no-orig',
      shortcode: 'igABC',
      thumbnailUrl: 'https://cdn.instagram.com/photo.jpg?se=1234',
      mediaType: 'image',
    };
    await downloadPost(post, ['image'], vi.fn());
    expect(global.fetch).toHaveBeenCalledWith(
      'https://cdn.instagram.com/photo.jpg?se=1234',
      expect.any(Object),
    );
  });

  it('appends ?name=orig when Twitter URL has no query string', async () => {
    const post: DownloadPostInput = {
      id: 'tw-no-qs',
      thumbnailUrl: 'https://pbs.twimg.com/media/clean.jpg',
      mediaType: 'image',
    };
    await downloadPost(post, ['image'], vi.fn());
    expect(global.fetch).toHaveBeenCalledWith(
      'https://pbs.twimg.com/media/clean.jpg?name=orig',
      expect.any(Object),
    );
  });
});
