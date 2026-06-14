import { describe, it, expect, vi } from 'vitest';

// ─── ESM module mocking ─────────────────────────────────────────────────────────
// analyzer.ts is ESM, so vi.mock() hoisting intercepts its static imports. The
// factories are built in vi.hoisted blocks so the test body and the mocked
// modules share the very same mock objects.

// Persist paused:true so recover() restores the pause BEFORE re-enqueuing: the
// pump never starts a runJob and the test stays fully synchronous. The userData
// dir + ai-model.json are created in a hoisted block so they exist before the
// mocked `electron.app.getPath` (and the analyzer import) reference them.
const userData = vi.hoisted(() => {
  const { mkdtempSync, writeFileSync } = require('fs') as typeof import('fs');
  const { tmpdir } = require('os') as typeof import('os');
  const { join } = require('path') as typeof import('path');
  const dir = mkdtempSync(join(tmpdir(), 'shelfy-analyzer-recover-'));
  writeFileSync(join(dir, 'ai-model.json'), JSON.stringify({ paused: true }));
  return dir;
});

// analyzer imports `import { app } from 'electron'`, and getPath('userData') must
// point at the temp dir holding the persisted pause file.
vi.mock('electron', () => ({ app: { getPath: (): string => userData } }));

// analyzer imports `import * as db from './db'` → return the named exports object.
const dbMock = vi.hoisted(() => ({
  clearStuckAnalyzing: vi.fn(),
  getPost: vi.fn(),
}));
vi.mock('../../electron/db', () => dbMock);

// analyzer imports `import * as jobstore from './jobstore'` → named exports object.
const jobstoreMock = vi.hoisted(() => ({
  mirror: vi.fn(),
  forget: vi.fn(),
  forgetAll: vi.fn(),
  forgetExcept: vi.fn(),
  resumable: vi.fn((): unknown[] => []),
  TERMINAL: new Set(['done', 'cancelled', 'error']),
}));
vi.mock('../../electron/jobstore', () => jobstoreMock);

// analyzer imports `import * as hardware from './hardware'` → named exports object.
vi.mock('../../electron/hardware', () => ({
  detect: vi.fn(() => ({})),
  computeLlamaTuning: vi.fn(() => ({})),
  recommendModel: vi.fn(() => null),
}));
// analyzer imports `import * as binaries from './binaries'` → named exports object.
vi.mock('../../electron/binaries', () => ({
  getLlamaVariant: vi.fn(() => null),
  markVariantFailed: vi.fn(),
}));

interface ResumableJob {
  key: string;
  postId?: string;
}
interface Analyzer {
  recover: () => { recovered: number };
  getIsPaused: () => boolean;
}

const analyzer = (await import('../../electron/analyzer')) as unknown as Analyzer;

// ─── recover() — crash-safe re-enqueue (jobstore.js contract) ───────────────────

describe('analyzer.recover()', () => {
  it('re-enqueues first and drops stale rows only via forgetExcept', () => {
    dbMock.getPost.mockImplementation((id: string) => {
      if (id === 'p-ok') return { id: 'p-ok', text: 'una caption analizzabile' };
      if (id === 'p-gone') return null;
      if (id === 'p-throws') throw new Error('db locked');
      if (id === 'p-noasset') return { id: 'p-noasset' };
      return null;
    });
    jobstoreMock.resumable.mockReturnValue([
      { key: 'p-ok:analyze', postId: 'p-ok' },
      { key: 'p-gone:analyze', postId: 'p-gone' },
      { key: 'p-throws:analyze', postId: 'p-throws' },
      { key: 'p-noasset:analyze', postId: 'p-noasset' },
      { key: 'malformed:analyze' }, // no postId
    ] satisfies ResumableJob[]);

    const res = analyzer.recover();

    expect(res.recovered).toBe(1);
    // Durable rows are never deleted before the re-enqueue has re-mirrored them.
    expect(jobstoreMock.forget).not.toHaveBeenCalled();
    expect(jobstoreMock.forgetExcept).toHaveBeenCalledTimes(1);

    const [kind, keep] = jobstoreMock.forgetExcept.mock.calls[0] as [string, Set<string>];
    expect(kind).toBe('analyze');
    expect(keep.has('p-ok:analyze')).toBe(true); // re-enqueued → fresh mirror stands
    expect(keep.has('p-throws:analyze')).toBe(true); // transient DB failure → retried next boot
    expect(keep.has('p-noasset:analyze')).toBe(true); // not analyzable yet → intent survives
    expect(keep.has('p-gone:analyze')).toBe(false); // post deleted → stale row dropped
    expect(keep.has('malformed:analyze')).toBe(false);

    // The re-enqueue mirrored the live job BEFORE any stale row was forgotten.
    expect(jobstoreMock.mirror).toHaveBeenCalled();
    expect(jobstoreMock.mirror.mock.invocationCallOrder[0]).toBeLessThan(
      jobstoreMock.forgetExcept.mock.invocationCallOrder[0],
    );

    // The persisted pause survived recovery (no silent auto-resume).
    expect(analyzer.getIsPaused()).toBe(true);
  });
});
