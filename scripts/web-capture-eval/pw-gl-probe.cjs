'use strict';

// Compare GL backends for headless WebGL rendering speed: how long until lusion's
// #preloader disappears (intro done) under each. SwiftShader is software (slow,
// universal); ANGLE default tries the real GPU (Metal on mac) — much faster if it
// works headless. Decides whether we can drop the forced-swiftshader flags.
//
// Run: electron scripts/web-capture-eval/pw-gl-probe.cjs

const { app } = require('electron');
const { chromium } = require('playwright-core');

const TARGET = 'https://lusion.co/';
const SLEEP = (ms) => new Promise((r) => setTimeout(r, ms));

const LOADER_GONE = `(() => {
  const vw=innerWidth, vh=innerHeight;
  for (const el of document.querySelectorAll('[id*=preload i],[class*=preload i],[id*=loader i],[class*=loader i]')) {
    const s=getComputedStyle(el); if (s.display==='none'||s.visibility==='hidden'||parseFloat(s.opacity||'1')===0) continue;
    if (s.position!=='fixed'&&s.position!=='absolute') continue;
    const r=el.getBoundingClientRect(); if (r.width>=vw*0.8&&r.height>=vh*0.8) return false;
  }
  return true;
})()`;

const WEBGL_OK = `(() => { try { const c=document.createElement('canvas'); const gl=c.getContext('webgl'); if(!gl) return 'no-webgl'; return gl.getParameter(gl.VERSION)+' / '+(gl.getExtension('WEBGL_debug_renderer_info')?gl.getParameter(gl.getExtension('WEBGL_debug_renderer_info').UNMASKED_RENDERER_WEBGL):'?'); } catch(e){return 'err:'+e.message;} })()`;

async function tryBackend(label, args) {
  const browser = await chromium.launch({ headless: true, args });
  const ctx = await browser.newContext({
    viewport: { width: 1280, height: 900 },
    deviceScaleFactor: 1,
    reducedMotion: 'reduce',
  });
  const pg = await ctx.newPage();
  const renderer = await pg.evaluate(WEBGL_OK).catch(() => 'eval-err');
  await pg.goto(TARGET, { waitUntil: 'load', timeout: 45000 });
  const t0 = Date.now();
  let goneAt = -1;
  for (let i = 0; i < 40; i++) {
    const gone = await pg.evaluate(LOADER_GONE).catch(() => false);
    if (gone) {
      goneAt = Date.now() - t0;
      break;
    }
    await SLEEP(500);
  }
  console.log(
    `[${label}] renderer=${renderer} | preloader gone at ${goneAt < 0 ? '>20s' : (goneAt / 1000).toFixed(1) + 's'}`,
  );
  await browser.close();
}

app.whenReady().then(async () => {
  try {
    await tryBackend('swiftshader', [
      '--ignore-gpu-blocklist',
      '--enable-unsafe-swiftshader',
      '--use-gl=angle',
      '--use-angle=swiftshader',
      '--no-sandbox',
    ]);
    await tryBackend('angle-default', ['--ignore-gpu-blocklist', '--enable-webgl', '--no-sandbox']);
    await tryBackend('angle-metal', [
      '--ignore-gpu-blocklist',
      '--use-gl=angle',
      '--use-angle=metal',
      '--no-sandbox',
    ]);
  } catch (e) {
    console.error('FAILED', e);
  } finally {
    app.exit(0);
  }
});
