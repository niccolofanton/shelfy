'use strict';

// Validate the FILMSTRIP capture for scroll-jacked WebGL experiences end-to-end:
// detect the locked-document case, drive the wheel, grab a viewport frame per step,
// stop when frames stop changing (ffmpeg 16×16 signature diff), then vstack the kept
// frames into one tall webp to eyeball quality + end-detection before porting to prod.
//
// Run: electron scripts/web-capture-eval/pw-journey.cjs [url]

const { app } = require('electron');
const { chromium } = require('playwright-core');
const path = require('path');
const os = require('os');
const fs = require('fs');
const { execFile } = require('child_process');

const TARGET = process.argv.find((a) => /^https?:\/\//.test(a)) || 'https://lusion.co/';
const FF = require('../../node_modules/ffmpeg-static');
const SLEEP = (ms) => new Promise((r) => setTimeout(r, ms));

function ff(args) {
  return new Promise((resolve, reject) => {
    execFile(FF, args, { maxBuffer: 1 << 26, encoding: 'buffer' }, (err, stdout, stderr) =>
      err ? reject(err) : resolve({ stdout, stderr }),
    );
  });
}

// 16×16 grayscale raw signature for a fast perceptual diff between frames.
async function signature(png) {
  const { stdout } = await ff([
    '-hide_banner',
    '-loglevel',
    'error',
    '-i',
    png,
    '-vf',
    'scale=16:16,format=gray',
    '-f',
    'rawvideo',
    '-',
  ]);
  return stdout; // 256 bytes
}
function meanDiff(a, b) {
  if (!a || !b || a.length !== b.length) return 255;
  let s = 0;
  for (let i = 0; i < a.length; i++) s += Math.abs(a[i] - b[i]);
  return s / a.length;
}

const JS_JACKED = `(() => {
  const d = document.documentElement, b = document.body;
  const ov = (getComputedStyle(d).overflowY + ' ' + (b ? getComputedStyle(b).overflowY : '')).toLowerCase();
  const docScrolls = d.scrollHeight > innerHeight * 1.5;
  const locked = /hidden|clip/.test(ov);
  let bigCanvas = false;
  for (const c of document.querySelectorAll('canvas')) {
    const r = c.getBoundingClientRect(), s = getComputedStyle(c);
    if (r.width >= innerWidth * 0.6 && r.height >= innerHeight * 0.6 && (s.position === 'fixed' || s.position === 'absolute')) bigCanvas = true;
  }
  return { jacked: !docScrolls && locked && bigCanvas, docScrolls, locked, bigCanvas, sh: d.scrollHeight };
})()`;

const MAX_FRAMES = 12;
const SETTLE_MS = 1100;
const SAME_THRESHOLD = 3.0; // mean per-pixel gray diff below this ⇒ "same frame"

async function run() {
  const browser = await chromium.launch({
    headless: true,
    args: [
      '--ignore-gpu-blocklist',
      '--enable-webgl',
      '--use-gl=angle',
      '--use-angle=metal',
      '--no-sandbox',
      '--hide-scrollbars',
      '--mute-audio',
    ],
  });
  const ctx = await browser.newContext({
    viewport: { width: 1280, height: 900 },
    deviceScaleFactor: 1,
  });
  const pg = await ctx.newPage();
  await pg.goto(TARGET, { waitUntil: 'load', timeout: 45000 });
  await SLEEP(9000); // intro

  const probe = await pg.evaluate(JS_JACKED);
  console.log('jacked?', JSON.stringify(probe));
  if (!probe.jacked) {
    console.log('Not scroll-jacked — would take the normal fullPage path.');
    await browser.close();
    return;
  }

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-journey-'));
  const deltaY = 900; // ~one viewport per step
  const frames = [];
  let prevSig = null;
  let sameRun = 0;
  await pg.mouse.move(640, 450);

  for (let i = 0; i < MAX_FRAMES; i++) {
    const png = path.join(dir, `f${String(i).padStart(2, '0')}.png`);
    await pg.screenshot({ path: png, type: 'png', animations: 'disabled' });
    const sig = await signature(png);
    const diff = prevSig ? meanDiff(prevSig, sig) : 999;
    console.log(`frame ${i}: diff=${diff.toFixed(1)}`);

    if (prevSig && diff < SAME_THRESHOLD) {
      sameRun++;
      // First step already static ⇒ not a journey; keep only frame 0.
      if (i === 1) {
        frames.length = 1;
        console.log('  static after 1 step → single frame');
        break;
      }
      if (sameRun >= 1) {
        console.log('  reached the end (frame unchanged) → stop');
        break;
      }
    } else {
      sameRun = 0;
      frames.push(png);
    }
    prevSig = sig;
    await pg.mouse.wheel(0, deltaY);
    await SLEEP(SETTLE_MS);
  }

  console.log(`kept ${frames.length} frames`);
  // vstack to one webp to view
  const out = path.join(dir, 'journey.webp');
  const inputs = [];
  for (const f of frames) inputs.push('-i', f);
  const filter = frames.length > 1 ? `vstack=inputs=${frames.length}` : 'null';
  await ff([
    '-hide_banner',
    '-loglevel',
    'error',
    ...inputs,
    '-filter_complex',
    filter,
    '-frames:v',
    '1',
    '-c:v',
    'libwebp',
    '-quality',
    '80',
    '-y',
    out,
  ]);
  console.log('JOURNEY', out);
  await browser.close();
}

app.whenReady().then(async () => {
  try {
    await run();
  } catch (e) {
    console.error('FAILED', e);
  } finally {
    app.exit(0);
  }
});
