'use strict';

// Harness di verifica della cattura screenshot (tab "AI Website" / web-ref).
// Gira DENTRO Electron reale (serve BrowserWindow offscreen) come smoke-web.cjs:
//   npm run eval:capture
//   CAP_ONLY=scrollsmoother npm run eval:capture   (filtra un singolo caso)
//
// Serve fixture HTML LOCALI e deterministiche (scripts/web-capture-eval/fixtures)
// che riproducono i failure mode reali dei siti-reference:
//   • virtual scroll (GSAP ScrollSmoother / Locomotive) → troncamento
//   • hero WebGL/Three.js → frame nero / canvas appiattito
//   • pinned section, lazy-load, e i controlli native/Lenis (non devono regredire)
//
// Le fixture sono raggiunte via host-resolver-rules (capture.test → 127.0.0.1) così
// l'SSRF-guard (net-safety) le accetta SENZA modifiche al codice di sicurezza.
//
// Metriche oggettive per caso:
//   coverage = altezza catturata / altezza reale (ground-truth in cases.json)
//   luma/variance = luminanza media e deviazione std (via ffmpeg) → black/blank
//   heroLuma = luminanza della fascia hero (top 900px) → canvas WebGL nero
// Exit code 0 se tutti PASS, 1 se almeno un FAIL, 2 su errore fatale/timeout.

const { app } = require('electron');
const path = require('path');
const fs = require('fs');
const http = require('http');
const { spawn } = require('child_process');

// Deve essere impostato PRIMA di whenReady: mappa un hostname "pubblico" → loopback.
app.commandLine.appendSwitch('host-resolver-rules', 'MAP capture.test 127.0.0.1');
app.setPath('userData', path.join(app.getPath('appData'), 'Shelfy'));
app.on('window-all-closed', () => {});

const FIX_DIR = path.join(__dirname, 'fixtures');
const HOST = 'capture.test';
const PORT = Number(process.env.CAP_PORT || 38427);
const CASES = require('./cases.json');
const ONLY = process.env.CAP_ONLY || '';

const COVERAGE_MIN = 0.85;
const LUMA_MIN = 8;
const LUMA_MAX = 248;
const STD_MIN = 4;
const HERO_MIN = 16;

const log = (...a) => console.log('[cap-eval]', ...a);

function resolveFfmpeg() {
  const cands = [
    process.env.FFMPEG_BIN,
    path.join(app.getPath('userData'), 'runtime-bin', 'bin', 'ffmpeg'),
    '/opt/homebrew/bin/ffmpeg',
    '/usr/local/bin/ffmpeg',
    '/usr/bin/ffmpeg',
  ];
  for (const p of cands) if (p && fs.existsSync(p)) return p;
  try {
    const s = require('ffmpeg-static');
    if (s && fs.existsSync(s)) return s;
  } catch {}
  return 'ffmpeg';
}
const FFMPEG = resolveFfmpeg();

function serve() {
  return new Promise((resolve) => {
    const srv = http.createServer((req, res) => {
      const rel = decodeURIComponent((req.url || '/').split('?')[0]).replace(/^\/+/, '');
      const f = path.join(FIX_DIR, rel);
      if (!f.startsWith(FIX_DIR) || !fs.existsSync(f) || !fs.statSync(f).isFile()) {
        res.statusCode = 404;
        res.end('not found');
        return;
      }
      res.setHeader('content-type', 'text/html; charset=utf-8');
      fs.createReadStream(f).pipe(res);
    });
    srv.listen(PORT, '127.0.0.1', () => resolve(srv));
  });
}

// Downscala a 64×64 gray e calcola media/deviazione standard della luminanza.
// crop = { top: px } (fascia superiore/hero) | { bottom: px } (fascia inferiore/tail).
function imgStats(file, crop) {
  return new Promise((resolve) => {
    let pre = '';
    if (crop && crop.top) pre = `crop=iw:min(${crop.top}\\,ih):0:0,`;
    else if (crop && crop.bottom)
      pre = `crop=iw:min(${crop.bottom}\\,ih):0:max(0\\,ih-${crop.bottom}),`;
    const vf = pre + 'scale=64:64,format=gray';
    const ff = spawn(FFMPEG, [
      '-hide_banner',
      '-loglevel',
      'error',
      '-i',
      path.resolve(file),
      '-vf',
      vf,
      '-frames:v',
      '1',
      '-f',
      'rawvideo',
      '-pix_fmt',
      'gray',
      '-',
    ]);
    const chunks = [];
    ff.stdout.on('data', (d) => chunks.push(d));
    ff.on('error', () => resolve(null));
    ff.on('close', () => {
      const buf = Buffer.concat(chunks);
      if (!buf.length) return resolve(null);
      let sum = 0;
      for (const b of buf) sum += b;
      const mean = sum / buf.length;
      let v = 0;
      for (const b of buf) v += (b - mean) * (b - mean);
      resolve({ mean, std: Math.sqrt(v / buf.length), n: buf.length });
    });
  });
}

async function main() {
  const webcapture = require('../../electron/webcapture.js');
  const srv = await serve();
  log('ffmpeg:', FFMPEG);
  log('fixtures:', `http://${HOST}:${PORT}/`);

  const cases = CASES.filter((c) => !ONLY || c.name.includes(ONLY) || c.file.includes(ONLY));
  if (cases.length === 0) {
    srv.close();
    log(ONLY ? `nessun caso corrisponde a CAP_ONLY="${ONLY}"` : 'nessun caso in cases.json');
    return 2; // niente da testare → errore, NON falso PASS
  }
  const rows = [];

  for (const c of cases) {
    const url = `http://${HOST}:${PORT}/${c.file}`;
    const steps = [];
    let cap = null;
    let err = null;
    try {
      cap = await webcapture.capturePage(url, {
        settleBeforeShotMs: c.settleMs != null ? c.settleMs : 1500,
        format: 'webp',
        quality: 92,
        onStep: (label) => steps.push(label),
      });
    } catch (e) {
      err = e;
    }

    let coverage = 0;
    let full = null;
    let hero = null;
    let tail = null;
    const want = c.checks || ['coverage', 'luma', 'variance'];
    if (cap) {
      coverage = (cap.height * 1280) / (cap.width * c.realHeight);
      full = await imgStats(cap.screenshotPath);
      if (want.includes('heroLuma')) hero = await imgStats(cap.screenshotPath, { top: 900 });
      if (want.includes('tail')) tail = await imgStats(cap.screenshotPath, { bottom: 600 });
    }

    const checks = {};
    if (want.includes('coverage')) checks.coverage = coverage >= COVERAGE_MIN;
    if (want.includes('luma'))
      checks.luma = !!full && full.mean >= LUMA_MIN && full.mean <= LUMA_MAX;
    if (want.includes('variance')) checks.variance = !!full && full.std >= STD_MIN;
    if (want.includes('heroLuma')) checks.heroLuma = !!hero && hero.mean >= HERO_MIN;
    // tail = la fascia inferiore deve contenere contenuto reale (non bianco/uniforme):
    // smaschera una cattura "alta ma vuota" (altezza giusta ma contenuto clippato).
    if (want.includes('tail'))
      checks.tail = !!tail && tail.std >= STD_MIN && tail.mean >= LUMA_MIN && tail.mean <= LUMA_MAX;

    const pass = !err && Object.values(checks).every(Boolean);
    const vs = steps.find((s) => s.startsWith('virtual-scroll')) || '';
    const cv = steps.find((s) => s.startsWith('canvas')) || '';

    rows.push({
      name: c.name,
      pass,
      coverage,
      img: cap ? `${cap.width}x${cap.height}` : null,
      capped: cap ? cap.capped : null,
      luma: full ? Number(full.mean.toFixed(1)) : null,
      std: full ? Number(full.std.toFixed(1)) : null,
      heroLuma: hero ? Number(hero.mean.toFixed(1)) : null,
      tailStd: tail ? Number(tail.std.toFixed(1)) : null,
      checks,
      detect: vs,
      canvas: cv,
      err: err ? err.message : null,
    });

    if (cap) await cap.dispose();

    log(`${pass ? 'PASS' : 'FAIL'}  ${c.name}`);
    log(
      `      cover=${coverage.toFixed(2)} img=${cap ? cap.width + 'x' + cap.height : '-'}` +
        ` luma=${full ? full.mean.toFixed(0) : '-'} std=${full ? full.std.toFixed(1) : '-'}` +
        `${hero ? ` hero=${hero.mean.toFixed(0)}` : ''}${tail ? ` tailStd=${tail.std.toFixed(1)}` : ''}` +
        `  ${vs} ${cv}${err ? '  ERR:' + err.message : ''}`,
    );
    log(
      `      checks: ${Object.entries(checks)
        .map(([k, v]) => k + '=' + (v ? '✓' : '✗'))
        .join('  ')}`,
    );
  }

  srv.close();
  const passed = rows.filter((r) => r.pass).length;
  log('========================================');
  log(`RISULTATO: ${passed}/${rows.length} PASS`);
  try {
    fs.writeFileSync(path.join(__dirname, 'last-run.json'), JSON.stringify(rows, null, 2));
  } catch {}
  // rows vuoto NON deve dare 0: un insieme vuoto è errore, non PASS.
  if (rows.length === 0) return 2;
  return passed === rows.length ? 0 : 1;
}

const hardStop = setTimeout(() => {
  console.error('[cap-eval] HARD TIMEOUT');
  try {
    app.exit(2);
  } catch {}
}, 300000);

app
  .whenReady()
  .then(main)
  .then((code) => {
    clearTimeout(hardStop);
    try {
      app.exit(code);
    } catch {}
  })
  .catch((e) => {
    console.error('[cap-eval] FATAL', (e && e.stack) || e);
    clearTimeout(hardStop);
    try {
      app.exit(2);
    } catch {}
  });
