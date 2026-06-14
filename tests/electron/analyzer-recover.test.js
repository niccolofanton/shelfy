import { describe, it, expect, vi } from 'vitest';
import { createRequire } from 'module';
import { mkdtempSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';

// ─── Require cache patching ────────────────────────────────────────────────────
// analyzer.js is CJS, so vi.mock hoisting won't intercept its synchronous
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

// Persist paused:true so recover() restores the pause BEFORE re-enqueuing: the
// pump never starts a runJob and the test stays fully synchronous.
const userData = mkdtempSync(join(tmpdir(), 'shelfy-analyzer-recover-'));
writeFileSync(join(userData, 'ai-model.json'), JSON.stringify({ paused: true }));

inject('electron', { app: { getPath: () => userData } });

const dbMock = inject('../../electron/db.js', {
  clearStuckAnalyzing: vi.fn(),
  getPost: vi.fn(),
});

const jobstoreMock = inject('../../electron/jobstore.js', {
  mirror: vi.fn(),
  forget: vi.fn(),
  forgetAll: vi.fn(),
  forgetExcept: vi.fn(),
  resumable: vi.fn(() => []),
  TERMINAL: new Set(['done', 'cancelled', 'error']),
});

inject('../../electron/hardware.js', {
  detect: vi.fn(() => ({})),
  computeLlamaTuning: vi.fn(() => ({})),
  recommendModel: vi.fn(() => null),
});
inject('../../electron/binaries.js', {
  getLlamaVariant: vi.fn(() => null),
  markVariantFailed: vi.fn(),
});

const analyzer = req('../../electron/analyzer.js');

// ─── recover() — crash-safe re-enqueue (jobstore.js contract) ───────────────────

describe('analyzer.recover()', () => {
  it('re-enqueues first and drops stale rows only via forgetExcept', () => {
    dbMock.getPost.mockImplementation((id) => {
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
    ]);

    const res = analyzer.recover();

    expect(res.recovered).toBe(1);
    // Durable rows are never deleted before the re-enqueue has re-mirrored them.
    expect(jobstoreMock.forget).not.toHaveBeenCalled();
    expect(jobstoreMock.forgetExcept).toHaveBeenCalledTimes(1);

    const [kind, keep] = jobstoreMock.forgetExcept.mock.calls[0];
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
