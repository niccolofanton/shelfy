'use strict';

// Probe a site's intro/preloader timeline: poll the DOM every 750ms and log signals
// that could mark "page is ready" (intro finished), so we can build a robust readiness
// wait instead of a blind fixed settle. Run single (no contention) to see the natural
// timeline.
//
// Run: electron scripts/web-capture-eval/pw-ready-probe.cjs [url]

const { app } = require('electron');
const { chromium } = require('playwright-core');

const TARGET = process.argv.find((a) => /^https?:\/\//.test(a)) || 'https://lusion.co/';
const SLEEP = (ms) => new Promise((r) => setTimeout(r, ms));

const PROBE = `(() => {
  const vw = innerWidth, vh = innerHeight, area = vw*vh;
  // Full-viewport overlays (fixed/absolute, opaque, covering >=70% viewport).
  const overlays = [];
  for (const el of document.querySelectorAll('div,section,canvas,main')) {
    const s = getComputedStyle(el);
    if (s.position !== 'fixed' && s.position !== 'absolute') continue;
    const r = el.getBoundingClientRect();
    if (r.width*r.height < area*0.7) continue;
    if (s.opacity === '0' || s.visibility === 'hidden' || s.display === 'none') continue;
    overlays.push({
      tag: el.tagName, id: el.id||'', cls: (el.className&&el.className.toString().slice(0,40))||'',
      z: s.zIndex, op: s.opacity, bg: s.backgroundColor,
      txt: (el.innerText||'').trim().slice(0,20),
    });
  }
  // Elements that look loader-ish by class/id.
  const loaderish = [...document.querySelectorAll('[class*=load i],[id*=load i],[class*=preload i],[class*=splash i],[class*=intro i]')]
    .filter(el => { const r=el.getBoundingClientRect(); const s=getComputedStyle(el); return r.width*r.height>area*0.3 && s.display!=='none' && s.visibility!=='hidden' && s.opacity!=='0'; })
    .map(el => ({ tag: el.tagName, id: el.id||'', cls: (el.className&&el.className.toString().slice(0,40))||'' }));
  const canvases = [...document.querySelectorAll('canvas')].map(c => { const r=c.getBoundingClientRect(); return Math.round(r.width)+'x'+Math.round(r.height); });
  return { sh: document.documentElement.scrollHeight, bodyClass: (document.body.className||'').slice(0,60), overlays, loaderish, canvases, ready: document.readyState };
})()`;

async function run() {
  const browser = await chromium.launch({
    headless: true,
    args: [
      '--ignore-gpu-blocklist',
      '--enable-unsafe-swiftshader',
      '--use-gl=angle',
      '--use-angle=swiftshader',
      '--no-sandbox',
    ],
  });
  const ctx = await browser.newContext({
    viewport: { width: 1280, height: 900 },
    deviceScaleFactor: 1,
    reducedMotion: 'reduce',
  });
  const pg = await ctx.newPage();
  await pg.goto(TARGET, { waitUntil: 'load', timeout: 45000 });
  for (let t = 0; t < 16; t++) {
    const info = await pg.evaluate(PROBE).catch((e) => ({ error: String(e) }));
    console.log(
      `t=${(t * 0.75).toFixed(1)}s sh=${info.sh} body="${info.bodyClass}" overlays=${(info.overlays || []).length} loaderish=${(info.loaderish || []).length} canvases=${JSON.stringify(info.canvases)}`,
    );
    if (info.overlays && info.overlays.length)
      console.log('     overlays:', JSON.stringify(info.overlays));
    if (info.loaderish && info.loaderish.length)
      console.log('     loaderish:', JSON.stringify(info.loaderish));
    await SLEEP(750);
  }
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
