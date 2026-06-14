#!/usr/bin/env node
// Generate the Windows "self-rebuild" update feed for a release.
//
// The Windows updater (electron/updater.js) does not ship a prebuilt installer:
// it reads a JSON manifest ({version, channel, zip, sha512}) and downloads a
// source zip that it rebuilds locally. This script produces those two artifacts
// into release/ so the CI release job can upload them to the GitHub Release
// alongside the electron-builder outputs.
//
//   release/SHELFY-src-<version>.zip   source archive (git HEAD, versioned files)
//   release/source.json                stable manifest (or source-beta.json) — names the zip
//
// Env:
//   SHELFY_VERSION   version to stamp (default: package.json version)
//   SHELFY_CHANNEL   "beta" → source-beta.json (default: stable → source.json)
//
// Produces the same source.json + zip layout the installed Windows clients expect
// (electron/updater.js: {version, channel, zip, sha512}).

import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const RELEASE_DIR = path.join(ROOT, 'release');

function pkgVersion(): string {
  try {
    return (
      JSON.parse(readFileSync(path.join(ROOT, 'package.json'), 'utf8')) as { version: string }
    ).version;
  } catch {
    return 'src';
  }
}

const VERSION = process.env.SHELFY_VERSION || pkgVersion();
const CHANNEL = process.env.SHELFY_CHANNEL === 'beta' ? 'beta' : 'stable';
const SOURCE_MANIFEST = CHANNEL === 'beta' ? 'source-beta.json' : 'source.json';

mkdirSync(RELEASE_DIR, { recursive: true });

const zipPath = path.join(RELEASE_DIR, `SHELFY-src-${VERSION}.zip`);
const res = spawnSync('git', ['archive', '--format=zip', '-o', zipPath, 'HEAD'], {
  cwd: ROOT,
  stdio: ['ignore', 'pipe', 'pipe'],
  encoding: 'utf8',
});
if (res.status !== 0) {
  console.error('[make-source-feed] git archive failed:\n' + (res.stderr || res.stdout));
  process.exit(1);
}

const sha512 = createHash('sha512').update(readFileSync(zipPath)).digest('base64');
const manifest = { version: VERSION, channel: CHANNEL, zip: path.basename(zipPath), sha512 };
writeFileSync(path.join(RELEASE_DIR, SOURCE_MANIFEST), JSON.stringify(manifest, null, 2));

console.log(
  `[make-source-feed] wrote ${path.basename(zipPath)} + ${SOURCE_MANIFEST} (v${VERSION}, ${CHANNEL})`,
);
