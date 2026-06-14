'use strict';

// Cross-platform in-app updater. The feed is hosted on GitHub Releases and read
// over HTTPS. Two release channels: stable (source.json / latest-mac.yml) and
// beta (source-beta.json / beta-mac.yml). The user picks the channel in
// Settings; it is persisted to userData (the renderer's localStorage isn't reachable here).
//
// Per platform:
//   • Windows: SELF-REBUILD. We don't ship a prebuilt installer; instead the
//     client reads `source.json` ({version, zip, sha512}), and on the user's
//     confirmation downloads the source zip, runs build-windows.ps1 to produce a
//     lightweight NSIS installer locally (the sidecar binaries are downloaded by
//     electron/binaries.js from upstream, not bundled), then runs that installer
//     to update in place. Requires Node.js on the client.
//   • macOS: unsigned, so no Squirrel auto-install. We read the channel manifest
//     and, if newer, offer an in-app .dmg download.
//   • Linux: AppImage has no in-app self-replace, so we read latest-linux.yml and,
//     if newer, surface a manual prompt that opens the Releases page in the browser.

const { app, shell, dialog, Notification } = require('electron');
const fs = require('fs');
const path = require('path');
const { spawn, spawnSync } = require('child_process');
const { createHash } = require('crypto');
const { readFeedUrl, extractArchive } = require('./archive-utils');

// Poll the feed hourly. The moments that matter are already covered by an
// immediate check: at boot (initUpdater) and on channel switch (ipc
// 'app:setUpdateChannel' → checkNow), so a tighter poll only burns fetches.
const CHECK_INTERVAL_MS = 60 * 60 * 1000;

let started = false;
let _win = null;
let _timer = null;
let _pendingManifest = null; // Windows: the source.json we're tracking for rebuild

// Update state shared with the renderer (drives the non-intrusive toast + the
// affordances in Settings). Guards ensure we never restart a download/build while
// one is already in flight.
const _state = {
  status: 'idle', // idle|available|downloading|building|built|installing|downloaded|manual|error
  version: null, // version of the pending update
  manual: false, // macOS: download-only, no auto-install
  rebuild: false, // Windows: this update is applied via self-rebuild
  url: null, // macOS: .dmg URL
  sha512: null, // macOS: expected base64 sha512 of the .dmg (from *-mac.yml), verified after download
  installer: null, // Windows: path to the freshly built installer (when status='built')
  progress: 0, // 0..1 during download/build
  log: null, // Windows: last build log line
  error: null, // error message when status='error'
};

function getUpdateState() {
  return { ..._state };
}

function emit() {
  if (_win && !_win.isDestroyed()) _win.webContents.send('updater:state', getUpdateState());
}

function setState(patch) {
  Object.assign(_state, patch);
  emit();
}

// Avviso di sistema quando l'aggiornamento è pronto da installare. La striscia
// Attività in-app è facile da non notare — soprattutto su Windows, dove tra il
// download e il "pronto" c'è una build di diversi minuti durante la quale l'utente
// di solito passa ad altro. Una notifica nativa (+ lampeggio della taskbar) lo
// richiama anche con la finestra in background. Best-effort: niente deve rompersi
// se le notifiche non sono supportate/permesse.
function focusWindow() {
  if (_win && !_win.isDestroyed()) {
    if (_win.isMinimized()) _win.restore();
    _win.show();
    _win.focus();
  }
}

function notifyUpdateReady(version) {
  try {
    if (Notification.isSupported()) {
      const n = new Notification({
        title: 'Aggiornamento pronto',
        body: `SHELFY ${version || ''}`.trim() + ' è pronto da installare. Riavvia per applicarlo.',
      });
      n.on('click', focusWindow);
      n.show();
    }
  } catch (err) {
    console.error('[updater] notifica non riuscita:', err);
  }
  // Lampeggio della voce nella taskbar (Windows) / rimbalzo nel dock (macOS).
  try {
    if (_win && !_win.isDestroyed()) _win.flashFrame(true);
  } catch {
    /* best-effort */
  }
}

// ── Channel persistence ──────────────────────────────────────────────────────
const VALID_CHANNELS = new Set(['stable', 'beta']);

function channelFilePath() {
  return path.join(app.getPath('userData'), 'update-channel.json');
}

// Cached in memory: the file is read once per session and the cache is updated
// by setUpdateChannel (the only writer), so each poll skips the readFileSync.
let _channel = null;

function getUpdateChannel() {
  if (_channel) return _channel;
  try {
    const { channel } = JSON.parse(fs.readFileSync(channelFilePath(), 'utf8'));
    _channel = VALID_CHANNELS.has(channel) ? channel : 'stable';
  } catch {
    _channel = 'stable';
  }
  return _channel;
}

function setUpdateChannel(channel) {
  const prev = getUpdateChannel();
  const ch = VALID_CHANNELS.has(channel) ? channel : 'stable';
  _channel = ch;
  try {
    fs.writeFileSync(channelFilePath(), JSON.stringify({ channel: ch }));
  } catch (err) {
    console.error('[updater] cannot persist channel:', err);
  }
  // A real channel switch invalidates any pending notification from the previous
  // channel (a stale 'available'/'built' would otherwise linger in the toast).
  // Don't disturb an in-flight download/build/install; checkNow re-evaluates after.
  if (ch !== prev && !['downloading', 'building', 'installing'].includes(_state.status)) {
    _pendingManifest = null;
    setState({
      status: 'idle',
      version: null,
      manual: false,
      rebuild: false,
      url: null,
      sha512: null,
      installer: null,
      progress: 0,
      log: null,
      error: null,
    });
  }
  return ch;
}

// macOS manifest channel name (electron-builder channel hierarchy: beta → beta).
function manifestChannel(channel) {
  return channel === 'beta' ? 'beta' : 'latest';
}

// Source manifest filename for the self-rebuild (Windows) path.
function sourceManifestName(channel) {
  return channel === 'beta' ? 'source-beta.json' : 'source.json';
}

// Per-channel feed base. Stable reads from `…/releases/latest/download`; beta
// builds are published as GitHub *pre-releases* (which `releases/latest/download`
// never resolves to), so they live under a rolling `beta` tag. A non-GitHub or
// non-`latest/download` feed is returned unchanged.
function feedForChannel(channel) {
  const feed = readFeedUrl();
  if (!feed) return null;
  if (channel === 'beta') {
    return feed.replace(/\/releases\/latest\/download\/?$/, '/releases/download/beta');
  }
  return feed;
}

// "newer than" semver compare. Handles the beta pre-release suffix
// `X.Y.Z-beta.N`: per semver a build WITH a pre-release tag is older
// than the same X.Y.Z without one, and two pre-releases are ordered by their
// dot-separated identifiers (numeric when both numeric). Without this, consecutive
// betas sharing X.Y.Z (differing only in the `-beta.N` counter) would never be seen
// as newer and the beta channel would never auto-update.
function isNewer(remote, local) {
  const parse = (v) => {
    const [core, pre = ''] = String(v).trim().replace(/^v/, '').split('-');
    return { nums: core.split('.').map((n) => parseInt(n, 10) || 0), pre };
  };
  const cmpPre = (a, b) => {
    if (a === b) return 0;
    if (!a) return 1; // no pre-release ranks above any pre-release
    if (!b) return -1;
    const ai = a.split('.'),
      bi = b.split('.');
    for (let i = 0; i < Math.max(ai.length, bi.length); i++) {
      if (ai[i] === undefined) return -1;
      if (bi[i] === undefined) return 1;
      const xn = parseInt(ai[i], 10),
        yn = parseInt(bi[i], 10);
      if (!Number.isNaN(xn) && !Number.isNaN(yn)) {
        if (xn !== yn) return xn > yn ? 1 : -1;
      } else if (ai[i] !== bi[i]) return ai[i] > bi[i] ? 1 : -1;
    }
    return 0;
  };
  const r = parse(remote),
    l = parse(local);
  for (let i = 0; i < 3; i++)
    if (r.nums[i] !== l.nums[i]) return (r.nums[i] || 0) > (l.nums[i] || 0);
  return cmpPre(r.pre, l.pre) > 0;
}

// A version string we're willing to put into local paths / URLs. Strict enough to
// block path traversal (no slashes, no '..') from a tampered/compromised manifest.
const VERSION_RE = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?$/;
function isValidVersion(v) {
  return typeof v === 'string' && VERSION_RE.test(v);
}

// A plain file name (no directory components, no traversal) from an untrusted
// manifest field. Returns the name, or null if it isn't a safe basename.
function safeBasename(name) {
  const s = String(name || '');
  if (!s || s !== path.basename(s) || s.includes('\\') || s.includes('/') || s.includes('..'))
    return null;
  return s;
}

// Pull the base64 sha512 that an electron-builder *-mac.yml pairs with a given
// artifact name. The yml has a `files:` array of `{ url, sha512, size }` plus a
// top-level `path`/`sha512`. We find the `url:`/`path:` line whose value equals
// `name` and return the nearest sha512 in that same block, falling back to the
// top-level sha512 when `name` matches the top-level path. Returns null if no
// matching, well-formed (base64) digest is found. No YAML lib here on purpose:
// the format is flat and we only need one field.
function ymlSha512For(yml, name) {
  if (!yml || !name) return null;
  const target = String(name).trim();
  const isB64Sha512 = (s) => typeof s === 'string' && /^[A-Za-z0-9+/]{80,}={0,2}$/.test(s);
  const lines = String(yml).split(/\r?\n/);
  // 1) Scan for a url:/path: line equal to `name`, then take the first sha512 that
  //    follows it (within the same files-array entry).
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(/^\s*-?\s*(?:url|path):\s*(\S+)\s*$/);
    if (!m) continue;
    const val = m[1].replace(/^['"]|['"]$/g, '');
    if (path.basename(val) !== target && val !== target) continue;
    for (let j = i + 1; j < Math.min(lines.length, i + 6); j++) {
      // Stop if we hit the next entry's url/path before any sha512.
      if (/^\s*-?\s*(?:url|path):/.test(lines[j])) break;
      const sm = lines[j].match(/^\s*sha512:\s*(\S+)\s*$/);
      if (sm) {
        const sha = sm[1].replace(/^['"]|['"]$/g, '');
        return isB64Sha512(sha) ? sha : null;
      }
    }
  }
  return null;
}

// Stream a URL to a file, reporting fractional progress; returns base64 sha512.
// downloadFile fires its chunk callback thousands of times for a large artifact
// (a ~150 MB .dmg/source zip). Forwarding each one as an `updater:state` event
// floods IPC and re-renders the whole Activity Center — the UI froze for ~30s
// during a macOS update download. Collapse to at most one emit per whole percent.
function perPercent(emit) {
  let last = -1;
  return (f) => {
    const p = Math.round((f || 0) * 100);
    if (p === last) return;
    last = p;
    emit(f);
  };
}

// Stall watchdog: if no chunk arrives within this window the download is aborted
// (a half-open TCP connection where the server accepts but stops sending would
// otherwise leave `reader.read()` pending forever, wedging the updater state at
// 'downloading' and blocking every re-entrancy guard until the app restarts).
const DOWNLOAD_STALL_MS = 30000;

async function downloadFile(url, dest, onProgress) {
  const controller = new AbortController();
  // Bound the CONNECT phase too: a server that accepts the socket but never sends
  // headers would leave `await fetch` pending forever (the stall watchdog below only
  // arms once headers arrive). Abort if they don't land within the stall window;
  // clear the timer as soon as fetch settles so the chunk watchdog takes over.
  const connectTimer = setTimeout(() => controller.abort(), DOWNLOAD_STALL_MS);
  let res;
  try {
    res = await fetch(url, { redirect: 'follow', signal: controller.signal });
  } finally {
    clearTimeout(connectTimer);
  }
  if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
  const total = Number(res.headers.get('content-length')) || 0;
  const hash = createHash('sha512');
  // Write to a temp file and rename only on full success, so an interrupted
  // download never leaves a truncated file that a retry would mistake for valid.
  const tmp = `${dest}.part`;
  const out = fs.createWriteStream(tmp);
  let received = 0;
  const reader = res.body.getReader();
  // Reset on every successful read; on timeout we abort so reader.read() rejects
  // and the catch below cleans up (.part removed, reader cancelled, guards freed).
  let stallTimer = null;
  const armStall = () => {
    if (stallTimer) clearTimeout(stallTimer);
    stallTimer = setTimeout(() => controller.abort(), DOWNLOAD_STALL_MS);
  };
  try {
    armStall();
    for (;;) {
      const { done, value } = await reader.read();
      armStall();
      if (done) break;
      const buf = Buffer.from(value);
      hash.update(buf);
      if (!out.write(buf))
        await new Promise((resolve, reject) => {
          const onDrain = () => {
            out.off('error', onErr);
            resolve();
          };
          const onErr = (e) => {
            out.off('drain', onDrain);
            reject(e);
          };
          out.once('drain', onDrain);
          out.once('error', onErr);
        });
      received += buf.length;
      if (onProgress && total) onProgress(received / total);
    }
    await new Promise((resolve, reject) => out.end((e) => (e ? reject(e) : resolve())));
  } catch (err) {
    // Cancel the response body so the underlying HTTP connection/socket is
    // released instead of dangling until a non-deterministic GC.
    try {
      await reader.cancel();
    } catch {
      /* ignore */
    }
    try {
      out.destroy();
    } catch {
      /* ignore */
    }
    try {
      fs.rmSync(tmp, { force: true });
    } catch {
      /* ignore */
    }
    throw err;
  } finally {
    if (stallTimer) clearTimeout(stallTimer);
  }
  fs.renameSync(tmp, dest);
  return hash.digest('base64');
}

// ── macOS: no Squirrel auto-install (unsigned build) ─────────────────────────
async function checkMacUpdate(channel) {
  const feed = feedForChannel(channel);
  if (!feed) return;
  const mc = manifestChannel(channel);

  let yml;
  try {
    const res = await fetch(`${feed}/${mc}-mac.yml`, {
      cache: 'no-store',
      signal: AbortSignal.timeout(15000),
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    yml = await res.text();
  } catch (err) {
    console.error(`[updater] mac feed fetch failed (${mc}-mac.yml):`, err);
    return;
  }

  const version = (yml.match(/^\s*version:\s*(.+?)\s*$/m) || [])[1];
  if (!version || !isValidVersion(version)) {
    console.error('[updater] mac: manifest version missing or invalid:', version);
    return;
  }
  if (!isNewer(version, app.getVersion())) {
    console.log(
      '[updater] mac: already up to date (have',
      app.getVersion(),
      'channel',
      channel,
      ')',
    );
    return;
  }
  // Already notified for this version — but keep going after an error (e.g. a
  // missing sha512 that the publisher later corrected) so the now-verifiable
  // update is re-evaluated at the next poll instead of dead-ending in 'error'
  // until 'Riprova'/restart. (Mirrors the Windows guard below.)
  if (_state.version === version && _state.status !== 'error') return;

  // productName, not app.getName() (which can be the package name) so the fallback
  // URL matches the real artifact name (SHELFY-<version>.dmg). The manifest is
  // untrusted: run the .dmg path through safeBasename so a tampered yml can't smuggle
  // '../' segments or an absolute path into the URL (matches manifest.zip in
  // rebuildNow). On a bad/unsafe name we fall back to the canonical artifact name.
  const rawDmgPath = (yml.match(/(?:path|url):\s*(\S+\.dmg)/) || [])[1];
  const dmgName = safeBasename(rawDmgPath) || `SHELFY-${version}-${process.arch}.dmg`;
  const dmgUrl = `${feed}/${dmgName}`.replace(/ /g, '%20');

  // electron-builder writes a base64 sha512 per artifact in the mac.yml: a `files:`
  // array of `{ url, sha512, size }` plus a top-level `path`/`sha512`. Pull the
  // sha512 belonging to the .dmg we chose so openDownload can verify the bytes
  // (the build is unsigned — no OS codesign gate — so this is the integrity check).
  const dmgSha512 = ymlSha512For(yml, dmgName) || ymlSha512For(yml, rawDmgPath);
  if (!dmgSha512) {
    console.warn(
      '[updater] mac: no sha512 for',
      dmgName,
      '— update will be rejected after download',
    );
  }

  console.log('[updater] mac: update available', version, '->', dmgUrl);
  setState({
    status: 'manual',
    version,
    manual: true,
    rebuild: false,
    url: dmgUrl,
    sha512: dmgSha512 || null,
  });
}

// ── Linux: AppImage, no in-app self-replace ──────────────────────────────────
// We can't safely swap a running AppImage from inside the app, so we don't do the
// Windows self-rebuild here (rebuildNow is a no-op off Windows) nor the macOS dmg
// flow. Instead: read latest-linux.yml / beta-linux.yml and, if a newer version
// exists, surface a 'manual' prompt whose action opens the Releases page in the
// browser (matches docs/install.md — the user grabs the new AppImage there).
async function checkLinuxUpdate(channel) {
  const feed = feedForChannel(channel);
  if (!feed) return;
  const mc = manifestChannel(channel);

  let yml;
  try {
    const res = await fetch(`${feed}/${mc}-linux.yml`, {
      cache: 'no-store',
      signal: AbortSignal.timeout(15000),
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    yml = await res.text();
  } catch (err) {
    console.error(`[updater] linux feed fetch failed (${mc}-linux.yml):`, err);
    return;
  }

  const version = (yml.match(/^\s*version:\s*(.+?)\s*$/m) || [])[1];
  if (!version || !isValidVersion(version)) {
    console.error('[updater] linux: manifest version missing or invalid:', version);
    return;
  }
  if (!isNewer(version, app.getVersion())) {
    console.log(
      '[updater] linux: already up to date (have',
      app.getVersion(),
      'channel',
      channel,
      ')',
    );
    return;
  }
  if (_state.version === version && _state.status !== 'error') return;

  // Open the Releases page (derived from the feed), not a per-file download.
  const releasesPage = feed.replace(/\/releases\/.*$/, '/releases');
  console.log('[updater] linux: update available', version, '-> manual (', releasesPage, ')');
  setState({
    status: 'manual',
    version,
    manual: true,
    rebuild: false,
    linux: true,
    url: releasesPage,
    sha512: null,
  });
}

// ── Windows: self-rebuild from source ────────────────────────────────────────
// Major version of the Node at `nodeExe`, or 0 if it can't run.
function nodeMajor(nodeExe) {
  try {
    const r = spawnSync(nodeExe, ['-v'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] });
    if (r.status !== 0) return 0;
    return parseInt(String(r.stdout).trim().replace(/^v/, '').split('.')[0], 10) || 0;
  } catch {
    return 0;
  }
}

// List child directories of `base` (silent if it doesn't exist).
function subdirs(base) {
  try {
    return fs
      .readdirSync(base, { withFileTypes: true })
      .filter((e) => e.isDirectory())
      .map((e) => path.join(base, e.name));
  } catch {
    return [];
  }
}

// The GUI app's PATH usually lacks per-shell version managers (fnm/nvm/volta), so
// the `node`/`npm`/`npx` the build needs can be missing even when the user builds
// fine in their terminal. Find a directory that holds Node >= 20 (npm/npx sit
// alongside) so we can prepend it to the build's PATH. Probes the common managers
// and system installs, picks the highest version, and as a last resort asks `fnm`
// directly. Returns the dir to prepend, '' if PATH's own node already works, or
// null if none was found (a node-probe.log is written for diagnosis).
function findBuildNodeDir() {
  if (process.platform !== 'win32') {
    return nodeMajor('node') >= 20 ? '' : null;
  }
  const exe = 'node.exe';
  const {
    LOCALAPPDATA: LA,
    APPDATA: AD,
    USERPROFILE: UP,
    ProgramFiles: PF,
    NVM_HOME,
    FNM_DIR,
  } = process.env;
  const cands = [];

  // fnm — <root>/node-versions/<ver>/installation (+ /<ver>) and the default alias.
  for (const root of [
    FNM_DIR,
    LA && path.join(LA, 'fnm'),
    AD && path.join(AD, 'fnm'),
    UP && path.join(UP, '.fnm'),
  ].filter(Boolean)) {
    cands.push(path.join(root, 'aliases', 'default'));
    for (const v of subdirs(path.join(root, 'node-versions')))
      cands.push(path.join(v, 'installation'), v);
  }
  // nvm-windows — <root>/v<ver>
  for (const root of [
    NVM_HOME,
    AD && path.join(AD, 'nvm'),
    UP && path.join(UP, 'AppData', 'Roaming', 'nvm'),
  ].filter(Boolean)) {
    for (const v of subdirs(root)) cands.push(v);
  }
  // volta — <LA>/Volta/tools/image/node/<ver>
  if (LA) for (const v of subdirs(path.join(LA, 'Volta', 'tools', 'image', 'node'))) cands.push(v);
  // scoop — <UP>/scoop/apps/nodejs*/current
  if (UP)
    for (const app of subdirs(path.join(UP, 'scoop', 'apps')))
      if (/nodejs/i.test(app)) cands.push(path.join(app, 'current'));
  // System installs.
  cands.push(path.join(PF || 'C:\\Program Files', 'nodejs'));
  if (process.env['ProgramFiles(x86)'])
    cands.push(path.join(process.env['ProgramFiles(x86)'], 'nodejs'));

  const tried = [];
  let best = null,
    bestMajor = 0;
  for (const d of cands) {
    const m = nodeMajor(path.join(d, exe));
    tried.push(`${String(m || '-').padStart(2)}  ${d}`);
    if (m >= 20 && m > bestMajor) {
      best = d;
      bestMajor = m;
    }
  }
  if (best) return best;

  // Last resort: ask fnm (it's often on PATH via its shim) for the default's node.
  try {
    const r = spawnSync(
      'fnm',
      ['exec', '--using=default', '--', 'node', '-e', 'process.stdout.write(process.execPath)'],
      { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] },
    );
    if (r.status === 0 && r.stdout) {
      const dir = path.dirname(r.stdout.trim());
      tried.push(`fnm exec -> ${r.stdout.trim()}`);
      if (nodeMajor(path.join(dir, exe)) >= 20) return dir;
    }
  } catch {
    /* fnm not on PATH */
  }

  if (nodeMajor('node') >= 20) return ''; // PATH node is fine
  try {
    fs.mkdirSync(rebuildRoot(), { recursive: true });
    fs.writeFileSync(path.join(rebuildRoot(), 'node-probe.log'), tried.join('\n') + '\n');
  } catch {
    /* best-effort */
  }
  return null;
}

function rebuildRoot() {
  return path.join(app.getPath('userData'), 'rebuild');
}

function installerPathFor(version) {
  return path.join(rebuildRoot(), `src-${version}`, 'release', `SHELFY-Setup-${version}.exe`);
}

// Remove rebuild artifacts from other versions (source trees carry node_modules, so
// they pile up fast). Keeps only what belongs to `keepVersion`.
function cleanupOldRebuilds(keepVersion) {
  const root = rebuildRoot();
  const keep = new Set([
    `src-${keepVersion}`,
    `src-${keepVersion}.zip`,
    `build-${keepVersion}.log`,
    'node-probe.log',
  ]);
  let entries;
  try {
    entries = fs.readdirSync(root);
  } catch {
    return;
  }
  for (const name of entries) {
    if (keep.has(name)) continue;
    if (/^src-/.test(name) || /\.zip$/.test(name) || /^build-.*\.log$/.test(name)) {
      try {
        fs.rmSync(path.join(root, name), { recursive: true, force: true });
      } catch {
        /* best-effort */
      }
    }
  }
}

// Check the source manifest; if newer, mark the update available (or 'built' when
// a matching installer was already produced in a previous, interrupted session).
async function checkWindowsUpdate(channel) {
  const feed = feedForChannel(channel);
  if (!feed) return;

  let manifest;
  try {
    const res = await fetch(`${feed}/${sourceManifestName(channel)}`, {
      cache: 'no-store',
      signal: AbortSignal.timeout(15000),
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    manifest = await res.json();
  } catch (err) {
    console.error('[updater] win source manifest fetch failed:', err);
    return;
  }

  const version = manifest?.version;
  if (!version || !isValidVersion(version)) {
    console.error('[updater] win: manifest version missing or invalid:', version);
    return;
  }
  if (!isNewer(version, app.getVersion())) {
    console.log(
      '[updater] win: already up to date (have',
      app.getVersion(),
      'channel',
      channel,
      ')',
    );
    return;
  }
  // Already tracking this version — but keep going after an error so the poll can
  // retry instead of dead-ending in 'error' until the app restarts.
  if (_state.version === version && _state.status !== 'idle' && _state.status !== 'error') return;

  _pendingManifest = manifest;

  // A previous run may have already built the installer for this version.
  if (fs.existsSync(installerPathFor(version))) {
    console.log('[updater] win: installer already built for', version);
    setState({
      status: 'built',
      version,
      rebuild: true,
      manual: false,
      installer: installerPathFor(version),
      progress: 1,
      error: null,
    });
    notifyUpdateReady(version);
    return;
  }
  console.log('[updater] win: update available', version);
  setState({ status: 'available', version, rebuild: true, manual: false, error: null });
}

// Run build-windows.ps1 in `cwd`, streaming output lines to onLine. `nodeDir` (if
// non-empty) is prepended to PATH so the build finds Node/npm/npx (see
// findBuildNodeDir). The full output is also written to rebuild/build-<ver>.log so
// client-side build failures are diagnosable.
function runBuildScript(cwd, version, nodeDir, onLine) {
  return new Promise((resolve, reject) => {
    const ps1 = path.join(cwd, 'build-windows.ps1');
    if (!fs.existsSync(ps1)) return reject(new Error('build-windows.ps1 missing in source'));

    const logPath = path.join(rebuildRoot(), `build-${version}.log`);
    let logStream = null;
    try {
      logStream = fs.createWriteStream(logPath);
    } catch {
      /* logging is best-effort */
    }

    // Prepend nodeDir to the EXISTING Path key. Windows stores it as 'Path'; adding
    // a second 'PATH' key would leave the child with a duplicate that breaks
    // executable resolution (spawn powershell ENOENT).
    const env = { ...process.env };
    if (nodeDir) {
      const pathKey = Object.keys(env).find((k) => k.toLowerCase() === 'path') || 'PATH';
      env[pathKey] = `${nodeDir}${path.delimiter}${env[pathKey] || ''}`;
    }
    // Resolve powershell by absolute path so the spawn never depends on PATH.
    const psExe =
      process.platform === 'win32'
        ? path.join(
            process.env.SystemRoot || 'C:\\Windows',
            'System32',
            'WindowsPowerShell',
            'v1.0',
            'powershell.exe',
          )
        : 'powershell';

    const child = spawn(
      psExe,
      ['-ExecutionPolicy', 'Bypass', '-NoProfile', '-File', ps1, '-Version', version],
      { cwd, windowsHide: true, env },
    );
    const onData = (b) => {
      const s = String(b);
      try {
        logStream?.write(s);
      } catch {
        /* ignore */
      }
      s.split(/\r?\n/).forEach((l) => {
        const t = l.trim();
        if (t) onLine(t);
      });
    };
    child.stdout.on('data', onData);
    child.stderr.on('data', onData);
    child.on('error', (e) => {
      try {
        logStream?.end();
      } catch {
        /* ignore */
      }
      reject(e);
    });
    child.on('close', (code) => {
      try {
        logStream?.end();
      } catch {
        /* ignore */
      }
      if (code === 0) resolve();
      else reject(new Error(`build fallita (exit ${code}) — log: ${logPath}`));
    });
  });
}

// User-initiated: download the source, build the installer locally. Leaves the
// state at 'built' so the user chooses when to restart (quitAndInstall runs it).
let _building = false;
async function rebuildNow() {
  // No self-rebuild outside Windows. Here this can only be the "Riprova" CTA on
  // the error state (macOS never reaches 'available'): degrade to a fresh check
  // so the action recovers — it re-discovers the update ('manual', with the
  // download CTA again) or leaves a truthful 'idle'. Reset the notified state
  // first: setState merges, so the stale `version` would otherwise make
  // checkMacUpdate early-return on its "already notified" guard and the error
  // would stay stuck. (Same reset as the channel-switch path in setUpdateChannel.)
  if (process.platform !== 'win32') {
    if (['downloading', 'installing'].includes(_state.status)) return false; // don't clobber an in-flight download
    _pendingManifest = null;
    setState({
      status: 'idle',
      version: null,
      manual: false,
      rebuild: false,
      url: null,
      sha512: null,
      installer: null,
      progress: 0,
      log: null,
      error: null,
    });
    checkNow();
    return true;
  }
  if (_building || _state.status === 'building' || _state.status === 'installing') return false;
  const manifest = _pendingManifest;
  if (!manifest?.version) return false;
  // The manifest is untrusted input (bucket compromise / MITM). Validate before
  // anything reaches a filesystem path or URL.
  if (!isValidVersion(manifest.version)) {
    setState({ status: 'error', error: 'versione del manifest non valida' });
    return false;
  }
  const zipName = safeBasename(manifest.zip);
  if (!zipName) {
    setState({ status: 'error', error: 'nome archivio sorgente non valido' });
    return false;
  }
  const feed = feedForChannel(getUpdateChannel());
  if (!feed) {
    setState({ status: 'error', error: 'feed non disponibile' });
    return false;
  }

  const nodeDir = findBuildNodeDir();
  if (nodeDir === null) {
    setState({
      status: 'error',
      error:
        'Node.js 20+ non trovato. Il self-rebuild richiede Node.js 20+ sul PC (https://nodejs.org).',
    });
    return false;
  }
  console.log('[updater] build node dir:', nodeDir || '(PATH)');

  _building = true;
  const version = manifest.version;
  const root = rebuildRoot();
  const srcDir = path.join(root, `src-${version}`);
  try {
    fs.mkdirSync(root, { recursive: true });
    cleanupOldRebuilds(version); // reclaim disk from previous versions' src trees

    // 1. download source zip (+ mandatory sha512 verify before we extract/run it)
    setState({ status: 'downloading', version, progress: 0, error: null });
    const zip = path.join(root, `src-${version}.zip`);
    const sha = await downloadFile(
      `${feed}/${zipName}`,
      zip,
      perPercent((f) => setState({ progress: f })),
    );
    if (!manifest.sha512 || sha !== manifest.sha512)
      throw new Error('sha512 della source zip mancante o non corrispondente');

    // 2. extract (git-archive zips have files at the root, no top-level folder)
    fs.rmSync(srcDir, { recursive: true, force: true });
    await extractArchive(zip, srcDir);
    try {
      fs.rmSync(zip, { force: true });
    } catch {
      /* the source tree is what we need now */
    }

    // 3. build the lightweight installer
    setState({ status: 'building', progress: 0, log: 'Avvio build…' });
    await runBuildScript(srcDir, version, nodeDir, (line) => setState({ log: line }));

    const installer = installerPathFor(version);
    if (!fs.existsSync(installer)) throw new Error('installer non prodotto dalla build');

    // 4. ready — wait for the user to restart
    setState({ status: 'built', installer, progress: 1, log: null });
    notifyUpdateReady(version);
    return true;
  } catch (err) {
    console.error('[updater] rebuild failed:', err);
    setState({ status: 'error', error: String(err?.message || err) });
    return false;
  } finally {
    _building = false;
  }
}

// ── Run a single check for the given channel ─────────────────────────────────
function runCheck(channel) {
  if (process.platform === 'darwin') {
    checkMacUpdate(channel).catch((err) => console.error('[updater] mac check failed:', err));
    return;
  }
  if (process.platform === 'linux') {
    checkLinuxUpdate(channel).catch((err) => console.error('[updater] linux check failed:', err));
    return;
  }
  // Windows self-rebuild: don't re-check while a build/install is in flight.
  if (['downloading', 'building', 'built', 'installing'].includes(_state.status)) {
    emit();
    return;
  }
  checkWindowsUpdate(channel).catch((err) => console.error('[updater] win check failed:', err));
}

// Restart and apply the update. macOS: n/a. Windows: run the freshly built
// installer (silent, update mode) and quit so it can replace the running app.
function quitAndInstall() {
  if (process.platform === 'win32' && _state.status === 'built' && _state.installer) {
    setState({ status: 'installing' });
    try {
      spawn(_state.installer, ['/S', '--updated', '--force-run'], {
        detached: true,
        stdio: 'ignore',
      }).unref();
    } catch (err) {
      console.error('[updater] failed to launch installer:', err);
      setState({ status: 'error', error: 'impossibile avviare l’installer' });
      return false;
    }
    setImmediate(() => app.quit());
    return true;
  }
  return false;
}

// macOS: download the .dmg in-app (with progress), open it, then ask whether to
// quit. Replacing /Applications/SHELFY.app is cleanest with the app closed, so on
// confirm we quit and let the user drag the new app over the old one.
async function openDownload() {
  if (!_state.manual || !_state.url) return false;
  if (_state.status === 'downloading') return false; // re-entrancy guard (double click)
  // Linux: no in-app AppImage replace — just open the Releases page in the browser.
  if (process.platform === 'linux') {
    try {
      await shell.openExternal(_state.url);
    } catch (err) {
      console.error('[updater] linux: openExternal failed:', err);
    }
    setState({ status: 'idle', manual: false, url: null, progress: 0 });
    return true;
  }
  const url = _state.url;
  const version = _state.version;
  const expectedSha = _state.sha512;
  // The build is unsigned (no OS codesign gate) and the .dmg is downloaded over
  // the HTTPS-pinned feed only. Without the per-file sha512 from *-mac.yml we have
  // NO integrity check on the bytes we hand to shell.openPath() — refuse rather
  // than open an unverifiable installer. (rebuildNow / ensureBinariesPack do the same.)
  if (!expectedSha) {
    console.error('[updater] mac: refusing update — no sha512 to verify the .dmg');
    setState({ status: 'error', error: 'aggiornamento non verificabile (sha512 mancante)' });
    return false;
  }
  try {
    const dir = path.join(app.getPath('userData'), 'updates');
    fs.mkdirSync(dir, { recursive: true });
    let base = 'update.dmg';
    try {
      const b = safeBasename(decodeURIComponent(url.split('/').pop() || ''));
      if (b) base = b;
    } catch {
      /* keep default */
    }
    const dest = path.join(dir, base);
    setState({ status: 'downloading', progress: 0 });
    const sha = await downloadFile(
      url,
      dest,
      perPercent((f) => setState({ progress: f })),
    );
    // Integrity gate: a mismatch means a tampered/corrupted .dmg. Delete it so it
    // can't be opened, then surface an error instead of handing it to the user.
    if (sha !== expectedSha) {
      try {
        fs.rmSync(dest, { force: true });
      } catch {
        /* ignore */
      }
      console.error('[updater] mac: sha512 mismatch on .dmg — deleted', dest);
      setState({
        status: 'error',
        error: 'verifica integrità fallita (sha512 non corrispondente)',
      });
      return false;
    }
    // Keep `version` (clear only the actionable bits) so the periodic poll won't
    // re-notify this same update in a loop after the user picks "Più tardi".
    setState({ status: 'idle', manual: false, url: null, sha512: null, progress: 0 });
    await shell.openPath(dest);

    // Confirm before quitting so the replacement in /Applications can succeed.
    const win = _win && !_win.isDestroyed() ? _win : null;
    const opts = {
      type: 'info',
      buttons: ['Chiudi SHELFY e installa', 'Più tardi'],
      defaultId: 0,
      cancelId: 1,
      title: 'Installa aggiornamento',
      message: `SHELFY ${version || ''} è pronto da installare.`,
      detail:
        'Nel disco appena aperto, trascina SHELFY nella cartella Applicazioni (sostituendo la versione attuale). Chiudo SHELFY ora così la sostituzione va a buon fine.',
    };
    const { response } = win
      ? await dialog.showMessageBox(win, opts)
      : await dialog.showMessageBox(opts);
    if (response === 0) setImmediate(() => app.quit());
    return true;
  } catch (err) {
    console.error('[updater] mac in-app download failed, opening browser:', err);
    setState({ status: 'manual' });
    shell.openExternal(url);
    return false;
  }
}

// Re-check now (after a channel switch). No-op in dev / unpackaged.
// Re-point the updater at a (re)created window so 'updater:state' pushes reach
// a live target. Used by the macOS 'activate' path after the window is recreated.
function setWindow(win) {
  _win = win || null;
}

function checkNow(win) {
  if (win) _win = win;
  if (process.env.ELECTRON_DEV === 'true' || !app.isPackaged) {
    console.log('[updater] checkNow skipped (dev / not packaged)');
    return;
  }
  runCheck(getUpdateChannel());
}

function initUpdater(win) {
  if (started) return;
  started = true;
  _win = win;

  if (process.env.ELECTRON_DEV === 'true' || !app.isPackaged) {
    console.log('[updater] skipped (dev / not packaged)');
    return;
  }

  const channel = getUpdateChannel();
  console.log('[updater] channel:', channel);
  runCheck(channel);

  if (!_timer) _timer = setInterval(() => checkNow(), CHECK_INTERVAL_MS);
}

module.exports = {
  initUpdater,
  setWindow,
  getUpdateChannel,
  setUpdateChannel,
  checkNow,
  readFeedUrl,
  getUpdateState,
  quitAndInstall,
  openDownload,
  rebuildNow,
};
