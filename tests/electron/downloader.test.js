import { describe, it, expect, beforeEach, vi } from 'vitest';
import { createRequire } from 'module';

// ─── Require cache patching ────────────────────────────────────────────────────
// vi.mock() hoisting only intercepts ESM imports; downloader.js is CJS
// (electron/package.json: {"type":"commonjs"}) so we patch the require cache
// directly before loading the module via createRequire.

const req = createRequire(import.meta.url);

// Mock: electron
const appGetPath = vi.fn(() => '/tmp/vitest-downloader-test');
req.cache[req.resolve('electron')] = {
  id: req.resolve('electron'),
  filename: req.resolve('electron'),
  loaded: true,
  exports: { app: { getPath: appGetPath } },
  children: [],
  paths: [],
};

// Mock: fs
// downloadUrl now streams res.body to a `<dest>.part` via createWriteStream and
// renames it into place on success, so the mock provides a writable-stream stub
// plus renameSync/unlinkSync alongside the legacy writeFileSync.
function makeWritableStub() {
  return {
    write: vi.fn(() => true), // never trigger backpressure (no 'drain' wait)
    end: vi.fn((cb) => {
      if (typeof cb === 'function') cb();
    }),
    once: vi.fn(),
    destroy: vi.fn(),
  };
}
const fsMock = {
  existsSync: vi.fn(() => false),
  mkdirSync: vi.fn(),
  writeFileSync: vi.fn(),
  readFileSync: vi.fn(),
  createWriteStream: vi.fn(() => makeWritableStub()),
  renameSync: vi.fn(),
  unlinkSync: vi.fn(),
};
req.cache[req.resolve('fs')] = {
  id: req.resolve('fs'),
  filename: req.resolve('fs'),
  loaded: true,
  exports: fsMock,
  children: [],
  paths: [],
};

// Mock: child_process — prevent real yt-dlp invocation at module load
const spawnSyncMock = vi.fn(() => ({ status: 0 }));
const spawnMock = vi.fn(() => ({
  stdout: { setEncoding: vi.fn(), on: vi.fn() },
  stderr: { setEncoding: vi.fn(), on: vi.fn() },
  on: vi.fn(),
}));
req.cache[req.resolve('child_process')] = {
  id: req.resolve('child_process'),
  filename: req.resolve('child_process'),
  loaded: true,
  exports: { spawnSync: spawnSyncMock, spawn: spawnMock },
  children: [],
  paths: [],
};

// Mock: ./db
const dbMock = { updatePaths: vi.fn(), updateMediaPath: vi.fn(), getPost: vi.fn() };
req.cache[req.resolve('../../electron/db.js')] = {
  id: req.resolve('../../electron/db.js'),
  filename: req.resolve('../../electron/db.js'),
  loaded: true,
  exports: dbMock,
  children: [],
  paths: [],
};

// Mock: ./thumbs — the blur-up generator needs electron.nativeImage, which the
// electron mock above doesn't provide. Resolves null = "no placeholder".
const microThumbMock = vi.fn(async () => null);
req.cache[req.resolve('../../electron/thumbs.js')] = {
  id: req.resolve('../../electron/thumbs.js'),
  filename: req.resolve('../../electron/thumbs.js'),
  loaded: true,
  exports: { microThumbDataUri: microThumbMock },
  children: [],
  paths: [],
};

// Load downloader via CJS require (bypasses Vite transform, respects cache patches above)
const downloader = req('../../electron/downloader.js');
const { downloadPost, getStatus, cancel, cancelAll, clearCompleted, setProgressEmitter } =
  downloader;

const tick = () => new Promise((r) => setTimeout(r, 0));

// The downloader keeps a process-wide queue + job history, so tests that assert
// on the exact set of fetches must start from a clean slate. cancelAll marks
// jobs cancelled synchronously but frees concurrency slots only once the aborted
// workers reach their finally block, so wait a few ticks for that to settle.
async function resetQueue() {
  cancelAll();
  clearCompleted();
  for (let i = 0; i < 5; i++) await tick();
}

// Let the queue drain so every enqueued fetch has actually been issued.
async function waitForIdle() {
  for (let i = 0; i < 50; i++) {
    await tick();
    if (!getStatus().some((j) => j.status === 'pending' || j.status === 'downloading')) return;
  }
}

// ─── Shared helpers ────────────────────────────────────────────────────────────

function makeFetchResponse(bytes = new Uint8Array(8)) {
  return {
    ok: true,
    status: 200,
    // downloadUrl consumes res.body as an (async-)iterable of chunks.
    body: [bytes],
    arrayBuffer: vi.fn().mockResolvedValue(bytes.buffer ?? bytes),
  };
}

function deferred() {
  let resolve, reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

// ─── Setup ────────────────────────────────────────────────────────────────────

beforeEach(async () => {
  fsMock.existsSync.mockReturnValue(false);
  fsMock.mkdirSync.mockReset();
  fsMock.writeFileSync.mockReset();
  fsMock.renameSync.mockReset();
  fsMock.unlinkSync.mockReset();
  fsMock.createWriteStream.mockReset();
  fsMock.createWriteStream.mockImplementation(() => makeWritableStub());
  global.fetch = vi.fn().mockResolvedValue(makeFetchResponse());
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
    global.fetch = vi.fn().mockReturnValue(fetchPromise.then(() => makeFetchResponse()));

    const post = {
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
    expect(job.status).toBe('done');
  });
});

// ─── 2. cancel(postId) ────────────────────────────────────────────────────────

describe('cancel()', () => {
  it('does not throw for a non-existent postId', () => {
    expect(() => cancel('non-existent-id')).not.toThrow();
  });

  it('removes the job from getStatus() after cancel', async () => {
    const { promise: fetchPromise, resolve: resolveFetch } = deferred();
    global.fetch = vi.fn().mockReturnValue(fetchPromise.then(() => makeFetchResponse()));

    const post = {
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
    const post = {
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
    const post = {
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
    const post = {
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
    const post = {
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
    const post = {
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

    const post = {
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
    const post = {
      id: 'video-post',
      thumbnailUrl: 'https://pbs.twimg.com/media/vid.jpg',
      mediaType: 'video',
    };
    await downloadPost(post, ['image'], vi.fn());
    expect(global.fetch).not.toHaveBeenCalled();
  });

  it('fetches the cover when a carousel has no media array', async () => {
    const post = {
      id: 'carousel-post',
      thumbnailUrl: 'https://cdn.instagram.com/c.jpg',
      mediaType: 'carousel',
    };
    await downloadPost(post, ['image'], vi.fn());
    expect(global.fetch).toHaveBeenCalled();
  });

  it('fetches image when post.mediaType is "image"', async () => {
    const post = {
      id: 'image-post',
      thumbnailUrl: 'https://cdn.instagram.com/photo.jpg',
      mediaType: 'image',
    };
    await downloadPost(post, ['image'], vi.fn());
    expect(global.fetch).toHaveBeenCalled();
  });

  it('fetches image when post.mediaType is "images"', async () => {
    const post = {
      id: 'images-post',
      thumbnailUrl: 'https://cdn.instagram.com/c.jpg',
      mediaType: 'images',
    };
    await downloadPost(post, ['image'], vi.fn());
    expect(global.fetch).toHaveBeenCalled();
  });

  it('fetches every slide of a carousel and names files by position', async () => {
    await resetQueue();
    global.fetch.mockClear();
    const post = {
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

    const fetched = global.fetch.mock.calls.map((c) => c[0]);
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
    global.fetch.mockClear();
    const post = {
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

    const fetched = global.fetch.mock.calls.map((c) => c[0]);
    expect(fetched).toContain('https://cdn.instagram.com/a.jpg');
    expect(fetched).toContain('https://cdn.instagram.com/c.jpg');
    expect(fetched).not.toContain('https://cdn.instagram.com/b.jpg');
  });
});

// ─── 6. downloadPost — error handling ─────────────────────────────────────────

describe('downloadPost — error handling', () => {
  it('calls onProgress with status "error" when fetch throws', async () => {
    global.fetch = vi.fn().mockRejectedValue(new Error('Network failure'));

    const post = {
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
    global.fetch = vi.fn().mockResolvedValue({ ok: false, status: 403, arrayBuffer: vi.fn() });

    const post = {
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
    global.fetch = vi.fn().mockRejectedValue(new Error('Connection refused'));

    const post = {
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
    const posts = [
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
    const post = {
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
    const post = {
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
    const post = {
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
