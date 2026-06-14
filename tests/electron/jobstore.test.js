import { describe, it, expect, beforeEach, vi } from 'vitest';
import { createRequire } from 'module';

// jobstore.js is CJS, so patch the require cache for its ./db dependency
// before loading it (same technique as downloader.test.js).
const req = createRequire(import.meta.url);

const dbMock = {
  jobUpsert: vi.fn(),
  jobsUpsertMany: vi.fn(),
  jobDelete: vi.fn(),
  jobsDeleteMany: vi.fn(),
  jobDeleteAll: vi.fn(),
  jobsByKind: vi.fn(() => []),
};
req.cache[req.resolve('../../electron/db.js')] = {
  id: req.resolve('../../electron/db.js'),
  filename: req.resolve('../../electron/db.js'),
  loaded: true,
  exports: dbMock,
  children: [],
  paths: [],
};

const jobstore = req('../../electron/jobstore.js');

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
    const job = { key: 'k1', postId: 'p1', status: 'downloading', progress: 0 };
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
    const job = { key: 'k2', postId: 'p2', status: 'pending', progress: 0 };
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
    const job = { key: 'k3', postId: 'p3', status: 'pending', progress: 0 };
    jobstore.mirror(KIND, job);
    jobstore.mirror(KIND, job);
    expect(dbMock.jobUpsert).toHaveBeenCalledTimes(2);
  });
});

describe('mirrorMany()', () => {
  it('writes all jobs through a single jobsUpsertMany call', () => {
    const jobs = [
      { key: 'a', postId: 'p1', status: 'pending', progress: 0 },
      { key: 'b', postId: 'p2', status: 'pending', progress: 0 },
    ];
    jobstore.mirrorMany(KIND, jobs);

    expect(dbMock.jobsUpsertMany).toHaveBeenCalledTimes(1);
    const rows = dbMock.jobsUpsertMany.mock.calls[0][0];
    expect(rows).toHaveLength(2);
    expect(rows.map((r) => r.key)).toEqual(['a', 'b']);
    expect(dbMock.jobUpsert).not.toHaveBeenCalled();
  });

  it('feeds the skip cache so later progress-only mirrors are skipped', () => {
    const job = { key: 'c', postId: 'p1', status: 'downloading', progress: 0 };
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
    const jobs = [
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
