// Transpila il main process Electron da `electron/` a `dist-electron/`.
//
// Strategia: transpile-only (bundle:false) file-per-file. NON impacchetta i
// node_modules — i moduli nativi (better-sqlite3), i `require` dinamici
// (ffmpeg-static, playwright-core) e gli asarUnpack restano intatti, esattamente
// come quando il main era JS puro. esbuild si limita a riscrivere import/export
// TS in `require`/`exports` CommonJS, preservando la struttura delle cartelle.
//
// One exception: the modules of `shared/` (the AI prompts and schemas of shared/ai/,
// shared with the web server) live outside `electron/`, so a relative `require` of
// them would not resolve from dist-electron/. The files that import them are bundled
// with only the `shared/` modules inlined; every other import stays a `require`.
//
// Uso:
//   tsx build/esbuild-electron.ts           build una tantum
//   tsx build/esbuild-electron.ts --watch   ricostruisce a ogni modifica

import * as esbuild from 'esbuild';
import { readdirSync, readFileSync, statSync, mkdirSync, writeFileSync } from 'node:fs';
import { join, dirname, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, '..');
const srcDir = join(root, 'electron');
const outDir = join(root, 'dist-electron');
const sharedDir = join(root, 'shared');

const SRC_EXT = /\.(ts|js|cjs|mjs)$/;

function walk(dir: string, acc: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) {
      walk(full, acc);
    } else if (SRC_EXT.test(name) && !name.endsWith('.d.ts')) {
      acc.push(full);
    }
  }
  return acc;
}

const entryPoints = walk(srcDir);

// Sources that import a module of shared/ (e.g. `from '../shared/ai/catalog'`).
const IMPORTS_SHARED = /\bfrom\s+['"](?:\.\.\/)+shared\//;
const sharedImporters = entryPoints.filter((file) =>
  IMPORTS_SHARED.test(readFileSync(file, 'utf8')),
);

// In their bundles, shared/ goes into the file and everything else stays external.
const inlineSharedOnly: esbuild.Plugin = {
  name: 'inline-shared-only',
  setup(build) {
    build.onResolve({ filter: /.*/ }, (args) => {
      if (args.kind === 'entry-point') return undefined;
      const local = args.path.startsWith('.');
      if (local && resolve(args.resolveDir, args.path).startsWith(sharedDir + sep))
        return undefined;
      return { path: args.path, external: true };
    });
  },
};

const common: esbuild.BuildOptions = {
  outdir: outDir,
  outbase: srcDir,
  platform: 'node',
  format: 'cjs',
  target: 'node22',
  sourcemap: true,
  logLevel: 'info',
};

const options: esbuild.BuildOptions = {
  ...common,
  entryPoints: entryPoints.filter((file) => !sharedImporters.includes(file)),
  bundle: false,
};

const sharedOptions: esbuild.BuildOptions = {
  ...common,
  entryPoints: sharedImporters,
  bundle: true,
  plugins: [inlineSharedOnly],
};

// dist-electron eredita altrimenti `"type": "module"` dal package.json root e
// Node tratterebbe gli output .js come ESM. Lo forziamo a CommonJS.
function writeTypeMarker(): void {
  mkdirSync(outDir, { recursive: true });
  writeFileSync(join(outDir, 'package.json'), JSON.stringify({ type: 'commonjs' }, null, 2) + '\n');
}

const watch = process.argv.includes('--watch');

if (watch) {
  // A file that starts importing shared/ while watching needs a restart.
  const contexts = await Promise.all([esbuild.context(options), esbuild.context(sharedOptions)]);
  writeTypeMarker();
  await Promise.all(contexts.map((ctx) => ctx.watch()));
  console.log('[esbuild-electron] watching electron/ and shared/ → dist-electron/');
} else {
  await Promise.all([esbuild.build(options), esbuild.build(sharedOptions)]);
  writeTypeMarker();
  console.log(`[esbuild-electron] built ${entryPoints.length} files → dist-electron/`);
}
