'use strict';

const { app, session } = require('electron');
const path = require('path');
const fs = require('fs');
const os = require('os');
const { spawn } = require('child_process');
const db = require('./db');
const jobstore = require('./jobstore');
const { microThumbDataUri } = require('./thumbs');
const { PARTITION, SOCIAL_UA } = require('./interceptor');
const { assertSafeMediaUrl } = require('./net-safety');

const KIND = 'download'; // jobstore namespace for this queue

// ─── Constants ────────────────────────────────────────────────────────────────

// Reuse the exact UA the webview logged in with (single source of truth in
// interceptor.js) so the cookies we export and the requests we replay carry a
// consistent browser identity — a UA mismatch is a bot signal / ban risk.
const UA = SOCIAL_UA;
const REFERERS = {
  instagram: 'https://www.instagram.com/',
  twitter: 'https://x.com/',
  pinterest: 'https://www.pinterest.com/',
};
const IMAGE_EXTS = ['jpg', 'jpeg', 'png', 'webp'];

// Global slot cap (anonymous image fetches, X, etc.). Instagram is throttled
// harder below: hammering the user's logged-in IG account with parallel
// downloads is the surest way to get it flagged or banned.
const CONCURRENCY = 3;

// Per-platform running-job cap; platforms not listed use CONCURRENCY. Instagram
// is serialized to 1 so video pulls happen strictly one-at-a-time on the
// authenticated session, mimicking human browsing instead of a burst.
const PLATFORM_CONCURRENCY = { instagram: 1 };

// Anti-ban pacing handed to yt-dlp (seconds). --sleep-requests spaces the
// metadata/extraction calls; --sleep-interval/--max-sleep-interval add a
// randomized pause before each media download. Randomization matters: a fixed
// cadence is itself a fingerprint. Only affects video (yt-dlp); anonymous image
// fetches are left untouched.
const YTDLP_SLEEP_REQUESTS = '1.5';
const YTDLP_SLEEP_MIN = '2';
const YTDLP_SLEEP_MAX = '6';
const YTDLP_PROGRESS_RE = /\[download\]\s+([\d.]+)%/;
const DOWNLOAD_TIMEOUT_MS = 60_000; // abort a stalled media fetch
const KILL_GRACE_MS = 5_000; // SIGTERM → SIGKILL fallback window

// Browser-shaped headers for direct image/thumbnail fetches. A bare UA+Referer
// request is an easy bot tell; a real Chromium image request also carries an
// Accept and the Sec-Fetch-* metadata below. Cookie-less by design — these hit
// public CDNs (scontent/pbs/pinimg), and sending the session jar cross-domain
// would be both unnecessary and a fingerprint of its own. (Video goes through
// yt-dlp, which sets its own headers.)
const IMAGE_FETCH_HEADERS = {
  Accept: 'image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8',
  'Sec-Fetch-Dest': 'image',
  'Sec-Fetch-Mode': 'no-cors',
  'Sec-Fetch-Site': 'cross-site',
};

// Human-like pacing for direct image/thumbnail fetches on authenticated,
// ban-prone platforms (yt-dlp already self-paces video, see YTDLP_SLEEP_*). A
// burst of carousel slides pulled back-to-back is a bot signal even when the
// requests are cookie-less, because they still share the user's IP + a social
// Referer. The pause is randomized [min,max] ms so a fixed cadence isn't itself
// a fingerprint. Platforms not listed (X, anonymous web) keep firing at the
// global concurrency with no added delay.
const IMAGE_PACING_MS = { instagram: [400, 1300], pinterest: [250, 900] };

// Pacing is real wall-clock time, which would push the test suite (50-tick
// drains) into false "still pending" failures — disable it under the runner.
const PACING_ENABLED = !process.env.VITEST && process.env.NODE_ENV !== 'test';

// SSRF guard for post-supplied media URLs lives in ./net-safety.js (shared
// single source of truth). `assertSafeMediaUrl` is imported
// at the top of this file; media URLs come from imported post JSON (untrusted),
// so we validate scheme + host before every fetch (see usage below).

function firstExisting(paths) {
  for (const p of paths) if (p && fs.existsSync(p)) return p;
  return null;
}

// Prefer a binary shipped in resources (so packaged builds don't depend on a
// system-wide install); fall back to a bare `yt-dlp` resolved via PATH.
let _ytDlpBin = null;
function ytDlpBin() {
  if (_ytDlpBin) return _ytDlpBin;
  const exe = process.platform === 'win32' ? 'yt-dlp.exe' : 'yt-dlp';
  _ytDlpBin =
    firstExisting([
      process.env.YTDLP_BIN,
      path.join(app.getPath('userData'), 'runtime-bin', 'bin', exe),
      path.join(process.resourcesPath || '', 'bin', exe),
      path.join(__dirname, '..', 'bin', exe),
    ]) || 'yt-dlp';
  return _ytDlpBin;
}

// Cache only a positive result: yt-dlp may be installed *after* the app starts,
// so a negative probe must stay re-checkable (caching false would make the
// feature permanently unavailable until restart). Negative probes are
// rate-limited so a missing binary doesn't cost one spawn per queued video,
// and the probe runs asynchronously so it never blocks the main process.
const YTDLP_PROBE_RETRY_MS = 30_000;
let _ytDlpAvailable = false;
let _ytDlpLastProbeAt = 0;
let _ytDlpProbe = null;

function probeYtDlp() {
  return new Promise((resolve) => {
    let child;
    try {
      child = spawn(ytDlpBin(), ['--version'], { stdio: 'ignore' });
    } catch {
      resolve(false);
      return;
    }
    child.on('error', () => resolve(false));
    child.on('close', (code) => resolve(code === 0));
  });
}

async function isYtDlpAvailable() {
  if (_ytDlpAvailable) return true;
  if (Date.now() - _ytDlpLastProbeAt < YTDLP_PROBE_RETRY_MS) return false;
  if (!_ytDlpProbe) {
    _ytDlpProbe = probeYtDlp().then((ok) => {
      _ytDlpProbe = null;
      _ytDlpLastProbeAt = Date.now();
      _ytDlpAvailable = ok;
      return ok;
    });
  }
  return _ytDlpProbe;
}

// ─── State ────────────────────────────────────────────────────────────────────
//
// jobKey = `${postId}:${assetType}`

const jobsMap = new Map(); // key → serializable job record (history kept)
const postCache = new Map(); // key → post object (needed for retry)
const abortMap = new Map(); // key → AbortController (active jobs only)
const activePromises = new Map(); // key → in-flight runJob() promise (active jobs only)
const pendingQueue = []; // ordered keys awaiting execution
const pendingSet = new Set(); // mirror of pendingQueue for O(1) membership (dedupe)
const pausedKeys = new Set(); // keys aborted by pause → re-queue instead of cancel

// Mutate pendingQueue and its membership mirror together so the dedupe check
// stays O(1). Splices in pumpQueue/cancelJob update both via these helpers.
function queuePush(key) {
  pendingQueue.push(key);
  pendingSet.add(key);
}
function queueUnshift(key) {
  pendingQueue.unshift(key);
  pendingSet.add(key);
}
function queueRemoveAt(i) {
  pendingSet.delete(pendingQueue[i]);
  pendingQueue.splice(i, 1);
}
function queueClear() {
  pendingQueue.length = 0;
  pendingSet.clear();
}

let isPaused = false;
let runningCount = 0;
const runningByPlatform = new Map(); // platform → count of in-flight jobs
let onJobUpdate = null; // (job) => void — set via setProgressEmitter

// ─── Helpers ──────────────────────────────────────────────────────────────────

function jobKey(postId, assetType, position) {
  return position == null ? `${postId}:${assetType}` : `${postId}:${assetType}:${position}`;
}

// The image media items of a post, each tagged with its slide position.
// Falls back to the cover thumbnail for legacy posts with no media array.
function imageMediaOf(post) {
  const media = Array.isArray(post.media) ? post.media : [];
  const images = media
    .map((m, i) => ({ ...m, position: i }))
    .filter((m) => m.type === 'image' && m.url);
  if (images.length === 0 && post.thumbnailUrl && post.mediaType !== 'video') {
    return [
      { type: 'image', url: post.thumbnailUrl, position: 0, localPath: post.imagePath || null },
    ];
  }
  return images;
}

// Trust the stored platform (set by every parser, incl. Pinterest). The legacy
// shortcode heuristic remains only as a fallback for very old records that
// predate the platform column.
function detectPlatform(post) {
  return post.platform || (post.shortcode ? 'instagram' : 'twitter');
}

// Identifiers and platform names originate from imported JSON / scrapers
// (untrusted) and end up in asset filenames via path.join — a crafted value
// containing `../` or path separators could escape the asset directory.
// Allow only [A-Za-z0-9_-], replace everything else with '_', cap the length,
// and fall back to 'unknown' so an empty/missing value never yields an empty
// filename segment.
function safeIdent(value) {
  const s = String(value ?? '')
    .replace(/[^A-Za-z0-9_-]/g, '_')
    .slice(0, 128);
  return s || 'unknown';
}

// Platforms our parsers actually emit; anything else (legacy/imported data)
// gets the same filename sanitization as post identifiers.
const KNOWN_PLATFORMS = new Set(['instagram', 'twitter', 'pinterest', 'web', 'manual']);
function safePlatform(platform) {
  return KNOWN_PLATFORMS.has(platform) ? platform : safeIdent(platform);
}

function getPostIdent(post) {
  return safeIdent(post.shortcode || post.id);
}

function extractExt(url) {
  if (!url) return 'jpg';
  const m = url.split('?')[0].match(/\.([a-zA-Z0-9]+)$/);
  const ext = m ? m[1].toLowerCase() : 'jpg';
  return IMAGE_EXTS.includes(ext) ? ext : 'jpg';
}

function getExistingImagePath(dir, prefix) {
  for (const ext of IMAGE_EXTS) {
    const p = path.join(dir, `${prefix}.${ext}`);
    if (fs.existsSync(p)) return p;
  }
  return null;
}

function twitterOrigUrl(url) {
  return url ? `${url.split('?')[0]}?name=orig` : url;
}

// Pinterest's CDN (i.pinimg.com) serves sized variants under a /<W>x/ or
// /<W>x<H>/ path segment; swapping it for /originals/ yields the full-resolution
// image. Not every pin keeps an original (it can 404), so execImage falls back
// to the served size on failure.
function pinterestOrigUrl(url) {
  return url ? url.replace(/\/\d+x(?:\d+)?\//, '/originals/') : url;
}

// Older records were saved with an empty author, producing an invalid
// `https://x.com//status/<id>` URL that yt-dlp rejects. yt-dlp accepts any
// non-empty username, so substitute the `i` placeholder.
function normalizeVideoUrl(url) {
  return url ? url.replace(/(:\/\/[^/]+)\/\/status\//, '$1/i/status/') : url;
}

function getAssetDir(sub) {
  return path.join(app.getPath('userData'), 'assets', sub);
}

function ensureDirs() {
  for (const sub of ['thumbnails', 'images', 'videos'])
    fs.mkdirSync(getAssetDir(sub), { recursive: true });
}

// Deletes every downloaded asset file from disk and leaves empty asset dirs
// behind. Cancels in-flight downloads first so nothing rewrites a file mid-wipe.
async function clearAllAssets() {
  await cancelAll();
  // cancelAll() only *requests* abort (ac.abort()); it doesn't wait for the
  // jobs to actually stop. A yt-dlp/ffmpeg child has a SIGKILL grace window and
  // settles only on 'close', and image jobs may still be mid renameSync. Await
  // the live runJob promises so no writer can touch the dir during/after the
  // wipe (a stale child re-creating videos/ files, or renameSync hitting ENOENT
  // against the just-deleted dir).
  await Promise.allSettled(Array.from(activePromises.values()));
  await fs.promises.rm(path.join(app.getPath('userData'), 'assets'), {
    recursive: true,
    force: true,
  });
  ensureDirs();
}

// ─── Cookies from the in-app webview session ────────────────────────────────────
//
// Neither Instagram nor X/Twitter serve full media to anonymous sessions:
// Instagram fails with "Unable to extract video url", and X hides video from
// the guest API for sensitive/age-gated tweets ("No video could be found in
// this tweet"). The app's Browser tab logs in via the `persist:social`
// session, so we export those cookies into a Netscape cookies.txt and hand it
// to yt-dlp. The jar holds every domain's cookies, but we scope each export to
// only the platform being downloaded (see writeSessionCookieFile) so yt-dlp
// never receives cookies for unrelated sites.

function toNetscapeLine(c) {
  const domain = c.domain || '';
  const includeSub = domain.startsWith('.') ? 'TRUE' : 'FALSE';
  const expiration = c.session || !c.expirationDate ? 0 : Math.floor(c.expirationDate);
  return [
    domain,
    includeSub,
    c.path || '/',
    c.secure ? 'TRUE' : 'FALSE',
    expiration,
    c.name,
    c.value,
  ].join('\t');
}

// Registrable domains whose cookies yt-dlp legitimately needs, per platform.
// Used to scope the exported cookies.txt to the target platform instead of
// dumping the whole session jar (which holds every site visited in the in-app
// Browser) — a smaller blast radius if the file ever leaks or yt-dlp is steered.
const PLATFORM_COOKIE_DOMAINS = {
  instagram: ['instagram.com', 'cdninstagram.com', 'facebook.com', 'fbcdn.net'],
  twitter: ['x.com', 'twitter.com', 'twimg.com'],
  pinterest: ['pinterest.com', 'pinimg.com'],
};

// True when a cookie's domain belongs to (or is a subdomain of) one of the
// platform's registrable domains. Cookie domains may carry a leading dot.
function cookieMatchesPlatform(cookie, platform) {
  const domains = PLATFORM_COOKIE_DOMAINS[platform];
  if (!domains) return false;
  const host = (cookie.domain || '').replace(/^\./, '').toLowerCase();
  return domains.some((d) => host === d || host.endsWith(`.${d}`));
}

// Dedicated dir for throwaway cookie jars. Lives inside userData (not the
// world-readable system temp dir) so the files inherit the app data dir's
// protections and are easy to sweep on boot.
function cookieTmpDir() {
  return path.join(app.getPath('userData'), 'tmp-cookies');
}

// Writes a throwaway cookies.txt for the current download and returns its path,
// or null if the session has no cookies for the platform. The jar is filtered to
// only the platform's domains so yt-dlp never receives cookies for unrelated
// sites. Caller is responsible for deleting it.
async function writeSessionCookieFile(platform) {
  const all = await session.fromPartition(PARTITION).cookies.get({});
  const cookies = platform ? all.filter((c) => cookieMatchesPlatform(c, platform)) : all;
  if (!cookies.length) return null;
  const body = ['# Netscape HTTP Cookie File', '', ...cookies.map(toNetscapeLine), ''].join('\n');
  const dir = cookieTmpDir();
  fs.mkdirSync(dir, { recursive: true });
  const file = path.join(
    dir,
    `shelfy-cookies-${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2)}.txt`,
  );
  fs.writeFileSync(file, body, { mode: 0o600 });
  return file;
}

// Removes orphan `shelfy-cookies-*` files left behind by a previous crash
// (normally each is unlinked in execVideo's finally, but a hard crash can leak
// them — they hold session cookies, so we don't want them lingering). Sweeps
// the current tmp-cookies dir plus the legacy system temp location older
// versions wrote to.
function cleanupOrphanCookieFiles() {
  const dirs = [];
  try {
    dirs.push(cookieTmpDir());
  } catch {}
  try {
    dirs.push(app.getPath('temp'));
  } catch {
    dirs.push(os.tmpdir());
  }
  for (const dir of dirs) {
    let names;
    try {
      names = fs.readdirSync(dir);
    } catch {
      continue;
    }
    for (const name of names) {
      if (/^shelfy-cookies-.*\.txt$/.test(name)) {
        try {
          fs.unlinkSync(path.join(dir, name));
        } catch {}
      }
    }
  }
}

// Run once at module load so leaked cookie jars don't accumulate across runs.
try {
  cleanupOrphanCookieFiles();
} catch {}

// ─── Job state helpers ────────────────────────────────────────────────────────

// Coalescer for renderer events. The contract stays one event per job: status
// transitions are emitted immediately, while progress-only updates (status
// unchanged) are buffered per key and flushed every PROGRESS_FLUSH_MS, so a
// burst of yt-dlp progress lines can't flood webContents.send.
const PROGRESS_FLUSH_MS = 100;
const dirtyProgress = new Map(); // key → latest job snapshot awaiting flush
let progressFlushTimer = null;

function flushProgress() {
  progressFlushTimer = null;
  if (onJobUpdate) {
    for (const job of dirtyProgress.values()) onJobUpdate(job);
  }
  dirtyProgress.clear();
}

function emitJob(job, prev) {
  if (!onJobUpdate) return;
  if (!prev || prev.status !== job.status) {
    dirtyProgress.delete(job.key);
    onJobUpdate({ ...job });
    return;
  }
  dirtyProgress.set(job.key, { ...job });
  if (!progressFlushTimer) {
    progressFlushTimer = setTimeout(flushProgress, PROGRESS_FLUSH_MS);
    progressFlushTimer.unref?.();
  }
}

// When set (bulk enqueue/recover), setJob collects mirrors here instead of
// persisting one-by-one; the collector is flushed in a single transaction.
let bulkMirror = null;

function withBulkMirror(fn) {
  if (bulkMirror) return fn(); // nested bulk → outer flush handles it
  bulkMirror = new Map();
  try {
    return fn();
  } finally {
    const jobs = [...bulkMirror.values()];
    bulkMirror = null;
    if (jobs.length > 0) jobstore.mirrorMany(KIND, jobs);
  }
}

function setJob(job) {
  const prev = jobsMap.get(job.key);
  jobsMap.set(job.key, { ...job });
  if (bulkMirror) bulkMirror.set(job.key, { ...job });
  else jobstore.mirror(KIND, job);
  emitJob(job, prev);
}

function patchJob(key, patch) {
  const j = jobsMap.get(key);
  if (!j) return;
  setJob({ ...j, ...patch });
}

// ─── fetch with AbortSignal ───────────────────────────────────────────────────

async function downloadUrl(url, destPath, referer, signal) {
  // Reject internal/loopback hosts and non-http(s) schemes before the request.
  assertSafeMediaUrl(url);

  // Compose the caller's abort signal with a timeout so a stalled connection
  // can't hang a download slot forever.
  const timeoutAc = new AbortController();
  const timer = setTimeout(() => timeoutAc.abort(), DOWNLOAD_TIMEOUT_MS);
  const signals = [timeoutAc.signal, ...(signal ? [signal] : [])];
  const composite =
    typeof AbortSignal.any === 'function' ? AbortSignal.any(signals) : signal || timeoutAc.signal; // fallback: prefer caller signal if no AbortSignal.any

  const tmp = `${destPath}.part`;
  try {
    const res = await fetch(url, {
      headers: {
        'User-Agent': UA,
        ...IMAGE_FETCH_HEADERS,
        ...(referer ? { Referer: referer } : {}),
      },
      redirect: 'follow',
      signal: composite,
    });
    if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);

    // Stream to a .part with backpressure, then rename atomically on success so
    // a truncated transfer is never mistaken for an "already downloaded" file.
    const out = fs.createWriteStream(tmp, { flags: 'w' });
    try {
      for await (const chunk of res.body) {
        if (!out.write(chunk)) {
          // Remove the paired listener explicitly after each await: once() only
          // auto-removes the listener for the event that actually fired, so on a
          // 'drain' (the common case) the matching 'error' listener would linger.
          // A multi-MB transfer hits backpressure many times, so the stale
          // listeners would pile up on this one stream (MaxListenersExceededWarning).
          await new Promise((resolve, reject) => {
            const onErr = (e) => {
              out.off('drain', onDrain);
              reject(e);
            };
            const onDrain = () => {
              out.off('error', onErr);
              resolve();
            };
            out.once('drain', onDrain);
            out.once('error', onErr);
          });
        }
      }
      await new Promise((resolve, reject) => out.end((err) => (err ? reject(err) : resolve())));
      fs.renameSync(tmp, destPath);
    } catch (err) {
      out.destroy();
      try {
        fs.unlinkSync(tmp);
      } catch {}
      throw err;
    }
  } finally {
    clearTimeout(timer);
  }
}

// ─── yt-dlp with AbortSignal ──────────────────────────────────────────────────

function runYtDlp(postUrl, outputPath, onProgress, signal, cookieFile) {
  return new Promise((resolve, reject) => {
    // A signal can already be aborted before we get here (e.g. the job was
    // cancelled during execVideo's cookie-file await): never spawn in that case.
    if (signal?.aborted) {
      reject(Object.assign(new Error('AbortError'), { name: 'AbortError' }));
      return;
    }
    // --newline forces each progress update onto its own \n-terminated line
    // (otherwise yt-dlp rewrites a single line with \r and the parser can't
    // split it). Progress is emitted on stdout; errors land on stderr.
    const args = [
      '--no-playlist',
      '--newline',
      // Present the same browser identity the cookies were minted under.
      '--user-agent',
      UA,
      // Human-like pacing to avoid tripping rate limits / bans (see constants).
      '--sleep-requests',
      YTDLP_SLEEP_REQUESTS,
      '--sleep-interval',
      YTDLP_SLEEP_MIN,
      '--max-sleep-interval',
      YTDLP_SLEEP_MAX,
    ];
    if (cookieFile) args.push('--cookies', cookieFile);
    // The `--` end-of-options marker guarantees yt-dlp can never interpret the
    // (post-supplied, untrusted) URL as an option — e.g. a postUrl beginning
    // with `-` such as `--exec=...` would otherwise be a command-injection vector.
    args.push('-o', outputPath, '--', postUrl);
    const child = spawn(ytDlpBin(), args, {
      stdio: ['pipe', 'pipe', 'pipe'],
    });

    // On abort, kill the child but DON'T settle yet: the promise rejects only
    // once the child has actually closed, so a quick pause/resume can't start a
    // second yt-dlp while the first still holds the same .part files.
    const onAbort = () => {
      child.kill('SIGTERM');
      // Escalate to SIGKILL if yt-dlp (and its ffmpeg child) ignore SIGTERM,
      // so an aborted job can't leave a zombie holding the file/port.
      const killTimer = setTimeout(() => {
        try {
          child.kill('SIGKILL');
        } catch {}
      }, KILL_GRACE_MS);
      killTimer.unref?.();
      child.once('close', () => clearTimeout(killTimer));
    };
    if (signal) signal.addEventListener('abort', onAbort, { once: true });

    const errLines = []; // keep last few non-progress lines for diagnostics

    // Per-stream line buffering: emit progress on any matching line, and keep
    // the rest as diagnostics so a failure can report a meaningful message.
    function makeLineHandler() {
      let buf = '';
      return (chunk) => {
        buf += chunk;
        const lines = buf.split(/\r?\n/);
        buf = lines.pop() ?? '';
        for (const line of lines) {
          const m = line.match(YTDLP_PROGRESS_RE);
          if (m) {
            onProgress?.(parseFloat(m[1]));
          } else if (line.trim()) {
            errLines.push(line.trim());
            if (errLines.length > 5) errLines.shift();
          }
        }
      };
    }

    child.stdout.setEncoding('utf8');
    child.stdout.on('data', makeLineHandler());
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', makeLineHandler());

    child.on('close', (code) => {
      if (signal) signal.removeEventListener('abort', onAbort);
      if (signal?.aborted) {
        reject(Object.assign(new Error('AbortError'), { name: 'AbortError' }));
      } else if (code === 0 && fs.existsSync(outputPath)) {
        resolve();
      } else {
        const errLine =
          errLines.filter((l) => /error/i.test(l)).pop() || errLines[errLines.length - 1];
        const detail = errLine
          ? `: ${errLine.replace(/^ERROR:\s*/i, '')}`
          : code === 0
            ? ': output file not produced'
            : '';
        reject(new Error(`yt-dlp exited ${code}${detail}`));
      }
    });
    child.on('error', (err) => {
      if (signal) signal.removeEventListener('abort', onAbort);
      reject(err);
    });
  });
}

// ─── Asset downloaders ────────────────────────────────────────────────────────

async function execThumbnail(post, signal) {
  const platform = detectPlatform(post);
  const id = getPostIdent(post);
  const dir = getAssetDir('thumbnails');
  const prefix = `${safePlatform(platform)}-${id}`;

  const existing = getExistingImagePath(dir, prefix);
  if (existing) return { thumbnailPath: existing };

  if (!post.thumbnailUrl) throw new Error('No thumbnailUrl');
  const ext = extractExt(post.thumbnailUrl);
  const dest = path.join(dir, `${prefix}.${ext}`);
  await downloadUrl(post.thumbnailUrl, dest, REFERERS[platform], signal);
  return { thumbnailPath: dest };
}

async function execImage(post, position, sourceUrl, signal) {
  const platform = detectPlatform(post);
  const id = getPostIdent(post);
  const dir = getAssetDir('images');
  const prefix = `${safePlatform(platform)}-${id}-${position}`;

  let existing = getExistingImagePath(dir, prefix);
  // Position 0 of single-image posts was historically saved without a suffix;
  // honor that file so we don't re-download already-archived images.
  if (!existing && position === 0)
    existing = getExistingImagePath(dir, `${safePlatform(platform)}-${id}`);
  if (existing) return { imagePath: existing, mediaPosition: position };

  const srcUrl = sourceUrl || post.thumbnailUrl;
  if (!srcUrl) throw new Error('No source URL');
  const ext = extractExt(srcUrl);
  const dest = path.join(dir, `${prefix}.${ext}`);
  if (platform === 'pinterest') {
    // Try the full-res /originals/ rewrite first; Pinterest doesn't always keep
    // an original (404), so fall back to the largest served size on failure.
    const orig = pinterestOrigUrl(srcUrl);
    try {
      await downloadUrl(orig, dest, REFERERS[platform], signal);
    } catch (err) {
      if (err?.name === 'AbortError' || orig === srcUrl) throw err;
      await downloadUrl(srcUrl, dest, REFERERS[platform], signal);
    }
    return { imagePath: dest, mediaPosition: position };
  }
  const url = platform === 'twitter' ? twitterOrigUrl(srcUrl) : srcUrl;
  await downloadUrl(url, dest, REFERERS[platform], signal);
  return { imagePath: dest, mediaPosition: position };
}

// Remove leftover format/fragment files for this video (`<base>.fNNN.mp4`,
// `.part`, `.part-FragN.part`, `.ytdl`), keeping only the merged `<base>.mp4`.
// A corrupt or truncated intermediate — e.g. an audio fragment from an
// interrupted run — is otherwise treated as a finished format on the next
// attempt, so yt-dlp skips the re-download and the merge dies with "Invalid
// data found when processing input". Purging them lets a retry start clean.
function cleanupVideoIntermediates(dest) {
  const dir = path.dirname(dest);
  const finalName = path.basename(dest);
  const prefix = finalName.replace(/\.mp4$/, '.');
  let names;
  try {
    names = fs.readdirSync(dir);
  } catch {
    return;
  }
  for (const name of names) {
    if (name.startsWith(prefix) && name !== finalName) {
      try {
        fs.unlinkSync(path.join(dir, name));
      } catch {}
    }
  }
}

async function execVideo(post, key, signal) {
  if (!(await isYtDlpAvailable())) throw new Error('yt-dlp not installed');
  if (!post.postUrl) throw new Error('No postUrl');

  // post.postUrl is untrusted (imported JSON / scrapers). Apply the same SSRF +
  // scheme guard the image path uses, and reject anything yt-dlp could read as
  // an option (leading '-') or any non-http(s) scheme, before handing it off.
  assertSafeMediaUrl(post.postUrl);

  const platform = detectPlatform(post);
  const id = getPostIdent(post);
  const dest = path.join(getAssetDir('videos'), `${safePlatform(platform)}-${id}.mp4`);

  if (fs.existsSync(dest)) return { videoPath: dest };

  const videoUrl = platform === 'twitter' ? normalizeVideoUrl(post.postUrl) : post.postUrl;
  const cookieFile = await writeSessionCookieFile(platform);
  // Collapse yt-dlp's per-line progress (--newline, dozens-hundreds of lines)
  // to whole-percent steps so a video doesn't patch the job hundreds of times.
  let lastPct = -1;
  try {
    await runYtDlp(
      videoUrl,
      dest,
      (pct) => {
        const rounded = Math.round(pct);
        if (rounded === lastPct) return;
        lastPct = rounded;
        patchJob(key, { progress: pct / 100 });
      },
      signal,
      cookieFile,
    );
  } catch (err) {
    // Pause/cancel leaves partials on purpose (resume reuses them); only purge
    // after a real failure so the next retry isn't poisoned by a bad fragment.
    if (err?.name !== 'AbortError') cleanupVideoIntermediates(dest);
    throw err;
  } finally {
    if (cookieFile) {
      try {
        fs.unlinkSync(cookieFile);
      } catch {}
    }
  }
  return { videoPath: dest };
}

// ─── Worker ───────────────────────────────────────────────────────────────────

// Write a finished download back to the DB. Image jobs record their per-slide
// path in post_media; the first slide also fills the post's cover image_path so
// existing "downloaded" badges and filters keep working. `thumbBlur` (the
// blur-up placeholder derived from the cover) rides along when the job
// produced a new cover; undefined leaves the stored one untouched (COALESCE).
function persistPaths(job, paths, thumbBlur = null) {
  if (job.assetType === 'image' && paths.imagePath != null) {
    const position = paths.mediaPosition ?? job.mediaPosition ?? 0;
    db.updateMediaPath(job.postId, position, paths.imagePath, 'image');
    if (position === 0) {
      db.updatePaths(job.postId, { imagePath: paths.imagePath, thumbBlur: thumbBlur ?? undefined });
    }
    return;
  }
  db.updatePaths(job.postId, { ...paths, thumbBlur: thumbBlur ?? undefined });
}

// Abort-aware pre-fetch pause for ban-prone image CDNs (see IMAGE_PACING_MS).
// Resolves immediately when pacing is off, the platform isn't throttled, or the
// job was already aborted; otherwise waits a randomized gap that a pause/cancel
// cuts short instead of blocking the slot for the full delay.
function paceImageFetch(platform, signal) {
  const range = IMAGE_PACING_MS[platform];
  if (!PACING_ENABLED || !range || signal?.aborted) return Promise.resolve();
  const [lo, hi] = range;
  const ms = lo + Math.floor(Math.random() * (hi - lo + 1));
  return new Promise((resolve) => {
    const t = setTimeout(resolve, ms);
    signal?.addEventListener(
      'abort',
      () => {
        clearTimeout(t);
        resolve();
      },
      { once: true },
    );
  });
}

async function runJob(key) {
  const job = jobsMap.get(key);
  const post = postCache.get(key);
  if (!job || !post) return;

  const ac = new AbortController();
  abortMap.set(key, ac);
  runningCount++;
  const platform = job.platform;
  runningByPlatform.set(platform, (runningByPlatform.get(platform) || 0) + 1);
  patchJob(key, { status: 'downloading', progress: 0, error: null });

  try {
    let paths;
    if (job.assetType === 'thumbnail') {
      await paceImageFetch(platform, ac.signal);
      paths = await execThumbnail(post, ac.signal);
    } else if (job.assetType === 'image') {
      await paceImageFetch(platform, ac.signal);
      paths = await execImage(post, job.mediaPosition ?? 0, job.mediaUrl, ac.signal);
    } else if (job.assetType === 'video') {
      paths = await execVideo(post, key, ac.signal);
    }

    if (paths) {
      // Blur-up placeholder: derived from the fresh cover and persisted WITH
      // the paths, BEFORE the 'done' patch is emitted — the renderer reacts to
      // 'done' by re-fetching this row and must already find thumb_blur there.
      // Kept out of the job record itself (a data URI has no business being
      // persisted in the jobstore or streamed to the Downloads view).
      let thumbBlur = null;
      const cover =
        job.assetType === 'thumbnail'
          ? paths.thumbnailPath
          : (paths.mediaPosition ?? job.mediaPosition ?? 0) === 0
            ? paths.imagePath
            : null;
      if (cover) {
        try {
          thumbBlur = await microThumbDataUri(cover);
        } catch {
          /* best-effort: a missing placeholder just means a plain dark tile */
        }
      }
      try {
        persistPaths(job, paths, thumbBlur);
      } catch (e) {
        console.warn('[downloader] DB update failed:', e?.message);
      }
    }

    patchJob(key, { status: 'done', progress: 1, ...(paths || {}) });
  } catch (err) {
    const isAbort = err?.name === 'AbortError' || err?.message === 'AbortError';
    if (isAbort) {
      if (pausedKeys.has(key)) {
        // Paused, not cancelled: return to the queue so resume restarts it.
        pausedKeys.delete(key);
        patchJob(key, { status: 'pending', progress: 0, error: null });
        if (!pendingSet.has(key)) queueUnshift(key);
      } else {
        patchJob(key, { status: 'cancelled', progress: 0 });
      }
    } else {
      const msg = err?.message || String(err);
      console.warn(`[downloader] ${job.assetType} ${job.postId}: ${msg}`);
      patchJob(key, { status: 'error', progress: 0, error: msg });
    }
  } finally {
    abortMap.delete(key);
    runningCount--;
    runningByPlatform.set(platform, Math.max(0, (runningByPlatform.get(platform) || 1) - 1));
    pumpQueue();
  }
}

function platformCap(platform) {
  return PLATFORM_CONCURRENCY[platform] ?? CONCURRENCY;
}

function pumpQueue() {
  if (isPaused) return;
  // Walk the queue in order and launch the first eligible jobs. A job is skipped
  // (left in place, not dropped) when its platform is already at its per-platform
  // cap, so e.g. a second Instagram job waits while a non-IG job behind it can
  // still start. Stops once the global slot cap is hit.
  for (let i = 0; i < pendingQueue.length && runningCount < CONCURRENCY; ) {
    const key = pendingQueue[i];
    const job = jobsMap.get(key);
    if (job?.status !== 'pending') {
      queueRemoveAt(i); // stale entry (cancelled/done) — drop it
      continue;
    }
    if ((runningByPlatform.get(job.platform) || 0) >= platformCap(job.platform)) {
      i++; // platform saturated — leave queued, try the next one
      continue;
    }
    queueRemoveAt(i);
    // Track the in-flight promise so clearAllAssets() can await actual job
    // termination (yt-dlp/ffmpeg children, write streams) before wiping the
    // assets dir — a plain abort only *requests* a stop, it doesn't await it.
    const p = runJob(key);
    activePromises.set(key, p);
    p.finally(() => {
      if (activePromises.get(key) === p) activePromises.delete(key);
    });
  }
}

// ─── Public: enqueue ──────────────────────────────────────────────────────────

const ASSET_PATH_FIELD = { thumbnail: 'thumbnailPath', image: 'imagePath', video: 'videoPath' };

// Image-bearing media types: 'image' (single), 'images' (multi-image tweet),
// 'carousel' (Instagram, may also include videos among the slides).
const IMAGE_MEDIA_TYPES = new Set(['image', 'images', 'carousel']);

function enqueueJob(post, assetType, { position = null, mediaUrl = null } = {}) {
  const key = jobKey(post.id, assetType, position);
  const existing = jobsMap.get(key);
  if (existing?.status === 'pending' || existing?.status === 'downloading') return;

  postCache.set(key, post);
  setJob({
    key,
    postId: post.id,
    platform: detectPlatform(post),
    assetType,
    mediaPosition: position,
    mediaUrl,
    status: 'pending',
    progress: 0,
    error: null,
    // Lightweight preview metadata so the Downloads list can show which post
    // is being downloaded, not just an opaque id.
    authorUsername: post.authorUsername ?? null,
    thumbnailUrl: post.thumbnailUrl ?? null,
    thumbnailPath: post.thumbnailPath ?? null,
    mediaType: post.mediaType ?? null,
  });
  if (!pendingSet.has(key)) queuePush(key);
}

// Returns the number of downloadable assets queued for this post. Callers (e.g.
// the download:post IPC) use a 0 return to detect "nothing to download" (a web
// reference or text-only post with no media) and give the user real feedback
// instead of a spinner that never resolves.
function enqueuePost(post, assetTypes, { missingOnly = false } = {}) {
  ensureDirs();

  // assetTypes crosses the renderer trust boundary (IPC payload). Whitelist it
  // to the known asset types so a bug/compromise sending a string (iterated as
  // chars by for..of) or any non-array can't spawn phantom jobs — those match
  // no runJob branch and get patched straight to 'done', faking completed
  // downloads in the UI and the jobstore.
  const types = (Array.isArray(assetTypes) ? assetTypes : []).filter(
    (t) => t === 'thumbnail' || t === 'image' || t === 'video',
  );

  let queued = 0;
  for (const assetType of types) {
    if (assetType === 'image') {
      // One job per image slide so carousels and multi-image tweets are fully
      // archived, not just their cover.
      if (!IMAGE_MEDIA_TYPES.has(post.mediaType)) continue;
      for (const img of imageMediaOf(post)) {
        if (missingOnly && img.localPath) continue;
        enqueueJob(post, 'image', { position: img.position, mediaUrl: img.url });
        queued++;
      }
      continue;
    }

    // Skip asset types that can't apply to this post's media type
    if (assetType === 'video' && post.mediaType !== 'video') continue;
    // Nothing to fetch for the thumbnail when the source URL is absent
    // (e.g. text-only tweets carry no thumbnail).
    if (assetType === 'thumbnail' && !post.thumbnailUrl) continue;
    // "Missing" is per asset type: re-download only the assets this post still
    // lacks, so partially-downloaded posts (e.g. thumbnail only) are completed.
    if (missingOnly && post[ASSET_PATH_FIELD[assetType]]) continue;

    enqueueJob(post, assetType);
    queued++;
  }

  pumpQueue();
  return queued;
}

// Returns the real counts so callers can report what actually happened:
// `queued` is the number of asset jobs created, `skipped` the number of posts
// that contributed none (nothing downloadable / already on disk).
function enqueueMany(posts, assetTypes, options = {}) {
  ensureDirs();
  let queued = 0;
  let skipped = 0;
  withBulkMirror(() => {
    for (const post of posts) {
      const n = enqueuePost(post, assetTypes, options);
      queued += n;
      if (n === 0) skipped++;
    }
  });
  return { queued, skipped };
}

// ─── Controls ─────────────────────────────────────────────────────────────────

function pauseAll() {
  isPaused = true;
  // Abort in-flight downloads so their spinners stop; runJob re-queues them as
  // pending (see the pausedKeys branch) ready to restart on resume.
  for (const [key, job] of jobsMap) {
    if (job.status === 'downloading') {
      pausedKeys.add(key);
      abortMap.get(key)?.abort();
    }
  }
}

function resumeAll() {
  isPaused = false;
  pumpQueue();
}

function cancelJob(key) {
  pausedKeys.delete(key);
  abortMap.get(key)?.abort();
  const qi = pendingQueue.indexOf(key);
  if (qi >= 0) queueRemoveAt(qi);
  const job = jobsMap.get(key);
  if (job && job.status !== 'done') {
    patchJob(key, { status: 'cancelled', progress: 0 });
  }
}

// Async so a bulk cancel doesn't block the main process: state flips happen in
// chunks (one mirror transaction each) with a setImmediate yield in between.
// Queues small enough to fit one chunk are settled synchronously, before the
// returned promise is awaited.
const CANCEL_CHUNK = 200;

async function cancelAll() {
  pausedKeys.clear();
  for (const ac of abortMap.values()) ac.abort();
  queueClear();
  isPaused = false;
  const keys = [];
  for (const [key, job] of jobsMap) {
    // Flip queued (pending), live (downloading) and already-failed (error) jobs
    // to 'cancelled' so the clearCompleted() pass that follows actually purges
    // them — otherwise 'error' jobs survive in jobsMap/jobstore and silently
    // reappear on the next refresh / app restart.
    if (job.status === 'pending' || job.status === 'downloading' || job.status === 'error')
      keys.push(key);
  }
  for (let i = 0; i < keys.length; i += CANCEL_CHUNK) {
    const batch = [];
    for (const key of keys.slice(i, i + CANCEL_CHUNK)) {
      const prev = jobsMap.get(key);
      if (!prev || prev.status === 'done' || prev.status === 'cancelled') continue;
      const next = { ...prev, status: 'cancelled', progress: 0 };
      jobsMap.set(key, next);
      batch.push(next);
      emitJob(next, prev);
    }
    jobstore.mirrorMany(KIND, batch);
    if (i + CANCEL_CHUNK < keys.length) await new Promise((resolve) => setImmediate(resolve));
  }
}

function retryJob(key) {
  const job = jobsMap.get(key);
  if (!job) return;
  if (job.status !== 'error' && job.status !== 'cancelled') return;
  patchJob(key, { status: 'pending', progress: 0, error: null });
  if (!pendingSet.has(key)) queuePush(key);
  pumpQueue();
}

function clearCompleted() {
  for (const [key, job] of jobsMap) {
    if (job.status === 'done' || job.status === 'cancelled') {
      jobsMap.delete(key);
      postCache.delete(key);
      jobstore.forget(KIND, key);
    }
  }
}

function getJobs() {
  return Array.from(jobsMap.values());
}

function getIsPaused() {
  return isPaused;
}

// ─── Boot recovery ────────────────────────────────────────────────────────────
//
// Re-enqueue downloads left unfinished by a previous run. Persisted job rows are
// grouped by post; we rehydrate the post from the DB and re-run enqueuePost with
// missingOnly so already-downloaded assets are skipped and only the gaps refill.
// Interrupted in-flight downloads restart from scratch (there's no byte resume).
function recover() {
  const rows = jobstore.resumable(KIND);
  if (rows.length === 0) return { recovered: 0 };
  // postId → { assetTypes, keys }: assetTypes drives the per-post re-enqueue,
  // keys lets us preserve the durable rows of posts that fail transiently.
  const byPost = new Map();
  for (const job of rows) {
    if (!job.postId || !job.assetType) continue;
    let entry = byPost.get(job.postId);
    if (!entry) byPost.set(job.postId, (entry = { assetTypes: new Set(), keys: [] }));
    entry.assetTypes.add(job.assetType);
    entry.keys.push(job.key);
  }
  let recovered = 0;
  const keep = new Set();
  withBulkMirror(() => {
    for (const [postId, { assetTypes, keys }] of byPost) {
      let post;
      try {
        post = db.getPost(postId);
      } catch {
        // Transient DB failure: keep this post's rows so the next boot retries
        // them instead of silently losing the downloads.
        for (const k of keys) keep.add(k);
        continue;
      }
      if (!post) continue; // post deleted → its stale rows are cleaned up below
      enqueuePost(post, Array.from(assetTypes), { missingOnly: true });
      recovered++;
    }
  });
  // Re-enqueue FIRST, forget AFTER: enqueuePost → setJob has already re-mirrored
  // every live key into the jobstore (flushed in one transaction by
  // withBulkMirror), so there is no window where a resumable job lacks a durable
  // row if the app dies mid-recovery. Everything not kept (post deleted, asset
  // already on disk via missingOnly, malformed row) is stale and gets dropped;
  // terminal rows are preserved by forgetExcept itself.
  for (const key of jobsMap.keys()) keep.add(key);
  jobstore.forgetExcept(KIND, keep);
  if (recovered > 0)
    console.log(`[downloader] recovered ${recovered} post(s) into the download queue`);
  return { recovered };
}

// ─── Legacy compat ────────────────────────────────────────────────────────────

function setProgressEmitter(fn) {
  onJobUpdate = fn;
}

function downloadPost(post, assetTypes, onProgress) {
  if (onProgress && !onJobUpdate) setProgressEmitter(onProgress);
  enqueuePost(post, assetTypes);
  return Promise.resolve({});
}

function downloadMany(posts, assetTypes, onProgress) {
  if (onProgress && !onJobUpdate) setProgressEmitter(onProgress);
  return Promise.resolve(enqueueMany(posts, assetTypes));
}

// ─── Exports ──────────────────────────────────────────────────────────────────

module.exports = {
  setProgressEmitter,
  clearAllAssets,
  enqueuePost,
  enqueueMany,
  downloadPost,
  downloadMany,
  pauseAll,
  resumeAll,
  cancelJob,
  cancelAll,
  retryJob,
  clearCompleted,
  recover,
  getJobs,
  getIsPaused,
  getStatus: getJobs,
  cancel: cancelJob,
};
