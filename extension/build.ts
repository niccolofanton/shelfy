// Builds the Shelfy MV3 extension, then sanity-checks it (P2 lane rule 8).
//
//   pnpm exec tsx extension/build.ts [--origin <url>] [--debug] [--out <dir>]
//
//   --origin  the Shelfy origin the build talks to and accepts messages from (default
//             https://refs.niccolofanton.dev; http://localhost:<port> for development and e2e)
//   --debug   adds the MAIN-world request census and its panel section
//   --out     output folder (default extension/dist)
//
// Load the result in Chrome: chrome://extensions → Developer mode → Load unpacked → the output
// folder. The manifest `key` keeps the extension's ID the same for every build (src/id.ts).

import * as esbuild from 'esbuild';
import { createHash } from 'node:crypto';
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { EXTENSION_ID } from './src/id';
import {
  FILES,
  buildManifest,
  validateBundles,
  validateManifest,
  type ChromeManifest,
} from './src/manifest';
import { DEFAULT_SHELFY_ORIGIN, parseShelfyOrigin } from './src/shared/hosts';

const here = dirname(fileURLToPath(import.meta.url));
const src = join(here, 'src');
const repoRoot = join(here, '..');
const HOOK_SOURCE = join(repoRoot, 'electron', 'webview-injected.ts');

export interface ExtensionBuildOptions {
  origin?: string;
  debug?: boolean;
  outDir?: string;
}

export interface ExtensionBuild {
  outDir: string;
  origin: string;
  debug: boolean;
  parser: string;
  manifest: ChromeManifest;
  files: Array<{ name: string; bytes: number }>;
  problems: string[];
}

/** Id of the bundled capture hook (`client.parser`, C5): the start of its source's SHA-256. */
export function parserBuildId(): string {
  return createHash('sha256').update(readFileSync(HOOK_SOURCE)).digest('hex').slice(0, 12);
}

export async function buildExtension(options: ExtensionBuildOptions = {}): Promise<ExtensionBuild> {
  const origin = parseShelfyOrigin(options.origin ?? DEFAULT_SHELFY_ORIGIN);
  if (!origin)
    throw new Error(
      `--origin must be https://<host> or http://localhost:<port>, not "${options.origin}"`,
    );
  const debug = options.debug ?? false;
  const outDir = resolve(options.outDir ?? join(here, 'dist'));
  const parser = parserBuildId();

  const common: esbuild.BuildOptions = {
    bundle: true,
    platform: 'browser',
    target: 'chrome120',
    // Readable output: easier review and debugging in the page.
    minify: false,
    sourcemap: false,
    // keepNames would inject a __name() helper, which breaks functions chrome.scripting
    // serializes into a page (the IG replay): they must stay self-contained.
    keepNames: false,
    legalComments: 'none',
    logLevel: 'warning',
    outdir: outDir,
    define: {
      __SHELFY_ORIGIN__: JSON.stringify(origin),
      __SHELFY_DEBUG__: JSON.stringify(debug),
      __SHELFY_PARSER__: JSON.stringify(parser),
    },
  };

  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });

  await esbuild.build({
    ...common,
    format: 'iife',
    entryPoints: {
      'hook.main': join(src, 'content', debug ? 'hook.debug.ts' : 'hook.main.ts'),
      bridge: join(src, 'content', 'bridge.ts'),
      'select.main': join(repoRoot, 'electron', 'webview-select.ts'),
      panel: join(src, 'panel', 'panel.ts'),
    },
  });
  await esbuild.build({
    ...common,
    format: 'esm',
    entryPoints: { sw: join(src, 'sw', 'index.ts') },
  });

  copyFileSync(join(src, 'panel', 'panel.html'), join(outDir, FILES.panelHtml));
  copyFileSync(join(src, 'panel', 'panel.css'), join(outDir, FILES.panelCss));
  // The desktop app icon (1024 px PNG); Chrome scales it down for the toolbar and the menu.
  copyFileSync(join(repoRoot, 'build', 'icon.png'), join(outDir, FILES.icon));

  const manifest = buildManifest({ origin, debug });
  writeFileSync(join(outDir, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);

  const readDist = (relativePath: string): string | null => {
    const path = join(outDir, relativePath);
    return existsSync(path) ? readFileSync(path, 'utf8') : null;
  };
  const problems = [
    ...validateManifest(manifest, { origin, debug }, (relativePath) =>
      existsSync(join(outDir, relativePath)),
    ),
    ...validateBundles(readDist, { debug }),
  ];
  const files = ['manifest.json', ...Object.values(FILES)].map((name) => ({
    name,
    bytes: existsSync(join(outDir, name)) ? statSync(join(outDir, name)).size : 0,
  }));
  return { outDir, origin, debug, parser, manifest, files, problems };
}

async function main(): Promise<void> {
  const { values } = parseArgs({
    options: {
      origin: { type: 'string' },
      debug: { type: 'boolean' },
      out: { type: 'string' },
    },
    strict: true,
  });
  const build = await buildExtension({
    origin: values.origin,
    debug: values.debug,
    outDir: values.out,
  });
  if (build.problems.length) {
    console.error(`[extension] sanity check failed (${build.problems.length}):`);
    for (const problem of build.problems) console.error(`  - ${problem}`);
    process.exit(1);
  }
  console.log(`[extension] built ${build.files.length} files into ${build.outDir}`);
  for (const file of build.files)
    console.log(`  ${file.name.padEnd(14)} ${(file.bytes / 1024).toFixed(1).padStart(7)} KB`);
  console.log(
    `[extension] id ${EXTENSION_ID} · origin ${build.origin} · parser ${build.parser}` +
      (build.debug ? ' · debug' : ''),
  );
  console.log(
    '[extension] sanity check passed: manifest, permissions, hosts, content scripts and bundles',
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href)
  await main();
