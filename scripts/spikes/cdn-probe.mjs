#!/usr/bin/env node
/**
 * SPIKE-2 probe: anonymous GET of CDN media URLs, paced per host group.
 *
 * Reads the TSV written by `cdn-sample.mjs` and fetches every URL once with no
 * cookies, a desktop Chrome User-Agent, a browser image `Accept` and the
 * platform `Referer` (the request the archive worker of plan §2.13 makes).
 * Host groups (Instagram, X, Pinterest) run in parallel; inside a group the
 * requests are sequential and start at least 1/--rate seconds apart, so no
 * host sees more than --rate requests per second.
 *
 * Writes one JSON line per URL to --out, keyed by the sample id. The output
 * never contains URLs: only status, redirects (host only), timings, size,
 * content type, a short body hash (to compare runs), and a block/challenge
 * classification for non-media answers.
 *
 * Runs unchanged on a laptop and in `node:24-alpine`; no dependencies.
 *
 * Usage:
 *   node scripts/spikes/cdn-probe.mjs --in sample.tsv --out results.jsonl
 *     [--label vps] [--rate 2] [--timeout 30000] [--max-bytes 15728640] [--limit N]
 */
import { createHash } from 'node:crypto';
import { createWriteStream, readFileSync } from 'node:fs';
import http from 'node:http';
import https from 'node:https';
import { setTimeout as sleep } from 'node:timers/promises';
import { parseArgs } from 'node:util';

const { values: args } = parseArgs({
  options: {
    in: { type: 'string' },
    out: { type: 'string' },
    label: { type: 'string', default: 'run' },
    rate: { type: 'string', default: '2' },
    timeout: { type: 'string', default: '30000' },
    'max-bytes': { type: 'string', default: String(15 * 1024 * 1024) },
    limit: { type: 'string' },
  },
});
if (!args.in || !args.out) {
  console.error('usage: cdn-probe.mjs --in sample.tsv --out results.jsonl [--label name]');
  process.exit(2);
}

const RATE = Number(args.rate);
const TIMEOUT_MS = Number(args.timeout);
const MAX_BYTES = Number(args['max-bytes']);
const MAX_REDIRECTS = 5;
const UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36';
const HOST_GROUPS = [
  ['instagram', /(^|\.)(cdninstagram\.com|fbcdn\.net)$/],
  ['x', /^(pbs|video)\.twimg\.com$/],
  ['pinterest', /(^|\.)pinimg\.com$/],
];
const REFERER = {
  instagram: 'https://www.instagram.com/',
  x: 'https://x.com/',
  pinterest: 'https://www.pinterest.com/',
};
const CHALLENGE_PATTERNS = [
  ['cloudflare_challenge', /cf-chl|challenge-platform|just a moment|attention required/],
  ['captcha', /captcha|perimeterx|datadome|are you a robot/],
  ['ig_signature_expired', /url signature expired/],
  ['ig_bad_timestamp', /bad url timestamp/],
  ['ig_signature_mismatch', /url signature mismatch/],
  ['block_page', /access denied|forbidden|blocked|unusual traffic|rate limit|too many requests/],
];

function hostGroup(host) {
  for (const [group, re] of HOST_GROUPS) if (re.test(host)) return group;
  return 'other';
}

function igExpiresIn(url) {
  const oe = url.searchParams.get('oe');
  if (!oe || !/^[0-9a-f]+$/i.test(oe)) return null;
  return Math.round(parseInt(oe, 16) - Date.now() / 1000);
}

function isMedia(contentType) {
  return /^(image|video)\//i.test(contentType ?? '');
}

/** Labels a non-media answer (block page, challenge, CDN error text). */
function classify(status, contentType, head) {
  if (isMedia(contentType) && status >= 200 && status < 300) return null;
  const text = head.toString('utf8').toLowerCase();
  for (const [label, re] of CHALLENGE_PATTERNS) if (re.test(text)) return label;
  if (status >= 200 && status < 300) return 'non_media_2xx';
  return null;
}

/** One HTTP exchange without following redirects. Never rejects. */
function exchange(url, headers, agents) {
  return new Promise((resolve) => {
    const t0 = performance.now();
    const lib = url.protocol === 'http:' ? http : https;
    let ttfb = null;
    let settled = false;
    const finish = (result) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve({ ttfbMs: ttfb, totalMs: Math.round(performance.now() - t0), ...result });
    };
    const req = lib.request(url, { method: 'GET', headers, agent: agents[url.protocol] });
    const timer = setTimeout(() => req.destroy(new Error('timeout')), TIMEOUT_MS);
    req.on('error', (err) =>
      finish({ error: err.message === 'timeout' ? 'timeout' : err.code || err.message }),
    );
    req.on('response', (res) => {
      ttfb = Math.round(performance.now() - t0);
      const hash = createHash('sha256');
      const head = [];
      let headLen = 0;
      let bytes = 0;
      res.on('data', (chunk) => {
        bytes += chunk.length;
        if (bytes > MAX_BYTES) {
          req.destroy(new Error('max_bytes'));
          return;
        }
        hash.update(chunk);
        if (headLen < 4096) {
          const part = chunk.subarray(0, 4096 - headLen);
          head.push(part);
          headLen += part.length;
        }
      });
      res.on('end', () =>
        finish({
          status: res.statusCode,
          headers: res.headers,
          bytes,
          sha: hash.digest('hex').slice(0, 16),
          head: Buffer.concat(head),
          reused: req.reusedSocket,
        }),
      );
      res.on('error', (err) =>
        finish({ status: res.statusCode, bytes, error: err.code || err.message }),
      );
      res.on('aborted', () => finish({ status: res.statusCode, bytes, error: 'aborted' }));
    });
    req.end();
  });
}

async function probe(item, agents, interval) {
  const started = Date.now();
  let url = new URL(item.url);
  const group = hostGroup(url.hostname);
  const headers = {
    'user-agent': UA,
    accept: 'image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8',
    'accept-language': 'en-US,en;q=0.9',
    referer: REFERER[group] ?? 'https://www.google.com/',
  };
  const record = {
    id: item.id,
    platform: item.platform,
    kind: item.kind,
    freshness: item.freshness,
    group,
    host: url.hostname,
    expiresInS: group === 'instagram' ? igExpiresIn(url) : null,
    redirects: [],
  };
  let res = await exchange(url, headers, agents);
  let ttfbMs = res.ttfbMs;
  let totalMs = res.totalMs;
  while (
    res.status >= 300 &&
    res.status < 400 &&
    res.headers?.location &&
    record.redirects.length < MAX_REDIRECTS
  ) {
    const next = new URL(res.headers.location, url);
    record.redirects.push({ status: res.status, host: next.hostname });
    url = next;
    await sleep(interval);
    res = await exchange(url, headers, agents);
    totalMs += res.totalMs;
    ttfbMs = (ttfbMs ?? 0) + (res.ttfbMs ?? 0);
  }
  const contentType = res.headers?.['content-type'] ?? null;
  const head = res.head ?? Buffer.alloc(0);
  const challenge = res.status ? classify(res.status, contentType, head) : null;
  return {
    ...record,
    startedAt: started,
    status: res.status ?? null,
    error: res.error ?? null,
    finalHost: url.hostname,
    ttfbMs,
    totalMs,
    bytes: res.bytes ?? 0,
    contentType,
    contentLength: res.headers?.['content-length'] ? Number(res.headers['content-length']) : null,
    server: res.headers?.server ?? null,
    retryAfter: res.headers?.['retry-after'] ?? null,
    cfMitigated: res.headers?.['cf-mitigated'] ?? null,
    sha: res.sha ?? null,
    reused: res.reused ?? null,
    challenge,
    // Short text of non-media answers, URLs stripped (local result files only).
    snippet:
      res.status && !isMedia(contentType) && head.length
        ? head
            .toString('utf8')
            .replace(/https?:\/\/\S+/g, '<url>')
            .replace(/\s+/g, ' ')
            .slice(0, 80)
        : null,
  };
}

const items = readFileSync(args.in, 'utf8')
  .split('\n')
  .filter((line) => line && !line.startsWith('#'))
  .map((line) => {
    const [id, platform, kind, freshness, url] = line.split('\t');
    return { id, platform, kind, freshness, url };
  })
  .slice(0, args.limit ? Number(args.limit) : undefined);

const groups = new Map();
for (const item of items) {
  const group = hostGroup(new URL(item.url).hostname);
  if (!groups.has(group)) groups.set(group, []);
  groups.get(group).push(item);
}

const out = createWriteStream(args.out, { flags: 'w', mode: 0o600 });
const runStart = Date.now();
const interval = 1000 / RATE;
console.error(
  `[${args.label}] ${items.length} URLs in ${groups.size} host groups, <= ${RATE} req/s per group`,
);

await Promise.all(
  [...groups.entries()].map(async ([group, list]) => {
    // One keep-alive socket per group, like a paced fetcher would hold.
    const agents = {
      'https:': new https.Agent({ keepAlive: true, maxSockets: 1 }),
      'http:': new http.Agent({ keepAlive: true, maxSockets: 1 }),
    };
    let nextStart = performance.now();
    let ok = 0;
    for (const [i, item] of list.entries()) {
      const wait = nextStart - performance.now();
      if (wait > 0) await sleep(wait);
      nextStart = performance.now() + interval + Math.random() * 100;
      const result = await probe(item, agents, interval);
      if (result.status >= 200 && result.status < 300) ok++;
      out.write(
        JSON.stringify({
          label: args.label,
          seq: i,
          tRunMs: result.startedAt - runStart,
          ...result,
        }) + '\n',
      );
      if ((i + 1) % 50 === 0 || i === list.length - 1) {
        console.error(`[${args.label}] ${group}: ${i + 1}/${list.length} done, ${ok} 2xx`);
      }
    }
    agents['https:'].destroy();
    agents['http:'].destroy();
  }),
);
await new Promise((resolve) => out.end(resolve));
console.error(`[${args.label}] finished in ${Math.round((Date.now() - runStart) / 1000)} s`);
