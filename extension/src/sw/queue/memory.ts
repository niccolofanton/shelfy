// In-memory QueueStore with IndexedDB's semantics where the queue relies on them: values are
// copied in and out (structured clone), transactions run one at a time, and a transaction that
// throws leaves nothing behind. Used by the unit tests; the worker uses sw/queue/idb.ts.

import {
  emptyMeta,
  type Batch,
  type Chunk,
  type QueueMeta,
  type QueueStore,
  type QueueTx,
  type Run,
} from './types';

interface State {
  nextSeq: number;
  chunks: Map<number, Chunk>;
  batches: Map<string, Batch>;
  runs: Map<string, Run>;
  meta: QueueMeta;
}

const copy = <T>(value: T): T => structuredClone(value);

export class MemoryQueueStore implements QueueStore {
  private state: State = {
    nextSeq: 1,
    chunks: new Map(),
    batches: new Map(),
    runs: new Map(),
    meta: emptyMeta(),
  };
  private chain: Promise<unknown> = Promise.resolve();
  /** Transactions run so far (tests use it to check that work is batched). */
  transactions = 0;

  transaction<T>(body: (tx: QueueTx) => Promise<T>): Promise<T> {
    const run = this.chain.then(async () => {
      this.transactions += 1;
      const before = copy(this.state);
      try {
        return await body(this.tx());
      } catch (err) {
        this.state = before;
        throw err;
      }
    });
    this.chain = run.catch(() => undefined);
    return run;
  }

  /** A copy of everything stored (tests inspect it). */
  dump(): { chunks: Chunk[]; batches: Batch[]; runs: Run[]; meta: QueueMeta } {
    return copy({
      chunks: [...this.state.chunks.values()],
      batches: [...this.state.batches.values()].sort((a, b) => (a.id < b.id ? -1 : 1)),
      runs: [...this.state.runs.values()],
      meta: this.state.meta,
    });
  }

  private tx(): QueueTx {
    const state = (): State => this.state;
    return {
      meta: async () => copy(state().meta),
      putMeta: async (meta) => void (state().meta = copy(meta)),
      addChunk: async (chunk) => {
        const seq = state().nextSeq++;
        state().chunks.set(seq, copy({ ...chunk, seq }));
        return seq;
      },
      groupChunks: async (group) =>
        [...state().chunks.values()]
          .filter((chunk) => chunk.group === group)
          .sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0))
          .map(copy),
      firstChunk: async () => {
        const seqs = [...state().chunks.keys()].sort((a, b) => a - b);
        return seqs.length ? copy(state().chunks.get(seqs[0]) as Chunk) : null;
      },
      deleteChunk: async (seq) => void state().chunks.delete(seq),
      putBatch: async (batch) => void state().batches.set(batch.id, copy(batch)),
      getBatch: async (id) => {
        const batch = state().batches.get(id);
        return batch ? copy(batch) : null;
      },
      deleteBatch: async (id) => void state().batches.delete(id),
      firstBatch: async () => {
        const ids = [...state().batches.keys()].sort();
        return ids.length ? copy(state().batches.get(ids[0]) as Batch) : null;
      },
      runBatchIds: async (runId) =>
        [...state().batches.values()]
          .filter((batch) => batch.runId === runId)
          .map((batch) => batch.id)
          .sort(),
      getRun: async (id) => {
        const run = state().runs.get(id);
        return run ? copy(run) : null;
      },
      putRun: async (run) => void state().runs.set(run.id, copy(run)),
      deleteRun: async (id) => void state().runs.delete(id),
      runs: async () => [...state().runs.values()].map(copy),
    };
  }
}
