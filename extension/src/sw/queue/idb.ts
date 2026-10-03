// The queue's IndexedDB store (database `shelfy`, version 1; record shapes in types.ts). Each
// transaction spans the four stores, is read-write with strict durability (a capture is
// acknowledged to the page only once it is on disk), and transactions run one after the other.
// Inside a transaction the queue awaits only these request wrappers: a promise that resolves
// from a request's success event continues while the transaction is still active.

import {
  emptyMeta,
  normalizeMeta,
  type Batch,
  type Chunk,
  type QueueMeta,
  type QueueStore,
  type QueueTx,
  type Run,
} from './types';

export const QUEUE_DB_NAME = 'shelfy';
const QUEUE_DB_VERSION = 1;
const STORES = ['chunks', 'batches', 'runs', 'meta'] as const;
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
    open.onupgradeneeded = () => {
      const db = open.result;
      // Version 1: created from scratch.
      const chunks = db.createObjectStore('chunks', { keyPath: 'seq', autoIncrement: true });
      chunks.createIndex('group', 'group');
      const batches = db.createObjectStore('batches', { keyPath: 'id' });
      batches.createIndex('runId', 'runId');
      db.createObjectStore('runs', { keyPath: 'id' });
      db.createObjectStore('meta', { keyPath: 'key' });
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
    getRun: async (id) => ((await request(runs.get(id))) as Run | undefined) ?? null,
    putRun: async (run: Run) => void (await request(runs.put(run))),
    deleteRun: async (id) => void (await request(runs.delete(id))),
    runs: async () => (await request(runs.getAll())) as Run[],
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
