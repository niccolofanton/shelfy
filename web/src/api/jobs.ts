// The web client's JobsApi (src/api/jobs.ts) on `/api/v1/jobs*` and
// `/api/v1/queues/*` (plan §2.9 Jobs, §2.12; P1-07's job system, P4-09's
// seam). Kept general — P2-08's Activity center reuses it (EXECUTION.md L14).
import type {
  Job,
  JobPage,
  JobsApi,
  JobsFilter,
  JobsPage,
  JobUpdate,
  QueueResult,
} from '@ui/api/jobs';
import type { EventStream } from './events';
import type { Http } from './http';
import { toSearchParams } from './mapping';
import type { components, operations } from './schema';

type Schemas = components['schemas'];
type ListJobsQuery = NonNullable<operations['listJobs']['parameters']['query']>;

async function json<T>(res: Response): Promise<T> {
  return (await res.json()) as T;
}

function toJob(job: Schemas['Job']): Job {
  return { ...job };
}

function toQueueResult(result: Schemas['QueueResult']): QueueResult {
  return { ...result, queue: { ...result.queue } };
}

export interface JobsApiOptions {
  // The client's realtime stream: `job.updated` patches a job already on screen.
  events: Pick<EventStream, 'on'>;
  // How the Idempotency-Key of a retry is made; a test can make it predictable.
  newIdempotencyKey?: () => string;
}

export function createJobsApi(http: Http, options: JobsApiOptions): JobsApi {
  const newIdempotencyKey = options.newIdempotencyKey ?? (() => crypto.randomUUID());

  return {
    async list(filter: JobsFilter, page: JobsPage): Promise<JobPage> {
      const query: ListJobsQuery = {
        kind: filter.kind?.length ? filter.kind : undefined,
        state: filter.state?.length ? filter.state : undefined,
        limit: Math.max(1, Math.min(200, Math.floor(page.limit))),
        cursor: page.cursor || undefined,
      };
      const result = await http.get<Schemas['JobPage']>('/api/v1/jobs', toSearchParams(query));
      return { items: result.items.map(toJob), nextCursor: result.nextCursor };
    },

    async summary() {
      const { queues } = await http.get<Schemas['JobsSummary']>('/api/v1/jobs/summary');
      return queues.map((q) => ({ ...q }));
    },

    async cancel(id: number): Promise<Job> {
      return toJob(
        await json<Schemas['Job']>(await http.send('POST', `/api/v1/jobs/${id}/cancel`)),
      );
    },
    async retry(id: number): Promise<Job> {
      return toJob(
        await json<Schemas['Job']>(
          await http.send('POST', `/api/v1/jobs/${id}/retry`, undefined, {
            idempotencyKey: newIdempotencyKey(),
          }),
        ),
      );
    },

    async pauseQueue(kind: string): Promise<QueueResult> {
      return toQueueResult(
        await json<Schemas['QueueResult']>(
          await http.send('POST', `/api/v1/queues/${encodeURIComponent(kind)}/pause`),
        ),
      );
    },
    async resumeQueue(kind: string): Promise<QueueResult> {
      return toQueueResult(
        await json<Schemas['QueueResult']>(
          await http.send('POST', `/api/v1/queues/${encodeURIComponent(kind)}/resume`),
        ),
      );
    },
    async cancelQueue(kind: string): Promise<QueueResult> {
      return toQueueResult(
        await json<Schemas['QueueResult']>(
          await http.send('POST', `/api/v1/queues/${encodeURIComponent(kind)}/cancel-all`),
        ),
      );
    },
    async clearFinishedQueue(kind: string): Promise<QueueResult> {
      return toQueueResult(
        await json<Schemas['QueueResult']>(
          await http.send('POST', `/api/v1/queues/${encodeURIComponent(kind)}/clear-finished`),
        ),
      );
    },

    onUpdate(listener: (update: JobUpdate) => void): () => void {
      return options.events.on('job.updated', (update) => listener({ ...update }));
    },
  };
}
