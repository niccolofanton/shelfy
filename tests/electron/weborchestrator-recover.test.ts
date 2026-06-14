import { describe, it, expect, vi, type Mock } from 'vitest';

// ─── ESM mocking ───────────────────────────────────────────────────────────────
// weborchestrator.ts is ESM and imports each dependency as `import * as dep` (and
// `import { assertSafeUrl } from './net-safety'`), so each vi.mock factory returns
// the named-exports object directly. vi.hoisted lets the factory and the test body
// share the same mock instance (vi.mock is hoisted above the imports).

interface DbMock {
  webPostId: Mock;
  getPost: Mock;
  createWebPlaceholder: Mock;
}

interface JobstoreMock {
  mirror: Mock;
  forget: Mock;
  forgetAll: Mock;
  forgetExcept: Mock;
  resumable: Mock;
  TERMINAL: Set<string>;
}

const dbMock = vi.hoisted(
  (): DbMock => ({
    webPostId: vi.fn((url: string) => url.replace(/^https?:\/\//, '')),
    getPost: vi.fn(() => null),
    createWebPlaceholder: vi.fn(),
  }),
);

const jobstoreMock = vi.hoisted(
  (): JobstoreMock => ({
    mirror: vi.fn(),
    forget: vi.fn(),
    forgetAll: vi.fn(),
    forgetExcept: vi.fn(),
    resumable: vi.fn((): unknown[] => []),
    TERMINAL: new Set(['done', 'cancelled', 'error']),
  }),
);

vi.mock('../../electron/db', () => dbMock);
vi.mock('../../electron/jobstore', () => jobstoreMock);

vi.mock('../../electron/webcapture', () => ({
  discoverPages: vi.fn(() => new Promise(() => {})),
  capturePage: vi.fn(),
}));
vi.mock('../../electron/capture-engine', () => ({
  capturePage: vi.fn(),
  discoverPages: vi.fn(() => new Promise(() => {})),
  closeBrowser: vi.fn(),
  activeEngine: (): string => 'osr',
}));
vi.mock('../../electron/web-enrich', () => ({}));
vi.mock('../../electron/analyzer', () => ({
  enqueuePost: vi.fn(() => ({ queued: true })),
}));
vi.mock('../../electron/net-safety', () => ({
  assertSafeUrl: vi.fn((url: string) => {
    if (url.includes('blocked')) throw new Error('Blocked host');
  }),
}));

interface WebJob {
  key: string;
  overwrite?: boolean;
}
interface WebOrchestrator {
  pauseAll: () => void;
  recover: () => { recovered: number };
  getJobs: () => WebJob[];
}

interface ResumableWebJob {
  key: string;
  url?: string;
  maxPages?: number;
  overwrite?: boolean;
}

const weborchestrator =
  (await import('../../electron/weborchestrator')) as unknown as WebOrchestrator;

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
    ] satisfies ResumableWebJob[]);

    const res = weborchestrator.recover();

    expect(res.recovered).toBe(1);
    // Durable rows are never deleted before the re-enqueue has re-mirrored them.
    expect(jobstoreMock.forget).not.toHaveBeenCalled();
    expect(jobstoreMock.forgetExcept).toHaveBeenCalledTimes(1);

    const [kind, keep] = jobstoreMock.forgetExcept.mock.calls[0] as [string, Set<string>];
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
    expect(job?.overwrite).toBe(true);
    expect(dbMock.createWebPlaceholder).toHaveBeenCalledWith('https://ok.example');
  });
});
