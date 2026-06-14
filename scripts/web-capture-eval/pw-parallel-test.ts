'use strict';

// Verify the capture engine runs N pages CONCURRENTLY in one shared browser
// (separate contexts), matching the orchestrator's 4-up page pool. Captures 4 URLs
// at once and prints wall-clock — should be ~one page's time, not the sum.
//
// Run: electron scripts/web-capture-eval/pw-parallel-test.cjs

import { app } from 'electron';

const URLS = [
  'https://example.com/',
  'https://lusion.co/',
  'https://www.we-go.it/',
  'https://lusion.co/about',
];

interface ParallelResult {
  url: string;
  ok: boolean;
  ms: number;
  size?: string;
  chunks?: number;
  error?: string;
}

async function run(): Promise<void> {
  const engine: typeof import('../../electron/capture-engine') = require('../../electron/capture-engine');
  const stamp = Math.floor(Date.now() / 1000);
  console.log(
    `[parallel] capturing ${URLS.length} URLs concurrently (engine=${engine.activeEngine()})`,
  );
  const t0 = Date.now();
  const results = await Promise.all(
    URLS.map(async (url): Promise<ParallelResult> => {
      const tA = Date.now();
      try {
        const ctx = await engine.capturePage(url, {
          captureStamp: stamp,
          settleBeforeShotMs: 8000,
        });
        const chunks = (ctx as { chunks?: unknown[] }).chunks;
        const out: ParallelResult = {
          url,
          ok: true,
          ms: Date.now() - tA,
          size: `${ctx.width}x${ctx.height}`,
          chunks: (chunks || []).length,
        };
        await ctx.dispose();
        return out;
      } catch (e) {
        return { url, ok: false, ms: Date.now() - tA, error: String(e && (e as Error).message) };
      }
    }),
  );
  const wall = ((Date.now() - t0) / 1000).toFixed(1);
  const sum = (results.reduce((a, r) => a + r.ms, 0) / 1000).toFixed(1);
  for (const r of results) {
    console.log(
      `  ${r.ok ? 'OK ' : 'ERR'} ${(r.ms / 1000).toFixed(1)}s  ${r.size || ''} ${r.chunks != null ? r.chunks + 'ch' : ''}  ${r.url}${r.error ? ' — ' + r.error : ''}`,
    );
  }
  console.log(
    `[parallel] wall-clock=${wall}s  (sum-of-pages=${sum}s) → speedup ~${(Number(sum) / Number(wall)).toFixed(1)}×`,
  );
  await engine.closeBrowser();
}

app.whenReady().then(async () => {
  try {
    await run();
    app.exit(0);
  } catch (err) {
    console.error('[parallel] FAILED', err);
    app.exit(1);
  }
});
