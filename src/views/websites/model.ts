// Pure view-model helpers for the Websites view (design-reference browser).
//
// Everything that crosses the IPC boundary here is loosely typed: web job
// records stream as `unknown`, `webMeta` is an open object, and v1 rows only
// carry a subset of the v2 capture fields. These helpers normalise that data
// into small, strictly-typed shapes the components can render without guards —
// and, crucially, they only ever surface STRINGS/NUMBERS for display, so an
// unexpected object in a payload can never end up as a React child.

// ── Primitive readers ────────────────────────────────────────────────────────

export function str(v: unknown): string {
  if (typeof v === 'string') return v;
  if (typeof v === 'number' && Number.isFinite(v)) return String(v);
  return '';
}

export function num(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

function obj(v: unknown): Record<string, unknown> | null {
  return v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
}

function list(v: unknown): unknown[] {
  return Array.isArray(v) ? v : [];
}

function strList(v: unknown): string[] {
  return list(v)
    .map((x) => str(x).trim())
    .filter(Boolean);
}

export function hostOf(url: string | null | undefined): string {
  if (!url) return '';
  try {
    return new URL(url).hostname.replace(/^www\./, '');
  } catch {
    return '';
  }
}

export function pathOf(url: string | null | undefined): string {
  if (!url) return '';
  try {
    const u = new URL(url);
    return `${u.pathname}${u.search}` || '/';
  } catch {
    return url;
  }
}

// ── Live capture jobs ───────────────────────────────────────────────────────

export type WebStatus =
  | 'pending'
  | 'queued'
  | 'discovering'
  | 'capturing'
  | 'extracting'
  | 'analyzing'
  | 'blocked'
  | 'done'
  | 'error'
  | 'cancelled';

const STATUSES = new Set<WebStatus>([
  'pending',
  'queued',
  'discovering',
  'capturing',
  'extracting',
  'analyzing',
  'blocked',
  'done',
  'error',
  'cancelled',
]);

export const ACTIVE_STATUSES = new Set<WebStatus>([
  'pending',
  'queued',
  'discovering',
  'capturing',
  'extracting',
  'analyzing',
]);

export interface TimelineEvent {
  id: string;
  kind: string;
  text: string;
  ts: number;
  // Discovered page URLs (the only structured payload the timeline renders).
  pages: string[];
}

export interface WebJob {
  key: string;
  postId: string | null;
  status: WebStatus;
  url: string;
  finalUrl: string;
  domain: string;
  title: string;
  stage: string;
  progress: number;
  error: string;
  partial: boolean;
  pagesTotal: number;
  pagesDone: number;
  events: TimelineEvent[];
  queuedAt: number | null;
  startedAt: number | null;
  finishedAt: number | null;
  blocked: { vendor: string; url: string; reason: string } | null;
}

// Event payloads are untyped and have crashed the view before (an object
// rendered as a child). Keep only well-formed strings.
function toEvent(raw: unknown, i: number): TimelineEvent | null {
  const e = obj(raw);
  if (!e) return null;
  const text = str(e.text);
  if (!text) return null;
  const data = obj(e.data);
  return {
    id: str(e.id) || `e${i}`,
    kind: str(e.kind) || 'info',
    text,
    ts: num(e.ts) ?? 0,
    pages: data ? strList(data.pages).slice(0, 40) : [],
  };
}

export function toWebJob(raw: unknown): WebJob | null {
  const j = obj(raw);
  if (!j) return null;
  const postId = str(j.postId) || null;
  const key = postId ? `web:${postId}` : str(j.key);
  if (!key) return null;
  const s = str(j.status) as WebStatus;
  const b = obj(j.blocked);
  const url = str(j.url);
  const finalUrl = str(j.finalUrl) || url;
  return {
    key,
    postId,
    status: STATUSES.has(s) ? s : 'pending',
    url,
    finalUrl,
    domain: str(j.domain) || hostOf(finalUrl),
    title: str(j.title),
    stage: str(j.stage),
    progress: Math.max(0, Math.min(1, num(j.progress) ?? 0)),
    error: str(j.error),
    partial: j.partial === true,
    pagesTotal: num(j.pagesTotal) ?? 0,
    pagesDone: num(j.pagesDone) ?? 0,
    events: list(j.events)
      .map(toEvent)
      .filter((e): e is TimelineEvent => e !== null),
    queuedAt: num(j.queuedAt),
    startedAt: num(j.startedAt),
    finishedAt: num(j.finishedAt),
    blocked: b ? { vendor: str(b.vendor), url: str(b.url), reason: str(b.reason) } : null,
  };
}

// ── AI analyzer job (shared queue, keyed `${postId}:analyze`) ───────────────

export interface AiJobView {
  key: string;
  status: string;
  streamText: string;
  model: string;
  error: string;
  label: string; // the post's author/site name as known to the analyzer
}

export function toAiJob(raw: unknown): AiJobView | null {
  const j = obj(raw);
  if (!j) return null;
  return {
    key: str(j.key),
    status: str(j.status),
    streamText: str(j.streamText),
    model: str(j.model),
    error: str(j.error),
    label: str(j.authorUsername),
  };
}

// Lenient reader over the model's PARTIAL streamed JSON: returns the (possibly
// still open) string value of `key`, or '' when the key hasn't streamed yet.
export function streamedString(text: string, key: string): string {
  if (!text) return '';
  const m = text.match(new RegExp(`"${key}"\\s*:\\s*"`));
  if (!m || m.index === undefined) return '';
  let out = '';
  for (let i = m.index + m[0].length; i < text.length; i++) {
    const c = text[i];
    if (c === '\\') {
      const n = text[i + 1];
      if (n === undefined) break;
      out += n === 'n' ? '\n' : n;
      i++;
      continue;
    }
    if (c === '"') break;
    out += c;
  }
  return out;
}

// What to show for a streaming web catalog (schema v2 emits observations →
// closed facets → … → summary → description). The summary wins once it has
// started; before that, the evidence-first observations.
export function streamPreview(text: string): {
  key: 'summary' | 'observations' | 'raw';
  text: string;
} {
  const summary = streamedString(text, 'summary');
  if (summary) return { key: 'summary', text: summary };
  const obs = streamedString(text, 'observations');
  if (obs) return { key: 'observations', text: obs };
  const raw = text.replace(/\s+/g, ' ').trim();
  return { key: 'raw', text: raw.length > 220 ? `…${raw.slice(-220)}` : raw };
}

// AI sub-step reached by the stream (order of the v2 schema keys).
export function aiStep(job: AiJobView | null): number {
  if (!job) return -1;
  switch (job.status) {
    case 'pending':
      return 0;
    case 'extracting':
      return 1;
    case 'analyzing': {
      const t = job.streamText;
      if (/"(summary|description)"\s*:\s*"./.test(t)) return 4;
      if (/"site_type"\s*:/.test(t)) return 3;
      return 2;
    }
    case 'done':
      return 5;
    default:
      return -1;
  }
}

// ── Site view model ─────────────────────────────────────────────────────────

export interface ImageRef {
  path: string;
  width: number;
  height: number;
}

export interface ChunkRef extends ImageRef {
  top: number;
  cssHeight: number;
}

export interface SectionRef extends ImageRef {
  kind: string;
  heading: string;
  top: number;
  pageIndex: number;
}

export interface PageView {
  url: string;
  pageType: string;
  title: string;
  hero: ImageRef | null;
  chunks: ChunkRef[];
  footer: ImageRef | null;
  sections: SectionRef[];
  heightCss: number | null;
  capped: boolean;
  jacked: boolean;
  qcStatus: string;
  qcReason: string;
  h1: string;
  headings: string[];
  ctas: string[];
}

export interface Swatch {
  hex: string;
  name: string;
  role: string;
  coverage: number | null;
}

export interface FontView {
  family: string;
  role: string;
  roles: string[];
  weights: number[];
  sizes: number[];
  share: number | null;
  provider: string;
  classification: string;
  sample: string;
  italic: boolean;
}

export interface TechEntry {
  name: string;
  category: string;
  version: string;
}

export interface AwardView {
  platform: string;
  level: string;
  date: string;
  profileUrl: string;
  evidence: string;
}

export interface VideoRef {
  path: string;
  preview: string;
  poster: string;
  width: number;
  height: number;
  duration: number;
}

export interface CaptureInfo {
  engine: string;
  discovery: string;
  skipped: { url: string; reason: string }[];
  consent: string;
  timings: { url: string; ms: number }[];
  viewport: string;
}

export interface SiteMeta {
  siteName: string;
  title: string;
  description: string;
  lang: string;
  languages: string[];
  themeColor: string;
  scheme: string;
  contrast: { text: string; background: string; ratio: number } | null;
  typeScale: { size: number; share: number }[];
  baseSize: number | null;
  scaleRatio: number | null;
  tech: TechEntry[];
  traits: Record<string, unknown>;
  video: VideoRef | null;
  social: { platform: string; href: string }[];
  credits: { text: string; href: string }[];
  organization: string;
  capture: CaptureInfo | null;
  favicon: string;
  ogImagePath: string;
}

export interface LegacyAi {
  description: string;
  tags: string[];
  category: string;
  contentType: string;
  language: string;
  saveReason: string;
  entities: string[];
  keywords: string[];
  model: string;
}

export interface SiteView {
  id: string;
  name: string;
  domain: string;
  url: string;
  favicon: string;
  cover: ImageRef | null; // the card/detail hero
  coverIsTall: boolean; // v1 full-page screenshot → anchor to the top
  palette: Swatch[];
  fonts: FontView[];
  tech: TechEntry[];
  awards: AwardView[];
  pages: PageView[];
  meta: SiteMeta;
  ai: Shelfy.WebAiCatalog | null;
  legacy: LegacyAi;
  capturedAt: number | null; // epoch ms
  isV2: boolean;
  hasCapture: boolean;
}

function imageRef(v: unknown): ImageRef | null {
  const o = obj(v);
  if (!o) return null;
  const path = str(o.path) || str(o.screenshotPath);
  if (!path) return null;
  return { path, width: num(o.width) ?? 0, height: num(o.height) ?? 0 };
}

function toPage(raw: unknown, index: number, fallbackUrl: string): PageView | null {
  const p = obj(raw);
  if (!p) return null;
  const hero =
    imageRef(p.hero) || (str(p.screenshotPath) ? imageRef({ path: p.screenshotPath }) : null);
  const chunks: ChunkRef[] = list(p.chunks)
    .map((c) => {
      const o = obj(c);
      const ref = imageRef(c);
      if (!o || !ref) return null;
      return { ...ref, top: num(o.top) ?? 0, cssHeight: num(o.cssHeight) ?? 0 };
    })
    .filter((c): c is ChunkRef => c !== null);
  const sections: SectionRef[] = list(p.sections)
    .map((s) => {
      const o = obj(s);
      const ref = imageRef(s);
      if (!o || !ref) return null;
      return {
        ...ref,
        kind: str(o.kind) || 'content',
        heading: str(o.heading),
        top: num(o.top) ?? 0,
        pageIndex: index,
      };
    })
    .filter((s): s is SectionRef => s !== null);
  const qc = obj(p.qc);
  const digest = obj(p.digest);
  return {
    url: str(p.url) || fallbackUrl,
    pageType: str(p.pageType),
    title: str(p.title),
    hero,
    chunks,
    footer: imageRef(p.footer),
    sections,
    heightCss: num(p.heightCss),
    capped: p.capped === true,
    jacked: p.jacked === true,
    qcStatus: qc ? str(qc.status) : '',
    qcReason: qc ? str(qc.reason) : '',
    h1: digest ? str(digest.h1) : '',
    headings: digest ? strList(digest.headings) : [],
    ctas: digest ? strList(digest.ctas) : [],
  };
}

function toSwatch(raw: unknown): Swatch | null {
  if (typeof raw === 'string') {
    const hex = normHex(raw);
    return hex ? { hex, name: '', role: '', coverage: null } : null;
  }
  const o = obj(raw);
  if (!o) return null;
  const hex = normHex(str(o.hex));
  if (!hex) return null;
  return {
    hex,
    name: str(o.name),
    role: str(o.role),
    coverage: num(o.coverage) ?? num(o.weight),
  };
}

function toFont(raw: unknown): FontView | null {
  if (typeof raw === 'string') raw = { family: raw };
  const o = obj(raw);
  const family = o ? str(o.family).trim() : '';
  if (!o || !family) return null;
  return {
    family,
    role: str(o.role) || str(o.usage),
    roles: strList(o.roles),
    weights: list(o.weights)
      .map(num)
      .filter((n): n is number => n !== null),
    sizes: list(o.sizes)
      .map(num)
      .filter((n): n is number => n !== null),
    share: num(o.share),
    provider: str(o.provider),
    classification: str(o.classification),
    sample: str(o.sample),
    italic: o.italic === true,
  };
}

function toAward(raw: unknown): AwardView | null {
  const o = obj(raw);
  if (!o) return null;
  const platform = str(o.platform);
  const level = str(o.level);
  if (!platform && !level) return null;
  return {
    platform,
    level,
    date: str(o.date),
    profileUrl: str(o.profileUrl),
    evidence: str(o.evidence),
  };
}

function toTech(raw: unknown): TechEntry | null {
  if (typeof raw === 'string')
    return raw.trim() ? { name: raw.trim(), category: '', version: '' } : null;
  const o = obj(raw);
  const name = o ? str(o.name) : '';
  if (!o || !name) return null;
  return { name, category: str(o.category), version: str(o.version) };
}

function toVideo(raw: unknown): VideoRef | null {
  const o = obj(raw);
  const path = o ? str(o.path) : '';
  if (!o || !path) return null;
  return {
    path,
    preview: str(o.preview),
    poster: str(o.poster),
    width: num(o.width) ?? 0,
    height: num(o.height) ?? 0,
    duration: num(o.duration) ?? 0,
  };
}

function toCapture(raw: unknown): CaptureInfo | null {
  const o = obj(raw);
  if (!o) return null;
  const consent = obj(o.consent);
  const timings = obj(o.timings);
  const vp = obj(o.viewport);
  return {
    engine: str(o.engine),
    discovery: str(o.discovery),
    skipped: list(o.skipped)
      .map((s) => {
        const so = obj(s);
        return so ? { url: str(so.url), reason: str(so.reason) } : null;
      })
      .filter((s): s is { url: string; reason: string } => !!s && !!s.url),
    consent: consent ? [str(consent.cmp), str(consent.result)].filter(Boolean).join(' · ') : '',
    timings: timings
      ? Object.entries(timings)
          .map(([url, ms]) => ({ url, ms: num(ms) ?? 0 }))
          .filter((x) => x.ms > 0)
      : [],
    viewport:
      vp && num(vp.width) && num(vp.height)
        ? `${num(vp.width)}×${num(vp.height)}${num(vp.scale) ? ` @${num(vp.scale)}×` : ''}`
        : '',
  };
}

export function readMeta(raw: unknown): SiteMeta {
  const m = obj(raw) || {};
  const contrast = obj(m.contrast);
  const ratio = contrast ? num(contrast.ratio) : null;
  const org = obj(m.organization);
  return {
    siteName: str(m.siteName),
    title: str(m.title),
    description: str(m.description),
    lang: str(m.lang),
    languages: strList(m.languages),
    themeColor: normHex(str(m.themeColor)) || '',
    scheme: str(m.scheme),
    contrast:
      contrast && ratio !== null
        ? {
            text: normHex(str(contrast.text)) || '',
            background: normHex(str(contrast.background)) || '',
            ratio,
          }
        : null,
    typeScale: list(m.typeScale)
      .map((x) => {
        const o = obj(x);
        const size = o ? num(o.size) : null;
        return o && size !== null ? { size, share: num(o.share) ?? 0 } : null;
      })
      .filter((x): x is { size: number; share: number } => x !== null),
    baseSize: num(m.baseSize),
    scaleRatio: num(m.scaleRatio),
    tech: list(m.tech)
      .map(toTech)
      .filter((x): x is TechEntry => x !== null),
    traits: obj(m.traits) || {},
    video: toVideo(m.video),
    social: list(m.social)
      .map((s) => {
        const o = obj(s);
        return o ? { platform: str(o.platform), href: str(o.href) } : null;
      })
      .filter((s): s is { platform: string; href: string } => !!s && /^https?:\/\//.test(s.href)),
    credits: list(m.credits)
      .map((s) => {
        const o = obj(s);
        return o ? { text: str(o.text), href: str(o.href) } : null;
      })
      .filter((s): s is { text: string; href: string } => !!s && !!s.text),
    organization: org ? str(org.name) : '',
    capture: toCapture(m.capture),
    favicon: str(m.favicon),
    ogImagePath: str(m.ogImagePath),
  };
}

// A short, human site name: the organisation, else the og:site_name / title
// trimmed at the first separator ("Lusion - Award Winning…" → "Lusion").
export function displayName(meta: SiteMeta, fallbacks: (string | null | undefined)[]): string {
  const cut = (s: string): string => s.split(/\s[-–—|·:]\s/)[0].trim();
  for (const c of [meta.organization, meta.siteName, meta.title, ...fallbacks]) {
    const v = cut(str(c));
    if (v) return v;
  }
  return '';
}

// Builds the view model from a persisted web post (or an archived snapshot
// projected onto a post, see snapshotToPost).
export function toSiteView(post: Shelfy.Post): SiteView {
  const url = post.webFinalUrl || post.postUrl || post.webUrl || '';
  const domain = (post.webDomain || hostOf(url)).replace(/^www\./, '');
  const meta = readMeta(post.webMeta);
  const pages = list(post.webPages)
    .map((p, i) => toPage(p, i, url))
    .filter((p): p is PageView => p !== null);
  const isV2 = pages.some((p) => !!p.hero && (p.sections.length > 0 || !!p.footer || !!p.pageType));
  const first = pages[0];
  let cover: ImageRef | null = first?.hero || null;
  if (!cover && meta.ogImagePath) cover = { path: meta.ogImagePath, width: 0, height: 0 };
  if (!cover) {
    const p = post.imagePath || post.thumbnailPath;
    if (p) cover = { path: p, width: 0, height: 0 };
  }
  // v1 heroes are the whole page in one tall frame (or its first band).
  const coverIsTall = !!cover && cover.height > cover.width * 0.8 && !isV2;
  const legacy: LegacyAi = {
    description: post.aiDescription || '',
    tags: strList(post.aiTags),
    category: post.aiCategory || '',
    contentType: post.aiContentType || '',
    language: post.aiLanguage || '',
    saveReason: post.aiSaveReason || '',
    entities: strList(post.aiEntities),
    keywords: strList(post.aiKeywords),
    model: post.aiModel || '',
  };
  const tech = meta.tech.length
    ? meta.tech
    : list(post.webTech)
        .map(toTech)
        .filter((x): x is TechEntry => x !== null);
  return {
    id: post.id,
    name: displayName(meta, [post.authorName, domain]) || domain || url,
    domain,
    url,
    favicon: meta.favicon,
    cover,
    coverIsTall,
    palette: list(post.webPalette)
      .map(toSwatch)
      .filter((s): s is Swatch => s !== null),
    fonts: list(post.webFonts)
      .map(toFont)
      .filter((f): f is FontView => f !== null),
    tech,
    awards: list(post.webAwards)
      .map(toAward)
      .filter((a): a is AwardView => a !== null),
    pages,
    meta,
    ai: post.aiWeb && typeof post.aiWeb === 'object' ? post.aiWeb : null,
    legacy,
    capturedAt: post.webCapturedAt ? post.webCapturedAt * 1000 : null,
    isV2,
    hasCapture: pages.length > 0 || !!cover,
  };
}

// An archived snapshot as a post-like object. Snapshots predate/omit the v2 AI
// catalog, so `aiWeb` is cleared and the legacy ai_* fields frozen at that
// capture are shown instead.
export function snapshotToPost(snap: Shelfy.WebSnapshot, base: Shelfy.Post): Shelfy.Post {
  return {
    ...base,
    authorName: snap.title || base.authorName,
    aiWeb: null,
    aiDescription: snap.aiDescription || null,
    aiTags: snap.aiTags || [],
    aiModel: snap.aiModel || null,
    aiCategory: snap.aiCategory || null,
    aiContentType: snap.aiContentType || null,
    aiEntities: snap.aiEntities || [],
    aiKeywords: snap.aiKeywords || [],
    aiLanguage: snap.aiLanguage || null,
    aiSaveReason: snap.aiSaveReason || null,
    webPalette: snap.webPalette || [],
    webFonts: snap.webFonts || [],
    webTech: snap.webTech || [],
    webAwards: snap.webAwards || [],
    webPages: snap.webPages || [],
    webMeta: snap.webMeta || base.webMeta,
    webCapturedAt: snap.capturedAt || null,
  };
}

// ── Colour helpers ──────────────────────────────────────────────────────────

export function normHex(v: string): string | null {
  const s = v.trim().replace(/^#/, '');
  if (/^[0-9a-f]{3}$/i.test(s))
    return `#${s
      .split('')
      .map((c) => c + c)
      .join('')
      .toLowerCase()}`;
  if (/^[0-9a-f]{6}$/i.test(s)) return `#${s.toLowerCase()}`;
  if (/^[0-9a-f]{8}$/i.test(s)) return `#${s.slice(0, 6).toLowerCase()}`;
  return null;
}

// Relative luminance → readable ink on top of a swatch.
export function inkOn(hex: string): string {
  const h = normHex(hex);
  if (!h) return '#fff';
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16) / 255);
  const lin = (c: number): number => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const L = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
  return L > 0.45 ? '#111' : '#fff';
}

const ROLE_RANK: Record<string, number> = {
  background: 0,
  accent: 1,
  surface: 2,
  text: 3,
  image: 4,
};

// The 5-swatch strip for a card: the dominant background, then the strongest
// accents, then surface/text — distinct hexes only.
export function stripColors(palette: Swatch[], n = 5): string[] {
  const sorted = [...palette].sort((a, b) => {
    const ra = ROLE_RANK[a.role] ?? 2;
    const rb = ROLE_RANK[b.role] ?? 2;
    if (ra !== rb) return ra - rb;
    return (b.coverage ?? 0) - (a.coverage ?? 0);
  });
  const bg = sorted.filter((s) => s.role === 'background').slice(0, 1);
  const accents = sorted.filter((s) => s.role === 'accent').slice(0, 3);
  const rest = sorted.filter((s) => !bg.includes(s) && !accents.includes(s));
  const out: string[] = [];
  for (const s of [...bg, ...accents, ...rest]) {
    if (!out.includes(s.hex)) out.push(s.hex);
    if (out.length >= n) break;
  }
  return out;
}

// WCAG grade of a contrast ratio.
export function contrastGrade(ratio: number): 'AAA' | 'AA' | 'AA18' | 'fail' {
  if (ratio >= 7) return 'AAA';
  if (ratio >= 4.5) return 'AA';
  if (ratio >= 3) return 'AA18';
  return 'fail';
}

// CSS font stack for a captured family: the family itself (rendered when the
// user has it installed) then a generic fallback matching its classification.
export function fontStack(f: FontView): string {
  const generic =
    f.classification === 'serif'
      ? 'Georgia, serif'
      : f.classification === 'mono' || f.role === 'mono'
        ? 'ui-monospace, monospace'
        : f.classification === 'script'
          ? 'cursive'
          : 'system-ui, sans-serif';
  return `"${f.family.replace(/"/g, '')}", ${generic}`;
}

// Capitalises raw lowercased facet values with no vocabulary (fonts, tech):
// "google tag manager" → "Google Tag Manager", "next.js" → "Next.js".
export function titleCase(v: string): string {
  return v.replace(/(^|[\s-])([a-z])/g, (_m, p: string, c: string) => p + c.toUpperCase());
}

export function formatDuration(sec: number): string {
  const s = Math.max(0, Math.round(sec));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

// ── Facet filters ───────────────────────────────────────────────────────────

export type FacetSelection = Record<string, string[]>;
export type FacetCounts = Record<string, { value: string; count: number }[]>;

// Most useful first (what a designer filters by when hunting a reference).
export const FACET_ORDER = [
  'siteType',
  'industry',
  'style',
  'theme',
  'colorMood',
  'layout',
  'hero',
  'typography',
  'font',
  'tech',
  'motion',
  'award',
  'craft',
  'density',
  'imagery',
  'components',
  'fontClass',
  'scheme',
  'color',
] as const;

// Facets whose values are proper names (no vocabulary to localise).
export const RAW_FACETS = new Set(['font', 'tech', 'award']);

export function toggleFacet(sel: FacetSelection, facet: string, value: string): FacetSelection {
  const v = value.toLowerCase();
  const cur = sel[facet] || [];
  const next = cur.includes(v) ? cur.filter((x) => x !== v) : [...cur, v];
  const out = { ...sel };
  if (next.length) out[facet] = next;
  else delete out[facet];
  return out;
}

export function selectionSize(sel: FacetSelection): number {
  return Object.values(sel).reduce((n, v) => n + v.length, 0);
}

// The AI catalog's facets, in the panel order (deterministic facets included).
export function catalogFacets(ai: Shelfy.WebAiCatalog | null): [string, string[]][] {
  const f = ai && obj(ai.facets) ? (ai.facets as Record<string, unknown>) : {};
  return FACET_ORDER.map((k): [string, string[]] => [k, strList(f[k])]).filter(
    ([, v]) => v.length > 0,
  );
}
