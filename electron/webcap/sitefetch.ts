// Site discovery, page ranking, HTTP fetch, image download/encode and on-disk
// asset paths for the web-reference capture.
//
// These helpers were part of electron/webcapture.ts (the v1 Electron capture
// engine). They are the only pieces of that 2,100-line file the v2 capture path
// (electron/webcap/*) and the P4 capture service still need, so they were moved
// here (SPIKE-11): the service bundles electron/webcap/* and no longer drags in
// the v1 engine, its BrowserWindow/session fallbacks or its DOM-prep scripts.
// webcapture.ts imports them back for its own engine, so the desktop is unchanged.
//
// Everything here is plain Node: `fetch` (through NODE_USE_ENV_PROXY in the
// service), the SSRF guard from net-safety, ffmpeg via resolveFfmpeg(), and
// `app` only for app.getPath (the capture service supplies an Electron shim).

import fs from 'fs';
import os from 'os';
import path from 'path';
import zlib from 'zlib';
import { promisify } from 'util';
import { spawn } from 'child_process';
import { createHash } from 'crypto';
import { app } from 'electron';
import { assertSafeUrl, isBlockedHostname } from '../net-safety';

const gunzipAsync = promisify(zlib.gunzip);

const DESKTOP_UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36';

const FETCH_TIMEOUT_MS = 10_000;
const FETCH_MAX_BYTES = 2 * 1024 * 1024; // cap any single fetched body (home/sitemap)
const GUNZIP_MAX_BYTES = FETCH_MAX_BYTES * 8; // cap decompressed sitemap output (gzip-bomb guard)
const FETCH_MAX_REDIRECTS = 5; // manual redirect-follow cap; every hop is SSRF-validated
const SITEMAP_DEPTH_CAP = 2; // sitemapindex recursion depth
const SITEMAP_CHILD_CAP = 5; // max child sitemaps visited
const RAW_URL_CAP = 200; // stop collecting once we have this many raw URLs
const MAX_PAGES_HARD = 8; // clamp for maxPages

// WebP downscale cap for fetched images (og:image / favicon). Equals the v1
// engine's VIEWPORT_W (electron/webcapture.ts); kept local so this module has no
// circular import back into the engine.
const WEBP_MAX_WIDTH = 1280;

function throwIfAborted(signal?: AbortSignal): void {
  if (signal && signal.aborted) {
    throw Object.assign(new Error('AbortError'), { name: 'AbortError' });
  }
}

const TRACKING_PARAMS = new Set([
  'gclid',
  'fbclid',
  'mc_eid',
  'mc_cid',
  'ref',
  'ref_src',
  'ref_url',
  'igshid',
  'spm',
  'yclid',
  'msclkid',
  '_ga',
]);
// Non-HTML asset extensions to drop from discovery candidates.
const ASSET_EXT_RE =
  /\.(?:jpg|jpeg|png|gif|webp|avif|svg|ico|css|js|mjs|json|jsonld|webmanifest|xml|pdf|zip|gz|rar|7z|mp4|webm|mov|mp3|wav|woff2?|ttf|otf|eot|map|txt|csv|rss|atom)$/i;

// ─── F1: URL normalization ──────────────────────────────────────────────────

// Parses the pasted input into { url, origin, domain }. Prepends https:// when
// the scheme is missing. Throws on a non-http(s) scheme or a blocked host.
export function normalizeInputUrl(raw: unknown): { url: string; origin: string; domain: string } {
  if (typeof raw !== 'string' || !raw.trim()) throw new Error('Invalid URL');
  let candidate = raw.trim();
  if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(candidate)) candidate = `https://${candidate}`;
  const u = assertSafeUrl(candidate); // throws on non-http(s) / blocked host
  return { url: u.toString(), origin: `${u.protocol}//${u.host}`, domain: u.hostname };
}

// Canonical dedup key for a candidate URL, or null to discard. Same-origin only;
// strips fragment, tracking params, trailing slash (except root); lowercases host.
// `base` is used both to resolve relative hrefs AND as the same-origin reference;
// it may be a bare origin ("https://x.com") or a full URL ("https://x.com/page").
export function normalizeUrl(raw: string, base: string): string | null {
  let u: URL, b: URL;
  try {
    b = new URL(base);
    u = new URL(raw, base);
  } catch {
    return null;
  }
  if (u.protocol !== 'http:' && u.protocol !== 'https:') return null;
  if (u.host !== b.host || u.protocol !== b.protocol) return null; // same-origin
  if (ASSET_EXT_RE.test(u.pathname)) return null;
  u.hash = '';
  // Drop tracking params (utm_* + the explicit set); sort the rest for stability.
  const keep: [string, string][] = [];
  for (const [k, v] of u.searchParams.entries()) {
    const key = k.toLowerCase();
    if (key.startsWith('utm_') || TRACKING_PARAMS.has(key)) continue;
    keep.push([k, v]);
  }
  keep.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  u.search = '';
  for (const [k, v] of keep) u.searchParams.append(k, v);
  // Collapse duplicate slashes; drop trailing slash except for root.
  let pathname = u.pathname.replace(/\/{2,}/g, '/');
  if (pathname.length > 1) pathname = pathname.replace(/\/+$/, '');
  u.pathname = pathname || '/';
  u.hostname = u.hostname.toLowerCase();
  return u.toString();
}

// Same-origin absolute URL string for a (possibly relative) href, WITHOUT the
// asset-extension filtering normalizeUrl applies. Used for sitemap locations,
// which legitimately end in .xml / .xml.gz.
export function sameOriginUrl(raw: string, base: string): string | null {
  let u: URL, b: URL;
  try {
    b = new URL(base);
    u = new URL(raw, base);
  } catch {
    return null;
  }
  if (u.protocol !== 'http:' && u.protocol !== 'https:') return null;
  if (u.host !== b.host || u.protocol !== b.protocol) return null;
  u.hash = '';
  return u.toString();
}

// ─── F1: path ranking ─────────────────────────────────────────────────────────

const LOCALE_SEG_RE = /^[a-z]{2}(?:-[a-z]{2})?$/i;

// The leading locale segment of a URL ("it" for /it/works), lowercased, or null.
function localeSegment(urlStr: string): string | null {
  try {
    const seg = new URL(urlStr).pathname.split('/').filter(Boolean)[0] || '';
    return LOCALE_SEG_RE.test(seg) ? seg.toLowerCase() : null;
  } catch {
    return null;
  }
}

// Bilingual IT/EN keyword → template buckets. Order matters: first match wins.
const PATH_RULES: { hint: string; score: number; re: RegExp }[] = [
  {
    hint: 'about',
    score: 80,
    re: /(?:^|\/)(?:about|about-us|chi-siamo|chisiamo|studio|team|agenzia|company)(?:$|\/|-)/i,
  },
  {
    hint: 'work',
    score: 75,
    re: /(?:^|\/)(?:work|works|portfolio|projects|progetti|lavori|cases|showcase)(?:$|\/|-)/i,
  },
  {
    hint: 'case-study',
    score: 75,
    re: /(?:^|\/)(?:case-stud(?:y|ies)|case-histor(?:y|ies)|casi)(?:$|\/|-)/i,
  },
  { hint: 'pricing', score: 70, re: /(?:^|\/)(?:pricing|prezzi|plans|piani|costi)(?:$|\/|-)/i },
  {
    hint: 'other',
    score: 65,
    re: /(?:^|\/)(?:services|servizi|what-we-do|cosa-facciamo|solutions|soluzioni)(?:$|\/|-)/i,
  },
  {
    hint: 'contact',
    score: 60,
    re: /(?:^|\/)(?:contact|contacts|contatti|contattaci|get-in-touch)(?:$|\/|-)/i,
  },
  {
    hint: 'blog',
    score: 40,
    re: /(?:^|\/)(?:blog|news|journal|articles|articoli|magazine|insights)(?:$|\/|-)/i,
  },
];

// Score a pathname → { score, templateHint }. Root is always the home (100).
export function scorePath(
  pathname: string,
  homeLocale: string | null,
): { score: number; templateHint: string } {
  const p = (pathname || '/').toLowerCase();
  if (p === '/' || p === '') return { score: 100, templateHint: 'home' };
  let segs = p.split('/').filter(Boolean);
  if (homeLocale && segs[0] === homeLocale) segs = segs.slice(1);
  if (!segs.length) return { score: 35, templateHint: 'other' };
  const section = segs[0];
  const depth = segs.length;
  for (const rule of PATH_RULES) {
    if (rule.re.test('/' + section)) {
      return { score: Math.max(20, rule.score - (depth - 1) * 8), templateHint: rule.hint };
    }
  }
  let score = 30 - (depth - 1) * 5;
  if (depth === 1) score += 5; // short paths look like index pages
  const last = segs[segs.length - 1] || '';
  if (/\d{3,}/.test(last) || last.length > 40) score -= 10; // article/detail slug
  return { score: Math.max(1, score), templateHint: 'other' };
}

function pathDepth(urlStr: string): number {
  try {
    return new URL(urlStr).pathname.split('/').filter(Boolean).length;
  } catch {
    return 99;
  }
}

// "Type" of a page for dedup = its top-level path SECTION (first segment).
function sectionKey(urlStr: string, homeLocale: string | null): string {
  try {
    let segs = new URL(urlStr).pathname.split('/').filter(Boolean);
    if (homeLocale && segs[0] && segs[0].toLowerCase() === homeLocale) segs = segs.slice(1);
    return segs[0] ? segs[0].toLowerCase() : 'home';
  } catch {
    return 'home';
  }
}
// At most this many pages per section (the section index + one representative
// detail); the home section is capped to 1.
const PER_SECTION_CAP = 2;

// A representative page candidate: a normalized URL plus its ranking metadata.
export interface PageCandidate {
  url: string;
  score: number;
  templateHint: string;
  isHome?: boolean;
}

// Normalize + rank + dedup-PER-SECTION + cap. The home is always pages[0].
export function selectRepresentative(
  rawUrls: string[],
  origin: string,
  finalUrl: string,
  maxPages: number,
): PageCandidate[] {
  const homeUrl = normalizeUrl(finalUrl, origin) || origin;
  const homeLocale = localeSegment(homeUrl); // "it" for /it/, null for a bare root
  const seen = new Set<string>();
  const candidates: PageCandidate[] = [];
  seen.add(homeUrl);
  candidates.push({ url: homeUrl, score: 100, templateHint: 'home', isHome: true });

  for (const raw of rawUrls) {
    const norm = normalizeUrl(raw, origin);
    if (!norm || seen.has(norm)) continue;
    seen.add(norm);
    if (homeLocale) {
      const loc = localeSegment(norm);
      if (loc && loc !== homeLocale) continue;
    }
    candidates.push({ url: norm, ...scorePath(new URL(norm).pathname, homeLocale) });
  }

  const byHomeThenScore = (a: PageCandidate, b: PageCandidate): number =>
    (b.isHome ? 1 : 0) - (a.isHome ? 1 : 0) ||
    b.score - a.score ||
    pathDepth(a.url) - pathDepth(b.url);
  candidates.sort(byHomeThenScore);

  const perSection = new Map<string, number>();
  const out: PageCandidate[] = [];
  for (const c of candidates) {
    if (out.length >= maxPages) break;
    const sec = sectionKey(c.url, homeLocale);
    const cap = sec === 'home' ? 1 : PER_SECTION_CAP;
    const used = perSection.get(sec) || 0;
    if (used >= cap) continue;
    perSection.set(sec, used + 1);
    out.push(c);
  }
  out.sort(byHomeThenScore);
  return out;
}

// ─── F1: fetch + sitemap + crawl ──────────────────────────────────────────────

export interface FetchResult {
  buffer: Buffer;
  text: string | null;
  headers: Record<string, string>;
  finalUrl: string;
}

// GET with UA + timeout + abort, following redirects MANUALLY so that EVERY hop
// is validated against the SSRF blocklist, capping the body size. `binary:true`
// returns a Buffer (for .gz sitemaps).
export async function fetchText(
  url: string,
  {
    signal,
    accept,
    binary = false,
  }: { signal?: AbortSignal; accept?: string; binary?: boolean } = {},
): Promise<FetchResult> {
  const ac = new AbortController();
  const onAbort = (): void => ac.abort();
  if (signal) {
    if (signal.aborted) ac.abort();
    else signal.addEventListener('abort', onAbort, { once: true });
  }
  const timer = setTimeout(() => ac.abort(), FETCH_TIMEOUT_MS);
  try {
    let currentUrl = url;
    let res: Response;
    for (let hop = 0; ; hop++) {
      res = await fetch(currentUrl, {
        redirect: 'manual',
        signal: ac.signal,
        headers: { 'User-Agent': DESKTOP_UA, ...(accept ? { Accept: accept } : {}) },
      });
      if (res.status < 300 || res.status >= 400) break; // not a redirect → done
      const location = res.headers.get('location');
      if (!location) break; // 3xx without Location → treat as final (fails !res.ok below)
      try {
        await res.body?.cancel();
      } catch {}
      if (hop >= FETCH_MAX_REDIRECTS) throw new Error(`Too many redirects for ${url}`);
      // assertSafeUrl throws on a non-http(s) scheme or a blocked (internal) host.
      currentUrl = assertSafeUrl(new URL(location, currentUrl).toString()).toString();
    }
    const finalUrl = res.url || currentUrl;
    const finalHost = new URL(finalUrl).hostname;
    if (isBlockedHostname(finalHost)) throw new Error(`Blocked redirect host: ${finalHost}`);
    if (!res.ok) throw new Error(`HTTP ${res.status} for ${url}`);

    const headersEmpty: Record<string, string> = {};
    res.headers.forEach((v, k) => {
      headersEmpty[k] = v;
    });
    if (!res.body) {
      return { buffer: Buffer.alloc(0), text: binary ? null : '', headers: headersEmpty, finalUrl };
    }

    const reader = res.body.getReader();
    const chunks: Uint8Array[] = [];
    let received = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value);
      received += value.length;
      if (received > FETCH_MAX_BYTES) {
        try {
          await reader.cancel();
        } catch {}
        break;
      }
    }
    const buf = Buffer.concat(chunks.map((c) => Buffer.from(c)));
    const headers: Record<string, string> = {};
    res.headers.forEach((v, k) => {
      headers[k] = v;
    });
    return { buffer: buf, text: binary ? null : buf.toString('utf8'), headers, finalUrl };
  } finally {
    clearTimeout(timer);
    if (signal) signal.removeEventListener('abort', onAbort);
  }
}

interface DiscoverCtx {
  origin: string;
  domain: string;
  signal?: AbortSignal;
  rawUrls: string[];
  seenSitemaps: Set<string>;
  seenSitemapUrls: Set<string>;
}

// robots.txt → declared `Sitemap:` URLs (same-origin after normalize). Falls
// back to the conventional /sitemap.xml. Best-effort; never throws.
async function fetchRobotsSitemaps(origin: string, ctx: DiscoverCtx): Promise<string[]> {
  const sitemaps: string[] = [];
  try {
    const { text } = await fetchText(`${origin}/robots.txt`, {
      signal: ctx.signal,
      accept: 'text/plain',
    });
    const re = /^\s*sitemap:\s*(\S+)\s*$/gim;
    let m: RegExpExecArray | null;
    while ((m = re.exec(text || ''))) {
      const norm = sameOriginUrl(m[1], origin);
      if (norm) sitemaps.push(norm);
    }
  } catch {
    /* no robots.txt — fall through */
  }
  if (!sitemaps.length) sitemaps.push(`${origin}/sitemap.xml`);
  return [...new Set(sitemaps)];
}

// Extract <loc>…</loc> entries from sitemap XML. Returns { isIndex, locs }.
export function parseSitemapXml(xml: string): { isIndex: boolean; locs: string[] } {
  const isIndex = /<sitemapindex[\s>]/i.test(xml);
  const locs: string[] = [];
  const re = /<loc>\s*([^<]+?)\s*<\/loc>/gi;
  let m: RegExpExecArray | null;
  while ((m = re.exec(xml))) {
    const v = m[1].trim().replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>');
    if (v) locs.push(v);
  }
  return { isIndex, locs };
}

// Fetch + parse one sitemap, recursing into a sitemapindex (capped). Robust to
// malformed XML / gzip; never throws to the caller.
async function parseSitemap(sitemapUrl: string, ctx: DiscoverCtx, depth = 0): Promise<void> {
  if (depth > SITEMAP_DEPTH_CAP) return;
  if (ctx.rawUrls.length >= RAW_URL_CAP) return;
  if (ctx.seenSitemaps.has(sitemapUrl)) return;
  ctx.seenSitemaps.add(sitemapUrl);
  let xml: string;
  try {
    const { buffer, headers } = await fetchText(sitemapUrl, {
      signal: ctx.signal,
      accept: 'application/xml',
      binary: true,
    });
    const isGz =
      /\.gz($|\?)/i.test(sitemapUrl) ||
      /gzip/i.test(headers['content-type'] || '') ||
      (buffer.length > 2 && buffer[0] === 0x1f && buffer[1] === 0x8b); // gzip magic
    xml = (
      isGz ? await gunzipAsync(buffer, { maxOutputLength: GUNZIP_MAX_BYTES }) : buffer
    ).toString('utf8');
  } catch {
    return;
  }

  const { isIndex, locs } = parseSitemapXml(xml);
  if (isIndex) {
    let visited = 0;
    for (const loc of locs) {
      if (visited >= SITEMAP_CHILD_CAP || ctx.rawUrls.length >= RAW_URL_CAP) break;
      const child = sameOriginUrl(loc, ctx.origin);
      if (!child) continue;
      visited++;
      await parseSitemap(child, ctx, depth + 1);
    }
    return;
  }
  for (const loc of locs) {
    if (ctx.rawUrls.length >= RAW_URL_CAP) break;
    const norm = normalizeUrl(loc, ctx.origin);
    if (norm && !ctx.seenSitemapUrls.has(norm)) {
      ctx.seenSitemapUrls.add(norm);
      ctx.rawUrls.push(norm);
    }
  }
}

// Minimal same-origin link extraction from the home HTML (regex on href).
export function extractLinks(html: string, baseUrl: string, origin: string): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  const re = /href\s*=\s*(?:"([^"]*)"|'([^']*)')/gi;
  let m: RegExpExecArray | null;
  while ((m = re.exec(html))) {
    const href = (m[1] || m[2] || '').trim();
    if (!href || /^(?:mailto:|tel:|javascript:|data:|#)/i.test(href)) continue;
    const norm = normalizeUrl(href, baseUrl);
    if (norm && `${new URL(norm).protocol}//${new URL(norm).host}` === origin && !seen.has(norm)) {
      seen.add(norm);
      out.push(norm);
    }
  }
  return out;
}

// Result of discoverPages(): the resolved entry plus the representative pages.
export interface DiscoverResult {
  finalUrl: string;
  domain: string;
  origin: string;
  source: string;
  pages: PageCandidate[];
}

// F1 entry point. Discover representative pages for a pasted URL.
export async function discoverPages(
  url: string,
  { maxPages = 6, signal }: { maxPages?: number; signal?: AbortSignal } = {},
): Promise<DiscoverResult> {
  throwIfAborted(signal);
  const cap = Math.max(1, Math.min(MAX_PAGES_HARD, Math.floor(Number(maxPages)) || 6));
  const { origin: seedOrigin, domain: seedDomain } = normalizeInputUrl(url);

  let finalUrl = `${seedOrigin}/`;
  let origin = seedOrigin;
  let domain = seedDomain;
  let homeHtml: string | null = null;
  let homeContentType = '';
  try {
    const res = await fetchText(seedOrigin, { signal, accept: 'text/html,application/xhtml+xml' });
    finalUrl = res.finalUrl || finalUrl;
    const fu = new URL(finalUrl);
    origin = `${fu.protocol}//${fu.host}`;
    domain = fu.hostname;
    homeContentType = res.headers['content-type'] || '';
    if (/text\/html|application\/xhtml/i.test(homeContentType)) homeHtml = res.text;
  } catch {
    if (seedOrigin.startsWith('https://')) {
      try {
        const httpOrigin = seedOrigin.replace('https://', 'http://');
        const res = await fetchText(httpOrigin, { signal, accept: 'text/html' });
        finalUrl = res.finalUrl || `${httpOrigin}/`;
        const fu = new URL(finalUrl);
        origin = `${fu.protocol}//${fu.host}`;
        domain = fu.hostname;
        if (/text\/html|application\/xhtml/i.test(res.headers['content-type'] || ''))
          homeHtml = res.text;
      } catch {
        /* still dead → seed-only below */
      }
    }
  }

  throwIfAborted(signal);

  const ctx: DiscoverCtx = {
    origin,
    domain,
    signal,
    rawUrls: [],
    seenSitemaps: new Set(),
    seenSitemapUrls: new Set(),
  };

  let source = 'seed-only';
  try {
    const sitemaps = await fetchRobotsSitemaps(origin, ctx);
    for (const sm of sitemaps) {
      if (ctx.rawUrls.length >= RAW_URL_CAP) break;
      await parseSitemap(sm, ctx, 0);
    }
  } catch {
    /* ignore */
  }
  if (ctx.rawUrls.length >= 2) source = 'sitemap';

  if (ctx.rawUrls.length < 2 && homeHtml) {
    const links = extractLinks(homeHtml, finalUrl, origin);
    if (links.length) {
      for (const l of links) if (!ctx.rawUrls.includes(l)) ctx.rawUrls.push(l);
      if (links.length >= 1) source = 'crawl';
    }
  }

  const pages = selectRepresentative(ctx.rawUrls, origin, finalUrl, cap);
  if (pages.length <= 1) source = 'seed-only';

  return { finalUrl, domain, origin, source, pages };
}

// ─── F2: storage / ffmpeg ──────────────────────────────────────────────────────

export function getCaptureDir(): string {
  return path.join(app.getPath('userData'), 'assets', 'web');
}

// Per-URL path under <userData>/assets/web/. An optional `stamp` (the capture
// epoch) is prefixed so re-captures of the SAME url write distinct files. The
// name is hashed from the URL, never raw user input.
export function screenshotPathForUrl(finalUrl: string, format = 'webp', stamp?: number): string {
  let host = 'site';
  try {
    host = new URL(finalUrl).hostname.replace(/[^a-z0-9.-]/gi, '_').slice(0, 40);
  } catch {}
  const key = createHash('sha256').update(String(finalUrl)).digest('hex').slice(0, 16);
  const prefix = stamp ? `${stamp}-` : '';
  return path.join(getCaptureDir(), `${prefix}${host}-${key}.${format}`);
}

// Reject paths that ffmpeg would misread as an option flag; resolve to absolute.
export function safeInputPath(p: string): string {
  if (typeof p !== 'string' || !p.trim()) throw new Error('Invalid media path');
  const abs = path.resolve(p);
  if (path.basename(abs).startsWith('-')) throw new Error(`Refusing unsafe media path: ${p}`);
  return abs;
}

interface SpawnResult {
  code: number | null;
  stderr: string;
}

export function spawnAsync(
  bin: string,
  args: string[],
  signal?: AbortSignal,
): Promise<SpawnResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { stdio: ['ignore', 'ignore', 'pipe'] });
    let stderr = '';
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (d: string) => {
      stderr += d;
    });
    const onAbort = (): void => {
      try {
        child.kill('SIGKILL');
      } catch {}
      reject(Object.assign(new Error('AbortError'), { name: 'AbortError' }));
    };
    const settle = <V>(fn: (v: V) => void, v: V): void => {
      if (signal) signal.removeEventListener('abort', onAbort);
      fn(v);
    };
    if (signal) {
      if (signal.aborted) return onAbort();
      signal.addEventListener('abort', onAbort, { once: true });
    }
    child.on('error', (err: Error) => settle(reject, err));
    child.on('close', (code: number | null) => settle(resolve, { code, stderr }));
  });
}

// PNG → WebP/PNG via ffmpeg. Output naming derives from a hash, never raw user
// input. `-protocol_whitelist file` blocks crafted remote-protocol paths.
export async function encodeImage(
  srcPng: string,
  outPath: string,
  format: string,
  quality: number,
  signal?: AbortSignal,
): Promise<void> {
  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  if (format === 'png') {
    fs.renameSync(srcPng, outPath);
    return;
  }
  const ffmpeg = resolveFfmpeg();
  await spawnAsync(
    ffmpeg,
    [
      '-hide_banner',
      '-protocol_whitelist',
      'file',
      '-i',
      safeInputPath(srcPng),
      '-vf',
      `scale='min(iw,${WEBP_MAX_WIDTH})':-2`,
      '-threads',
      '1',
      '-frames:v',
      '1',
      '-c:v',
      'libwebp',
      '-quality',
      String(quality),
      '-y',
      outPath,
    ],
    signal,
  );
  if (!fs.existsSync(outPath)) throw new Error('WebP encode produced no output');
}

// Part A — og:image fallback. Download a page's social-preview image, SSRF-gated
// and size-capped, and encode it to a WebP under the capture-dir layout. Returns
// the WebP path, or null on any failure (never throws).
export async function fetchImageToWebp(
  imageUrl: string | null | undefined,
  {
    pageUrl,
    stamp,
    format = 'webp',
    quality = 82,
    signal,
  }: {
    pageUrl?: string;
    stamp?: number;
    format?: string;
    quality?: number;
    signal?: AbortSignal;
  } = {},
): Promise<string | null> {
  if (!imageUrl) return null;
  let abs: string;
  try {
    abs = new URL(imageUrl, pageUrl || undefined).toString();
  } catch {
    return null;
  }
  let safe: URL;
  try {
    safe = assertSafeUrl(abs); // throws on non-http(s) / blocked host
  } catch {
    return null;
  }
  let tmpDir: string | null = null;
  try {
    const { buffer } = await fetchText(safe.toString(), {
      signal,
      accept: 'image/*',
      binary: true,
    });
    if (!buffer || buffer.length < 64) return null;
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-og-'));
    const tmpIn = path.join(tmpDir, 'og-src');
    fs.writeFileSync(tmpIn, buffer);
    const outPath = screenshotPathForUrl(`${abs}#og`, format, stamp);
    await encodeImage(tmpIn, outPath, format, quality, signal);
    return fs.existsSync(outPath) ? outPath : null;
  } catch {
    return null;
  } finally {
    if (tmpDir) {
      try {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      } catch {}
    }
  }
}

// Resolve the ffmpeg binary: env override (FFMPEG_BIN, the capture service's
// Debian ffmpeg), then the desktop's bundled runtime-bin / resources / static
// locations, then PATH.
export function resolveFfmpeg(): string {
  const exe = process.platform === 'win32' ? 'ffmpeg.exe' : 'ffmpeg';
  const candidates = [
    process.env.FFMPEG_BIN,
    path.join(app.getPath('userData'), 'runtime-bin', 'bin', exe),
    path.join(process.resourcesPath || '', 'bin', exe),
    path.join(__dirname, '..', 'bin', exe),
  ];
  for (const p of candidates) {
    if (p && fs.existsSync(p)) return p;
  }
  let staticPath: string | null = null;
  try {
    staticPath = require('ffmpeg-static');
  } catch {}
  if (staticPath && staticPath.includes('app.asar') && !staticPath.includes('app.asar.unpacked')) {
    staticPath = staticPath.replace('app.asar', 'app.asar.unpacked');
  }
  for (const p of [
    staticPath,
    '/opt/homebrew/bin/ffmpeg',
    '/usr/bin/ffmpeg',
    '/usr/local/bin/ffmpeg',
  ]) {
    if (p && fs.existsSync(p)) return p;
  }
  return 'ffmpeg';
}
