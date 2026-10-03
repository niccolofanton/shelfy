// Build the capture service into a self-contained CJS bundle (capture/dist), the
// way the P4-13 image runs it: plain Node 24 with `electron` replaced by the
// shim (electron/ stays untouched, D25). Runtime deps (playwright-core, the
// adblocker, autoconsent, tldts, ffmpeg-static, zod) stay external and resolve
// from the image's node_modules; the adblock engine and the recorded fixtures are
// copied in.
//
// Usage (from the repo root):  pnpm capture:build  [--out <dir>]

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { build } from 'esbuild';
import { manifestJsonSchema } from './src/protocol';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..');
const outArg = process.argv.indexOf('--out');
const OUT = outArg > 0 ? path.resolve(process.argv[outArg + 1]) : path.join(HERE, 'dist');

const EXTERNAL = [
  'playwright-core',
  '@ghostery/adblocker',
  '@ghostery/adblocker-playwright',
  '@duckduckgo/autoconsent',
  'tldts-experimental',
  'ffmpeg-static',
  'zod',
];

function need(file: string, hint: string): void {
  if (!fs.existsSync(file)) {
    console.error(`missing ${path.relative(ROOT, file)}: ${hint}`);
    process.exit(1);
  }
}

async function main(): Promise<void> {
  need(path.join(ROOT, 'build', 'adblock', 'engine.bin'), 'run `pnpm install` at the repo root');

  // Keep the committed protocol JSON Schema in sync with zod (the test asserts it
  // too). Only rewrite when it changed semantically, so a normal build never
  // dirties the tree over formatting (the committed file is prettier-formatted).
  const schemaPath = path.join(HERE, 'protocol.schema.json');
  const generated = manifestJsonSchema();
  const current = fs.existsSync(schemaPath)
    ? JSON.parse(fs.readFileSync(schemaPath, 'utf8'))
    : null;
  if (JSON.stringify(current) !== JSON.stringify(generated)) {
    fs.writeFileSync(schemaPath, JSON.stringify(generated, null, 2) + '\n');
    console.log('regenerated capture/protocol.schema.json');
  }

  fs.rmSync(OUT, { recursive: true, force: true });
  fs.mkdirSync(OUT, { recursive: true });

  await build({
    entryPoints: [path.join(HERE, 'src', 'main.ts')],
    outfile: path.join(OUT, 'server.cjs'),
    bundle: true,
    platform: 'node',
    target: 'node24',
    format: 'cjs',
    sourcemap: true,
    logLevel: 'warning',
    alias: { electron: path.join(HERE, 'src', 'electron-shim.ts') },
    external: EXTERNAL,
  });

  const copy = (from: string, to: string): void => {
    fs.mkdirSync(path.dirname(to), { recursive: true });
    fs.cpSync(from, to, { recursive: true, dereference: true });
  };
  // The prebuilt adblock engine (the shim reports a packaged app; blocker.ts reads
  // SHELFY_ADBLOCK_ENGINE, which the image points at resources/adblock/engine.bin).
  copy(
    path.join(ROOT, 'build', 'adblock', 'engine.bin'),
    path.join(OUT, 'resources', 'adblock', 'engine.bin'),
  );
  copy(path.join(HERE, 'protocol.schema.json'), path.join(OUT, 'protocol.schema.json'));
  const recorded = path.join(HERE, 'fixtures', 'recorded');
  if (fs.existsSync(recorded)) copy(recorded, path.join(OUT, 'fixtures', 'recorded'));

  console.log(`built ${path.relative(process.cwd(), OUT) || '.'}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
