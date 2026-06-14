// Batch test del modulo reale electron/analyzer.js su 10 job misti
// (video + immagini + testo). Mocka `electron` e `./db`, costruisce i post da
// fixture estratte dal DB reale (vedi i file /tmp/sb_*.json generati via sqlite3),
// li accoda TUTTI insieme e ne osserva l'esecuzione end-to-end.
//
// Serve anche da test di robustezza per il bug KV-cache #17200 di llama.cpp:
// 10 richieste consecutive sullo stesso server warm devono completare tutte.
//
// Uso: node scripts/analyzer-batch-test.cjs

const Module = require('module');
const path = require('path');
const os = require('os');
const fs = require('fs');

const USERDATA = path.join(os.homedir(), 'Library/Application Support/Shelfy');
const persisted = [];

const origLoad = Module._load;
Module._load = function (request) {
  if (request === 'electron') {
    return { app: { getPath: (k) => (k === 'userData' ? USERDATA : os.tmpdir()) } };
  }
  if (request === './db') {
    return {
      updateAiAnalysis: (id, patch) => persisted.push({ id, status: patch.status }),
      getFrequentTags: () => [],
    };
  }
  return origLoad.apply(this, arguments);
};

const analyzer = require(path.join(__dirname, '..', 'electron', 'analyzer.js'));

function load(file) {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')); } catch { return []; }
}

// ── Build 10 mixed posts from real data ──────────────────────────────────────
const videoRows = load('/tmp/sb_video.json').filter((r) => r.videoPath && fs.existsSync(r.videoPath)).slice(0, 4);
const imageRows = load('/tmp/sb_image.json').filter((r) => r.thumbnailPath && fs.existsSync(r.thumbnailPath)).slice(0, 3);
const textRows = load('/tmp/sb_text.json').filter((r) => typeof r.text === 'string' && r.text.trim().length > 0).slice(0, 3);

const posts = [
  ...videoRows.map((r) => ({
    id: String(r.id), mediaType: 'video', videoPath: r.videoPath,
    thumbnailPath: r.thumbnailPath ?? null, text: r.text ?? null, _kind: 'video',
  })),
  ...imageRows.map((r) => ({
    id: String(r.id), mediaType: 'image', imagePath: null,
    thumbnailPath: r.thumbnailPath, media: [], text: r.text ?? null, _kind: 'image',
  })),
  ...textRows.map((r) => ({
    id: String(r.id), mediaType: 'text', text: r.text ?? null, _kind: 'text',
  })),
];

const kindByKey = new Map();
for (const p of posts) kindByKey.set(`${p.id}:analyze`, p._kind);

console.log(`model status:`, analyzer.getModelStatus().ready ? 'ready' : 'NOT READY');
console.log(`enqueuing ${posts.length} jobs: ${videoRows.length} video, ${imageRows.length} image, ${textRows.length} text\n`);

const t0 = Date.now();
const startedAt = new Map();
const terminal = new Map(); // key → { status, secs, category, contentType, tags, error }
const TOTAL = posts.length;
// Numero di job realmente accodati: i post non analizzabili (canAnalyze=false)
// non emettono mai progresso, quindi la fine va valutata su questo valore e non
// su TOTAL, altrimenti il run resta appeso fino al timeout globale.
let target = TOTAL;

analyzer.setProgressEmitter((job) => {
  if (!startedAt.has(job.key) && job.status !== 'pending') startedAt.set(job.key, Date.now());
  if (['done', 'error', 'cancelled'].includes(job.status) && !terminal.has(job.key)) {
    const secs = ((Date.now() - (startedAt.get(job.key) || t0)) / 1000).toFixed(1);
    terminal.set(job.key, {
      status: job.status, secs, kind: kindByKey.get(job.key),
      category: job.category, contentType: job.contentType,
      tags: job.tags, description: job.description, error: job.error,
    });
    const r = terminal.get(job.key);
    const head = `[${r.kind.padEnd(5)}] ${job.key.split(':')[0]}`;
    if (r.status === 'done') {
      console.log(`✓ ${head} (${r.secs}s) — ${r.category} / ${r.contentType} — tags: ${(r.tags || []).join(', ')}`);
    } else {
      console.log(`✗ ${head} (${r.secs}s) — ${r.status}${r.error ? ': ' + r.error : ''}`);
    }
    if (terminal.size === target) finish();
  }
});

function finish() {
  const wall = ((Date.now() - t0) / 1000).toFixed(1);
  const done = [...terminal.values()].filter((r) => r.status === 'done');
  const failed = [...terminal.values()].filter((r) => r.status !== 'done');
  console.log(`\n─── SUMMARY ───`);
  // Denominator is `target` (the jobs actually enqueued), not TOTAL: posts that
  // weren't enqueueable (canAnalyze=false / duplicate key) never produce a terminal
  // event, so comparing against TOTAL would report a false failure.
  console.log(`completati: ${done.length}/${target}  falliti: ${failed.length}  tempo totale: ${wall}s`);
  const byKind = {};
  for (const r of terminal.values()) {
    byKind[r.kind] = byKind[r.kind] || { ok: 0, ko: 0 };
    if (r.status === 'done') byKind[r.kind].ok++; else byKind[r.kind].ko++;
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

const { queued } = analyzer.enqueueMany(posts);
target = queued;
if (queued < TOTAL) {
  console.log(`scartati ${TOTAL - queued} post non accodati (non analizzabili o duplicati): attesi ${queued} job terminali\n`);
}
// Se nessun post è risultato accodabile, non arriverà alcun evento di
// progresso: chiudiamo subito invece di restare appesi fino al timeout.
if (queued === 0) finish();

// Safety timeout: 10 jobs × up to a couple minutes each.
setTimeout(() => { console.error('\nTIMEOUT globale'); analyzer.shutdown(); process.exit(2); }, 20 * 60_000);
