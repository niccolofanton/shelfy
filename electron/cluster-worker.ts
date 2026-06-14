// worker_threads entry point for tag clustering. The label-propagation + per-edge
// cosine fusion in buildTagCommunities is pure CPU (no DB, no I/O) and, on large
// libraries (thousands of tags × 384-dim vectors), runs long enough to freeze the
// main process / IPC. Running it here keeps the UI responsive. db.js falls back to
// the in-process path if this worker can't start, so it is best-effort.
import { parentPort, workerData } from 'worker_threads';
import { buildTagCommunities, cosineSim } from './cluster-core';

// Shapes db.js serializes into workerData (Map → array of entries; vectors as
// number[]). Local to this file: they describe the cross-thread payload, not a
// persisted domain shape.
interface ClusterWorkerData {
  freq?: Array<[string, number]>;
  edges?: Array<{ a: string; b: string; c: number }>;
  vecByNorm?: Array<[string, number[]]> | null;
  opts?: Record<string, unknown>;
}

// This module only ever runs as a worker_threads entry, so parentPort is never
// null here (it is null only on the main thread). Asserting it keeps the original
// behavior — a throw if it were somehow null — instead of silently no-op'ing.
const port = parentPort!;

try {
  const { freq, edges, vecByNorm, opts } = (workerData as ClusterWorkerData | null) || {};
  const freqMap = new Map<string, number>(freq || []);
  // Rebuild cosSim from the per-tag vectors exactly as db.js does in-process, so
  // the worker output is identical to the fallback.
  let cosSim: ((a: string, b: string) => number) | null = null;
  if (vecByNorm && vecByNorm.length) {
    const vecMap = new Map<string, number[]>(vecByNorm);
    cosSim = (a: string, b: string): number => {
      const va = vecMap.get(a);
      const vb = vecMap.get(b);
      return va && vb ? cosineSim(va, vb) : 0;
    };
  }
  const groups = buildTagCommunities(freqMap, edges || [], { ...(opts || {}), cosSim });
  port.postMessage({ ok: true, groups });
} catch (err: unknown) {
  const message = err instanceof Error ? err.message : undefined;
  port.postMessage({ ok: false, error: String(message || err) });
}
