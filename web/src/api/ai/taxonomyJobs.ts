// A run is a durable ai.run job. SSE drives progress; polling covers lost
// events and tab reconnects. Cancel only targets the job this call accepted.
import type { AiTaxonomyProgress } from '@ui/api/ai/tags';
import type { EventStream } from '../events';
import { ApiError, isApiError, type Http } from '../http';
import type { components } from '../schema';
type Schemas = components['schemas'];
type Kind = 'clusters' | 'aliases';
type Job = Schemas['Job'];
type Update = Schemas['JobUpdatedEvent'];
export interface TaxonomyJobsOptions {
  events?: Pick<EventStream, 'on'>;
  pollIntervalMs?: number;
  newIdempotencyKey?: () => string;
}
interface Session {
  accepted?: Promise<Job>;
  done: Promise<Job>;
  apply: (update: Job | Update) => void;
  closed: boolean;
}
export function createTaxonomyJobs(http: Http, options: TaxonomyJobsOptions = {}) {
  const sessions = new Map<Kind, Session>();
  const interval = options.pollIntervalMs ?? 2000;
  const key = options.newIdempotencyKey ?? (() => crypto.randomUUID());
  function run(kind: Kind, progress?: (p: AiTaxonomyProgress) => void): Promise<Job> {
    const existing = sessions.get(kind);
    if (existing) return existing.done;
    let job: Job | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let reading = false;
    const pending = new Map<number, Update>();
    const off: (() => void)[] = [];
    let resolve!: (job: Job) => void;
    let reject!: (error: unknown) => void;
    const done = new Promise<Job>((ok, fail) => {
      resolve = ok;
      reject = fail;
    });
    const finish = (error?: unknown) => {
      if (session.closed) return;
      session.closed = true;
      if (timer) clearTimeout(timer);
      off.forEach((unsubscribe) => unsubscribe());
      sessions.delete(kind);
      if (error) reject(error);
      else resolve(job!);
    };
    const apply = (update: Job | Update) => {
      if (session.closed || update.kind !== 'ai.run') return;
      if (!job) {
        if (pending.size < 200 || pending.has(update.id)) pending.set(update.id, update as Update);
        return;
      }
      if (update.id !== job.id) return;
      job = { ...job, ...update };
      const stage = /^(embed|group|refine|aliases|complete):(\d+)\/(\d+)$/.exec(job.stage ?? '');
      progress?.({
        jobId: job.id,
        state: job.state,
        progress: job.progress,
        stage: stage?.[1] ?? null,
        done: stage ? Number(stage[2]) : 0,
        total: stage ? Number(stage[3]) : 0,
        waiting: job.state === 'queued' && job.runAt > Date.now() + 1000,
      });
      if (job.state === 'succeeded') finish();
      else if (job.state === 'cancelled')
        finish(Object.assign(new Error('Taxonomy run cancelled'), { code: 'cancelled' }));
      else if (job.state === 'failed')
        finish(
          Object.assign(new Error('Taxonomy run failed'), { code: job.errorCode ?? 'unavailable' }),
        );
    };
    const refresh = async () => {
      if (session.closed || !job || reading) return;
      reading = true;
      if (timer) clearTimeout(timer);
      timer = null;
      try {
        let cursor: string | null = null;
        do {
          const query = new URLSearchParams({ kind: 'ai.run', limit: '200' });
          if (cursor) query.set('cursor', cursor);
          const page: Schemas['JobPage'] = await http.get('/api/v1/jobs', query);
          const found = page.items.find((item) => item.id === job!.id);
          if (found) {
            apply(found);
            return;
          }
          cursor = page.nextCursor;
        } while (cursor && !session.closed);
        if (!session.closed) finish(new ApiError(404, 'not_found'));
      } catch (error) {
        // Network interruption does not cancel a durable job. Authentication and
        // other API failures still surface through the app's ordinary handling.
        if (!isApiError(error, 'network')) finish(error);
      } finally {
        reading = false;
        if (!session.closed) timer = setTimeout(() => void refresh(), interval);
      }
    };
    // Install listeners before starting: even a zero-work run can finish before
    // its POST response reaches the browser.
    const session: Session = {
      done,
      apply,
      closed: false,
    };
    sessions.set(kind, session);
    if (options.events) {
      off.push(options.events.on('job.updated', apply));
      off.push(options.events.on('resync', () => void refresh()));
    }
    session.accepted = (async () => {
      const path = kind === 'clusters' ? '/tag-clusters/regenerate' : '/tag-aliases/propose';
      const response = await http.send('POST', `/api/v1${path}`, undefined, {
        idempotencyKey: key(),
      });
      const accepted = (await response.json()) as Job;
      if (!Number.isSafeInteger(accepted.id) || accepted.id < 1 || accepted.kind !== 'ai.run')
        throw new ApiError(502, 'internal');
      job = accepted;
      apply(accepted);
      const buffered = pending.get(accepted.id);
      pending.clear();
      if (buffered) apply(buffered);
      if (!session.closed) void refresh();
      return accepted;
    })();
    void session.accepted.catch(finish);
    return done;
  }
  async function cancel(kind: Kind): Promise<{ cancelled: boolean }> {
    const session = sessions.get(kind);
    if (!session?.accepted) return { cancelled: false };
    const accepted = await session.accepted;
    if (session.closed) return { cancelled: false };
    const job = (await (
      await http.send('POST', `/api/v1/jobs/${accepted.id}/cancel`)
    ).json()) as Job;
    session.apply(job);
    return { cancelled: job.state === 'cancelled' };
  }
  return { run, cancel };
}
