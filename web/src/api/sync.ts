import {
  isSyncPlatform,
  type SyncApi,
  type SyncExtension,
  type SyncProgress,
  type SyncRun,
} from '@ui/api/sync';
import { createSyncExtension } from '../extension/bridge';
import type { EventStream } from './events';
import type { Http } from './http';

export function createSyncApi(
  http: Http,
  events: Pick<EventStream, 'on'>,
  extension: SyncExtension = createSyncExtension(),
  expectedAccountId?: string,
): SyncApi {
  return {
    ...extension,
    async connection() {
      const status = await extension.connection();
      if (!expectedAccountId || status.accountId !== expectedAccountId || !status.tokenId)
        return {
          ...status,
          syncing: {},
          planner: [],
          code: 'account_mismatch',
          extension:
            status.extension.state === 'ready'
              ? { ...status.extension, paired: false }
              : status.extension,
        };
      return status;
    },
    start: (target, binding) =>
      !expectedAccountId || binding.expectedAccountId !== expectedAccountId
        ? Promise.resolve({ ok: false, code: 'account_mismatch' })
        : extension.start(target, binding),
    stop: (platform, binding) =>
      !expectedAccountId || binding.expectedAccountId !== expectedAccountId
        ? Promise.resolve({ ok: false, code: 'account_mismatch' })
        : extension.stop(platform, binding),
    list({ limit = 100, cursor, state, platform, signal } = {}) {
      const query = new URLSearchParams({ limit: String(Math.min(100, Math.max(1, limit))) });
      if (cursor) query.set('cursor', cursor);
      if (state) query.set('state', state);
      if (platform) query.set('platform', platform);
      return http.get<{ items: SyncRun[]; nextCursor: string | null }>(
        '/api/v1/sync-runs',
        query,
        signal,
      );
    },
    onProgress(listener) {
      return events.on('sync.progress', (data) => {
        if (
          !isSyncPlatform(data.platform) ||
          !['running', 'done', 'stopped', 'failed'].includes(data.state)
        )
          return;
        if (
          [data.scanned, data.inserted, data.known, data.updated, data.pages].some(
            (n) => !Number.isSafeInteger(n) || n < 0,
          )
        )
          return;
        listener(data as SyncProgress);
      });
    },
    onRefresh(listener) {
      const hello = events.on('hello', listener);
      const resync = events.on('resync', listener);
      return () => {
        hello();
        resync();
      };
    },
  };
}
