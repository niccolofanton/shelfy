import type {
  AnalyzeRequest,
  AnalyzeResult,
  WebAiQueueApi,
  WebAiQueuePage,
} from '@ui/api/ai/webQueue';
import type { EventStream } from '../events';
import type { Http } from '../http';
import { toPostSelector } from '../mapping';
import type { components } from '../schema';
type Schemas = components['schemas'];
function body(request: AnalyzeRequest): Schemas['AnalyzeRequest'] {
  return {
    selector: toPostSelector(request.selector),
    mode: request.mode === 'selected' && 'filter' in request.selector ? 'all' : request.mode,
    deep: request.deep ?? false,
  };
}
export function createWebAiQueueApi(http: Http, events: Pick<EventStream, 'on'>): WebAiQueueApi {
  const confirmationKeys = new Map<string, string>();
  const reads = new Map<string, Promise<WebAiQueuePage>>();
  async function json<T>(response: Response): Promise<T> {
    return (await response.json()) as T;
  }
  return {
    get(options = {}, signal) {
      const query = new URLSearchParams();
      if (options.state) query.set('state', options.state);
      if (options.cursor) query.set('cursor', options.cursor);
      const key = query.toString();
      if (!signal && reads.has(key)) return reads.get(key)!;
      const read = http.get<Schemas['QueueView']>('/api/v1/ai/queue', query, signal);
      if (!signal) {
        reads.set(key, read);
        void read.finally(() => reads.delete(key)).catch(() => {});
      }
      return read;
    },
    async estimate(request) {
      return json<AnalyzeResult>(
        await http.send('POST', '/api/v1/ai/analyze', { ...body(request), estimateOnly: true }),
      );
    },
    async confirm(request, token) {
      let key = confirmationKeys.get(token);
      if (!key) {
        key = crypto.randomUUID();
        confirmationKeys.set(token, key);
      }
      const result = await json<AnalyzeResult>(
        await http.send(
          'POST',
          '/api/v1/ai/analyze',
          { ...body(request), confirmToken: token },
          { idempotencyKey: key },
        ),
      );
      confirmationKeys.delete(token);
      return result;
    },
    async cancel(keys) {
      return (
        await json<Schemas['QueueChanged']>(
          await http.send('POST', '/api/v1/ai/queue/cancel', keys ? { keys } : { all: true }),
        )
      ).changed;
    },
    async retry(keys) {
      return (
        await json<Schemas['QueueChanged']>(
          await http.send('POST', '/api/v1/ai/queue/retry', keys ? { keys } : { all: true }, {
            idempotencyKey: crypto.randomUUID(),
          }),
        )
      ).changed;
    },
    async pause() {
      await http.send('POST', '/api/v1/queues/ai.drain/pause');
    },
    async resume() {
      await http.send('POST', '/api/v1/queues/ai.drain/resume');
    },
    onChanged(listener) {
      const offs = [
        events.on('job.updated', (job) => {
          if (job.kind === 'ai.drain') listener();
        }),
        events.on('posts.changed', listener),
        events.on('provider.status', listener),
        events.on('resync', listener),
      ];
      return () => {
        for (const off of offs) off();
      };
    },
    onStream: (listener) => events.on('ai.stream', listener),
  };
}
