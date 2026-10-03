#!/usr/bin/env node
/**
 * Builds the SPIKE-11 capture harness into a self-contained directory that runs
 * with plain Node 24 (for example in mcr.microsoft.com/playwright, or copied to
 * the VPS):
 *
 *   out/run-site.cjs           src/run-site.ts + electron/webcap/* bundled by esbuild,
 *                              with `electron` replaced by src/electron-shim.ts
 *   out/resources/adblock/     the compiled content-blocking engine (build/adblock)
 *   out/fixtures/              the six capture fixtures (scripts/web-capture-eval)
 *   out/ssrf-probe.mjs         the SPIKE-4 probe suite (scripts/spikes/ssrf-probe.mjs)
 *   out/node_modules/          the runtime dependencies from ./package.json
 *
 * Usage (from the repo root):
 *   pnpm install --frozen-lockfile                              # root: esbuild, build/adblock/engine.bin
 *   pnpm --dir scripts/spikes/capture-harness install --frozen-lockfile --prod
 *   node scripts/spikes/capture-harness/build.mjs [--out <dir>]
 */

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { build } from 'esbuild';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..', '..', '..');
const outArg = process.argv.indexOf('--out');
const OUT = outArg > 0 ? path.resolve(process.argv[outArg + 1]) : path.join(HERE, 'out');

const RUNTIME_DEPS = Object.keys(
  JSON.parse(fs.readFileSync(path.join(HERE, 'package.json'), 'utf8')).dependencies,
);

function need(file, hint) {
  if (!fs.existsSync(file)) {
    console.error(`missing ${path.relative(ROOT, file)}: ${hint}`);
    process.exit(1);
  }
}

need(path.join(ROOT, 'build', 'adblock', 'engine.bin'), 'run `pnpm install` at the repo root');
need(
  path.join(HERE, 'node_modules', 'playwright-core'),
  'run `pnpm --dir scripts/spikes/capture-harness install --frozen-lockfile --prod`',
);

fs.rmSync(OUT, { recursive: true, force: true });
fs.mkdirSync(OUT, { recursive: true });

await build({
  entryPoints: [path.join(HERE, 'src', 'run-site.ts')],
  outfile: path.join(OUT, 'run-site.cjs'),
  bundle: true,
  platform: 'node',
  target: 'node24',
  format: 'cjs',
  sourcemap: true,
  logLevel: 'warning',
  alias: { electron: path.join(HERE, 'src', 'electron-shim.ts') },
  // Loaded at runtime from out/node_modules; ffmpeg-static is optional in
  // electron/webcapture.ts and absent here (FFMPEG_BIN points at ffmpeg).
  external: [...RUNTIME_DEPS, 'ffmpeg-static'],
});

const copy = (from, to) => fs.cpSync(from, to, { recursive: true, dereference: true });
copy(
  path.join(ROOT, 'build', 'adblock', 'engine.bin'),
  path.join(OUT, 'resources', 'adblock', 'engine.bin'),
);
copy(path.join(ROOT, 'scripts', 'web-capture-eval', 'fixtures'), path.join(OUT, 'fixtures'));
copy(path.join(ROOT, 'scripts', 'spikes', 'ssrf-probe.mjs'), path.join(OUT, 'ssrf-probe.mjs'));
copy(path.join(HERE, 'node_modules'), path.join(OUT, 'node_modules'));

console.log(`built ${path.relative(process.cwd(), OUT) || '.'}`);
