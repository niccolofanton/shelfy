'use strict';

// Exercise the capture pipeline under the Electron runtime. Two modes:
//   default: drive the engine SELECTOR (capture-engine.js) so the Playwright/OSR
//            choice + fallback is what actually runs (parity with the orchestrator).
// Captures one live URL and prints the pageCtx summary + screenshot path.
//
// Run: npm run eval:pw-engine -- https://lusion.co/

import { app } from 'electron';

const TARGET = process.argv.find((a) => /^https?:\/\//.test(a)) || 'https://lusion.co/';

interface ChunkInfo {
  width: number;
  height: number;
}

interface EngineCtx {
  screenshotPath: string | null;
  width: number;
  height: number;
  capped: boolean;
  chunks?: ChunkInfo[];
  finalUrl: string;
  title: string;
  html: string;
  evaluate: (code: string) => Promise<unknown>;
  dispose: () => Promise<void>;
}

async function run(): Promise<void> {
  const engine =
    require('../../electron/capture-engine') as typeof import('../../electron/capture-engine');
  console.log('[engine] active:', engine.activeEngine());
  const stamp = Math.floor(Date.now() / 1000);
  console.log(`[engine] capturing ${TARGET}`);
  const t0 = Date.now();
  const ctx = (await engine.capturePage(TARGET, {
    captureStamp: stamp,
    settleBeforeShotMs: 9000,
    onStep: (label: string, delta: number) => {
      if (delta >= 1500) console.log(`  ⏱ ${label}: ${(delta / 1000).toFixed(1)}s`);
    },
  })) as EngineCtx;
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
    .catch((e: unknown) => ({ error: String(e) }));
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
