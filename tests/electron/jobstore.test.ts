import { describe, it, expect, beforeEach, vi, type Mock } from 'vitest';

interface JobInput {
  key: string;
  postId?: string;
  status?: string;
  progress?: number;
}

interface DbMock {
  jobUpsert: Mock;
  jobsUpsertMany: Mock;
  jobDelete: Mock;
  jobsDeleteMany: Mock;
  jobDeleteAll: Mock;
  jobsByKind: Mock;
}

// jobstore imports its db dependency as `import * as db from './db'`, so the
// vi.mock factory returns the named exports object directly. vi.hoisted lets the
// factory and the test body share the same mock instance.
const dbMock = vi.hoisted(
  (): DbMock => ({
    jobUpsert: vi.fn(),
    jobsUpsertMany: vi.fn(),
    jobDelete: vi.fn(),
    jobsDeleteMany: vi.fn(),
    jobDeleteAll: vi.fn(),
    jobsByKind: vi.fn(() => []),
  }),
);
vi.mock('../../electron/db', () => dbMock);

interface JobStore {
  mirror: (kind: string, job: JobInput) => void;
  mirrorMany: (kind: string, jobs: Array<JobInput | null | { noKey: true }>) => void;
  forget: (kind: string, key: string) => void;
  forgetMany: (kind: string, keys: string[]) => void;
  forgetAll: (kind: string) => void;
  forgetExcept: (kind: string, keep: Set<string>) => void;
}

const jobstore = (await import('../../electron/jobstore')) as unknown as JobStore;

const KIND = 'test';

beforeEach(() => {
  // Drop the module-level signature cache for the kind, then reset the mocks so
  // assertions only see calls made by the test body.
  jobstore.forgetAll(KIND);
  for (const fn of Object.values(dbMock)) fn.mockReset();
  dbMock.jobsByKind.mockReturnValue([]);
});

describe('mirror()', () => {
  it('persists the first mirror and skips progress-only changes', () => {
    const job: JobInput = { key: 'k1', postId: 'p1', status: 'downloading', progress: 0 };
    jobstore.mirror(KIND, job);
    expect(dbMock.jobUpsert).toHaveBeenCalledTimes(1);

    jobstore.mirror(KIND, { ...job, progress: 0.42 });
    jobstore.mirror(KIND, { ...job, progress: 0.97 });
    expect(dbMock.jobUpsert).toHaveBeenCalledTimes(1);

    jobstore.mirror(KIND, { ...job, progress: 1, status: 'done' });
    expect(dbMock.jobUpsert).toHaveBeenCalledTimes(2);
    expect(dbMock.jobUpsert).toHaveBeenLastCalledWith(
      expect.objectContaining({ kind: KIND, key: 'k1', status: 'done', progress: 1 }),
    );
  });

  it('persists again after forget() for the same key', () => {
    const job: JobInput = { key: 'k2', postId: 'p2', status: 'pending', progress: 0 };
    jobstore.mirror(KIND, job);
    jobstore.forget(KIND, 'k2');
    expect(dbMock.jobDelete).toHaveBeenCalledWith(KIND, 'k2');

    jobstore.mirror(KIND, job);
    expect(dbMock.jobUpsert).toHaveBeenCalledTimes(2);
  });

  it('does not cache the signature when the persist fails', () => {
    dbMock.jobUpsert.mockImplementationOnce(() => {
      throw new Error('disk full');
    });
    const job: JobInput = { key: 'k3', postId: 'p3', status: 'pending', progress: 0 };
    jobstore.mirror(KIND, job);
    jobstore.mirror(KIND, job);
    expect(dbMock.jobUpsert).toHaveBeenCalledTimes(2);
  });
});

describe('mirrorMany()', () => {
  it('writes all jobs through a single jobsUpsertMany call', () => {
    const jobs: JobInput[] = [
      { key: 'a', postId: 'p1', status: 'pending', progress: 0 },
      { key: 'b', postId: 'p2', status: 'pending', progress: 0 },
    ];
    jobstore.mirrorMany(KIND, jobs);

    expect(dbMock.jobsUpsertMany).toHaveBeenCalledTimes(1);
    const rows = dbMock.jobsUpsertMany.mock.calls[0][0] as JobInput[];
    expect(rows).toHaveLength(2);
    expect(rows.map((r) => r.key)).toEqual(['a', 'b']);
    expect(dbMock.jobUpsert).not.toHaveBeenCalled();
  });

  it('feeds the skip cache so later progress-only mirrors are skipped', () => {
    const job: JobInput = { key: 'c', postId: 'p1', status: 'downloading', progress: 0 };
    jobstore.mirrorMany(KIND, [job]);

    jobstore.mirror(KIND, { ...job, progress: 0.5 });
    expect(dbMock.jobUpsert).not.toHaveBeenCalled();

    jobstore.mirrorMany(KIND, [{ ...job, progress: 0.9 }]);
    expect(dbMock.jobsUpsertMany).toHaveBeenCalledTimes(1);
  });

  it('skips empty/unchanged batches entirely', () => {
    jobstore.mirrorMany(KIND, []);
    jobstore.mirrorMany(KIND, [null, { noKey: true }]);
    expect(dbMock.jobsUpsertMany).not.toHaveBeenCalled();
  });
});

describe('forgetMany()', () => {
  it('deletes via jobsDeleteMany and resets the skip cache', () => {
    const jobs: JobInput[] = [
      { key: 'd', postId: 'p1', status: 'pending', progress: 0 },
      { key: 'e', postId: 'p2', status: 'pending', progress: 0 },
    ];
    jobstore.mirrorMany(KIND, jobs);
    jobstore.forgetMany(KIND, ['d', 'e']);
    expect(dbMock.jobsDeleteMany).toHaveBeenCalledWith(KIND, ['d', 'e']);

    jobstore.mirror(KIND, jobs[0]);
    expect(dbMock.jobUpsert).toHaveBeenCalledTimes(1);
  });

  it('is a no-op for an empty key list', () => {
    jobstore.forgetMany(KIND, []);
    expect(dbMock.jobsDeleteMany).not.toHaveBeenCalled();
  });
});

describe('forgetExcept()', () => {
  it('drops only non-terminal rows that were not re-enqueued', () => {
    dbMock.jobsByKind.mockReturnValue([
      { key: 'keep', status: 'pending' },
      { key: 'drop', status: 'pending' },
      { key: 'terminal', status: 'done' },
    ]);
    jobstore.forgetExcept(KIND, new Set(['keep']));
    expect(dbMock.jobsDeleteMany).toHaveBeenCalledWith(KIND, ['drop']);
  });
});
