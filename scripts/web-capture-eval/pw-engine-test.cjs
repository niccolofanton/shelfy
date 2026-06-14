'use strict';

// Exercise the capture pipeline under the Electron runtime. Two modes:
//   default: drive the engine SELECTOR (capture-engine.js) so the Playwright/OSR
//            choice + fallback is what actually runs (parity with the orchestrator).
// Captures one live URL and prints the pageCtx summary + screenshot path.
//
// Run: npm run eval:pw-engine -- https://lusion.co/

const { app } = require('electron');

const TARGET = process.argv.find((a) => /^https?:\/\//.test(a)) || 'https://lusion.co/';

async function run() {
  const engine = require('../../electron/capture-engine.js');
  console.log('[engine] active:', engine.activeEngine());
  const stamp = Math.floor(Date.now() / 1000);
  console.log(`[engine] capturing ${TARGET}`);
  const t0 = Date.now();
  const ctx = await engine.capturePage(TARGET, {
    captureStamp: stamp,
    settleBeforeShotMs: 9000,
    onStep: (label, delta) => {
      if (delta >= 1500) console.log(`  ⏱ ${label}: ${(delta / 1000).toFixed(1)}s`);
    },
  });
  console.log(`[engine] done in ${((Date.now() - t0) / 1000).toFixed(1)}s`);
  console.log('  screenshotPath:', ctx.screenshotPath);
  console.log('  size:', ctx.width + 'x' + ctx.height, ctx.capped ? '(capped)' : '');
  console.log(
    '  chunks:',
    (ctx.chunks || []).length,
    '→',
    (ctx.chunks || []).map((c) => `${c.width}x${c.height}`).join(', '),
  );
  console.log('  finalUrl:', ctx.finalUrl);
  console.log('  title:', ctx.title);
  console.log('  html chars:', (ctx.html || '').length);

  const probe = await ctx
    .evaluate('({ canvases: document.querySelectorAll("canvas").length })')
    .catch((e) => ({ error: String(e) }));
  console.log('  evaluate probe:', JSON.stringify(probe));

  await ctx.dispose();
  await engine.closeBrowser();
}

app.whenReady().then(async () => {
  try {
    await run();
    app.exit(0);
  } catch (err) {
    console.error('[engine] FAILED', err);
    app.exit(1);
  }
});
