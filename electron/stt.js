'use strict';

// Local speech-to-text with whisper.cpp (no cloud, no Python). Mirrors the
// llama-server lifecycle in analyzer.js: a `whisper-server` binary is spawned on
// a free loopback port, the model weights are downloaded on first use, and the
// renderer posts captured audio (16kHz mono WAV) to /inference to get text back.
//
// Like the VLM analyzer, the model is a swappable preset: every entry in MODELS
// is a whisper.cpp GGML file served by the same whisper-server binary over the
// same /inference API, so switching model = swapping which .bin gets loaded.

const { app } = require('electron');
const path = require('path');
const fs = require('fs');
const { spawn } = require('child_process');
const { firstExisting, freePort, waitForHttp, downloadFile } = require('./serverUtils');
const hardware = require('./hardware');

// ─── Constants ────────────────────────────────────────────────────────────────

const SERVER_IDLE_MS = 5 * 60_000;  // shut the server down after this idle period
const HEALTH_TIMEOUT_MS = 60_000;   // small model loads quickly, but allow cold disk
const INFER_TIMEOUT_MS = 30_000;    // abort a single transcription if it stalls
const KILL_GRACE_MS = 5_000;        // SIGTERM → SIGKILL fallback window

// Terminate a child gracefully, escalating to SIGKILL if it ignores SIGTERM so
// whisper-server can't linger as a zombie holding its loopback port.
function killChild(child) {
  if (!child) return;
  try { child.kill('SIGTERM'); } catch {}
  const t = setTimeout(() => { try { child.kill('SIGKILL'); } catch {} }, KILL_GRACE_MS);
  child.once('close', () => clearTimeout(t));
  child.once('exit', () => clearTimeout(t));
}

const HF_BASE = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main';
const DEFAULT_LANGUAGE = 'it';

// ─── Model registry ─────────────────────────────────────────────────────────────
// All multilingual (suited to Italian dictation). Each is a single GGML file from
// the official whisper.cpp repo, drop-in on the same whisper-server binary.
const MODELS = {
  'whisper-base': {
    id: 'whisper-base',
    name: 'Whisper Base',
    tier: 'Veloce',
    file: 'ggml-base.bin',
    sizeGB: 0.14,
    sizeLabel: '142 MB',
    note: 'Più leggero e veloce dello Small, qualità inferiore. Per macchine deboli.',
  },
  'whisper-small': {
    id: 'whisper-small',
    name: 'Whisper Small',
    tier: 'Bilanciato',
    file: 'ggml-small.bin',
    sizeGB: 0.47,
    sizeLabel: '466 MB',
    note: 'Buon equilibrio fra qualità e velocità.',
  },
  'whisper-turbo-q5': {
    id: 'whisper-turbo-q5',
    name: 'Whisper Large v3 Turbo (q5)',
    tier: 'Qualità leggera',
    recommended: true,
    file: 'ggml-large-v3-turbo-q5_0.bin',
    sizeGB: 0.55,
    sizeLabel: '547 MB',
    note: 'Stessa stazza dello Small ma qualità nettamente superiore. Consigliato.',
  },
  'whisper-turbo': {
    id: 'whisper-turbo',
    name: 'Whisper Large v3 Turbo',
    tier: 'Qualità',
    file: 'ggml-large-v3-turbo.bin',
    sizeGB: 1.5,
    sizeLabel: '1.5 GB',
    note: 'Qualità alta, ~6× più veloce del large-v3. Per macchine con buone risorse.',
  },
};
const DEFAULT_MODEL_ID = 'whisper-small';

// Persisted selection lives in a tiny JSON file in userData. Cached in-process;
// falls back to the default on any read/parse error.
function configPath() {
  return path.join(app.getPath('userData'), 'stt-model.json');
}

// Read-modify-write so independent settings (modelId, threads) don't clobber each
// other when persisted separately.
function readConfig() {
  try { return JSON.parse(fs.readFileSync(configPath(), 'utf8')) || {}; }
  catch { return {}; }
}
function writeConfig(patch) {
  try {
    fs.mkdirSync(app.getPath('userData'), { recursive: true });
    fs.writeFileSync(configPath(), JSON.stringify({ ...readConfig(), ...patch }), 'utf8');
  } catch (e) {
    console.warn('[stt] could not persist config:', e.message);
  }
}

let _selectedId = null;

function getSelectedModelId() {
  if (_selectedId && MODELS[_selectedId]) return _selectedId;
  try {
    const id = JSON.parse(fs.readFileSync(configPath(), 'utf8'))?.modelId;
    if (id && MODELS[id]) return (_selectedId = id);
  } catch {}
  return (_selectedId = DEFAULT_MODEL_ID);
}

function getSelectedModel() {
  return MODELS[getSelectedModelId()];
}

// Switches the active model: persists the choice and tears down any running
// server so the next transcription respawns with the new weights.
function setModel(id) {
  if (!MODELS[id]) throw new Error('Unknown STT model id: ' + id);
  _selectedId = id;
  writeConfig({ modelId: id });
  shutdown();
  return getStatus();
}

// ─── Performance tuning ─────────────────────────────────────────────────────────
// whisper-server only needs a thread count (it auto-uses its compiled GPU backend;
// there is no -ngl, and flash-attn is intentionally NOT enabled — it degrades
// non-English transcription, and our dictation is Italian). Default from the
// detected hardware; overridable from Settings, persisted as `threads` ('auto' or
// a positive integer).
let _threads = null;

function getThreadsOverride() {
  if (_threads !== null) return _threads;
  const raw = readConfig().threads;
  if (raw == null || raw === 'auto') return (_threads = 'auto');
  const n = Math.floor(Number(raw));
  return (_threads = Number.isFinite(n) ? Math.max(1, Math.min(64, n)) : 'auto');
}

function resolveThreads() {
  const override = getThreadsOverride();
  return override === 'auto' ? hardware.computeWhisperTuning().threads : override;
}

// Resolved tuning for the UI: { effective, auto, override }.
function getTuning() {
  return {
    effective: resolveThreads(),
    auto: hardware.computeWhisperTuning().threads,
    override: getThreadsOverride(),
  };
}

function setTuning({ threads } = {}) {
  if (threads !== undefined) {
    _threads = threads == null || threads === 'auto'
      ? 'auto'
      : Math.max(1, Math.min(64, Math.floor(Number(threads)) || 1));
    writeConfig({ threads: _threads });
    shutdown(); // thread count is baked in at spawn → respawn to apply
  }
  return getTuning();
}

// ─── Binary + model resolution (dev vs packaged) ────────────────────────────────

function resolveWhisperServer() {
  const exe = process.platform === 'win32' ? 'whisper-server.exe' : 'whisper-server';
  const candidates = [
    process.env.WHISPER_SERVER_BIN,
    path.join(app.getPath('userData'), 'runtime-bin', 'whisper', exe),
    path.join(process.resourcesPath || '', 'whisper', exe),
    path.join(__dirname, '..', '.vlm', 'whisper', exe),
  ];
  const found = firstExisting(candidates);
  if (!found) throw new Error('whisper-server binary not found');
  return found;
}

// The binary must be supplied by the dev/build (not downloaded at runtime),
// mirroring llama-server. Probed so the UI can tell "binary missing" apart from
// "model not downloaded yet".
function binaryAvailable() {
  try { resolveWhisperServer(); return true; } catch { return false; }
}

// Whisper models are single files; they share userData/models with the VLM
// presets but never collide (unique ggml-*.bin names vs the VLM's per-id subdirs).
function getModelDir() {
  return path.join(app.getPath('userData'), 'models');
}

function modelPath(id = getSelectedModelId()) {
  return path.join(getModelDir(), MODELS[id].file);
}

function isModelReady(id) {
  return !!MODELS[id] && fs.existsSync(modelPath(id));
}

// ─── Download state ─────────────────────────────────────────────────────────────

let modelDownloading = false;
let _downloadingId = null;   // model id whose download is currently in flight
let _downloadAbort = null;   // AbortController for the in-flight download
let _downloadAction = null;  // null | 'pause' | 'cancel' — how an abort is treated

function getStatus() {
  const id = getSelectedModelId();
  const modelReady = isModelReady(id);
  const binaryReady = binaryAvailable();
  return {
    modelReady,
    binaryReady,
    ready: modelReady && binaryReady,
    downloading: modelDownloading,
    downloadingId: _downloadingId,
    modelId: id,
    name: MODELS[id].name,
  };
}

// Catalog for the settings UI: every preset with its on-disk + active state.
function listModels() {
  const selected = getSelectedModelId();
  const binaryReady = binaryAvailable();
  return Object.values(MODELS).map((m) => {
    const ready = fs.existsSync(modelPath(m.id));
    const partial = !ready && fs.existsSync(`${modelPath(m.id)}.part`);
    return {
      id: m.id,
      name: m.name,
      tier: m.tier,
      note: m.note,
      sizeGB: m.sizeGB,
      sizeLabel: m.sizeLabel,
      recommended: !!m.recommended,
      ready,
      partial,
      active: m.id === selected,
      downloading: m.id === _downloadingId,
      binaryReady,
    };
  });
}

// Deletes the on-disk file (and any .part) for a model. Shuts the server down
// first if it's serving `id`.
function deleteModelFiles(id) {
  if (!MODELS[id]) return;
  if (server && server.modelId === id) shutdown();
  for (const p of [modelPath(id), `${modelPath(id)}.part`]) {
    try { fs.unlinkSync(p); } catch {}
  }
}

function deleteModel(id) {
  if (!MODELS[id]) throw new Error('Unknown STT model id: ' + id);
  if (modelDownloading && _downloadingId === id) {
    throw new Error('Cannot delete a model while it is downloading');
  }
  deleteModelFiles(id);
  return listModels();
}

// Pauses the in-flight download: aborts but keeps the .part so a later
// downloadModel() resumes from where it stopped.
function pauseModelDownload() {
  if (modelDownloading && _downloadAbort) {
    _downloadAction = 'pause';
    _downloadAbort.abort();
    return { paused: true };
  }
  return getStatus();
}

// Cancels a download and discards partial progress. Aborts the in-flight one
// when it matches `id` (or `id` is omitted); otherwise wipes a paused partial
// for `id`. Never touches a fully-ready model.
function cancelModelDownload(id) {
  if (modelDownloading && _downloadAbort && (!id || id === _downloadingId)) {
    _downloadAction = 'cancel';
    _downloadAbort.abort();
    return { canceled: true };
  }
  const target = id || getSelectedModelId();
  if (MODELS[target] && !isModelReady(target)) deleteModelFiles(target);
  return { canceled: true, ...getStatus() };
}

// Downloads the GGML file for `id` (defaults to the active model). onProgress
// (fraction, label). Runs in the background: the active model is independent, so
// dictation can keep using an already-downloaded model while this runs.
// Resumable and pause/cancel-able via its own AbortController.
async function downloadModel(id, onProgress) {
  if (typeof id === 'function') { onProgress = id; id = null; } // legacy (no-id) call
  if (modelDownloading) throw new Error('STT model download already in progress');
  if (id && !MODELS[id]) throw new Error('Unknown STT model id: ' + id);
  id = id || getSelectedModelId();
  modelDownloading = true;
  _downloadingId = id;
  _downloadAction = null;
  _downloadAbort = new AbortController();
  const signal = _downloadAbort.signal;
  try {
    fs.mkdirSync(getModelDir(), { recursive: true });
    const dest = modelPath(id);
    if (!fs.existsSync(dest)) {
      await downloadFile(`${HF_BASE}/${MODELS[id].file}`, dest, (f) => onProgress?.(f, 'voce'), signal, {
        keepPartialOnAbort: () => _downloadAction === 'pause',
        // TODO: no pinned SHA256 in MODELS yet; when added (MODELS[id].sha256),
        // it flows through here so downloadFile verifies integrity pre-rename.
        expectedSha: MODELS[id].sha256,
      });
    }
    onProgress?.(1, 'voce');
    return getStatus();
  } catch (err) {
    if (_downloadAction === 'cancel') {
      deleteModelFiles(id);
      return { canceled: true, ...getStatus() };
    }
    if (_downloadAction === 'pause') {
      return { paused: true, ...getStatus() };
    }
    throw err;
  } finally {
    modelDownloading = false;
    _downloadingId = null;
    _downloadAbort = null;
    _downloadAction = null;
  }
}

// ─── whisper-server lifecycle ───────────────────────────────────────────────────

let server = null;     // { child, port, ready: Promise<void>, modelId }
let serverStarting = null; // in-flight ensureServer() promise (single-flight guard)
let idleTimer = null;

async function ensureServer() {
  const id = getSelectedModelId();
  // A server pinned to a different model must be torn down so the new weights
  // get loaded (the active model changed via setModel).
  if (server && server.modelId !== id) shutdown();
  if (server) return server;
  // Coalesce concurrent callers onto a single spawn so two transcriptions
  // arriving together don't each launch a whisper-server.
  if (serverStarting) return serverStarting;
  if (!getStatus().ready) throw new Error('STT_MODEL_NOT_READY');

  serverStarting = (async () => {
    const bin = resolveWhisperServer();
    const port = await freePort();
    const child = spawn(bin, [
      '--model', modelPath(id),
      '--host', '127.0.0.1', '--port', String(port),
      '-t', String(resolveThreads()),
      // whisper-server enables flash-attn by default, but it degrades transcription
      // quality on non-English languages (whisper.cpp #3020) — and our dictation is
      // Italian (DEFAULT_LANGUAGE). Disable it: quality over the small speed gain.
      '--no-flash-attn',
    ], { stdio: ['ignore', 'ignore', 'pipe'] });

    child.stderr.setEncoding('utf8');
    child.stderr.on('data', () => {}); // drained; surfaced only on early exit

    // A failed spawn (ENOENT/EACCES) emits 'error' and would otherwise throw
    // an unhandled exception (or leave us blocked until HEALTH_TIMEOUT). Reject
    // the startup promise with the real reason instead.
    const spawnError = new Promise((_, reject) => {
      child.on('error', (e) => {
        killChild(child);
        reject(e);
      });
    });
    const ready = waitForHttp(port, '/', HEALTH_TIMEOUT_MS);
    child.on('exit', () => {
      if (server && server.child === child) server = null;
    });

    const candidate = { child, port, ready, modelId: id };
    try {
      await Promise.race([ready, spawnError]);
    } catch (e) {
      killChild(child);
      throw e;
    }
    server = candidate;
    // Arm the idle-shutdown timer as soon as the server is ready, not only on a
    // successful transcribe(): `stt:ensure` (and a dictation session that captures
    // no/too-little audio) can spawn the server without ever calling transcribe(),
    // which would otherwise leave the child + its RAM/port alive until app quit.
    touchServer();
    return server;
  })();

  try {
    return await serverStarting;
  } finally {
    serverStarting = null;
  }
}

function touchServer() {
  if (idleTimer) clearTimeout(idleTimer);
  idleTimer = setTimeout(() => shutdown(), SERVER_IDLE_MS);
}

function shutdown() {
  if (idleTimer) { clearTimeout(idleTimer); idleTimer = null; }
  if (server) {
    killChild(server.child);
    server = null;
  }
}

// Synchronous, immediate teardown for app quit: SIGKILL guarantees no orphaned
// whisper-server holds the port if the grace-period timer can't fire (process
// exiting).
function forceShutdown() {
  if (idleTimer) { clearTimeout(idleTimer); idleTimer = null; }
  if (server) {
    try { if (server.child) server.child.kill('SIGKILL'); } catch {}
    server = null;
  }
}

// ─── Transcription ──────────────────────────────────────────────────────────────

// Transcribes a 16kHz mono WAV buffer via whisper-server's /inference endpoint.
// `wav` is an ArrayBuffer/Buffer/Uint8Array coming over IPC from the renderer.
async function transcribe(wav, { language = DEFAULT_LANGUAGE } = {}) {
  const { port } = await ensureServer();
  touchServer();

  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), INFER_TIMEOUT_MS);
  try {
    const form = new FormData();
    form.append('file', new Blob([Buffer.from(wav)], { type: 'audio/wav' }), 'audio.wav');
    form.append('response_format', 'json');
    form.append('temperature', '0');
    if (language) form.append('language', language);

    const res = await fetch(`http://127.0.0.1:${port}/inference`, {
      method: 'POST', body: form, signal: ac.signal,
    });
    if (!res.ok) throw new Error(`whisper-server HTTP ${res.status}`);
    const data = await res.json().catch(() => ({}));
    const text = typeof data?.text === 'string' ? data.text.trim() : '';
    return { text };
  } finally {
    clearTimeout(timer);
  }
}

module.exports = {
  getStatus,
  listModels,
  setModel,
  downloadModel,
  pauseModelDownload,
  cancelModelDownload,
  deleteModel,
  ensureServer,
  transcribe,
  getTuning,
  setTuning,
  shutdown,
  forceShutdown,
};
