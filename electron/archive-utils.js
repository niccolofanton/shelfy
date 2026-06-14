'use strict';

// Helpers shared by the updater (self-rebuild source zip) and the binary
// provisioner (sidecar packs). They live in their own dependency-free module so
// binaries.js doesn't have to require updater.js (require cycle) and the
// zip-slip guard exists in exactly one place instead of two copies that must
// evolve in parallel.

const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');

// ── Feed URL ─────────────────────────────────────────────────────────────────
// app-update.yml is immutable for an installed app, but readFeedUrl is hit on
// every update poll and every provisioning download: read it once and cache the
// first successful result (it is only re-read while it keeps failing).
let _feedUrl = null;

function readFeedUrl() {
  if (_feedUrl) return _feedUrl;
  try {
    const cfgPath = path.join(process.resourcesPath, 'app-update.yml');
    const txt = fs.readFileSync(cfgPath, 'utf8');
    const m = txt.match(/^\s*url:\s*(.+?)\s*$/m);
    if (!m) return null;
    const url = m[1].replace(/^['"]|['"]$/g, '').replace(/\/+$/, '');
    // Hard requirement: the feed must be HTTPS. Every update artifact (yml
    // manifests, the source zip we extract+run, the dmg) is fetched from here, so
    // a plain-http feed would let a network MITM swap them. Refuse anything else.
    if (!/^https:\/\//i.test(url)) {
      console.error(`[updater] refusing non-HTTPS feed URL: ${url}`);
      return null;
    }
    _feedUrl = url;
    return _feedUrl;
  } catch (err) {
    console.error('[updater] cannot read app-update.yml feed URL:', err);
    return null;
  }
}

// ── Archive extraction ───────────────────────────────────────────────────────
// `tar` (bsdtar) ships with Windows 10+ and macOS; it extracts BOTH .tar.gz and
// .zip archives, so it's our single extraction primitive. Spawned async: these
// archives reach hundreds of MB and a sync spawn would freeze the main process
// ("Not Responding") for seconds, right during first-run provisioning.
function runTar(args) {
  return new Promise((resolve, reject) => {
    const child = spawn('tar', args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (s) => {
      stdout += s;
    });
    child.stderr.on('data', (s) => {
      stderr += s;
    });
    child.on('error', reject);
    child.on('close', (status) => resolve({ status, stdout, stderr }));
  });
}

// Reject an archive whose member names would escape the extraction dir (zip-slip):
// absolute paths, Windows drive letters, or any '..' path segment. bsdtar strips
// leading slashes but does NOT reject '..' members, so a tampered archive (e.g.
// the unpinned ffmpeg zip) could otherwise write into userData siblings. Validate
// the listing before extracting; throw on the first bad entry.
async function assertSafeArchive(archive) {
  const res = await runTar(['-tf', archive]);
  if (res.status !== 0)
    throw new Error(`archive listing failed (exit ${res.status}): ${res.stderr || res.stdout}`);
  for (const raw of String(res.stdout).split(/\r?\n/)) {
    const name = raw.trim();
    if (!name) continue;
    if (
      path.isAbsolute(name) ||
      /^[A-Za-z]:[\\/]/.test(name) ||
      name.split(/[\\/]/).includes('..')
    ) {
      throw new Error(`unsafe path in archive (zip-slip): ${name}`);
    }
  }
}

// Validates entry names first (assertSafeArchive) so a member can't escape destDir.
// `stripComponents` drops that many leading path segments (tar --strip-components),
// e.g. to flatten an upstream archive whose files sit under a top-level dir.
async function extractArchive(archive, destDir, { stripComponents = 0 } = {}) {
  await assertSafeArchive(archive);
  fs.mkdirSync(destDir, { recursive: true });
  const args = ['-xf', archive, '-C', destDir];
  if (stripComponents > 0) args.push(`--strip-components=${stripComponents}`);
  const res = await runTar(args);
  if (res.status !== 0)
    throw new Error(`extract failed (exit ${res.status}): ${res.stderr || res.stdout}`);
}

module.exports = { readFeedUrl, assertSafeArchive, extractArchive };
