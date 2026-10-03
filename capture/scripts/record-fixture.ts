// Record a capture against a local fixture page into capture/fixtures/recorded/,
// for fake.ts replay and the parity/ingest tests (P4-14). No network: the fixture
// pages are served by Playwright's router (fixtures.shelfy.test), as SPIKE-11 did.
//
// Usage (repo root):
//   pnpm exec tsx capture/scripts/record-fixture.ts [--fixture native-tall] [--name basic] [--video]

import fs from 'fs';
import os from 'os';
import path from 'path';
import { getBrowser, closeBrowser, setBrowserLaunchOverrides } from '../../electron/webcap/browser';
import { setFfmpegThreads } from '../../electron/webcap/encode';
import { serveFixturesOnBrowser, FIXTURE_ORIGIN } from '../tests/fixtures-serve';
import { runCapture, type EmitLine } from '../src/run';
import { ENV } from '../src/env';
import type { CaptureLine } from '../src/protocol';

// Paths are cwd-based so this records correctly whether run via tsx or esbuild
// bundled (the bundle, like the service, aliases `electron` to the shim). Run it
// from the repo root.
const ROOT = process.cwd();
const FIX_DIR = path.join(ROOT, 'scripts', 'web-capture-eval', 'fixtures');

function arg(name: string, fallback: string): string {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 && process.argv[i + 1] ? process.argv[i + 1] : fallback;
}

async function main(): Promise<void> {
  const fixture = arg('fixture', 'native-tall');
  const name = arg('name', 'basic');
  const wantVideo = process.argv.includes('--video');
  process.env.SHELFY_ADBLOCK_ENGINE = path.join(ROOT, 'build', 'adblock', 'engine.bin');

  const base = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-record-'));
  process.env.CAPTURE_WORK_BASE = base;

  setBrowserLaunchOverrides({ selfHeal: false });
  setFfmpegThreads(1);
  const browser = await getBrowser();
  serveFixturesOnBrowser(browser, FIX_DIR);

  const captureId = '01JRECORDFIXTURE0000000000';
  const lines: CaptureLine[] = [];
  const emit: EmitLine = (l) => lines.push(l);

  await runCapture(
    {
      captureId,
      url: `${FIXTURE_ORIGIN}/${fixture}.html`,
      maxPages: 1,
      singlePage: true,
      video: wantVideo,
      workDir: `/work/${captureId}`,
    },
    {
      emit,
      signal: new AbortController().signal,
      env: { ...ENV, workBase: base, video: wantVideo, pagesParallel: 1, deviceScale: 1 },
    },
  );
  await closeBrowser();

  const workDir = path.join(base, captureId);
  const out = path.join(ROOT, 'capture', 'fixtures', 'recorded', name);
  fs.rmSync(out, { recursive: true, force: true });
  fs.mkdirSync(out, { recursive: true });
  for (const f of fs.readdirSync(workDir)) {
    fs.copyFileSync(path.join(workDir, f), path.join(out, f));
  }
  fs.writeFileSync(
    path.join(out, 'lines.ndjson'),
    lines.map((l) => JSON.stringify(l)).join('\n') + '\n',
  );
  fs.rmSync(base, { recursive: true, force: true });

  const done = lines.find((l) => l.type === 'done');
  const failed = lines.find((l) => l.type === 'failed');
  console.log(
    `recorded ${name}: ${lines.length} lines, ${fs.readdirSync(out).length} files -> ${path.relative(ROOT, out)}`,
  );
  console.log(done ? `done: ${JSON.stringify(done)}` : `FAILED: ${JSON.stringify(failed)}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
