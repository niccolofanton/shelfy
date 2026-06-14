'use strict';

// Before/after eval of the awwwards-hardening fixes (round 6) on LIVE sites.
//
// Captures each target with the Playwright engine and quantifies screenshot
// quality: per-chunk 16×16 gray signature via ffmpeg → a chunk whose pixel
// stddev is near zero is a BLANK band (the GSAP-reverse failure mode).
//
//   npx electron scripts/web-capture-eval/pw-awwwards-eval.cjs --baseline \
//       --out /tmp/awwwards-eval-baseline.json
//   npx electron scripts/web-capture-eval/pw-awwwards-eval.cjs \
//       --out /tmp/awwwards-eval-fixed.json
//
// --baseline neuters JS_FORCE_SCROLL_ANIM_END (the headline fix) by patching
// webcapture._internals BEFORE webcapture-playwright destructures it, so the
// same engine runs with pre-fix scroll-animation behavior. The other fixes
// (ready-before-kill ordering, fullPage+clip, no reducedMotion) stay active in
// both modes — they are not flag-toggleable.

import { app } from 'electron';
import fs from 'fs';
import { execFile } from 'child_process';
import { createRequire } from 'module';

const require = createRequire(import.meta.url);

// Isolate eval artifacts away from the real app's userData.
app.setPath('userData', '/tmp/shelfy-awwwards-eval');

const BASELINE = process.argv.includes('--baseline');
const outIdx = process.argv.indexOf('--out');
const OUT =
  outIdx !== -1
    ? process.argv[outIdx + 1]
    : `/tmp/awwwards-eval-${BASELINE ? 'baseline' : 'fixed'}.json`;

const urlArgs = process.argv.filter((a) => /^https?:\/\//.test(a));
const TARGETS = urlArgs.length
  ? urlArgs
  : [
      'https://lusion.co/', // scroll-jacked WebGL → filmstrip (regression check)
      'https://www.exo-ape.com/', // GSAP/ScrollTrigger reveals
      'https://locomotive.ca/en', // Locomotive scroll
      'https://we-go.it/', // baseline noto (DOM-centric, ~7300px)
    ];

const PER_CAPTURE_TIMEOUT_MS = 180_000;

const webcapture =
  require('../../electron/webcapture') as typeof import('../../electron/webcapture');
if (BASELINE) {
  // Neuter the fix BEFORE the engine destructures _internals at require time.
  webcapture._internals.JS_FORCE_SCROLL_ANIM_END = '({ st: 0, tweens: 0, baseline: true })';
}
const pw =
  require('../../electron/webcapture-playwright') as typeof import('../../electron/webcapture-playwright');
const resolveFfmpeg = webcapture._internals.resolveFfmpeg;

interface BandStats {
  mean: number;
  stddev: number;
}

interface Chunk {
  screenshotPath: string | null;
  width: number;
  height: number;
}

interface PwCtx {
  screenshotPath: string | null;
  chunks: Chunk[];
  width: number;
  height: number;
  capped: boolean;
  webglHeavy: boolean;
  dispose?: () => Promise<void>;
}

// 16×16 gray signature → { mean, stddev }. Near-zero stddev = uniform band.
function bandStats(file: string): Promise<BandStats | null> {
  return new Promise((resolve) => {
    execFile(
      resolveFfmpeg(),
      [
        '-hide_banner',
        '-loglevel',
        'error',
        '-i',
        file,
        '-vf',
        'scale=16:16,format=gray',
        '-f',
        'rawvideo',
        '-',
      ],
      { maxBuffer: 1 << 20, encoding: 'buffer' },
      (err, stdout) => {
        if (err || !stdout || !stdout.length) return resolve(null);
        let sum = 0;
        for (const b of stdout) sum += b;
        const mean = sum / stdout.length;
        let varSum = 0;
        for (const b of stdout) varSum += (b - mean) * (b - mean);
        resolve({ mean: Math.round(mean), stddev: Math.round(Math.sqrt(varSum / stdout.length)) });
      },
    );
  });
}

const BLANK_STDDEV = 8; // below → the band is one flat color (blank/black/white)

interface BandResult {
  mean?: number;
  stddev?: number;
  h?: number;
  blank?: boolean;
  error?: boolean;
}

interface CaptureResult {
  url: string;
  ok: boolean;
  durationS: number;
  width?: number;
  height?: number;
  capped?: boolean;
  webglHeavy?: boolean;
  chunkCount?: number;
  blankBands?: number;
  bands?: BandResult[];
  steps: string[];
  screenshotPath?: string | null;
  error?: string;
}

async function captureOne(url: string): Promise<CaptureResult> {
  const steps: string[] = [];
  const t0 = Date.now();
  const ac = new AbortController();
  const killer = setTimeout(() => ac.abort(), PER_CAPTURE_TIMEOUT_MS);
  let ctx: PwCtx | null = null;
  try {
    ctx = (await pw.capturePage(url, {
      captureStamp: Math.floor(Date.now() / 1000),
      signal: ac.signal,
      onStep: (label: string, delta: number) => {
        steps.push(`${label}+${(delta / 1000).toFixed(1)}s`);
      },
    })) as PwCtx;
    const durationS = (Date.now() - t0) / 1000;
    const chunks = Array.isArray(ctx.chunks) ? ctx.chunks : [];
    const bands: BandResult[] = [];
    for (const c of chunks) {
      const st = c.screenshotPath ? await bandStats(c.screenshotPath) : null;
      bands.push(st ? { ...st, h: c.height, blank: st.stddev < BLANK_STDDEV } : { error: true });
    }
    return {
      url,
      ok: true,
      durationS: Math.round(durationS * 10) / 10,
      width: ctx.width,
      height: ctx.height,
      capped: !!ctx.capped,
      webglHeavy: !!ctx.webglHeavy,
      chunkCount: chunks.length,
      blankBands: bands.filter((b) => b.blank).length,
      bands,
      steps,
      screenshotPath: ctx.screenshotPath,
    };
  } catch (err) {
    return {
      url,
      ok: false,
      durationS: Math.round(((Date.now() - t0) / 1000) * 10) / 10,
      error: String((err as { message?: string })?.message || err).slice(0, 200),
      steps,
    };
  } finally {
    clearTimeout(killer);
    try {
      await ctx?.dispose?.();
    } catch {}
  }
}

app.whenReady().then(async () => {
  console.log(
    `[eval] mode=${BASELINE ? 'BASELINE (gsap-freeze OFF)' : 'FIXED'} targets=${TARGETS.length}`,
  );
  const results: CaptureResult[] = [];
  for (const url of TARGETS) {
    console.log(`[eval] capturing ${url} …`);
    const r = await captureOne(url);
    console.log(
      r.ok
        ? `  → ${r.durationS}s  ${r.width}x${r.height}  chunks=${r.chunkCount}  blank=${r.blankBands}  webgl=${r.webglHeavy}${r.capped ? '  CAPPED' : ''}`
        : `  → FAILED ${r.durationS}s  ${r.error}`,
    );
    if (r.steps.length) console.log(`     steps: ${r.steps.join(' | ')}`);
    results.push(r);
  }
  try {
    await pw.closeBrowser();
  } catch {}
  fs.writeFileSync(
    OUT,
    JSON.stringify({ baseline: BASELINE, when: new Date().toISOString(), results }, null, 2),
  );
  console.log(`[eval] written ${OUT}`);
  app.exit(results.every((r) => r.ok) ? 0 : 1);
});
