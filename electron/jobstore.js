// Shared persistence layer for the in-memory background queues
// (downloader / analyzer / weborchestrator). Those three managers are near
// identical clones: every state transition flows through their setJob(), so a
// single mirror() call there keeps a durable copy of each job in the `jobs`
// table. At boot each manager calls recover() and re-enqueues whatever resumable()
// returns, so a crash or quit mid-queue no longer loses work.
//
// This module owns the job-record <-> row mapping and the resumable/terminal
// policy; db.js owns the raw SQL (jobUpsert/jobsUpsertMany/jobDelete/
// jobsDeleteMany/jobDeleteAll/jobsByKind).

const db = require('./db');

// Terminal states are never resumed at boot. 'error' rows are kept (so the queue
// UI still shows the failure) but not auto-retried — a permanently failing job
// would otherwise restart on every launch.
const TERMINAL = new Set(['done', 'cancelled', 'error']);

// Heavy, regenerable fields we don't want to serialise on every transition. The
// web pipeline appends to `events` constantly and rebuilds `pages` on resume; the
// analyzer rewrites the growing `streamText` on every token (persisting it would
// be O(n²) in tokens). None of them is needed to resume a job.
const HEAVY_KEYS = new Set(['events', 'pages', 'logs', 'streamText']);

function compactPayload(job) {
  const out = {};
  for (const [k, v] of Object.entries(job)) {
    if (!HEAVY_KEYS.has(k)) out[k] = v;
  }
  return out;
}

// Last persisted signature (everything except `progress`) per kind:key. A
// transition whose only difference is `progress` skips the SQLite write: every
// queue's recover() restarts work from zero, so durable progress is dead weight
// and persisting it per yt-dlp line was a synchronous commit per line.
const lastPersisted = new Map();

function cacheKey(kind, key) {
  return `${kind}\u0000${key}`;
}

function persistSignature(payload) {
  return JSON.stringify({ ...payload, progress: undefined });
}

function toRow(kind, job, payloadJson) {
  return {
    kind,
    key: job.key,
    post_id: job.postId ?? null,
    payload: payloadJson,
    status: job.status ?? 'pending',
    progress: typeof job.progress === 'number' ? job.progress : 0,
    error: job.error ?? null,
    attempts: typeof job.attempts === 'number' ? job.attempts : 0,
  };
}

// Mirror a job record into the jobs table. Best-effort: a persistence failure
// must never take down a live job, so callers wrap this and we also guard here.
function mirror(kind, job) {
  if (!job || !job.key) return;
  const payload = compactPayload(job);
  const sig = persistSignature(payload);
  const ck = cacheKey(kind, job.key);
  if (lastPersisted.get(ck) === sig) return; // progress-only change
  try {
    db.jobUpsert(toRow(kind, job, JSON.stringify(payload)));
    lastPersisted.set(ck, sig);
  } catch (e) {
    console.warn(`[jobstore] mirror ${kind}:${job.key} failed:`, e?.message);
  }
}

// Batch mirror for bulk paths (enqueueMany/cancelAll/recover): a single
// transaction instead of one implicit commit per job.
function mirrorMany(kind, jobs) {
  const rows = [];
  const sigs = [];
  for (const job of jobs || []) {
    if (!job || !job.key) continue;
    const payload = compactPayload(job);
    const sig = persistSignature(payload);
    const ck = cacheKey(kind, job.key);
    if (lastPersisted.get(ck) === sig) continue;
    rows.push(toRow(kind, job, JSON.stringify(payload)));
    sigs.push([ck, sig]);
  }
  if (rows.length === 0) return;
  try {
    db.jobsUpsertMany(rows);
    for (const [ck, sig] of sigs) lastPersisted.set(ck, sig);
  } catch (e) {
    console.warn(`[jobstore] mirrorMany ${kind} failed:`, e?.message);
  }
}

function forget(kind, key) {
  lastPersisted.delete(cacheKey(kind, key));
  try {
    db.jobDelete(kind, key);
  } catch (e) {
    console.warn(`[jobstore] forget ${kind}:${key} failed:`, e?.message);
  }
}

function forgetMany(kind, keys) {
  const list = Array.from(keys || []).filter(Boolean);
  if (list.length === 0) return;
  for (const key of list) lastPersisted.delete(cacheKey(kind, key));
  try {
    db.jobsDeleteMany(kind, list);
  } catch (e) {
    console.warn(`[jobstore] forgetMany ${kind} failed:`, e?.message);
  }
}

function forgetAll(kind) {
  const prefix = `${kind}\u0000`;
  for (const ck of lastPersisted.keys()) {
    if (ck.startsWith(prefix)) lastPersisted.delete(ck);
  }
  try {
    db.jobDeleteAll(kind);
  } catch (e) {
    console.warn(`[jobstore] forgetAll ${kind} failed:`, e?.message);
  }
}

// Crash-safe variant of forget for boot recovery: instead of deleting a row
// BEFORE re-enqueuing it (which loses the job if the app dies in that window, or
// if the re-enqueue bails out), recover() should re-enqueue FIRST — the new
// setJob/mirror upserts the same key — then call forgetExcept() with the set of
// keys that were successfully re-enqueued. Only the stale rows that were NOT
// replaced (e.g. a post that no longer exists) are dropped; everything resumable
// keeps its durable row until a fresh mirror overwrites it.
function forgetExcept(kind, keepKeys) {
  const keep = keepKeys instanceof Set ? keepKeys : new Set(keepKeys || []);
  let rows;
  try {
    rows = db.jobsByKind(kind);
  } catch (e) {
    console.warn(`[jobstore] forgetExcept ${kind} failed:`, e?.message);
    return;
  }
  const drop = [];
  for (const row of rows) {
    if (TERMINAL.has(row.status)) continue; // keep terminal rows per policy
    if (keep.has(row.key)) continue; // re-enqueued → its fresh mirror stands
    drop.push(row.key);
  }
  forgetMany(kind, drop);
}

// Returns the non-terminal jobs for a kind, each as its deserialised payload
// (the compact job record). These are what recover() should re-enqueue.
function resumable(kind) {
  let rows;
  try {
    rows = db.jobsByKind(kind);
  } catch (e) {
    console.warn(`[jobstore] resumable ${kind} failed:`, e?.message);
    return [];
  }
  const out = [];
  for (const row of rows) {
    if (TERMINAL.has(row.status)) continue;
    let job;
    try {
      job = row.payload ? JSON.parse(row.payload) : {};
    } catch {
      job = {};
    }
    out.push({ ...job, key: row.key, postId: row.post_id ?? job.postId, status: row.status });
  }
  return out;
}

module.exports = {
  mirror,
  mirrorMany,
  forget,
  forgetMany,
  forgetAll,
  forgetExcept,
  resumable,
  TERMINAL,
};
