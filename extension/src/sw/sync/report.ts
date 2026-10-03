import { RUN_REPORT_FORMAT, type RunReport, type ReportRun } from '../../shared/run-report';
import { RUN_KEYS_TTL_MS } from '../queue/types';
import type { Queue } from '../queue/queue';
import type { SettingsStore } from '../settings';
/** Export only accepted canonical keys and listing metadata. No captions,
 * author fields, media URLs, account IDs or credentials leave the worker. */
export async function exportRunReport(
  queue: Queue,
  store: SettingsStore,
  now: number,
): Promise<RunReport> {
  const pairing = await store.pairing();
  if (!pairing) throw new Error('not_paired');
  const records = await queue.acceptedRecords();
  const current = await store.pairing();
  if (!current || current.tokenId !== pairing.tokenId || current.token !== pairing.token)
    throw new Error('account_mismatch');
  const runs = new Map<string, ReportRun>();
  for (const record of records) {
    if (
      record.accountTokenId !== pairing.tokenId ||
      !record.platform ||
      !record.listingKey ||
      !record.trigger ||
      record.at < now - RUN_KEYS_TTL_MS
    )
      continue;
    const run = runs.get(record.runId) ?? {
      runId: record.runId,
      serverRunId: record.serverRunId,
      platform: record.platform,
      listingKey: record.listingKey,
      trigger: record.trigger,
      firstAcceptedAt: record.at,
      lastAcceptedAt: record.at,
      keys: [],
    };
    run.firstAcceptedAt = Math.min(run.firstAcceptedAt, record.at);
    run.lastAcceptedAt = Math.max(run.lastAcceptedAt, record.at);
    run.keys = [...new Set([...run.keys, ...record.keys])].sort();
    runs.set(run.runId, run);
  }
  return {
    format: RUN_REPORT_FORMAT,
    version: 1,
    exportedAt: new Date(now).toISOString(),
    retentionDays: 7,
    runs: [...runs.values()].sort((a, b) => b.lastAcceptedAt - a.lastAcceptedAt),
  };
}
