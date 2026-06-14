// Batch test del modulo reale electron/analyzer.js su 10 job misti
// (video + immagini + testo). Mocka `electron` e `./db`, costruisce i post da
// fixture estratte dal DB reale (vedi i file /tmp/sb_*.json generati via sqlite3),
// li accoda TUTTI insieme e ne osserva l'esecuzione end-to-end.
//
// Serve anche da test di robustezza per il bug KV-cache #17200 di llama.cpp:
// 10 richieste consecutive sullo stesso server warm devono completare tutte.
//
// Uso: node scripts/analyzer-batch-test.cjs

import Module from 'module';
import path from 'path';
import os from 'os';
import fs from 'fs';
import type { app as ElectronApp } from 'electron';

// Node exposes the internal CommonJS loader on the Module object; @types/node does
// not declare it, so we describe just the slice we monkeypatch.
type ModuleLoad = (request: string, parent?: unknown, isMain?: boolean) => unknown;
interface ModuleWithLoad {
  _load: ModuleLoad;
}

type Analyzer = typeof import('../electron/analyzer');

// The progress emitter delivers an AnalyzeJob; category/contentType are not part
// of that record (the analyzer only writes them to the DB), but the original
// script reads them defensively, so we keep them as optional reads.
type AnalyzerJob = Parameters<NonNullable<Parameters<Analyzer['setProgressEmitter']>[0]>>[0];
type ProgressJob = AnalyzerJob & { category?: string; contentType?: string };

// One mixed post fed to enqueueMany, plus the local `_kind` bookkeeping tag.
type Kind = 'video' | 'image' | 'text';
interface MixedPost {
  id: string;
  mediaType: Shelfy.MediaType;
  videoPath?: string | null;
  imagePath?: string | null;
  thumbnailPath?: string | null;
  media?: Shelfy.PostMedia[];
  text: string | null;
  _kind: Kind;
}

// A terminal record collected per job key.
interface TerminalRecord {
  status: AnalyzerJob['status'];
  secs: string;
  kind: Kind | undefined;
  category: string | undefined;
  contentType: string | undefined;
  tags: string[] | undefined;
  description: string | undefined;
  error: string | null | undefined;
}

const USERDATA = path.join(os.homedir(), 'Library/Application Support/Shelfy');
const persisted: Array<{ id: string; status: unknown }> = [];

const origLoad = (Module as unknown as ModuleWithLoad)._load;
(Module as unknown as ModuleWithLoad)._load = function (this: unknown, request: string): unknown {
  if (request === 'electron') {
    return {
      app: { getPath: (k: string) => (k === 'userData' ? USERDATA : os.tmpdir()) },
    } satisfies { app: Pick<typeof ElectronApp, 'getPath'> };
  }
  if (request === './db') {
    return {
      updateAiAnalysis: (id: string, patch: { status: unknown }) =>
        persisted.push({ id, status: patch.status }),
      getFrequentTags: () => [],
    };
  }
  // eslint-disable-next-line prefer-rest-params
  return origLoad.apply(this, arguments as unknown as Parameters<ModuleLoad>);
};

const analyzer = require(path.join(__dirname, '..', 'electron', 'analyzer.js')) as Analyzer;

interface FixtureRow {
  id: string | number;
  videoPath?: string | null;
  thumbnailPath?: string | null;
  imagePath?: string | null;
  text?: string | null;
}

function load(file: string): FixtureRow[] {
  try {
    const v: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
    return Array.isArray(v) ? (v as FixtureRow[]) : [];
  } catch {
    return [];
  }
}

// ── Build 10 mixed posts from real data ──────────────────────────────────────
const videoRows = load('/tmp/sb_video.json')
  .filter((r) => r.videoPath && fs.existsSync(r.videoPath))
  .slice(0, 4);
const imageRows = load('/tmp/sb_image.json')
  .filter((r) => r.thumbnailPath && fs.existsSync(r.thumbnailPath))
  .slice(0, 3);
const textRows = load('/tmp/sb_text.json')
  .filter((r) => typeof r.text === 'string' && r.text.trim().length > 0)
  .slice(0, 3);

const posts: MixedPost[] = [
  ...videoRows.map(
    (r): MixedPost => ({
      id: String(r.id),
      mediaType: 'video',
      videoPath: r.videoPath,
      thumbnailPath: r.thumbnailPath ?? null,
      text: r.text ?? null,
      _kind: 'video',
    }),
  ),
  ...imageRows.map(
    (r): MixedPost => ({
      id: String(r.id),
      mediaType: 'image',
      imagePath: null,
      thumbnailPath: r.thumbnailPath,
      media: [],
      text: r.text ?? null,
      _kind: 'image',
    }),
  ),
  ...textRows.map(
    (r): MixedPost => ({
      id: String(r.id),
      mediaType: 'text',
      text: r.text ?? null,
      _kind: 'text',
    }),
  ),
];

const kindByKey = new Map<string, Kind>();
for (const p of posts) kindByKey.set(`${p.id}:analyze`, p._kind);

console.log(`model status:`, analyzer.getModelStatus().ready ? 'ready' : 'NOT READY');
console.log(
  `enqueuing ${posts.length} jobs: ${videoRows.length} video, ${imageRows.length} image, ${textRows.length} text\n`,
);

const t0 = Date.now();
const startedAt = new Map<string, number>();
const terminal = new Map<string, TerminalRecord>(); // key → { status, secs, category, contentType, tags, error }
const TOTAL = posts.length;
// Numero di job realmente accodati: i post non analizzabili (canAnalyze=false)
// non emettono mai progresso, quindi la fine va valutata su questo valore e non
// su TOTAL, altrimenti il run resta appeso fino al timeout globale.
let target = TOTAL;

analyzer.setProgressEmitter((rawJob) => {
  const job = rawJob as ProgressJob;
  if (!startedAt.has(job.key) && job.status !== 'pending') startedAt.set(job.key, Date.now());
  if (['done', 'error', 'cancelled'].includes(job.status) && !terminal.has(job.key)) {
    const secs = ((Date.now() - (startedAt.get(job.key) || t0)) / 1000).toFixed(1);
    terminal.set(job.key, {
      status: job.status,
      secs,
      kind: kindByKey.get(job.key),
      category: job.category,
      contentType: job.contentType,
      tags: job.tags,
      description: job.description,
      error: job.error,
    });
    const r = terminal.get(job.key)!;
    const head = `[${(r.kind ?? '').padEnd(5)}] ${job.key.split(':')[0]}`;
    if (r.status === 'done') {
      console.log(
        `✓ ${head} (${r.secs}s) — ${r.category} / ${r.contentType} — tags: ${(r.tags || []).join(', ')}`,
      );
    } else {
      console.log(`✗ ${head} (${r.secs}s) — ${r.status}${r.error ? ': ' + r.error : ''}`);
    }
    if (terminal.size === target) finish();
  }
});

function finish(): void {
  const wall = ((Date.now() - t0) / 1000).toFixed(1);
  const done = [...terminal.values()].filter((r) => r.status === 'done');
  const failed = [...terminal.values()].filter((r) => r.status !== 'done');
  console.log(`\n─── SUMMARY ───`);
  // Denominator is `target` (the jobs actually enqueued), not TOTAL: posts that
  // weren't enqueueable (canAnalyze=false / duplicate key) never produce a terminal
  // event, so comparing against TOTAL would report a false failure.
  console.log(
    `completati: ${done.length}/${target}  falliti: ${failed.length}  tempo totale: ${wall}s`,
  );
  const byKind: Record<string, { ok: number; ko: number }> = {};
  for (const r of terminal.values()) {
    const k = r.kind ?? 'unknown';
    byKind[k] = byKind[k] || { ok: 0, ko: 0 };
    if (r.status === 'done') byKind[k].ok++;
    else byKind[k].ko++;
  }
  console.log(`per modalità:`, JSON.stringify(byKind));
  const allDone = target > 0 && done.length === target;
  console.log(
    `KV-cache #17200: ${
      allDone
        ? 'OK (tutte le richieste consecutive completate)'
        : target === 0
          ? 'NESSUN JOB ACCODABILE — verificare le fixture'
          : 'POSSIBILE PROBLEMA — verificare i falliti'
    }`,
  );
  // sample text/image descriptions
  const oneText = [...terminal.values()].find((r) => r.kind === 'text' && r.status === 'done');
  const oneImg = [...terminal.values()].find((r) => r.kind === 'image' && r.status === 'done');
  if (oneText) console.log(`\nesempio TEXT → ${oneText.description}`);
  if (oneImg) console.log(`esempio IMAGE → ${oneImg.description}`);
  analyzer.shutdown();
  process.exit(failed.length ? 1 : 0);
}

const { queued } = analyzer.enqueueMany(posts as unknown as Parameters<Analyzer['enqueueMany']>[0]);
target = queued;
if (queued < TOTAL) {
  console.log(
    `scartati ${TOTAL - queued} post non accodati (non analizzabili o duplicati): attesi ${queued} job terminali\n`,
  );
}
// Se nessun post è risultato accodabile, non arriverà alcun evento di
// progresso: chiudiamo subito invece di restare appesi fino al timeout.
if (queued === 0) finish();

// Safety timeout: 10 jobs × up to a couple minutes each.
setTimeout(() => {
  console.error('\nTIMEOUT globale');
  analyzer.shutdown();
  process.exit(2);
}, 20 * 60_000);
