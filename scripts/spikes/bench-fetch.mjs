#!/usr/bin/env node
/**
 * X1 benchmark data: anonymous Instagram media fetch for a shortcode list.
 *
 * Adapts SPIKE-9's anonymous Instagram routes (`video-probe.mjs`'s `igPage`,
 * `igGraphql` and `igMediaFields`, see `docs/web-port/spikes/09-video-hydration.md`)
 * to return **every** carousel child of a post, not just one, and to download
 * the resolved files instead of only measuring them.
 *
 * For each job line it:
 *   1. resolves the post: the post page first (the logged-out media object
 *      `xig_polaris_media.if_not_gated_logged_out` in the inline JSON), then
 *      GraphQL (`PolarisLoggedOutDesktopWWWPostRootContentQuery`) if the page
 *      route fails or is gated;
 *   2. lists every child (the single item, or every `carousel_media` entry of
 *      a sidecar) with its position, type and best image/video URL
 *      (`image_versions2.candidates[0]`, `video_versions[0]`);
 *   3. downloads only the positions the job asked for (`--jobs`), to
 *      `<out>/<n>/slide-<position>.<jpg|mp4>`;
 *   4. if a video's direct URL fails to download, falls back to yt-dlp with
 *      the L17 format string (`-S "proto:https,vcodec:h264,res:1080" -f
 *      "bv*+ba/b"`), one playlist item (`position + 1`) of the post URL.
 *
 * No cookies are sent or kept, and redirects into login or challenge pages are
 * never followed. Requests are paced per host group (`--ig-gap-ms` to
 * www.instagram.com, `--gap-ms` to the CDN and to yt-dlp's own requests) with
 * jitter, same as `video-probe.mjs`. The whole run stops at the first 429,
 * rate-limit message, challenge or login-wall redirect; a post whose media is
 * merely gated (`if_not_gated_logged_out` is null) is skipped and the run
 * continues. There are no retries.
 *
 * Input and output both hold the owner's personal data (shortcodes, post
 * ids, downloaded media): keep `--jobs`, `--out` and `--manifest` outside the
 * repo, and delete them once the files are copied where they are needed.
 *
 * Usage:
 *   node scripts/spikes/bench-fetch.mjs --jobs jobs.tsv --out /work/out \
 *     --manifest /work/manifest.jsonl [--ig-gap-ms 3000] [--gap-ms 1000] \
 *     [--ytdlp yt-dlp] [--ffmpeg ffmpeg] [--limit N] [--skip N]
 *
 * jobs.tsv (tab-separated, '#'-prefixed lines ignored):
 *   n        two-digit (or any) sample id, becomes the output subdirectory
 *   shortcode  the Instagram shortcode (no URL needed)
 *   need     comma list of "position:type", e.g. "0:image,2:video"
 *
 * Example container invocation (the throwaway VPS pattern of SPIKE-9):
 *   docker run --rm --name shelfy-x1-fetch --cpus 0.5 --memory 512m \
 *     --cap-drop ALL --security-opt no-new-privileges \
 *     -v /tmp/shelfy-x1/node:/opt/node:ro -v /tmp/shelfy-x1/work:/work \
 *     --entrypoint /opt/node/bin/node ghcr.io/niccolofanton/shelfy-api:v0.1.0-rc.3 \
 *     /work/bench-fetch.mjs --jobs /work/jobs.tsv --out /work/out \
 *     --manifest /work/manifest.jsonl --ytdlp /usr/local/bin/yt-dlp --ffmpeg /usr/bin/ffmpeg
 */
import { spawn } from 'node:child_process';
import {
  createWriteStream,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
} from 'node:fs';
import http from 'node:http';
import https from 'node:https';
import { dirname, join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { parseArgs } from 'node:util';
import zlib from 'node:zlib';

const { values: args } = parseArgs({
  options: {
    jobs: { type: 'string' },
    out: { type: 'string' },
    manifest: { type: 'string' },
    'ig-gap-ms': { type: 'string', default: '3000' },
    'gap-ms': { type: 'string', default: '1000' },
    ytdlp: { type: 'string', default: 'yt-dlp' },
    ffmpeg: { type: 'string', default: 'ffmpeg' },
    'max-bytes': { type: 'string', default: String(300 * 1024 * 1024) },
    limit: { type: 'string' },
    skip: { type: 'string', default: '0' },
  },
});
if (!args.jobs || !args.out || !args.manifest) {
  console.error('usage: bench-fetch.mjs --jobs jobs.tsv --out /work/out --manifest manifest.jsonl');
  process.exit(2);
}

const IG_GAP_MS = Number(args['ig-gap-ms']);
const GAP_MS = Number(args['gap-ms']);
const MAX_BYTES = Number(args['max-bytes']);
const PAGE_MAX_BYTES = 8 * 1024 * 1024;
const REQUEST_TIMEOUT_MS = 30_000;
const DOWNLOAD_TIMEOUT_MS = 180_000;
const YTDLP_TIMEOUT_MS = 300_000;
const UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36';
const HTML_ACCEPT =
  'text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8';
const IG_APP_ID = '936619743392459';
const IG_DOC_ID = '27130156389949648';
const IG_FRIENDLY = 'PolarisLoggedOutDesktopWWWPostRootContentQuery';
// L17's format string, applied only when a direct URL download fails.
const YTDLP_FORMAT = 'bv*+ba/b';
const DOC_HEADERS = {
  'user-agent': UA,
  accept: HTML_ACCEPT,
  'accept-language': 'en-US,en;q=0.9',
  'accept-encoding': 'gzip, deflate, br',
  'upgrade-insecure-requests': '1',
  'sec-fetch-dest': 'document',
  'sec-fetch-mode': 'navigate',
  'sec-fetch-site': 'none',
  'sec-fetch-user': '?1',
};
const JSON_ACCEPT_HEADERS = {
  'user-agent': UA,
  accept: '*/*',
  'accept-language': 'en-US,en;q=0.9',
  'accept-encoding': 'gzip, deflate, br',
};
const MEDIA_HEADERS = {
  'user-agent': UA,
  accept: '*/*',
  'accept-language': 'en-US,en;q=0.9',
  referer: 'https://www.instagram.com/',
  'sec-fetch-dest': 'image',
  'sec-fetch-mode': 'no-cors',
  'sec-fetch-site': 'cross-site',
};

// ─── Pacing ──────────────────────────────────────────────────────────────────

function hostGroup(host) {
  const h = host.toLowerCase();
  if (h === 'www.instagram.com' || h === 'instagram.com') return ['ig-www', IG_GAP_MS];
  if (/(^|\.)(cdninstagram\.com|fbcdn\.net)$/.test(h)) return ['ig-cdn', GAP_MS];
  return [h, GAP_MS];
}
const lastStart = new Map();
async function paceGroup(group, gap) {
  const wait = (lastStart.get(group) ?? 0) + gap + Math.random() * 250 - Date.now();
  if (wait > 0) await sleep(wait);
  lastStart.set(group, Date.now());
}
const pace = (url) => paceGroup(...hostGroup(new URL(url).hostname));

// ─── HTTP (copied from video-probe.mjs: no redirects into a wall, gzip/br/deflate) ──

const AGENTS = {
  'https:': new https.Agent({ keepAlive: true, maxSockets: 4 }),
  'http:': new http.Agent({ keepAlive: true, maxSockets: 4 }),
};

function exchange(
  url,
  { method = 'GET', headers = {}, body = null, timeoutMs = REQUEST_TIMEOUT_MS } = {},
) {
  return new Promise((resolve) => {
    const lib = url.protocol === 'http:' ? http : https;
    const allHeaders = body ? { ...headers, 'content-length': Buffer.byteLength(body) } : headers;
    const req = lib.request(url, { method, headers: allHeaders, agent: AGENTS[url.protocol] });
    const timer = setTimeout(() => req.destroy(new Error('timeout')), timeoutMs);
    req.on('error', (err) => {
      clearTimeout(timer);
      resolve({ error: err.message === 'timeout' ? 'timeout' : err.code || err.message });
    });
    req.on('response', (res) => resolve({ res, req, timer }));
    if (body) req.write(body);
    req.end();
  });
}

async function readText(res, maxBytes) {
  const encoding = String(res.headers['content-encoding'] ?? '').toLowerCase();
  let stream = res;
  if (encoding === 'gzip' || encoding === 'x-gzip') stream = res.pipe(zlib.createGunzip());
  else if (encoding === 'br') stream = res.pipe(zlib.createBrotliDecompress());
  else if (encoding === 'deflate') stream = res.pipe(zlib.createInflate());
  const chunks = [];
  let bytes = 0;
  try {
    for await (const chunk of stream) {
      bytes += chunk.length;
      if (bytes > maxBytes) {
        res.destroy();
        return { text: Buffer.concat(chunks).toString('utf8'), bytes, truncated: true };
      }
      chunks.push(chunk);
    }
  } catch (err) {
    return { text: Buffer.concat(chunks).toString('utf8'), bytes, error: err.code || err.message };
  }
  return { text: Buffer.concat(chunks).toString('utf8'), bytes };
}

function pathClass(url, from) {
  const p = url.pathname.toLowerCase();
  if (/\/(accounts\/login|login|i\/flow\/login|signup)/.test(p)) return 'login';
  if (/\/(challenge|checkpoint|captcha)/.test(p)) return 'challenge';
  return url.hostname === from.hostname ? 'same-host' : 'other-host';
}

async function request(
  url,
  { method = 'GET', headers = {}, body = null, maxBytes = PAGE_MAX_BYTES } = {},
) {
  const t0 = performance.now();
  const redirects = [];
  let current = new URL(url);
  let ttfbMs = null;
  let requests = 0;
  const done = (extra) => ({
    ms: Math.round(performance.now() - t0),
    ttfbMs,
    redirects,
    requests,
    ...extra,
  });
  for (;;) {
    await pace(current.href);
    requests++;
    const ex = await exchange(current, { method, headers, body });
    if (ex.error) return done({ status: null, error: ex.error });
    const { res, timer } = ex;
    ttfbMs ??= Math.round(performance.now() - t0);
    const location = res.headers.location;
    if (res.statusCode >= 300 && res.statusCode < 400 && location && redirects.length < 5) {
      const next = new URL(location, current);
      const to = pathClass(next, current);
      redirects.push({ status: res.statusCode, to });
      res.resume();
      clearTimeout(timer);
      if (to === 'login' || to === 'challenge') {
        return done({ status: res.statusCode, headers: res.headers, text: '', bytes: 0 });
      }
      current = next;
      if (res.statusCode !== 307 && res.statusCode !== 308) {
        method = 'GET';
        body = null;
      }
      continue;
    }
    const read = await readText(res, maxBytes);
    clearTimeout(timer);
    return done({ status: res.statusCode, headers: res.headers, ...read });
  }
}

function blockOf(res) {
  if (!res) return null;
  if (res.status === 429) return 'rate_limited';
  if (res.redirects?.some((r) => r.to === 'challenge')) return 'challenge';
  if (res.redirects?.some((r) => r.to === 'login')) return 'login_wall';
  if (res.headers?.['cf-mitigated']) return 'challenge';
  return null;
}
function bodyBlockOf(res) {
  const text = res?.text ?? '';
  if (/please wait a few minutes before you try again|rate limit exceeded/i.test(text))
    return 'rate_limited';
  if (
    /<title>\s*just a moment\.\.\.|cf-chl-|challenge-platform\/h\/|are you a robot|unusual traffic from your/i.test(
      text,
    )
  )
    return 'challenge';
  if (/"require_login":\s*true|"pageID":"[^"]*login[^"]*"/i.test(text)) return 'login_wall';
  return null;
}

function parseJson(text) {
  try {
    return JSON.parse(String(text ?? '').replace(/^for \(;;\);/, ''));
  } catch {
    return null;
  }
}

/** Depth-first search for the first object accepted by `pred`. */
function findObject(root, pred, budget = { n: 300_000 }) {
  const stack = [root];
  while (stack.length && budget.n-- > 0) {
    const node = stack.pop();
    if (!node || typeof node !== 'object') continue;
    if (!Array.isArray(node) && pred(node)) return node;
    const values = Array.isArray(node) ? node : Object.values(node);
    for (let i = values.length - 1; i >= 0; i--) {
      if (values[i] && typeof values[i] === 'object') stack.push(values[i]);
    }
  }
  return null;
}

// ─── Instagram: resolve a post, adapted from video-probe.mjs ───────────────

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
function igPk(code) {
  const short = code.length > 28 ? code.slice(0, -28) : code;
  let pk = 0n;
  for (const ch of short) pk = pk * 64n + BigInt(B64.indexOf(ch));
  return pk.toString();
}

/** Every carousel child (or the single item), in order, with its best URLs. */
function igAllChildren(media) {
  const items =
    media.media_type === 8 && Array.isArray(media.carousel_media) ? media.carousel_media : [media];
  return items.map((item, position) => {
    const video = item.video_versions?.[0]?.url ?? null;
    const image = item.image_versions2?.candidates?.[0]?.url ?? item.display_uri ?? null;
    return { position, type: video ? 'video' : 'image', imageUrl: image, videoUrl: video };
  });
}

async function igPageMedia(shortcode) {
  const res = await request(`https://www.instagram.com/p/${shortcode}/`, { headers: DOC_HEADERS });
  const block = blockOf(res) ?? bodyBlockOf(res);
  if (res.status !== 200) return { ok: false, reason: `http_${res.status}`, block };
  for (const m of res.text.matchAll(
    /<script type="application\/json"[^>]*>([\s\S]*?)<\/script>/g,
  )) {
    if (!m[1].includes('xig_polaris_media')) continue;
    const holder = findObject(parseJson(m[1]), (o) => 'xig_polaris_media' in o);
    const xig = holder?.xig_polaris_media;
    if (!xig) continue;
    const media = xig.if_not_gated_logged_out;
    if (!media) return { ok: false, reason: 'gated', block: null };
    return { ok: true, route: 'ig-page', media };
  }
  return { ok: false, reason: 'parse_fail', block };
}

let igLsd = null;
async function igGraphqlMedia(shortcode) {
  if (!igLsd || Date.now() - igLsd.at > 10 * 60_000) {
    const homeRes = await request('https://www.instagram.com/', { headers: DOC_HEADERS });
    const token = homeRes.text?.match(/"LSD",\[\],\{"token":"([^"]+)"/)?.[1];
    if (!token) {
      return {
        ok: false,
        reason: homeRes.status === 200 ? 'no_lsd' : `http_${homeRes.status}`,
        block: blockOf(homeRes) ?? bodyBlockOf(homeRes),
      };
    }
    igLsd = { token, at: Date.now() };
  }
  const res = await request('https://www.instagram.com/api/graphql', {
    method: 'POST',
    headers: {
      ...JSON_ACCEPT_HEADERS,
      'content-type': 'application/x-www-form-urlencoded',
      'x-ig-app-id': IG_APP_ID,
      'x-asbd-id': '359341',
      'x-ig-www-claim': '0',
      'x-fb-friendly-name': IG_FRIENDLY,
      'x-fb-lsd': igLsd.token,
      'x-requested-with': 'XMLHttpRequest',
      origin: 'https://www.instagram.com',
      referer: `https://www.instagram.com/p/${shortcode}/`,
      'sec-fetch-dest': 'empty',
      'sec-fetch-mode': 'cors',
      'sec-fetch-site': 'same-origin',
    },
    body: new URLSearchParams({
      lsd: igLsd.token,
      fb_api_caller_class: 'RelayModern',
      fb_api_req_friendly_name: IG_FRIENDLY,
      server_timestamps: 'true',
      variables: JSON.stringify({ media_id: igPk(shortcode) }),
      doc_id: IG_DOC_ID,
    }).toString(),
  });
  const block = blockOf(res) ?? bodyBlockOf(res);
  if (res.status !== 200) return { ok: false, reason: `http_${res.status}`, block };
  const json = parseJson(res.text);
  if (!json) return { ok: false, reason: 'parse_fail', block };
  if (json.errors?.length && !json.data) {
    igLsd = null;
    return { ok: false, reason: 'graphql_error', block: null };
  }
  const xig = json.data?.xig_polaris_media;
  if (!xig) return { ok: false, reason: 'unavailable', block: null };
  const media = xig.if_not_gated_logged_out;
  if (!media) return { ok: false, reason: 'gated', block: null };
  return { ok: true, route: 'ig-graphql', media };
}

async function resolvePost(shortcode) {
  const page = await igPageMedia(shortcode);
  if (page.ok || page.block) return page;
  const gql = await igGraphqlMedia(shortcode);
  return gql.ok || gql.block ? gql : page;
}

// ─── Downloads ───────────────────────────────────────────────────────────────

function run(cmd, cmdArgs, timeoutMs) {
  return new Promise((resolve) => {
    const child = spawn(cmd, cmdArgs, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    const timer = setTimeout(() => child.kill('SIGKILL'), timeoutMs);
    child.stdout.on('data', (d) => (stdout += d));
    child.stderr.on('data', (d) => (stdout += d));
    child.on('error', () => {
      clearTimeout(timer);
      resolve({ code: -1, stdout });
    });
    child.on('close', (code) => {
      clearTimeout(timer);
      resolve({ code, stdout });
    });
  });
}

/** Full GET of a file (image or video) with redirects, a byte cap and a timeout. */
async function downloadFile(url, dest) {
  const t0 = performance.now();
  const result = {};
  let current = new URL(url);
  mkdirSync(dirname(dest), { recursive: true });
  const tmp = `${dest}.part`;
  try {
    for (let hop = 0; ; hop++) {
      await pace(current.href);
      const ex = await exchange(current, {
        headers: MEDIA_HEADERS,
        timeoutMs: DOWNLOAD_TIMEOUT_MS,
      });
      if (ex.error) {
        result.error = ex.error;
        return result;
      }
      const { res, timer } = ex;
      result.status = res.statusCode;
      result.contentType = res.headers['content-type'] ?? null;
      result.block = res.statusCode === 429 ? 'rate_limited' : null;
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location && hop < 5) {
        res.resume();
        clearTimeout(timer);
        current = new URL(res.headers.location, current);
        continue;
      }
      if (res.statusCode < 200 || res.statusCode >= 300) {
        res.resume();
        clearTimeout(timer);
        result.error = `http_${res.statusCode}`;
        return result;
      }
      const out = createWriteStream(tmp);
      let bytes = 0;
      try {
        for await (const chunk of res) {
          bytes += chunk.length;
          if (bytes > MAX_BYTES) {
            result.error = 'max_bytes';
            res.destroy();
            break;
          }
          if (!out.write(chunk)) await new Promise((r) => out.once('drain', r));
        }
      } catch (err) {
        result.error = err.message === 'timeout' ? 'timeout' : err.code || err.message;
      }
      clearTimeout(timer);
      await new Promise((r) => out.end(r));
      result.bytes = bytes;
      break;
    }
    if (!result.error) {
      renameSync(tmp, dest);
      result.ok = true;
    }
  } finally {
    rmSync(tmp, { force: true });
    result.ms = Math.round(performance.now() - t0);
  }
  return result;
}

/** Fallback: yt-dlp on the whole post, targeting one playlist item (1-based). */
async function ytdlpSlide(shortcode, position, dest) {
  const [group, gap] = hostGroup('www.instagram.com');
  await paceGroup(group, gap);
  const base = `ytdlp-${shortcode}-${position}`;
  const workDir = dirname(dest);
  const ytArgs = [
    '--ignore-config',
    '--no-cookies',
    '--no-cookies-from-browser',
    '--no-cache-dir',
    '--no-plugin-dirs',
    '--use-extractors',
    'Instagram',
    '--playlist-items',
    String(position + 1),
    '--newline',
    '--no-progress',
    '--no-mtime',
    '--socket-timeout',
    '30',
    '--sleep-requests',
    '1',
    '--max-filesize',
    String(MAX_BYTES),
    '-S',
    'proto:https,vcodec:h264,res:1080',
    '-f',
    YTDLP_FORMAT,
    '--merge-output-format',
    'mp4',
    '--ffmpeg-location',
    args.ffmpeg,
    '-o',
    join(workDir, `${base}.%(ext)s`),
    '--',
    `https://www.instagram.com/p/${shortcode}/`,
  ];
  const r = await run(args.ytdlp, ytArgs, YTDLP_TIMEOUT_MS);
  const files = existsSync(workDir)
    ? readdirSync(workDir).filter((f) => f.startsWith(`${base}.`))
    : [];
  const media = files.find((f) => {
    const suffix = f.slice(base.length + 1);
    return /^[a-z0-9]+$/i.test(suffix) && suffix !== 'part';
  });
  let result;
  if (media && r.code === 0) {
    renameSync(join(workDir, media), dest);
    result = { ok: true, bytes: statSync(dest).size, mode: 'ytdlp' };
  } else {
    const errorLine = r.stdout
      .split(/\r?\n/)
      .filter((l) => /^ERROR:/.test(l))
      .pop();
    result = {
      ok: false,
      error: errorLine
        ? errorLine.replace(/https?:\/\/\S+/g, '<url>').slice(0, 160)
        : `exit_${r.code}`,
      mode: 'ytdlp',
    };
  }
  for (const f of files) rmSync(join(workDir, f), { force: true });
  return result;
}

// ─── Main ────────────────────────────────────────────────────────────────────

const jobs = readFileSync(args.jobs, 'utf8')
  .split('\n')
  .filter((line) => line && !line.startsWith('#'))
  .map((line) => {
    const [n, shortcode, need] = line.split('\t');
    const positions = new Map(
      (need ?? '')
        .split(',')
        .filter(Boolean)
        .map((entry) => {
          const [pos, type] = entry.split(':');
          return [Number(pos), type];
        }),
    );
    return { n, shortcode, positions };
  });

mkdirSync(args.out, { recursive: true });
mkdirSync(dirname(args.manifest), { recursive: true });
const manifestOut = createWriteStream(args.manifest, { flags: 'a', mode: 0o600 });
const writeManifest = (obj) => manifestOut.write(JSON.stringify(obj) + '\n');

const skip = Number(args.skip);
const selected = jobs.slice(skip, args.limit ? skip + Number(args.limit) : undefined);
console.error(`bench-fetch: ${selected.length} post(s) to resolve`);

let stopped = null;
for (const job of selected) {
  if (stopped) break;
  const resolved = await resolvePost(job.shortcode);
  if (resolved.block) {
    stopped = { reason: resolved.block, n: job.n, stage: 'resolve' };
    writeManifest({
      n: job.n,
      ok: false,
      reason: resolved.reason ?? resolved.block,
      block: resolved.block,
    });
    break;
  }
  if (!resolved.ok) {
    console.error(`[${job.n}] resolve failed: ${resolved.reason}`);
    writeManifest({ n: job.n, ok: false, reason: resolved.reason });
    continue;
  }
  const children = igAllChildren(resolved.media);
  const results = [];
  for (const [position, wantType] of job.positions) {
    const child = children[position];
    if (!child) {
      results.push({ position, ok: false, error: 'no_such_position' });
      continue;
    }
    if (wantType && wantType !== child.type) {
      console.error(`[${job.n}] position ${position}: expected ${wantType}, IG has ${child.type}`);
    }
    const destDir = join(args.out, job.n);
    if (child.type === 'video') {
      if (!child.videoUrl) {
        results.push({ position, ok: false, error: 'no_video_url' });
        continue;
      }
      const dest = join(destDir, `slide-${position}.mp4`);
      let dl = await downloadFile(child.videoUrl, dest);
      if (dl.block) {
        stopped = { reason: dl.block, n: job.n, stage: 'download' };
        results.push({ position, type: 'video', ok: false, error: dl.block });
        break;
      }
      if (!dl.ok) {
        console.error(
          `[${job.n}] slide ${position} direct video failed (${dl.error}); trying yt-dlp`,
        );
        dl = await ytdlpSlide(job.shortcode, position, dest);
      }
      results.push({
        position,
        type: 'video',
        ok: Boolean(dl.ok),
        bytes: dl.bytes ?? null,
        mode: dl.mode ?? 'direct',
        error: dl.ok ? null : dl.error,
      });
    } else {
      if (!child.imageUrl) {
        results.push({ position, ok: false, error: 'no_image_url' });
        continue;
      }
      const dest = join(destDir, `slide-${position}.jpg`);
      const dl = await downloadFile(child.imageUrl, dest);
      if (dl.block) {
        stopped = { reason: dl.block, n: job.n, stage: 'download' };
        results.push({ position, type: 'image', ok: false, error: dl.block });
        break;
      }
      results.push({
        position,
        type: 'image',
        ok: Boolean(dl.ok),
        bytes: dl.bytes ?? null,
        error: dl.ok ? null : dl.error,
      });
    }
  }
  writeManifest({
    n: job.n,
    ok: true,
    route: resolved.route,
    mediaCount: children.length,
    results,
  });
  console.error(`[${job.n}] ${results.filter((r) => r.ok).length}/${results.length} fetched`);
  if (stopped) break;
}

if (stopped) {
  writeManifest({ type: 'stop', ...stopped });
  console.error(`STOPPED: ${stopped.reason} at post ${stopped.n} (${stopped.stage})`);
  process.exitCode = 1;
} else {
  console.error('bench-fetch: done');
}
await new Promise((resolve) => manifestOut.end(resolve));
