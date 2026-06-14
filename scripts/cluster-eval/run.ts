'use strict';

//
// Tag-CLUSTERING evaluation harness (P10).
//
// PURPOSE — measure the QUALITY of the tag clustering produced by the real
// production code (electron/db.js `getTagCandidateGroups`, co-occurrence Jaccard
// + optional embeddings) against a hand-annotated GOLD partition, with ARI and
// purity. Without this, clustering changes ship blind (no regression signal).
//
// RUNTIME — must run under Electron's node (better-sqlite3 ABI). Wired by npm:
//     npm run eval:cluster                 # score against ./gold.json
//     npm run eval:cluster -- --runs=5     # K runs → median + dispersion
//     npm run eval:cluster -- --gold=path  # custom gold file
//
// ISOLATION — copies shelfy.sqlite (+ -wal/-shm) into a scratch userData dir and
// points the production code there via an `electron` shim, so it NEVER mutates
// the real archive. Embeddings (if a model is installed) are used by the code
// under test exactly as in production; if absent it falls back to Jaccard-only.
//
// GOLD — a partition of tag_norm into named clusters. Ships as gold.example.json
// (placeholder schema). Copy it to gold.json and annotate against YOUR archive.
// See README.md.

import fs from 'fs';
import os from 'os';
import path from 'path';
import { fileURLToPath } from 'url';
import { createRequire } from 'module';
import { scoreClustering, type Label } from './score';
import { summarizeRuns } from '../lib/agg-stats'; // P11

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

// Internal Module._load hook surface (not part of the public `module` typings).
type ModuleLoad = (this: unknown, request: string, ...rest: unknown[]) => unknown;
interface ModuleWithLoad {
  _load: ModuleLoad;
}

// ── 0. Runtime guard ────────────────────────────────────────────────────────
if (!process.versions.electron) {
  console.error('\n[cluster-eval] This harness must run under Electron node (better-sqlite3 ABI).');
  console.error('[cluster-eval] Use:  npm run eval:cluster   (not plain `node`).\n');
  process.exit(2);
}

// ── 1. Args ───────────────────────────────────────────────────────────────────
const args = process.argv.slice(2);
const RUNS = Math.max(
  1,
  Number((args.find((a) => a.startsWith('--runs=')) || '').split('=')[1]) || 1,
);
const goldArg = (args.find((a) => a.startsWith('--gold=')) || '').split('=')[1] || null;

// ── 2. Scratch userData (read-only snapshot of the real archive) ───────────────
const REAL_USERDATA = path.join(os.homedir(), 'Library', 'Application Support', 'Shelfy');
const SCRATCH = path.join(__dirname, '.scratch');
const SCRATCH_DB = path.join(SCRATCH, 'shelfy.sqlite');

function setupScratch(): void {
  fs.mkdirSync(SCRATCH, { recursive: true });
  for (const suffix of ['', '-wal', '-shm']) {
    const src = path.join(REAL_USERDATA, `shelfy.sqlite${suffix}`);
    const dst = `${SCRATCH_DB}${suffix}`;
    if (fs.existsSync(src)) fs.copyFileSync(src, dst);
    else if (fs.existsSync(dst)) fs.rmSync(dst);
  }
  // Embedding model weights: symlink the real models dir so embeddings.js can
  // spawn its server if a model is installed (otherwise the code falls back to
  // Jaccard-only — both paths are valid to evaluate).
  const realModels = path.join(REAL_USERDATA, 'models');
  const linkModels = path.join(SCRATCH, 'models');
  try {
    if (!fs.existsSync(linkModels) && fs.existsSync(realModels))
      fs.symlinkSync(realModels, linkModels, 'dir');
  } catch {
    /* embeddings optional */
  }
}

// ── 3. electron shim — point production modules at the scratch dir ──────────────
function installElectronShim(): void {
  const Module = require('module') as typeof import('module') & ModuleWithLoad;
  const orig = Module._load;
  const fake = {
    app: {
      getPath: (k: string) => (k === 'userData' ? SCRATCH : os.tmpdir()),
      getName: () => 'ShelfyEval',
      getVersion: () => '0.0.0',
      isPackaged: false,
    },
  };
  Module._load = function (this: unknown, request: string, ...rest: unknown[]) {
    if (request === 'electron') return fake;
    return orig.call(this, request, ...rest);
  };
}

// ── 4. Gold loading ───────────────────────────────────────────────────────────
// Gold shape: { "clusters": [ { "label": "...", "tags": ["norm", ...] }, ... ] }.
// Returns { goldLabels: Map(tagNorm → label), nClusters, nTags }.
interface GoldCluster {
  label?: unknown;
  tags?: unknown[];
}
interface GoldFile {
  clusters?: GoldCluster[];
}
interface LoadedGold {
  goldLabels: Map<string, Label>;
  nClusters: number;
  nTags: number;
  placeholder: boolean;
  file: string;
}

function loadGold(): LoadedGold {
  const defaultPath = path.join(__dirname, 'gold.json');
  const examplePath = path.join(__dirname, 'gold.example.json');
  let file = goldArg ? path.resolve(goldArg) : defaultPath;
  let placeholder = false;
  if (!fs.existsSync(file)) {
    if (goldArg) {
      console.error(`[cluster-eval] gold file not found: ${file}`);
      process.exit(2);
    }
    file = examplePath;
    placeholder = true;
    console.warn(
      '\n[cluster-eval] ⚠  gold.json non trovato — uso gold.example.json (PLACEHOLDER).',
    );
    console.warn(
      '[cluster-eval]    I punteggi NON sono significativi finché non crei gold.json reale. Vedi README.md.\n',
    );
  }
  const parsed = JSON.parse(fs.readFileSync(file, 'utf8')) as GoldFile;
  const goldLabels = new Map<string, Label>();
  for (const c of parsed.clusters || []) {
    const label = String(c.label ?? '').trim();
    for (const t of c.tags || []) {
      const norm = String(t ?? '')
        .trim()
        .toLowerCase();
      if (norm && label) goldLabels.set(norm, label);
    }
  }
  return {
    goldLabels,
    nClusters: (parsed.clusters || []).length,
    nTags: goldLabels.size,
    placeholder,
    file,
  };
}

// ── 5. Predicted partition from the production clustering ───────────────────────
// getTagCandidateGroups → [{ tags:[norm], neighbors }]. Each group is one cluster;
// a tag not in any group is left UNLABELED (excluded from ARI's intersection) and
// counted toward coverage instead — never silently treated as a hit.
interface PredictedLabels {
  predLabels: Map<string, Label>;
  nGroups: number;
}

async function predictedLabels(db: typeof import('../../electron/db')): Promise<PredictedLabels> {
  const groups = await db.getTagCandidateGroups();
  const predLabels = new Map<string, Label>();
  groups.forEach((g, i) => {
    for (const t of g.tags || []) predLabels.set(String(t).toLowerCase(), `c${i}`);
  });
  return { predLabels, nGroups: groups.length };
}

// ── 6. Main ─────────────────────────────────────────────────────────────────
async function main(): Promise<void> {
  setupScratch();
  installElectronShim();

  const db = require('../../electron/db') as typeof import('../../electron/db');
  db.initialize();

  const { goldLabels, nClusters, nTags, placeholder, file } = loadGold();
  if (!nTags) {
    console.error('[cluster-eval] gold vuoto: nessun tag annotato. Vedi README.md.');
    process.exit(2);
  }

  const perRun: { ari: number | null; purity: number | null }[] = [];
  let lastCoverage: number | null = null;
  let lastNGroups = 0;
  for (let run = 0; run < RUNS; run++) {
    const { predLabels, nGroups } = await predictedLabels(db);
    const s = scoreClustering(predLabels, goldLabels);
    perRun.push({ ari: s.ari, purity: s.purity });
    lastCoverage = s.evaluated / nTags; // quota di tag gold effettivamente clusterizzati
    lastNGroups = nGroups;
  }

  const summary = summarizeRuns(perRun); // { ari: {median,...}, purity: {...} }

  const fmt = (x: number | null | undefined): string => (x == null ? '  —  ' : x.toFixed(3));
  console.log('\n── Cluster eval (P10) ───────────────────────────────────────');
  console.log(
    `gold: ${path.basename(file)}${placeholder ? ' (PLACEHOLDER)' : ''} — ${nClusters} cluster, ${nTags} tag`,
  );
  console.log(
    `produced: ${lastNGroups} gruppi | copertura gold: ${((lastCoverage ?? 0) * 100).toFixed(0)}% (${Math.round((lastCoverage ?? 0) * nTags)}/${nTags} tag valutati)`,
  );
  console.log(`runs: ${RUNS}`);
  console.log('metric    median    mean     iqr      min      max');
  for (const k of ['ari', 'purity'] as const) {
    const st = summary[k] || {};
    console.log(
      `${k.padEnd(9)} ${fmt(st.median)}   ${fmt(st.mean)}   ${fmt(st.iqr)}   ${fmt(st.min)}   ${fmt(st.max)}`,
    );
  }
  console.log('─────────────────────────────────────────────────────────────');
  console.log('Nota: purity alta da sola NON basta (premia la frammentazione); leggila con ARI.\n');

  const report = {
    gold: path.basename(file),
    placeholder,
    goldClusters: nClusters,
    goldTags: nTags,
    producedGroups: lastNGroups,
    coverage: lastCoverage,
    runs: RUNS,
    metrics: summary,
  };
  fs.writeFileSync(path.join(__dirname, 'last-report.json'), JSON.stringify(report, null, 2));

  try {
    db.close();
  } catch {
    /* best effort */
  }
  // Embeddings server (if started) keeps the event loop alive — force exit.
  try {
    (
      require('../../electron/embeddings') as typeof import('../../electron/embeddings')
    ).forceShutdown();
  } catch {
    /* optional */
  }
  process.exit(0);
}

main().catch((err: unknown) => {
  console.error(
    '[cluster-eval] errore:',
    err && err instanceof Error && err.stack ? err.stack : err,
  );
  process.exit(1);
});
