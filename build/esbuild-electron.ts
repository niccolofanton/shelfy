// Transpila il main process Electron da `electron/` a `dist-electron/`.
//
// Strategia: transpile-only (bundle:false) file-per-file. NON impacchetta i
// node_modules — i moduli nativi (better-sqlite3), i `require` dinamici
// (ffmpeg-static, playwright-core) e gli asarUnpack restano intatti, esattamente
// come quando il main era JS puro. esbuild si limita a riscrivere import/export
// TS in `require`/`exports` CommonJS, preservando la struttura delle cartelle.
//
// Uso:
//   tsx build/esbuild-electron.ts           build una tantum
//   tsx build/esbuild-electron.ts --watch   ricostruisce a ogni modifica

import * as esbuild from 'esbuild';
import { readdirSync, statSync, mkdirSync, writeFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, '..');
const srcDir = join(root, 'electron');
const outDir = join(root, 'dist-electron');

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

const options: esbuild.BuildOptions = {
  entryPoints,
  outdir: outDir,
  outbase: srcDir,
  bundle: false,
  platform: 'node',
  format: 'cjs',
  target: 'node22',
  sourcemap: true,
  logLevel: 'info',
};

// dist-electron eredita altrimenti `"type": "module"` dal package.json root e
// Node tratterebbe gli output .js come ESM. Lo forziamo a CommonJS.
function writeTypeMarker(): void {
  mkdirSync(outDir, { recursive: true });
  writeFileSync(join(outDir, 'package.json'), JSON.stringify({ type: 'commonjs' }, null, 2) + '\n');
}

const watch = process.argv.includes('--watch');

if (watch) {
  const ctx = await esbuild.context(options);
  writeTypeMarker();
  await ctx.watch();
  console.log('[esbuild-electron] watching electron/ → dist-electron/');
} else {
  await esbuild.build(options);
  writeTypeMarker();
  console.log(`[esbuild-electron] built ${entryPoints.length} files → dist-electron/`);
}
