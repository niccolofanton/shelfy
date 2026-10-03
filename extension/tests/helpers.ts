// Test helpers: an in-memory chrome.storage area, synthetic hook items, and the worker's parts
// wired to the fake Shelfy API (scripts/fake-api.ts) with a manual clock. Synthetic data only.

import { FakeShelfyApi } from '../scripts/fake-api';
import { EXTENSION_VERSION } from '../src/shared/version';
import { createUlid } from '../src/shared/ulid';
import { ApiClient } from '../src/sw/api';
import { ConfigService } from '../src/sw/config';
import { MemoryQueueStore } from '../src/sw/queue/memory';
import { Queue, type QueueOptions } from '../src/sw/queue/queue';
import { SettingsStore, type StorageArea } from '../src/sw/settings';
import { Uploader } from '../src/sw/uploader';

export const ORIGIN = 'http://localhost:18286';
export const T0 = 1_760_000_000_000;

export function memoryStorage(): StorageArea & { data: Map<string, unknown> } {
  const data = new Map<string, unknown>();
  return {
    data,
    async get(keys) {
      const out: Record<string, unknown> = {};
      for (const key of Array.isArray(keys) ? keys : [keys])
        if (data.has(key)) out[key] = structuredClone(data.get(key));
      return out;
    },
    async set(items) {
      for (const [key, value] of Object.entries(items)) data.set(key, structuredClone(value));
    },
    async remove(keys) {
      for (const key of Array.isArray(keys) ? keys : [keys]) data.delete(key);
    },
  };
}

/** A hook item (desktop InterceptItem shape) of an Instagram post with pk `3400…<n>`. */
export function igItem(n: number, extra: Record<string, unknown> = {}): Record<string, unknown> {
  const pk = `34000000000${String(n).padStart(8, '0')}`;
  return {
    id: `${pk}_9000000001`,
    platform: 'instagram',
    shortcode: '',
    postUrl: '',
    profileUrl: 'https://www.instagram.com/synthetic_author/',
    authorUsername: 'synthetic_author',
    authorName: 'Synthetic Author',
    text: `Synthetic caption ${n}`,
    thumbnailUrl: `https://scontent-synth1-1.cdninstagram.com/v/${n}.jpg?oe=68F00000`,
    mediaType: 'image',
    media: [{ type: 'image', url: `https://scontent-synth1-1.cdninstagram.com/v/${n}.jpg` }],
    timestamp: '2025-09-16T05:20:00.000Z',
    ...extra,
  };
}

export interface Harness {
  api: FakeShelfyApi;
  storage: ReturnType<typeof memoryStorage>;
  store: SettingsStore;
  queueStore: MemoryQueueStore;
  queue: Queue;
  client: ApiClient;
  config: ConfigService;
  uploader: Uploader;
  clock: { now: number };
  wakes: number[];
  changes: { count: number };
  /** Network down: every fetch rejects like Chrome's "Failed to fetch". */
  network: { down: boolean };
  /** Pairs the harness with a freshly minted token. */
  pairNow(): Promise<string>;
}

export function harness(options: Partial<QueueOptions> = {}): Harness {
  const clock = { now: T0 };
  const now = (): number => clock.now;
  const api = new FakeShelfyApi();
  const storage = memoryStorage();
  const store = new SettingsStore(storage);
  const network = { down: false };
  const fetchApi = api.fetchHandler(ORIGIN);
  const client = new ApiClient({
    origin: ORIGIN,
    version: EXTENSION_VERSION,
    fetch: async (input, init) => {
      if (network.down) throw new TypeError('Failed to fetch');
      return fetchApi(input, init);
    },
    credentials: async () => {
      const [pairing, settings] = await Promise.all([store.pairing(), store.settings()]);
      return { token: pairing?.token ?? null, access: settings.access };
    },
    now,
  });
  const queueStore = new MemoryQueueStore();
  let seed = 0;
  const queue = new Queue(queueStore, {
    client: { ext: EXTENSION_VERSION, parser: 'test-parser' },
    ulid: createUlid(now, (length) => new Uint8Array(length).fill(++seed % 256)),
    ...options,
  });
  const config = new ConfigService({ api: client, store, version: EXTENSION_VERSION, now });
  const wakes: number[] = [];
  const changes = { count: 0 };
  const uploader = new Uploader({
    queue,
    api: client,
    store,
    config,
    now,
    random: () => 0.5,
    wakeAt: (at) => void wakes.push(at),
    changed: () => void (changes.count += 1),
  });
  return {
    api,
    storage,
    store,
    queueStore,
    queue,
    client,
    config,
    uploader,
    clock,
    wakes,
    changes,
    network,
    async pairNow() {
      const token = api.mintToken();
      await store.setPairing({
        token,
        tokenId: 'tok-1',
        accountId: 'account-synthetic',
        scopes: ['ingest'],
        pairedAt: clock.now,
      });
      return token;
    },
  };
}
