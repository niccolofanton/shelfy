// The signed-in account's background jobs (web port plan §2.12, §2.19): the
// Jobs view (P4-09) is the only UI today, but this seam is deliberately
// general — P2-08's Activity center reuses it (EXECUTION.md L14) instead of
// opening a second path onto the same server state.
//
// Transport-neutral, like src/api/account.ts: the web client
// (web/src/api/jobs.ts) maps these operations onto `/api/v1/jobs*` and
// `/api/v1/queues/*`, and its `onUpdate` onto the `job.updated` realtime
// event. Only the web client has one (ShelfyClient.jobs): the desktop keeps
// its own Downloads queue (PG18) and leaves it undefined.

// A job's lifecycle state (crates/server/src/events/model.rs JobState).
export type JobState = 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled';

export const FINAL_JOB_STATES: readonly JobState[] = ['succeeded', 'failed', 'cancelled'];

export function isFinalJobState(state: JobState): boolean {
  return (FINAL_JOB_STATES as readonly string[]).includes(state);
}

// A background job, as `GET /jobs` and the job actions return it.
export interface Job {
  id: number;
  // `archive.drain`, `capture.site`, … An unknown kind (a newer server, or a
  // kind this build does not label yet) still renders: see jobKindLabel.
  kind: string;
  state: JobState;
  // 0–1, when the job reports one.
  progress: number | null;
  // A stage code, when the job has stages; a kind with none leaves it null.
  stage: string | null;
  // The post this job works on, when it has one (capture, video, archive…).
  postKey: string | null;
  // Why the job failed, or its last error before a retry. A stable code.
  errorCode: string | null;
  attempts: number;
  maxAttempts: number;
  runAt: number;
  createdAt: number;
  updatedAt: number;
  finishedAt: number | null;
}

// `job.updated`: only the fields that change while a job runs (plan §2.10).
// `attempts` and the timestamps are not part of the event — they are
// refreshed by the next list page, or by the job a cancel/retry call itself
// returns.
export interface JobUpdate {
  id: number;
  kind: string;
  state: JobState;
  progress: number | null;
  stage: string | null;
  postKey: string | null;
  errorCode: string | null;
}

export interface JobsFilter {
  // Only these kinds; empty means every kind.
  kind?: string[];
  // Only these states; empty means every state.
  state?: JobState[];
}

export interface JobsPage {
  // 1–200.
  limit: number;
  // `nextPage` of the previous call; absent for the first page.
  cursor?: string | null;
}

export interface JobPage {
  items: Job[];
  // Pass as `cursor` for the next page; null on the last page.
  nextCursor: string | null;
}

// One kind of job of the user: its queue.
export interface QueueSummary {
  kind: string;
  // The user paused it: its queued jobs wait.
  paused: boolean;
  queued: number;
  running: number;
  succeeded: number;
  failed: number;
  cancelled: number;
}

// The answer of an action on a queue.
export interface QueueResult {
  queue: QueueSummary;
  // What the action changed: jobs cancelled or deleted (1/0 for pause/resume).
  affected: number;
}

export interface JobsApi {
  // One page of the user's jobs, newest first.
  list(filter: JobsFilter, page: JobsPage): Promise<JobPage>;
  // Every queue of the user (registered kinds, plus any the user has jobs of).
  summary(): Promise<QueueSummary[]>;

  // Cancels a queued or running job. A job already finished answers a
  // conflict; cancelling a cancelled job is a no-op.
  cancel(id: number): Promise<Job>;
  // Queues a failed or cancelled job again, every try available, error
  // cleared. Sends its own Idempotency-Key so a repeated click (or an
  // automatic resend after a re-authentication) retries once.
  retry(id: number): Promise<Job>;

  pauseQueue(kind: string): Promise<QueueResult>;
  resumeQueue(kind: string): Promise<QueueResult>;
  cancelQueue(kind: string): Promise<QueueResult>;
  clearFinishedQueue(kind: string): Promise<QueueResult>;

  // Live updates, at most every 250 ms per job; returns the unsubscribe
  // function. Patches a job already on screen — it never inserts a job the
  // caller has not fetched yet (a resync, or the next list load, does).
  onUpdate(listener: (update: JobUpdate) => void): () => void;
}
