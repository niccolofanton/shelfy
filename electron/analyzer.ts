'use strict';

// Local video categorization with a vision model (no cloud, no Python).
//
// Pipeline per job: ffmpeg extracts a handful of keyframes from the downloaded
// video → they are sent as images to a locally-spawned llama.cpp `llama-server`
// (Qwen3-VL) over its OpenAI-compatible /v1/chat/completions endpoint, which
// returns a JSON-schema-constrained { description, tags } object in Italian.
//
// The job queue, abort handling and progress emitter mirror downloader.js so
// the renderer can reuse the same patterns.

import { app } from 'electron';
import path from 'path';
import fs from 'fs';
import os from 'os';
import { spawn } from 'child_process';
import type { ChildProcessByStdio } from 'child_process';
import type { Readable } from 'stream';

// Precise type for the children we spawn with stdio ['ignore','ignore','pipe']:
// no stdin/stdout, a readable stderr. spawn() infers this for those options, so
// the holders must match it (ChildProcessWithoutNullStreams would falsely claim
// non-null stdin/stdout). We only ever read .stderr, .kill() and .on() — all
// available on this type.
type LlamaChild = ChildProcessByStdio<null, null, Readable>;
import * as db from './db';
import * as jobstore from './jobstore';
import * as hardware from './hardware';
import * as binaries from './binaries';
import {
  acquireRemoteRequest,
  aiRoute,
  chatEndpoint,
  onRemoteStatusChange,
  probeRemote,
  type AiRoute,
  type ProviderConfig,
} from './ai-providers';
import { firstExisting, freePort, waitForHttp, downloadFile } from './serverUtils';
import * as webCatalog from './webcap/ai-catalog';
// The prompts and schemas live in shared/ai/ (plan §2.15), shared with the web server;
// build/esbuild-electron.ts inlines these modules into this file's output.
import {
  buildUserPrompt,
  buildWebUserPrompt,
  catalogRequest,
  cleanStringArray,
  normalizeCatalogOutput,
  stripPromptMarkers,
  type AnalyzeKind,
  type AnalyzeResult,
  type RawCatalog,
} from '../shared/ai/catalog';
import { maxTokens, responseFormat, systemPrompt, task, userPrompt } from '../shared/ai/prompts';

const KIND = 'analyze'; // jobstore namespace for this queue
// Thrown when the configured remote node stops answering mid-job: the job goes
// back to the queue instead of failing.
const REMOTE_UNAVAILABLE = 'REMOTE_UNAVAILABLE';

// ─── Local types ──────────────────────────────────────────────────────────────

// A model preset in the registry below. `recommended` flags the default suggestion.
interface ModelPreset {
  id: string;
  name: string;
  tier: string;
  recommended?: boolean;
  modelFile: string;
  mmprojFile: string;
  hfBase: string;
  sizeGB: number;
  minRamGB: number;
  note: string;
}

// Persisted settings file shape (ai-model.json in userData).
interface AnalyzerConfig {
  modelId?: string;
  concurrency?: number;
  tuning?: Partial<Record<TuningKey, TuningValue>>;
  paused?: boolean;
}

// Tuning override keys and their normalized value (a concrete value, or 'auto'
// meaning "use the hardware-derived default"). kvCache is an enum; the rest numeric.
type TuningKey = 'threads' | 'threadsBatch' | 'gpuLayers' | 'ubatch' | 'kvCache';
type KvCache = 'f16' | 'q8_0';
type TuningValue = number | KvCache | 'auto' | 'fit';

// The hardware-derived auto tuning (from hardware.computeLlamaTuning).
interface AutoTuning {
  threads: number;
  threadsBatch: number;
  gpuLayers: 'fit' | 0;
  ubatch: number;
  kvCache: KvCache;
  memoryWarning: string | null;
}

// The concrete flag values used at spawn (auto values, or overridden ones).
interface EffectiveTuning {
  threads: number;
  threadsBatch: number;
  gpuLayers: number | 'fit';
  ubatch: number;
  kvCache: KvCache | 'auto';
}

interface ResolvedTuning {
  effective: EffectiveTuning;
  auto: AutoTuning;
  overrides: Record<TuningKey, TuningValue>;
  variant: string;
  memoryWarning: string | null;
}

// On-disk status of the two weight files for a model.
interface ModelFiles {
  model: boolean;
  mmproj: boolean;
}

interface ModelStatus {
  ready: boolean;
  downloading: boolean;
  downloadingId: string | null;
  files: ModelFiles;
  modelId: string;
  name: string;
  sizeGB: number;
  minRamGB: number;
  remote?: boolean;
  remoteUnreachable?: boolean;
}

// One entry in the settings catalog (listModels).
interface ModelListEntry {
  id: string;
  name: string;
  tier: string;
  note: string;
  sizeGB: number;
  minRamGB: number;
  recommended: boolean;
  ready: boolean;
  partial: boolean;
  active: boolean;
  downloading: boolean;
}

// Result objects returned by the download/pause/cancel flows.
type DownloadResult =
  | ModelStatus
  | ({ canceled: true } & Partial<ModelStatus>)
  | ({ paused: true } & Partial<ModelStatus>);

// The spawned llama-server, once promoted past its health check.
interface RunningServer {
  child: LlamaChild | null;
  port: number;
  ready: Promise<void>;
  modelId: string;
  concurrency?: number;
  tuningKey?: string;
}

// Flags toggling the fallback spawn attempts (GPU → CPU degradation path).
interface SpawnOpts {
  forceCpu?: boolean;
  noMmprojOffload?: boolean;
}

// Tagged outcome of one spawn attempt (never rejects; see attemptLlamaSpawn).
type SpawnExitInfo = { code: number | null; signal: NodeJS.Signals | null };
type SpawnOutcome =
  | { ok: true; child: LlamaChild }
  | {
      ok: false;
      kind: 'exit' | 'spawn' | 'timeout';
      detail: Error | SpawnExitInfo | null;
      stderrTail: string;
    };

// Progress callbacks used by the extraction/inference pipeline.
type StageProgress = { stage: string; frac: number };
type OnStageProgress = (p: StageProgress) => void;
type OnProgressFraction = (fraction: number, label: string) => void;
type OnToken = (full: string) => void;
type OnStage = (stage: string) => void;
type OnQueueProgress = (p: { done: number; total: number }) => void;

// The analysis kind (social posts vs web references), the structured result of
// analyzeFrames and the raw catalog answer are shared/ai/catalog.ts's types.

// A validated tag cluster (label + member tag norms).
interface RefinedGroup {
  label: string;
  tags: string[];
}

// A validated alias pair, in db.saveTagAliases' input shape.
interface AliasPair {
  aliasNorm: string;
  aliasForm: string;
  canonicalNorm: string;
  canonicalForm: string;
}

// The two-tier suggestion bundle the chat search produces.
interface TagGroups {
  broad: string[];
  specific: string[];
  keywords: string[];
}

interface ChatSearchResult {
  reply: string;
  tagsToAdd: string[];
  tagsToRemove: string[];
  keywordsToAdd: string[];
  tagGroups: TagGroups;
  modelUsed: boolean;
}

// One chat turn (subset of roles the chat actually consumes).
interface ChatMessage {
  role: 'user' | 'assistant';
  content: string;
}

// The analyzer's lifecycle for a single queued analysis job.
type AnalyzeJobStatus = 'pending' | 'extracting' | 'analyzing' | 'done' | 'cancelled' | 'error';

// The in-memory job record (serializable; mirrored to jobstore). Distinct from the
// persisted snake_case Shelfy.Job: this is the live UI/queue record.
interface AnalyzeJob {
  key: string;
  postId: string;
  platform: string;
  status: AnalyzeJobStatus;
  progress: number;
  error: string | null;
  authorUsername?: string | null;
  thumbnailUrl?: string | null;
  thumbnailPath?: string | null;
  mediaType?: Shelfy.MediaType | null;
  queuedAt?: number;
  startedAt: number | null;
  finishedAt: number | null;
  durationMs: number | null;
  stage: string | null;
  phaseProgress: number | null;
  indeterminate: boolean;
  model: string | null;
  streamText: string | null;
  description?: string;
  tags?: string[];
  entities?: string[];
  keywords?: string[];
  saveReason?: string;
  language?: string;
}

// The post shape the analyzer pipeline reads. A superset of AnalysisPost with the
// web-reference fields it also consumes (platform, media[].role, text, webTech…).
type AnalyzePost = Shelfy.Post | Shelfy.AnalysisPost;

// ── llama-server /v1/chat/completions wire shapes (only the fields read) ─────────

interface ChatCompletionResponse {
  choices?: Array<{ message?: { content?: string | null } }>;
}

interface ChatCompletionChunk {
  choices?: Array<{ delta?: { content?: string | null } }>;
}

// The raw refine-response shape (tag_refine schema) before validation.
interface RawRefineGroup {
  name?: unknown;
  tags?: unknown;
}
interface RawRefineResponse {
  groups?: RawRefineGroup[];
  outliers?: string[];
}

// One raw {alias, canonical} pair from the tag_aliases schema, before validation.
interface RawAliasPair {
  alias?: unknown;
  canonical?: unknown;
}

// ─── Constants ────────────────────────────────────────────────────────────────

// How many classification jobs run at once. User-configurable (1..MAX_CONCURRENCY,
// persisted) — see getConcurrency/setConcurrency. The default of 1 matches the old
// "one at a time" behaviour; higher values trade VRAM (a bigger KV cache, since the
// server runs with --parallel N and -c PER_SLOT_CTX*N) for batch throughput.
const DEFAULT_CONCURRENCY = 1;
const MAX_CONCURRENCY = 10;
const PER_SLOT_CTX = 4096; // context budget reserved per concurrent slot
const N_FRAMES = 4; // keyframes sampled per video
const FRAME_SCALE = 448; // long-side px sent to the model
const N_WEB_SLIDES = 4; // page screenshots sent per web reference (= N_FRAMES)
const SERVER_IDLE_MS = 5 * 60_000; // shut the server down after this idle period
const HEALTH_TIMEOUT_MS = 180_000; // model load can take a while on first run

const INFER_TIMEOUT_MS = 120_000; // abort a single inference if it stalls
const REMOTE_INFER_TIMEOUT_MS = 300_000; // cold router model swaps can take longer

function remoteThinkingOptions(provider: ProviderConfig | null): object {
  // Qwen's default hidden reasoning can exhaust short JSON/chat budgets before
  // emitting any content. llama.cpp accepts this per-request template option.
  return provider?.model === 'qwen3.8-27b'
    ? { chat_template_kwargs: { enable_thinking: false } }
    : {};
}

// DRY ("Don't Repeat Yourself") sampler params, sent with the vision-extract
// requests. Fed an out-of-distribution image (an abstract/glitch video with no
// caption), a VLM can lock onto one token and repeat it until max_tokens — e.g.
// Gemma 4 emitting "pesce pesce …" ×537, which leaves the json_schema string
// unterminated → the response fails JSON.parse ("Model returned invalid JSON").
// DRY penalizes only VERBATIM repetition past `allowed_length` tokens, so it never
// perturbs normal structured output. (A plain repeat_penalty just shifts the loop
// to emoji spam; DRY broke the loop AND kept the answer valid and on-topic.)
const DRY_SAMPLING = {
  dry_multiplier: 0.8,
  dry_base: 1.75,
  dry_allowed_length: 2,
  dry_penalty_last_n: -1,
};

// Taxonomy axes were dropped from the AI flow (tags carry all signal now), but
// the accessor stays for downstream callers that still expect the shape.
function getTaxonomy(): { categories: string[]; contentTypes: string[] } {
  return { categories: [], contentTypes: [] };
}

// ─── Model registry ─────────────────────────────────────────────────────────────
// Every preset is a vision LLM (VLM) with a separate `mmproj` vision projector,
// served by the bundled llama-server (b9500) over the OpenAI /v1/chat/completions
// API with json_schema-constrained output. That infra is identical across presets,
// so switching model = swapping which GGUF weights get loaded (no binary/API change).
const MODELS: Record<string, ModelPreset> = {
  'qwen3vl-4b': {
    id: 'qwen3vl-4b',
    name: 'Qwen3-VL 4B',
    tier: 'Veloce',
    modelFile: 'Qwen3VL-4B-Instruct-Q4_K_M.gguf',
    mmprojFile: 'mmproj-Qwen3VL-4B-Instruct-F16.gguf',
    hfBase: 'https://huggingface.co/Qwen/Qwen3-VL-4B-Instruct-GGUF/resolve/main',
    sizeGB: 3.3,
    minRamGB: 8,
    note: 'Il più leggero e veloce. Ottima base, ideale per archivi grandi.',
  },
  'qwen3vl-8b': {
    id: 'qwen3vl-8b',
    name: 'Qwen3-VL 8B',
    tier: 'Bilanciato',
    recommended: true,
    modelFile: 'Qwen3VL-8B-Instruct-Q4_K_M.gguf',
    mmprojFile: 'mmproj-Qwen3VL-8B-Instruct-F16.gguf',
    hfBase: 'https://huggingface.co/Qwen/Qwen3-VL-8B-Instruct-GGUF/resolve/main',
    sizeGB: 6.2,
    minRamGB: 16,
    note: 'OCR e ragionamento migliori del 4B. Consigliato con 16 GB+ di RAM.',
  },
  // Gemma 4 (giu 2026): multimodale encoder-free con proiettore vision di tipo
  // `gemma4uv`, supportato solo da llama.cpp ≥ b9500 (b9370 falliva con "unknown
  // projector type: gemma4uv"). Il pack GGUF di ggml-org spedisce comunque un mmproj
  // separato, quindi calza nella struttura modelFile+mmprojFile. L'E4B è la taglia
  // compatta (multimodale come la 12B) per macchine con 8 GB+ di RAM.
  'gemma4-e4b': {
    id: 'gemma4-e4b',
    name: 'Gemma 4 E4B',
    tier: 'Alternativa leggera',
    modelFile: 'gemma-4-E4B-it-Q4_K_M.gguf',
    mmprojFile: 'mmproj-gemma-4-E4B-it-Q8_0.gguf',
    hfBase: 'https://huggingface.co/ggml-org/gemma-4-E4B-it-GGUF/resolve/main',
    sizeGB: 5.9,
    minRamGB: 8,
    note: 'Ultima generazione Google in formato compatto: multimodale unificato, OCR e multilingue forti. Ideale per archivi grandi con 8 GB+ di RAM.',
  },
  'gemma4-12b': {
    id: 'gemma4-12b',
    name: 'Gemma 4 12B',
    tier: 'Top qualità',
    modelFile: 'gemma-4-12B-it-Q4_K_M.gguf',
    mmprojFile: 'mmproj-gemma-4-12B-it-Q8_0.gguf',
    hfBase: 'https://huggingface.co/ggml-org/gemma-4-12B-it-GGUF/resolve/main',
    sizeGB: 7.5,
    minRamGB: 16,
    note: 'Ultima generazione Google: multimodale unificato, OCR e multilingue ai vertici. La più capace della lista. Richiede 16 GB+ di RAM.',
  },
};
const DEFAULT_MODEL_ID = 'qwen3vl-4b';

// Persisted settings live in a tiny JSON file in userData (no settings table in
// the DB): { modelId, concurrency }. Cached in-process; falls back to the default
// on any read/parse error.
function configPath(): string {
  return path.join(app.getPath('userData'), 'ai-model.json');
}

// Read-modify-write so independent settings (modelId, concurrency) don't clobber
// each other when persisted separately.
function readConfig(): AnalyzerConfig {
  try {
    return (JSON.parse(fs.readFileSync(configPath(), 'utf8')) as AnalyzerConfig) || {};
  } catch {
    return {};
  }
}
function writeConfig(patch: AnalyzerConfig): void {
  try {
    fs.mkdirSync(app.getPath('userData'), { recursive: true });
    fs.writeFileSync(configPath(), JSON.stringify({ ...readConfig(), ...patch }), 'utf8');
  } catch (e) {
    console.warn('[analyzer] could not persist config:', (e as Error).message);
  }
}

let _selectedId: string | null = null;

function getSelectedModelId(): string {
  if (_selectedId && MODELS[_selectedId]) return _selectedId;
  const id = readConfig().modelId;
  if (id && MODELS[id]) return (_selectedId = id);
  return (_selectedId = DEFAULT_MODEL_ID);
}

function getSelectedModel(): ModelPreset {
  return MODELS[getSelectedModelId()];
}

let _concurrency: number | null = null;

// Clamp to [1, MAX_CONCURRENCY] and coerce non-finite/garbage to the default.
function clampConcurrency(n: unknown): number {
  const v = Math.floor(Number(n));
  if (!Number.isFinite(v)) return DEFAULT_CONCURRENCY;
  return Math.max(1, Math.min(MAX_CONCURRENCY, v));
}

function getConcurrency(): number {
  if (_concurrency != null) return _concurrency;
  const raw = readConfig().concurrency;
  return (_concurrency = raw == null ? DEFAULT_CONCURRENCY : clampConcurrency(raw));
}

// Sets how many jobs run concurrently. Persists the choice and pumps the queue so
// a raised limit immediately picks up more pending work. The slot count is baked
// into the server at spawn (--parallel/-c), so it only changes on respawn: if the
// model is idle we tear it down now for a clean restart; if work is in flight we
// leave it running (killing it would abort active jobs) and let it respawn with
// the new value on the next idle/teardown cycle. Returns the applied value.
function setConcurrency(n: unknown): number {
  const v = clampConcurrency(n);
  _concurrency = v;
  writeConfig({ concurrency: v });
  if (runningCount === 0 && inflightCount === 0) shutdown();
  pumpQueue();
  return v;
}

// Switches the active model: persists the choice and tears down any running
// server so the next inference respawns with the new weights.
function setModel(id: string): ModelStatus {
  if (!MODELS[id]) throw new Error('Unknown model id: ' + id);
  _selectedId = id;
  writeConfig({ modelId: id });
  // Don't kill a server with work in flight: ensureServer() already shuts down
  // and respawns on a modelId mismatch, so in-flight requests finish on the old
  // model and the new one is loaded on the next call.
  if (runningCount === 0 && inflightCount === 0) shutdown();
  pumpQueue();
  return getModelStatus();
}

// ─── Performance tuning (hardware-aware spawn flags) ────────────────────────────
// llama-server's spawn flags (-t, --threads-batch, -ngl, -ub, --cache-type-k/v)
// default to values derived from the detected hardware (see hardware.js). Each is
// independently overridable from Settings; an override of 'auto' (or absent) means
// "use the detected default". Overrides persist in ai-model.json under `tuning`.
const TUNING_KEYS: TuningKey[] = ['threads', 'threadsBatch', 'gpuLayers', 'ubatch', 'kvCache'];

let _tuning: Record<TuningKey, TuningValue> | null = null;

function normalizeTuningValue(key: TuningKey, val: unknown): TuningValue {
  if (val == null || val === 'auto') return 'auto';
  if (key === 'kvCache') return val === 'f16' || val === 'q8_0' ? val : 'auto';
  const n = Math.floor(Number(val));
  if (!Number.isFinite(n)) return 'auto';
  if (key === 'threads' || key === 'threadsBatch') return Math.max(1, Math.min(64, n));
  if (key === 'gpuLayers') return Math.max(0, Math.min(999, n));
  if (key === 'ubatch') return Math.max(64, Math.min(4096, n));
  return 'auto';
}

function getTuningOverrides(): Record<TuningKey, TuningValue> {
  if (_tuning) return _tuning;
  const raw = readConfig().tuning || {};
  const out = {} as Record<TuningKey, TuningValue>;
  for (const k of TUNING_KEYS) out[k] = normalizeTuningValue(k, raw[k]);
  return (_tuning = out);
}

// The variant of the llama.cpp build actually installed (drives whether GPU flags
// make sense). Best-effort: falls back to the platform default if unreadable.
function currentVariant(): string {
  try {
    return binaries.getLlamaVariant();
  } catch {
    return process.platform === 'darwin' ? 'metal' : 'cpu';
  }
}

// Combine the hardware-derived defaults with the user's overrides into the
// concrete flag values used at spawn. Returns { effective, auto, overrides,
// variant, memoryWarning } so the UI can show "Automatico (N)" vs a manual value.
function resolveTuning(): ResolvedTuning {
  const model = getSelectedModel();
  const auto = hardware.computeLlamaTuning({
    variant: currentVariant(),
    modelSizeGB: model?.sizeGB || 0,
    concurrency: getConcurrency(),
    ctxPerSlot: PER_SLOT_CTX,
  }) as AutoTuning;
  const overrides = getTuningOverrides();
  const effective = {} as EffectiveTuning;
  for (const k of TUNING_KEYS) {
    (effective as Record<TuningKey, TuningValue>)[k] =
      overrides[k] === 'auto'
        ? (auto as unknown as Record<TuningKey, TuningValue>)[k]
        : overrides[k];
  }
  // A manual `threads` override shouldn't leave the prefill thread count below it.
  if (effective.threadsBatch < effective.threads) effective.threadsBatch = effective.threads;
  return {
    effective,
    auto,
    overrides,
    variant: currentVariant(),
    memoryWarning: auto.memoryWarning,
  };
}

// Stable identity of the effective flags — when it changes, a running server must
// respawn to pick up the new values (they're baked in at spawn time).
function tuningKey(effective: EffectiveTuning): string {
  return TUNING_KEYS.map((k) => (effective as Record<TuningKey, TuningValue>)[k]).join('|');
}

function getTuning(): ResolvedTuning {
  return resolveTuning();
}

// Persists tuning overrides. Like setConcurrency, the flags are fixed at spawn, so
// we tear down an idle server for a clean restart; a busy one respawns on its next
// idle cycle (killing it would abort in-flight jobs). Returns the resolved tuning.
function setTuning(patch: Partial<Record<TuningKey, unknown>> = {}): ResolvedTuning {
  const cur = getTuningOverrides();
  const next = { ...cur };
  for (const k of TUNING_KEYS) {
    if (k in patch) next[k] = normalizeTuningValue(k, patch[k]);
  }
  _tuning = next;
  writeConfig({ tuning: next });
  if (runningCount === 0 && inflightCount === 0) shutdown();
  pumpQueue();
  return resolveTuning();
}

// Aggregated hardware report for Settings: detected host, resolved tuning, and the
// most capable model preset that fits comfortably (advisory — not auto-applied).
function getHardwareInfo(): {
  hardware: ReturnType<typeof hardware.detect>;
  tuning: ResolvedTuning;
  recommendedModelId: string | null;
  recommendedVariant: ReturnType<typeof hardware.detect>['recommendedVariant'];
} {
  const presets = Object.values(MODELS).map((m) => ({
    id: m.id,
    minRamGB: m.minRamGB,
    sizeGB: m.sizeGB,
  }));
  return {
    hardware: hardware.detect(),
    tuning: resolveTuning(),
    recommendedModelId: hardware.recommendModel(presets),
    recommendedVariant: hardware.detect().recommendedVariant,
  };
}

// ─── Binary resolution (dev vs packaged) ────────────────────────────────────────

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

function resolveFfmpeg(): string {
  // Prefer a binary shipped in resources (deterministic across platforms);
  // ffmpeg-static's postinstall download is unreliable on Windows.
  const exe = process.platform === 'win32' ? 'ffmpeg.exe' : 'ffmpeg';
  const bundled = firstExisting([
    process.env.FFMPEG_BIN,
    path.join(app.getPath('userData'), 'runtime-bin', 'bin', exe),
    path.join(process.resourcesPath || '', 'bin', exe),
    path.join(__dirname, '..', 'bin', exe),
  ]);
  if (bundled) return bundled;

  let staticPath: string | null = null;
  try {
    staticPath = require('ffmpeg-static') as string;
  } catch {}
  // In a packaged asar the static path points inside app.asar.unpacked.
  if (staticPath && staticPath.includes('app.asar') && !staticPath.includes('app.asar.unpacked')) {
    staticPath = staticPath.replace('app.asar', 'app.asar.unpacked');
  }
  return (
    firstExisting([
      staticPath ?? undefined,
      '/opt/homebrew/bin/ffmpeg',
      '/usr/bin/ffmpeg',
      '/usr/local/bin/ffmpeg',
    ]) || 'ffmpeg'
  );
}

// ─── Model files ────────────────────────────────────────────────────────────────

// Each preset gets its own subdir so models with identically-named projector
// files (e.g. both Gemma presets ship `mmproj-model-f16.gguf`) never collide.
function getModelsRoot(): string {
  return path.join(app.getPath('userData'), 'models');
}

function getModelDir(id: string = getSelectedModelId()): string {
  return path.join(getModelsRoot(), id);
}

function modelPaths(id: string = getSelectedModelId()): { model: string; mmproj: string } {
  const m = MODELS[id];
  const dir = getModelDir(id);
  return { model: path.join(dir, m.modelFile), mmproj: path.join(dir, m.mmprojFile) };
}

// One-time move: the first shipped version stored the default model flat in
// models/. Relocate it into models/<defaultId>/ so it isn't re-downloaded (it's a
// rename within the same dir → instant, no copy). Idempotent and best-effort.
let _migrated = false;
function migrateLegacyLayout(): void {
  if (_migrated) return;
  _migrated = true;
  try {
    const m = MODELS[DEFAULT_MODEL_ID];
    const root = getModelsRoot();
    const dir = getModelDir(DEFAULT_MODEL_ID);
    for (const f of [m.modelFile, m.mmprojFile]) {
      const legacy = path.join(root, f);
      const dest = path.join(dir, f);
      if (fs.existsSync(legacy) && !fs.existsSync(dest)) {
        fs.mkdirSync(dir, { recursive: true });
        fs.renameSync(legacy, dest);
      }
    }
  } catch (e) {
    console.warn('[analyzer] model layout migration skipped:', (e as Error).message);
  }
}

function getModelStatus(): ModelStatus {
  migrateLegacyLayout();
  const id = getSelectedModelId();
  const m = MODELS[id];
  const { model, mmproj } = modelPaths(id);
  const files = { model: fs.existsSync(model), mmproj: fs.existsSync(mmproj) };
  return {
    ready: files.model && files.mmproj,
    downloading: modelDownloading,
    downloadingId: _downloadingId,
    files,
    modelId: id,
    name: m.name,
    sizeGB: m.sizeGB,
    minRamGB: m.minRamGB,
  };
}

function getAnalysisModelStatus(): ModelStatus {
  const route = aiRoute('vision');
  if (route.mode === 'local') return getModelStatus();
  // A configured node that is down still counts as the analysis model: jobs wait
  // for it (or for the user to opt into local models) rather than erroring out.
  const provider = route.mode === 'remote' ? route.provider : { id: 'remote', name: route.name };
  return {
    ready: true,
    remoteUnreachable: route.mode === 'blocked',
    downloading: false,
    downloadingId: null,
    files: { model: false, mmproj: false },
    modelId: provider.id,
    name: provider.name,
    sizeGB: 0,
    minRamGB: 0,
    remote: true,
  };
}

// Catalog for the settings UI: every preset with its on-disk + active state.
function listModels(): ModelListEntry[] {
  migrateLegacyLayout();
  const selected = getSelectedModelId();
  return Object.values(MODELS).map((m) => {
    const { model, mmproj } = modelPaths(m.id);
    const ready = fs.existsSync(model) && fs.existsSync(mmproj);
    // A resumable partial exists if a .part is on disk, or one of the two files
    // is complete while the other is still missing (interrupted between jobs).
    const partial =
      !ready &&
      (fs.existsSync(`${model}.part`) ||
        fs.existsSync(`${mmproj}.part`) ||
        fs.existsSync(model) ||
        fs.existsSync(mmproj));
    return {
      id: m.id,
      name: m.name,
      tier: m.tier,
      note: m.note,
      sizeGB: m.sizeGB,
      minRamGB: m.minRamGB,
      recommended: !!m.recommended,
      ready,
      partial,
      active: m.id === selected,
      downloading: m.id === _downloadingId,
    };
  });
}

// Whether both weight files for a model are present on disk.
function isModelReady(id: string): boolean {
  if (!MODELS[id]) return false;
  const { model, mmproj } = modelPaths(id);
  return fs.existsSync(model) && fs.existsSync(mmproj);
}

let modelDownloading = false;
let _downloadingId: string | null = null; // model id whose download is currently in flight
let _downloadAbort: AbortController | null = null; // AbortController for the in-flight download
let _downloadAction: null | 'pause' | 'cancel' = null; // null | 'pause' | 'cancel' — how an abort is treated

// Deletes the on-disk files (and any leftover .part) for a model by wiping its
// per-model dir. Best-effort. Shuts the server down first if it's serving `id`.
function deleteModelFiles(id: string): void {
  // Tear down a server serving `id` — or one still mid-spawn loading `id` (during
  // the load window `server` is null but the child has the GGUF memory-mapped, so
  // checking only `server.modelId` would miss it and rmSync would race the spawn).
  if ((server && server.modelId === id) || spawningModelId === id) shutdown();
  try {
    // shutdown() cancels the spawn loop (no re-spawn) and signals the child, but
    // the kill is async: on Windows the dying child can still hold the GGUF
    // memory-mapped for a moment, so retry rather than fail to unlink it. On
    // Unix unlinking an mmapped file is fine (the inode lingers until close).
    fs.rmSync(getModelDir(id), { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  } catch (e) {
    console.warn('[analyzer] could not delete model files:', (e as Error).message);
  }
}

// Removes a downloaded model from disk. Returns the refreshed catalog so the UI
// can re-render. Refuses only while THAT model's own download is in flight.
function deleteModel(id: string): ModelListEntry[] {
  if (!MODELS[id]) throw new Error('Unknown model id: ' + id);
  if (modelDownloading && _downloadingId === id) {
    throw new Error('Cannot delete a model while it is downloading');
  }
  deleteModelFiles(id);
  return listModels();
}

// Pauses the in-flight download: aborts the stream but keeps the .part so the
// next downloadModel() call resumes from where it stopped.
function pauseModelDownload(): { paused: true } | ModelStatus {
  if (modelDownloading && _downloadAbort) {
    _downloadAction = 'pause';
    _downloadAbort.abort();
    return { paused: true };
  }
  return getModelStatus();
}

// Cancels a download and discards partial progress. If the in-flight download
// matches `id` (or `id` is omitted) it's aborted and its .part dropped;
// otherwise a paused/partial download for `id` is wiped so it returns to a clean
// "not downloaded" state. Never touches a fully-ready model.
function cancelModelDownload(id?: string): { canceled: true } & Partial<ModelStatus> {
  if (modelDownloading && _downloadAbort && (!id || id === _downloadingId)) {
    _downloadAction = 'cancel';
    _downloadAbort.abort();
    return { canceled: true };
  }
  const target = id || getSelectedModelId();
  if (MODELS[target] && !isModelReady(target)) deleteModelFiles(target);
  return { canceled: true, ...getModelStatus() };
}

// Downloads model + mmproj for `id` (defaults to the active model) into its dir.
// onProgress(fraction, label). Runs in the background: the active/selected model
// is independent, so analysis can keep using an already-downloaded model while
// this runs. Resumable: re-invoking after a pause continues from the kept .part
// files. Owns its own AbortController so pause/cancel can interrupt it. On a
// paused or canceled abort it resolves with a flag instead of throwing.
async function downloadModel(
  id?: string | null | OnProgressFraction,
  onProgress?: OnProgressFraction,
): Promise<DownloadResult> {
  if (typeof id === 'function') {
    onProgress = id;
    id = null;
  } // legacy (no-id) call
  if (modelDownloading) throw new Error('Model download already in progress');
  if (id && !MODELS[id]) throw new Error('Unknown model id: ' + id);
  id = id || getSelectedModelId();
  modelDownloading = true;
  _downloadingId = id;
  _downloadAction = null;
  _downloadAbort = new AbortController();
  const signal = _downloadAbort.signal;
  try {
    migrateLegacyLayout();
    const m = MODELS[id];
    fs.mkdirSync(getModelDir(id), { recursive: true });
    const { model, mmproj } = modelPaths(id);
    const jobs = [
      { url: `${m.hfBase}/${m.modelFile}`, dest: model, label: 'modello', weight: 0.8 },
      { url: `${m.hfBase}/${m.mmprojFile}`, dest: mmproj, label: 'vision', weight: 0.2 },
    ];
    let base = 0;
    for (const j of jobs) {
      if (!fs.existsSync(j.dest)) {
        await downloadFile(
          j.url,
          j.dest,
          (f: number) => onProgress?.(base + f * j.weight, j.label),
          signal,
          {
            keepPartialOnAbort: () => _downloadAction === 'pause',
          },
        );
      }
      base += j.weight;
      onProgress?.(base, j.label);
    }
    return getModelStatus();
  } catch (err) {
    if (_downloadAction === 'cancel') {
      deleteModelFiles(id);
      return { canceled: true, ...getModelStatus() };
    }
    if (_downloadAction === 'pause') {
      return { paused: true, ...getModelStatus() };
    }
    throw err;
  } finally {
    modelDownloading = false;
    _downloadingId = null;
    _downloadAbort = null;
    _downloadAction = null;
  }
}

// ─── llama-server lifecycle ─────────────────────────────────────────────────────

let server: RunningServer | null = null; // { child, port, ready: Promise<void> }
let idleTimer: ReturnType<typeof setTimeout> | null = null;
let serverStarting: Promise<RunningServer> | null = null; // memoized in-flight ensureServer() promise (de-dupes concurrent spawns)
// The child spawned by an in-flight ensureServer() BEFORE it is promoted to
// `server` (it stays null until the health check passes, which can take up to
// HEALTH_TIMEOUT_MS while a multi-GB model loads). shutdown()/forceShutdown()
// must reap this too, or a quit/teardown during the load window orphans the
// freshly spawned llama-server (it keeps holding VRAM and its loopback port).
let spawningChild: LlamaChild | null = null; // the child currently mid-spawn, or null
let spawningModelId: string | null = null; // model id that mid-spawn child is loading, or null
// Bumped on every fresh ensureServer() start AND by shutdown()/forceShutdown().
// The spawn loop captures its value and re-checks it after each attempt: if it
// changed, a teardown (or a newer start) happened while the attempt was loading,
// so the loop must abandon — otherwise killing the mid-spawn child just makes the
// loop re-spawn a new one (the attempt resolves kind:'exit', which isn't a break).
let spawnGeneration = 0;
// Generic count of model requests in flight across ALL callers (analysis,
// search, cluster) — not just queued runJob work (runningCount). Each model-call
// entry point bumps it and disarms the idle timer; the idle shutdown only fires
// when BOTH counters are zero, so a search/cluster call in flight can never be
// killed mid-request the way it could when only runningCount gated shutdown.
let inflightCount = 0;

// Build the llama-server argv for one spawn attempt. `opts.forceCpu` pins layers to
// CPU and drops KV-quant; `opts.noMmprojOffload` keeps the vision projector on CPU
// (the workaround for backends whose CLIP graph is incomplete, e.g. some Vulkan).
function buildLlamaArgs(
  model: string,
  mmproj: string,
  port: number,
  concurrency: number,
  tuning: EffectiveTuning,
  opts: SpawnOpts = {},
): string[] {
  const args = [
    '--model',
    model,
    '--mmproj',
    mmproj,
    '--host',
    '127.0.0.1',
    '--port',
    String(port),
    '-c',
    String(PER_SLOT_CTX * concurrency),
    '--parallel',
    String(concurrency),
    '-t',
    String(tuning.threads),
    '--threads-batch',
    String(tuning.threadsBatch),
    '-fa',
    'on',
    '-ub',
    String(tuning.ubatch),
    // Disable chain-of-thought. We always want a single, direct json_schema-shaped
    // answer: a "thinking" model (e.g. Gemma 4, whose template defaults thinking=on)
    // would otherwise spend the whole token budget in `reasoning_content` and leave
    // `message.content` EMPTY → "Model returned invalid JSON". Harmless no-op for the
    // non-thinking presets (Qwen3-VL Instruct), verified to still emit JSON.
    '--reasoning',
    'off',
    '--no-warmup',
  ];
  // GPU layers: 'fit' → don't pass -ngl, let llama-server's --fit size offload to the
  // device's free memory; a number (manual override) → pass it verbatim.
  const ngl = opts.forceCpu ? 0 : tuning.gpuLayers;
  if (ngl === 'fit') args.push('-fit', 'on');
  else args.push('-ngl', String(ngl));
  if (tuning.kvCache === 'q8_0' && !opts.forceCpu)
    args.push('--cache-type-k', 'q8_0', '--cache-type-v', 'q8_0');
  if (opts.noMmprojOffload) args.push('--no-mmproj-offload');
  return args;
}

// One spawn attempt. Resolves to a tagged outcome (never rejects):
//   { ok: true, child }
//   { ok: false, kind: 'exit'|'spawn'|'timeout', detail, stderrTail }
// On any failure the child is already terminated. Distinguishing 'exit' (the
// process crashed — OOM / missing DLL / unsupported op, worth retrying with safer
// flags) from 'timeout' (alive but slow to load — safer flags won't help) lets the
// caller decide whether to fall back.
async function attemptLlamaSpawn(bin: string, args: string[], port: number): Promise<SpawnOutcome> {
  const child = spawn(bin, args, { stdio: ['ignore', 'ignore', 'pipe'] });
  // Expose the just-spawned child so shutdown()/forceShutdown()/deleteModelFiles()
  // can reap it during the (possibly very long) health-check window, before it is
  // promoted to `server`. Cleared by the caller on success and below on failure.
  spawningChild = child;

  // Keep the last ~50 stderr lines so a failure can report WHY (OOM, bad GGUF…).
  const stderrRing: string[] = [];
  child.stderr.setEncoding('utf8');
  let stderrCarry = '';
  child.stderr.on('data', (d: string) => {
    stderrCarry += d;
    let nl: number;
    while ((nl = stderrCarry.indexOf('\n')) !== -1) {
      stderrRing.push(stderrCarry.slice(0, nl));
      stderrCarry = stderrCarry.slice(nl + 1);
      if (stderrRing.length > 50) stderrRing.shift();
    }
  });
  const stderrTail = (): string => {
    const lines = stderrCarry ? [...stderrRing, stderrCarry] : stderrRing;
    const tail = lines.slice(-50).join('\n').trim();
    return tail ? `\n--- llama-server stderr (tail) ---\n${tail}` : '';
  };

  let exitInfo: SpawnExitInfo | null = null;
  const exited = new Promise<{ kind: 'exit' }>((resolve) =>
    child.on('exit', (code, signal) => {
      exitInfo = { code, signal };
      resolve({ kind: 'exit' });
    }),
  );
  // Each branch resolves a tagged outcome (no rejections) so Promise.race is clean.
  const outcome = await Promise.race<
    { kind: 'ok' } | { kind: 'timeout' } | { kind: 'exit' } | { kind: 'spawn'; err: Error }
  >([
    waitForHttp(port, '/health', HEALTH_TIMEOUT_MS).then(
      () => ({ kind: 'ok' as const }),
      () => ({ kind: 'timeout' as const }),
    ),
    exited,
    new Promise<{ kind: 'spawn'; err: Error }>((resolve) =>
      child.on('error', (e: Error) => resolve({ kind: 'spawn', err: e })),
    ),
  ]);

  if (outcome.kind === 'ok' && exitInfo == null) {
    return { ok: true, child };
  }
  // This attempt failed (or was reaped by shutdown/forceShutdown): the child is
  // gone, so drop the in-flight handle to avoid a stale reference.
  if (spawningChild === child) spawningChild = null;
  try {
    child.kill('SIGKILL');
  } catch {}
  return {
    ok: false,
    kind: outcome.kind === 'ok' ? 'exit' : outcome.kind,
    detail: outcome.kind === 'spawn' ? outcome.err : exitInfo,
    stderrTail: stderrTail(),
  };
}

// Optional renderer hook, set by main: invoked when a GPU variant fails to start so
// the UI can warn and the CPU build can be re-provisioned. See setVariantFallbackHandler.
let onVariantFallback: ((variant: string) => void) | null = null;
function setVariantFallbackHandler(fn: ((variant: string) => void) | null): void {
  onVariantFallback = typeof fn === 'function' ? fn : null;
}

async function ensureServer(): Promise<RunningServer> {
  const id = getSelectedModelId();
  // Test/eval seam: reuse an already-running external llama-server (set
  // SHELFY_EXTERNAL_LLAMA_PORT) instead of spawning one per process — lets
  // parallel eval agents share a single warm model within a fixed RAM budget.
  const extPort = process.env.SHELFY_EXTERNAL_LLAMA_PORT;
  if (extPort) {
    if (!server || server.port !== Number(extPort)) {
      server = { child: null, port: Number(extPort), ready: Promise.resolve(), modelId: id };
    }
    return server;
  }
  const concurrency = getConcurrency();
  const tuning = resolveTuning().effective;
  const tKey = tuningKey(tuning);
  // A running server pinned to a different model — or to a stale concurrency
  // (slot count baked into --parallel/-c at spawn) or stale tuning flags — must be
  // torn down so it respawns with the current settings.
  const stale =
    server &&
    (server.modelId !== id || server.concurrency !== concurrency || server.tuningKey !== tKey);
  if (stale) {
    // Don't respawn while OTHER model calls are mid-flight: the caller has already
    // bumped inflightCount for itself, so inflightCount > 1 means a sibling fetch is
    // live on this shared server and a teardown here would abort it (the very thing
    // setConcurrency/setTuning/setModel deliberately avoid). Keep the live server and
    // let it respawn with the new flags on the next idle/teardown cycle, as designed.
    if (inflightCount > 1) return server!;
    shutdown();
  }
  if (server) return server;
  // De-dupe concurrent starts: analysis + search/cluster can both reach here
  // before the first spawn's health check resolves. Without memoizing the
  // start promise they'd each pass the `if (server)` check and spawn a second
  // llama-server (double RAM/VRAM). The first caller wins; the rest await it.
  if (serverStarting) return serverStarting;
  const status = getModelStatus();
  if (!status.ready) throw new Error('MODEL_NOT_READY');

  spawningModelId = id; // which model the mid-spawn child (if any) is loading
  const myGen = ++spawnGeneration; // this loop's identity; teardown bumps it to cancel us
  serverStarting = (async (): Promise<RunningServer> => {
    const bin = resolveLlamaServer();
    const { model, mmproj } = modelPaths(id);
    const variant = currentVariant();
    const gpuVariant = variant === 'cuda' || variant === 'vulkan' || variant === 'metal';
    // Attempts ordered most-capable → safest. On a GPU build, if the backend can't
    // start (OOM / missing DLL / unsupported vision op) we degrade rather than fail
    // outright: first move the vision projector to CPU (covers incomplete Vulkan CLIP
    // graphs), then pin everything to CPU (always runs, just slow). A CPU build has a
    // single attempt — its layers are already on CPU.
    // --parallel N gives N inference slots; -c is the TOTAL context split across them.
    const attempts: SpawnOpts[] = gpuVariant
      ? [{}, { noMmprojOffload: true }, { noMmprojOffload: true, forceCpu: true }]
      : [{}];

    try {
      let last: Extract<SpawnOutcome, { ok: false }> | null = null;
      for (let i = 0; i < attempts.length; i++) {
        const port = await freePort();
        const args = buildLlamaArgs(model, mmproj, port, concurrency, tuning, attempts[i]);
        const res = await attemptLlamaSpawn(bin, args, port);
        // A teardown (shutdown/forceShutdown) or a newer ensureServer() superseded
        // this loop while the attempt was loading: abandon instead of re-spawning.
        // Kill any child that did come up so we don't leak it.
        if (spawnGeneration !== myGen) {
          try {
            if (res.ok) res.child.kill('SIGKILL');
          } catch {}
          throw new Error('SPAWN_ABORTED');
        }
        if (res.ok) {
          // The child is now promoted to `server`; it is no longer "mid-spawn".
          if (spawningChild === res.child) spawningChild = null;
          // Reap the module `server` if this child later dies (idle teardown / crash).
          res.child.on('exit', () => {
            if (server && server.child === res.child) server = null;
          });
          server = {
            child: res.child,
            port,
            ready: Promise.resolve(),
            modelId: id,
            concurrency,
            tuningKey: tKey,
          };
          if (i > 0)
            console.warn(
              `[analyzer] llama-server started on fallback attempt ${i} (${JSON.stringify(attempts[i])})`,
            );
          return server;
        }
        last = res;
        // A slow load (timeout) won't be helped by safer flags — stop retrying.
        if (res.kind === 'timeout') break;
      }

      // Every attempt failed. For a downloadable GPU variant (Windows cuda/vulkan),
      // demote it so the next provisioning fetches a CPU build that actually runs, and
      // notify the renderer. Metal isn't demoted: macOS ships a single pack and the
      // CPU-flag fallback above already covers it.
      let extra = '';
      if (variant === 'cuda' || variant === 'vulkan') {
        try {
          binaries.markVariantFailed(variant);
        } catch {}
        try {
          onVariantFallback?.(variant);
        } catch {}
        extra =
          `\nL'accelerazione "${variant}" non si è avviata: l'app userà la CPU. ` +
          'Scarica la versione CPU da Impostazioni → Componenti runtime.';
      }
      const code =
        last && last.detail && !(last.detail instanceof Error) ? last.detail.code : undefined;
      const exited = code != null && code !== 0 ? ` (llama-server exited with code ${code})` : '';
      throw new Error(
        `Avvio di llama-server non riuscito${exited}.${extra}${last?.stderrTail || ''}`,
      );
    } finally {
      // Whether we succeeded, failed, or were reaped, no child is mid-spawn now.
      spawningChild = null;
      spawningModelId = null;
    }
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
    // Only shut down when nothing is using the model: neither a queued analysis
    // job (runningCount) nor an ad-hoc search/cluster call (inflightCount).
    if (runningCount === 0 && inflightCount === 0) shutdown();
  }, SERVER_IDLE_MS);
}

function shutdown(): void {
  // Cancel any in-flight spawn loop so killing the mid-spawn child below doesn't
  // just make the loop re-spawn a fresh server with now-stale settings.
  spawnGeneration++;
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  if (server) {
    try {
      if (server.child) server.child.kill('SIGTERM');
    } catch {}
    server = null;
  }
  // Also reap a server still mid-spawn (the freshly spawned child isn't promoted
  // to `server` until its health check passes — up to HEALTH_TIMEOUT_MS while a
  // multi-GB model loads). attemptLlamaSpawn's exited/error branch then resolves
  // !ok, so ensureServer() rejects rather than leaking an orphan.
  if (spawningChild) {
    try {
      spawningChild.kill('SIGTERM');
    } catch {}
    spawningChild = null;
    spawningModelId = null;
  }
}

// Synchronous, immediate teardown for app quit: SIGKILL leaves no chance of an
// orphaned llama-server holding VRAM/port if the grace-period timer never fires
// (the process is exiting). Also reaps a server still mid-spawn.
function forceShutdown(): void {
  // Cancel any in-flight spawn loop (see shutdown()): on quit we must not let it
  // re-spawn a child after we've SIGKILLed the mid-spawn one.
  spawnGeneration++;
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
  // SIGKILL the mid-spawn child too: on quit the child loading a multi-GB model
  // is the most likely thing in flight, and it would otherwise be orphaned.
  if (spawningChild) {
    try {
      spawningChild.kill('SIGKILL');
    } catch {}
    spawningChild = null;
    spawningModelId = null;
  }
}

// ─── ffmpeg frame extraction ────────────────────────────────────────────────────

// Media paths come from social content (downloaded files, carousel slides), so
// they are untrusted. Before handing one to ffmpeg as an input we resolve it to
// an absolute path and reject anything that would be misread as an option flag
// (a leading '-', which ffmpeg parses as a switch, not a filename). Callers also
// pass `-protocol_whitelist file` so a crafted path can't make ffmpeg open a
// remote/pipe protocol. Returns the safe absolute path or throws.
function safeInputPath(p: unknown): string {
  if (typeof p !== 'string' || !p.trim()) throw new Error('Invalid media path');
  const abs = path.resolve(p);
  if (path.basename(abs).startsWith('-')) throw new Error(`Refusing unsafe media path: ${p}`);
  return abs;
}

function spawnAsync(
  bin: string,
  args: string[],
  signal?: AbortSignal,
): Promise<{ code: number | null; stderr: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { stdio: ['ignore', 'ignore', 'pipe'] });
    let stderr = '';
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (d: string) => {
      stderr += d;
    });
    if (signal) {
      const onAbort = (): void => {
        child.kill('SIGKILL');
        reject(Object.assign(new Error('AbortError'), { name: 'AbortError' }));
      };
      if (signal.aborted) return onAbort();
      signal.addEventListener('abort', onAbort, { once: true });
    }
    child.on('error', reject);
    child.on('close', (code) => resolve({ code, stderr }));
  });
}

// ffmpeg-static ships no ffprobe, so read the duration from ffmpeg's own banner.
async function probeDuration(ffmpeg: string, file: string, signal?: AbortSignal): Promise<number> {
  const { stderr } = await spawnAsync(
    ffmpeg,
    ['-hide_banner', '-protocol_whitelist', 'file', '-i', safeInputPath(file)],
    signal,
  );
  const m = stderr.match(/Duration:\s*(\d+):(\d+):(\d+(?:\.\d+)?)/);
  if (!m) return 0;
  return +m[1] * 3600 + +m[2] * 60 + parseFloat(m[3]);
}

// Extracts N evenly-spaced keyframes; returns base64 data URLs. Cleans up temp.
// onProgress({ stage, frac }) fires per frame so the UI can show "fotogramma 2/4".
async function extractFrames(
  videoPath: string,
  signal?: AbortSignal,
  onProgress?: OnStageProgress,
): Promise<string[]> {
  const ffmpeg = resolveFfmpeg();
  // The file can vanish between enqueue and run (user moved/deleted it); fail
  // with a clear message rather than an opaque ffmpeg error.
  if (typeof videoPath !== 'string' || !fs.existsSync(videoPath)) {
    throw new Error(`Video file not found: ${videoPath}`);
  }
  const safeVideo = safeInputPath(videoPath);
  onProgress?.({ stage: 'Analisi durata del video…', frac: 0 });
  const dur = await probeDuration(ffmpeg, safeVideo, signal);
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-frames-'));
  try {
    const urls: string[] = [];
    const count = dur ? N_FRAMES : 1;
    for (let i = 0; i < count; i++) {
      onProgress?.({ stage: `Estrazione fotogramma ${i + 1}/${count}`, frac: i / count });
      const t = dur ? (dur * (i + 0.5)) / N_FRAMES : 0;
      const out = path.join(tmp, `f${i}.jpg`);
      await spawnAsync(
        ffmpeg,
        [
          '-protocol_whitelist',
          'file',
          '-ss',
          t.toFixed(2),
          '-i',
          safeVideo,
          '-frames:v',
          '1',
          '-vf',
          `scale=${FRAME_SCALE}:${FRAME_SCALE}:force_original_aspect_ratio=decrease`,
          '-q:v',
          '4',
          '-f',
          'image2',
          '-y',
          out,
        ],
        signal,
      );
      if (fs.existsSync(out)) {
        urls.push(`data:image/jpeg;base64,${(await fs.promises.readFile(out)).toString('base64')}`);
      }
      onProgress?.({ stage: `Estrazione fotogramma ${i + 1}/${count}`, frac: (i + 1) / count });
    }
    if (!urls.length) throw new Error('No frames extracted from video');
    return urls;
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

// Scales a local image file down to FRAME_SCALE (same filter as extractFrames)
// and returns it as a base64 jpeg data URL. Cleans up its temp dir.
async function scaleImageToDataUrl(
  ffmpeg: string,
  file: string,
  signal?: AbortSignal,
): Promise<string> {
  const safeFile = safeInputPath(file);
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-img-'));
  try {
    const out = path.join(tmp, 'img.jpg');
    await spawnAsync(
      ffmpeg,
      [
        '-protocol_whitelist',
        'file',
        '-i',
        safeFile,
        '-frames:v',
        '1',
        '-vf',
        `scale=${FRAME_SCALE}:${FRAME_SCALE}:force_original_aspect_ratio=decrease`,
        '-q:v',
        '4',
        '-f',
        'image2',
        '-y',
        out,
      ],
      signal,
    );
    if (!fs.existsSync(out)) throw new Error(`Failed to scale image ${path.basename(file)}`);
    return `data:image/jpeg;base64,${(await fs.promises.readFile(out)).toString('base64')}`;
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

// ─── Screenshot quality control (web reference capture) ─────────────────────
// A dedicated, cheap VLM check whose ONLY job is to tell whether a captured page
// screenshot actually shows the loaded page, or is black / blank / a loading
// screen / only partially rendered. Used by the web orchestrator to decide
// whether to re-capture a page (open it, wait longer). Fails OPEN: any error or
// a not-ready model returns { ok: true, status: 'unknown' } so it can never block
// or break the capture pipeline. Its prompts and schema are the "qc" task of shared/ai/.
const ASSESS_TIMEOUT_MS = 30_000;

// Crop the TOP square (above-the-fold) of a tall full-page screenshot and scale
// it down — that region is where a black hero / loading spinner / blank state is
// most evident, and it keeps the payload tiny.
async function scaleTopToDataUrl(
  ffmpeg: string,
  file: string,
  signal?: AbortSignal,
): Promise<string> {
  const safeFile = safeInputPath(file);
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-qc-'));
  try {
    const out = path.join(tmp, 'qc.jpg');
    await spawnAsync(
      ffmpeg,
      [
        '-protocol_whitelist',
        'file',
        '-i',
        safeFile,
        '-frames:v',
        '1',
        '-vf',
        `crop=w=iw:h='min(ih,iw)':x=0:y=0,scale=${FRAME_SCALE}:${FRAME_SCALE}:force_original_aspect_ratio=decrease`,
        '-q:v',
        '4',
        '-f',
        'image2',
        '-y',
        out,
      ],
      signal,
    );
    if (!fs.existsSync(out)) throw new Error(`Failed to scale image ${path.basename(file)}`);
    return `data:image/jpeg;base64,${(await fs.promises.readFile(out)).toString('base64')}`;
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

/**
 * Classify the load state of a captured page screenshot.
 * @param {string} imagePath  local path to the screenshot (webp/png).
 * @param {{ signal?: AbortSignal }} [opts]
 * @returns {Promise<{ ok: boolean, status: string, reason?: string, ready: boolean }>}
 */
async function assessScreenshot(
  imagePath: string,
  { signal }: { signal?: AbortSignal } = {},
): Promise<{ ok: boolean; status: string; reason?: string; ready: boolean }> {
  if (!imagePath || !fs.existsSync(imagePath)) return { ok: true, status: 'unknown', ready: false };
  const route = aiRoute('vision');
  // Node configured but down: skip the QC (fail open) instead of using local.
  if (route.mode === 'blocked') return { ok: true, status: 'unknown', ready: false };
  const provider = route.mode === 'remote' ? route.provider : null;
  if (!provider && !getModelStatus().ready) return { ok: true, status: 'unknown', ready: false };

  let dataUrl: string;
  try {
    const ffmpeg = resolveFfmpeg();
    dataUrl = await scaleTopToDataUrl(ffmpeg, imagePath, signal);
  } catch {
    return { ok: true, status: 'unknown', ready: true }; // scaling failed → don't block
  }

  inflightCount++;
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  const ac = new AbortController();
  const onAbort = (): void => ac.abort();
  if (signal) {
    if (signal.aborted) ac.abort();
    else signal.addEventListener('abort', onAbort, { once: true });
  }
  let releaseRemote: (() => void) | null = null;
  try {
    releaseRemote = provider ? await acquireRemoteRequest(ac.signal) : null;
  } catch {
    if (signal) signal.removeEventListener('abort', onAbort);
    inflightCount--;
    touchServer();
    return { ok: true, status: 'unknown', ready: true };
  }
  const timer = setTimeout(() => ac.abort(), provider ? 120_000 : ASSESS_TIMEOUT_MS);

  try {
    const endpoint = provider
      ? chatEndpoint(provider)
      : {
          url: `http://127.0.0.1:${(await ensureServer()).port}/v1/chat/completions`,
          headers: { 'Content-Type': 'application/json' },
          model: undefined,
        };
    const body = {
      messages: [
        { role: 'system', content: systemPrompt('qc') },
        {
          role: 'user',
          content: [
            { type: 'text', text: userPrompt('qc') },
            { type: 'image_url', image_url: { url: dataUrl } },
          ],
        },
      ],
      response_format: responseFormat('qc'),
      temperature: task('qc').temperature,
      max_tokens: maxTokens('qc'),
      ...(provider ? { model: endpoint.model } : { cache_prompt: false }),
      ...remoteThinkingOptions(provider),
    };
    const res = await fetch(endpoint.url, {
      method: 'POST',
      headers: endpoint.headers,
      body: JSON.stringify(body),
      signal: ac.signal,
      redirect: 'error',
    });
    if (!res.ok) throw new Error(`llama-server HTTP ${res.status}`);
    const json = (await res.json()) as ChatCompletionResponse;
    const parsed = JSON.parse(json.choices?.[0]?.message?.content ?? '{}') as {
      status?: unknown;
      reason?: string;
    };
    const status = typeof parsed.status === 'string' ? parsed.status : 'unknown';
    return {
      ok: status === 'ok' || status === 'unknown',
      status,
      reason: parsed.reason,
      ready: true,
    };
  } catch {
    return { ok: true, status: 'unknown', ready: true }; // fail open
  } finally {
    clearTimeout(timer);
    releaseRemote?.();
    if (signal) signal.removeEventListener('abort', onAbort);
    inflightCount--;
    touchServer();
  }
}

// Collects the visual inputs (base64 data URLs) to send to the model. Videos
// go through frame extraction; images/carousels read the LOCAL files and scale
// them. For a carousel, up to 4 image slides with a localPath are used.
// onProgress({ stage, frac }) reports the extraction sub-step to the UI.
async function collectVisualInputs(
  post: AnalyzePost,
  signal?: AbortSignal,
  onProgress?: OnStageProgress,
): Promise<string[]> {
  // Only run frame extraction when the video is actually on disk. A video post
  // whose file wasn't downloaded still passes canAnalyze() on the strength of its
  // thumbnail/caption — so fall through to the image/text fallback below instead
  // of throwing "Video file not found" (the bulk of analysis errors on libraries
  // saved as thumbnails-only).
  if (
    post.mediaType === 'video' &&
    typeof post.videoPath === 'string' &&
    fs.existsSync(post.videoPath)
  ) {
    return extractFrames(post.videoPath, signal, onProgress);
  }

  // Web reference: the page screenshots are ALREADY local image files (produced by
  // F2/webcapture) → no ffmpeg frame-extraction, only scaleImageToDataUrl (448px),
  // exactly like a carousel. This branch is placed BEFORE the carousel branch on
  // purpose: a web post shares the multi-slide shape, but must follow a dedicated
  // path (slides ordered hero-first, capped to N_WEB_SLIDES, no thumbnail fallback).
  // Hero-first ordering means that if the model's context truncates, the most
  // informative slide (above-the-fold) is the one that survives.
  if (isWebPost(post) && Array.isArray(post.media)) {
    const ROLE_ORDER: Record<string, number> = { hero: 0, interna: 1, footer: 2, mobile: 3 };
    const ffmpeg = resolveFfmpeg();
    const slides = post.media
      .filter(
        (m): m is Shelfy.PostMedia & { localPath: string } =>
          !!m && !!m.localPath && (m.type === 'image' || !m.type) && fs.existsSync(m.localPath),
      )
      // `role` is not always persisted on a post.media row (the DB orders slides by
      // position, hero-first by construction); fall back to original order when absent.
      .sort(
        (a, b) =>
          (ROLE_ORDER[(a as { role?: string }).role ?? ''] ?? 99) -
          (ROLE_ORDER[(b as { role?: string }).role ?? ''] ?? 99),
      )
      .slice(0, N_WEB_SLIDES);
    if (slides.length) {
      const urls: string[] = [];
      for (let i = 0; i < slides.length; i++) {
        onProgress?.({
          stage: `Preparazione screenshot ${i + 1}/${slides.length}`,
          frac: i / slides.length,
        });
        urls.push(await scaleImageToDataUrl(ffmpeg, slides[i].localPath, signal));
        onProgress?.({
          stage: `Preparazione screenshot ${i + 1}/${slides.length}`,
          frac: (i + 1) / slides.length,
        });
      }
      if (urls.length) return urls;
    }
    // No readable screenshot → text-only on the extracted page text (contentText).
    if (typeof post.text === 'string' && post.text.trim()) {
      onProgress?.({ stage: 'Nessuno screenshot: uso il testo della pagina', frac: 1 });
      return [];
    }
    throw new Error('No local screenshot to analyze');
  }

  const ffmpeg = resolveFfmpeg();
  const isCarousel =
    post.mediaType === 'carousel' || (Array.isArray(post.media) && post.media.length > 1);

  if (isCarousel && Array.isArray(post.media)) {
    const slides = post.media
      .filter(
        (m): m is Shelfy.PostMedia & { localPath: string } =>
          !!m && !!m.localPath && (m.type === 'image' || !m.type) && fs.existsSync(m.localPath),
      )
      .slice(0, 4);
    if (slides.length) {
      const urls: string[] = [];
      for (let i = 0; i < slides.length; i++) {
        onProgress?.({
          stage: `Preparazione immagine ${i + 1}/${slides.length}`,
          frac: i / slides.length,
        });
        urls.push(await scaleImageToDataUrl(ffmpeg, slides[i].localPath, signal));
        onProgress?.({
          stage: `Preparazione immagine ${i + 1}/${slides.length}`,
          frac: (i + 1) / slides.length,
        });
      }
      if (urls.length) return urls;
    }
  }

  // Single image (or carousel fallback).
  const single =
    post.imagePath ||
    post.thumbnailPath ||
    post.media?.find((m) => m && m.localPath && fs.existsSync(m.localPath))?.localPath;
  if (single && fs.existsSync(single)) {
    onProgress?.({ stage: 'Preparazione immagine…', frac: 0.5 });
    const url = await scaleImageToDataUrl(ffmpeg, single, signal);
    onProgress?.({ stage: 'Preparazione immagine…', frac: 1 });
    return [url];
  }

  // No local visual asset: fall back to text-only when a caption exists, so
  // text bookmarks still get categorized from their caption alone.
  if (typeof post.text === 'string' && post.text.trim()) {
    onProgress?.({ stage: 'Nessun media: uso la didascalia', frac: 1 });
    return [];
  }
  throw new Error('No local visual asset to analyze');
}

// ─── Model call ─────────────────────────────────────────────────────────────────

// The catalog prompts (social and web), their response schemas and the normalization of
// the answers live in shared/ai/ (catalog.*, web_catalog.*, shared/ai/catalog.ts), shared
// with the web server. The web schema's purpose and industry are closed enums that map 1:1
// onto ai_content_type / ai_category ('other' is the escape hatch, schema v2).

// Runs one inference. Combines the caller's `signal` with a local timeout that
// aborts the fetch if the model stalls; cleans up the timer in finally. When
// `onToken(fullText)` is supplied the request streams (SSE) and reports the
// accumulating raw output so the UI can show the generation live; the final
// JSON.parse is identical either way.
// One JSON-schema-constrained chat completion (local llama-server or the remote
// provider), optionally streamed. Shared by the social/web catalog paths.
interface ChatJsonRequest {
  system: string;
  userContent: unknown[];
  responseFormat: object;
  maxTokens?: number;
  temperature?: number;
  timeoutMs?: number;
}

async function runChatJson(
  request: ChatJsonRequest,
  signal?: AbortSignal,
  onToken?: OnToken,
  provider: ProviderConfig | null = null,
): Promise<RawCatalog> {
  inflightCount++;
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  // ensureServer() can reject (spawn failure, freePort error, MODEL_NOT_READY) —
  // its rejection must not leak the inflightCount bump above, or the idle teardown
  // (gated on inflightCount === 0) is disabled for the rest of the session.
  let endpoint: { url: string; headers: Record<string, string>; model?: string };
  if (provider) endpoint = chatEndpoint(provider);
  else {
    try {
      const { port } = await ensureServer();
      endpoint = {
        url: `http://127.0.0.1:${port}/v1/chat/completions`,
        headers: { 'Content-Type': 'application/json' },
      };
    } catch (err) {
      inflightCount--;
      touchServer();
      throw err;
    }
  }
  const ac = new AbortController();
  const onAbort = (): void => ac.abort();
  if (signal) {
    if (signal.aborted) ac.abort();
    else signal.addEventListener('abort', onAbort, { once: true });
  }
  let releaseRemote: (() => void) | null = null;
  try {
    releaseRemote = provider ? await acquireRemoteRequest(ac.signal) : null;
  } catch (error) {
    if (signal) signal.removeEventListener('abort', onAbort);
    inflightCount--;
    touchServer();
    throw error;
  }
  const timeoutMs = request.timeoutMs ?? (provider ? REMOTE_INFER_TIMEOUT_MS : INFER_TIMEOUT_MS);
  const timer = setTimeout(() => ac.abort(), timeoutMs);

  const stream = typeof onToken === 'function';
  const body = {
    messages: [
      { role: 'system', content: request.system },
      { role: 'user', content: request.userContent },
    ],
    response_format: request.responseFormat,
    temperature: request.temperature ?? 0.2,
    max_tokens: request.maxTokens ?? 768,
    ...(provider ? { model: endpoint.model } : { ...DRY_SAMPLING, cache_prompt: false }),
    ...remoteThinkingOptions(provider),
    ...(stream ? { stream: true } : {}),
  };
  try {
    const res = await fetch(endpoint.url, {
      method: 'POST',
      headers: endpoint.headers,
      body: JSON.stringify(body),
      signal: ac.signal,
      redirect: 'error',
    });
    // Check the HTTP status before touching the body: a non-ok response can carry
    // a non-JSON body, and parsing it first would mask the real status with a
    // generic JSON parse error.
    if (!res.ok) throw new Error(`llama-server HTTP ${res.status}`);

    let content = '';
    if (stream) {
      // Parse the SSE stream: lines `data: {json}` carry choices[0].delta.content.
      let sseBuf = '';
      let deltas = 0;
      let firstAt = 0;
      const t0 = Date.now();
      const decoder = new TextDecoder();
      const handleLine = (line: string): void => {
        const t = line.trim();
        if (!t.startsWith('data:')) return;
        const payload = t.slice(5).trim();
        if (!payload || payload === '[DONE]') return;
        let json: ChatCompletionChunk;
        try {
          json = JSON.parse(payload) as ChatCompletionChunk;
        } catch {
          return;
        }
        const delta = json.choices?.[0]?.delta?.content;
        if (typeof delta === 'string' && delta) {
          if (!firstAt) firstAt = Date.now() - t0;
          deltas++;
          content += delta;
          onToken!(content);
        }
      };
      for await (const chunk of res.body as unknown as AsyncIterable<Uint8Array | string>) {
        sseBuf += typeof chunk === 'string' ? chunk : decoder.decode(chunk, { stream: true });
        let nl: number;
        while ((nl = sseBuf.indexOf('\n')) !== -1) {
          handleLine(sseBuf.slice(0, nl));
          sseBuf = sseBuf.slice(nl + 1);
        }
      }
      // Flush the decoder: a trailing multibyte UTF-8 sequence held across the
      // final chunk boundary would otherwise be dropped (corrupting the last
      // accented char/emoji). decode() with no args emits any buffered bytes.
      sseBuf += decoder.decode();
      if (sseBuf) handleLine(sseBuf);
      // Diagnostic: a high delta count = the server is streaming token by token; a
      // count of ~1 means it buffered the grammar output and flushed it at the end.
      console.log(`[analyzer] inference stream: ${deltas} deltas, first token +${firstAt}ms`);
    } else {
      const json = (await res.json()) as ChatCompletionResponse;
      content = json.choices?.[0]?.message?.content ?? '';
    }

    try {
      return JSON.parse(content) as RawCatalog;
    } catch {
      throw new Error('Model returned invalid JSON');
    }
  } catch (err) {
    // A local-timeout abort is reported to the caller as a real failure (not a
    // user cancellation) only when the caller's own signal didn't fire.
    if (ac.signal.aborted && !(signal && signal.aborted)) {
      throw new Error(`Inference timed out after ${timeoutMs}ms`);
    }
    throw err;
  } finally {
    clearTimeout(timer);
    releaseRemote?.();
    if (signal) signal.removeEventListener('abort', onAbort);
    inflightCount--;
    touchServer();
  }
}

async function runInference(
  frameUrls: string[],
  caption: unknown,
  frequentTags: unknown,
  signal?: AbortSignal,
  onToken?: OnToken,
  kind: AnalyzeKind = 'social',
  provider: ProviderConfig | null = null,
): Promise<RawCatalog> {
  // kind selects the shared/ai task: "catalog" (social) or "web_catalog".
  const request = catalogRequest(caption, frequentTags, frameUrls.length > 0, kind);
  return runChatJson(
    {
      system: request.system,
      userContent: [
        { type: 'text', text: request.user },
        ...frameUrls.map((url) => ({ type: 'image_url', image_url: { url } })),
      ],
      responseFormat: { type: 'json_schema', json_schema: request.schema },
      temperature: request.temperature,
      maxTokens: request.maxTokens,
    },
    signal,
    onToken,
    provider,
  );
}

async function analyzeFrames(
  frameUrls: string[],
  caption: unknown,
  frequentTags: unknown,
  signal?: AbortSignal,
  onStage?: OnStage,
  onToken?: OnToken,
  kind: AnalyzeKind = 'social',
  provider: ProviderConfig | null = null,
): Promise<AnalyzeResult> {
  let parsed: RawCatalog;
  const modelUsed = provider?.name || getSelectedModel().name;
  try {
    parsed = await runInference(frameUrls, caption, frequentTags, signal, onToken, kind, provider);
  } catch (err) {
    const e = err as { name?: string; message?: string } | undefined;
    const isAbort =
      e?.name === 'AbortError' || e?.message === 'AbortError' || (signal && signal.aborted);
    if (isAbort) throw err;
    if (provider) {
      // A remote SSE stream may finish without usable content even when the
      // server accepted the request. Retry once without streaming before giving
      // up on the selected remote model.
      try {
        if (onToken && e?.message === 'Model returned invalid JSON') {
          onStage?.('Risposta incompleta: nuovo tentativo senza streaming…');
          parsed = await runInference(
            frameUrls,
            caption,
            frequentTags,
            signal,
            undefined,
            kind,
            provider,
          );
        } else {
          throw err;
        }
      } catch (retryErr) {
        // Re-probe the node: if it is down, the job waits for it (or for the user
        // to opt into local models) — never a silent switch to the local model.
        const status = await probeRemote();
        if (status.reachable === false) throw new Error(REMOTE_UNAVAILABLE);
        throw retryErr;
      }
    } else {
      // llama.cpp KV-cache corruption bug (#17200): restart the server and retry once.
      // The retry runs NON-streaming (no onToken) — the proven path — so analysis
      // still succeeds even if streamed grammar-constrained output misbehaves.
      // Under concurrency > 1 the llama-server is SHARED across sibling jobs (spawned
      // with --parallel N), and runInference's finally has already decremented this
      // job's inflightCount, so inflightCount now counts only the siblings still
      // mid-fetch. Only tear the server down when no sibling is using it: otherwise a
      // SIGTERM here would abort their in-flight fetches (a non-abort network error),
      // cascading them into their own restart-and-retry. With siblings in flight the
      // failure was just as likely caused by a sibling that already restarted the
      // server, so we simply retry the request against the current server.
      console.warn(
        `[analyzer] inference failed, ${inflightCount === 0 ? 'restarting server and ' : ''}retrying once: ${e?.message || err}`,
      );
      onStage?.('Errore del server: riavvio e nuovo tentativo…');
      if (inflightCount === 0) shutdown();
      parsed = await runInference(frameUrls, caption, frequentTags, signal, undefined, kind);
    }
  }

  // Two tiers (general: themes, specific: details) plus the flat `tags` that feeds
  // ai_tags; for the web, purpose/industry → contentType/category (raw enum slugs).
  return normalizeCatalogOutput(parsed, kind, modelUsed);
}

// ─── Web reference catalog v2 ───────────────────────────────────────────────────

async function analyzeWebCatalog(
  post: Shelfy.Post,
  signal: AbortSignal,
  onStage: (stage: string, frac?: number) => void,
  onToken: OnToken,
  provider: ProviderConfig | null,
): Promise<{ mapped: webCatalog.MappedCatalog; modelUsed: string }> {
  onStage('Preparazione di immagini e dati del sito…', 0.1);
  const input = await webCatalog.buildCatalogInput(post, provider ? 'remote' : 'local', signal);
  const modelUsed = provider?.name || getSelectedModel().name;
  onStage(`Interrogazione del modello (${modelUsed}) con ${input.images} immagini…`);
  const request = {
    system: webCatalog.WEB_CATALOG_SYSTEM,
    userContent: input.userContent,
    responseFormat: webCatalog.WEB_CATALOG_FORMAT,
    maxTokens: input.maxTokens,
    // Low temperature: the catalog is classification, not prose; it must be stable.
    temperature: 0.1,
  };
  let raw: RawCatalog;
  try {
    raw = await runChatJson(request, signal, onToken, provider);
  } catch (err) {
    const e = err as { name?: string; message?: string } | undefined;
    if (e?.name === 'AbortError' || e?.message === 'AbortError' || signal.aborted) throw err;
    try {
      // A streamed reply can end without usable JSON: one non-streaming retry.
      if (e?.message !== 'Model returned invalid JSON') throw err;
      onStage('Risposta incompleta: nuovo tentativo senza streaming…');
      raw = await runChatJson(request, signal, undefined, provider);
    } catch (retryErr) {
      if (provider) {
        const status = await probeRemote();
        if (status.reachable === false) throw new Error(REMOTE_UNAVAILABLE);
      }
      throw retryErr;
    }
  }
  return {
    mapped: webCatalog.mapCatalog(raw as webCatalog.RawWebCatalog, post, modelUsed),
    modelUsed,
  };
}

// ─── Search query expansion (text-only) ─────────────────────────────────────────
//
// The "suggest" task of shared/ai/. It has its own neutral system prompt: the
// cataloging one is biased toward creative-tech, which skews suggestions for
// everyday subjects (e.g. "AirPods" → shader/glsl).

// Interseca un elenco di tag PROPOSTI dal modello col vocabolario REALE
// dell'archivio (P6), così che i chip suggeriti non diano mai 0 risultati.
// Per ogni candidato, in ordine:
//   1. match esatto su tag_norm (db.resolveAlias risolve anche l'identità);
//   2. risoluzione alias (db.resolveAlias) → forma canonica reale;
//   3. fuzzy (db.searchTagsByText) → primo match con post, poi risolto su alias.
// Scarta i candidati che non risolvono ad alcun tag con post. Restituisce le
// forme CANONICHE reali (display form), deduplicate per norm canonico.
// `realTagSet` è l'insieme (lowercased) dei tag_norm che hanno almeno un post,
// usato per confermare che un norm esista davvero nell'archivio.
function intersectWithVocab(candidates: unknown, realTagSet: Set<string>): string[] {
  const out: string[] = [];
  const seen = new Set<string>(); // norm canonici già emessi
  const accept = (norm: unknown, form: unknown): boolean => {
    const key = String(norm ?? '')
      .trim()
      .toLowerCase();
    if (!key || seen.has(key)) return false;
    seen.add(key);
    out.push(String(form ?? norm).trim());
    return true;
  };

  for (const raw of Array.isArray(candidates) ? candidates : []) {
    const norm = String(raw ?? '')
      .trim()
      .toLowerCase();
    if (!norm) continue;

    // 1+2. Match esatto / alias: resolveAlias restituisce { norm, form } canonici
    // (identità se non è un alias). Accetta solo se il canonico esiste davvero.
    let resolved: Shelfy.ResolvedAlias | null = null;
    try {
      resolved = db.resolveAlias(norm);
    } catch {
      resolved = null;
    }
    const canonNorm = resolved && resolved.norm ? String(resolved.norm).trim().toLowerCase() : norm;
    const canonForm =
      resolved && resolved.form ? resolved.form : (resolved && resolved.norm) || raw;
    if (realTagSet.has(canonNorm)) {
      accept(canonNorm, canonForm);
      continue;
    }

    // 3. Fuzzy: primo tag-nome che matcha lessicalmente ed esiste con post.
    let fuzzy: Shelfy.TagCount[] = [];
    try {
      fuzzy = db.searchTagsByText(norm, { limit: 5 }) || [];
    } catch {
      fuzzy = [];
    }
    for (const f of fuzzy) {
      const fNorm = String(f.tag ?? '')
        .trim()
        .toLowerCase();
      if (!fNorm) continue;
      let fr: Shelfy.ResolvedAlias | null = null;
      try {
        fr = db.resolveAlias(fNorm);
      } catch {
        fr = null;
      }
      const frNorm = fr && fr.norm ? String(fr.norm).trim().toLowerCase() : fNorm;
      const frForm = fr && fr.form ? fr.form : f.tag;
      if (realTagSet.has(frNorm)) {
        accept(frNorm, frForm);
        break;
      }
    }
  }
  return out;
}

// Insieme (lowercased) dei tag_norm reali con almeno un post, dal vocabolario
// dell'archivio. Best-effort: vuoto se il DB non espone ancora getTagStats.
function realTagVocabSet(): Set<string> {
  const set = new Set<string>();
  try {
    for (const t of db.getTagStats({ limit: 100000 }) || []) {
      const norm = String(t.tag ?? '')
        .trim()
        .toLowerCase();
      if (norm) set.add(norm);
    }
  } catch {}
  return set;
}

// Espansione deterministica (no LLM) di una query in linguaggio naturale verso i
// tag REALI dell'archivio (P7). Fonde due segnali, entrambi intersecati col
// vocabolario reale (match esatto → alias → fuzzy) così che ogni tag restituito
// abbia per costruzione almeno un post:
//   1. distinctiveness: i tag dei post il cui contenuto menziona la query,
//      ordinati per lift (db.getTagDistinctivenessForTextQuery);
//   2. parole-chiave del messaggio (db.extractContentTerms) risolte sui tag-nome.
// Restituisce forme canoniche reali, deduplicate. [] se l'archivio non è
// interrogabile (degrado graceful → si resta col solo retrieveSpecificTags).
function expandQueryToVocab(
  query: unknown,
  { limit = SPECIFIC_VOCAB_LIMIT }: { limit?: number } = {},
): string[] {
  const q = typeof query === 'string' ? query.trim() : '';
  if (!q) return [];
  const realSet = realTagVocabSet();
  if (realSet.size === 0) return [];

  const candidates: string[] = [];
  // 1. Distinctiveness: già tag reali, ma li passiamo comunque per alias/fuzzy
  //    così le forme alias collassano sulla canonica.
  try {
    for (const d of db.getTagDistinctivenessForTextQuery(q, { limit }) || []) {
      if (d && d.tag) candidates.push(d.tag);
    }
  } catch {}
  // 2. Parole-chiave del messaggio: i termini di contenuto che il fuzzy può
  //    mappare su tag-nome (sinonimi/forme diverse → tag reale).
  let terms: string[] = [];
  try {
    terms = db.extractContentTerms(q) || [];
  } catch {
    terms = [];
  }
  for (const t of terms) candidates.push(t);

  return intersectWithVocab(candidates, realSet).slice(0, limit);
}

// Turns a user search string into a short set of RELATED filter tags via the
// LOCAL model (text-only, no frames) — e.g. "AirPods" → cuffie, Apple, musica,
// design — to surface as clickable filter chips below the gallery search bar.
// Mirrors runInference's signal+timeout handling. Returns { tags }.
async function expandSearchQuery(
  query: unknown,
  signal?: AbortSignal,
): Promise<{ tags: string[] }> {
  if (!getModelStatus().ready) throw new Error('MODEL_NOT_READY');
  const q = typeof query === 'string' ? query.trim() : '';

  inflightCount++;
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  // A rejection from ensureServer() must not leak the inflightCount bump (it gates
  // idle teardown); decrement and re-throw before the main try/finally would run.
  let port: number;
  try {
    ({ port } = await ensureServer());
  } catch (err) {
    inflightCount--;
    touchServer();
    throw err;
  }
  const ac = new AbortController();
  const onAbort = (): void => ac.abort();
  if (signal) {
    if (signal.aborted) ac.abort();
    else signal.addEventListener('abort', onAbort, { once: true });
  }
  const timer = setTimeout(() => ac.abort(), INFER_TIMEOUT_MS);

  const body = {
    messages: [
      { role: 'system', content: systemPrompt('suggest') },
      { role: 'user', content: userPrompt('suggest', { query: q }) },
    ],
    response_format: responseFormat('suggest'),
    temperature: task('suggest').temperature,
    max_tokens: maxTokens('suggest'),
    cache_prompt: false,
  };
  try {
    const res = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
      signal: ac.signal,
    });
    if (!res.ok) throw new Error(`llama-server HTTP ${res.status}`);
    const json = (await res.json()) as ChatCompletionResponse;
    const content = json.choices?.[0]?.message?.content ?? '';
    let parsed: { tags?: unknown };
    try {
      parsed = JSON.parse(content) as { tags?: unknown };
    } catch {
      throw new Error('Model returned invalid JSON');
    }
    // P6: interseca i tag proposti col vocabolario reale dell'archivio (match
    // esatto → alias → fuzzy) e scarta quelli che non risolvono ad alcun tag con
    // post, così i chip suggeriti non danno mai 0 risultati. Restituisce le forme
    // canoniche reali. Se l'archivio è vuoto/non interrogabile, ritorna i tag
    // ripuliti come prima (degrado graceful).
    const proposed = cleanStringArray(parsed.tags, { cap: 8 });
    const realSet = realTagVocabSet();
    if (realSet.size === 0) return { tags: proposed };
    return { tags: intersectWithVocab(proposed, realSet).slice(0, 8) };
  } catch (err) {
    if (ac.signal.aborted && !(signal && signal.aborted)) {
      throw new Error(`Inference timed out after ${INFER_TIMEOUT_MS}ms`);
    }
    throw err;
  } finally {
    clearTimeout(timer);
    if (signal) signal.removeEventListener('abort', onAbort);
    inflightCount--;
    touchServer();
  }
}

// ─── Tag clustering (text-only refinement of co-occurrence candidates) ───────────
//
// db.getTagCandidateGroups produces small, dense candidate groups from weighted
// co-occurrence. For each, the local model (text-only) gives a canonical name,
// splits mixed themes and ejects outliers. Output is validated to a subset of the
// input tags, then persisted by db.saveClusterRun. One short call per group: the
// "cluster_refine" task of shared/ai/.

// Builds the per-group instruction: the tags as a bullet list, each annotated
// with its most frequent co-occurring neighbors as compact context.
function buildRefinePrompt(group: Shelfy.TagCandidateGroup): string {
  const tags = Array.isArray(group?.tags) ? group.tags : [];
  const lines = tags.map((t) => {
    const nb = (group.neighbors && group.neighbors[t]) || [];
    return nb.length ? `- ${t} (${nb.join(', ')})` : `- ${t}`;
  });
  return userPrompt('cluster_refine', { tags: lines.join('\n') });
}

// Best-effort recovery when strict JSON is truncated past max_tokens: pull out
// every balanced {...} block (inner group objects stay complete even when the
// outer object is cut off) and keep those shaped like a group.
function recoverTruncatedGroups(content: string): RawRefineResponse {
  const objs: string[] = [];
  const stack: number[] = [];
  for (let i = 0; i < content.length; i++) {
    const ch = content[i];
    if (ch === '{') stack.push(i);
    else if (ch === '}') {
      const s = stack.pop();
      if (s !== undefined) objs.push(content.slice(s, i + 1));
    }
  }
  const groups: RawRefineGroup[] = [];
  for (const o of objs) {
    try {
      const g = JSON.parse(o) as RawRefineGroup;
      if (g && typeof g.name === 'string' && Array.isArray(g.tags)) groups.push(g);
    } catch {}
  }
  return { groups, outliers: [] };
}

function parseRefineResponse(content: unknown): RawRefineResponse {
  if (typeof content !== 'string' || !content.trim()) return { groups: [], outliers: [] };
  try {
    return JSON.parse(content) as RawRefineResponse;
  } catch {}
  return recoverTruncatedGroups(content);
}

// Validates a model response against the input tag set: keeps only tags that were
// actually in the group (lowercased), drops cross-group duplicates (first wins),
// and discards groups with < 2 tags or an empty name. Pure; exported for testing.
function validateRefinedGroups(inputTags: unknown, parsed: unknown): RefinedGroup[] {
  const allowed = new Set(
    (Array.isArray(inputTags) ? inputTags : []).map((t) => String(t).toLowerCase()),
  );
  const used = new Set<string>();
  const out: RefinedGroup[] = [];
  const groupsIn = (parsed as RawRefineResponse | null | undefined)?.groups;
  for (const g of Array.isArray(groupsIn) ? groupsIn : []) {
    const label = typeof g?.name === 'string' ? g.name.trim() : '';
    if (!label) continue;
    const tags: string[] = [];
    for (const raw of Array.isArray(g?.tags) ? g.tags : []) {
      if (typeof raw !== 'string') continue;
      const t = raw.trim().toLowerCase();
      if (!t || !allowed.has(t) || used.has(t)) continue;
      used.add(t);
      tags.push(t);
    }
    if (tags.length >= 2) out.push({ label, tags });
  }
  return out;
}

// One inference per candidate group. Mirrors expandSearchQuery's signal+timeout
// handling. Returns validated [{ label, tags:[norm] }].
async function refineOneGroup(
  group: Shelfy.TagCandidateGroup,
  signal?: AbortSignal,
): Promise<RefinedGroup[]> {
  inflightCount++;
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  // Don't leak the inflightCount bump if ensureServer() rejects — this runs in a
  // serial per-group loop, so a server that starts failing would otherwise strand
  // inflightCount at a large value and permanently disable idle teardown.
  let port: number;
  try {
    ({ port } = await ensureServer());
  } catch (err) {
    inflightCount--;
    touchServer();
    throw err;
  }
  const ac = new AbortController();
  const onAbort = (): void => ac.abort();
  if (signal) {
    if (signal.aborted) ac.abort();
    else signal.addEventListener('abort', onAbort, { once: true });
  }
  const timer = setTimeout(() => ac.abort(), INFER_TIMEOUT_MS);

  const nTags = Array.isArray(group?.tags) ? group.tags.length : 0;
  const body = {
    messages: [
      { role: 'system', content: systemPrompt('cluster_refine') },
      { role: 'user', content: buildRefinePrompt(group) },
    ],
    response_format: responseFormat('cluster_refine'),
    temperature: task('cluster_refine').temperature,
    max_tokens: maxTokens('cluster_refine', nTags),
    cache_prompt: true,
  };
  try {
    const res = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
      signal: ac.signal,
    });
    if (!res.ok) throw new Error(`llama-server HTTP ${res.status}`);
    const json = (await res.json()) as ChatCompletionResponse;
    const content = json.choices?.[0]?.message?.content ?? '';
    return validateRefinedGroups(group.tags, parseRefineResponse(content));
  } catch (err) {
    if (ac.signal.aborted && !(signal && signal.aborted)) {
      throw new Error(`Inference timed out after ${INFER_TIMEOUT_MS}ms`);
    }
    throw err;
  } finally {
    clearTimeout(timer);
    if (signal) signal.removeEventListener('abort', onAbort);
    inflightCount--;
    touchServer();
  }
}

// Refines all candidate groups serially (concurrency 1). On a per-group failure
// (non-abort) falls back to keeping the raw group under its most frequent tag so
// no tags are lost. onProgress({ done, total }) fires after each group.
async function refineTagGroups(
  candidates: Shelfy.TagCandidateGroup[] | null | undefined,
  { signal, onProgress }: { signal?: AbortSignal; onProgress?: OnQueueProgress } = {},
): Promise<RefinedGroup[]> {
  const total = Array.isArray(candidates) ? candidates.length : 0;
  const clusters: RefinedGroup[] = [];
  let done = 0;
  onProgress?.({ done, total });
  for (const group of candidates || []) {
    if (signal?.aborted) throw Object.assign(new Error('AbortError'), { name: 'AbortError' });
    try {
      clusters.push(...(await refineOneGroup(group, signal)));
    } catch (err) {
      const e = err as { name?: string; message?: string } | undefined;
      if (e?.name === 'AbortError' || signal?.aborted) throw err;
      console.warn(`[analyzer] cluster refine failed for a group: ${e?.message || err}`);
      if (Array.isArray(group?.tags) && group.tags.length >= 2) {
        clusters.push({ label: group.tags[0], tags: group.tags });
      }
    }
    onProgress?.({ done: ++done, total });
  }
  return clusters;
}

// Full clustering run: build candidates → refine with the model → persist as
// 'proposed'. Keeps the server alive for the whole batch. Returns a summary.
async function clusterTags({
  signal,
  onProgress,
}: { signal?: AbortSignal; onProgress?: OnQueueProgress } = {}): Promise<{
  runId: number;
  count: number;
  candidates: number;
}> {
  if (!getModelStatus().ready) throw new Error('MODEL_NOT_READY');
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  try {
    const candidates = await db.getTagCandidateGroups();
    const clusters = await refineTagGroups(candidates, { signal, onProgress });
    const saved = db.saveClusterRun(clusters);
    return { runId: saved.runId, count: saved.count, candidates: candidates.length };
  } finally {
    touchServer();
  }
}

// ─── Alias building (synonym canonicalization, P3) ───────────────────────────────
//
// Maintenance (NOT on the per-post path): takes the tags without an alias and maps
// the near-synonyms onto a canonical form that ALREADY EXISTS in the canonical
// vocabulary (the allowlist). Hard rule: the model may ONLY map onto allowlisted
// tags, never coin new ones; a tag that is no synonym stays its own canonical form
// (it is OMITTED from the output → no alias). This is the "aliases" task of
// shared/ai/ (the same system prompt as the refinement). The orchestrator then calls
// db.saveTagAliases on the returned pairs — this function persists nothing.

// Quanti tag senza alias processare per chiamata e dimensione del batch inviato al
// modello (un batch troppo grande sfora il contesto/max_tokens).
const ALIAS_INPUT_LIMIT = 400; // tag senza alias considerati per run
const ALIAS_VOCAB_LIMIT = 300; // dimensione allowlist canonica nel prompt
const ALIAS_BATCH_SIZE = 40; // tag-candidato per chiamata LLM

// Prompt per un batch: la lista dei tag-candidato e l'allowlist canonica.
function buildAliasPrompt(batch: Shelfy.VocabTag[], vocab: Shelfy.VocabTag[]): string {
  const cands = (Array.isArray(batch) ? batch : []).map((t) => t.form || t.norm).filter(Boolean);
  const canon = (Array.isArray(vocab) ? vocab : []).map((t) => t.form || t.norm).filter(Boolean);
  return userPrompt('aliases', { candidates: cands.join(', '), vocabulary: canon.join(', ') });
}

// Valida l'output del modello come fa validateRefinedGroups: tiene SOLO le coppie
// il cui `alias` è tra i candidati (lowercased) e il cui `canonical` è
// nell'allowlist canonica (lowercased), scartando le auto-mappe e le catene
// (canonica che è essa stessa un candidato-alias). Restituisce coppie nel formato
// di db.saveTagAliases: [{ aliasNorm, aliasForm, canonicalNorm, canonicalForm }].
function validateAliasPairs(
  batch: Shelfy.VocabTag[],
  vocab: Shelfy.VocabTag[],
  parsed: unknown,
): AliasPair[] {
  // form canonica reale per ogni norm (preserva il display dell'allowlist).
  const canonByNorm = new Map<string, string>();
  for (const t of Array.isArray(vocab) ? vocab : []) {
    const norm = String(t.norm ?? t.form ?? '')
      .trim()
      .toLowerCase();
    if (norm) canonByNorm.set(norm, t.form || t.norm);
  }
  const aliasFormByNorm = new Map<string, string>();
  for (const t of Array.isArray(batch) ? batch : []) {
    const norm = String(t.norm ?? t.form ?? '')
      .trim()
      .toLowerCase();
    if (norm) aliasFormByNorm.set(norm, t.form || t.norm);
  }

  const out: AliasPair[] = [];
  const seen = new Set<string>();
  const aliasesIn = (parsed as { aliases?: RawAliasPair[] } | null | undefined)?.aliases;
  for (const p of Array.isArray(aliasesIn) ? aliasesIn : []) {
    const aliasNorm = String(p?.alias ?? '')
      .trim()
      .toLowerCase();
    const canonicalNorm = String(p?.canonical ?? '')
      .trim()
      .toLowerCase();
    if (!aliasNorm || !canonicalNorm) continue;
    if (aliasNorm === canonicalNorm) continue; // no auto-mappe
    if (!aliasFormByNorm.has(aliasNorm)) continue; // alias deve essere un candidato
    if (!canonByNorm.has(canonicalNorm)) continue; // canonica deve essere nell'allowlist
    if (aliasFormByNorm.has(canonicalNorm)) continue; // no catene: canonica non è essa stessa un candidato
    if (seen.has(aliasNorm)) continue; // un alias mappa a una sola canonica
    seen.add(aliasNorm);
    out.push({
      aliasNorm,
      aliasForm: aliasFormByNorm.get(aliasNorm)!,
      canonicalNorm,
      canonicalForm: canonByNorm.get(canonicalNorm)!,
    });
  }
  return out;
}

// Una chiamata LLM per batch. Mirror di refineOneGroup per signal+timeout.
// Restituisce coppie validate nel formato di db.saveTagAliases.
async function buildAliasesForBatch(
  batch: Shelfy.VocabTag[],
  vocab: Shelfy.VocabTag[],
  signal?: AbortSignal,
): Promise<AliasPair[]> {
  inflightCount++;
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  // Don't leak the inflightCount bump if ensureServer() rejects — this runs in a
  // serial per-batch loop, so a server that starts failing would otherwise strand
  // inflightCount at a large value and permanently disable idle teardown.
  let port: number;
  try {
    ({ port } = await ensureServer());
  } catch (err) {
    inflightCount--;
    touchServer();
    throw err;
  }
  const ac = new AbortController();
  const onAbort = (): void => ac.abort();
  if (signal) {
    if (signal.aborted) ac.abort();
    else signal.addEventListener('abort', onAbort, { once: true });
  }
  const timer = setTimeout(() => ac.abort(), INFER_TIMEOUT_MS);

  const body = {
    messages: [
      { role: 'system', content: systemPrompt('aliases') },
      { role: 'user', content: buildAliasPrompt(batch, vocab) },
    ],
    response_format: responseFormat('aliases'),
    temperature: task('aliases').temperature,
    max_tokens: maxTokens('aliases', batch.length),
    cache_prompt: true,
  };
  try {
    const res = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
      signal: ac.signal,
    });
    if (!res.ok) throw new Error(`llama-server HTTP ${res.status}`);
    const json = (await res.json()) as ChatCompletionResponse;
    const content = json.choices?.[0]?.message?.content ?? '';
    let parsed: { aliases: RawAliasPair[] };
    try {
      parsed = JSON.parse(content) as { aliases: RawAliasPair[] };
    } catch {
      parsed = { aliases: [] };
    }
    return validateAliasPairs(batch, vocab, parsed);
  } catch (err) {
    if (ac.signal.aborted && !(signal && signal.aborted)) {
      throw new Error(`Inference timed out after ${INFER_TIMEOUT_MS}ms`);
    }
    throw err;
  } finally {
    clearTimeout(timer);
    if (signal) signal.removeEventListener('abort', onAbort);
    inflightCount--;
    touchServer();
  }
}

// Costruzione alias completa (manutenzione). Prende i tag senza alias
// (db.getUnaliasedTags) e l'allowlist canonica (db.getCanonicalVocab), processa a
// batch serialmente (concurrency 1), valida ogni output contro l'allowlist e
// restituisce le coppie da persistere — NON chiama db.saveTagAliases (lo fa
// l'orchestratore). onProgress({ done, total }) fira dopo ogni batch.
async function buildTagAliases({
  signal,
  onProgress,
}: { signal?: AbortSignal; onProgress?: OnQueueProgress } = {}): Promise<AliasPair[]> {
  if (!getModelStatus().ready) throw new Error('MODEL_NOT_READY');
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }

  let candidates: Shelfy.VocabTag[] = [];
  try {
    candidates = db.getUnaliasedTags({ limit: ALIAS_INPUT_LIMIT }) || [];
  } catch {
    candidates = [];
  }
  let vocab: Shelfy.VocabTag[] = [];
  try {
    vocab = db.getCanonicalVocab({ limit: ALIAS_VOCAB_LIMIT }) || [];
  } catch {
    vocab = [];
  }

  // Niente candidati o nessuna canonica su cui mappare: nessun alias da proporre.
  if (!candidates.length || !vocab.length) {
    onProgress?.({ done: 0, total: 0 });
    return [];
  }

  // I candidati che SONO già nell'allowlist canonica non vanno mappati su altro
  // (sono canonici di sé): si escludono dall'input per non auto-mapparli.
  const canonNormSet = new Set(
    vocab
      .map((t) =>
        String(t.norm ?? t.form ?? '')
          .trim()
          .toLowerCase(),
      )
      .filter(Boolean),
  );
  const batchable = candidates.filter((t) => {
    const norm = String(t.norm ?? t.form ?? '')
      .trim()
      .toLowerCase();
    return norm && !canonNormSet.has(norm);
  });

  const batches: Shelfy.VocabTag[][] = [];
  for (let i = 0; i < batchable.length; i += ALIAS_BATCH_SIZE) {
    batches.push(batchable.slice(i, i + ALIAS_BATCH_SIZE));
  }

  const pairs: AliasPair[] = [];
  const usedAlias = new Set<string>(); // un alias non deve comparire in più batch-output
  let done = 0;
  const total = batches.length;
  onProgress?.({ done, total });
  try {
    for (const batch of batches) {
      if (signal?.aborted) throw Object.assign(new Error('AbortError'), { name: 'AbortError' });
      try {
        for (const p of await buildAliasesForBatch(batch, vocab, signal)) {
          if (usedAlias.has(p.aliasNorm)) continue;
          usedAlias.add(p.aliasNorm);
          pairs.push(p);
        }
      } catch (err) {
        const e = err as { name?: string; message?: string } | undefined;
        if (e?.name === 'AbortError' || signal?.aborted) throw err;
        // Un batch fallito non perde gli altri: si salta (nessun alias inventato).
        console.warn(`[analyzer] alias build failed for a batch: ${e?.message || err}`);
      }
      onProgress?.({ done: ++done, total });
    }
  } finally {
    touchServer();
  }
  return pairs;
}

// ─── Conversational tag-search chat (streaming) ──────────────────────────────────

const CHAT_TIMEOUT_MS = 30_000; // chat replies are short; fail fast if it stalls
const REMOTE_CHAT_TIMEOUT_MS = 180_000; // allow one cold router model swap
const BROAD_VOCAB_LIMIT = 150; // top-frequency tags offered as the "general" pool
const SPECIFIC_VOCAB_LIMIT = 60; // query-retrieved long-tail tags offered as "specific"
const MAX_PROPOSED = 30; // hard cap across BOTH tiers; the model picks only the relevant ones (often fewer)
const PER_TIER_CAP = 15; // per-tier cap (general / specific)
// The model splits its picks into two semantic tiers: broad/general themes and
// fine-grained/detailed tags. They're parsed separately so the UI can space
// them apart. TAGS_OPEN is kept as a legacy fallback if the model emits a
// single block instead of the two tiers.
const GENERAL_OPEN = '[[GENERAL]]';
const GENERAL_CLOSE = '[[/GENERAL]]';
const SPECIFIC_OPEN = '[[SPECIFIC]]';
const SPECIFIC_CLOSE = '[[/SPECIFIC]]';
const TAGS_OPEN = '[[TAGS]]';
const TAGS_CLOSE = '[[/TAGS]]';
const REMOVE_OPEN = '[[REMOVE]]';
const REMOVE_CLOSE = '[[/REMOVE]]';
// Free-text search keywords EXTRACTED FROM THE USER MESSAGE (not archive tags):
// the concrete words to match literally against post captions + AI descriptions,
// like a traditional search. Parsed free-form (no vocab allowlist).
const KEYWORDS_OPEN = '[[KEYWORDS]]';
const KEYWORDS_CLOSE = '[[/KEYWORDS]]';
const MAX_KEYWORDS = 6; // applied search keywords cap
// Conversational text ends at the first block marker; all markers start with this.
const BLOCK_MARKER = '[[';

// Builds the system prompt for the search chat. Injects two real-tag allowlists
// — the broad top-frequency pool and the query-retrieved specific pool — so the
// model can only propose tags that actually exist in the archive, and picks each
// tier from the matching list. Also injects the active tags so it can refine.
function buildChatSystemPrompt(
  broadVocab: unknown,
  specificVocab: unknown,
  activeTags: unknown,
): string {
  const broad = Array.isArray(broadVocab)
    ? broadVocab.filter((t): t is string => typeof t === 'string' && !!t.trim())
    : [];
  const specific = Array.isArray(specificVocab)
    ? specificVocab.filter((t): t is string => typeof t === 'string' && !!t.trim())
    : [];
  const active = Array.isArray(activeTags)
    ? activeTags.filter((t): t is string => typeof t === 'string' && !!t.trim())
    : [];
  // The "chat" task of shared/ai/: the caps and the sentinels are the parser's own.
  return systemPrompt('chat', {
    broad: broad.join(', '),
    specific: specific.join(', '),
    active: active.join(', '),
    perTierCap: String(PER_TIER_CAP),
    maxKeywords: String(MAX_KEYWORDS),
    generalOpen: GENERAL_OPEN,
    generalClose: GENERAL_CLOSE,
    specificOpen: SPECIFIC_OPEN,
    specificClose: SPECIFIC_CLOSE,
    keywordsOpen: KEYWORDS_OPEN,
    keywordsClose: KEYWORDS_CLOSE,
    removeOpen: REMOVE_OPEN,
    removeClose: REMOVE_CLOSE,
  });
}

// Minimum topic-association lift a LEXICAL tag-name match must clear to enter the
// pool. A name match (tag_norm LIKE %term%) is only relevant if the tag also
// actually CO-OCCURS with the query's content in the archive; otherwise it is an
// incidental substring collision (the prime noise source for hard queries). We
// reuse the distinctiveness harvest's matched-post set as the validator.
const SPECIFIC_LEXICAL_MIN_LIFT = 0.12;

// Pool dei tag GENERALI offerto al modello come "temi ampi" (P2). Usa il tier
// PERSISTITO via db.getTagStats({ tier: 'general' }); se quella lista è vuota
// (archivio legacy con tier NULL) ricade sul comportamento storico: i top-N tag
// per frequenza globale, senza filtro di tier. Restituisce forme display.
function getBroadVocab(): string[] {
  let general: string[] = [];
  try {
    general = (db.getTagStats({ limit: BROAD_VOCAB_LIMIT, tier: 'general' }) || [])
      .map((t) => t.tag)
      .filter(Boolean);
  } catch {
    general = [];
  }
  if (general.length) return general;
  // Fallback per-frequenza (legacy, tier NULL).
  try {
    return (db.getTagStats({ limit: BROAD_VOCAB_LIMIT }) || []).map((t) => t.tag).filter(Boolean);
  } catch {
    return [];
  }
}

// Builds the query-relevant "specific" candidate pool from the FULL archive
// vocabulary. DISTINCTIVENESS-FIRST (deterministic, no LLM):
//   1. db.getTagDistinctivenessForTextQuery — finds posts whose CONTENT (incl.
//      ai_keywords) mentions the query, then ranks the tags those posts carry by
//      LIFT (concentrated in the matched set, rare globally). This ALWAYS runs
//      (no <5 fallback) and is the primary, on-topic source — it surfaces the
//      long-tail tags that name-matching can't reach (e.g. "airpods" → "prodotto
//      tecnologico", "cuffie") while filtering out archive-dominant filler.
//   2. db.searchTagsByText — lexical tag-NAME matches, but ONLY those that the
//      matched-post set confirms are on-topic (lift ≥ SPECIFIC_LEXICAL_MIN_LIFT).
//      This keeps the control-query wins (shader/fluid tag names ARE the topic)
//      while dropping incidental substring collisions that used to dominate.
// The co-occurrence expansion is GONE: it was the major noise source, dragging in
// hub tags unrelated to the query. Excludes tags already in the broad pool or
// active. Returns display-form tags, capped at SPECIFIC_VOCAB_LIMIT.
function retrieveSpecificTags(query: unknown, excludeSet: Set<string>): string[] {
  const out: string[] = [];
  const seen = new Set<string>(); // lowercased keys
  const push = (tag: unknown): void => {
    if (out.length >= SPECIFIC_VOCAB_LIMIT) return;
    const t = String(tag ?? '').trim();
    if (!t) return;
    const key = t.toLowerCase();
    if (seen.has(key) || excludeSet.has(key)) return;
    seen.add(key);
    out.push(t);
  };

  // P2: insieme dei tag con tier 'specific' PERSISTITO. Usato per dare priorità,
  // all'interno dei candidati query-relevant, ai tag che il modello ha davvero
  // etichettato come "specifici". Se vuoto (archivio legacy, tier NULL) il
  // comportamento resta invariato (nessuna ri-priorità per tier).
  const specificTierSet = new Set<string>();
  try {
    for (const t of db.getTagStats({ limit: 1000, tier: 'specific' }) || []) {
      const norm = String(t.tag ?? '')
        .trim()
        .toLowerCase();
      if (norm) specificTierSet.add(norm);
    }
  } catch {}
  const isSpecificTier = (tag: unknown): boolean =>
    specificTierSet.has(
      String(tag ?? '')
        .trim()
        .toLowerCase(),
    );

  // 1. Distinctiveness harvest from post content — the primary on-topic source.
  // Quando esiste un tier persistito, i candidati con tier 'specific' entrano per
  // primi (sono per definizione tag di dettaglio); gli altri restano come prima.
  let distinct: Shelfy.TagDistinctiveness[] = [];
  try {
    distinct =
      db.getTagDistinctivenessForTextQuery(query as string, { limit: SPECIFIC_VOCAB_LIMIT }) || [];
  } catch {
    distinct = [];
  }
  // Map of lowercased tag-name → topic lift, to validate lexical matches below.
  const liftByTag = new Map<string, number>();
  for (const d of distinct)
    liftByTag.set(
      String(d.tag ?? '')
        .trim()
        .toLowerCase(),
      d.lift || 0,
    );
  // Quando c'è un tier persistito, prima i tag con tier 'specific', poi il resto
  // (ordine stabile: il harvest è già ordinato per lift). Senza tier persistito
  // l'ordine è quello originale.
  if (specificTierSet.size > 0) {
    for (const d of distinct) if (isSpecificTier(d.tag)) push(d.tag);
  }
  for (const d of distinct) push(d.tag);

  // 2. Lexical tag-name matches, gated by topic association. A name match only
  //    enters if the distinctiveness harvest confirms it co-occurs with the
  //    query content (lift ≥ threshold). When the harvest is empty (no matched
  //    posts at all), fall back to admitting lexical matches ungated so a thin
  //    query still gets SOMETHING from its literal tag names.
  let matches: Shelfy.TagCount[] = [];
  try {
    matches = db.searchTagsByText(query as string, { limit: SPECIFIC_VOCAB_LIMIT }) || [];
  } catch {
    matches = [];
  }
  const gateLexical = liftByTag.size > 0;
  for (const m of matches) {
    if (out.length >= SPECIFIC_VOCAB_LIMIT) break;
    if (gateLexical) {
      const lift = liftByTag.get(
        String(m.tag ?? '')
          .trim()
          .toLowerCase(),
      );
      if (lift == null || lift < SPECIFIC_LEXICAL_MIN_LIFT) continue;
    }
    push(m.tag);
  }

  return out;
}

// Lenient parse of a sentinel block body into normalized tags intersected with
// `vocabSet`: splits on commas/newlines/multiple spaces, lowercases, trims,
// strips a leading '#', drops anything not in the allowlist. Returns unique tags.
function parseTagBlock(
  text: unknown,
  open: string,
  close: string,
  vocabSet: Set<string>,
): string[] {
  if (typeof text !== 'string') return [];
  const start = text.indexOf(open);
  if (start === -1) return [];
  const from = start + open.length;
  const end = text.indexOf(close, from);
  const body = end === -1 ? text.slice(from) : text.slice(from, end);
  const out: string[] = [];
  const seen = new Set<string>();
  for (const raw of body.split(/[,\n]+|\s{2,}/)) {
    const tag = raw.trim().replace(/^#+/, '').trim().toLowerCase();
    if (!tag) continue;
    if (!vocabSet.has(tag)) continue;
    if (seen.has(tag)) continue;
    seen.add(tag);
    out.push(tag);
  }
  return out;
}

// Parse the free-text KEYWORDS block. Unlike tags these are NOT intersected with
// any vocab — they're the user's own search terms, extracted by the model from
// the message. Splits on commas/newlines ONLY (keeps multi-word phrases intact),
// lowercases, trims, strips a leading '#', drops too-short (<2) and over-long
// (>40 chars / >4 words) entries, de-dupes, caps at MAX_KEYWORDS.
function parseKeywordBlock(text: unknown, open: string, close: string): string[] {
  if (typeof text !== 'string') return [];
  const start = text.indexOf(open);
  if (start === -1) return [];
  const from = start + open.length;
  const end = text.indexOf(close, from);
  const body = end === -1 ? text.slice(from) : text.slice(from, end);
  const out: string[] = [];
  const seen = new Set<string>();
  for (const raw of body.split(/[,\n]+/)) {
    const kw = raw.trim().replace(/^#+/, '').replace(/\s+/g, ' ').trim().toLowerCase();
    if (kw.length < 2 || kw.length > 40) continue;
    if (kw.split(' ').length > 4) continue;
    if (seen.has(kw)) continue;
    seen.add(kw);
    out.push(kw);
    if (out.length >= MAX_KEYWORDS) break;
  }
  return out;
}

// Deterministic keywords from the user's message itself, used when the model is
// off OR emits no keyword block. Reuses db.extractContentTerms (the shared
// stopword-aware tokenizer) so "extract keywords from my message" still holds
// without the LLM — the message's content words ARE the search keywords.
function deterministicKeywords(lastUserMessage: string): string[] {
  let terms: string[] = [];
  try {
    terms = db.extractContentTerms(lastUserMessage) || [];
  } catch {
    terms = [];
  }
  return terms.slice(0, MAX_KEYWORDS);
}

// Deterministic tag suggestions, the sole source when the model is off. Mirrors
// the model path's two tiers:
//   broad    — substring match of the last user message against the generic
//              broad vocab, plus tags co-occurring with the active tags;
//   specific — lexical matches over the FULL vocab (db.searchTagsByText), the
//              long-tail the broad pool can't reach.
// Excludes already-active tags, dedups across both tiers, caps each tier.
function deterministicTagMatches(
  lastUserMessage: string,
  activeTags: unknown,
  broadVocab: unknown,
): { broad: string[]; specific: string[] } {
  const activeSet = new Set(
    (Array.isArray(activeTags) ? activeTags : [])
      .map((t) =>
        String(t ?? '')
          .trim()
          .toLowerCase(),
      )
      .filter(Boolean),
  );
  const seen = new Set<string>();
  const broad: string[] = [];
  const specific: string[] = [];
  const pushTo = (arr: string[], tag: unknown, cap: number): void => {
    if (arr.length >= cap) return;
    const t = String(tag ?? '')
      .trim()
      .toLowerCase();
    if (!t || activeSet.has(t) || seen.has(t)) return;
    seen.add(t);
    arr.push(t);
  };

  // broad (a): substring match against the last user message.
  const msg = String(lastUserMessage ?? '').toLowerCase();
  if (msg.trim()) {
    for (const tag of Array.isArray(broadVocab) ? broadVocab : []) {
      const t = String(tag ?? '')
        .trim()
        .toLowerCase();
      if (t && t.length >= 2 && msg.includes(t)) pushTo(broad, t, PER_TIER_CAP);
    }
  }
  // broad (b): co-occurrence with the active tags.
  for (const active of activeSet) {
    let related: Shelfy.TagCount[] = [];
    try {
      related = db.getTagCooccurrence(active) || [];
    } catch {
      related = [];
    }
    for (const r of related) pushTo(broad, r.tag, PER_TIER_CAP);
  }

  // specific: lexical retrieval over the full vocab.
  let matches: Shelfy.TagCount[] = [];
  try {
    matches = db.searchTagsByText(lastUserMessage, { limit: PER_TIER_CAP }) || [];
  } catch {
    matches = [];
  }
  for (const m of matches) pushTo(specific, m.tag, PER_TIER_CAP);

  return { broad, specific };
}

// Multi-turn conversational tag search. Streams the conversational reply token
// by token via onToken (only the text BEFORE the [[TAGS]] sentinel), then
// returns the parsed { reply, tagsToAdd, tagsToRemove, modelUsed }.
//   - messages: [{role:'user'|'assistant', content}], last is the new user turn.
//   - activeTags: tags already applied (for refinement context).
//   - onToken: (token) => void, fired with conversational text chunks.
//   - signal: optional AbortSignal to cancel the in-flight chat.
// When the model is not ready, skips the server entirely and returns
// deterministic matches with modelUsed:false.
async function chatSearch(
  messages: unknown,
  activeTags: string[] = [],
  onToken?: OnToken,
  signal?: AbortSignal,
): Promise<ChatSearchResult> {
  const history = (Array.isArray(messages) ? messages : []).filter(
    (m): m is ChatMessage =>
      !!m && (m.role === 'user' || m.role === 'assistant') && typeof m.content === 'string',
  );
  const lastUser = [...history].reverse().find((m) => m.role === 'user');
  const lastUserMessage = lastUser ? lastUser.content : '';

  const activeSet = new Set(
    (Array.isArray(activeTags) ? activeTags : [])
      .map((t) =>
        String(t ?? '')
          .trim()
          .toLowerCase(),
      )
      .filter(Boolean),
  );

  // Two candidate pools (display forms): broad = tag GENERALI; specific =
  // query-retrieved long-tail tags. Their union è l'allowlist contro cui sono
  // filtrate le scelte del modello.
  // P2: il livello "generale" usa il tier PERSISTITO (db.getTagStats({tier:'general'}))
  // invece di reinventarlo dalla frequenza globale. Se la lista per-tier è vuota
  // (archivio interamente legacy, tier NULL) si ricade sul vecchio comportamento
  // per-frequenza (nessun filtro di tier).
  const broadVocab = getBroadVocab();
  const broadSet = new Set(broadVocab.map((t) => String(t).trim().toLowerCase()));

  const excludeSet = new Set([...broadSet, ...activeSet]);
  const specificVocab = retrieveSpecificTags(lastUserMessage, excludeSet);

  // P7: query expansion PRIMA che l'allowlist filtri le scelte del modello. Una
  // query in linguaggio naturale o con sinonimi ("cuffie wireless" vs "auricolari")
  // può non avere alcun tag-nome che matcha lessicalmente la frase: i tag corretti
  // entrano nel pool solo se prima li facciamo emergere via le PAROLE-CHIAVE del
  // messaggio + distinctiveness, intersecate col vocabolario reale (match esatto →
  // alias → fuzzy). I tag così risolti vengono aggiunti al pool specifico (al netto
  // di esclusi e già presenti) prima della costruzione del prompt. Deterministico,
  // nessuna chiamata LLM extra.
  const expanded = expandQueryToVocab(lastUserMessage);
  const specificSeen = new Set(specificVocab.map((t) => String(t).trim().toLowerCase()));
  for (const tag of expanded) {
    if (specificVocab.length >= SPECIFIC_VOCAB_LIMIT) break;
    const key = String(tag).trim().toLowerCase();
    if (!key || excludeSet.has(key) || specificSeen.has(key)) continue;
    specificSeen.add(key);
    specificVocab.push(tag);
  }

  const vocabSet = new Set(
    [...broadVocab, ...specificVocab].map((t) => String(t).trim().toLowerCase()),
  );

  const deterministic = deterministicTagMatches(lastUserMessage, activeTags, broadVocab);

  // Search KEYWORDS extracted from the user message — the free-text search terms
  // matched against post captions + AI descriptions (traditional search). When the
  // model runs it produces these (parsed below); this deterministic version (the
  // message's own content words) is the fallback for model-not-ready / empty block.
  const fallbackKeywords = deterministicKeywords(lastUserMessage);

  const route = aiRoute('search');
  if (route.mode === 'blocked') {
    return {
      reply: 'Nodo AI non raggiungibile: cerco per parola chiave.',
      tagsToAdd: [...deterministic.broad, ...deterministic.specific],
      tagsToRemove: [],
      keywordsToAdd: fallbackKeywords,
      tagGroups: {
        broad: deterministic.broad,
        specific: deterministic.specific,
        keywords: fallbackKeywords,
      },
      modelUsed: false,
    };
  }
  const remoteProvider = route.mode === 'remote' ? route.provider : null;
  // The remote text model can search the local image index even when the local
  // vision model has not been downloaded.
  if (!remoteProvider && !getModelStatus().ready) {
    return {
      reply: 'Il modello non è pronto: cerco per parola chiave.',
      tagsToAdd: [...deterministic.broad, ...deterministic.specific],
      tagsToRemove: [],
      keywordsToAdd: fallbackKeywords,
      tagGroups: {
        broad: deterministic.broad,
        specific: deterministic.specific,
        keywords: fallbackKeywords,
      },
      modelUsed: false,
    };
  }

  let endpoint: { url: string; headers: Record<string, string>; model?: string };
  if (remoteProvider) {
    endpoint = chatEndpoint(remoteProvider);
  } else {
    inflightCount++;
    if (idleTimer) {
      clearTimeout(idleTimer);
      idleTimer = null;
    }
    try {
      const { port } = await ensureServer();
      endpoint = {
        url: `http://127.0.0.1:${port}/v1/chat/completions`,
        headers: { 'Content-Type': 'application/json' },
      };
    } catch (err) {
      inflightCount--;
      touchServer();
      throw err;
    }
  }
  const ac = new AbortController();
  const onAbort = (): void => ac.abort();
  if (signal) {
    if (signal.aborted) ac.abort();
    else signal.addEventListener('abort', onAbort, { once: true });
  }
  let releaseRemote: (() => void) | null = null;
  try {
    releaseRemote = remoteProvider ? await acquireRemoteRequest(ac.signal) : null;
  } catch (error) {
    if (signal) signal.removeEventListener('abort', onAbort);
    throw error;
  }
  const chatTimeoutMs = remoteProvider ? REMOTE_CHAT_TIMEOUT_MS : CHAT_TIMEOUT_MS;
  const timer = setTimeout(() => ac.abort(), chatTimeoutMs);

  const body = {
    messages: [
      { role: 'system', content: buildChatSystemPrompt(broadVocab, specificVocab, activeTags) },
      ...history.map((m) => ({ role: m.role, content: m.content })),
    ],
    stream: true,
    temperature: task('chat').temperature,
    max_tokens: maxTokens('chat'),
    ...(remoteProvider ? { model: endpoint.model } : { cache_prompt: true }),
    ...remoteThinkingOptions(remoteProvider),
  };

  let full = ''; // entire accumulated model output
  let emitted = 0; // chars of `full` already streamed to onToken
  let stopEmitting = false; // true once a "[[" block marker is reached

  // Streams conversational text via onToken, holding back the last char so a
  // partial "[[" prefix is never emitted. Once the first block marker appears
  // we stop emitting and only keep accumulating for the final parse.
  const flush = (): void => {
    if (stopEmitting || typeof onToken !== 'function') return;
    const idx = full.indexOf(BLOCK_MARKER);
    if (idx !== -1) {
      // Emit the conversational text up to the first marker, then stop forever.
      if (idx > emitted) {
        const chunk = full.slice(emitted, idx);
        if (chunk) onToken(chunk);
      }
      emitted = idx;
      stopEmitting = true;
      return;
    }
    // No marker yet: emit everything except the trailing HOLD chars, which
    // could turn out to be the start of "[[" on the next chunk.
    const HOLD = BLOCK_MARKER.length; // 2 chars
    const safeEnd = Math.max(emitted, full.length - HOLD);
    if (safeEnd > emitted) {
      const chunk = full.slice(emitted, safeEnd);
      if (chunk) onToken(chunk);
      emitted = safeEnd;
    }
  };

  try {
    const res = await fetch(endpoint.url, {
      method: 'POST',
      redirect: 'error',
      headers: endpoint.headers,
      body: JSON.stringify(body),
      signal: ac.signal,
    });
    if (!res.ok) throw new Error(`Chat provider HTTP ${res.status}`);

    // Parse the SSE stream: lines `data: {json}` carry choices[0].delta.content.
    let sseBuf = '';
    const decoder = new TextDecoder();
    const handleLine = (line: string): void => {
      const trimmed = line.trim();
      if (!trimmed.startsWith('data:')) return;
      const payload = trimmed.slice(5).trim();
      if (!payload || payload === '[DONE]') return;
      let json: ChatCompletionChunk;
      try {
        json = JSON.parse(payload) as ChatCompletionChunk;
      } catch {
        return;
      }
      const delta = json.choices?.[0]?.delta?.content;
      if (typeof delta === 'string' && delta) {
        full += delta;
        flush();
      }
    };

    for await (const chunk of res.body as unknown as AsyncIterable<Uint8Array | string>) {
      sseBuf += typeof chunk === 'string' ? chunk : decoder.decode(chunk, { stream: true });
      let nl: number;
      while ((nl = sseBuf.indexOf('\n')) !== -1) {
        handleLine(sseBuf.slice(0, nl));
        sseBuf = sseBuf.slice(nl + 1);
      }
    }
    // Flush the decoder so a trailing multibyte UTF-8 sequence held across the
    // last chunk boundary isn't dropped (corrupting the final accented char/emoji).
    sseBuf += decoder.decode();
    if (sseBuf) handleLine(sseBuf);
    // Final flush in case the stream ended without ever hitting the sentinel.
    flush();
    if (!stopEmitting && typeof onToken === 'function' && full.length > emitted) {
      const chunk = full.slice(emitted);
      if (chunk) onToken(chunk);
      emitted = full.length;
    }
  } catch (err) {
    if (ac.signal.aborted && !(signal && signal.aborted)) {
      throw new Error(`Chat timed out after ${chatTimeoutMs}ms`);
    }
    throw err;
  } finally {
    clearTimeout(timer);
    releaseRemote?.();
    if (signal) signal.removeEventListener('abort', onAbort);
    if (!remoteProvider) {
      inflightCount--;
      touchServer();
    }
  }

  // Parse the two tiers (lenient, intersected with the vocab allowlist). If the
  // model emitted a single legacy [[TAGS]] block instead, treat it as broad.
  let broad = parseTagBlock(full, GENERAL_OPEN, GENERAL_CLOSE, vocabSet);
  const specific = parseTagBlock(full, SPECIFIC_OPEN, SPECIFIC_CLOSE, vocabSet);
  if (broad.length === 0 && specific.length === 0) {
    broad = parseTagBlock(full, TAGS_OPEN, TAGS_CLOSE, vocabSet);
  }
  const tagsToRemove = parseTagBlock(full, REMOVE_OPEN, REMOVE_CLOSE, vocabSet);

  // Trust the model's allowlist-filtered picks. Keep the two tiers separate (so
  // the UI can space them apart) while sharing one dedup + global cap. We do NOT
  // pad with the deterministic net here: it's only a fallback when the model
  // isn't ready (see above). Padding would refill empty slots with generic
  // high-frequency tags — better to propose few relevant tags, or none.
  const seen = new Set<string>();
  const broadOut: string[] = [];
  const specificOut: string[] = [];
  const addTo = (arr: string[], tag: unknown): void => {
    if (seen.size >= MAX_PROPOSED || arr.length >= PER_TIER_CAP) return;
    const t = String(tag ?? '')
      .trim()
      .toLowerCase();
    if (!t || seen.has(t)) return;
    seen.add(t);
    arr.push(t);
  };
  for (const tag of broad) addTo(broadOut, tag);
  for (const tag of specific) addTo(specificOut, tag);

  const tagsToAdd = [...broadOut, ...specificOut];

  // Keywords from the model's [[KEYWORDS]] block (free-form). If the model emitted
  // none, fall back to the message's own content words so a text search still runs.
  let keywords = parseKeywordBlock(full, KEYWORDS_OPEN, KEYWORDS_CLOSE);
  if (keywords.length === 0) keywords = fallbackKeywords;

  const tagGroups = { broad: broadOut, specific: specificOut, keywords };

  // reply = conversational text before the first block marker.
  const markerIdx = full.indexOf(BLOCK_MARKER);
  const reply = (markerIdx === -1 ? full : full.slice(0, markerIdx)).trim();

  return { reply, tagsToAdd, tagsToRemove, keywordsToAdd: keywords, tagGroups, modelUsed: true };
}

// ─── KEYWORD retrieval (deterministic, no LLM) ───────────────────────────────────
//
// The "parole suggerite" pillar: short words/phrases the user might search for in
// the static + AI-generated descriptions. Thin wrapper over db.getKeywordsForTextQuery
// (which mines posts.ai_keywords ranked by distinctiveness to the query's matched
// set). Deterministic and garbage-tolerant; returns an array of strings.
function retrieveKeywords(query: string, { limit = 12 }: { limit?: number } = {}): string[] {
  try {
    return db.getKeywordsForTextQuery(query, { limit }) || [];
  } catch {
    return [];
  }
}

// ─── Job state (mirrors downloader.js) ──────────────────────────────────────────

const jobsMap = new Map<string, AnalyzeJob>(); // key → serializable job record
const postCache = new Map<string, AnalyzePost>(); // key → post (for retry)
const abortMap = new Map<string, AbortController>(); // key → AbortController
const pendingQueue: string[] = [];
const pausedKeys = new Set<string>(); // keys aborted by pause → re-queue instead of cancel
let runningCount = 0;
let isPaused = false;
let onJobUpdate: ((job: AnalyzeJob) => void) | null = null;

// Cap on retained 'done' (non-retryable) jobs. Without an explicit "clear
// completed" action, finished analyses would otherwise accumulate forever in
// jobsMap, postCache and the jobstore SQLite `jobs` table — thousands of records
// for a large library, surviving restart via recover(). We auto-prune the oldest
// 'done' jobs beyond this cap. error/cancelled are retryable so they're left
// alone (the user can retry or clear them explicitly).
const MAX_DONE_JOBS = 200;

// Drop the oldest 'done' jobs once their count exceeds MAX_DONE_JOBS, freeing the
// in-memory record, the cached post, and the durable jobstore row. Ordered by
// finishedAt (fallback queuedAt) so the most recent completions stay visible.
function pruneDoneJobs(): void {
  const done: AnalyzeJob[] = [];
  for (const job of jobsMap.values()) if (job.status === 'done') done.push(job);
  if (done.length <= MAX_DONE_JOBS) return;
  done.sort((a, b) => (a.finishedAt || a.queuedAt || 0) - (b.finishedAt || b.queuedAt || 0));
  const drop = done.slice(0, done.length - MAX_DONE_JOBS);
  for (const job of drop) {
    jobsMap.delete(job.key);
    postCache.delete(job.key);
    jobstore.forget(KIND, job.key);
  }
}

function jobKey(postId: string): string {
  return `${postId}:analyze`;
}
function detectPlatform(post: AnalyzePost): string {
  if (isWebPost(post)) return 'web';
  return (post as Shelfy.Post).platform || (post.shortcode ? 'instagram' : 'twitter');
}

// A web reference, whatever path queued it: the bulk paths (analyze:posts /
// analyze:missing) hand over LIGHT posts that carry media_type but no platform.
function isWebPost(post: AnalyzePost): boolean {
  return (post as Shelfy.Post).platform === 'web' || post.mediaType === 'website';
}

// Transient fields that drive only the live UI and are NOT persisted by jobstore
// (streamText is in jobstore's HEAVY_KEYS; stage/phaseProgress/indeterminate are
// regenerated on resume). A patch touching only these doesn't change the durable
// row, so we update jobsMap + emit to the renderer but skip the synchronous
// SQLite mirror — avoiding ~12 redundant DB writes/sec/job during streaming.
const TRANSIENT_KEYS = new Set(['streamText', 'stage', 'phaseProgress', 'indeterminate']);

function setJob(job: AnalyzeJob, { persist = true }: { persist?: boolean } = {}): void {
  jobsMap.set(job.key, { ...job });
  if (persist) jobstore.mirror(KIND, job);
  onJobUpdate?.({ ...job });
  // 'done' is terminal and non-retryable (retryJob only handles error/cancelled),
  // so the cached post is no longer needed for this key. Drop it immediately and
  // bound the retained-done set so the queue can't grow without limit.
  if (job.status === 'done') {
    postCache.delete(job.key);
    pruneDoneJobs();
  }
}

function patchJob(key: string, patch: Partial<AnalyzeJob>): void {
  const j = jobsMap.get(key);
  if (!j) return;
  // If every changed field is transient/UI-only, skip the durable mirror — the
  // persisted row content is identical, so the SQLite write would be pure waste.
  const persist = Object.keys(patch).some((k) => !TRANSIENT_KEYS.has(k));
  setJob({ ...j, ...patch }, { persist });
}

// `route` is resolved once by pumpQueue (which never passes 'blocked'), so every
// job of a pump pass targets the same model.
async function runJob(key: string, route: AiRoute): Promise<void> {
  const job = jobsMap.get(key);
  const cached = postCache.get(key);
  if (!job || !cached || route.mode === 'blocked') return;
  // Web references are re-read in full: the light posts handed over by the bulk
  // paths lack the web_* fields (tech stack, pages, metadata) the web prompt needs.
  const post: AnalyzePost = isWebPost(cached) ? (db.getPost(cached.id) ?? cached) : cached;

  const ac = new AbortController();
  abortMap.set(key, ac);
  runningCount++;
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
  // startedAt drives the renderer's ETA / "last tag took" stats; finishedAt +
  // durationMs are stamped on every terminal transition below. stage/phaseProgress/
  // indeterminate feed the per-job phase detail in the AI Tags queue.
  const startedAt = Date.now();
  const visionProvider = route.mode === 'remote' ? route.provider : null;
  const modelName = visionProvider?.name || getSelectedModel().name;
  patchJob(key, {
    status: 'extracting',
    progress: 0,
    error: null,
    startedAt,
    finishedAt: null,
    durationMs: null,
    stage: 'Preparazione…',
    phaseProgress: 0,
    indeterminate: false,
    model: modelName,
    streamText: null,
  });

  try {
    if (!getModelStatus().ready && !visionProvider) throw new Error('MODEL_NOT_READY');
    // Web references: structured design catalog v2 (see webcap/ai-catalog.ts).
    if (isWebPost(post) && Array.isArray((post as Shelfy.Post).webPages)) {
      let lastEmit = 0;
      const onToken = (full: string): void => {
        const t = Date.now();
        if (t - lastEmit < 80) return;
        lastEmit = t;
        patchJob(key, { streamText: full });
      };
      db.updateAiAnalysis(post.id, { status: 'analyzing' });
      const { mapped, modelUsed } = await analyzeWebCatalog(
        post as Shelfy.Post,
        ac.signal,
        (stage, frac) =>
          patchJob(key, {
            status: frac === undefined ? 'analyzing' : 'extracting',
            stage,
            ...(frac === undefined
              ? { progress: 0.5, phaseProgress: null, indeterminate: true, streamText: '' }
              : { phaseProgress: frac, progress: frac * 0.45 }),
          }),
        onToken,
        visionProvider,
      );
      mapped.catalog.model = modelUsed;
      db.updateAiAnalysis(post.id, {
        description: mapped.description,
        tags: mapped.tags,
        generalTags: mapped.generalTags,
        specificTags: mapped.specificTags,
        entities: mapped.entities,
        keywords: mapped.keywords,
        language: mapped.language,
        saveReason: mapped.saveReason,
        category: mapped.category,
        contentType: mapped.contentType,
        webAi: mapped.catalog,
        status: 'done',
        model: modelUsed,
      });
      const finishedAt = Date.now();
      patchJob(key, {
        status: 'done',
        model: modelUsed,
        progress: 1,
        description: mapped.description,
        tags: mapped.tags,
        entities: mapped.entities,
        keywords: mapped.keywords,
        saveReason: mapped.saveReason,
        language: mapped.language,
        finishedAt,
        durationMs: finishedAt - startedAt,
        stage: null,
        phaseProgress: 1,
        indeterminate: false,
        streamText: null,
      });
      return;
    }
    const frames = await collectVisualInputs(
      post,
      ac.signal,
      ({ stage, frac } = {} as StageProgress) => {
        patchJob(key, { stage, phaseProgress: frac ?? 0, progress: (frac ?? 0) * 0.45 });
      },
    );
    patchJob(key, {
      status: 'analyzing',
      progress: 0.5,
      phaseProgress: null,
      indeterminate: true,
      stage: `Interrogazione del modello (${modelName})…`,
      streamText: '',
    });
    db.updateAiAnalysis(post.id, { status: 'analyzing' });

    // Throttle the live stream to ~12 updates/s: each emit ships the whole job
    // record over IPC, and the model produces tokens far faster than the UI needs.
    let lastEmit = 0;
    const onToken = (full: string): void => {
      const t = Date.now();
      if (t - lastEmit < 80) return;
      lastEmit = t;
      patchJob(key, { streamText: full });
    };

    // Web posts (platform==='web') use the web_catalog schema + web prompt; everything
    // else stays on the social path. For the web, frequentTags carries the deterministic
    // tech stack (post.webTech) so it lands in entities (see buildWebUserPrompt/analyzeFrames).
    const kind: AnalyzeKind = isWebPost(post) ? 'web' : 'social';
    const freq =
      kind === 'web'
        ? Array.isArray((post as Shelfy.Post).webTech)
          ? (post as Shelfy.Post).webTech
          : []
        : db.getFrequentTags();
    const result = await analyzeFrames(
      frames,
      post.text,
      freq,
      ac.signal,
      (stage) => patchJob(key, { stage }),
      onToken,
      kind,
      visionProvider,
    );
    const {
      description,
      tags,
      generalTags,
      specificTags,
      entities,
      keywords,
      saveReason,
      language,
      category,
      contentType,
    } = result;
    db.updateAiAnalysis(post.id, {
      description,
      tags,
      entities,
      keywords,
      language,
      saveReason,
      generalTags,
      specificTags,
      // category/contentType are populated only for web (purpose→ai_content_type,
      // industry→ai_category); undefined on the social path → columns untouched.
      category,
      contentType,
      status: 'done',
      model: result.modelUsed || modelName,
    });
    const finishedAt = Date.now();
    patchJob(key, {
      status: 'done',
      model: result.modelUsed || modelName,
      progress: 1,
      description,
      tags,
      entities,
      keywords,
      saveReason,
      language,
      finishedAt,
      durationMs: finishedAt - startedAt,
      stage: null,
      phaseProgress: 1,
      indeterminate: false,
      streamText: null,
    });
  } catch (err) {
    // clearAll() may have emptied jobsMap while this job was in flight; writing
    // its status back to DB now would resurrect an incoherent 'analyzing'/'error'
    // record. Bail before any DB/job writes (the finally still runs).
    if (!jobsMap.has(key)) return;
    const e = err as { name?: string; message?: string } | undefined;
    const isAbort = e?.name === 'AbortError' || e?.message === 'AbortError';
    if ((isAbort && pausedKeys.has(key)) || e?.message === REMOTE_UNAVAILABLE) {
      // Paused (or the remote node dropped), not cancelled: return to the FRONT
      // of the queue (the work is discarded — analysis restarts from scratch on
      // resume) so this job is the first to pick back up. Reset timing/phase fields.
      pausedKeys.delete(key);
      patchJob(key, {
        status: 'pending',
        progress: 0,
        error: null,
        startedAt: null,
        finishedAt: null,
        durationMs: null,
        stage: null,
        phaseProgress: null,
        indeterminate: false,
        streamText: null,
      });
      db.updateAiAnalysis(post.id, { status: null });
      if (!pendingQueue.includes(key)) pendingQueue.unshift(key);
    } else if (isAbort) {
      const finishedAt = Date.now();
      patchJob(key, {
        status: 'cancelled',
        progress: 0,
        finishedAt,
        durationMs: finishedAt - startedAt,
        stage: null,
        indeterminate: false,
        streamText: null,
      });
      db.updateAiAnalysis(post.id, { status: null });
    } else {
      const finishedAt = Date.now();
      const msg = e?.message || String(err);
      console.warn(`[analyzer] ${post.id}: ${msg}`);
      patchJob(key, {
        status: 'error',
        progress: 0,
        error: msg,
        finishedAt,
        durationMs: finishedAt - startedAt,
        stage: null,
        indeterminate: false,
        streamText: null,
      });
      db.updateAiAnalysis(post.id, { status: 'error' });
    }
  } finally {
    abortMap.delete(key);
    runningCount--;
    touchServer();
    pumpQueue();
  }
}

function pumpQueue(): void {
  if (isPaused || pendingQueue.length === 0) return;
  // Hold the queue while the configured node is unreachable; a status change
  // (node back, or the user opting into local models) pumps it again.
  const route = aiRoute('vision');
  if (route.mode === 'blocked') return;
  while (runningCount < getConcurrency() && pendingQueue.length > 0) {
    const key = pendingQueue.shift()!;
    const job = jobsMap.get(key);
    if (job?.status === 'pending') runJob(key, route);
  }
}

onRemoteStatusChange(() => pumpQueue());

// ─── Public API ─────────────────────────────────────────────────────────────────

// True if the post has any LOCAL visual asset on disk (video, image, thumbnail,
// or a downloaded carousel slide). Not limited to videos anymore.
function canAnalyze(post: AnalyzePost | null | undefined): boolean {
  if (!post) return false;
  // A not-downloaded video is too imprecise to tag: without the file on disk we
  // could only fall back to its cover thumbnail / caption, which misrepresents a
  // moving clip. So a video is analyzable ONLY when its file is actually on disk
  // (frame extraction). Images/carousels/text bookmarks keep the looser rule
  // below (thumbnail or caption is enough).
  if (post.mediaType === 'video') {
    return typeof post.videoPath === 'string' && fs.existsSync(post.videoPath);
  }
  const candidates: Array<string | null | undefined> = [
    post.videoPath,
    post.imagePath,
    post.thumbnailPath,
  ];
  if (Array.isArray(post.media)) {
    for (const m of post.media) if (m && m.localPath) candidates.push(m.localPath);
  }
  if (candidates.some((p) => p && fs.existsSync(p))) return true;
  // Text-only fallback: no visual asset, but a caption we can categorize.
  return typeof post.text === 'string' && post.text.trim().length > 0;
}

function enqueuePost(
  post: AnalyzePost,
  { pump = true }: { pump?: boolean } = {},
): { queued: boolean } {
  if (!canAnalyze(post)) return { queued: false };
  const key = jobKey(post.id);
  const existing = jobsMap.get(key);
  if (existing && ['pending', 'extracting', 'analyzing'].includes(existing.status))
    return { queued: false };

  postCache.set(key, post);
  setJob({
    key,
    postId: post.id,
    platform: detectPlatform(post),
    status: 'pending',
    progress: 0,
    error: null,
    authorUsername: post.authorUsername ?? null,
    thumbnailUrl: post.thumbnailUrl ?? null,
    thumbnailPath: post.thumbnailPath ?? null,
    mediaType: post.mediaType ?? null,
    queuedAt: Date.now(),
    startedAt: null,
    finishedAt: null,
    durationMs: null,
    stage: null,
    phaseProgress: null,
    indeterminate: false,
    model: null,
    streamText: null,
  });
  if (!pendingQueue.includes(key)) pendingQueue.push(key);
  // Bulk callers (enqueueMany) defer the pump to a single call after the whole
  // batch is queued, avoiding N redundant pumpQueue scans in a tight loop.
  if (pump) pumpQueue();
  return { queued: true };
}

function enqueueMany(posts: AnalyzePost[]): { queued: number } {
  let queued = 0;
  // Enqueue the whole batch first (no per-post pump), then drain once. Cuts the
  // O(n) pumpQueue calls of the old loop down to a single pass.
  for (const post of posts) if (enqueuePost(post, { pump: false }).queued) queued++;
  pumpQueue();
  return { queued };
}

function cancelJob(key: string): void {
  pausedKeys.delete(key);
  abortMap.get(key)?.abort();
  const qi = pendingQueue.indexOf(key);
  if (qi >= 0) pendingQueue.splice(qi, 1);
  const job = jobsMap.get(key);
  if (job && job.status !== 'done') patchJob(key, { status: 'cancelled', progress: 0 });
}

function cancelAll(): void {
  // Cancel every queued AND in-flight job. The running job lives in abortMap but
  // not in pendingQueue, so handling only the queue would leave it 'analyzing'
  // until its fetch/ffmpeg unwinds — making the UI count stick at 1 and forcing
  // a second press. Routing all keys through cancelJob marks each 'cancelled'
  // immediately (and aborts the in-flight work).
  const keys = new Set([...pendingQueue, ...abortMap.keys()]);
  for (const key of keys) cancelJob(key);
  pendingQueue.length = 0;
  pausedKeys.clear();
  isPaused = false;
  writeConfig({ paused: false });
}

// Hard reset: abort in-flight work, drop the queue, and wipe every record so the
// list empties. Aborted jobs hit runJob's catch after jobsMap is cleared, where
// patchJob is a no-op (the key is gone) — so no stray 'cancelled' events fire.
function clearAll(): void {
  for (const ac of abortMap.values()) {
    try {
      ac.abort();
    } catch {}
  }
  pendingQueue.length = 0;
  pausedKeys.clear();
  isPaused = false;
  writeConfig({ paused: false });
  jobsMap.clear();
  postCache.clear();
  jobstore.forgetAll(KIND);
}

// ── Pause / resume (mirrors downloader.js) ──────────────────────────────────────

function pauseAll(): { paused: true } {
  isPaused = true;
  // Persist so a restart doesn't silently auto-resume: recover() reads this flag
  // and leaves the queue paused instead of pumping it.
  writeConfig({ paused: true });
  // Abort the in-flight analysis to free the machine immediately; runJob re-queues
  // it as pending (pausedKeys branch) so resume restarts it from scratch.
  for (const [key, job] of jobsMap) {
    if (job.status === 'extracting' || job.status === 'analyzing') {
      pausedKeys.add(key);
      abortMap.get(key)?.abort();
    }
  }
  return { paused: true };
}

function resumeAll(): { paused: false } {
  isPaused = false;
  writeConfig({ paused: false });
  pumpQueue();
  return { paused: false };
}

function getIsPaused(): boolean {
  return isPaused;
}

function retryJob(key: string): void {
  const job = jobsMap.get(key);
  if (!job || (job.status !== 'error' && job.status !== 'cancelled')) return;
  patchJob(key, {
    status: 'pending',
    progress: 0,
    error: null,
    queuedAt: Date.now(),
    startedAt: null,
    finishedAt: null,
    durationMs: null,
    stage: null,
    phaseProgress: null,
    indeterminate: false,
    streamText: null,
  });
  if (!pendingQueue.includes(key)) pendingQueue.push(key);
  pumpQueue();
}

function clearCompleted(): void {
  for (const [key, job] of jobsMap) {
    if (job.status === 'done' || job.status === 'cancelled') {
      jobsMap.delete(key);
      postCache.delete(key);
      jobstore.forget(KIND, key);
    }
  }
}

// Boot recovery: re-enqueue analyses interrupted by a previous run. First reset
// any post stuck at ai_status='analyzing' (covers orphans without a job row too),
// then re-enqueue each persisted non-terminal job by rehydrating its post. The
// analysis restarts from scratch (it isn't checkpointed mid-run).
function recover(): { recovered: number } {
  try {
    db.clearStuckAnalyzing();
  } catch (e) {
    console.warn('[analyzer] clearStuckAnalyzing failed:', (e as Error)?.message);
  }
  // Restore the persisted pause state BEFORE re-enqueuing: enqueuePost pumps the
  // queue internally, and the pumpQueue guard (if (isPaused) return) only holds
  // back work if isPaused is already true. Without this, a queue paused before
  // quit would silently auto-resume on the next launch.
  if (readConfig().paused === true) isPaused = true;
  const rows = jobstore.resumable(KIND);
  let recovered = 0;
  const keep = new Set<string>();
  for (const job of rows) {
    if (!job.postId) continue;
    let post: Shelfy.Post | null | undefined;
    try {
      post = db.getPost(job.postId);
    } catch {
      // Transient DB failure: keep this job's row so the next boot retries it
      // instead of silently losing the analysis.
      keep.add(job.key);
      continue;
    }
    if (!post) continue; // post deleted → its stale row is cleaned up below
    if (enqueuePost(post).queued) {
      recovered++;
    } else if (!jobsMap.has(job.key)) {
      // Not analyzable right now (e.g. asset not on disk yet): keep the durable
      // row so the analysis intent survives until the post becomes analyzable.
      keep.add(job.key);
    }
  }
  // Re-enqueue FIRST, forget AFTER: enqueuePost → setJob has already re-mirrored
  // every live key into the jobstore, so there is no window where a resumable
  // job lacks a durable row if the app dies mid-recovery (see jobstore.js).
  for (const key of jobsMap.keys()) keep.add(key);
  jobstore.forgetExcept(KIND, keep);
  if (recovered > 0)
    console.log(`[analyzer] recovered ${recovered} analysis job(s) into the queue`);
  return { recovered };
}

function getJobs(): AnalyzeJob[] {
  return Array.from(jobsMap.values());
}

function setProgressEmitter(fn: ((job: AnalyzeJob) => void) | null): void {
  onJobUpdate = fn;
}

export {
  enqueuePost,
  enqueueMany,
  getJobs,
  cancelJob,
  cancelAll,
  clearAll,
  pauseAll,
  resumeAll,
  getIsPaused,
  retryJob,
  clearCompleted,
  recover,
  setProgressEmitter,
  getModelStatus,
  getAnalysisModelStatus,
  listModels,
  setModel,
  getConcurrency,
  setConcurrency,
  MAX_CONCURRENCY,
  getTuning,
  setTuning,
  getHardwareInfo,
  setVariantFallbackHandler,
  downloadModel,
  pauseModelDownload,
  cancelModelDownload,
  deleteModel,
  shutdown,
  forceShutdown,
  getTaxonomy,
  assessScreenshot,
  expandSearchQuery,
  chatSearch,
  retrieveKeywords,
  clusterTags,
  refineTagGroups,
  buildTagAliases,
  // Pure helpers exported for unit testing (no behavior change).
  runInference,
  analyzeFrames,
  buildUserPrompt,
  buildChatSystemPrompt,
  retrieveSpecificTags,
  cleanStringArray,
  canAnalyze,
  validateRefinedGroups,
  parseRefineResponse,
  intersectWithVocab,
  expandQueryToVocab,
  validateAliasPairs,
  // The desktop functions the web port is checked against (golden fixtures,
  // scripts/golden/ai-*.ts). The catalog ones live in shared/ai/catalog.ts.
  normalizeCatalogOutput,
  stripPromptMarkers,
  buildWebUserPrompt,
  parseTagBlock,
  parseKeywordBlock,
  deterministicKeywords,
  deterministicTagMatches,
};
