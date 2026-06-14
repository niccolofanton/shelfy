'use strict';

// Runtime binary provisioner.
//
// The heavy sidecar binaries (yt-dlp, ffmpeg, llama-server, whisper-server) are
// NOT bundled in the installer. They are downloaded once into
//   <userData>/runtime-bin/{bin,llama,whisper}/
// and the resolvers in downloader/analyzer/stt look there first.
//
// Provisioning strategy, by platform:
//   • Windows: download each component DIRECTLY from its upstream release
//     (GitHub / gyan.dev), letting the user pick the llama GPU variant
//     (cpu/cuda/vulkan) in Settings.
//   • macOS / Linux: yt-dlp and llama-server are likewise downloaded DIRECTLY
//     from upstream (pinned + SHA-verified; macOS arm64 Metal, Linux x64 cpu/vulkan).
//     Only whisper-server (no upstream macOS/Linux binary — built from source by CI)
//     and ffmpeg (no stable per-file URL) come from a minimal "binary pack" (.tar.gz)
//     on the GitHub feed, listed in binaries.json (keys darwin-arm64 / linux-x64).

const { app } = require('electron');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { createHash } = require('crypto');
const { readFeedUrl, extractArchive } = require('./archive-utils');
const { sha256File } = require('./serverUtils');
const hardware = require('./hardware');

const MANIFEST_FILE = 'binaries.json';
const VARIANT_FILE = 'llama-variant.json';
const VALID_VARIANTS = new Set(['cpu', 'cuda', 'vulkan', 'metal']);

// Upstream release coordinates (mirror build-windows.ps1 / provision-binaries.ps1).
const LLAMA_BUILD = 'b9500';
const WHISPER_TAG = 'v1.8.5';

// yt-dlp is PINNED to a specific release rather than tracking releases/latest.
// Why: 'releases/latest' makes provisioning non-reproducible (the binary silently
// changes under us) and offers no supply-chain guarantee — a compromised or MITM'd
// release would be installed and run with no detection. Pinning a tag + verifying
// the artifact against the SHA256 published by yt-dlp (SHA2-256SUMS) gives us a
// reproducible, tamper-evident download. Bump YTDLP_VERSION and refresh the hashes
// from https://github.com/yt-dlp/yt-dlp/releases/download/<TAG>/SHA2-256SUMS when
// updating; keep build-windows.ps1 / provision-binaries.ps1 in sync.
const YTDLP_VERSION = '2026.03.17';
// Lowercase hex SHA256 of each upstream artifact for YTDLP_VERSION (from the
// release's SHA2-256SUMS). 'yt-dlp.exe' is fetched on Windows, 'yt-dlp_macos' on
// macOS — both downloaded directly from the pinned upstream release and verified
// against these hashes before use.
const YTDLP_SHA256 = {
  'yt-dlp.exe': '3db811b366b2da47337d2fcfdfe5bbd9a258dad3f350c54974f005df115a1545',
  'yt-dlp_macos': 'e80c47b3ce712acee51d5e3d4eace2d181b44d38f1942c3a32e3c7ff53cd9ed5',
  'yt-dlp_linux': 'c2b0189f581fe4a2ddd41954f1bcb7d327db04b07ed0dea97e4f1b3e09b5dd8e',
};
// Map the app's GPU variant to the llama.cpp Windows asset suffix.
const LLAMA_WIN_ASSET = { cpu: 'cpu-x64', cuda: 'cuda-12.4-x64', vulkan: 'vulkan-x64' };
// Linux (x64) asset suffix. No CUDA build is published for Linux in this llama.cpp
// release, so NVIDIA falls back to the Vulkan build (handled in ensureBinariesLinux).
const LLAMA_LINUX_ASSET = { cpu: 'ubuntu-x64', vulkan: 'ubuntu-vulkan-x64' };

// Hosts we're willing to download sidecar binaries from (mirrors the model-host
// allowlist in serverUtils.downloadFile). GitHub release downloads redirect to
// objects.githubusercontent.com / release-assets.githubusercontent.com; ffmpeg
// comes from www.gyan.dev; the macOS mini-pack from the feed host (now GitHub,
// read at runtime from app-update.yml, so it's checked dynamically below).
const DOWNLOAD_HOST_ALLOWLIST = ['github.com', 'objects.githubusercontent.com', 'gyan.dev'];
const DOWNLOAD_HOST_SUFFIX_ALLOWLIST = ['.githubusercontent.com', '.gyan.dev'];

function isAllowedDownloadHost(hostname) {
  const h = (hostname || '').toLowerCase();
  if (DOWNLOAD_HOST_ALLOWLIST.includes(h)) return true;
  if (DOWNLOAD_HOST_SUFFIX_ALLOWLIST.some((s) => h.endsWith(s))) return true;
  // The feed host (macOS mini-pack) comes from app-update.yml at runtime.
  try {
    const feed = readFeedUrl();
    if (feed && new URL(feed).hostname.toLowerCase() === h) return true;
  } catch {
    /* no feed → not allowed */
  }
  return false;
}

// Pinned SHA256 (lowercase hex) of the directly-downloaded executables/archives,
// keyed by archive (see verifyPinnedSha256 call sites). Same rationale as
// YTDLP_SHA256: a pinned hash makes the download reproducible and tamper-evident.
// A null value means "not pinned yet": the download proceeds with a loud WARNING
// instead of failing, so filling these in activates verification without breaking
// anything. Keep in sync with LLAMA_BUILD / WHISPER_TAG when bumping.
const BINARY_SHA256 = {
  // llama.cpp ${LLAMA_BUILD} macOS arm64 tarball — downloaded directly on macOS
  // (https://github.com/ggml-org/llama.cpp/releases/download/<LLAMA_BUILD>/llama-<LLAMA_BUILD>-bin-macos-arm64.tar.gz)
  'llama-macos-arm64': '3b2fca4a5c819892e087e346aca957f12b0ad6723560dedb99ffa6c68698b055',
  // llama.cpp ${LLAMA_BUILD} Linux x64 tarballs (cpu + vulkan), downloaded directly on Linux
  'llama-ubuntu-x64': 'd6d370a509166788d191261225746adc9dee0666973a989897b2224ef5073b30',
  'llama-ubuntu-vulkan-x64': '5614d2786372a95d4e5f2d7994d790aa445983969dd9ad8324fe839728738eae',
  // llama.cpp ${LLAMA_BUILD} release zips, per variant
  // (https://github.com/ggml-org/llama.cpp/releases/download/<LLAMA_BUILD>/llama-<LLAMA_BUILD>-bin-win-<asset>.zip)
  'llama-cpu-x64': '9d04ebc1af723cb11be09a0ec1f9a375934697e8d7fe57439e508a636c197a28',
  'llama-cuda-12.4-x64': 'b2b1a00f7470259b594cdd4cfc100c8fb53dc2070b83e6d2b715632e578136e7',
  'llama-vulkan-x64': '9be538f17a8d0e90493478cd20ad6e021cb408e45db4c2fe3781be2733421b57',
  // CUDA runtime zip shipped alongside the cuda build
  // (cudart-llama-bin-win-cuda-12.4-x64.zip from the same llama.cpp release)
  'cudart-cuda-12.4-x64': '8c79a9b226de4b3cacfd1f83d24f962d0773be79f1e7b75c6af4ded7e32ae1d6',
  // whisper.cpp ${WHISPER_TAG} whisper-bin-x64.zip
  // (https://github.com/ggml-org/whisper.cpp/releases/download/<WHISPER_TAG>/whisper-bin-x64.zip)
  whisper: '2a0e85915d0ff9e2a1d1b45b19973f05fbcdfb51d3af03557454acf6ccaa5e8a',
  // ffmpeg-release-essentials.zip — NOTE: the gyan.dev URL is rolling (content
  // changes per build), so pinning requires first switching to a versioned source
  // (see the TODO(supply-chain) note at the ffmpeg download below).
  ffmpeg: null, // TODO: pinnare SHA256 reale (vedi TODO(supply-chain) sotto: serve una sorgente versionata)
};

// Verify a downloaded archive/executable against its pinned SHA256 BEFORE it is
// extracted or used. If the pin is null (not filled in yet) we only WARN, so
// current behaviour is preserved until real hashes are provided.
async function verifyPinnedSha256(file, key, label) {
  const expected = BINARY_SHA256[key];
  if (!expected) {
    console.warn(
      `[binaries] WARN: ${label} scaricato SENZA verifica di integrità (hash non pinnato)`,
    );
    return;
  }
  const actual = await sha256File(file);
  if (actual.toLowerCase() !== expected.toLowerCase()) {
    try {
      fs.rmSync(file, { force: true });
    } catch {
      /* ignore */
    }
    throw new Error(`${label} SHA256 mismatch: expected ${expected}, got ${actual}`);
  }
}

// Embedding model coordinates (multilingual-e5-small, GGUF q8_0). Unlike the
// sidecar EXECUTABLES provisioned above, the embedding WEIGHTS are a downloadable
// model file — fetched on first use into userData/models by electron/embeddings.js
// (same channel as the whisper/VLM model downloads, not the binary provisioner).
// It is served by the EXISTING llama-server binary with `--embedding`, so no new
// executable is needed. Kept here as the single source of truth for the file name,
// download URL and size so the model registry in embeddings.js and any manifest
// tooling agree. b9500-compatible.
const EMBEDDING_MODEL = {
  id: 'e5-small',
  file: 'multilingual-e5-small-Q8_0.gguf',
  url: 'https://huggingface.co/keisuke-miyako/multilingual-e5-small-gguf-q8_0/resolve/main/multilingual-e5-small-Q8_0.gguf',
  sizeGB: 0.13,
  dim: 384,
};

function runtimeBinDir() {
  return path.join(app.getPath('userData'), 'runtime-bin');
}

function exeName(base) {
  return process.platform === 'win32' ? `${base}.exe` : base;
}

// The binaries a complete provisioning provides, with their in-runtime-bin
// relative paths (mirrors the old extraResources layout).
function expectedBinaries() {
  return {
    'yt-dlp': path.join('bin', exeName('yt-dlp')),
    ffmpeg: path.join('bin', exeName('ffmpeg')),
    'llama-server': path.join('llama', exeName('llama-server')),
    'whisper-server': path.join('whisper', exeName('whisper-server')),
  };
}

// ── Llama GPU variant (Windows-relevant; persisted in userData) ──────────────
// The variant decides WHICH llama.cpp build gets downloaded (cpu/cuda/vulkan/metal).
// Three inputs combine into the *effective* variant the rest of the app uses:
//   • explicit user choice (Settings dropdown) — highest priority
//   • otherwise the hardware-recommended variant (NVIDIA→cuda, AMD/Intel→vulkan…)
//   • minus any variant that has been marked failed at runtime (a GPU build that
//     won't start, e.g. stale driver / missing DLL) → demoted to cpu, so the next
//     provisioning fetches a build that actually runs.
// Persisted shape: { variant?, explicit?: bool, failed?: string[] }.

// On macOS the build is always Metal; on Windows/Linux fall back to whatever the
// hardware probe recommends (cpu when no GPU / detection fails).
function defaultVariant() {
  if (process.platform === 'darwin') return 'metal';
  try {
    const rec = hardware.detect().recommendedVariant;
    return VALID_VARIANTS.has(rec) ? rec : 'cpu';
  } catch {
    return 'cpu';
  }
}

function readVariantState() {
  try {
    const s =
      JSON.parse(fs.readFileSync(path.join(app.getPath('userData'), VARIANT_FILE), 'utf8')) || {};
    return {
      variant: VALID_VARIANTS.has(s.variant) ? s.variant : null,
      explicit: !!s.explicit,
      failed: Array.isArray(s.failed) ? s.failed.filter((v) => VALID_VARIANTS.has(v)) : [],
    };
  } catch {
    return { variant: null, explicit: false, failed: [] };
  }
}

function writeVariantState(patch) {
  const next = { ...readVariantState(), ...patch };
  try {
    fs.writeFileSync(path.join(app.getPath('userData'), VARIANT_FILE), JSON.stringify(next));
  } catch (err) {
    console.error('[binaries] cannot persist llama variant:', err);
  }
  return next;
}

// The variant actually used for provisioning + spawning (after applying failures).
function getLlamaVariant() {
  const st = readVariantState();
  const base = st.explicit && st.variant ? st.variant : defaultVariant();
  // A GPU build that failed to start is useless — fall back to CPU (which always
  // runs) until the user explicitly re-selects it (clearing the failed flag).
  if (st.failed.includes(base) && base !== 'cpu') return 'cpu';
  return base;
}

// Full state for the UI: what's chosen, what's effective, what failed.
function getVariantState() {
  const st = readVariantState();
  return { ...st, effective: getLlamaVariant(), recommended: defaultVariant() };
}

// An explicit user choice. Re-selecting a variant clears its failed flag (the user
// is forcing a fresh attempt, e.g. after a driver update).
function setLlamaVariant(variant) {
  const v = VALID_VARIANTS.has(variant) ? variant : defaultVariant();
  const st = readVariantState();
  writeVariantState({ variant: v, explicit: true, failed: st.failed.filter((x) => x !== v) });
  return v;
}

// Records that a variant's build failed to start. Idempotent. After this, the
// effective variant demotes to cpu so the next provisioning fetches a usable build.
function markVariantFailed(variant) {
  if (!VALID_VARIANTS.has(variant) || variant === 'cpu') return getVariantState();
  const st = readVariantState();
  if (!st.failed.includes(variant)) writeVariantState({ failed: [...st.failed, variant] });
  return getVariantState();
}

// Pack key for this platform (used by the macOS mini-pack path + status display).
function packKey() {
  const base = `${process.platform}-${process.arch}`;
  return process.platform === 'win32' ? `${base}-${getLlamaVariant()}` : base;
}

// ── Llama build marker ───────────────────────────────────────────────────────
// Records which llama.cpp build is installed in runtime-bin/llama. status() only
// checked that the llama-server FILE exists, so a build bump (LLAMA_BUILD) would
// NOT trigger re-provisioning on an existing install: an upgraded app kept running
// the old binary. That silently breaks any model needing a newer build — e.g.
// Gemma 4's `gemma4uv` vision projector fails on b9370 with "unknown projector
// type". The marker lets status() treat a present-but-outdated binary as missing.
function llamaBuildMarker() {
  return path.join(runtimeBinDir(), 'llama', '.build');
}
function installedLlamaBuild() {
  try {
    return fs.readFileSync(llamaBuildMarker(), 'utf8').trim();
  } catch {
    return null;
  }
}
function writeLlamaBuildMarker() {
  try {
    fs.writeFileSync(llamaBuildMarker(), LLAMA_BUILD);
  } catch (err) {
    console.warn('[binaries] could not write llama build marker:', err.message);
  }
}

// A llama runtime is only usable if its shared libs actually RESOLVE. The macOS
// pack stores them as relative symlinks (libfoo.0.dylib -> libfoo.0.0.9500.dylib)
// that llama-server loads via @loader_path. A pack built before the
// make-binary-packs.mjs verbatimSymlinks fix rewrote those links to ABSOLUTE paths
// on the build machine — so they dangle on every other Mac and llama-server dies at
// load with "Library not loaded: @rpath/libllama-common.0.dylib". status() only
// checked that the llama-server FILE exists, so such an install looked "ready" and
// never re-provisioned. Treat any dangling symlink in llama/ as a broken install so
// the (fixed) pack is re-fetched. No-op on Windows, whose llama dir has no symlinks.
function llamaLibsResolvable() {
  const dir = path.join(runtimeBinDir(), 'llama');
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return false;
  }
  // existsSync follows the link → false for a symlink whose target is missing.
  return entries.every((e) => !e.isSymbolicLink() || fs.existsSync(path.join(dir, e.name)));
}

// Heal a llama/ dir whose shared-lib symlinks dangle. A macOS pack built before the
// make-binary-packs.mjs verbatimSymlinks fix stored those links as ABSOLUTE paths on
// the build machine (…/llama/libfoo.0.0.9500.dylib): they resolve on the builder but
// dangle on every OTHER Mac, so llama-server dies at load, status() reports it missing
// (via llamaLibsResolvable) and — since re-fetching the same broken pack changes
// nothing — the runtime card stays stuck on "Mancanti: 1" forever. Repoint each
// dangling symlink at the BASENAME of its target, which is a real file sitting right
// beside it in the same dir, turning the absolute link into the relative one the pack
// should have shipped. Chained links (libfoo.dylib → libfoo.0.dylib → libfoo.0.0.x)
// heal regardless of iteration order because the new target is derived from the OLD
// link text, not from current resolvability. Idempotent; a no-op on a correct pack
// (relative links already resolve) and on Windows (no symlinks in llama/).
function repairLlamaSymlinks() {
  const dir = path.join(runtimeBinDir(), 'llama');
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const e of entries) {
    if (!e.isSymbolicLink()) continue;
    const link = path.join(dir, e.name);
    if (fs.existsSync(link)) continue; // already resolves → leave it untouched
    let target;
    try {
      target = fs.readlinkSync(link);
    } catch {
      continue;
    }
    const rel = path.basename(target);
    if (!rel || rel === e.name) continue; // nothing usable / would self-reference
    try {
      fs.unlinkSync(link);
      fs.symlinkSync(rel, link);
    } catch (err) {
      console.warn(`[binaries] could not repair llama symlink ${e.name}: ${err.message}`);
    }
  }
}

// ── Status ───────────────────────────────────────────────────────────────────
function status() {
  const root = runtimeBinDir();
  const expected = expectedBinaries();
  const present = {};
  let missing = 0;
  for (const [name, rel] of Object.entries(expected)) {
    let ok = fs.existsSync(path.join(root, rel));
    // A present-but-stale llama-server (its build marker doesn't match the build
    // this app expects) must count as missing so the provisioner re-fetches it.
    if (ok && name === 'llama-server' && installedLlamaBuild() !== LLAMA_BUILD) ok = false;
    // A present llama-server whose shared-lib symlinks dangle is unusable — re-fetch.
    if (ok && name === 'llama-server' && !llamaLibsResolvable()) ok = false;
    present[name] = ok;
    if (!ok) missing++;
  }
  return { ready: missing === 0, present, missing, dir: root, variant: getLlamaVariant() };
}

// ── Generic download/extract helpers ─────────────────────────────────────────
// Stream a download to disk; returns the base64 sha512.
async function downloadToFile(url, dest, onProgress) {
  const res = await fetch(url, { redirect: 'follow' });
  if (!res.ok || !res.body) throw new Error(`download HTTP ${res.status} for ${url}`);
  // Constrain the FINAL URL (after redirects), like serverUtils.downloadFile: a
  // binary download must end on an https host in our allowlist. A redirect chain
  // that lands on http:// or an unexpected host (poisoned redirect / compromised
  // mirror) is rejected before a single byte is written to disk.
  {
    let finalUrl;
    try {
      finalUrl = new URL(res.url || url);
    } catch {
      throw new Error(`invalid download URL: ${url}`);
    }
    if (finalUrl.protocol !== 'https:')
      throw new Error(`refusing non-https download (after redirect): ${finalUrl.href}`);
    if (!isAllowedDownloadHost(finalUrl.hostname))
      throw new Error(`refusing download from unexpected host: ${finalUrl.hostname}`);
  }
  const total = Number(res.headers.get('content-length')) || 0;
  const hash = createHash('sha512');
  // Write to a temp file and rename only on full success: an interrupted download
  // must not leave a truncated binary (e.g. yt-dlp.exe goes straight into bin/, and
  // a partial copy would be skipped as "already present" on the next run, then fail
  // at spawn time).
  const tmp = `${dest}.part`;
  const out = fs.createWriteStream(tmp);
  let received = 0;
  const reader = res.body.getReader();
  // Persistent 'error' listener for the stream's whole lifetime: the per-cycle
  // once('error') below only covers the backpressure await window, so a stream
  // error emitted BETWEEN cycles (e.g. an async flush failure / ENOSPC on a large
  // pack) would otherwise reach an EventEmitter with no 'error' listener and throw
  // as an uncaughtException. Capture it and surface it from the loop instead.
  let streamErr = null;
  out.on('error', (e) => {
    streamErr = e;
  });
  try {
    for (;;) {
      if (streamErr) throw streamErr;
      const { done, value } = await reader.read();
      if (done) break;
      const buf = Buffer.from(value);
      hash.update(buf);
      if (!out.write(buf))
        await new Promise((resolve, reject) => {
          // Already errored between cycles → don't wait on a dead stream (it will
          // never emit 'drain'/'error' again), reject immediately.
          if (streamErr) {
            reject(streamErr);
            return;
          }
          // Pair the listeners so the loser is removed when the race settles:
          // otherwise the 'error' once-listener survives every drain and they
          // pile up over a large download (MaxListenersExceededWarning + leak).
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
      if (onProgress) onProgress(total ? received / total : 0, received, total);
    }
    await new Promise((resolve, reject) => out.end((err) => (err ? reject(err) : resolve())));
  } catch (err) {
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
  }
  fs.renameSync(tmp, dest);
  return hash.digest('base64');
}

// downloadToFile with a few retries + linear backoff. Provisioning pulls large
// files straight from upstream (GitHub / gyan.dev) or the macOS mini-pack on the
// feed, where a single dropped connection or a transient 5xx would otherwise abort the
// whole provision with no recovery. Each attempt restarts from scratch
// (downloadToFile writes to a fresh .part), so a half-received file never leaks.
async function downloadWithRetry(url, dest, onProgress, attempts = 3) {
  let lastErr;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    try {
      return await downloadToFile(url, dest, onProgress);
    } catch (err) {
      lastErr = err;
      if (attempt < attempts) {
        console.warn(
          `[binaries] download failed (attempt ${attempt}/${attempts}) for ${url}: ${err?.message || err}`,
        );
        await new Promise((r) => setTimeout(r, 1000 * attempt));
      }
    }
  }
  throw lastErr;
}

// Recursively find the first file named `name` (case-insensitive); returns its
// absolute path or null.
function findFile(root, name) {
  const target = name.toLowerCase();
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop();
    let entries;
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const e of entries) {
      const p = path.join(dir, e.name);
      if (e.isDirectory()) stack.push(p);
      else if (e.name.toLowerCase() === target) return p;
    }
  }
  return null;
}

// Copy every entry of `srcDir` into `destDir` (flattening one level).
function copyDirContents(srcDir, destDir) {
  fs.mkdirSync(destDir, { recursive: true });
  for (const e of fs.readdirSync(srcDir, { withFileTypes: true })) {
    fs.cpSync(path.join(srcDir, e.name), path.join(destDir, e.name), {
      recursive: true,
      force: true,
    });
  }
}

function freshTmpDir(tag) {
  return fs.mkdtempSync(path.join(os.tmpdir(), `shelfy-${tag}-`));
}

// Collapse per-chunk download progress to at most one emit per whole percent
// (same fix as updater.perPercent): downloadToFile fires onProgress for EVERY
// chunk, and forwarding each one to the renderer means tens of thousands of
// webContents.send + setState for a large pack (~650 MB CUDA). Math.floor only
// reaches 100 at fraction=1, so phase changes and the final event of each phase
// always pass through.
function perPercent(emit) {
  let lastPhase = null;
  let lastPct = -1;
  return (phase, fraction) => {
    const pct = Math.floor((fraction || 0) * 100);
    if (phase === lastPhase && pct === lastPct) return;
    lastPhase = phase;
    lastPct = pct;
    emit(phase, fraction);
  };
}

// ── Windows provisioning (direct from upstream) ──────────────────────────────
async function ensureBinariesWindows(onProgress, force) {
  const root = runtimeBinDir();
  const binDir = path.join(root, 'bin');
  const llamaDir = path.join(root, 'llama');
  const whisperDir = path.join(root, 'whisper');
  for (const d of [binDir, llamaDir, whisperDir]) fs.mkdirSync(d, { recursive: true });

  const variant = getLlamaVariant();
  const llamaAsset = LLAMA_WIN_ASSET[variant] || LLAMA_WIN_ASSET.cpu;
  const has = (p) => !force && fs.existsSync(p);
  const ghLlama = (asset) =>
    `https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_BUILD}/${asset}`;

  // yt-dlp.exe — single self-contained binary. Pinned to YTDLP_VERSION (not
  // releases/latest) and verified against the published SHA256 before it's used.
  const ytDlpDest = path.join(binDir, 'yt-dlp.exe');
  if (!has(ytDlpDest)) {
    onProgress('yt-dlp', 0);
    await downloadWithRetry(
      `https://github.com/yt-dlp/yt-dlp/releases/download/${YTDLP_VERSION}/yt-dlp.exe`,
      ytDlpDest,
      (f) => onProgress('yt-dlp', f),
    );
    // Integrity gate: a mismatch means a tampered/corrupted download. Delete the
    // bad file so it can't be spawned and isn't mistaken for "already present" on
    // the next run, then fail loudly with the expected/actual hashes.
    const expected = YTDLP_SHA256['yt-dlp.exe'];
    const actual = await sha256File(ytDlpDest);
    if (actual.toLowerCase() !== expected.toLowerCase()) {
      try {
        fs.rmSync(ytDlpDest, { force: true });
      } catch {
        /* ignore */
      }
      throw new Error(
        `yt-dlp.exe SHA256 mismatch (yt-dlp ${YTDLP_VERSION}): expected ${expected}, got ${actual}`,
      );
    }
  }

  // ffmpeg.exe — extract from the gyan.dev essentials zip. Copy via a .part rename
  // so an interrupted copy can't leave a truncated ffmpeg.exe that has() would then
  // treat as "already present" and never re-fetch.
  // TODO(supply-chain): unlike yt-dlp this download is NOT pinned/verified. gyan.dev
  //   serves a rolling "ffmpeg-release-essentials.zip" (the URL has no version and
  //   the content changes per build), so there's no stable tag or matching checksum
  //   to pin against — a compromised mirror would be installed undetected. To harden:
  //   switch to a versioned source that publishes per-release checksums (e.g. a
  //   pinned BtbN/FFmpeg-Builds release with its SHA256 / .sha256 sidecar) and verify
  //   the extracted ffmpeg.exe with sha256File() the same way yt-dlp.exe is above.
  if (!has(path.join(binDir, 'ffmpeg.exe'))) {
    const tmp = freshTmpDir('ffmpeg');
    try {
      const zip = path.join(tmp, 'ffmpeg.zip');
      onProgress('ffmpeg', 0);
      await downloadWithRetry(
        'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip',
        zip,
        (f) => onProgress('ffmpeg', f),
      );
      await verifyPinnedSha256(zip, 'ffmpeg', 'ffmpeg-release-essentials.zip');
      await extractArchive(zip, tmp);
      const exe = findFile(tmp, 'ffmpeg.exe');
      if (!exe) throw new Error('ffmpeg.exe not found in archive');
      const dest = path.join(binDir, 'ffmpeg.exe');
      fs.copyFileSync(exe, `${dest}.part`);
      fs.renameSync(`${dest}.part`, dest);
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  }

  // llama-server.exe (+ DLLs, + CUDA runtime) — zip files sit at the archive root,
  // so we extract straight into llama/. We tag the dir with the installed variant:
  // switching variant (cuda↔vulkan↔cpu) MUST wipe the old files first, otherwise a
  // stale runtime (e.g. cudart*.dll from cuda) lingers next to the exe and Windows
  // may load the wrong DLLs (the server is spawned without a custom cwd/Path).
  const variantMarker = path.join(llamaDir, '.variant');
  const installedVariant = (() => {
    try {
      return fs.readFileSync(variantMarker, 'utf8').trim();
    } catch {
      return null;
    }
  })();
  // Re-fetch when the variant OR the pinned build changed (a build bump must reach
  // existing installs, otherwise an update keeps the old llama-server — see
  // llamaBuildMarker()).
  const llamaOk =
    !force &&
    installedVariant === variant &&
    installedLlamaBuild() === LLAMA_BUILD &&
    fs.existsSync(path.join(llamaDir, 'llama-server.exe'));
  if (!llamaOk) {
    fs.rmSync(llamaDir, { recursive: true, force: true });
    fs.mkdirSync(llamaDir, { recursive: true });
    const tmp = freshTmpDir('llama');
    try {
      const zip = path.join(tmp, 'llama.zip');
      const label = `llama-server (${variant})`;
      onProgress(label, 0);
      await downloadWithRetry(ghLlama(`llama-${LLAMA_BUILD}-bin-win-${llamaAsset}.zip`), zip, (f) =>
        onProgress(label, f),
      );
      await verifyPinnedSha256(
        zip,
        `llama-${llamaAsset}`,
        `llama-${LLAMA_BUILD}-bin-win-${llamaAsset}.zip`,
      );
      await extractArchive(zip, llamaDir);
      if (variant === 'cuda') {
        const cz = path.join(tmp, 'cudart.zip');
        onProgress('CUDA runtime', 0);
        await downloadWithRetry(ghLlama(`cudart-llama-bin-win-${llamaAsset}.zip`), cz, (f) =>
          onProgress('CUDA runtime', f),
        );
        await verifyPinnedSha256(
          cz,
          `cudart-${llamaAsset}`,
          `cudart-llama-bin-win-${llamaAsset}.zip`,
        );
        await extractArchive(cz, llamaDir);
      }
      if (!fs.existsSync(path.join(llamaDir, 'llama-server.exe')))
        throw new Error('llama-server.exe not found after extract');
      fs.writeFileSync(variantMarker, variant);
      writeLlamaBuildMarker();
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  }

  // whisper-server.exe (+ DLLs) — lives under Release/ in the zip. Stage the copy
  // into a sibling dir on the SAME volume, then swap it in with an atomic rename so
  // an interrupted copy can't leave whisper/ half-populated but with the .exe
  // already present (which has() would accept while a DLL is missing → spawn fails).
  if (!has(path.join(whisperDir, 'whisper-server.exe'))) {
    const tmp = freshTmpDir('whisper');
    const stage = path.join(root, 'whisper.new');
    try {
      const zip = path.join(tmp, 'whisper.zip');
      onProgress('whisper-server', 0);
      await downloadWithRetry(
        `https://github.com/ggml-org/whisper.cpp/releases/download/${WHISPER_TAG}/whisper-bin-x64.zip`,
        zip,
        (f) => onProgress('whisper-server', f),
      );
      await verifyPinnedSha256(zip, 'whisper', `whisper-bin-x64.zip (${WHISPER_TAG})`);
      const ex = path.join(tmp, 'x');
      await extractArchive(zip, ex);
      const srv = findFile(ex, 'whisper-server.exe');
      if (!srv) throw new Error('whisper-server.exe not found in archive');
      fs.rmSync(stage, { recursive: true, force: true });
      copyDirContents(path.dirname(srv), stage);
      fs.rmSync(whisperDir, { recursive: true, force: true });
      fs.renameSync(stage, whisperDir);
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
      fs.rmSync(stage, { recursive: true, force: true });
    }
  }

  const after = status();
  if (!after.ready) throw new Error(`provisioning incomplete: missing ${after.missing} binaries`);
  return { ok: true, installed: `win-${variant}` };
}

// ── macOS provisioning ───────────────────────────────────────────────────────
// Like Windows, yt-dlp and llama-server are downloaded DIRECTLY from their
// upstream releases (pinned + SHA-verified). Only whisper-server (no upstream
// macOS binary — built from source by CI) and ffmpeg (no stable per-file macOS
// URL — from ffmpeg-static) come from a minimal pack on the GitHub feed, listed
// in binaries.json.
async function fetchManifest(feed) {
  const res = await fetch(`${feed}/${MANIFEST_FILE}`, { cache: 'no-store' });
  if (!res.ok) throw new Error(`manifest HTTP ${res.status}`);
  return res.json();
}

// The mini-pack is channel-independent, but it lives on whichever release is
// published. On the beta channel the stable feed (releases/latest/download) can
// 404 while stable is still a draft, so try the channel's base first and fall back
// to the stable base. The channel comes from the same file the updater persists.
function packFeedCandidates() {
  const stable = readFeedUrl();
  if (!stable) return [];
  let channel = 'stable';
  try {
    const p = path.join(app.getPath('userData'), 'update-channel.json');
    if (JSON.parse(fs.readFileSync(p, 'utf8')).channel === 'beta') channel = 'beta';
  } catch {
    /* default stable */
  }
  if (channel === 'beta') {
    return [stable.replace(/\/releases\/latest\/download\/?$/, '/releases/download/beta'), stable];
  }
  return [stable];
}

async function fetchManifestWithFeed() {
  const candidates = packFeedCandidates();
  if (!candidates.length) throw new Error('no feed URL (app-update.yml missing)');
  let lastErr;
  for (const feed of candidates) {
    try {
      return { feed, manifest: await fetchManifest(feed) };
    } catch (err) {
      lastErr = err;
    }
  }
  throw lastErr;
}

async function ensureBinariesMac(onProgress, force) {
  if (process.arch !== 'arm64') {
    // SHELFY ships arm64-only on macOS (Metal). x64 here means Rosetta / Intel.
    throw new Error(
      "Le funzioni AI di SHELFY richiedono un Mac Apple Silicon (M1 o successivo). Se sei su Apple Silicon, apri l'app senza Rosetta.",
    );
  }
  const root = runtimeBinDir();
  const binDir = path.join(root, 'bin');
  const llamaDir = path.join(root, 'llama');
  for (const d of [binDir, llamaDir]) fs.mkdirSync(d, { recursive: true });
  const has = (p) => !force && fs.existsSync(p);

  // yt-dlp — single self-contained macOS binary, pinned to YTDLP_VERSION and
  // verified against the published SHA256 before it's used.
  const ytDlpDest = path.join(binDir, 'yt-dlp');
  if (!has(ytDlpDest)) {
    onProgress('yt-dlp', 0);
    await downloadWithRetry(
      `https://github.com/yt-dlp/yt-dlp/releases/download/${YTDLP_VERSION}/yt-dlp_macos`,
      ytDlpDest,
      (f) => onProgress('yt-dlp', f),
    );
    const expected = YTDLP_SHA256['yt-dlp_macos'];
    const actual = await sha256File(ytDlpDest);
    if (actual.toLowerCase() !== expected.toLowerCase()) {
      try {
        fs.rmSync(ytDlpDest, { force: true });
      } catch {
        /* ignore */
      }
      throw new Error(
        `yt-dlp_macos SHA256 mismatch (yt-dlp ${YTDLP_VERSION}): expected ${expected}, got ${actual}`,
      );
    }
    try {
      fs.chmodSync(ytDlpDest, 0o755);
    } catch {
      /* best effort */
    }
  }

  // llama-server (+ dylibs) — upstream macOS arm64 tar.gz, pinned SHA256. tar
  // preserves the relative dylib symlinks verbatim; --strip-components=1 drops the
  // leading `llama-<build>/` dir so files land straight in llama/. Re-fetch when the
  // pinned build changed or the libs no longer resolve.
  const llamaOk =
    !force &&
    installedLlamaBuild() === LLAMA_BUILD &&
    fs.existsSync(path.join(llamaDir, 'llama-server')) &&
    llamaLibsResolvable();
  if (!llamaOk) {
    fs.rmSync(llamaDir, { recursive: true, force: true });
    fs.mkdirSync(llamaDir, { recursive: true });
    const tmp = freshTmpDir('llama');
    try {
      const tar = path.join(tmp, 'llama.tar.gz');
      onProgress('llama-server', 0);
      await downloadWithRetry(
        `https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_BUILD}/llama-${LLAMA_BUILD}-bin-macos-arm64.tar.gz`,
        tar,
        (f) => onProgress('llama-server', f),
      );
      await verifyPinnedSha256(
        tar,
        'llama-macos-arm64',
        `llama-${LLAMA_BUILD}-bin-macos-arm64.tar.gz`,
      );
      await extractArchive(tar, llamaDir, { stripComponents: 1 });
      // belt-and-suspenders: a no-op when the relative symlinks already resolve.
      repairLlamaSymlinks();
      const server = path.join(llamaDir, 'llama-server');
      if (!fs.existsSync(server)) throw new Error('llama-server not found after extract');
      try {
        fs.chmodSync(server, 0o755);
      } catch {
        /* best effort */
      }
      writeLlamaBuildMarker();
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  }

  // whisper-server + ffmpeg — the only self-hosted bits: a minimal pack on the
  // GitHub feed. Listed in binaries.json, sha512-verified, extracted into
  // runtime-bin/ (adds bin/ffmpeg + whisper/, leaving llama/ and bin/yt-dlp intact).
  const needPack =
    force ||
    !fs.existsSync(path.join(binDir, 'ffmpeg')) ||
    !fs.existsSync(path.join(root, 'whisper', 'whisper-server'));
  if (needPack) {
    const { feed, manifest } = await fetchManifestWithFeed();
    const key = packKey();
    const pack = manifest.packs?.[key];
    if (!pack || !pack.url) throw new Error(`no binary pack for "${key}" in manifest`);
    const tmp = path.join(os.tmpdir(), `shelfy-bin-${process.pid}.tar.gz`);
    try {
      onProgress('download', 0);
      const sha = await downloadWithRetry(`${feed}/${pack.url}`, tmp, (f) =>
        onProgress('download', f),
      );
      if (!pack.sha512 || sha !== pack.sha512) throw new Error('sha512 mismatch on binary pack');
      onProgress('extract', 1);
      await extractArchive(tmp, root);
    } finally {
      try {
        fs.unlinkSync(tmp);
      } catch {
        /* ignore */
      }
    }
  }

  const after = status();
  if (!after.ready) throw new Error(`provisioning incomplete: missing ${after.missing} binaries`);
  return { ok: true, installed: packKey() };
}

// ── Linux provisioning ───────────────────────────────────────────────────────
// Mirrors macOS: yt-dlp and llama-server come DIRECTLY from upstream (pinned +
// SHA-verified Linux tarballs), per GPU variant (cpu / vulkan; NVIDIA falls back to
// vulkan since no CUDA build is published). whisper-server (built from source by CI)
// and ffmpeg come from the minimal pack on the GitHub feed (key "linux-x64").
async function ensureBinariesLinux(onProgress, force) {
  if (process.arch !== 'x64') {
    throw new Error(
      `Le funzioni AI di SHELFY su Linux richiedono x86_64 (architettura non supportata: ${process.arch}).`,
    );
  }
  const root = runtimeBinDir();
  const binDir = path.join(root, 'bin');
  const llamaDir = path.join(root, 'llama');
  for (const d of [binDir, llamaDir]) fs.mkdirSync(d, { recursive: true });
  const has = (p) => !force && fs.existsSync(p);

  // yt-dlp — single self-contained Linux binary, pinned + SHA-verified.
  const ytDlpDest = path.join(binDir, 'yt-dlp');
  if (!has(ytDlpDest)) {
    onProgress('yt-dlp', 0);
    await downloadWithRetry(
      `https://github.com/yt-dlp/yt-dlp/releases/download/${YTDLP_VERSION}/yt-dlp_linux`,
      ytDlpDest,
      (f) => onProgress('yt-dlp', f),
    );
    const expected = YTDLP_SHA256['yt-dlp_linux'];
    const actual = await sha256File(ytDlpDest);
    if (actual.toLowerCase() !== expected.toLowerCase()) {
      try {
        fs.rmSync(ytDlpDest, { force: true });
      } catch {
        /* ignore */
      }
      throw new Error(
        `yt-dlp_linux SHA256 mismatch (yt-dlp ${YTDLP_VERSION}): expected ${expected}, got ${actual}`,
      );
    }
    try {
      fs.chmodSync(ytDlpDest, 0o755);
    } catch {
      /* best effort */
    }
  }

  // llama-server (+ .so libs) — upstream Linux tar.gz, pinned SHA256, per GPU
  // variant. cuda/vulkan both map to the Vulkan build; everything else to cpu. Like
  // Windows, switching variant or bumping the build must wipe and re-fetch.
  const variant = getLlamaVariant();
  const linuxVariant = variant === 'vulkan' || variant === 'cuda' ? 'vulkan' : 'cpu';
  const llamaAsset = LLAMA_LINUX_ASSET[linuxVariant];
  const variantMarker = path.join(llamaDir, '.variant');
  const installedVariant = (() => {
    try {
      return fs.readFileSync(variantMarker, 'utf8').trim();
    } catch {
      return null;
    }
  })();
  const llamaOk =
    !force &&
    installedVariant === linuxVariant &&
    installedLlamaBuild() === LLAMA_BUILD &&
    fs.existsSync(path.join(llamaDir, 'llama-server')) &&
    llamaLibsResolvable();
  if (!llamaOk) {
    fs.rmSync(llamaDir, { recursive: true, force: true });
    fs.mkdirSync(llamaDir, { recursive: true });
    const tmp = freshTmpDir('llama');
    try {
      const tar = path.join(tmp, 'llama.tar.gz');
      const label = `llama-server (${linuxVariant})`;
      onProgress(label, 0);
      await downloadWithRetry(
        `https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_BUILD}/llama-${LLAMA_BUILD}-bin-${llamaAsset}.tar.gz`,
        tar,
        (f) => onProgress(label, f),
      );
      await verifyPinnedSha256(
        tar,
        `llama-${llamaAsset}`,
        `llama-${LLAMA_BUILD}-bin-${llamaAsset}.tar.gz`,
      );
      await extractArchive(tar, llamaDir, { stripComponents: 1 });
      repairLlamaSymlinks();
      const server = path.join(llamaDir, 'llama-server');
      if (!fs.existsSync(server)) throw new Error('llama-server not found after extract');
      try {
        fs.chmodSync(server, 0o755);
      } catch {
        /* best effort */
      }
      fs.writeFileSync(variantMarker, linuxVariant);
      writeLlamaBuildMarker();
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  }

  // whisper-server + ffmpeg — minimal pack on the GitHub feed (key linux-x64),
  // built by CI. sha512-verified, extracted into runtime-bin/.
  const needPack =
    force ||
    !fs.existsSync(path.join(binDir, 'ffmpeg')) ||
    !fs.existsSync(path.join(root, 'whisper', 'whisper-server'));
  if (needPack) {
    const { feed, manifest } = await fetchManifestWithFeed();
    const key = packKey();
    const pack = manifest.packs?.[key];
    if (!pack || !pack.url) throw new Error(`no binary pack for "${key}" in manifest`);
    const tmp = path.join(os.tmpdir(), `shelfy-bin-${process.pid}.tar.gz`);
    try {
      onProgress('download', 0);
      const sha = await downloadWithRetry(`${feed}/${pack.url}`, tmp, (f) =>
        onProgress('download', f),
      );
      if (!pack.sha512 || sha !== pack.sha512) throw new Error('sha512 mismatch on binary pack');
      onProgress('extract', 1);
      await extractArchive(tmp, root);
    } finally {
      try {
        fs.unlinkSync(tmp);
      } catch {
        /* ignore */
      }
    }
  }

  const after = status();
  if (!after.ready) throw new Error(`provisioning incomplete: missing ${after.missing} binaries`);
  return { ok: true, installed: packKey() };
}

// ── Provisioning entry point ─────────────────────────────────────────────────
// No-op in dev (binaries come from the source tree via the resolvers' dev
// fallback). onProgress(phaseLabel, fraction).
// Provisioning is strictly serialized through a single tail promise so two passes
// can NEVER touch the runtime-bin dir at once (ensureBinariesWindows does rm + mkdir
// + extract; interleaving corrupts it). Every pass `await`s the previous one INSIDE
// its body, and the tail bookkeeping is assigned synchronously (no await gap), so
// concurrent callers can't both slip past the guard.
//
// `_lastForce` / `_lastPromise` track the most-recently-queued pass for piggyback:
// a new call reuses it when that pass is at least as strong (a force:true caller —
// e.g. main.js's variant-fallback CPU re-provision — must NOT piggyback a force:false
// pass, or it would return early as `alreadyPresent` on the stale GPU file and the
// forced wipe-and-refetch would never run). This dedupes the common "two forced calls
// arrive while the boot provision is in flight" case into a single forced pass.
let _tail = Promise.resolve();
let _lastPromise = null;
let _lastForce = false;
async function ensureBinaries(onProgress = () => {}, { force = false } = {}) {
  if (!app.isPackaged) return { ok: true, skipped: 'dev' };
  // Piggyback on the last-queued pass when it's still pending and at least as strong.
  if (_lastPromise && (_lastForce || !force)) return _lastPromise;
  // Throttle here, not in the callers (ipc.js / main.js forward straight to the
  // renderer): every consumer of ensureBinaries gets per-percent progress.
  const progress = perPercent(onProgress);
  const prev = _tail;
  const promise = (async () => {
    // Wait for any queued/running pass to settle first (a weaker one's failure must
    // not abort our forced pass), THEN run, so provisions never overlap.
    try {
      await prev;
    } catch {
      /* ignore the previous pass's failure */
    }
    // Self-heal a previously-extracted-but-broken llama pack (absolute symlinks from an
    // old build machine) BEFORE deciding whether to re-provision, so a stuck "Mancanti:
    // 1" install recovers on the next launch without re-downloading the whole pack.
    if (process.platform !== 'win32') repairLlamaSymlinks();
    const st = status();
    if (st.ready && !force) return { ok: true, alreadyPresent: true };
    return process.platform === 'win32'
      ? ensureBinariesWindows(progress, force)
      : process.platform === 'darwin'
        ? ensureBinariesMac(progress, force)
        : ensureBinariesLinux(progress, force);
  })();
  // Assigned synchronously (before `prev` settles) so a concurrent caller sees this
  // pass as the tail and piggybacks/queues against it instead of starting its own.
  _tail = promise.catch(() => {});
  _lastPromise = promise;
  _lastForce = force;
  // Reset the piggyback state on settle WITHOUT spawning a fresh rejecting chain:
  // `promise.finally(...)` would return a new promise that re-rejects on failure and
  // is never handled, producing a spurious unhandledRejection on every failed provision.
  // Attach the cleanup to an already-swallowed branch instead.
  promise
    .then(
      () => {},
      () => {},
    )
    .finally(() => {
      if (_lastPromise === promise) {
        _lastPromise = null;
        _lastForce = false;
      }
    });
  return promise;
}

module.exports = {
  ensureBinaries,
  status,
  getLlamaVariant,
  setLlamaVariant,
  getVariantState,
  markVariantFailed,
  runtimeBinDir,
  packKey,
  EMBEDDING_MODEL,
};
