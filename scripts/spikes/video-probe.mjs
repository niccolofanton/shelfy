#!/usr/bin/env node
/**
 * SPIKE-9 probe: on-demand video and link hydration without user cookies.
 *
 * For every post of the sample (`video-sample.mjs`) it runs each route of the
 * post's platform in turn and records what came back:
 *
 * | Route | What it asks |
 * |---|---|
 * | ig-embed | `/p/<code>/embed/captioned/` loaded as a cross-site iframe (legacy template with `contextJSON`) |
 * | ig-page | the post page; the logged-out media object in the inline JSON (`video_versions`) |
 * | ig-graphql | `POST /api/graphql` `PolarisLoggedOutDesktopWWWPostRootContentQuery` with the LSD token of the home page (the recipe of yt-dlp 2026.08.19) |
 * | ig-ytdlp, x-ytdlp, pin-ytdlp | yt-dlp with the flags of plan §2.13 |
 * | x-syndication | `cdn.syndication.twimg.com/tweet-result` (the endpoint of X's embed widget) |
 * | x-oembed | `publish.x.com/oembed` (hydration only) |
 * | pin-pidgets | `widgets.pinterest.com/v3/pidgets/pins/info/` |
 * | pin-resource | `www.pinterest.com/resource/PinResource/get/` (yt-dlp's endpoint) |
 * | pin-page | the pin page; the Relay responses inlined in the HTML |
 * | pin-oembed | `www.pinterest.com/oembed.json` (hydration only) |
 *
 * Each route reports the hydration fields it found (caption, author, date,
 * image, video), whether caption and author match the library (hashed keys
 * from the sample), the video rendition it would fetch and, for Instagram, the
 * hours left before the `oe` expiry of the video and poster URLs. Unless
 * --resolve-only is set, the video is downloaded to --work, checked with
 * ffprobe (codec, size, duration, `moov` before `mdat`) and deleted at once.
 * When two routes of one post resolve to the same CDN file, the second one
 * only checks its own URL with a 1 KiB range request.
 *
 * No cookies are sent or kept, and redirects into login or challenge pages
 * are not followed. Requests to one host group start at least --gap-ms apart
 * (--ig-gap-ms for www.instagram.com), and yt-dlp runs with
 * `--sleep-requests 1`. A route stops after two posts in a row behind a login
 * wall; a platform stops at the first 429, rate-limit message or challenge.
 *
 * The output holds one JSON line per post and route, keyed by the sample id,
 * plus `stop` lines. It contains no URLs, captions or usernames: only
 * statuses, timings, sizes, booleans, codec data and 12-hex hashes.
 *
 * Runs in the `shelfy-api` image with a mounted Node binary (`--ytdlp`,
 * `--ffmpeg`, `--ffprobe` point at the image's tools) or on a laptop.
 *
 * Usage:
 *   node scripts/spikes/video-probe.mjs --in sample.tsv --out results.jsonl
 *     [--label vps] [--platforms instagram,x,pinterest] [--routes ig-embed,...]
 *     [--work /tmp/shelfy-spike9-dl] [--resolve-only] [--limit N] [--skip N]
 *     [--gap-ms 1000] [--ig-gap-ms 3000] [--ytdlp yt-dlp] [--ffmpeg ffmpeg]
 *     [--ffprobe ffprobe] [--max-bytes 314572800]
 */
import { spawn } from 'node:child_process';
import {
  closeSync,
  createWriteStream,
  existsSync,
  fstatSync,
  mkdirSync,
  openSync,
  readdirSync,
  readFileSync,
  readSync,
  rmSync,
  statSync,
} from 'node:fs';
import http from 'node:http';
import https from 'node:https';
import { join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { parseArgs } from 'node:util';
import zlib from 'node:zlib';
import { authorKey, captionKey, decodeEntities, oeSecondsLeft } from './video-common.mjs';

const { values: args } = parseArgs({
  options: {
    in: { type: 'string' },
    out: { type: 'string' },
    label: { type: 'string', default: 'run' },
    platforms: { type: 'string', default: 'instagram,x,pinterest' },
    routes: { type: 'string' },
    work: { type: 'string', default: '/tmp/shelfy-spike9-dl' },
    'resolve-only': { type: 'boolean', default: false },
    limit: { type: 'string' },
    skip: { type: 'string', default: '0' },
    'gap-ms': { type: 'string', default: '1000' },
    'ig-gap-ms': { type: 'string', default: '3000' },
    ytdlp: { type: 'string', default: 'yt-dlp' },
    ffmpeg: { type: 'string', default: 'ffmpeg' },
    ffprobe: { type: 'string', default: 'ffprobe' },
    'max-bytes': { type: 'string', default: String(300 * 1024 * 1024) },
  },
});
if (!args.in || !args.out) {
  console.error('usage: video-probe.mjs --in sample.tsv --out results.jsonl [--label vps]');
  process.exit(2);
}

const GAP_MS = Number(args['gap-ms']);
const IG_GAP_MS = Number(args['ig-gap-ms']);
const MAX_BYTES = Number(args['max-bytes']);
const RESOLVE_ONLY = args['resolve-only'];
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
// The format string of plan §2.13.
const YTDLP_FORMAT = 'bv*[ext=mp4]+ba[ext=m4a]/b[ext=mp4]/b';
const YTDLP_EXTRACTOR = { instagram: 'Instagram', x: 'twitter', pinterest: 'Pinterest' };
const ROUTES = {
  instagram: ['ig-embed', 'ig-page', 'ig-graphql', 'ig-ytdlp'],
  x: ['x-syndication', 'x-oembed', 'x-ytdlp'],
  pinterest: ['pin-pidgets', 'pin-resource', 'pin-page', 'pin-oembed', 'pin-ytdlp'],
};
const VIDEO_ROUTES = new Set(
  Object.values(ROUTES)
    .flat()
    .filter((r) => !r.endsWith('-oembed')),
);
const REFERER = {
  instagram: 'https://www.instagram.com/',
  x: 'https://x.com/',
  pinterest: 'https://www.pinterest.com/',
};
const CDN_GROUP = { instagram: 'ig-cdn', x: 'x-video', pinterest: 'pin-cdn' };
// Node's fetch rewrites `sec-fetch-mode` to `cors`, which changes what Instagram
// serves, so every request goes through node:https with exactly these headers.
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
const JSON_HEADERS = {
  'user-agent': UA,
  accept: 'application/json',
  'accept-language': 'en-US,en;q=0.9',
  'accept-encoding': 'gzip, deflate, br',
};

// ─── Pacing per host group ───────────────────────────────────────────────────

function hostGroup(host) {
  const h = host.toLowerCase();
  if (h === 'www.instagram.com' || h === 'instagram.com') return ['ig-www', IG_GAP_MS];
  if (/(^|\.)(cdninstagram\.com|fbcdn\.net)$/.test(h)) return ['ig-cdn', GAP_MS];
  if (h === 'video.twimg.com') return ['x-video', GAP_MS];
  if (/^publish\.(x|twitter)\.com$/.test(h)) return ['x-publish', GAP_MS];
  if (/(^|\.)(x\.com|twitter\.com)$/.test(h)) return ['x-www', GAP_MS];
  if (/(^|\.)pinimg\.com$/.test(h)) return ['pin-cdn', GAP_MS];
  if (/^(www\.)?pinterest\.com$/.test(h)) return ['pin-www', GAP_MS];
  return [h, GAP_MS];
}

const lastStart = new Map();
async function paceGroup(group, gap) {
  const wait = (lastStart.get(group) ?? 0) + gap + Math.random() * 250 - Date.now();
  if (wait > 0) await sleep(wait);
  lastStart.set(group, Date.now());
}
const pace = (url) => paceGroup(...hostGroup(new URL(url).hostname));

// ─── HTTP ─────────────────────────────────────────────────────────────────────

/** Where a redirect points, without keeping the URL. */
function pathClass(url, from) {
  const p = url.pathname.toLowerCase();
  if (/\/(accounts\/login|login|i\/flow\/login|signup)/.test(p)) return 'login';
  if (/\/(challenge|checkpoint|captcha)/.test(p)) return 'challenge';
  return url.hostname === from.hostname ? 'same-host' : 'other-host';
}

const AGENTS = {
  'https:': new https.Agent({ keepAlive: true, maxSockets: 4 }),
  'http:': new http.Agent({ keepAlive: true, maxSockets: 4 }),
};

/**
 * One HTTP exchange without redirects. Resolves when the response headers
 * arrive ({ res, req, timer }) or on error ({ error }); the timer keeps running
 * so it also bounds reading the body.
 */
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

/** Decoded body text with a size cap (gzip, deflate and br). */
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

/** One request with manual redirects, a body cap and timings. Never throws. */
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
      // Never follow into a wall: one request less to a host that refuses us.
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

/** Block signals carried by the status line or a redirect: rate limit, challenge, login wall. */
function blockOf(res) {
  if (!res) return null;
  if (res.status === 429) return 'rate_limited';
  if (res.redirects?.some((r) => r.to === 'challenge')) return 'challenge';
  if (res.redirects?.some((r) => r.to === 'login')) return 'login_wall';
  if (res.headers?.['cf-mitigated']) return 'challenge';
  return null;
}

/**
 * Block signals in the body of an answer that failed to parse. Only checked on
 * failures: healthy pages mention login and captcha modules in their scripts.
 */
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

/** A failed resolution: the error plus any block signal of the answer. */
const fail = (res, error) => ({
  res,
  block: blockOf(res) ?? bodyBlockOf(res),
  error: res.error ?? error,
});

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

function stripTags(html) {
  return decodeEntities(
    String(html ?? '')
      .replace(/<br\s*\/?>/gi, '\n')
      .replace(/<[^>]+>/g, ''),
  );
}

function metaContent(html, property) {
  for (const tag of html.match(/<meta\b[^>]*>/gi) ?? []) {
    const name = tag.match(/\b(?:property|name)\s*=\s*"([^"]*)"/i)?.[1]?.toLowerCase();
    if (name === property)
      return decodeEntities(tag.match(/\bcontent\s*=\s*"([^"]*)"/i)?.[1] ?? '');
  }
  return null;
}

/** `/<w>x<h>/` in a CDN path (X variants). */
function dims(url) {
  const m = String(url).match(/\/(\d{2,5})x(\d{2,5})\//);
  return m ? { w: Number(m[1]), h: Number(m[2]) } : {};
}

// ─── Instagram ───────────────────────────────────────────────────────────────

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
function igPk(code) {
  const short = code.length > 28 ? code.slice(0, -28) : code;
  let pk = 0n;
  for (const ch of short) pk = pk * 64n + BigInt(B64.indexOf(ch));
  return pk.toString();
}
const igCode = (url) =>
  new URL(url).pathname.match(/^\/(?:p|reel|reels|tv)\/([A-Za-z0-9_-]+)/)?.[1];

/** Fields of a logged-out media object (`xig_polaris_media.if_not_gated_logged_out`). */
function igMediaFields(media) {
  let item = media;
  if (media.media_type === 8 && Array.isArray(media.carousel_media)) {
    item = media.carousel_media.find((m) => m.video_versions?.length) ?? media.carousel_media[0];
  }
  const versions = item?.video_versions ?? [];
  return {
    caption: media.caption?.text ?? null,
    author: media.user?.username ?? media.owner?.username ?? null,
    date: media.taken_at ?? null,
    mediaUrl: item?.image_versions2?.candidates?.[0]?.url ?? media.display_uri ?? null,
    video: versions[0]?.url
      ? { url: versions[0].url, kind: 'mp4', variants: versions.length }
      : null,
    dash: Boolean(item?.video_dash_manifest),
  };
}

async function igEmbed(item) {
  const code = igCode(item.url);
  // A cross-site iframe load gets the legacy embed template; a plain document
  // load gets the Comet app, which fetches the post client-side.
  const res = await request(`https://www.instagram.com/p/${code}/embed/captioned/`, {
    headers: {
      'user-agent': UA,
      accept: HTML_ACCEPT,
      'accept-language': 'en-US,en;q=0.9',
      'accept-encoding': 'gzip, deflate, br',
      'upgrade-insecure-requests': '1',
      'sec-fetch-dest': 'iframe',
      'sec-fetch-mode': 'navigate',
      'sec-fetch-site': 'cross-site',
    },
  });
  if (res.status !== 200) return fail(res, `http_${res.status}`);
  const m = res.text.match(/"contextJSON":"((?:[^"\\]|\\.)*)"/);
  if (!m) {
    if (/EmbedBrokenMedia|"contextJSON":null/.test(res.text)) return fail(res, 'unavailable');
    return fail(res, 'parse_fail');
  }
  const ctx = parseJson(JSON.parse(`"${m[1]}"`));
  const sm = ctx?.gql_data?.shortcode_media;
  if (!sm) return fail(res, 'unavailable');
  let node = sm;
  if (sm.edge_sidecar_to_children?.edges?.length) {
    node = sm.edge_sidecar_to_children.edges.map((e) => e.node).find((n) => n.is_video) ?? sm;
  }
  return {
    res,
    block: null,
    caption: sm.edge_media_to_caption?.edges?.[0]?.node?.text ?? null,
    author: sm.owner?.username ?? null,
    date: sm.taken_at_timestamp ?? null,
    mediaUrl: node.display_url ?? sm.display_url ?? null,
    video: node.video_url ? { url: node.video_url, kind: 'mp4', variants: 1 } : null,
    note: node.is_video && !node.video_url ? 'video_without_url' : null,
  };
}

async function igPage(item) {
  const code = igCode(item.url);
  const res = await request(`https://www.instagram.com/p/${code}/`, { headers: DOC_HEADERS });
  if (res.status !== 200) return fail(res, `http_${res.status}`);
  const pageId = res.text.match(/"pageID":"([^"]+)"/)?.[1] ?? null;
  for (const m of res.text.matchAll(
    /<script type="application\/json"[^>]*>([\s\S]*?)<\/script>/g,
  )) {
    if (!m[1].includes('xig_polaris_media')) continue;
    const holder = findObject(parseJson(m[1]), (o) => 'xig_polaris_media' in o);
    const xig = holder?.xig_polaris_media;
    if (!xig) continue;
    const media = xig.if_not_gated_logged_out;
    if (!media) return fail(res, 'gated');
    return { res, block: null, ...igMediaFields(media) };
  }
  if (pageId === 'httpErrorPage') return fail(res, 'unavailable');
  return fail(res, 'parse_fail');
}

let igLsd = null;
async function igGraphql(item) {
  const code = igCode(item.url);
  let homeRes = null;
  if (!igLsd || Date.now() - igLsd.at > 10 * 60_000) {
    homeRes = await request('https://www.instagram.com/', { headers: DOC_HEADERS });
    const token = homeRes.text?.match(/"LSD",\[\],\{"token":"([^"]+)"/)?.[1];
    if (!token) return fail(homeRes, homeRes.status === 200 ? 'no_lsd' : `http_${homeRes.status}`);
    igLsd = { token, at: Date.now() };
  }
  const res = await request('https://www.instagram.com/api/graphql', {
    method: 'POST',
    headers: {
      'user-agent': UA,
      accept: '*/*',
      'accept-language': 'en-US,en;q=0.9',
      'accept-encoding': 'gzip, deflate, br',
      'content-type': 'application/x-www-form-urlencoded',
      'x-ig-app-id': IG_APP_ID,
      'x-asbd-id': '359341',
      'x-ig-www-claim': '0',
      'x-fb-friendly-name': IG_FRIENDLY,
      'x-fb-lsd': igLsd.token,
      'x-requested-with': 'XMLHttpRequest',
      origin: 'https://www.instagram.com',
      referer: `https://www.instagram.com/p/${code}/`,
      'sec-fetch-dest': 'empty',
      'sec-fetch-mode': 'cors',
      'sec-fetch-site': 'same-origin',
    },
    body: new URLSearchParams({
      lsd: igLsd.token,
      fb_api_caller_class: 'RelayModern',
      fb_api_req_friendly_name: IG_FRIENDLY,
      server_timestamps: 'true',
      variables: JSON.stringify({ media_id: igPk(code) }),
      doc_id: IG_DOC_ID,
    }).toString(),
  });
  if (homeRes) {
    // The token fetch is part of the route's cost.
    res.requests += homeRes.requests;
    res.ms += homeRes.ms;
  }
  if (res.status !== 200) return fail(res, `http_${res.status}`);
  const json = parseJson(res.text);
  if (!json) return fail(res, 'parse_fail');
  if (json.errors?.length && !json.data) {
    igLsd = null;
    return fail(res, 'graphql_error');
  }
  const xig = json.data?.xig_polaris_media;
  if (!xig) return fail(res, 'unavailable');
  const media = xig.if_not_gated_logged_out;
  if (!media) return fail(res, 'gated');
  return { res, block: null, ...igMediaFields(media) };
}

// ─── X ───────────────────────────────────────────────────────────────────────

const tweetId = (url) => new URL(url).pathname.match(/\/status(?:es)?\/(\d+)/)?.[1];

/** Best MP4 variant whose shorter side is at most 1080 px. */
function pickXVariant(variants) {
  const mp4 = variants
    .filter((v) => /mp4/.test(v.content_type ?? v.type ?? '') && (v.url ?? v.src))
    .map((v) => ({ url: v.url ?? v.src, bitrate: v.bitrate ?? 0, ...dims(v.url ?? v.src) }));
  const bounded = mp4.filter((v) => !v.w || Math.min(v.w, v.h) <= 1080);
  const pool = (bounded.length ? bounded : mp4).sort((a, b) => b.bitrate - a.bitrate);
  return pool[0] ? { ...pool[0], kind: 'mp4', variants: mp4.length } : null;
}

async function xSyndication(item) {
  const id = tweetId(item.url);
  const token = ((Number(id) / 1e15) * Math.PI).toString(36).replace(/(0+|\.)/g, '');
  const res = await request(
    `https://cdn.syndication.twimg.com/tweet-result?id=${id}&lang=en&token=${token}`,
    { headers: JSON_HEADERS },
  );
  if (res.status === 404) return fail(res, 'not_found');
  if (res.status !== 200) return fail(res, `http_${res.status}`);
  const j = parseJson(res.text);
  if (!j || !Object.keys(j).length) return fail(res, 'not_found');
  if (j.__typename === 'TweetTombstone') {
    const text = JSON.stringify(j.tombstone ?? '').toLowerCase();
    return fail(res, /age|adult|sensitive/.test(text) ? 'age_restricted' : 'unavailable');
  }
  const isVideo = (m) => m.type === 'video' || m.type === 'animated_gif';
  const videoMedia = (j.mediaDetails ?? []).find(isVideo);
  const quotedVideo = (j.quoted_tweet?.mediaDetails ?? []).some(isVideo);
  return {
    res,
    block: null,
    caption: j.text ?? null,
    author: j.user?.screen_name ?? null,
    date: j.created_at ? Math.round(Date.parse(j.created_at) / 1000) : null,
    mediaUrl: j.mediaDetails?.[0]?.media_url_https ?? j.photos?.[0]?.url ?? null,
    video: videoMedia ? pickXVariant(videoMedia.video_info?.variants ?? []) : null,
    note: videoMedia
      ? videoMedia.type
      : quotedVideo
        ? 'video_in_quote'
        : j.card
          ? 'card'
          : 'no_media_video',
  };
}

async function xOembed(item) {
  const res = await request(
    `https://publish.x.com/oembed?url=${encodeURIComponent(item.url)}&omit_script=true&dnt=true`,
    { headers: JSON_HEADERS },
  );
  if (res.status === 404) return fail(res, 'not_found');
  if (res.status !== 200) return fail(res, `http_${res.status}`);
  const j = parseJson(res.text);
  if (!j?.html) return fail(res, 'parse_fail');
  const p = j.html.match(/<p\b[^>]*>([\s\S]*?)<\/p>/)?.[1] ?? null;
  const dateText = [...j.html.matchAll(/<a\b[^>]*>([^<]*)<\/a>/g)].pop()?.[1];
  const date = dateText ? Date.parse(dateText) : NaN;
  let author = null;
  try {
    author = new URL(j.author_url).pathname.split('/').filter(Boolean)[0] ?? null;
  } catch {}
  return {
    res,
    block: null,
    caption: p ? stripTags(p) : null,
    author,
    date: Number.isFinite(date) ? Math.round(date / 1000) : null,
    mediaUrl: null,
    video: null,
    note: /pic\.(twitter|x)\.com/.test(j.html) ? 'media_link_only' : null,
  };
}

// ─── Pinterest ───────────────────────────────────────────────────────────────

const pinId = (url) => new URL(url).pathname.match(/\/pin\/(\d+)/)?.[1];

/** Best MP4 of a Pinterest video list (snake or camel case keys); HLS if nothing else. */
function pickPinVideo(list) {
  const entries = Object.entries(list ?? {})
    .filter(([, v]) => v && typeof v === 'object' && v.url)
    .map(([k, v]) => ({
      key: k.replace(/_/g, '').toUpperCase(),
      url: v.url,
      w: v.width,
      h: v.height,
    }));
  if (!entries.length) return null;
  const mp4 = entries.filter((e) => /\.mp4(\?|$)/.test(e.url));
  const pick =
    mp4.find((e) => e.key === 'V720P') ??
    mp4.sort((a, b) => (b.w ?? 0) - (a.w ?? 0))[0] ??
    entries.find((e) => /\.m3u8(\?|$)/.test(e.url));
  if (!pick) return null;
  return {
    url: pick.url,
    w: pick.w ?? null,
    h: pick.h ?? null,
    kind: /\.m3u8/.test(pick.url) ? 'hls' : 'mp4',
    variants: entries.length,
  };
}

/** The video list of a pin object: its own, or the first video block of a story pin. */
function pinVideoList(pin) {
  const own = pin?.videos?.video_list ?? pin?.videos?.videoList;
  if (own && Object.values(own).some((v) => v?.url)) return own;
  const story = pin?.story_pin_data ?? pin?.storyPinData;
  for (const page of story?.pages ?? []) {
    for (const block of page?.blocks ?? []) {
      const list =
        block?.video?.video_list ?? block?.video?.videoList ?? block?.videoDataV2?.videoList;
      if (list && Object.values(list).some((v) => v?.url)) return list;
    }
  }
  return null;
}

function pinterestUsername(profileUrl) {
  try {
    return new URL(profileUrl).pathname.split('/').filter(Boolean)[0] ?? null;
  } catch {
    return null;
  }
}

function pinFields(pin) {
  const list = pinVideoList(pin);
  const images = pin.images ?? {};
  const image =
    images.orig?.url ?? images['736x']?.url ?? images['564x']?.url ?? images['237x']?.url ?? null;
  const createdAt = pin.created_at ? Date.parse(pin.created_at) : NaN;
  return {
    caption: pin.description?.trim() || pin.title?.trim() || pin.grid_title?.trim() || null,
    author: pin.pinner?.username ?? pinterestUsername(pin.pinner?.profile_url) ?? null,
    date: Number.isFinite(createdAt) ? Math.round(createdAt / 1000) : null,
    mediaUrl: image,
    video: list ? pickPinVideo(list) : null,
    note: pin.story_pin_data || pin.storyPinData ? 'story_pin' : null,
  };
}

async function pinPidgets(item) {
  const res = await request(
    `https://widgets.pinterest.com/v3/pidgets/pins/info/?pin_ids=${pinId(item.url)}`,
    { headers: JSON_HEADERS },
  );
  if (res.status !== 200) return fail(res, `http_${res.status}`);
  const pin = parseJson(res.text)?.data?.[0];
  if (!pin) return fail(res, 'parse_fail');
  if (pin.error) return fail(res, 'not_found');
  return { res, block: null, ...pinFields(pin) };
}

async function pinResource(item) {
  const data = JSON.stringify({
    options: { field_set_key: 'unauth_react_main_pin', id: pinId(item.url) },
  });
  const res = await request(
    `https://www.pinterest.com/resource/PinResource/get/?data=${encodeURIComponent(data)}`,
    {
      headers: {
        ...JSON_HEADERS,
        accept: 'application/json, text/javascript, */*; q=0.01',
        'x-pinterest-pws-handler': 'www/pin/[id].js',
      },
    },
  );
  const j = parseJson(res.text);
  if (res.status === 404 || j?.resource_response?.error?.http_status === 404)
    return fail(res, 'not_found');
  if (res.status !== 200) return fail(res, `http_${res.status}`);
  const pin = j?.resource_response?.data;
  if (!pin) return fail(res, 'parse_fail');
  return { res, block: null, ...pinFields(pin) };
}

async function pinPage(item) {
  const id = pinId(item.url);
  const res = await request(`https://www.pinterest.com/pin/${id}/`, { headers: DOC_HEADERS });
  if (res.status === 404) return fail(res, 'not_found');
  if (res.status !== 200) return fail(res, `http_${res.status}`);
  const responses = [];
  for (const m of res.text.matchAll(
    /window\.__PWS_RELAY_REGISTER_COMPLETED_REQUEST__\("[^"]*",\s*([\s\S]*?)\);\s*<\/script>/g,
  )) {
    const json = parseJson(m[1]);
    if (json) responses.push(json);
  }
  // The page inlines the pin twice; only the complete copy has the pinner's username.
  const isPin = (o) =>
    String(o.entityId) === id && ('description' in o || 'title' in o || 'pinner' in o);
  const pin =
    findObject(responses, (o) => isPin(o) && Boolean(o.pinner?.username)) ??
    findObject(responses, isPin);
  const listHolder = findObject(
    responses,
    (o) =>
      o.videoList &&
      typeof o.videoList === 'object' &&
      Object.values(o.videoList).some((v) => v?.url),
  );
  const ogImage = metaContent(res.text, 'og:image');
  if (!pin && !listHolder) {
    if (!ogImage) return fail(res, 'parse_fail');
    return {
      res,
      block: null,
      caption: metaContent(res.text, 'og:description') || metaContent(res.text, 'og:title'),
      author: null,
      date: null,
      mediaUrl: ogImage,
      video: null,
      note: 'og_only',
    };
  }
  const createdAt = pin?.createdAt ? Date.parse(pin.createdAt) : NaN;
  return {
    res,
    block: null,
    caption:
      pin?.description?.trim() ||
      pin?.closeupUnifiedDescription?.trim() ||
      pin?.title?.trim() ||
      metaContent(res.text, 'og:description') ||
      null,
    author: pin?.pinner?.username ?? pin?.nativeCreator?.username ?? null,
    date: Number.isFinite(createdAt) ? Math.round(createdAt / 1000) : null,
    mediaUrl: pin?.imageSpec_orig?.url ?? pin?.imageSpec_736x?.url ?? ogImage ?? null,
    video: listHolder ? pickPinVideo(listHolder.videoList) : null,
  };
}

async function pinOembed(item) {
  const res = await request(
    `https://www.pinterest.com/oembed.json?url=${encodeURIComponent(item.url)}`,
    { headers: JSON_HEADERS },
  );
  if (res.status === 404) return fail(res, 'not_found');
  if (res.status !== 200) return fail(res, `http_${res.status}`);
  const j = parseJson(res.text);
  if (!j) return fail(res, 'parse_fail');
  return {
    res,
    block: null,
    caption: j.title || null,
    author: pinterestUsername(j.author_url),
    date: null,
    mediaUrl: j.thumbnail_url ?? null,
    video: null,
  };
}

// ─── Media checks ────────────────────────────────────────────────────────────

function run(cmd, cmdArgs, timeoutMs) {
  return new Promise((resolve) => {
    const child = spawn(cmd, cmdArgs, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    const timer = setTimeout(() => child.kill('SIGKILL'), timeoutMs);
    child.stdout.on('data', (d) => (stdout += d));
    child.stderr.on('data', () => {});
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

async function ffprobe(file) {
  const r = await run(
    args.ffprobe,
    ['-v', 'error', '-print_format', 'json', '-show_streams', '-show_format', file],
    60_000,
  );
  const j = parseJson(r.stdout);
  if (!j) return { error: 'ffprobe_failed' };
  const v = (j.streams ?? []).find((s) => s.codec_type === 'video');
  const a = (j.streams ?? []).find((s) => s.codec_type === 'audio');
  return {
    vcodec: v?.codec_name ?? null,
    acodec: a?.codec_name ?? null,
    w: v?.width ?? null,
    h: v?.height ?? null,
    durS: j.format?.duration ? Math.round(Number(j.format.duration) * 10) / 10 : null,
    format: j.format?.format_name ?? null,
  };
}

/** Top-level MP4 box order: `moov` before `mdat` means the file streams without a remux. */
function mp4Layout(file) {
  let fd;
  try {
    fd = openSync(file, 'r');
    const size = fstatSync(fd).size;
    const header = Buffer.alloc(16);
    const order = [];
    let offset = 0;
    while (offset + 8 <= size && order.length < 64) {
      readSync(fd, header, 0, 16, offset);
      let boxSize = header.readUInt32BE(0);
      const type = header.toString('latin1', 4, 8);
      if (boxSize === 1) boxSize = Number(header.readBigUInt64BE(8));
      else if (boxSize === 0) boxSize = size - offset;
      if (boxSize < 8) break;
      order.push(type);
      offset += boxSize;
    }
    const moov = order.indexOf('moov');
    const mdat = order.indexOf('mdat');
    return {
      faststart: moov >= 0 && (mdat < 0 || moov < mdat),
      fragmented: order.includes('moof'),
    };
  } catch {
    return { faststart: null, fragmented: null };
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

function mediaHeaders(platform) {
  return {
    'user-agent': UA,
    accept: '*/*',
    'accept-language': 'en-US,en;q=0.9',
    referer: REFERER[platform],
    'sec-fetch-dest': 'video',
    'sec-fetch-mode': 'no-cors',
    'sec-fetch-site': 'cross-site',
  };
}

/** Full GET of a video into --work, then ffprobe; the file is deleted before returning. */
async function downloadVideo(url, platform, base) {
  const dest = join(args.work, `${base}.bin`);
  const t0 = performance.now();
  const result = { mode: 'full' };
  let current = new URL(url);
  try {
    for (let hop = 0; ; hop++) {
      await pace(current.href);
      const ex = await exchange(current, {
        headers: mediaHeaders(platform),
        timeoutMs: DOWNLOAD_TIMEOUT_MS,
      });
      if (ex.error) {
        result.error = ex.error;
        return result;
      }
      const { res, timer } = ex;
      result.status = res.statusCode;
      result.ttfbMs ??= Math.round(performance.now() - t0);
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
      const out = createWriteStream(dest);
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
      result.probe = await ffprobe(dest);
      Object.assign(result, mp4Layout(dest));
    }
  } finally {
    result.ms = Math.round(performance.now() - t0);
    rmSync(dest, { force: true });
    result.ok = !result.error && Boolean(result.probe?.vcodec);
  }
  return result;
}

/** 1 KiB range GET: does this signed URL serve too? */
async function rangeCheck(url, platform) {
  const t0 = performance.now();
  await pace(url);
  const ex = await exchange(new URL(url), {
    headers: { ...mediaHeaders(platform), range: 'bytes=0-1023' },
  });
  if (ex.error) return { mode: 'range', error: ex.error, ok: false };
  const { res, timer } = ex;
  res.destroy();
  clearTimeout(timer);
  return {
    mode: 'range',
    status: res.statusCode,
    ms: Math.round(performance.now() - t0),
    contentType: res.headers['content-type'] ?? null,
    ok: res.statusCode === 206 || res.statusCode === 200,
    block: res.statusCode === 429 ? 'rate_limited' : null,
  };
}

// ─── yt-dlp ──────────────────────────────────────────────────────────────────

const YTDLP_ERRORS = [
  ['rate_limited', /\b429\b|too many requests|rate.?limit exceeded/i],
  ['login_or_rate_limit', /rate-limit reached or login required/i],
  ['login_required', /login required|log in|sign in|--cookies|not granting access|authentication/i],
  ['age_restricted', /age.?restrict|sensitive|nsfw|adult content|inappropriate/i],
  ['private', /private/i],
  [
    'not_found',
    /\b404\b|not found|does not exist|no longer available|has been (?:removed|deleted)|unavailable/i,
  ],
  ['no_video', /no video|no formats|requested format is not available|there is no video/i],
  ['max_filesize', /max-filesize|larger than max/i],
  ['unsupported_url', /unsupported url/i],
  [
    'network',
    /timed out|timeout|connection|reset by peer|ssl|temporary failure|name resolution|eof/i,
  ],
  ['postprocess', /ffmpeg|postprocess|merg/i],
];

function sanitize(line, idsToHide) {
  let s = line.replace(/https?:\/\/\S+/g, '<url>');
  for (const id of idsToHide) if (id) s = s.split(id).join('<id>');
  return s.replace(/\d{6,}/g, '<n>').slice(0, 120);
}

async function ytdlp(item, platform, base, hide) {
  const [group, gap] = hostGroup(new URL(item.url).hostname);
  await paceGroup(group, gap);
  const ytArgs = [
    '--ignore-config',
    '--no-cookies',
    '--no-cookies-from-browser',
    '--no-cache-dir',
    '--no-plugin-dirs',
    '--use-extractors',
    YTDLP_EXTRACTOR[platform],
    '--no-playlist',
    '--newline',
    '--no-progress',
    '--no-mtime',
    '--socket-timeout',
    '30',
    '--sleep-requests',
    '1',
    '--max-filesize',
    String(MAX_BYTES),
    '-f',
    YTDLP_FORMAT,
    '--merge-output-format',
    'mp4',
    '--ffmpeg-location',
    args.ffmpeg,
    '--write-info-json',
    '-o',
    join(args.work, `${base}.%(ext)s`),
  ];
  if (RESOLVE_ONLY) ytArgs.push('--skip-download');
  ytArgs.push('--', item.url);

  const t0 = performance.now();
  let extractMs = null;
  const lines = [];
  const exit = await new Promise((resolve) => {
    const child = spawn(args.ytdlp, ytArgs, {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, HOME: args.work },
    });
    const timer = setTimeout(() => child.kill('SIGKILL'), YTDLP_TIMEOUT_MS);
    const onData = (chunk) => {
      for (const line of String(chunk).split(/\r?\n/)) {
        if (!line.trim()) continue;
        if (
          extractMs === null &&
          /^\[info\] .*(Downloading \d+ format|Writing video metadata)/.test(line)
        )
          extractMs = Math.round(performance.now() - t0);
        lines.push(line);
      }
    };
    child.stdout.on('data', onData);
    child.stderr.on('data', onData);
    child.on('error', () => {
      clearTimeout(timer);
      resolve(-1);
    });
    child.on('close', (code) => {
      clearTimeout(timer);
      resolve(code);
    });
  });
  const ms = Math.round(performance.now() - t0);
  // yt-dlp just used the platform's page and CDN hosts.
  lastStart.set(group, Date.now());
  lastStart.set(CDN_GROUP[platform], Date.now());

  const errorLine = lines.filter((l) => /^ERROR:/.test(l)).pop() ?? null;
  const errorClass = errorLine
    ? (YTDLP_ERRORS.find(([, re]) => re.test(errorLine))?.[0] ?? 'other')
    : exit !== 0
      ? 'other'
      : null;
  const warnings = [
    ...new Set(lines.filter((l) => /^WARNING:/.test(l)).map((l) => sanitize(l, hide))),
  ];

  const files = existsSync(args.work)
    ? readdirSync(args.work).filter((f) => f.startsWith(`${base}.`))
    : [];
  const infoFile = files.find((f) => f.endsWith('.info.json'));
  const info = infoFile ? parseJson(readFileSync(join(args.work, infoFile), 'utf8')) : null;
  // The merged or single-format output: `<base>.<ext>`, no format id, no `.part`.
  const media = files.find((f) => {
    const suffix = f.slice(base.length + 1);
    return /^[a-z0-9]+$/i.test(suffix) && suffix !== 'json' && suffix !== 'part';
  });
  let download = null;
  if (media && !RESOLVE_ONLY) {
    const file = join(args.work, media);
    download = {
      mode: 'ytdlp',
      bytes: statSync(file).size,
      ms: extractMs !== null ? ms - extractMs : null,
      probe: await ffprobe(file),
      ...mp4Layout(file),
    };
    download.ok = exit === 0 && Boolean(download.probe?.vcodec);
  }
  for (const f of files) rmSync(join(args.work, f), { force: true });

  // The written info JSON drops `requested_formats`: look the chosen ids up in `formats`.
  const requested = String(info?.format_id ?? '')
    .split('+')
    .map((id) => info?.formats?.find((f) => f.format_id === id))
    .filter(Boolean);
  if (!requested.length && info?.url) requested.push(info);
  const videoFormat = requested.find((f) => f.vcodec && f.vcodec !== 'none') ?? requested[0];
  // IG: `channel` is the username. X: `uploader_id` is the screen name. Pinterest:
  // only the native creator's display name (`uploader`), never the pinner.
  const author =
    platform === 'instagram'
      ? (info?.channel ?? info?.uploader_id)
      : platform === 'x'
        ? info?.uploader_id
        : info?.uploader;
  return {
    res: { status: exit, ms: extractMs ?? ms, requests: null, redirects: [] },
    block:
      errorClass === 'rate_limited'
        ? 'rate_limited'
        : errorClass === 'login_required'
          ? 'login_wall'
          : null,
    error: errorClass,
    caption: info?.description ?? null,
    author: author ?? null,
    date: info?.timestamp ?? null,
    mediaUrl: info?.thumbnail ?? null,
    video: videoFormat?.url
      ? {
          url: videoFormat.url,
          kind: /m3u8/.test(videoFormat.protocol ?? '') ? 'hls' : 'mp4',
          w: videoFormat.width ?? null,
          h: videoFormat.height ?? null,
          variants: info?.formats?.length ?? null,
        }
      : null,
    ytdlp: {
      exit,
      totalMs: ms,
      extractMs,
      errorClass,
      // Unclassified errors keep their sanitized text so the classes can be extended.
      errorLine: errorClass === 'other' && errorLine ? sanitize(errorLine, hide) : null,
      warnings,
      formatId: info?.format_id ? sanitize(info.format_id, []) : null,
      vcodec: videoFormat?.vcodec ?? null,
      protocol: requested.map((f) => f.protocol).join('+') || null,
      formats: info?.formats?.length ?? null,
    },
    download,
  };
}

// ─── Main loop ───────────────────────────────────────────────────────────────

const RESOLVERS = {
  'ig-embed': igEmbed,
  'ig-page': igPage,
  'ig-graphql': igGraphql,
  'x-syndication': xSyndication,
  'x-oembed': xOembed,
  'pin-pidgets': pinPidgets,
  'pin-resource': pinResource,
  'pin-page': pinPage,
  'pin-oembed': pinOembed,
};

const sample = readFileSync(args.in, 'utf8')
  .split('\n')
  .filter((line) => line && !line.startsWith('#'))
  .map((line) => {
    const [id, platform, kind, url, capKey, authKey] = line.split('\t');
    return { id, platform, kind, url, capKey: capKey || null, authKey: authKey || null };
  });

mkdirSync(args.work, { recursive: true });
const out = createWriteStream(args.out, { flags: 'a', mode: 0o600 });
const write = (obj) => out.write(JSON.stringify({ label: args.label, ...obj }) + '\n');
const runStart = Date.now();
const onlyRoutes = args.routes ? new Set(args.routes.split(',')) : null;
const hours = (s) => (s === null ? null : Math.round(s / 360) / 10);

for (const platform of args.platforms.split(',')) {
  const routes = ROUTES[platform].filter((r) => !onlyRoutes || onlyRoutes.has(r));
  const skip = Number(args.skip);
  const items = sample
    .filter((s) => s.platform === platform)
    .slice(skip, args.limit ? skip + Number(args.limit) : undefined);
  console.error(`[${args.label}] ${platform}: ${items.length} posts × ${routes.length} routes`);
  let platformStop = null;
  const routeStop = new Set();
  const loginStreak = new Map();

  for (const [index, item] of items.entries()) {
    if (platformStop) break;
    const seq = skip + index;
    const fetched = new Map(); // CDN path → the route whose full download succeeded
    const hide = [item.url.match(/\/(?:p|reel|status|pin)\/([^/?#]+)/)?.[1]];
    for (const route of routes) {
      if (platformStop || routeStop.has(route)) continue;
      const t = Date.now() - runStart;
      let r;
      try {
        r = route.endsWith('-ytdlp')
          ? await ytdlp(item, platform, `${item.id}-${route}`, hide)
          : await RESOLVERS[route](item);
      } catch (err) {
        r = { res: {}, block: null, error: `exception:${String(err.message).slice(0, 60)}` };
      }

      const nowMs = Date.now();
      const video = r.video ?? null;
      let dl = r.download ?? null;
      if (video && !route.endsWith('-ytdlp') && !RESOLVE_ONLY) {
        if (video.kind === 'hls') {
          dl = { mode: 'skipped', error: 'hls_only', ok: false };
        } else {
          const key = video.url.split('?')[0];
          dl = fetched.has(key)
            ? { ...(await rangeCheck(video.url, platform)), sameAs: fetched.get(key) }
            : await downloadVideo(video.url, platform, `${item.id}-${route}`);
          if (!fetched.has(key) && dl.ok) fetched.set(key, route);
        }
      }

      const hydratedCaptionKey = r.caption ? captionKey(r.caption) : null;
      const hydratedAuthorKey = r.author ? authorKey(r.author) : null;
      const block = r.block ?? dl?.block ?? null;
      const resolved = Boolean(video?.url);
      let outcome;
      if (block) outcome = 'blocked';
      else if (RESOLVE_ONLY) outcome = resolved ? 'resolved' : (r.error ?? 'no_video');
      else if (dl?.ok) outcome = 'ok';
      else if (resolved) outcome = `fetch_failed:${dl?.error ?? 'unknown'}`;
      else outcome = r.error ?? 'no_video';

      write({
        type: 'result',
        id: item.id,
        platform,
        route,
        seq,
        tRunMs: t,
        videoRoute: VIDEO_ROUTES.has(route),
        outcome,
        resolve: {
          status: r.res?.status ?? null,
          ms: r.res?.ms ?? null,
          ttfbMs: r.res?.ttfbMs ?? null,
          bytes: r.res?.bytes ?? null,
          requests: r.res?.requests ?? null,
          redirects: r.res?.redirects ?? [],
          error: r.error ?? null,
          block,
          note: r.note ?? null,
        },
        fields: {
          caption: Boolean(r.caption && String(r.caption).trim()),
          author: Boolean(r.author),
          date: Boolean(r.date),
          media: Boolean(r.mediaUrl),
          video: resolved,
        },
        captionKey: hydratedCaptionKey,
        authorKey: hydratedAuthorKey,
        captionMatch: item.capKey && hydratedCaptionKey ? item.capKey === hydratedCaptionKey : null,
        authorMatch: item.authKey && hydratedAuthorKey ? item.authKey === hydratedAuthorKey : null,
        video: video
          ? {
              kind: video.kind,
              w: video.w ?? null,
              h: video.h ?? null,
              bitrate: video.bitrate ?? null,
              variants: video.variants ?? null,
              dash: r.dash ?? null,
              oeH: platform === 'instagram' ? hours(oeSecondsLeft(video.url, nowMs)) : null,
            }
          : null,
        posterOeH:
          platform === 'instagram' && r.mediaUrl ? hours(oeSecondsLeft(r.mediaUrl, nowMs)) : null,
        download: dl,
        ytdlp: r.ytdlp ?? null,
      });
      console.error(`[${args.label}] ${item.id} ${route}: ${outcome}`);

      // Stop rules: a platform at the first rate limit or challenge, a route after
      // two posts in a row behind a login wall.
      if (block === 'rate_limited' || block === 'challenge') {
        platformStop = { reason: block, route, atSeq: seq };
      } else if (block === 'login_wall') {
        const streak = (loginStreak.get(route) ?? 0) + 1;
        loginStreak.set(route, streak);
        if (streak >= 2) {
          routeStop.add(route);
          write({
            type: 'stop',
            platform,
            route,
            reason: 'login_wall',
            atSeq: seq,
            tRunMs: Date.now() - runStart,
          });
        }
      } else {
        loginStreak.set(route, 0);
      }
    }
  }
  if (platformStop) {
    write({ type: 'stop', platform, ...platformStop, tRunMs: Date.now() - runStart });
    console.error(
      `[${args.label}] ${platform} stopped: ${platformStop.reason} on ${platformStop.route}`,
    );
  }
}

await new Promise((resolve) => out.end(resolve));
console.error(`[${args.label}] finished in ${Math.round((Date.now() - runStart) / 1000)} s`);
