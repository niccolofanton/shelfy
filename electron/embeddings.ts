// Embedding di testo in locale (no cloud, no Python). Rispecchia il lifecycle di
// analyzer.js / stt.js: un'istanza di `llama-server` viene avviata in modo lazy su
// una PORTA loopback separata da quella del VLM e dello STT, avviata con
// `--embedding`, e serve l'endpoint OpenAI-compatibile /v1/embeddings. Il modello è
// un piccolo embedder MULTILINGUE (multilingual-e5-small, GGUF) scaricato tramite
// l'infra modelli esistente (stessa convenzione di stt.js: file singolo in
// userData/models, download in background con pausa/ripresa).
//
// Il consumer è db.js (clustering tag P4): se il modello non è scaricato,
// isEmbeddingReady() torna false e embedTexts() lancia un errore chiaro, così il
// chiamante fa fallback a solo-jaccard.

import { app } from 'electron';
import path from 'path';
import fs from 'fs';
import { spawn } from 'child_process';
import type { ChildProcess } from 'child_process';
import { firstExisting, freePort, waitForHttp, downloadFile } from './serverUtils';
import * as hardware from './hardware';
import * as binaries from './binaries';

// Un singolo preset embedding: i campi base (id/file/url/sizeGB/dim) arrivano da
// binaries.EMBEDDING_MODEL; qui aggiungiamo i metadati di registro/UI. `sha256`
// non è ancora fissato (vedi downloadModel), perciò è opzionale.
interface ModelRecord {
  id: string;
  file: string;
  url: string;
  sizeGB: number;
  dim: number;
  name: string;
  tier: string;
  recommended: boolean;
  sizeLabel: string;
  note: string;
  sha256?: string;
}

// Stato del server embedding in volo: il child (null quando si riusa un server
// esterno via env), la porta loopback, la promise di readiness e il modello servito.
interface ServerState {
  child: ChildProcess | null;
  port: number;
  ready: Promise<void>;
  modelId: string;
}

// Snapshot di stato per la UI/IPC di Settings.
interface EmbeddingStatus {
  modelReady: boolean;
  binaryReady: boolean;
  ready: boolean;
  downloading: boolean;
  downloadingId: string | null;
  modelId: string;
  name: string;
}

// Una riga del catalogo modelli per la UI (listModels).
interface ModelListEntry {
  id: string;
  name: string;
  tier: string;
  note: string;
  sizeGB: number;
  sizeLabel: string;
  recommended: boolean;
  ready: boolean;
  partial: boolean;
  active: boolean;
  downloading: boolean;
  binaryReady: boolean;
}

// Forma della risposta OpenAI-compatibile di /v1/embeddings.
interface EmbeddingResponseItem {
  index?: number;
  embedding?: number[];
}
interface EmbeddingResponse {
  data?: EmbeddingResponseItem[];
}

// ─── Costanti ─────────────────────────────────────────────────────────────────

const SERVER_IDLE_MS = 5 * 60_000; // spegne il server dopo questo periodo di inattività
const HEALTH_TIMEOUT_MS = 60_000; // l'embedder è piccolo, ma il disco freddo può essere lento
const INFER_TIMEOUT_MS = 60_000; // aborta una singola richiesta di embedding se si blocca
const KILL_GRACE_MS = 5_000; // finestra SIGTERM → SIGKILL
const EMBED_BATCH = 32; // quanti testi per richiesta HTTP (batching ragionevole)
const CTX_SIZE = 2048; // contesto del server embedding (i tag/forme sono brevi)

// e5 richiede un prefisso esplicito sul testo ("query:" / "passage:"); per un uso
// simmetrico (similarità tag↔tag nel clustering) si usa lo stesso prefisso ovunque.
// Vedi intfloat/multilingual-e5-small.
const E5_PREFIX = 'query: ';

// Termina un child con escalation a SIGKILL se ignora SIGTERM, così il server non
// resta zombie a tenere la sua porta loopback.
function killChild(child: ChildProcess | null): void {
  if (!child) return;
  try {
    child.kill('SIGTERM');
  } catch {}
  const t = setTimeout(() => {
    try {
      child.kill('SIGKILL');
    } catch {}
  }, KILL_GRACE_MS);
  child.once('close', () => clearTimeout(t));
  child.once('exit', () => clearTimeout(t));
}

// ─── Registro modello ───────────────────────────────────────────────────────────
// Un solo preset: multilingual-e5-small (GGUF, q8_0). È genuinamente multilingue
// (copre bene l'italiano dei tag), leggerissimo (~132 MB) e gira sullo stesso
// binario llama-server (b9500) con --embedding — nessun cambio di binario/API.
const MODELS: Record<string, ModelRecord> = {
  'e5-small': {
    // Coordinate di file/URL/dim/size dalla source-of-truth in binaries.js
    // (EMBEDDING_MODEL), così registry e tooling di provisioning restano allineati.
    ...binaries.EMBEDDING_MODEL,
    name: 'Multilingual E5 Small',
    tier: 'Embedding',
    recommended: true,
    sizeLabel: '132 MB',
    note: 'Embedder multilingue (incl. italiano) per raggruppare tag simili. Leggerissimo.',
  },
};
const DEFAULT_MODEL_ID = 'e5-small';

// Un solo modello: la selezione è fissa. Manteniamo l'accessor per coerenza con
// analyzer/stt e per eventuali preset futuri.
function getSelectedModelId(): string {
  return DEFAULT_MODEL_ID;
}
function getSelectedModel(): ModelRecord {
  return MODELS[getSelectedModelId()];
}

// ─── Risoluzione binario + modello (dev vs packaged) ────────────────────────────
// Riusa lo stesso llama-server dell'analyzer (stessi candidati di percorso).
function resolveLlamaServer(): string {
  const exe = process.platform === 'win32' ? 'llama-server.exe' : 'llama-server';
  const candidates = [
    process.env.LLAMA_SERVER_BIN,
    path.join(app.getPath('userData'), 'runtime-bin', 'llama', exe),
    path.join(process.resourcesPath || '', 'llama', exe),
    path.join(__dirname, '..', '.vlm', 'llama-b9500', exe),
    path.join(__dirname, '..', '.vlm', 'llama-b9500-win', exe),
  ];
  const found = firstExisting(candidates);
  if (!found) throw new Error('llama-server binary not found');
  return found;
}

function binaryAvailable(): boolean {
  try {
    resolveLlamaServer();
    return true;
  } catch {
    return false;
  }
}

// I modelli embedding sono file singoli; condividono userData/models con i preset
// VLM/STT ma non collidono (nome file univoco vs sottocartelle per-id del VLM).
function getModelDir(): string {
  return path.join(app.getPath('userData'), 'models');
}

function modelPath(id: string = getSelectedModelId()): string {
  return path.join(getModelDir(), MODELS[id].file);
}

// Whether the model's weight file is present on disk.
function isModelReady(id: string = getSelectedModelId()): boolean {
  return !!MODELS[id] && fs.existsSync(modelPath(id));
}

// API pubblica richiesta dal contratto (SEAM 3): true se il modello di embedding è
// scaricato/disponibile. Non avvia nulla, non blocca.
function isEmbeddingReady(): boolean {
  return isModelReady(getSelectedModelId());
}

// ─── Stato download ─────────────────────────────────────────────────────────────

let modelDownloading = false;
let _downloadingId: string | null = null; // id del modello il cui download è in corso
let _downloadAbort: AbortController | null = null; // AbortController per il download in corso
let _downloadAction: 'pause' | 'cancel' | null = null; // null | 'pause' | 'cancel' — come trattare un abort

function getStatus(): EmbeddingStatus {
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

// Catalogo per la UI di Settings: ogni preset con lo stato on-disk + attivo.
function listModels(): ModelListEntry[] {
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
      active: m.id === selected, // unico modello → sempre attivo
      downloading: m.id === _downloadingId,
      binaryReady,
    };
  });
}

// C'è un solo modello: setModel è un no-op coerente (la riga UI lo mostra sempre
// attivo). Manteniamo la firma per parità con analyzer/stt.
function setModel(id: string): EmbeddingStatus {
  if (!MODELS[id]) throw new Error('Unknown embedding model id: ' + id);
  return getStatus();
}

// Cancella il file on-disk (e l'eventuale .part) per un modello. Spegne prima il
// server se sta servendo `id`.
function deleteModelFiles(id: string): void {
  if (!MODELS[id]) return;
  if (server && server.modelId === id) shutdown();
  for (const p of [modelPath(id), `${modelPath(id)}.part`]) {
    try {
      fs.unlinkSync(p);
    } catch {}
  }
}

function deleteModel(id: string): ModelListEntry[] {
  if (!MODELS[id]) throw new Error('Unknown embedding model id: ' + id);
  if (modelDownloading && _downloadingId === id) {
    throw new Error('Cannot delete a model while it is downloading');
  }
  deleteModelFiles(id);
  return listModels();
}

// Mette in pausa il download in corso: aborta ma mantiene il .part così un
// successivo downloadModel() riprende da dove si era fermato.
function pauseModelDownload(): { paused: boolean } | EmbeddingStatus {
  if (modelDownloading && _downloadAbort) {
    _downloadAction = 'pause';
    _downloadAbort.abort();
    return { paused: true };
  }
  return getStatus();
}

// Annulla un download e scarta il progresso parziale. Aborta quello in corso se
// corrisponde a `id` (o se `id` è omesso); altrimenti ripulisce un parziale in
// pausa per `id`. Non tocca mai un modello già pronto.
function cancelModelDownload(id?: string | null): { canceled: boolean } & Partial<EmbeddingStatus> {
  if (modelDownloading && _downloadAbort && (!id || id === _downloadingId)) {
    _downloadAction = 'cancel';
    _downloadAbort.abort();
    return { canceled: true };
  }
  const target = id || getSelectedModelId();
  if (MODELS[target] && !isModelReady(target)) deleteModelFiles(target);
  return { canceled: true, ...getStatus() };
}

// Risultato di downloadModel: lo stato base, eventualmente marcato canceled/paused.
type DownloadResult =
  | EmbeddingStatus
  | ({ canceled: boolean } & Partial<EmbeddingStatus>)
  | ({ paused: boolean } & Partial<EmbeddingStatus>);

// Callback di progresso del download: frazione 0..1 + label di fase.
type ProgressFn = (fraction: number, label: string) => void;

// Scarica il file GGUF per `id` (default: il modello attivo). onProgress(fraction,
// label). Gira in background: l'attivo è indipendente, così altre operazioni
// possono usare un modello già scaricato mentre questo gira. Riprendibile e
// pausabile/annullabile tramite il proprio AbortController.
async function downloadModel(
  id?: string | ProgressFn | null,
  onProgress?: ProgressFn,
): Promise<DownloadResult> {
  if (typeof id === 'function') {
    onProgress = id;
    id = null;
  } // chiamata legacy (no-id)
  if (modelDownloading) throw new Error('Embedding model download already in progress');
  if (id && !MODELS[id]) throw new Error('Unknown embedding model id: ' + id);
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
      await downloadFile(MODELS[id].url, dest, (f) => onProgress?.(f, 'embedding'), signal, {
        keepPartialOnAbort: () => _downloadAction === 'pause',
        expectedSha: MODELS[id].sha256, // non ancora fissato; flow pronto se aggiunto
      });
    }
    onProgress?.(1, 'embedding');
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

// ─── Tuning (thread, hardware-aware) ────────────────────────────────────────────
// Un embedder gira interamente in prefill e di norma su CPU (modello minuscolo).
// Serve solo un thread count; lo deriviamo dall'hardware come whisper.
function resolveThreads(): number {
  try {
    return hardware.computeEmbeddingTuning().threads;
  } catch {
    return Math.max(1, Math.min(8, require('os').cpus()?.length || 4));
  }
}

// ─── Lifecycle llama-server (--embedding) ───────────────────────────────────────

let server: ServerState | null = null; // { child, port, ready, modelId }
let serverStarting: Promise<ServerState> | null = null; // promise ensureServer() in volo (single-flight guard)
let idleTimer: ReturnType<typeof setTimeout> | null = null;
let inflightCount = 0; // richieste di embedding in volo: l'idle non spegne mid-flight

async function ensureServer(): Promise<ServerState> {
  const id = getSelectedModelId();
  // Seam di test/eval: riusa un llama-server embedding esterno già avviato
  // (SHELFY_EXTERNAL_EMBED_PORT) invece di spawnarne uno per processo.
  const extPort = process.env.SHELFY_EXTERNAL_EMBED_PORT;
  if (extPort) {
    if (!server || server.port !== Number(extPort)) {
      server = { child: null, port: Number(extPort), ready: Promise.resolve(), modelId: id };
    }
    return server;
  }
  // Un server agganciato a un modello diverso va smontato (cambio modello futuro).
  if (server && server.modelId !== id) shutdown();
  if (server) return server;
  // Coalescing: due chiamate concorrenti non devono spawnare due server.
  if (serverStarting) return serverStarting;
  if (!getStatus().ready) throw new Error('EMBEDDING_MODEL_NOT_READY');

  serverStarting = (async () => {
    const bin = resolveLlamaServer();
    const port = await freePort();
    // Flag coerenti con la convenzione b9500 dell'analyzer: --embedding attiva il
    // pooling, -t i thread, --no-warmup salta il warmup (inutile per l'embedder).
    // Niente -ngl: il modello è minuscolo e gira bene su CPU; lasciamo a
    // llama-server il default. KV/flash-attn non servono per il solo embedding.
    const child = spawn(
      bin,
      [
        '--model',
        modelPath(id),
        '--embedding',
        '--host',
        '127.0.0.1',
        '--port',
        String(port),
        '-c',
        String(CTX_SIZE),
        '-t',
        String(resolveThreads()),
        '--no-warmup',
      ],
      { stdio: ['ignore', 'ignore', 'pipe'] },
    );

    child.stderr!.setEncoding('utf8');
    const stderrRing: string[] = [];
    child.stderr!.on('data', (d: string) => {
      stderrRing.push(d);
      if (stderrRing.length > 50) stderrRing.shift();
    });

    // Uno spawn fallito (ENOENT/EACCES) emette 'error': rigetta la promise di
    // startup con la causa reale invece di restare bloccati fino all'health timeout.
    const spawnError = new Promise<never>((_, reject) => {
      child.on('error', (e) => {
        killChild(child);
        reject(e);
      });
    });
    const ready = waitForHttp(port, '/health', HEALTH_TIMEOUT_MS);
    child.on('exit', () => {
      if (server && server.child === child) server = null;
    });

    const candidate: ServerState = { child, port, ready, modelId: id };
    try {
      await Promise.race([ready, spawnError]);
    } catch (e) {
      killChild(child);
      const tail = stderrRing.join('').trim().slice(-2000);
      throw new Error(
        `Avvio del server embedding non riuscito.${tail ? `\n--- stderr ---\n${tail}` : ''}`,
      );
    }
    server = candidate;
    return server;
  })();

  try {
    return await serverStarting;
  } finally {
    serverStarting = null;
  }
}

function touchServer(): void {
  if (idleTimer) clearTimeout(idleTimer);
  idleTimer = setTimeout(() => {
    if (inflightCount === 0) shutdown();
  }, SERVER_IDLE_MS);
  // L'idle timer non deve mai tenere vivo l'event loop: l'uscita dell'app
  // (forceShutdown) lo azzera comunque, ma unref evita di bloccare il quit.
  idleTimer.unref?.();
}

function shutdown(): void {
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  if (server) {
    killChild(server.child);
    server = null;
  }
}

// Teardown sincrono immediato per l'uscita dell'app: SIGKILL garantisce nessun
// server orfano che tiene la porta se il timer di grazia non può scattare.
function forceShutdown(): void {
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  if (server) {
    try {
      if (server.child) server.child.kill('SIGKILL');
    } catch {}
    server = null;
  }
}

function getServerStatus(): { running: boolean; port: number | null; modelId: string | null } {
  return { running: !!server, port: server?.port ?? null, modelId: server?.modelId ?? null };
}

// ─── Embedding ──────────────────────────────────────────────────────────────────

// Normalizza un vettore a norma L2 = 1 (in-place su una copia). Un vettore nullo
// resta nullo (evita divisione per zero).
function l2normalize(vec: number[]): number[] {
  let sum = 0;
  for (let i = 0; i < vec.length; i++) sum += vec[i] * vec[i];
  const norm = Math.sqrt(sum);
  if (!norm || !Number.isFinite(norm)) return vec.map(() => 0);
  const out = new Array<number>(vec.length);
  for (let i = 0; i < vec.length; i++) out[i] = vec[i] / norm;
  return out;
}

// Estrae l'array di vettori dalla risposta /v1/embeddings (formato OpenAI:
// { data: [{ index, embedding: number[] }] }), ordinandoli per `index` così
// l'ordine corrisponde all'input anche se il server li riordina.
function parseEmbeddingResponse(json: EmbeddingResponse | null, expectedCount: number): number[][] {
  const data = Array.isArray(json?.data) ? json.data : null;
  if (!data || data.length !== expectedCount) {
    throw new Error(
      `Embedding response shape unexpected (got ${data ? data.length : 'none'}, expected ${expectedCount})`,
    );
  }
  const sorted = data.slice().sort((a, b) => (a.index ?? 0) - (b.index ?? 0));
  return sorted.map((d) => {
    const emb = d?.embedding;
    if (!Array.isArray(emb) || !emb.length) throw new Error('Embedding response missing a vector');
    return emb;
  });
}

// Invia un singolo batch di testi all'endpoint /v1/embeddings, con signal+timeout
// combinati (come runInference dell'analyzer). Ritorna number[][] grezzi (NON
// normalizzati: la normalizzazione la fa embedTexts a valle).
async function embedBatch(
  port: number,
  texts: string[],
  signal?: AbortSignal,
): Promise<number[][]> {
  const ac = new AbortController();
  const onAbort = (): void => ac.abort();
  if (signal) {
    if (signal.aborted) ac.abort();
    else signal.addEventListener('abort', onAbort, { once: true });
  }
  const timer = setTimeout(() => ac.abort(), INFER_TIMEOUT_MS);
  try {
    const res = await fetch(`http://127.0.0.1:${port}/v1/embeddings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ input: texts.map((t) => E5_PREFIX + t) }),
      signal: ac.signal,
    });
    if (!res.ok) throw new Error(`embedding server HTTP ${res.status}`);
    const json = (await res.json()) as EmbeddingResponse;
    return parseEmbeddingResponse(json, texts.length);
  } catch (err) {
    if (ac.signal.aborted && !(signal && signal.aborted)) {
      throw new Error(`Embedding timed out after ${INFER_TIMEOUT_MS}ms`);
    }
    throw err;
  } finally {
    clearTimeout(timer);
    if (signal) signal.removeEventListener('abort', onAbort);
  }
}

// API pubblica (SEAM 3): un vettore L2-normalizzato per testo, stesso ordine.
// [] su input vuoto. Avvia (lazy) il server embedding e batcha le richieste.
// Lancia un errore chiaro se il modello non è pronto (il consumer db fa fallback).
async function embedTexts(
  texts: unknown[],
  { signal }: { signal?: AbortSignal } = {},
): Promise<number[][]> {
  if (!Array.isArray(texts) || texts.length === 0) return [];
  // Coerce ogni elemento a stringa non vuota; un testo vuoto romperebbe e5.
  const clean = texts.map(
    (t) => (typeof t === 'string' ? t.trim() : String(t ?? '').trim()) || ' ',
  );

  if (!isEmbeddingReady()) {
    throw new Error('EMBEDDING_MODEL_NOT_READY: scarica il modello di embedding da Impostazioni');
  }

  inflightCount++;
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  try {
    const { port } = await ensureServer();
    const out: number[][] = [];
    for (let i = 0; i < clean.length; i += EMBED_BATCH) {
      if (signal?.aborted) throw Object.assign(new Error('AbortError'), { name: 'AbortError' });
      const batch = clean.slice(i, i + EMBED_BATCH);
      const vectors = await embedBatch(port, batch, signal);
      for (const v of vectors) out.push(l2normalize(v));
    }
    return out;
  } finally {
    inflightCount--;
    // Riarma l'idle timer solo se un server esiste davvero: se ensureServer()
    // ha lanciato (modello non pronto / spawn/health falliti) non c'è nulla da
    // spegnere e un timer da 5 min punterebbe a un server null (no-op).
    if (server) touchServer();
  }
}

// API pubblica (SEAM 3): coseno fra due vettori. Assume vettori L2-normalizzati
// (→ dot product), ma è robusto: torna 0 su empty/lunghezze diverse/vettore nullo.
// Risultato clampato in [-1, 1].
function cosineSim(a: number[], b: number[]): number {
  if (!Array.isArray(a) || !Array.isArray(b)) return 0;
  const n = Math.min(a.length, b.length);
  if (n === 0) return 0;
  let dot = 0;
  for (let i = 0; i < n; i++) dot += a[i] * b[i];
  if (!Number.isFinite(dot)) return 0;
  // Clamp: errori in virgola mobile possono dare 1.0000001 anche su vettori unitari.
  return Math.max(-1, Math.min(1, dot));
}

export {
  // API del contratto (SEAM 3)
  embedTexts,
  cosineSim,
  isEmbeddingReady,
  // Lifecycle/stato (coerenti con analyzer/stt)
  ensureServer,
  shutdown,
  forceShutdown,
  getServerStatus,
  getStatus,
  // Gestione modello (per Settings / IPC)
  listModels,
  setModel,
  downloadModel,
  pauseModelDownload,
  cancelModelDownload,
  deleteModel,
  isModelReady,
  // Helper puri (esportati per i test)
  l2normalize,
  parseEmbeddingResponse,
};
