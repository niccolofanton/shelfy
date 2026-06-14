'use strict';

// Diagnostic: how tall does a virtual-scroll site (lusion.co) report at each stage,
// and does reducedMotion collapse the experience? Drives the decision for the
// full-page-capture tuning. One browser launch, two contexts (rm on/off).
//
// Run: electron scripts/web-capture-eval/pw-measure.cjs [url]

const { app } = require('electron');
const { chromium } = require('playwright-core');
const webcapture = require('../../electron/webcapture.js');

const { JS_NEUTRALIZE_VIRTUAL_SCROLL, JS_MEASURE, jsAutoScroll, DEFAULT_MAX_HEIGHT } =
  webcapture._internals;

const TARGET = process.argv.find((a) => /^https?:\/\//.test(a)) || 'https://lusion.co/';
const SLEEP = (ms) => new Promise((r) => setTimeout(r, ms));

async function measureWith(browser, reducedMotion) {
  const ctx = await browser.newContext({
    viewport: { width: 1280, height: 900 },
    deviceScaleFactor: 1,
    reducedMotion: reducedMotion ? 'reduce' : 'no-preference',
  });
  const pg = await ctx.newPage();
  await pg.goto(TARGET, { waitUntil: 'load', timeout: 45000 });
  await pg.waitForLoadState('networkidle', { timeout: 15000 }).catch(() => {});
  await SLEEP(2000);

  const raw = await pg.evaluate('document.documentElement.scrollHeight');
  const vscroll = await pg.evaluate(JS_NEUTRALIZE_VIRTUAL_SCROLL).catch(() => ({ libs: [] }));
  await SLEEP(600);
  await pg.evaluate(jsAutoScroll(DEFAULT_MAX_HEIGHT)).catch(() => {});
  await SLEEP(600);
  const afterScrollHeight = await pg.evaluate('document.documentElement.scrollHeight');
  const measure = await pg.evaluate(JS_MEASURE).catch((e) => ({ error: String(e) }));

  console.log(`\n=== reducedMotion=${reducedMotion} ===`);
  console.log('  raw scrollHeight       :', raw);
  console.log('  neutralize libs        :', JSON.stringify(vscroll && vscroll.libs));
  console.log('  scrollHeight after      :', afterScrollHeight);
  console.log('  JS_MEASURE.scrollHeight :', measure && measure.scrollHeight);
  await ctx.close();
}

app.whenReady().then(async () => {
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
  try {
    await measureWith(browser, true);
    await measureWith(browser, false);
  } catch (e) {
    console.error('FAILED', e);
  } finally {
    await browser.close();
    app.exit(0);
  }
});
