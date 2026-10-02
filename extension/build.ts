// Builds the SPIKE-3 Chrome MV3 extension into extension/dist, then sanity-checks it.
//
//   pnpm exec tsx extension/build.ts
//
// Load the result in Chrome: chrome://extensions → Developer mode → Load unpacked →
// select extension/dist. See docs/web-port/spikes/03-extension-capture.md.

import * as esbuild from 'esbuild';
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { FILES, buildManifest, validateBundles, validateManifest } from './src/manifest';

const here = dirname(fileURLToPath(import.meta.url));
const src = join(here, 'src');
const dist = join(here, 'dist');
const repoRoot = join(here, '..');

const common: esbuild.BuildOptions = {
  bundle: true,
  platform: 'browser',
  target: 'chrome120',
  // Readable output: easier review for the store and for debugging in the page.
  minify: false,
  sourcemap: false,
  // keepNames would inject a __name() helper into igFeedReplay, which Chrome serializes into
  // the page for chrome.scripting.executeScript: the function must stay self-contained.
  keepNames: false,
  legalComments: 'none',
  logLevel: 'warning',
  outdir: dist,
};

rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

await esbuild.build({
  ...common,
  format: 'iife',
  entryPoints: {
    'hook.main': join(src, 'hook.main.ts'),
    bridge: join(src, 'bridge.ts'),
    panel: join(src, 'panel', 'panel.ts'),
  },
});
await esbuild.build({
  ...common,
  format: 'esm',
  entryPoints: { sw: join(src, 'sw.ts') },
});

copyFileSync(join(src, 'panel', 'panel.html'), join(dist, FILES.panelHtml));
copyFileSync(join(src, 'panel', 'panel.css'), join(dist, FILES.panelCss));
// The desktop app icon (1024 px PNG); Chrome scales it down for the toolbar and the menu.
copyFileSync(join(repoRoot, 'build', 'icon.png'), join(dist, FILES.icon));

const manifest = buildManifest();
writeFileSync(join(dist, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);

const readDist = (relativePath: string): string | null => {
  const path = join(dist, relativePath);
  return existsSync(path) ? readFileSync(path, 'utf8') : null;
};
const problems = [
  ...validateManifest(manifest, (relativePath) => existsSync(join(dist, relativePath))),
  ...validateBundles(readDist),
];
if (problems.length) {
  console.error(`[extension] sanity check failed (${problems.length}):`);
  for (const problem of problems) console.error(`  - ${problem}`);
  process.exit(1);
}

const files = ['manifest.json', ...Object.values(FILES)];
console.log(`[extension] built ${files.length} files into ${dist}`);
for (const file of files) {
  const kb = (statSync(join(dist, file)).size / 1024).toFixed(1);
  console.log(`  ${file.padEnd(14)} ${kb.padStart(7)} KB`);
}
console.log('[extension] sanity check passed: manifest, permissions, content scripts and bundles');
