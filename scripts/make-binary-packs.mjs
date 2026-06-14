#!/usr/bin/env node
// Build the macOS "mini binary pack" (.tar.gz) for the runtime provisioner.
//
// On macOS the only sidecars NOT fetched directly from an upstream release are
// whisper-server (no upstream macOS binary — built from source) and ffmpeg (no
// stable per-file macOS URL — taken from the ffmpeg-static npm dep). yt-dlp and
// llama-server are downloaded directly from upstream by electron/binaries.js, so
// they are NOT in this pack. On Windows every sidecar is fetched directly, so no
// pack is produced there.
//
// Pack layout:
//   bin/ffmpeg
//   whisper/<whisper-server + dylibs>
//
// Output (NOT uploaded here — the CI release job publishes it to GitHub Releases):
//   release/shelfy-bin-<platform>-<arch>.tar.gz
//   release/binaries.json   { packs: { "<key>": { url, sha512, size } } }
//
// Env: WHISPER_DIR (default .vlm/whisper) — the built whisper-server tree.
// Usage: node scripts/make-binary-packs.mjs [--dry-run]

import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  writeFileSync,
  rmSync,
  existsSync,
  statSync,
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const RELEASE_DIR = path.join(ROOT, 'release');

const DRY_RUN = process.argv.includes('--dry-run');
const PLATFORM = process.platform;
const ARCH = process.arch;
const isWin = PLATFORM === 'win32';
const exe = (b) => (isWin ? `${b}.exe` : b);
const packKey = `${PLATFORM}-${ARCH}`;

function die(msg) {
  console.error(`[make-packs] ${msg}`);
  process.exit(1);
}

// ── Locate the source binaries for the mini pack (ffmpeg + whisper only) ───────
function ffmpegSource() {
  if (isWin) return path.join(ROOT, 'bin', 'ffmpeg.exe');
  try {
    return require('ffmpeg-static');
  } catch {
    return path.join(ROOT, 'bin', 'ffmpeg');
  }
}
const whisperDir = process.env.WHISPER_DIR || path.join(ROOT, '.vlm', 'whisper');

const sources = {
  [path.join('bin', exe('ffmpeg'))]: ffmpegSource(),
  whisper: whisperDir,
};
for (const [rel, src] of Object.entries(sources)) {
  if (!existsSync(src)) die(`missing source for "${rel}": ${src}`);
}

// ── Stage into a temp tree, then tar.gz it ───────────────────────────────────
const stage = mkdtempSync(path.join(os.tmpdir(), 'shelfy-pack-'));
process.on('exit', () => {
  try {
    rmSync(stage, { recursive: true, force: true });
  } catch {}
});
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(sig, () => process.exit(130));

console.log(`[make-packs] ${packKey} mini-pack (ffmpeg + whisper) -> shelfy-bin-${packKey}.tar.gz`);
for (const [rel, src] of Object.entries(sources)) {
  const dest = path.join(stage, rel);
  mkdirSync(path.dirname(dest), { recursive: true });
  // verbatimSymlinks: keep symlink targets EXACTLY as written. whisper's shared
  // libs ship as RELATIVE symlinks resolved via @loader_path; without this flag
  // cpSync rewrites them to ABSOLUTE paths on THIS build machine, which dangle on
  // every other Mac and make whisper-server die at load.
  cpSync(src, dest, { recursive: statSync(src).isDirectory(), verbatimSymlinks: true });
}

if (DRY_RUN) {
  console.log(
    `[make-packs] (dry-run) would write release/shelfy-bin-${packKey}.tar.gz + release/binaries-${packKey}.json`,
  );
  process.exit(0);
}

// `tar` ships on macOS, Linux and Windows 10+. -C stage . preserves the tree + exec bits.
mkdirSync(RELEASE_DIR, { recursive: true });
const outName = `shelfy-bin-${packKey}.tar.gz`;
const outPath = path.join(RELEASE_DIR, outName);
const res = spawnSync('tar', ['-czf', outPath, '-C', stage, '.'], { stdio: 'inherit' });
if (res.status !== 0) die('tar failed');

const buf = readFileSync(outPath);
const sha512 = createHash('sha512').update(buf).digest('base64');
const size = buf.length;

// Write a per-key manifest FRAGMENT (release/binaries-<key>.json). Packs built on
// different runners (darwin-arm64, linux-x64) must not clobber a shared file, so
// each emits its own fragment and the CI release job merges them into binaries.json.
const fragmentPath = path.join(RELEASE_DIR, `binaries-${packKey}.json`);
writeFileSync(
  fragmentPath,
  JSON.stringify({ packs: { [packKey]: { url: outName, sha512, size } } }, null, 2),
);

console.log(
  `[make-packs] ${outName}  ${(size / 1e6).toFixed(1)} MB  sha512=${sha512.slice(0, 16)}…  -> release/binaries-${packKey}.json`,
);
