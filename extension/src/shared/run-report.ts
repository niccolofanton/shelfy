import { isRecord, isPlatform, type Platform, type Trigger } from './protocol';
import { parseListingKey } from './listing';
export const RUN_REPORT_FORMAT = 'shelfy-run-report';
export interface ReportRun {
  runId: string;
  serverRunId: string | null;
  platform: Platform;
  listingKey: string;
  trigger: Trigger;
  firstAcceptedAt: number;
  lastAcceptedAt: number;
  keys: string[];
}
export interface RunReport {
  format: typeof RUN_REPORT_FORMAT;
  version: 1;
  exportedAt: string;
  retentionDays: 7;
  runs: ReportRun[];
}
export function parseRunReport(value: unknown): RunReport {
  if (
    !isRecord(value) ||
    value.format !== RUN_REPORT_FORMAT ||
    value.version !== 1 ||
    typeof value.exportedAt !== 'string' ||
    !Array.isArray(value.runs)
  )
    throw new Error('invalid Shelfy run report');
  const runs = value.runs.map((run) => {
    if (
      !isRecord(run) ||
      typeof run.runId !== 'string' ||
      !isPlatform(run.platform) ||
      typeof run.listingKey !== 'string' ||
      parseListingKey(run.listingKey)?.platform !== run.platform ||
      !Array.isArray(run.keys) ||
      !run.keys.every((key) => typeof key === 'string' && /^(ig|x|pin)_\d{1,40}$/.test(key)) ||
      !Number.isFinite(run.firstAcceptedAt) ||
      !Number.isFinite(run.lastAcceptedAt) ||
      !['passive', 'manual', 'web', 'scheduled', 'selection', 'refresh'].includes(
        String(run.trigger),
      )
    )
      throw new Error('malformed run report entry');
    return run as unknown as ReportRun;
  });
  // Empty exports cannot be mistaken for a successful parity run.
  if (!runs.some((run) => run.keys.length)) throw new Error('run report has no accepted keys');
  return {
    format: RUN_REPORT_FORMAT,
    version: 1,
    exportedAt: value.exportedAt,
    retentionDays: 7,
    runs,
  };
}
