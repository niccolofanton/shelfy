// The queue's IndexedDB store (database `shelfy`, version 2; record shapes in types.ts). Each
// transaction spans every store, is read-write with strict durability (a capture is
// acknowledged to the page only once it is on disk), and transactions run one after the other.
// Inside a transaction the queue awaits only these request wrappers: a promise that resolves
// from a request's success event continues while the transaction is still active.

import {
  emptyMeta,
  normalizeMeta,
  normalizeRun,
  type Batch,
  type Chunk,
  type QueueMeta,
  type QueueStore,
  type QueueTx,
  type Run,
  type RunKeys,
} from './types';

export const QUEUE_DB_NAME = 'shelfy';
const QUEUE_DB_VERSION = 2;
const STORES = ['chunks', 'batches', 'runs', 'meta', 'runKeys'] as const;
const META_KEY = 'meta';

function request<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error('IndexedDB request failed'));
  });
}

export function openQueueDb(name = QUEUE_DB_NAME): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const open = indexedDB.open(name, QUEUE_DB_VERSION);
    open.onupgradeneeded = (event) => {
      const db = open.result;
      if (event.oldVersion < 1) {
        // Version 1 (P2-06): the queue.
        const chunks = db.createObjectStore('chunks', { keyPath: 'seq', autoIncrement: true });
        chunks.createIndex('group', 'group');
        const batches = db.createObjectStore('batches', { keyPath: 'id' });
        batches.createIndex('runId', 'runId');
        db.createObjectStore('runs', { keyPath: 'id' });
        db.createObjectStore('meta', { keyPath: 'key' });
      }
      if (event.oldVersion < 2) {
        // Version 2 (P2-13): the accepted keys of each run.
        const runKeys = db.createObjectStore('runKeys', { keyPath: 'id' });
        runKeys.createIndex('runId', 'runId');
        runKeys.createIndex('at', 'at');
      }
    };
    open.onsuccess = () => {
      const db = open.result;
      // Another context upgrading the database (a newer build): let it, and reopen later.
      db.onversionchange = () => db.close();
      resolve(db);
    };
    open.onerror = () => reject(open.error ?? new Error('could not open IndexedDB'));
  });
}

function wrap(tx: IDBTransaction): QueueTx {
  const chunks = tx.objectStore('chunks');
  const batches = tx.objectStore('batches');
  const runs = tx.objectStore('runs');
  const meta = tx.objectStore('meta');
  const runKeys = tx.objectStore('runKeys');
  return {
    meta: async () => {
      const value = await request(meta.get(META_KEY));
      return value === undefined ? emptyMeta() : normalizeMeta(value);
    },
    putMeta: async (value: QueueMeta) => void (await request(meta.put(value))),
    addChunk: async (chunk: Chunk) => {
      const { seq: _seq, ...record } = chunk;
      return Number(await request(chunks.add(record)));
    },
    groupChunks: async (group) =>
      (await request(chunks.index('group').getAll(IDBKeyRange.only(group)))) as Chunk[],
    firstChunk: async () => {
      const [first] = (await request(chunks.getAll(null, 1))) as Chunk[];
      return first ?? null;
    },
    deleteChunk: async (seq) => void (await request(chunks.delete(seq))),
    putBatch: async (batch: Batch) => void (await request(batches.put(batch))),
    getBatch: async (id) => ((await request(batches.get(id))) as Batch | undefined) ?? null,
    deleteBatch: async (id) => void (await request(batches.delete(id))),
    firstBatch: async () => {
      const [first] = (await request(batches.getAll(null, 1))) as Batch[];
      return first ?? null;
    },
    runBatchIds: async (runId) =>
      (await request(batches.index('runId').getAllKeys(IDBKeyRange.only(runId)))).map(String),
    getRun: async (id) => {
      const run = (await request(runs.get(id))) as Run | undefined;
      return run ? normalizeRun(run) : null;
    },
    putRun: async (run: Run) => void (await request(runs.put(run))),
    deleteRun: async (id) => void (await request(runs.delete(id))),
    runs: async () => ((await request(runs.getAll())) as Run[]).map(normalizeRun),
    putRunKeys: async (record: RunKeys) => void (await request(runKeys.put(record))),
    runKeys: async (runId) =>
      ((await request(runKeys.index('runId').getAll(IDBKeyRange.only(runId)))) as RunKeys[]).sort(
        (a, b) => a.at - b.at,
      ),
    pruneRunKeys: async (before) => {
      const ids = await request(
        runKeys.index('at').getAllKeys(IDBKeyRange.upperBound(before, true)),
      );
      for (const id of ids) await request(runKeys.delete(id));
      return ids.length;
    },
  };
}

export class IdbQueueStore implements QueueStore {
  private db: Promise<IDBDatabase> | null = null;
  private chain: Promise<unknown> = Promise.resolve();

  constructor(private readonly name = QUEUE_DB_NAME) {}

  private open(): Promise<IDBDatabase> {
    this.db ??= openQueueDb(this.name).catch((err: unknown) => {
      this.db = null;
      throw err;
    });
    return this.db;
  }

  transaction<T>(body: (tx: QueueTx) => Promise<T>): Promise<T> {
    const run = this.chain.then(() => this.run(body));
    this.chain = run.catch(() => undefined);
    return run;
  }

  private async run<T>(body: (tx: QueueTx) => Promise<T>): Promise<T> {
    const db = await this.open();
    let tx: IDBTransaction;
    try {
      tx = db.transaction([...STORES], 'readwrite', { durability: 'strict' });
    } catch {
      // The connection was closed (version change): open a new one once.
      this.db = null;
      tx = (await this.open()).transaction([...STORES], 'readwrite', { durability: 'strict' });
    }
    const done = new Promise<void>((resolve, reject) => {
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error ?? new Error('IndexedDB transaction failed'));
      tx.onabort = () => reject(tx.error ?? new Error('IndexedDB transaction aborted'));
    });
    let result: T;
    try {
      result = await body(wrap(tx));
    } catch (err) {
      done.catch(() => undefined);
      try {
        tx.abort();
      } catch {
        /* already finished */
      }
      throw err;
    }
    await done;
    return result;
  }
}
