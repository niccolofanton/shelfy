import { describe, expect, it, vi } from 'vitest';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { exportRunReport } from '../src/sw/sync/report';
import { parseRunReport, RUN_REPORT_FORMAT } from '../src/shared/run-report';
import { runCompareCli } from '../scripts/compare-lib';
import { harness, T0, igItem } from './helpers';
import { prefilterBatch } from '../src/sw/prefilter';
import type { RunKeys } from '../src/sw/queue/types';
const record = (owner = 'tok-1', overrides: Partial<RunKeys> = {}): RunKeys => ({
  id: 'batch',
  runId: 'run',
  serverRunId: 'server',
  at: T0,
  keys: ['ig_3500000000000000001'],
  accountTokenId: owner,
  platform: 'instagram',
  listingKey: 'instagram:ig_saved',
  trigger: 'web',
  ...overrides,
});
describe('accepted-key parity exports', () => {
  it('exports deduplicated accepted keys and listing metadata, excludes legacy/expired/other-account records and credentials', async () => {
    const h = harness();
    await h.pairNow();
    vi.spyOn(h.queue, 'acceptedRecords').mockResolvedValue([
      record(),
      record('tok-1', {
        id: 'otherbatch',
        keys: ['ig_3500000000000000001', 'ig_3500000000000000002'],
      }),
      record('tok-2', { runId: 'other-account' }),
      record(undefined, { accountTokenId: undefined, runId: 'legacy' }),
      record('tok-1', { runId: 'expired', at: T0 - 8 * 86400000 }),
    ]);
    const report = await exportRunReport(h.queue, h.store, T0);
    expect(report.runs).toHaveLength(1);
    expect(report.runs[0].keys).toHaveLength(2);
    expect(parseRunReport(report)).toEqual(report);
    const json = JSON.stringify(report);
    expect(json).not.toContain((await h.store.pairing())!.token);
    expect(json).not.toContain('caption');
    expect(json).not.toContain('tok-1');
  });
  it('retains a real accepted batch after the closed run leaves the queue', async () => {
    const h = harness();
    await h.pairNow();
    const run = await h.queue.openRun(
      {
        platform: 'instagram',
        trigger: 'web',
        accountTokenId: 'tok-1',
        listing: { kind: 'ig_saved', externalId: null, name: null },
        collection: { mode: 'none' },
        tabId: 7,
        docId: 'doc',
        listingKey: 'instagram:ig_saved',
      },
      T0,
    );
    await h.queue.captureToRun(run.id, {
      source: 'replay',
      hasNextPage: false,
      items: prefilterBatch([igItem(1)], 'instagram').items,
      at: T0,
      messageId: null,
    });
    await h.queue.endRuns((value) => value.id === run.id, 'end_of_feed', T0);
    await h.uploader.flush({ force: true });
    expect(await h.queue.getRun(run.id)).toBeNull();
    const report = await exportRunReport(h.queue, h.store, T0);
    expect(report.runs).toMatchObject([
      { runId: run.id, platform: 'instagram', listingKey: 'instagram:ig_saved', trigger: 'web' },
    ]);
    expect(report.runs[0].keys).toHaveLength(1);
  });
  it('rejects a re-pair while accepted records await and refuses empty parity reports', async () => {
    const h = harness();
    await h.pairNow();
    const a = (await h.store.pairing())!;
    vi.spyOn(h.queue, 'acceptedRecords').mockImplementation(async () => {
      await h.store.setPairing({ ...a, tokenId: 'tok-B' });
      return [record()];
    });
    await expect(exportRunReport(h.queue, h.store, T0)).rejects.toThrow('account_mismatch');
    expect(() =>
      parseRunReport({
        format: RUN_REPORT_FORMAT,
        version: 1,
        exportedAt: new Date().toISOString(),
        runs: [],
      }),
    ).toThrow('no accepted keys');
  });
  it('compares --run-report per listing against desktop aliases and fails when a known desktop key is missing', () => {
    const dir = mkdtempSync(join(tmpdir(), 'shelfy-parity-'));
    const out: string[] = [],
      errors: string[] = [];
    try {
      const input = join(dir, 'run.json'),
        desktop = join(dir, 'desktop.json');
      writeFileSync(
        input,
        JSON.stringify({
          format: RUN_REPORT_FORMAT,
          version: 1,
          exportedAt: new Date(T0).toISOString(),
          runs: [{ ...record(), firstAcceptedAt: T0, lastAcceptedAt: T0 }],
        }),
      );
      writeFileSync(
        desktop,
        JSON.stringify({
          posts: [{ id: '3500000000000000001_9001', platform: 'instagram' }],
          collections: [],
        }),
      );
      const io = {
        stdout: (value: string) => out.push(value),
        stderr: (value: string) => errors.push(value),
        writeFile: vi.fn(),
      };
      expect(runCompareCli(['--run-report', input, '--desktop-export', desktop], io)).toBe(0);
      expect(out.join('')).toContain('100.00 %');
      expect(errors).toEqual([]);
      writeFileSync(
        desktop,
        JSON.stringify({
          posts: [1, 2].map((n) => ({ id: `350000000000000000${n}`, platform: 'instagram' })),
          collections: [],
        }),
      );
      expect(runCompareCli(['--run-report', input, '--desktop-export', desktop], io)).toBe(1);
      expect(
        runCompareCli(
          ['--run-report', input, '--extension', input, '--desktop-export', desktop],
          io,
        ),
      ).toBe(2);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
