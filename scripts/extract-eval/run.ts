// Extraction eval harness. Runs the REAL electron/analyzer.js extraction pipeline
// on a fixed set of 20 posts (cases.json) against a single shared, already-running
// llama-server (SHELFY_EXTERNAL_LLAMA_PORT, default 8099 = the app's gemma3-12b),
// then scores the model's tags/keywords against the ground-truth oracle.
//
// Run under electron-as-node (better-sqlite3 ABI):
//   npm run eval:extract            # all 20 posts, score vs ground-truth.json
//   npm run eval:extract -- --limit=4
//   npm run eval:extract -- --ids=2042584997689737464,1812372254266724856
//
// DON'T edit this file or score.cjs or cases.json during tuning — they're the contract.

import path from 'path';
import os from 'os';
import fs from 'fs';
import { scoreCase, type GroundTruth, type ModelOutput, type CaseScore } from './score';
import { summarize } from '../lib/agg-stats';

// Internal Module._load hook surface (not part of the public `module` typings).
type ModuleLoad = (this: unknown, request: string, ...rest: unknown[]) => unknown;
interface ModuleWithLoad {
  _load: ModuleLoad;
}

// ── shapes ────────────────────────────────────────────────────────────────────
interface CaseMedia {
  type?: string;
  localPath?: string | null;
}
interface CasePost {
  id: string | number;
  mediaType: string;
  videoPath?: string | null;
  imagePath?: string | null;
  thumbnailPath?: string | null;
  media?: CaseMedia[];
  text?: string;
}
interface CasesFile {
  posts: CasePost[];
  frequentTags?: string[];
}

// Terminal job record emitted by the analyzer progress emitter.
interface AnalyzeJob extends ModelOutput {
  key: string;
  postId: string;
  status: string;
  error?: string | null;
}

// The slice of electron/analyzer.js this harness drives.
interface Analyzer {
  setProgressEmitter: (fn: (job: AnalyzeJob) => void) => void;
  enqueueMany: (posts: CasePost[]) => unknown;
  shutdown: () => void;
}

// db shim surface (updateAiAnalysis patch we read `status` from).
interface AiAnalysisPatch {
  status?: string;
}

const HERE = __dirname;
const SCRATCH = path.join(HERE, '.scratch');
const USERDATA = path.join(SCRATCH, 'userdata');
const PORT = Number(process.env.SHELFY_EXTERNAL_LLAMA_PORT || pickArg('port') || 8099);
const MODEL_ID = process.env.EVAL_MODEL_ID || 'gemma3-12b';
const PER_POST_TIMEOUT = Number(pickArg('timeout') || 240) * 1000;

function pickArg(name: string): string | null {
  const a = process.argv.find((x) => x.startsWith(`--${name}=`));
  return a ? a.split('=')[1] : null;
}

const CASES = JSON.parse(fs.readFileSync(path.join(HERE, 'cases.json'), 'utf8')) as CasesFile;
const GT_PATH = path.join(HERE, 'ground-truth.json');
const GT = fs.existsSync(GT_PATH)
  ? (JSON.parse(fs.readFileSync(GT_PATH, 'utf8')) as Record<string, GroundTruth>)
  : null;

// ── P11: riproducibilità & multi-run ─────────────────────────────────────────
// L'estrazione VLM è non deterministica: senza temperature 0 due run sullo stesso
// post danno tag diversi. Per la RIPRODUCIBILITÀ vai a T=0.
//
// NB: la temperature è hardcoded in electron/analyzer.js (es. temperature: 0.2 nel
// path di estrazione tag) e questo harness NON può modificarla dall'esterno
// (l'analyzer non legge un override d'ambiente). Per misure riproducibili:
//   1) imposta a 0 la temperature del path di estrazione in electron/analyzer.js, OPPURE
//   2) usa --runs=K (qui sotto) per stimare la dispersione del composito tra run.
// Vedi README.md, sezione "P11 — riproducibilità (T=0)".
//
// --runs=K: ri-esegue l'INTERA estrazione K volte e riporta, per ogni post,
// mediana/varianza/IQR del composito sui K run. Default 1 (contratto single-shot
// invariato quando il flag è assente).
const RUNS = Math.max(1, Number(pickArg('runs')) || 1);

// ── score-only mode: re-score a saved raw outputs file vs ground-truth ───────
// (no inference, no server) — lets us score a captured baseline after authoring GT.
const scoreRaw = pickArg('score-raw');
if (scoreRaw) {
  if (!GT) {
    console.error('[eval] --score-raw needs ground-truth.json');
    process.exit(1);
  }
  const raw = JSON.parse(fs.readFileSync(scoreRaw, 'utf8')) as Array<
    ModelOutput & { id: string | number }
  >;
  const byId = new Map(raw.map((r) => [String(r.id), r]));
  const rows: Array<{ id: string; mediaType: string; s: CaseScore }> = [];
  for (const p of CASES.posts) {
    const id = String(p.id);
    const gt = GT[id];
    const out = byId.get(id);
    if (!gt || !out) continue;
    rows.push({ id, mediaType: p.mediaType, s: scoreCase(gt, out) });
  }
  const mean = (f: (s: CaseScore) => number): number =>
    rows.reduce((a, r) => a + f(r.s), 0) / (rows.length || 1);
  const passed = rows.filter((r) => r.s.composite >= 0.55).length;
  console.log('─── PER-POST (score-raw) ───');
  for (const r of rows) {
    const s = r.s;
    console.log(
      `${s.composite >= 0.55 ? 'PASS' : 'FAIL'} ${s.composite.toFixed(3)} ${r.mediaType.padEnd(8)} ${r.id.slice(0, 22).padEnd(22)} R=${s.recall.toFixed(3)} cov=${s.coverage.toFixed(3)} fbd=${s.forbiddenRate.toFixed(3)} kw=${s.kwScore.toFixed(3)} subj=${s.subjectHit}`,
    );
    if (s.missedMust && s.missedMust.length)
      console.log(`        missed: ${s.missedMust.join(', ')}`);
    if (s.forbiddenHits && s.forbiddenHits.length)
      console.log(`        FORBIDDEN: ${s.forbiddenHits.join(', ')}`);
    console.log(`        tags: ${(s.tags || []).join(', ')}`);
  }
  console.log(
    `\nPASS ${passed}/${rows.length}  meanComposite=${mean((s) => s.composite).toFixed(3)}  recall=${mean((s) => s.recall).toFixed(3)}  coverage=${mean((s) => s.coverage).toFixed(3)}  forbidden=${mean((s) => s.forbiddenRate).toFixed(3)}  kw=${mean((s) => s.kwScore).toFixed(3)}  subj=${mean((s) => s.subjectHit).toFixed(3)}`,
  );
  process.exit(0);
}

// ── scratch userData: force the app's model + symlink real weights ───────────
function setupScratch(): void {
  fs.mkdirSync(USERDATA, { recursive: true });
  fs.writeFileSync(path.join(USERDATA, 'ai-model.json'), JSON.stringify({ modelId: MODEL_ID }));
  const realModels = path.join(os.homedir(), 'Library', 'Application Support', 'Shelfy', 'models');
  const linkModels = path.join(USERDATA, 'models');
  try {
    if (!fs.existsSync(linkModels) && fs.existsSync(realModels))
      fs.symlinkSync(realModels, linkModels, 'dir');
  } catch (e) {
    console.warn('[eval] could not link models dir:', (e as Error).message);
  }
}

// ── shim electron + ./db so the real analyzer loads outside the app ──────────
const persisted: Array<{ id: string; status?: string }> = [];
function installShims(): void {
  const Module = require('module') as typeof import('module') & ModuleWithLoad;
  const orig = Module._load;
  const fakeElectron = {
    app: {
      getPath: (k: string) => (k === 'userData' ? USERDATA : os.tmpdir()),
      getName: () => 'ShelfyExtractEval',
      getVersion: () => '0.0.0',
      isPackaged: false,
    },
  };
  const fakeDb = {
    updateAiAnalysis: (id: string, patch: AiAnalysisPatch) =>
      persisted.push({ id, status: patch.status }),
    getFrequentTags: () => CASES.frequentTags || [],
  };
  Module._load = function (this: unknown, request: string, ...rest: unknown[]) {
    if (request === 'electron') return fakeElectron;
    if (request === './db' || request === './db.js') return fakeDb;
    return orig.call(this, request, ...rest);
  };
}

setupScratch();
installShims();
process.env.SHELFY_EXTERNAL_LLAMA_PORT = String(PORT);

// Tuning agents pass their own edited copy of analyzer.js via EVAL_ANALYZER_PATH
// so 5 variants can be evaluated concurrently against the shared server without
// touching the repo's electron/analyzer.js.
const ANALYZER_PATH =
  process.env.EVAL_ANALYZER_PATH || path.join(HERE, '..', '..', 'electron', 'analyzer');
const analyzer = require(ANALYZER_PATH) as Analyzer;

// Report output paths (override so concurrent agents don't clobber each other).
const REPORT_PATH = process.env.EVAL_REPORT || path.join(HERE, 'last-report.json');
const RAW_PATH = process.env.EVAL_RAW || path.join(HERE, 'last-raw.json');

// ── select posts ─────────────────────────────────────────────────────────────
let posts = CASES.posts;
const idsArg = pickArg('ids');
if (idsArg) {
  const set = new Set(idsArg.split(','));
  posts = posts.filter((p) => set.has(String(p.id)));
}
const limit = pickArg('limit');
if (limit) posts = posts.slice(0, Number(limit));

// ── run extraction, collect terminal results ─────────────────────────────────
let results = new Map<string, AnalyzeJob>(); // postId -> job (rimpiazzata a ogni run con --runs)
const t0 = Date.now();

function awaitAll(): Promise<void> {
  return new Promise((resolve) => {
    results = new Map(); // P11: ripulisci i risultati del run precedente
    const pending = new Set(posts.map((p) => `${p.id}:analyze`));
    const timer = setTimeout(
      () => {
        console.error('\n[eval] GLOBAL TIMEOUT');
        resolve();
      },
      PER_POST_TIMEOUT * posts.length + 60000,
    );
    analyzer.setProgressEmitter((job) => {
      if (!['done', 'error', 'cancelled'].includes(job.status)) return;
      if (!pending.has(job.key)) return;
      pending.delete(job.key);
      results.set(job.postId, job);
      const tags = (job.tags || []).join(', ');
      process.stdout.write(
        `  ${job.status === 'done' ? '✓' : '✗'} ${job.postId.slice(0, 22).padEnd(22)} ${job.status === 'done' ? tags.slice(0, 90) : job.error || ''}\n`,
      );
      if (pending.size === 0) {
        clearTimeout(timer);
        resolve();
      }
    });
    analyzer.enqueueMany(posts);
  });
}

function fmt(n: number): string {
  return n.toFixed(3);
}

interface ScoreRow {
  id: string;
  mediaType: string;
  ok: boolean;
  s: CaseScore;
}

// Punteggia i `results` correnti vs GT → array di righe { id, mediaType, ok, s }.
function scoreCurrent(): ScoreRow[] {
  const rows: ScoreRow[] = [];
  for (const p of posts) {
    const id = String(p.id);
    const gt = GT![id];
    const job = results.get(id);
    if (!gt) {
      console.warn(`[eval] no GT for ${id}, skipping`);
      continue;
    }
    if (!job || job.status !== 'done') {
      rows.push({
        id,
        mediaType: p.mediaType,
        ok: false,
        s: {
          recall: 0,
          coverage: 0,
          forbiddenRate: 1,
          kwScore: 0,
          subjectHit: 0,
          composite: 0,
          matchedMust: [],
          missedMust: gt.mustHave || [],
          forbiddenHits: [],
          tags: [],
        },
      });
      continue;
    }
    rows.push({ id, mediaType: p.mediaType, ok: true, s: scoreCase(gt, job) });
  }
  return rows;
}

(async () => {
  console.log(
    `[eval] model=${MODEL_ID} server=:${PORT} posts=${posts.length}${RUNS > 1 ? ` runs=${RUNS}` : ''}\n`,
  );
  await awaitAll();
  const wall = ((Date.now() - t0) / 1000).toFixed(0);

  if (!GT) {
    // No oracle yet: dump raw outputs for baseline inspection / GT authoring.
    const raw = posts.map((p) => {
      const j = results.get(String(p.id)) || ({} as AnalyzeJob);
      return {
        id: String(p.id),
        mediaType: p.mediaType,
        caption: (p.text || '').slice(0, 120),
        tags: j.tags || [],
        keywords: j.keywords || [],
        entities: j.entities || [],
        description: j.description || '',
        error: j.error || null,
      };
    });
    fs.writeFileSync(RAW_PATH, JSON.stringify(raw, null, 2));
    console.log(`\n[eval] no ground-truth.json — wrote raw outputs to ${RAW_PATH} (${wall}s).`);
    console.log('[eval] author ground-truth.json, then re-run to score.');
    analyzer.shutdown();
    process.exit(0);
  }

  // score (primo run già eseguito sopra)
  const rows = scoreCurrent();

  // ── P11: run aggiuntivi → dispersione del composito per post ────────────────
  // runComposites: postId → [composite del run 1, run 2, …]. Il run 1 è `rows`.
  const runComposites = new Map<string, number[]>(rows.map((r) => [r.id, [r.s.composite]]));
  for (let run = 2; run <= RUNS; run++) {
    console.log(`\n[eval] run ${run}/${RUNS} …`);
    await awaitAll();
    for (const r of scoreCurrent()) {
      if (!runComposites.has(r.id)) runComposites.set(r.id, []);
      runComposites.get(r.id)!.push(r.s.composite);
    }
  }
  // Riepilogo per-post della stabilità (mediana/varianza/IQR del composito).
  const stability =
    RUNS > 1
      ? rows.map((r) => ({
          id: r.id,
          mediaType: r.mediaType,
          composite: summarize(runComposites.get(r.id)),
        }))
      : null;

  const THRESH = 0.55;
  console.log('\n─── PER-POST ───');
  for (const r of rows) {
    const s = r.s;
    const pass = s.composite >= THRESH ? 'PASS' : 'FAIL';
    console.log(
      `${pass} ${fmt(s.composite)} ${r.mediaType.padEnd(8)} ${r.id.slice(0, 22).padEnd(22)} R=${fmt(s.recall)} cov=${fmt(s.coverage)} fbd=${fmt(s.forbiddenRate)} kw=${fmt(s.kwScore)} subj=${s.subjectHit}`,
    );
    if (s.missedMust && s.missedMust.length)
      console.log(`        missed: ${s.missedMust.join(', ')}`);
    if (s.forbiddenHits && s.forbiddenHits.length)
      console.log(`        FORBIDDEN: ${s.forbiddenHits.join(', ')}`);
    console.log(`        tags: ${(s.tags || []).join(', ')}`);
  }

  const mean = (f: (s: CaseScore) => number): number =>
    rows.reduce((a, r) => a + f(r.s), 0) / (rows.length || 1);
  const passed = rows.filter((r) => r.s.composite >= THRESH).length;
  const report = {
    model: MODEL_ID,
    posts: rows.length,
    passed,
    wallSec: +wall,
    runs: RUNS,
    stability, // P11: null se single-shot
    meanComposite: +mean((s) => s.composite).toFixed(3),
    meanRecall: +mean((s) => s.recall).toFixed(3),
    meanCoverage: +mean((s) => s.coverage).toFixed(3),
    meanForbiddenRate: +mean((s) => s.forbiddenRate).toFixed(3),
    meanKwScore: +mean((s) => s.kwScore).toFixed(3),
    meanSubjectHit: +mean((s) => s.subjectHit).toFixed(3),
    rows,
  };
  fs.writeFileSync(REPORT_PATH, JSON.stringify(report, null, 2));

  console.log('\n─── SUMMARY ───');
  console.log(
    `PASS ${passed}/${rows.length}  meanComposite=${report.meanComposite}  recall=${report.meanRecall}  coverage=${report.meanCoverage}  forbidden=${report.meanForbiddenRate}  kw=${report.meanKwScore}  subj=${report.meanSubjectHit}  (${wall}s)`,
  );
  if (stability) {
    console.log(`\n─── P11 STABILITÀ (${RUNS} run) — composito mediana[IQR] per post ───`);
    for (const st of stability) {
      const c = st.composite;
      console.log(
        `  ${st.id.slice(0, 22).padEnd(22)} ${st.mediaType.padEnd(8)} median=${fmt(c.median || 0)} iqr=${fmt(c.iqr || 0)} var=${fmt(c.variance || 0)} [${fmt(c.min || 0)}–${fmt(c.max || 0)}]`,
      );
    }
    console.log(
      '  (per misure riproducibili imposta temperature 0 in electron/analyzer.js — vedi README)',
    );
  }
  analyzer.shutdown();
  process.exit(0);
})();
