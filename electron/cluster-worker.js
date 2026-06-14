// worker_threads entry point for tag clustering. The label-propagation + per-edge
// cosine fusion in buildTagCommunities is pure CPU (no DB, no I/O) and, on large
// libraries (thousands of tags × 384-dim vectors), runs long enough to freeze the
// main process / IPC. Running it here keeps the UI responsive. db.js falls back to
// the in-process path if this worker can't start, so it is best-effort.
const { parentPort, workerData } = require('worker_threads');
const { buildTagCommunities, cosineSim } = require('./cluster-core');

try {
  const { freq, edges, vecByNorm, opts } = workerData || {};
  const freqMap = new Map(freq || []);
  // Rebuild cosSim from the per-tag vectors exactly as db.js does in-process, so
  // the worker output is identical to the fallback.
  let cosSim = null;
  if (vecByNorm && vecByNorm.length) {
    const vecMap = new Map(vecByNorm);
    cosSim = (a, b) => {
      const va = vecMap.get(a);
      const vb = vecMap.get(b);
      return va && vb ? cosineSim(va, vb) : 0;
    };
  }
  const groups = buildTagCommunities(freqMap, edges || [], { ...(opts || {}), cosSim });
  parentPort.postMessage({ ok: true, groups });
} catch (err) {
  parentPort.postMessage({ ok: false, error: String((err && err.message) || err) });
}
