'use strict';

// Web reference enrichment (POC "siti web come reference").
//
// THREE independent, side-effect-free feature groups live here, sharing no
// mutable state:
//   • F3 — content extraction: HTML/DOM → { title, meta, og, twitter, jsonld,
//     headings, mainText, lang } + site-level aggregation + prompt-injection
//     sanitization.
//   • F4 — deterministic metadata: palette / fonts / tech-stack from the LIVE
//     page (via pageCtx.evaluate) + an ffmpeg palettegen fallback on the
//     screenshot. No VLM.
//   • F7 — awards lookup: detect award badges/self-links the site exposes in its
//     own HTML (zero network), mapped to searchable tags/entities.
//
// CommonJS, like the rest of electron/*. F3/F7 are pure functions over strings.
// F4 touches the live page through the flat `pageCtx` handle from F2 and, only
// for the palette fallback, spawns ffmpeg (path resolved exactly like
// analyzer.js does). NO new npm dependencies: cheerio is absent and jsdom is a
// devDependency only, so HTML parsing is minimal regex/string work.

import fs from 'fs';
import os from 'os';
import path from 'path';
import { spawn } from 'child_process';

// ─── Local shapes (file-internal; domain types stay in types/domain.d.ts) ────

// A flat <meta> tag pair as produced by extractMetaTags.
interface MetaTag {
  key: string;
  content: string;
}

// Headings grouped per level, keyed h1/h2/h3 (string-indexable for `h${level}`).
interface Headings {
  h1: string[];
  h2: string[];
  h3: string[];
  [level: string]: string[];
}

// og / twitter meta maps (open-ended keys, all string values).
type MetaMap = Record<string, string>;

// A single page's extracted content (output of extractContent / asPageContent).
interface PageContent {
  url?: string;
  title?: string;
  metaDescription?: string;
  og?: MetaMap;
  twitter?: MetaMap;
  jsonld?: JsonLdNode[];
  headings?: Partial<Headings>;
  mainText?: string;
  textLength?: number;
  truncated?: boolean;
  lang?: string;
}

// Wrapper shape some callers pass: { title, content: PageContent }.
interface PageContentWrapper {
  title?: string;
  content?: PageContent;
}

// A parsed JSON-LD object (open-ended; only allowlisted text fields are read).
type JsonLdNode = Record<string, unknown>;

// Result of extractMainText.
interface MainText {
  text: string;
  length: number;
  truncated: boolean;
}

// Aggregated site-level output of aggregateSiteText.
interface AggregateResult {
  contentText: string;
  lang: string;
  webMeta: WebMetaOut;
}

interface WebMetaOut {
  siteName: string;
  title: string;
  description: string;
  lang: string;
  ogImage: string;
  pageCount: number;
  jsonldTypes: string[];
  entities: string[];
}

// A parsed CSS color with alpha.
interface RgbaColor {
  r: number;
  g: number;
  b: number;
  a: number;
}

// One palette swatch (F4 output).
interface PaletteColor {
  hex: string;
  role: string;
  weight: number;
}

// One computed-style sample harvested from the live page by PALETTE_EVAL_SRC.
interface PaletteSample {
  color: string;
  kind: string;
  area: number;
}

// A distinct RGB pixel color counted from the palettegen PNG.
interface PixelColor {
  r: number;
  g: number;
  b: number;
  n: number;
}

// One detected font (F4 output).
interface FontInfo {
  family: string;
  usage: string;
  provider: string;
}

// Raw font signals harvested from the live page by FONTS_EVAL_SRC.
interface FontEvalResult {
  heading?: string;
  body?: string;
  mono?: string;
  loaded?: string[];
  links?: string[];
  error?: string;
}

// Context fed to fontProvider() to attribute a family to a source.
interface FontProviderCtx {
  links: string[];
  html: string;
}

// Runtime window.* probe result harvested by TECH_RUNTIME_EVAL_SRC.
type TechRuntime = Record<string, unknown>;

// Context fed to each TECH_RULES.test().
interface TechCtx {
  html: string;
  headers: Record<string, string>;
  runtime: TechRuntime;
  generator: string;
  poweredBy: string;
  server: string;
}

interface TechRule {
  name: string;
  test: (c: TechCtx) => boolean;
}

// The flat pageCtx handle from F2 (webcapture). F4 only uses this subset; all
// fields are optional because every extractor degrades gracefully on absence.
interface PageCtx {
  evaluate?: (code: string) => Promise<unknown>;
  screenshotPath?: string;
  html?: string;
  headers?: Record<string, unknown>;
}

// Evidence strength for an award hit (F7).
type AwardEvidence = 'self-link' | 'badge-img' | 'badge-script' | 'text-only';

// One detector entry in AWARD_DETECTORS.
interface AwardDetector {
  platform: string;
  hosts: string[];
  entryPath: RegExp;
  badge: RegExp;
  name: string;
  levels: Array<[RegExp, string]>;
}

// A detected award (F7 output).
interface Award {
  platform: string;
  level?: string;
  date?: string;
  profileUrl?: string;
  evidence: AwardEvidence;
  confidence: number;
}

// A page as accepted by detectAwards (or a bare HTML string).
interface AwardPage {
  url?: string;
  html: string;
}

// Parsed <a> tag (F7).
interface Anchor {
  href: string;
  text: string;
  title: string;
  ariaLabel: string;
}

// Parsed <img> tag (F7).
interface ImgTag {
  src: string;
  alt: string;
}

// ─── Caps (aligned with analyzer.js CAPTION_MAX = 1200) ──────────────────────
const MAIN_TEXT_CAP = 2000; // per-page mainText cap (before aggregation)
const AGGREGATE_CAP = 1200; // site contentText cap → stays within CAPTION_MAX
const HEADING_CAP = 12; // max headings kept per level
const SANITIZE_HARD_CAP = 20000; // absolute ceiling for any single sanitized blob
// Absolute ceiling on the raw HTML any synchronous regex pass may touch. The
// captured DOM (document.documentElement.outerHTML) has NO size cap upstream, so
// without this a hostile/oversized page can drive O(n²) regex backtracking that
// blocks the Electron main process (the cooperative AbortSignal in the
// orchestrator cannot interrupt a synchronous regex). 4 MB is far above any real
// page's meaningful text while bounding the worst case to a few ms.
const MAX_HTML = 4 * 1024 * 1024; // 4 MB
// Bound the per-tag attribute scan so `<tag\b[^>]*>` cannot backtrack across a
// long run of '<' (or '<tag') prefixes. The bound must still fit REAL attribute
// payloads — meta descriptions, og/twitter:image URLs, long hrefs/srcs routinely
// exceed a few hundred bytes — so a too-small value silently drops legitimate
// tags. 16 KB accommodates real tags while keeping the worst case linear
// (positions × ATTR_SCAN stays in the low-hundreds-of-ms on a 4 MB adversarial input).
const ATTR_SCAN = 16 * 1024;

// JSON-LD @type allowlist (purpose/industry-oriented). Everything else is noise.
const JSONLD_TYPE_ALLOW = new Set([
  'organization',
  'product',
  'service',
  'article',
  'newsarticle',
  'blogposting',
  'website',
  'recipe',
  'event',
  'localbusiness',
  'softwareapplication',
  'course',
  'person',
  'breadcrumblist',
]);
// Text-only fields lifted from an allowed JSON-LD block (never the raw object).
const JSONLD_TEXT_FIELDS = [
  'name',
  'description',
  'headline',
  'brand',
  'category',
  'applicationCategory',
  'articleSection',
];

// ════════════════════════════════════════════════════════════════════════════
//  SHARED helpers
// ════════════════════════════════════════════════════════════════════════════

// Minimal HTML-entity decode for the regex fallback path (the live DOM is
// already decoded). Covers the common named entities + numeric refs.
const NAMED_ENTITIES: Record<string, string> = {
  amp: '&',
  lt: '<',
  gt: '>',
  quot: '"',
  apos: "'",
  nbsp: ' ',
  copy: '©',
  reg: '®',
  trade: '™',
  hellip: '…',
  mdash: '—',
  ndash: '–',
  rsquo: '’',
  lsquo: '‘',
  rdquo: '”',
  ldquo: '“',
  deg: '°',
  euro: '€',
  pound: '£',
  laquo: '«',
  raquo: '»',
};
function decodeEntities(s: string): string {
  if (typeof s !== 'string' || s.indexOf('&') === -1) return s || '';
  return s.replace(/&(#x?[0-9a-f]+|[a-z]+);/gi, (m, body: string) => {
    if (body[0] === '#') {
      const cp =
        body[1] === 'x' || body[1] === 'X'
          ? parseInt(body.slice(2), 16)
          : parseInt(body.slice(1), 10);
      return Number.isFinite(cp) && cp > 0 && cp <= 0x10ffff ? String.fromCodePoint(cp) : m;
    }
    const k = body.toLowerCase();
    return Object.prototype.hasOwnProperty.call(NAMED_ENTITIES, k) ? NAMED_ENTITIES[k] : m;
  });
}

// Collapse whitespace (incl. NBSP) into single spaces and trim.
function collapseWs(s: unknown): string {
  return String(s == null ? '' : s)
    .replace(/[\s ]+/g, ' ')
    .trim();
}

// Strip ALL HTML tags, keeping text. Comments and CDATA removed first.
// The tag removal is a single LINEAR scan (not `<[^>]*>`), so a long run of '<'
// (or '<tag') chars in untrusted HTML cannot trigger O(n²) regex backtracking —
// each '<' without a closing '>' costs O(1), not O(remaining length).
function stripTags(html: unknown): string {
  const s = String(html == null ? '' : html)
    .replace(/<!--[\s\S]*?-->/g, ' ')
    .replace(/<!\[CDATA\[[\s\S]*?\]\]>/g, ' ');
  let out = '';
  let i = 0;
  const n = s.length;
  while (i < n) {
    const lt = s.indexOf('<', i);
    if (lt === -1) {
      out += s.slice(i);
      break;
    }
    out += s.slice(i, lt); // text before the tag
    const gt = s.indexOf('>', lt + 1); // matching close of this tag
    if (gt === -1) {
      out += ' ';
      break;
    } // unterminated '<…': drop the rest
    out += ' '; // the whole <…> becomes a space
    i = gt + 1;
  }
  return out;
}

// Remove whole elements (open→close) by tag name, case-insensitive, including
// self-contained content (scripts, styles, nav chrome…). Best-effort string op.
// Attribute scans are length-bounded to avoid quadratic backtracking on long
// runs of '<tag' prefixes in untrusted HTML.
function dropElements(html: unknown, tags: string[]): string {
  let out = String(html == null ? '' : html);
  for (const tag of tags) {
    out = out.replace(
      new RegExp(`<${tag}\\b[^>]{0,${ATTR_SCAN}}>[\\s\\S]*?<\\/${tag}>`, 'gi'),
      ' ',
    );
    // also drop self-closing / unclosed leftovers of void-ish chrome tags
    out = out.replace(new RegExp(`<${tag}\\b[^>]{0,${ATTR_SCAN}}\\/?>`, 'gi'), ' ');
  }
  return out;
}

// ════════════════════════════════════════════════════════════════════════════
//  SECURITY — untrusted-text sanitization (CRITICAL, F3 §4)
// ════════════════════════════════════════════════════════════════════════════
//
// Every string that ends up in `contentText` (which analyzer.js drops between
// its <<<CAPTION>>> … <<<FINE CAPTION>>> markers and feeds to the VLM as a
// `user` message) is content controlled by an arbitrary third-party site. A
// hostile page could embed "ignore previous instructions…" prose OR literally
// emit the closing marker to break out of the delimited block.
//
// sanitizeForPrompt() is the in-depth defense applied BEFORE the text leaves
// this module:
//   1. neutralize the analyzer's delimitation markers (`<<<`, `>>>`, and the
//      full `<<<CAPTION>>>` / `<<<FINE CAPTION>>>` forms) → a site cannot close
//      the block and inject "outside the markers".
//   2. strip residual HTML markup + decode entities (we emit data, not markup).
//   3. remove zero-width / control / bidi-override chars used to obfuscate.
//   4. collapse whitespace + hard length cap (a huge blob is itself an abuse
//      vector against the token budget).
// It never INTERPRETS the text — it only produces inert data. analyzer.js still
// re-delimits downstream; this is the second barrier, not a replacement.

const SANITIZE_DEFAULT_MAX = AGGREGATE_CAP;

// Neutralize the analyzer's delimitation markers so a hostile page can neither
// close the <<<CAPTION>>> block nor inject its named forms.
function neutralizeMarkers(s: unknown): string {
  return String(s == null ? '' : s)
    .replace(/<<<\s*\/?\s*FINE\s+CAPTION\s*>>>/gi, ' ')
    .replace(/<<<\s*CAPTION\s*>>>/gi, ' ')
    .replace(/<{3,}/g, '‹') // innocuous look-alike for residual angle-triples
    .replace(/>{3,}/g, '›');
}

function sanitizeForPrompt(text: unknown, opts: { maxChars?: number } = {}): string {
  const maxChars = Math.max(1, Math.min(SANITIZE_HARD_CAP, opts.maxChars || SANITIZE_DEFAULT_MAX));
  let s = String(text == null ? '' : text);

  // (1) neutralize delimitation markers FIRST — before tag stripping, because
  // stripTags' <[^>]+> would otherwise greedily consume "<<CAPTION>" and leave
  // a dangling ">>" run. Named forms first, then residual angle-triple runs.
  s = neutralizeMarkers(s);

  // (2) strip markup + decode entities (decode AFTER strip so an encoded tag
  // can't reconstitute into live markup), then re-run the marker pass in case
  // entity-decoding revealed an HTML-encoded marker (&lt;&lt;&lt;CAPTION…).
  s = stripTags(s);
  s = decodeEntities(s);
  s = neutralizeMarkers(s);

  // (3) remove zero-width, soft hyphen, BOM, bidi overrides, and C0/C1 control
  // chars (keep \t \n which collapseWs handles).
  s = s.replace(/[​-‏‪-‮⁠-⁤⁪-⁯﻿­]/g, '').replace(/[ ---]/g, ' ');

  // (4) collapse + cap on a word boundary.
  s = collapseWs(s);
  if (s.length > maxChars) {
    const cut = s.slice(0, maxChars);
    const lastSpace = cut.lastIndexOf(' ');
    s = (lastSpace > maxChars * 0.6 ? cut.slice(0, lastSpace) : cut).trimEnd() + '…';
  }
  return s;
}
// Back-compat alias used by the spec's helper name.
const sanitizeUntrustedText = (s: unknown, opts?: { maxChars?: number }): string =>
  sanitizeForPrompt(s, opts);

// ════════════════════════════════════════════════════════════════════════════
//  F3 — content extraction
// ════════════════════════════════════════════════════════════════════════════

// Pull all <meta> tags into a flat list of { key, content } where key is the
// lowercased name|property attribute.
function extractMetaTags(html: string): MetaTag[] {
  const out: MetaTag[] = [];
  const re = new RegExp(`<meta\\b[^>]{0,${ATTR_SCAN}}>`, 'gi');
  let m: RegExpExecArray | null;
  while ((m = re.exec(html))) {
    const tag = m[0];
    const keyM = tag.match(/\b(?:name|property|itemprop)\s*=\s*("([^"]*)"|'([^']*)'|([^\s>]+))/i);
    const contentM = tag.match(/\bcontent\s*=\s*("([^"]*)"|'([^']*)'|([^\s>]+))/i);
    if (!keyM || !contentM) continue;
    const key = (keyM[2] ?? keyM[3] ?? keyM[4] ?? '').trim().toLowerCase();
    const content = decodeEntities((contentM[2] ?? contentM[3] ?? contentM[4] ?? '').trim());
    if (key) out.push({ key, content });
  }
  return out;
}

function extractTitle(html: string): string {
  const m = html.match(new RegExp(`<title\\b[^>]{0,${ATTR_SCAN}}>([\\s\\S]*?)<\\/title>`, 'i'));
  return m ? collapseWs(decodeEntities(stripTags(m[1]))) : '';
}

function extractLang(html: string): string {
  const m = html.match(
    new RegExp(`<html\\b[^>]{0,${ATTR_SCAN}}\\blang\\s*=\\s*("([^"]*)"|'([^']*)'|([^\\s>]+))`, 'i'),
  );
  const raw = m ? (m[2] ?? m[3] ?? m[4] ?? '') : '';
  return (raw.split('-')[0] || '').trim().toLowerCase();
}

// Headings per level, cleaned + deduped + capped.
function extractHeadings(html: string): Headings {
  const out: Headings = { h1: [], h2: [], h3: [] };
  for (const level of [1, 2, 3]) {
    const re = new RegExp(`<h${level}\\b[^>]{0,${ATTR_SCAN}}>([\\s\\S]*?)<\\/h${level}>`, 'gi');
    const seen = new Set<string>();
    let m: RegExpExecArray | null;
    while ((m = re.exec(html)) && out[`h${level}`].length < HEADING_CAP) {
      const t = collapseWs(decodeEntities(stripTags(m[1])));
      if (t && !seen.has(t.toLowerCase())) {
        seen.add(t.toLowerCase());
        out[`h${level}`].push(t);
      }
    }
  }
  return out;
}

// JSON-LD blocks, filtered to the allowed @type set. Returns the parsed objects
// (raw, for storage/debug — NEVER fed to the prompt as-is, see §4).
function extractJsonLd(html: string): JsonLdNode[] {
  const out: JsonLdNode[] = [];
  const re = new RegExp(
    `<script\\b[^>]{0,${ATTR_SCAN}}type\\s*=\\s*("application/ld\\+json"|'application/ld\\+json'|application/ld\\+json)[^>]{0,${ATTR_SCAN}}>([\\s\\S]*?)<\\/script>`,
    'gi',
  );
  let m: RegExpExecArray | null;
  while ((m = re.exec(html))) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(m[2].trim());
    } catch {
      continue;
    }
    const blocks = Array.isArray(parsed) ? parsed : [parsed];
    for (const b of blocks) {
      if (!b || typeof b !== 'object') continue;
      const obj = b as JsonLdNode;
      // @graph holds an array of nodes — flatten one level.
      const nodes = Array.isArray(obj['@graph']) ? (obj['@graph'] as unknown[]) : [obj];
      for (const node of nodes) {
        if (node && typeof node === 'object' && jsonldTypeAllowed((node as JsonLdNode)['@type']))
          out.push(node as JsonLdNode);
      }
    }
  }
  return out;
}

function jsonldTypeAllowed(type: unknown): boolean {
  const types = Array.isArray(type) ? type : [type];
  return types.some((t) => typeof t === 'string' && JSONLD_TYPE_ALLOW.has(t.toLowerCase()));
}

// Allowlisted text fields from a JSON-LD node (for aggregation), each a string.
function jsonldTextFields(node: JsonLdNode): string[] {
  const out: string[] = [];
  for (const f of JSONLD_TEXT_FIELDS) {
    let v = node[f];
    if (v && typeof v === 'object') v = (v as JsonLdNode).name || (v as JsonLdNode)['@value'] || ''; // {name:'Acme'} brand shape
    if (typeof v === 'string' && v.trim()) out.push(v.trim());
  }
  return out;
}

// Readability-lite mainText: drop chrome, pick the densest text block, cap.
function extractMainText(html: unknown): MainText {
  // 0. hard-cap raw HTML before any regex pass (DoS guard, see MAX_HTML).
  const capped = String(html == null ? '' : html).slice(0, MAX_HTML);
  // 1. structural removal of script/style/chrome elements.
  const doc = dropElements(capped, [
    'script',
    'style',
    'noscript',
    'template',
    'svg',
    'nav',
    'header',
    'footer',
    'aside',
  ]);

  // 2. prefer <main>/<article>; else use the whole remaining body.
  const block =
    matchFirst(doc, new RegExp(`<main\\b[^>]{0,${ATTR_SCAN}}>([\\s\\S]*?)<\\/main>`, 'i')) ||
    matchFirst(doc, new RegExp(`<article\\b[^>]{0,${ATTR_SCAN}}>([\\s\\S]*?)<\\/article>`, 'i')) ||
    matchFirst(doc, new RegExp(`<body\\b[^>]{0,${ATTR_SCAN}}>([\\s\\S]*?)<\\/body>`, 'i')) ||
    doc;

  // 3. drop residual chrome-classed containers (cookie/consent/menu/etc.) by a
  //    coarse div-removal whose class/id looks like chrome. Best-effort.
  const chromeRe =
    /(^|[-_ ])(nav|menu|footer|header|cookie|consent|banner|sidebar|breadcrumb|share|social|newsletter|popup|modal)([-_ ]|$)/i;
  const cleaned = block.replace(
    new RegExp(`<(div|section|ul)\\b([^>]{0,${ATTR_SCAN}})>`, 'gi'),
    (full: string, tag: string, attrs: string) => {
      const cls =
        (attrs.match(/\b(?:class|id|role)\s*=\s*("([^"]*)"|'([^']*)')/i) || [])[2] ||
        (attrs.match(/\b(?:class|id|role)\s*=\s*('([^']*)')/i) || [])[2] ||
        '';
      return chromeRe.test(cls) || /aria-hidden\s*=\s*["']?true/i.test(attrs)
        ? `<${tag} data-chrome>`
        : full;
    },
  );
  // (the marker doesn't remove content, but de-prioritizes — for the POC the
  //  text strip below is the real reducer; keep it simple and just strip.)

  // 4. to text + normalize.
  let text = collapseWs(decodeEntities(stripTags(cleaned)));
  // drop lines that are clearly residual menu (handled implicitly via collapse).

  const length = text.length;
  let truncated = false;
  if (text.length > MAIN_TEXT_CAP) {
    const cut = text.slice(0, MAIN_TEXT_CAP);
    const sp = cut.lastIndexOf(' ');
    text = (sp > MAIN_TEXT_CAP * 0.6 ? cut.slice(0, sp) : cut) + '…';
    truncated = true;
  }
  return { text, length, truncated };
}

function matchFirst(html: string, re: RegExp): string | null {
  const m = html.match(re);
  return m ? m[1] : null;
}

// Build the og/twitter maps from a meta list.
function ogTwitterMaps(metas: MetaTag[]): { og: MetaMap; twitter: MetaMap } {
  const og: MetaMap = {};
  const twitter: MetaMap = {};
  for (const { key, content } of metas) {
    if (key.startsWith('og:')) {
      const k = key.slice(3);
      if (!og[k]) og[k] = content;
    } else if (key.startsWith('twitter:')) {
      const k = key.slice(8);
      if (!twitter[k]) twitter[k] = content;
    } else if (key === 'article:section' && !og.section) {
      og.section = content;
    }
  }
  return { og, twitter };
}

function metaByName(metas: MetaTag[], name: string): string {
  const hit = metas.find((m) => m.key === name && m.content);
  return hit ? hit.content : '';
}

/**
 * Extract structured content from ONE rendered page.
 * @param {string} html  rendered DOM (post-hydration) or raw HTML string.
 * @param {string} [url] canonical/final URL of the page.
 * @returns {{ url, title, metaDescription, og, twitter, jsonld, headings, mainText, textLength, truncated, lang }}
 */
function extractContent(
  html: unknown,
  url = '',
): Required<
  Pick<
    PageContent,
    | 'url'
    | 'title'
    | 'metaDescription'
    | 'og'
    | 'twitter'
    | 'jsonld'
    | 'headings'
    | 'mainText'
    | 'textLength'
    | 'truncated'
    | 'lang'
  >
> {
  // Hard-cap raw HTML before any regex pass (DoS guard, see MAX_HTML). The
  // captured DOM has no upstream size cap and all extraction here is synchronous.
  const safe = (typeof html === 'string' ? html : '').slice(0, MAX_HTML);
  const metas = extractMetaTags(safe);
  const { og, twitter } = ogTwitterMaps(metas);
  const headings = extractHeadings(safe);

  const rawTitle = extractTitle(safe);
  const title = rawTitle || og.title || twitter.title || headings.h1[0] || '';
  const metaDescription =
    metaByName(metas, 'description') || og.description || twitter.description || '';

  // lang: <html lang> → og:locale → empty (no heavy heuristic in the POC).
  let lang = extractLang(safe);
  if (!lang && og.locale) lang = (og.locale.split(/[-_]/)[0] || '').toLowerCase();

  const main = extractMainText(safe);
  const jsonld = extractJsonLd(safe);

  return {
    url: url || og.url || '',
    title: collapseWs(title),
    metaDescription: collapseWs(metaDescription),
    og,
    twitter,
    jsonld,
    headings,
    mainText: main.text,
    textLength: main.length,
    truncated: main.truncated,
    lang,
  };
}

// Accept either { title, content:{...} } or a bare PageContent.
function asPageContent(p: PageContent | PageContentWrapper | null | undefined): PageContent | null {
  if (!p || typeof p !== 'object') return null;
  const wrapper = p as PageContentWrapper;
  if (wrapper.content && typeof wrapper.content === 'object') {
    return { ...wrapper.content, title: wrapper.title || wrapper.content.title || '' };
  }
  return p as PageContent;
}

// Dominant language among pages (first non-empty wins by frequency).
function dominantLang(langs: Array<string | undefined>): string {
  const counts: Record<string, number> = {};
  for (const l of langs) {
    if (l) counts[l] = (counts[l] || 0) + 1;
  }
  let best = '';
  let n = 0;
  for (const [l, c] of Object.entries(counts)) {
    if (c > n) {
      n = c;
      best = l;
    }
  }
  return best;
}

/**
 * Aggregate N pages of one site into a single sanitized caption block + webMeta.
 * @param {Array} pages  PageContent[] (or [{ title, content }]). pages[0] = home.
 * @param {{ maxChars?: number }} [opts]
 * @returns {{ contentText: string, lang: string, webMeta: object }}
 */
function aggregateSiteText(
  pages: Array<PageContent | PageContentWrapper> | unknown,
  opts: { maxChars?: number } = {},
): AggregateResult {
  const maxChars = Math.max(1, Math.min(AGGREGATE_CAP, opts.maxChars || AGGREGATE_CAP));
  const list = (Array.isArray(pages) ? pages : [])
    .map(asPageContent)
    .filter((p): p is PageContent => Boolean(p));

  const home: PageContent = list[0] || {};
  const homeOg: MetaMap = home.og || {};

  // Collect, in precedence order, the most authoritative purpose/industry signal.
  // The home page contributes first (weighted by ordering, not by repetition).
  const parts: string[] = [];
  const seenText = new Set<string>(); // dedup identical normalized mainText across pages
  const jsonldTypes = new Set<string>();
  const entities: string[] = [];

  const pushPart = (s: string): void => {
    const c = collapseWs(s);
    if (c) parts.push(c);
  };

  // 1. siteName + title (home first)
  const siteName = homeOg.site_name || '';
  pushPart([siteName, home.title].filter(Boolean).join(' — '));
  // 2. description
  pushPart(home.metaDescription || '');

  // 3. JSON-LD allowlisted text fields + 4. headings + 5. mainText, per page
  //    (home weighted by being first).
  for (let i = 0; i < list.length; i++) {
    const p = list[i];
    for (const node of Array.isArray(p.jsonld) ? p.jsonld : []) {
      const types = Array.isArray(node['@type']) ? (node['@type'] as unknown[]) : [node['@type']];
      for (const t of types) if (typeof t === 'string') jsonldTypes.add(t);
      const txt = jsonldTextFields(node);
      if (txt.length) pushPart(txt.join(' · '));
      // Organization/Product/Brand names → entities
      const tlc = types.map((t) => String(t || '').toLowerCase());
      if (
        tlc.some((t) => t === 'organization' || t === 'product' || t === 'localbusiness') &&
        typeof node.name === 'string'
      ) {
        entities.push(node.name.trim());
      }
    }
    const h: Partial<Headings> = p.headings || {};
    pushPart([...(h.h1 || []), ...(h.h2 || [])].slice(0, 6).join(' · '));
    const mt = (p.mainText || '').trim();
    const key = mt.slice(0, 200).toLowerCase();
    if (mt && !seenText.has(key)) {
      seenText.add(key);
      pushPart(mt);
    }
  }

  // Join, then sanitize+cap once (sanitize neutralizes injection markers).
  const joined = parts.filter(Boolean).join('\n');
  const contentText = sanitizeForPrompt(joined, { maxChars });

  const lang = dominantLang(list.map((p) => p.lang));

  // webMeta — for storage/UI/search, NOT for the prompt. Strings sanitized too
  // (they may surface in UI / future prompt use), JSON-LD types are slugs.
  const webMeta: WebMetaOut = {
    siteName: collapseWs(siteName) || domainFromUrl(home.url || ''),
    title: collapseWs(home.title),
    description: sanitizeForPrompt(home.metaDescription, { maxChars: 400 }),
    lang,
    ogImage: homeOg.image || (home.twitter && home.twitter.image) || '',
    pageCount: list.length,
    jsonldTypes: [...jsonldTypes],
    entities: dedupStrings(entities.map((e) => sanitizeForPrompt(e, { maxChars: 120 }))),
  };

  return { contentText, lang, webMeta };
}

function dedupStrings(arr: string[]): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const v of arr) {
    const t = collapseWs(v);
    if (!t) continue;
    const k = t.toLowerCase();
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(t);
  }
  return out;
}

function domainFromUrl(u: string): string {
  try {
    return new URL(u).hostname.replace(/^www\./, '');
  } catch {
    return '';
  }
}

// ════════════════════════════════════════════════════════════════════════════
//  F4 — deterministic metadata (palette / fonts / tech stack)
// ════════════════════════════════════════════════════════════════════════════
//
// pageCtx contract (from F2):
//   { screenshotPath, width, height, finalUrl, title, capped, html, headers,
//     evaluate(codeString) -> Promise<jsonValue>, dispose() }
// `evaluate` runs a STRING of JS in the live page (webContents.executeJavaScript)
// and returns a JSON-serializable value. All three extractors are best-effort:
// any failure degrades to a partial/empty result, never throws.

// --- ffmpeg resolution (mirrors analyzer.js resolveFfmpeg, sans require) ----
function firstExisting(paths: Array<string | null | undefined>): string | null {
  for (const p of paths) {
    try {
      if (p && fs.existsSync(p)) return p;
    } catch {
      /* ignore */
    }
  }
  return null;
}
function resolveFfmpeg(): string {
  const exe = process.platform === 'win32' ? 'ffmpeg.exe' : 'ffmpeg';
  let userData = '';
  let resources = '';
  try {
    ({ app } = require('electron') as typeof import('electron'));
    userData = app.getPath('userData');
  } catch {
    /* non-electron ctx */
  }
  try {
    resources = process.resourcesPath || '';
  } catch {
    /* ignore */
  }
  const bundled = firstExisting([
    process.env.FFMPEG_BIN,
    userData && path.join(userData, 'runtime-bin', 'bin', exe),
    resources && path.join(resources, 'bin', exe),
    path.join(__dirname, '..', 'bin', exe),
  ]);
  if (bundled) return bundled;
  let staticPath: string | null = null;
  try {
    staticPath = require('ffmpeg-static') as string | null;
  } catch {
    /* optional */
  }
  if (staticPath && staticPath.includes('app.asar') && !staticPath.includes('app.asar.unpacked')) {
    staticPath = staticPath.replace('app.asar', 'app.asar.unpacked');
  }
  return (
    firstExisting([
      staticPath,
      '/opt/homebrew/bin/ffmpeg',
      '/usr/bin/ffmpeg',
      '/usr/local/bin/ffmpeg',
    ]) || 'ffmpeg'
  );
}
let app: typeof import('electron').app | undefined; // lazily filled by resolveFfmpeg() when running under Electron

// Reject a media path ffmpeg could misread as an option flag (leading '-').
function safeInputPath(p: unknown): string {
  if (typeof p !== 'string' || !p.trim()) throw new Error('Invalid media path');
  const abs = path.resolve(p);
  if (path.basename(abs).startsWith('-')) throw new Error(`Refusing unsafe media path: ${p}`);
  return abs;
}

// Hard ceiling for the ffmpeg palette fallback. ffmpeg can wedge on malformed /
// oversized screenshots; without this the Promise never settles, the child leaks
// and buildWebMetadata (awaited by capturePageTask) hangs the whole page task.
const FFMPEG_TIMEOUT_MS = 10000;

// SIGKILLs the child on AbortSignal abort OR after FFMPEG_TIMEOUT_MS, mirroring
// analyzer.js:spawnAsync. The optional `signal` lets the orchestrator's per-page
// timeout cancel ffmpeg; the internal timer guarantees liveness even when no
// signal is threaded in.
function spawnAsync(
  bin: string,
  args: string[],
  signal?: AbortSignal,
): Promise<{ code: number | null; stderr: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { stdio: ['ignore', 'ignore', 'pipe'] });
    let stderr = '';
    let settled = false;
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (d: string) => {
      stderr += d;
    });

    const timer = setTimeout(() => {
      if (settled) return;
      try {
        child.kill('SIGKILL');
      } catch {
        /* already gone */
      }
      settled = true;
      reject(Object.assign(new Error('ffmpeg timeout'), { name: 'TimeoutError' }));
    }, FFMPEG_TIMEOUT_MS);

    const onAbort = (): void => {
      if (settled) return;
      try {
        child.kill('SIGKILL');
      } catch {
        /* already gone */
      }
      settled = true;
      clearTimeout(timer);
      reject(Object.assign(new Error('AbortError'), { name: 'AbortError' }));
    };
    if (signal) {
      if (signal.aborted) return onAbort();
      signal.addEventListener('abort', onAbort, { once: true });
    }

    child.on('error', (err) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(err);
    });
    child.on('close', (code) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve({ code, stderr });
    });
  });
}

// --- color helpers ----------------------------------------------------------
function clamp255(n: number): number {
  return Math.max(0, Math.min(255, Math.round(n)));
}
function rgbToHex(r: number, g: number, b: number): string {
  return '#' + [r, g, b].map((v) => clamp255(v).toString(16).padStart(2, '0')).join('');
}
function relLuminance(r: number, g: number, b: number): number {
  const f = (c: number): number => {
    c /= 255;
    return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}
function rgbSaturation(r: number, g: number, b: number): number {
  const mx = Math.max(r, g, b);
  const mn = Math.min(r, g, b);
  return mx === 0 ? 0 : (mx - mn) / mx;
}

// The browser serializes every computed color to "rgb(r, g, b)" / "rgba(...)".
function parseCssColor(str: unknown): RgbaColor | null {
  if (typeof str !== 'string') return null;
  const m = str.match(/rgba?\(\s*([\d.]+)[ ,]+([\d.]+)[ ,]+([\d.]+)(?:[ ,/]+([\d.]+))?/i);
  if (!m) return null;
  const a = m[4] == null ? 1 : parseFloat(m[4]);
  if (a === 0) return null; // fully transparent → no contribution
  return { r: +m[1], g: +m[2], b: +m[3], a };
}

// JS source executed inside the live page to sample computed styles by area.
// Returns plain JSON: { samples: [{ color, kind, area }] }. No DOM handles leak.
const PALETTE_EVAL_SRC = `(() => {
  try {
    const out = [];
    const push = (color, kind, area) => { if (color) out.push({ color, kind, area: area || 0 }); };
    const areaOf = (el) => { try { const r = el.getBoundingClientRect(); return Math.max(0, r.width) * Math.max(0, r.height); } catch (e) { return 0; } };
    const cs = (el) => { try { return getComputedStyle(el); } catch (e) { return null; } };
    const bgEls = [document.documentElement, document.body]
      .concat([].slice.call(document.querySelectorAll('header,nav,main,section,[class*=hero]')).slice(0, 20));
    bgEls.forEach((el) => { if (!el) return; const s = cs(el); if (s) push(s.backgroundColor, 'background', areaOf(el)); });
    [].slice.call(document.querySelectorAll('body,h1,h2,h3,p')).slice(0, 40).forEach((el) => { const s = cs(el); if (s) push(s.color, 'text', areaOf(el)); });
    [].slice.call(document.querySelectorAll('a,button,[class*=btn],[class*=cta]')).slice(0, 40).forEach((el) => {
      const s = cs(el); if (!s) return; const ar = areaOf(el);
      push(s.backgroundColor, 'accent', ar); push(s.color, 'accent', ar);
    });
    return { samples: out };
  } catch (e) { return { samples: [], error: String(e && e.message || e) }; }
})()`;

/**
 * PALETTE — CSS-first (computed styles weighted by area), ffmpeg palettegen
 * fallback on the screenshot.
 * @param {object} pageCtx  { evaluate, screenshotPath }
 * @param {AbortSignal} [signal]  cancels the ffmpeg fallback (SIGKILL on abort).
 * @returns {Promise<Array<{ hex, role, weight }>>}
 */
async function extractPalette(
  pageCtx: PageCtx = {},
  signal?: AbortSignal,
): Promise<PaletteColor[]> {
  // 1. CSS-first.
  try {
    if (pageCtx && typeof pageCtx.evaluate === 'function') {
      const res = (await pageCtx.evaluate(PALETTE_EVAL_SRC)) as { samples?: unknown } | null;
      const samples = res && Array.isArray(res.samples) ? (res.samples as PaletteSample[]) : [];
      const css = paletteFromSamples(samples);
      if (css.length >= 2) return css;
    }
  } catch {
    /* fall through to pixel fallback */
  }

  // 2. pixel fallback via ffmpeg palettegen on the screenshot.
  try {
    if (pageCtx && pageCtx.screenshotPath) {
      const px = await paletteFromScreenshot(pageCtx.screenshotPath, signal);
      if (px.length) return px;
    }
  } catch {
    /* give up gracefully */
  }
  return [];
}

// Aggregate the per-element samples into weighted, role-tagged hex colors.
function paletteFromSamples(samples: PaletteSample[]): PaletteColor[] {
  // hex → { area, kinds, r, g, b }
  const byHex = new Map<
    string,
    { area: number; kinds: Record<string, number>; r: number; g: number; b: number }
  >();
  let totalArea = 0;
  for (const s of samples) {
    const c = parseCssColor(s.color);
    if (!c) continue;
    const hex = rgbToHex(c.r, c.g, c.b);
    const area = Math.max(0, Number(s.area) || 0) + 1; // +1 so zero-area still counts a little
    totalArea += area;
    let e = byHex.get(hex);
    if (!e) {
      e = { area: 0, kinds: {}, r: c.r, g: c.g, b: c.b };
      byHex.set(hex, e);
    }
    e.area += area;
    e.kinds[s.kind] = (e.kinds[s.kind] || 0) + area;
  }
  if (!byHex.size) return [];
  const rows = [...byHex.entries()].map(([hex, e]) => {
    // dominant kind by area → role
    let role = 'other';
    let best = 0;
    for (const [k, a] of Object.entries(e.kinds)) {
      if (a > best) {
        best = a;
        role = k;
      }
    }
    // Differentiate a light background from a dark one so downstream UI/search can
    // tell a light theme from a dark theme (a dark hero/section background gets its
    // own role instead of being lumped into the generic 'background').
    if (role === 'background' && relLuminance(e.r, e.g, e.b) <= 0.55) role = 'background-dark';
    return { hex, role, weight: e.area, _sat: rgbSaturation(e.r, e.g, e.b) };
  });
  rows.sort((a, b) => b.weight - a.weight);
  const top = rows.slice(0, 8);
  const sum = top.reduce((acc, r) => acc + r.weight, 0) || 1;
  return top.map((r) => ({
    hex: r.hex,
    role: r.role,
    weight: Math.round((r.weight / sum) * 100) / 100,
  }));
}

// Run ffmpeg palettegen → a tiny PNG of N swatches → read its colors.
async function paletteFromScreenshot(
  screenshotPath: string,
  signal?: AbortSignal,
): Promise<PaletteColor[]> {
  const ffmpeg = resolveFfmpeg();
  const safe = safeInputPath(screenshotPath);
  if (!fs.existsSync(safe)) return [];
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-pal-'));
  try {
    const out = path.join(tmp, 'pal.png');
    const { code } = await spawnAsync(
      ffmpeg,
      [
        '-protocol_whitelist',
        'file',
        '-i',
        safe,
        '-vf',
        'scale=320:-1:flags=area,palettegen=max_colors=8:stats_mode=full',
        '-y',
        out,
      ],
      signal,
    );
    if (code !== 0 || !fs.existsSync(out)) return [];
    const colors = readPngPalette(out);
    if (!colors.length) return [];
    // role by luminance: lightest→background, darkest→text, most saturated→accent.
    const withL = colors.map((c) => ({
      ...c,
      L: relLuminance(c.r, c.g, c.b),
      S: rgbSaturation(c.r, c.g, c.b),
    }));
    const lightest = withL.reduce((a, b) => (b.L > a.L ? b : a));
    const darkest = withL.reduce((a, b) => (b.L < a.L ? b : a));
    const accent = withL.reduce((a, b) => (b.S > a.S ? b : a));
    return withL.slice(0, 8).map((c, i) => ({
      hex: rgbToHex(c.r, c.g, c.b),
      role:
        c === lightest ? 'background' : c === darkest ? 'text' : c === accent ? 'accent' : 'other',
      weight: Math.round((1 - i / withL.length) * 100) / 100,
    }));
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

// Minimal PNG reader for the palettegen output: parse IHDR + (optionally PLTE)
// + IDAT, inflate, read RGB(A) pixels. palettegen emits a small truecolor PNG
// (one row of N swatches), so we read distinct pixel colors.
function readPngPalette(file: string): PixelColor[] {
  let buf: Buffer;
  try {
    buf = fs.readFileSync(file);
  } catch {
    return [];
  }
  if (buf.length < 8 || buf.readUInt32BE(0) !== 0x89504e47) return [];
  let pos = 8;
  let width = 0;
  let height = 0;
  let bitDepth = 8;
  let colorType = 6;
  const idat: Buffer[] = [];
  let plte: Buffer | null = null;
  while (pos + 8 <= buf.length) {
    const len = buf.readUInt32BE(pos);
    const type = buf.toString('ascii', pos + 4, pos + 8);
    const dataStart = pos + 8;
    const dataEnd = dataStart + len;
    if (dataEnd > buf.length) break;
    if (type === 'IHDR') {
      width = buf.readUInt32BE(dataStart);
      height = buf.readUInt32BE(dataStart + 4);
      bitDepth = buf[dataStart + 8];
      colorType = buf[dataStart + 9];
    } else if (type === 'PLTE') {
      plte = buf.slice(dataStart, dataEnd);
    } else if (type === 'IDAT') {
      idat.push(buf.slice(dataStart, dataEnd));
    } else if (type === 'IEND') {
      break;
    }
    pos = dataEnd + 4; // skip CRC
  }
  if (bitDepth !== 8 || !width || !height) {
    // Palette PNG with indexed colors: read PLTE directly.
    if (plte && plte.length >= 3) return pltEntries(plte);
    return [];
  }
  let raw: Buffer;
  try {
    raw = (require('zlib') as typeof import('zlib')).inflateSync(Buffer.concat(idat));
  } catch {
    return plte ? pltEntries(plte) : [];
  }
  const channels = colorType === 6 ? 4 : colorType === 2 ? 3 : colorType === 3 ? 1 : 0;
  if (!channels) return plte ? pltEntries(plte) : [];
  const stride = width * channels;
  const seen = new Map<string, PixelColor>();
  const prev = Buffer.alloc(stride);
  let cur = Buffer.alloc(stride);
  let rp = 0;
  for (let y = 0; y < height && rp < raw.length; y++) {
    const filter = raw[rp++];
    cur = Buffer.alloc(stride);
    for (let i = 0; i < stride; i++) {
      const x = raw[rp++] || 0;
      const a = i >= channels ? cur[i - channels] : 0;
      const b = prev[i];
      const c = i >= channels ? prev[i - channels] : 0;
      let val: number;
      switch (filter) {
        case 1:
          val = x + a;
          break;
        case 2:
          val = x + b;
          break;
        case 3:
          val = x + ((a + b) >> 1);
          break;
        case 4:
          val = x + paeth(a, b, c);
          break;
        default:
          val = x;
      }
      cur[i] = val & 0xff;
    }
    for (let x = 0; x < width; x++) {
      const o = x * channels;
      let r: number;
      let g: number;
      let bb: number;
      let al = 255;
      if (channels === 1) {
        const idx = cur[o];
        if (plte && idx * 3 + 2 < plte.length) {
          r = plte[idx * 3];
          g = plte[idx * 3 + 1];
          bb = plte[idx * 3 + 2];
        } else {
          r = g = bb = cur[o];
        }
      } else {
        r = cur[o];
        g = cur[o + 1];
        bb = cur[o + 2];
        if (channels === 4) al = cur[o + 3];
      }
      if (al === 0) continue;
      const hex = rgbToHex(r, g, bb);
      const e = seen.get(hex);
      if (e) e.n++;
      else seen.set(hex, { r, g, b: bb, n: 1 });
    }
    cur.copy(prev);
  }
  return [...seen.values()].sort((x, y) => y.n - x.n).slice(0, 8);
}
function pltEntries(plte: Buffer): PixelColor[] {
  const out: PixelColor[] = [];
  for (let i = 0; i + 2 < plte.length && out.length < 8; i += 3)
    out.push({ r: plte[i], g: plte[i + 1], b: plte[i + 2], n: 1 });
  return out;
}
function paeth(a: number, b: number, c: number): number {
  const p = a + b - c;
  const pa = Math.abs(p - a);
  const pb = Math.abs(p - b);
  const pc = Math.abs(p - c);
  if (pa <= pb && pa <= pc) return a;
  if (pb <= pc) return b;
  return c;
}

// --- fonts -------------------------------------------------------------------
const FONTS_EVAL_SRC = `(() => {
  try {
    const firstReal = (stack) => {
      if (!stack) return '';
      const toks = String(stack).split(',');
      for (let raw of toks) {
        const t = raw.trim().replace(/^['"]|['"]$/g, '');
        const lc = t.toLowerCase();
        if (!t) continue;
        if (['serif','sans-serif','monospace','system-ui','ui-sans-serif','ui-serif','ui-monospace','cursive','fantasy','-apple-system','blinkmacsystemfont','inherit','initial'].indexOf(lc) !== -1) continue;
        return t;
      }
      return '';
    };
    const famOf = (sel) => {
      const el = document.querySelector(sel);
      if (!el) return '';
      try { return firstReal(getComputedStyle(el).fontFamily); } catch (e) { return ''; }
    };
    const loaded = [];
    try { if (document.fonts && document.fonts.forEach) document.fonts.forEach((f) => loaded.push(f.family && f.family.replace(/^['"]|['"]$/g,''))); } catch (e) {}
    const links = [].slice.call(document.querySelectorAll('link[rel=stylesheet][href],link[rel=preconnect][href]')).map((l) => l.getAttribute('href') || '');
    return {
      heading: famOf('h1') || famOf('h2') || famOf('h3'),
      body: famOf('p') || famOf('body'),
      mono: famOf('code') || famOf('pre') || famOf('kbd'),
      loaded: loaded.filter(Boolean),
      links: links,
    };
  } catch (e) { return { error: String(e && e.message || e) }; }
})()`;

const SYSTEM_FONTS = new Set([
  'arial',
  'helvetica',
  'helvetica neue',
  'times',
  'times new roman',
  'georgia',
  'courier',
  'courier new',
  'verdana',
  'tahoma',
  'trebuchet ms',
  'segoe ui',
  'roboto',
  'system-ui',
  'menlo',
  'monaco',
  'consolas',
]);

function fontProvider(family: string, ctx: FontProviderCtx): string {
  const fam = String(family || '').toLowerCase();
  const links = (ctx.links || []).join(' ').toLowerCase();
  const html = (ctx.html || '').toLowerCase();
  const hay = links + ' ' + html;
  // Google / Adobe by host presence (and only attribute to THIS family loosely).
  if (/fonts\.googleapis\.com|fonts\.gstatic\.com/.test(hay)) return 'google';
  if (/use\.typekit\.net|use\.adobe\.com|typekit/.test(hay)) return 'adobe';
  if (SYSTEM_FONTS.has(fam)) return 'system';
  // a self-hosted @font-face referencing this family on the same origin
  if (
    new RegExp('@font-face[\\s\\S]{0,400}' + fam.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'i').test(
      html,
    )
  )
    return 'self';
  return 'unknown';
}

/**
 * FONT — computed font-family on heading/body/mono + declared sources.
 * @param {object} pageCtx  { evaluate, html }
 * @returns {Promise<Array<{ family, usage, provider }>>}
 */
async function extractFonts(pageCtx: PageCtx = {}): Promise<FontInfo[]> {
  let info: FontEvalResult = {};
  try {
    if (pageCtx && typeof pageCtx.evaluate === 'function') {
      info = ((await pageCtx.evaluate(FONTS_EVAL_SRC)) as FontEvalResult | null) || {};
    }
  } catch {
    info = {};
  }

  const ctx: FontProviderCtx = { links: info.links || [], html: pageCtx.html || '' };
  // augment links with @font-face / google link parse from raw html (fallback)
  if (pageCtx.html) {
    const linkHrefs = (
      pageCtx.html.match(
        new RegExp(`<link\\b[^>]{0,${ATTR_SCAN}}href\\s*=\\s*["']([^"']+)["']`, 'gi'),
      ) || []
    )
      .map((s) => (s.match(/href\s*=\s*["']([^"']+)["']/i) || [])[1])
      .filter(Boolean);
    ctx.links = ctx.links.concat(linkHrefs);
  }

  const RANK: Record<string, number> = { heading: 3, body: 2, mono: 1, other: 0 };
  const byFamily = new Map<string, FontInfo>();
  const add = (family: string | undefined, usage: string): void => {
    const fam = collapseWs(family);
    if (!fam) return;
    const key = fam.toLowerCase();
    const cur = byFamily.get(key);
    if (!cur || RANK[usage] > RANK[cur.usage]) {
      byFamily.set(key, { family: fam, usage, provider: fontProvider(fam, ctx) });
    }
  };
  add(info.heading, 'heading');
  add(info.body, 'body');
  add(info.mono, 'mono');
  // families loaded but not matched to a slot → 'other'
  for (const f of info.loaded || []) {
    const key = String(f).toLowerCase();
    if (!byFamily.has(key)) add(f, 'other');
  }
  return [...byFamily.values()];
}

// --- tech stack --------------------------------------------------------------
// ~18 curated rules. Each: { name, test({ html, headers, runtime }) }.
const TECH_RULES: TechRule[] = [
  {
    name: 'next.js',
    test: (c) =>
      /__NEXT_DATA__|\/_next\/static\//.test(c.html) ||
      /next\.js/i.test(c.poweredBy) ||
      !!c.runtime.next,
  },
  { name: 'nuxt', test: (c) => /__NUXT__|\/_nuxt\//.test(c.html) || !!c.runtime.nuxt },
  {
    name: 'react',
    test: (c) => /data-reactroot|data-reactid|id=["']__next["']/.test(c.html) || !!c.runtime.react,
  },
  { name: 'vue', test: (c) => /\bdata-v-[0-9a-f]{6,8}\b/.test(c.html) || !!c.runtime.vue },
  {
    name: 'svelte',
    test: (c) =>
      /class=["'][^"']*\bsvelte-[0-9a-z]+/.test(c.html) || /\/_app\/immutable\//.test(c.html),
  },
  {
    name: 'gatsby',
    test: (c) => /id=["']___gatsby["']|\/page-data\//.test(c.html) || !!c.runtime.gatsby,
  },
  { name: 'astro', test: (c) => /<astro-island|content=["']Astro["']/i.test(c.html) },
  {
    name: 'wordpress',
    test: (c) => /wp-content|wp-includes/.test(c.html) || /wordpress/i.test(c.generator),
  },
  {
    name: 'shopify',
    test: (c) =>
      /cdn\.shopify\.com|Shopify\.theme/.test(c.html) ||
      !!c.headers['x-shopify-stage'] ||
      !!c.runtime.shopify,
  },
  {
    name: 'webflow',
    test: (c) =>
      /webflow/i.test(c.generator) ||
      /\bclass=["'][^"']*\bw-(nav|container|button)\b/.test(c.html) ||
      /assets\.website-files\.com|assets-global\.website-files\.com/.test(c.html),
  },
  {
    name: 'framer',
    test: (c) => /framer/i.test(c.generator) || /framerusercontent\.com/.test(c.html),
  },
  {
    name: 'wix',
    test: (c) =>
      /static\.wixstatic\.com/.test(c.html) ||
      Object.keys(c.headers).some((h) => h.startsWith('x-wix')),
  },
  {
    name: 'squarespace',
    test: (c) => /squarespace/i.test(c.generator) || /static1\.squarespace\.com/.test(c.html),
  },
  {
    name: 'gsap',
    test: (c) => /\bgsap(\.min)?\.js|cdn\.jsdelivr\.net[^"']*gsap/.test(c.html) || !!c.runtime.gsap,
  },
  { name: 'three.js', test: (c) => /\bthree(\.min)?\.js\b/.test(c.html) || !!c.runtime.three },
  {
    name: 'lenis',
    test: (c) => /\blenis(\.min)?\.js\b|@studio-freight\/lenis/.test(c.html) || !!c.runtime.lenis,
  },
  { name: 'vercel', test: (c) => /vercel/i.test(c.server) || !!c.headers['x-vercel-id'] },
  { name: 'netlify', test: (c) => /netlify/i.test(c.server) || !!c.headers['x-nf-request-id'] },
  { name: 'cloudflare', test: (c) => /cloudflare/i.test(c.server) || !!c.headers['cf-ray'] },
];

const TECH_RUNTIME_EVAL_SRC = `(() => {
  try {
    var w = window;
    return {
      next: !!w.__NEXT_DATA__,
      nuxt: !!w.__NUXT__,
      react: !!w.React || !!(w.__REACT_DEVTOOLS_GLOBAL_HOOK__),
      vue: !!w.Vue || !!w.__VUE__,
      gatsby: !!w.___gatsby,
      shopify: !!w.Shopify,
      gsap: !!w.gsap,
      three: !!w.THREE,
      lenis: !!w.Lenis || !!w.lenis,
    };
  } catch (e) { return {}; }
})()`;

function lowerHeaders(headers: Record<string, unknown> | undefined): Record<string, string> {
  const out: Record<string, string> = {};
  if (headers && typeof headers === 'object') {
    for (const [k, v] of Object.entries(headers)) {
      out[String(k).toLowerCase()] = Array.isArray(v) ? v.join(', ') : String(v == null ? '' : v);
    }
  }
  return out;
}

function metaGeneratorOf(html: string): string {
  const m = html.match(
    new RegExp(
      `<meta\\b[^>]{0,${ATTR_SCAN}}name\\s*=\\s*["']generator["'][^>]{0,${ATTR_SCAN}}>`,
      'i',
    ),
  );
  if (!m) return '';
  const cm = m[0].match(/content\s*=\s*("([^"]*)"|'([^']*)')/i);
  return cm ? decodeEntities(cm[2] ?? cm[3] ?? '') : '';
}

/**
 * TECH STACK — static fingerprint (html/headers) + optional runtime probe.
 * @param {string} html
 * @param {object} headers  HTTP response headers (lowercased keys ok).
 * @param {object} [pageCtx] for runtime window.* probes (optional).
 * @returns {Promise<string[]>} normalized lowercase slugs, deduped, stable order.
 */
async function detectTechStack(
  html: unknown,
  headers: Record<string, unknown> | undefined,
  pageCtx?: PageCtx,
): Promise<string[]> {
  const safeHtml = typeof html === 'string' ? html : '';
  const hdr = lowerHeaders(headers);
  let runtime: TechRuntime = {};
  try {
    if (pageCtx && typeof pageCtx.evaluate === 'function') {
      runtime = ((await pageCtx.evaluate(TECH_RUNTIME_EVAL_SRC)) as TechRuntime | null) || {};
    }
  } catch {
    runtime = {};
  }

  const generator = metaGeneratorOf(safeHtml);
  const ctx: TechCtx = {
    html: safeHtml,
    headers: hdr,
    runtime,
    generator,
    poweredBy: hdr['x-powered-by'] || '',
    server: hdr.server || '',
  };

  const found: string[] = [];
  const seen = new Set<string>();
  for (const rule of TECH_RULES) {
    let hit = false;
    try {
      hit = !!rule.test(ctx);
    } catch {
      hit = false;
    }
    if (hit && !seen.has(rule.name)) {
      seen.add(rule.name);
      found.push(rule.name);
    }
  }
  // Generic generator fallback (CMS/builder not in the table) → slug it.
  if (generator) {
    const slug = generator
      .toLowerCase()
      .split(/[\s/]+/)[0]
      .replace(/[^a-z0-9.-]/g, '');
    if (slug && slug.length > 1 && !seen.has(slug)) {
      seen.add(slug);
      found.push(slug);
    }
  }
  return found;
}

/**
 * Convenience orchestrator: palette + fonts + techStack for one (home) page.
 * @param {object} pageCtx  { evaluate, screenshotPath, html, headers }
 * @param {AbortSignal} [signal]  threaded into the ffmpeg palette fallback.
 * @returns {Promise<{ palette, fonts, techStack }>}
 */
async function buildWebMetadata(
  pageCtx: PageCtx = {},
  signal?: AbortSignal,
): Promise<{ palette: PaletteColor[]; fonts: FontInfo[]; techStack: string[] }> {
  const [palette, fonts, techStack] = await Promise.all([
    extractPalette(pageCtx, signal).catch(() => [] as PaletteColor[]),
    extractFonts(pageCtx).catch(() => [] as FontInfo[]),
    detectTechStack(pageCtx.html, pageCtx.headers, pageCtx).catch(() => [] as string[]),
  ]);
  return { palette, fonts, techStack };
}

// ════════════════════════════════════════════════════════════════════════════
//  F7 — awards lookup (badge / self-link in the site's own HTML)
// ════════════════════════════════════════════════════════════════════════════

// One declarative entry per platform. host(s) + entry-path regex + badge regex
// + level lexicon. evidence preference: self-link > badge-img > badge-script > text-only.
const AWARD_DETECTORS: AwardDetector[] = [
  {
    platform: 'awwwards',
    hosts: ['awwwards.com', 'www.awwwards.com'],
    entryPath: /\/sites\/[a-z0-9-]+/i,
    badge: /assets\.awwwards\.com\/.*badge|awwwards/i,
    name: 'Awwwards',
    levels: [
      [/site of the day|\bsotd\b/i, 'sotd'],
      [/developer award/i, 'developer'],
      [/honou?rable mention/i, 'honorable'],
      [/mobile excellence/i, 'mobile-excellence'],
      [/site of the month/i, 'site-of-the-month'],
    ],
  },
  {
    platform: 'cssda',
    hosts: ['cssdesignawards.com', 'www.cssdesignawards.com'],
    entryPath: /\/sites\/[a-z0-9-]+(?:\/\d+)?\/?/i,
    badge: /cssdesignawards\.com\/.*award|css design award/i,
    name: 'CSS Design Awards',
    levels: [
      [/website of the day|\bwotd\b/i, 'wotd'],
      [/special kudos/i, 'css-special-kudos'],
      [/ui\/?ux/i, 'uiux'],
    ],
  },
  {
    platform: 'fwa',
    hosts: ['thefwa.com', 'www.thefwa.com'],
    entryPath: /\/cases\/[a-z0-9-]+/i,
    badge: /thefwa\.com\/.*badge|\bfwa\b/i,
    name: 'FWA',
    levels: [
      [/fwa of the day|\bfotd\b/i, 'fotd'],
      [/fwa of the month|\bfotm\b/i, 'fotm'],
    ],
  },
  {
    platform: 'godly',
    hosts: ['godly.website', 'www.godly.website'],
    entryPath: /\/sites\/[a-z0-9-]+/i,
    badge: /godly/i,
    name: 'Godly',
    levels: [],
  },
  {
    platform: 'landbook',
    hosts: ['land-book.com', 'www.land-book.com'],
    entryPath: /\/gallery\/[a-z0-9-]+/i,
    badge: /land-book/i,
    name: 'Land-book',
    levels: [],
  },
  {
    platform: 'siteinspire',
    hosts: ['siteinspire.com', 'www.siteinspire.com'],
    entryPath: /\/websites\/\d+-/i,
    badge: /siteinspire/i,
    name: 'SiteInspire',
    levels: [],
  },
];

const EVIDENCE_CONFIDENCE: Record<AwardEvidence, number> = {
  'self-link': 0.9,
  'badge-img': 0.6,
  'badge-script': 0.55,
  'text-only': 0.25,
};
const EVIDENCE_RANK: Record<AwardEvidence, number> = {
  'self-link': 4,
  'badge-img': 3,
  'badge-script': 2,
  'text-only': 1,
};

function hostOf(u: string): string {
  try {
    return new URL(u).hostname.toLowerCase().replace(/^www\./, '');
  } catch {
    return '';
  }
}
function siteSlug(siteUrl: string): string {
  // domain → slug-ish form for self-link domain-match (e.g. "acme-studio.com" → "acme-studio")
  const host = hostOf(siteUrl);
  if (!host) return '';
  const label = host.split('.')[0];
  return label.toLowerCase();
}

// Isolate the footer region of an HTML string (best-effort). Falls back to the
// last ~30% of the document when there's no <footer>.
function footerRegion(html: string): string {
  const m = html.match(new RegExp(`<footer\\b[^>]{0,${ATTR_SCAN}}>([\\s\\S]*?)<\\/footer>`, 'i'));
  if (m) return m[1];
  return html.slice(Math.floor(html.length * 0.7));
}

function levelFromText(detector: AwardDetector, text: string): string | undefined {
  for (const [re, level] of detector.levels) {
    if (re.test(text)) return level;
  }
  return undefined;
}
function dateFromText(text: string): string | undefined {
  const my = text.match(
    /\b(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?\s+(\d{4})\b/i,
  );
  if (my) {
    const mm = (
      {
        jan: '01',
        feb: '02',
        mar: '03',
        apr: '04',
        may: '05',
        jun: '06',
        jul: '07',
        aug: '08',
        sep: '09',
        oct: '10',
        nov: '11',
        dec: '12',
      } as Record<string, string>
    )[my[1].slice(0, 3).toLowerCase()];
    return `${my[2]}-${mm}`;
  }
  const y = text.match(/\b(20\d{2})\b/);
  return y ? y[1] : undefined;
}

/**
 * Detect awards the site exposes in its own HTML. Pure & deterministic.
 * @param {Array<{ url?: string, html: string }|string>} pages  pages[0] = home.
 * @param {string} [siteUrl]  canonical site URL (default pages[0].url).
 * @param {{ allowTextOnly?: boolean }} [opts]
 * @returns {Array<{ platform, level?, date?, profileUrl?, evidence, confidence }>}
 */
function detectAwards(
  pages: Array<AwardPage | string> | unknown,
  siteUrl?: string,
  opts: { allowTextOnly?: boolean } = {},
): Award[] {
  const allowTextOnly = !!opts.allowTextOnly; // default OFF (high false-positive)
  const list = (Array.isArray(pages) ? pages : [])
    .map((p) => (typeof p === 'string' ? { html: p } : p))
    .filter((p): p is AwardPage => !!p && typeof (p as AwardPage).html === 'string');
  if (!list.length) return [];
  const site = siteUrl || list[0].url || '';
  const slug = siteSlug(site);

  // platform → best Award found so far
  const best = new Map<string, Award>();
  const consider = (award: Award): void => {
    const prev = best.get(award.platform);
    if (!prev || EVIDENCE_RANK[award.evidence] > EVIDENCE_RANK[prev.evidence]) {
      // merge level/date/profileUrl forward when upgrading
      best.set(award.platform, {
        ...award,
        level: award.level || (prev && prev.level),
        date: award.date || (prev && prev.date),
        profileUrl: award.profileUrl || (prev && prev.profileUrl),
      });
    } else {
      // keep stronger evidence but fill missing fields from the weaker hit
      if (!prev.level && award.level) prev.level = award.level;
      if (!prev.date && award.date) prev.date = award.date;
      if (!prev.profileUrl && award.profileUrl) prev.profileUrl = award.profileUrl;
    }
  };

  for (const page of list) {
    // Hard-cap raw HTML before any regex pass (DoS guard, see MAX_HTML).
    const html = String(page.html).slice(0, MAX_HTML);
    const footer = footerRegion(html);

    // Pre-extract anchors and imgs (from footer first, then whole doc for self-links).
    const anchors = matchAnchors(html);
    const imgs = matchImgs(footer);

    for (const det of AWARD_DETECTORS) {
      // 1. self-link: an <a> to host + entry path. Domain-match mitigation: the
      //    entry slug should reference THIS site (slug-in-path), or at least be a
      //    specific /sites/<slug> entry (not the platform home/category).
      let selfLink: Anchor | null = null;
      const platformEntryLinks: Anchor[] = [];
      for (const a of anchors) {
        const h = hostOf(a.href);
        if (!det.hosts.includes(h) && !det.hosts.includes('www.' + h)) continue;
        if (!det.entryPath.test(pathOf(a.href))) continue; // must be a specific entry, not the home
        platformEntryLinks.push(a);
      }
      // Reject blogroll/portfolio pattern: many distinct entries → it's a gallery,
      // not "I'm awarded". Only treat as self-link if exactly the site's own entry.
      const matchingSelf = platformEntryLinks.filter(
        (a) => slug && pathOf(a.href).toLowerCase().includes(slug),
      );
      if (matchingSelf.length) selfLink = matchingSelf[0];
      else if (platformEntryLinks.length === 1) selfLink = platformEntryLinks[0]; // single specific entry, no slug match → medium

      if (selfLink) {
        const ctxText = `${selfLink.text} ${selfLink.title} ${selfLink.ariaLabel}`;
        const domainMatch = slug && pathOf(selfLink.href).toLowerCase().includes(slug);
        consider({
          platform: det.platform,
          level:
            levelFromText(det, ctxText) ||
            levelFromText(det, footerTextNear(footer, selfLink.href)),
          date: dateFromText(`${pathOf(selfLink.href)} ${ctxText}`),
          profileUrl: absolutize(selfLink.href),
          evidence: 'self-link',
          confidence: domainMatch ? EVIDENCE_CONFIDENCE['self-link'] : 0.7,
        });
        continue; // strongest evidence for this platform on this page; move on
      }

      // 2. badge-img: an <img> whose src/alt matches the badge regex (footer).
      const badgeImg = imgs.find((im) => det.badge.test(`${im.src} ${im.alt}`));
      if (badgeImg) {
        consider({
          platform: det.platform,
          level: levelFromText(det, `${badgeImg.alt} ${footerTextNear(footer, badgeImg.src)}`),
          profileUrl: undefined,
          evidence: 'badge-img',
          confidence: EVIDENCE_CONFIDENCE['badge-img'],
        });
        continue;
      }

      // 3. badge-script: a <script src=…badge…> / widget.
      const scripts = matchScriptSrcs(footer);
      const badgeScript = scripts.find(
        (s) => det.hosts.some((host) => s.includes(host)) && /badge|widget/i.test(s),
      );
      if (badgeScript) {
        consider({
          platform: det.platform,
          evidence: 'badge-script',
          confidence: EVIDENCE_CONFIDENCE['badge-script'],
        });
        continue;
      }

      // 4. text-only (opt-in): platform name + level lexicon in the footer text.
      if (allowTextOnly) {
        const footerText = collapseWs(decodeEntities(stripTags(footer)));
        const level = levelFromText(det, footerText);
        const named = new RegExp(
          `\\b${det.name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`,
          'i',
        ).test(footerText);
        if (level || named) {
          consider({
            platform: det.platform,
            level,
            evidence: 'text-only',
            confidence: EVIDENCE_CONFIDENCE['text-only'],
          });
        }
      }
    }
  }

  return [...best.values()];
}

function pathOf(u: string): string {
  try {
    return new URL(u, 'https://x.invalid').pathname;
  } catch {
    return String(u || '');
  }
}
function absolutize(u: string): string {
  return String(u || '');
}

// Parse <a> tags into { href, text, title, ariaLabel }.
function matchAnchors(html: string): Anchor[] {
  const out: Anchor[] = [];
  const re = new RegExp(`<a\\b([^>]{0,${ATTR_SCAN}})>([\\s\\S]*?)<\\/a>`, 'gi');
  let m: RegExpExecArray | null;
  while ((m = re.exec(html))) {
    const attrs = m[1];
    const href = attrVal(attrs, 'href');
    if (!href) continue;
    out.push({
      href,
      text: collapseWs(decodeEntities(stripTags(m[2]))),
      title: attrVal(attrs, 'title'),
      ariaLabel: attrVal(attrs, 'aria-label'),
    });
  }
  return out;
}
function matchImgs(html: string): ImgTag[] {
  const out: ImgTag[] = [];
  const re = new RegExp(`<img\\b([^>]{0,${ATTR_SCAN}})>`, 'gi');
  let m: RegExpExecArray | null;
  while ((m = re.exec(html))) out.push({ src: attrVal(m[1], 'src'), alt: attrVal(m[1], 'alt') });
  return out;
}
function matchScriptSrcs(html: string): string[] {
  const out: string[] = [];
  const re = new RegExp(`<script\\b([^>]{0,${ATTR_SCAN}})>`, 'gi');
  let m: RegExpExecArray | null;
  while ((m = re.exec(html))) {
    const s = attrVal(m[1], 'src');
    if (s) out.push(s);
  }
  return out;
}
function attrVal(attrs: string, name: string): string {
  const m = String(attrs || '').match(
    new RegExp(`\\b${name}\\s*=\\s*("([^"]*)"|'([^']*)'|([^\\s>]+))`, 'i'),
  );
  return m ? decodeEntities(m[2] ?? m[3] ?? m[4] ?? '') : '';
}
// Grab a small slice of footer text around the first occurrence of a needle.
function footerTextNear(footer: string, needle: string): string {
  const idx = needle ? footer.indexOf(needle) : -1;
  if (idx === -1) return '';
  const slice = footer.slice(Math.max(0, idx - 300), idx + 300);
  return collapseWs(decodeEntities(stripTags(slice)));
}

/**
 * Map detected awards to searchable tags + entities. Only STRONG evidence
 * (self-link / badge-img) contributes; text-only never does.
 * @param {Array} awards  output of detectAwards.
 * @returns {{ tags: string[], entities: string[] }}
 */
function awardsToTagsEntities(awards: Award[] | unknown): { tags: string[]; entities: string[] } {
  const tags = new Set<string>();
  const entities = new Set<string>();
  const det = new Map(AWARD_DETECTORS.map((d) => [d.platform, d]));
  // platform → base tag slug
  const TAG_BASE: Record<string, string> = {
    awwwards: 'awwwards',
    cssda: 'css-design-awards',
    fwa: 'fwa',
    godly: 'godly',
    landbook: 'land-book',
    siteinspire: 'siteinspire',
  };
  for (const a of Array.isArray(awards) ? awards : []) {
    if (!a || (a.evidence !== 'self-link' && a.evidence !== 'badge-img')) continue;
    const base = TAG_BASE[a.platform] || a.platform;
    tags.add(base);
    tags.add('award-winning');
    if (a.level) tags.add(`${base}-${a.level}`);
    const d = det.get(a.platform);
    if (d && d.name) entities.add(d.name);
  }
  return { tags: [...tags], entities: [...entities] };
}

/**
 * Conservative quality boost for F9 ranking. Strong, domain-matched evidence
 * lifts the tier floor; text-only never contributes. Additive + capped.
 * @param {Array} awards
 * @returns {{ tierFloor?: 'specific', score: number }}
 */
function awardsQualityBoost(awards: Award[] | unknown): { tierFloor?: 'specific'; score: number } {
  let score = 0;
  let strong = false;
  for (const a of Array.isArray(awards) ? awards : []) {
    if (a && (a.evidence === 'self-link' || a.evidence === 'badge-img')) {
      strong = true;
      score += a.confidence || 0;
    }
  }
  score = Math.min(1, score); // capped
  return strong ? { tierFloor: 'specific', score } : { score };
}

export {
  // F3
  extractContent,
  aggregateSiteText,
  sanitizeForPrompt,
  sanitizeUntrustedText,
  extractMainText,
  extractJsonLd,
  // F4
  extractPalette,
  extractFonts,
  detectTechStack,
  buildWebMetadata,
  // F7
  detectAwards,
  awardsToTagsEntities,
  awardsQualityBoost,
  AWARD_DETECTORS,
};
