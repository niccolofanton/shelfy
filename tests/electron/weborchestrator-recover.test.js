import { describe, it, expect, vi } from 'vitest';
import { createRequire } from 'module';

// ─── Require cache patching ────────────────────────────────────────────────────
// weborchestrator.js is CJS, so vi.mock hoisting won't intercept its synchronous
// requires — patch the require cache directly (same approach as downloader.test.js).

const req = createRequire(import.meta.url);

function inject(id, exports) {
  const resolved = req.resolve(id);
  req.cache[resolved] = {
    id: resolved,
    filename: resolved,
    loaded: true,
    exports,
    children: [],
    paths: [],
  };
  return exports;
}

const dbMock = inject('../../electron/db.js', {
  webPostId: vi.fn((url) => url.replace(/^https?:\/\//, '')),
  getPost: vi.fn(() => null),
  createWebPlaceholder: vi.fn(),
});

const jobstoreMock = inject('../../electron/jobstore.js', {
  mirror: vi.fn(),
  forget: vi.fn(),
  forgetAll: vi.fn(),
  forgetExcept: vi.fn(),
  resumable: vi.fn(() => []),
  TERMINAL: new Set(['done', 'cancelled', 'error']),
});

inject('../../electron/webcapture.js', {
  discoverPages: vi.fn(() => new Promise(() => {})),
  capturePage: vi.fn(),
});
inject('../../electron/capture-engine.js', {
  capturePage: vi.fn(),
  discoverPages: vi.fn(() => new Promise(() => {})),
  closeBrowser: vi.fn(),
  activeEngine: () => 'osr',
});
inject('../../electron/web-enrich.js', {});
inject('../../electron/analyzer.js', { enqueuePost: vi.fn(() => ({ queued: true })) });
inject('../../electron/net-safety.js', {
  assertSafeUrl: vi.fn((url) => {
    if (url.includes('blocked')) throw new Error('Blocked host');
  }),
});

const weborchestrator = req('../../electron/weborchestrator.js');

// ─── recover() — crash-safe re-enqueue (jobstore.js contract) ───────────────────

describe('weborchestrator.recover()', () => {
  it('re-enqueues first and drops stale rows only via forgetExcept', () => {
    // Pause the queue so the re-enqueued job stays 'pending' (no runJob side
    // effects) and the test remains fully synchronous.
    weborchestrator.pauseAll();

    jobstoreMock.resumable.mockReturnValue([
      { key: 'web:ok.example', url: 'https://ok.example', maxPages: 2, overwrite: true },
      { key: 'web:blocked.example', url: 'https://blocked.example' },
      { key: 'web:malformed' }, // no url
    ]);

    const res = weborchestrator.recover();

    expect(res.recovered).toBe(1);
    // Durable rows are never deleted before the re-enqueue has re-mirrored them.
    expect(jobstoreMock.forget).not.toHaveBeenCalled();
    expect(jobstoreMock.forgetExcept).toHaveBeenCalledTimes(1);

    const [kind, keep] = jobstoreMock.forgetExcept.mock.calls[0];
    expect(kind).toBe('web');
    expect(keep.has('web:ok.example')).toBe(true); // re-enqueued → fresh mirror stands
    expect(keep.has('web:blocked.example')).toBe(true); // failed re-enqueue → retried next boot
    expect(keep.has('web:malformed')).toBe(false); // malformed row → dropped

    // The re-enqueue mirrored the live job BEFORE any stale row was forgotten.
    expect(jobstoreMock.mirror).toHaveBeenCalled();
    expect(jobstoreMock.mirror.mock.invocationCallOrder[0]).toBeLessThan(
      jobstoreMock.forgetExcept.mock.invocationCallOrder[0],
    );

    // The recovered job carries the persisted overwrite flag.
    const job = weborchestrator.getJobs().find((j) => j.key === 'web:ok.example');
    expect(job).toBeDefined();
    expect(job.overwrite).toBe(true);
    expect(dbMock.createWebPlaceholder).toHaveBeenCalledWith('https://ok.example');
  });
});
