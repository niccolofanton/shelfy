// Web-reference capture orchestrator (F10).
//
// A queue twin of analyzer.js / downloader.js, dedicated to the "paste a URL →
// screenshot + category + tags" pipeline. It does NOT implement discovery,
// screenshotting, enrichment or AI — it CHAINS the wave 1/2 modules:
//
//   discovering (webcapture.discoverPages)
//     → capturing  (webcapture.capturePage per page, buildWebMetadata + extractContent
//                   on the live home, then dispose())
//     → extracting (web-enrich.aggregateSiteText + detectAwards + awardsToTagsEntities)
//     → upsert     (db.upsertWebReference — promotes the placeholder to a full row)
//     → analyzing  (analyzer.enqueuePost — delegated to the shared AI queue)
//     → done
//
// Placeholder-first: enqueueWeb() creates a raw web post immediately (db.create
// WebPlaceholder) and fires 'interceptor:newPosts' so the gallery shows a card
// right away; the rest happens in the background. The job record is emitted on
// 'web:progress' at every transition, exactly like download:progress/analyze:
// progress, so the renderer can reuse the same hook pattern.
//
// Partial-persistence rule: if ≥1 screenshot was captured but enrichment/AI fail,
// the reference is saved anyway (status 'done', partial:true) — only a total
// failure (no usable screenshot) is a retryable 'error'.

import * as db from './db';
import * as jobstore from './jobstore';
import * as webcapture from './webcapture';
import * as captureEngine from './capture-engine';
import * as enrich from './web-enrich';
import * as analyzer from './analyzer';
import { assertSafeUrl } from './net-safety';

const KIND = 'web'; // jobstore namespace for this queue

// ─── Internal types ─────────────────────────────────────────────────────────

// The branding metadata enrich.buildWebMetadata produces and the renderer
// consumes: palette swatches ({ hex, role, weight }) and fonts ({ family,
// usage, provider }) are RICH objects, not bare strings — the web_*_json columns
// store them verbatim and PostCard/WebMetaPanel read .hex/.family/.usage. The
// concrete element interfaces live in web-enrich; derive them from its return
// type so this file stays the single source of truth without re-declaring them.
type WebMetadata = Awaited<ReturnType<typeof enrich.buildWebMetadata>>;
type WebPalette = WebMetadata['palette'];
type WebFonts = WebMetadata['fonts'];

// The pipeline phases / job statuses. `phase` and `status` are distinct fields
// that draw from this same set: `status` uses 'pending' for the resting queued
// state, while `phase` uses 'queued' for it. 'done'/'cancelled'/'error' are
// terminal.
type Phase =
  | 'pending'
  | 'discovering'
  | 'capturing'
  | 'extracting'
  | 'analyzing'
  | 'queued'
  | 'done'
  | 'cancelled'
  | 'error';

// Phases that carry a progress weight (the four active pipeline stages).
type WeightedPhase = 'discovering' | 'capturing' | 'extracting' | 'analyzing';

// One event in a job's append-only timeline (see "Event timeline" below).
type EventKind = 'read' | 'artifact' | 'branding' | 'awards' | 'write' | 'info' | 'error';

interface JobEvent {
  id: number;
  ts: number;
  phase: Phase | undefined;
  kind: EventKind;
  text: string;
  data?: unknown;
}

// The slim per-page shape the renderer consumes (job.pages and, after upsert,
// post.webPages). Carries the screenshot chunks so the gallery thumbnail uses
// the light top band while the lightbox can stack every band.
interface JobPage {
  url: string;
  screenshotPath: string;
  chunks?: Shelfy.WebPageChunk[];
  width: number;
  height: number;
}

// In-memory web-scan job record (distinct from the persisted Shelfy.Job mirror):
// the serializable snapshot streamed to the Websites view and kept in jobsMap.
// `key` is jobKey(postId).
interface WebJobRecord {
  key: string;
  postId: string;
  url: string;
  finalUrl: string;
  domain: string | null;
  maxPages: number;
  overwrite: boolean;
  singlePage: boolean;
  placeholderCreated: boolean;
  status: Phase;
  phase: Phase;
  progress: number;
  phaseProgress: number;
  stage: string | null;
  partial: boolean;
  pagesTotal: number;
  pagesDone: number;
  error: string | null;
  queuedAt: number;
  startedAt: number | null;
  finishedAt: number | null;
  title: string | null;
  screenshotPath: string | null;
  source: string | null;
  lang: string | null;
  palette: WebPalette;
  fonts: WebFonts;
  techStack: string[];
  awards: Shelfy.WebAward[];
  pages: JobPage[];
  events: JobEvent[];
}

// The progress emitter set by main: receives a fresh snapshot on every transition.
type JobUpdateEmitter = (job: WebJobRecord) => void;

// The list-refresh emitter set by main: fires 'interceptor:newPosts'.
interface ListRefreshPayload {
  count: number;
  platform: 'web';
  refresh?: boolean;
}
type ListRefreshEmitter = (payload: ListRefreshPayload) => void;

// A captured page, internal to the pipeline (richer than the renderer's JobPage):
// keeps the raw html + extracted content + the WebGL probe so QC/re-capture and
// enrichment can use them.
interface CapturedPage {
  url: string;
  screenshotPath: string;
  chunks?: Shelfy.WebPageChunk[];
  width: number;
  height: number;
  html: string;
  content: WebContent | null;
  webglHeavy: boolean;
  usedOgImage?: boolean;
}

// The shape enrich.extractContent returns (the subset this module reads).
interface WebContent {
  url: string;
  title: string;
  metaDescription: string;
  og: { image?: string; [k: string]: unknown };
  twitter: { image?: string; [k: string]: unknown };
  jsonld: unknown;
  headings: unknown;
  mainText: string;
  textLength: number;
  truncated: boolean;
  lang: string;
}

// The aggregate enrich.aggregateSiteText returns. webMeta mirrors the exact
// closed shape that function produces (siteName/title/description/…); it's a
// structural instance of Shelfy.WebMeta but, lacking that type's open index
// signature, can't be *typed* as it directly — see the cast at the upsert site.
type SiteAggregate = ReturnType<typeof enrich.aggregateSiteText>;

// The QC verdict analyzer.assessScreenshot returns; `midBand` is set locally when
// a tall page's middle band fails (see assessPage).
interface QcVerdict {
  ok?: boolean;
  status: string;
  ready?: boolean;
  reason?: string;
  midBand?: boolean;
}

// Options accepted by captureWebReference().
interface CaptureOptions {
  signal?: AbortSignal;
  maxPages?: number;
  onProgress?: (info: {
    postId: string;
    url: string;
    phase: WeightedPhase;
    fraction: number;
    [k: string]: unknown;
  }) => void;
  overwrite?: boolean;
  singlePage?: boolean;
}

// Options accepted by enqueueWeb().
interface EnqueueOptions {
  maxPages?: number;
  overwrite?: boolean;
  recovered?: boolean;
  singlePage?: boolean;
}

// The synchronous-ish handle enqueueWeb() returns.
interface EnqueueResult {
  id: string;
  finalUrl: string;
  domain: string | null;
  queued: boolean;
}

// ─── Constants ──────────────────────────────────────────────────────────────

const WEB_CONCURRENCY = 1; // one site/job at a time
// Within a single site, capture this many pages concurrently. Each page renders in
// its own Playwright context inside the shared browser (isolated cookies/storage),
// so 4-up parallelism cuts the per-page ~15s settle from a long sequential sum to
// roughly ceil(pages/4) batches of wall-clock.
const CAPTURE_PAGE_CONCURRENCY = 4;
// WebGL-heavy sites: N concurrent Three.js/OGL scenes contend for ONE GPU —
// intros/preloaders slow down, timeouts multiply, and the shared browser's GPU
// process can crash. The home capture probes the site (pageCtx.webglHeavy) and
// the remaining pages then run at this reduced parallelism.
const CAPTURE_PAGE_CONCURRENCY_WEBGL = 2;
const DEFAULT_MAX_PAGES = 6;
const MAX_PAGES_CLAMP = 8;

// Per-phase timeouts (abort the phase, not necessarily the whole job — partial
// persistence keeps a job alive past a failed enrichment phase). Composed with
// the job's own abort signal via AbortSignal.any (pattern from downloader.js).
const DISCOVER_TIMEOUT_MS = 15_000;
// A capture can legitimately take: goto ≤45s + networkidle ≤20s + preloader wait
// ≤30s + autoscroll ≤30s + settles ~6s + shot ≤30s. The old 60s budget killed
// precisely the heavy (WebGL/preloader) sites this feature targets — and a
// timed-out page gets no QC re-capture nor og:image fallback. Generous beats
// truncated here; typical pages finish far earlier.
const CAPTURE_TIMEOUT_PER_PAGE_MS = 150_000;
const EXTRACT_TIMEOUT_MS = 20_000;
// Re-capture of a page the QC model flagged as black/loading: open it again and
// wait 30s for it to finish rendering, with a correspondingly longer budget
// (nav ≤45s + ready ≤30s + autoscroll ≤30s + settle 30s + shot ≤30s).
const RECAPTURE_WAIT_MS = 30_000;
const RECAPTURE_TIMEOUT_MS = 180_000;
// Screenshot-QC assessment pool: verdicts are independent per page and the VLM
// server is spawned with --parallel slots, so a few checks can run at once.
// Kept small so a concurrent tag-analysis batch isn't starved of slots.
const QC_ASSESS_CONCURRENCY = 3;

// Progress weights per phase (sum to 1.0); see spec F10 §2.3.
const PHASE_WEIGHTS: Record<WeightedPhase, number> = {
  discovering: 0.1,
  capturing: 0.55,
  extracting: 0.25,
  analyzing: 0.1,
};
const PHASE_BASE: Record<WeightedPhase, number> = {
  discovering: 0,
  capturing: 0.1,
  extracting: 0.65,
  analyzing: 0.9,
};

// ─── State (mirror of analyzer.js / downloader.js) ────────────────────────────

const jobsMap = new Map<string, WebJobRecord>(); // key → serializable job record
const urlCache = new Map<string, string>(); // key → original url (needed for retry)
const abortMap = new Map<string, AbortController>(); // key → AbortController (active jobs only)
const pendingQueue: string[] = []; // ordered keys awaiting execution
const pausedKeys = new Set<string>(); // keys aborted by pause → re-queue instead of cancel

let isPaused = false;
let runningCount = 0;
let onJobUpdate: JobUpdateEmitter | null = null; // (job) => void — set via setProgressEmitter

// Optional refresh emitter, set by main: fires 'interceptor:newPosts' so the
// gallery re-fetches when a placeholder appears or a reference is promoted.
let onListRefresh: ListRefreshEmitter | null = null; // ({ count, platform, refresh }) => void

// ─── Helpers ──────────────────────────────────────────────────────────────────

function jobKey(postId: string): string {
  return `web:${postId}`;
}

function setJob(job: WebJobRecord): void {
  jobsMap.set(job.key, { ...job });
  jobstore.mirror(KIND, job);
  onJobUpdate?.({ ...job });
}

function patchJob(key: string, patch: Partial<WebJobRecord>): void {
  const j = jobsMap.get(key);
  if (!j) return; // key cleared (clearAll) → no-op, no spurious event
  setJob({ ...j, ...patch });
}

// Maps a phase-local fraction (0..1) onto the global progress bar.
function phaseProgress(phase: WeightedPhase, frac: number): number {
  const f = Math.max(0, Math.min(1, Number(frac) || 0));
  const base = PHASE_BASE[phase] ?? 0;
  const weight = PHASE_WEIGHTS[phase] ?? 0;
  return base + f * weight;
}

// Extra job-record fields a phase transition can carry alongside the auto-managed
// status/phase/progress/phaseProgress (which emitPhase sets itself).
type PhaseExtra = Omit<Partial<WebJobRecord>, 'status' | 'phase' | 'progress' | 'phaseProgress'>;

// Emit a phase transition / sub-progress. `frac` is the fraction WITHIN the phase.
function emitPhase(key: string, phase: WeightedPhase, frac: number, extra: PhaseExtra = {}): void {
  patchJob(key, {
    status: phase,
    phase,
    phaseProgress: Math.max(0, Math.min(1, Number(frac) || 0)),
    progress: phaseProgress(phase, frac),
    ...extra,
  });
}

// ─── Event timeline ────────────────────────────────────────────────────────────
// A per-job append-only log of EVERYTHING the pipeline does — what it reads, what
// artefacts it produces, what it writes — so the Websites panel can narrate the
// behind-the-scenes work step by step. Each event is { id, ts, phase, kind, text,
// data? }; kind ∈ read | artifact | branding | awards | write | info | error.
// Capped so a pathological run can't grow the record unbounded. The full array
// rides along on every setJob() emit (structured-cloned over IPC), so the
// renderer always has the complete, ordered list.
const EVENTS_CAP = 250;
let eventSeq = 0;

function pushEvent(key: string, kind: EventKind, text: string, data?: unknown): void {
  const j = jobsMap.get(key);
  if (!j) return; // key cleared → no-op
  const prev = Array.isArray(j.events) ? j.events : [];
  const evt: JobEvent = { id: ++eventSeq, ts: Date.now(), phase: j.phase, kind, text };
  if (data !== undefined) evt.data = data;
  const base = prev.length >= EVENTS_CAP ? prev.slice(prev.length - EVENTS_CAP + 1) : prev.slice();
  base.push(evt);
  setJob({ ...j, events: base });
}

// Human-readable label for how discovery found the pages.
const SOURCE_LABEL: Record<string, string> = {
  sitemap: 'Pagine trovate dalla sitemap',
  crawl: 'Pagine trovate esplorando la home',
  'seed-only': 'Nessuna sitemap: solo la pagina iniziale',
  'single-page': 'Solo questa pagina',
};

// Run `worker(item, i)` over `items` with at most `limit` in flight at once.
// Results are returned positionally (results[i] = worker's return for item i), so
// ordering is preserved regardless of completion order. A worker that throws
// rejects the whole pool (used here only for a genuine job-abort; per-page failures
// are swallowed inside the worker and surface as a null result).
//
// When `signal` is given, each runner loop bails BEFORE pulling the next item once
// the signal is aborted: a cancelled job must not keep spinning up (and tearing
// down) fresh Playwright contexts for the remaining indices after one in-flight
// worker has rethrown the abort.
async function runPool<T, R>(
  items: T[],
  limit: number,
  worker: (item: T, i: number) => Promise<R>,
  signal?: AbortSignal,
): Promise<R[]> {
  const results = new Array<R>(items.length);
  let next = 0;
  async function runner(): Promise<void> {
    for (;;) {
      if (signal?.aborted) return; // job cancelled → stop claiming new items
      const i = next++;
      if (i >= items.length) return;
      results[i] = await worker(items[i], i);
    }
  }
  const n = Math.max(1, Math.min(limit, items.length));
  await Promise.all(Array.from({ length: n }, () => runner()));
  return results;
}

// Map an internal captured page → the slim shape the renderer consumes (job.pages
// and, after upsert, post.webPages). Carries the screenshot chunks so the gallery
// thumbnail uses the light top band while the lightbox can stack every band.
function toJobPage(p: CapturedPage): JobPage {
  return {
    url: p.url,
    screenshotPath: p.screenshotPath,
    chunks: Array.isArray(p.chunks) ? p.chunks : undefined,
    width: p.width,
    height: p.height,
  };
}

// Trim a URL to "/path" (origin stripped) for compact log lines; keep "/" for home.
function shortPath(u: string): string {
  try {
    const x = new URL(u);
    return (x.pathname || '/') + (x.search || '');
  } catch {
    return String(u || '');
  }
}

// Compose the job abort signal with a per-phase timeout. Returns { signal, done }
// where done() clears the timer. Falls back gracefully when AbortSignal.any is
// unavailable (old runtimes).
function withTimeout(
  jobSignal: AbortSignal | undefined,
  ms: number,
): { signal: AbortSignal; done: () => void } {
  const timeoutAc = new AbortController();
  const timer = setTimeout(() => timeoutAc.abort(), ms);
  const signals = [timeoutAc.signal, ...(jobSignal ? [jobSignal] : [])];
  const signal =
    typeof AbortSignal.any === 'function'
      ? AbortSignal.any(signals)
      : jobSignal || timeoutAc.signal;
  return { signal, done: () => clearTimeout(timer) };
}

function isAbortErr(err: unknown): boolean {
  const e = err as { name?: unknown; message?: unknown } | null | undefined;
  return e?.name === 'AbortError' || e?.message === 'AbortError';
}

function clampMaxPages(n: unknown): number {
  const v = Math.floor(Number(n));
  if (!Number.isFinite(v)) return DEFAULT_MAX_PAGES;
  return Math.max(1, Math.min(MAX_PAGES_CLAMP, v));
}

// Vertical screenshot bands. The capture-engine PageCtx contract does not declare
// `chunks` (only the OSR/Playwright engines may attach it opportunistically), so
// read it defensively off the live ctx — exactly as the original code did with the
// `Array.isArray(ctx.chunks)` guard — and return undefined when absent.
function ctxChunks(ctx: unknown): Shelfy.WebPageChunk[] | undefined {
  const c = (ctx as { chunks?: unknown } | null | undefined)?.chunks;
  return Array.isArray(c) ? (c as Shelfy.WebPageChunk[]) : undefined;
}

// ─── Pipeline executor ────────────────────────────────────────────────────────

// The single job run by the queue. Placeholder already exists (created at
// enqueue). Drives the phases, persists, and delegates AI. Returns nothing —
// state lives in the job record. Throws on a non-recoverable failure so runJob's
// catch can decide cancelled vs error.
async function captureWebReference(
  url: string,
  {
    signal,
    maxPages = DEFAULT_MAX_PAGES,
    onProgress,
    overwrite = false,
    singlePage = false,
  }: CaptureOptions = {},
): Promise<void> {
  const key = jobKey(db.webPostId(url));
  // Single-page mode captures exactly the pasted URL — no sitemap/crawl, so a max
  // of one page regardless of the maxPages hint.
  const cap = singlePage ? 1 : clampMaxPages(maxPages);
  // One epoch for the whole capture: used both as web_captured_at and as the
  // screenshot filename prefix, so this version's files never collide with a
  // prior capture's (version history).
  const captureStamp = Math.floor(Date.now() / 1000);
  const report = (phase: WeightedPhase, frac: number, extra?: PhaseExtra): void => {
    emitPhase(key, phase, frac, extra);
    try {
      onProgress?.({ postId: db.webPostId(url), url, phase, fraction: frac, ...extra });
    } catch {}
  };

  // ── Phase 1: discovering ──────────────────────────────────────────────────
  report('discovering', 0, { stage: singlePage ? 'Pagina singola…' : 'Ricerca delle pagine…' });
  pushEvent(key, 'read', `Apertura di ${url}`);
  let discovery: {
    finalUrl: string;
    domain: string | null;
    origin: string | null;
    source: string;
    pages: { url: string }[];
  } | null;
  if (singlePage) {
    // Skip sitemap discovery/ranking entirely: capture only the pasted URL (an
    // article/guide). The redirect-resolved finalUrl + domain come from the
    // capture step itself; here we just seed the single page.
    let domainHint: string | null = null;
    try {
      domainHint = new URL(url).hostname.replace(/^www\./, '');
    } catch {}
    discovery = {
      finalUrl: url,
      domain: domainHint,
      origin: null,
      source: 'single-page',
      pages: [{ url }],
    };
  } else {
    const { signal: s, done } = withTimeout(signal, DISCOVER_TIMEOUT_MS);
    try {
      discovery = await webcapture.discoverPages(url, { maxPages: cap, signal: s });
    } catch (err) {
      // A genuine cancel propagates; a discovery failure degrades to seed-only.
      if (isAbortErr(err) && signal?.aborted) throw err;
      discovery = null;
    } finally {
      done();
    }
  }
  signal?.throwIfAborted?.();

  const finalUrl = (discovery && discovery.finalUrl) || url;
  const domain = (discovery && discovery.domain) || null;
  const source = (discovery && discovery.source) || 'seed-only';
  const discPages =
    discovery && Array.isArray(discovery.pages) && discovery.pages.length
      ? discovery.pages
      : [{ url: finalUrl }];
  report('discovering', 1, {
    finalUrl,
    domain,
    source,
    pagesTotal: discPages.length,
    pagesDone: 0,
  });
  if (domain && finalUrl !== url) pushEvent(key, 'read', `Risolto in ${finalUrl}`, { finalUrl });
  pushEvent(
    key,
    'info',
    `${SOURCE_LABEL[source] || 'Pagine individuate'} — ${discPages.length} pagine`,
    {
      source,
      pages: discPages.map((p) => p.url || finalUrl),
    },
  );

  // ── Phase 2: capturing ──────────────────────────────────────────────────────
  // Up to CAPTURE_PAGE_CONCURRENCY pages render at once, each in its own Playwright
  // context inside the shared browser. The home (index 0) additionally harvests
  // buildWebMetadata WHILE its context is alive. Results are placed by index so the
  // hero stays first; failed pages become null and are compacted out afterwards.
  const total = discPages.length;
  let webMetadata: WebMetadata | null = null; // { palette, fonts, techStack } from the home
  let homeFinalUrl = finalUrl;
  let pagesDone = 0;
  // Positional results (live[i] filled as page i lands) → incremental thumbnails in
  // discovery order regardless of which page finishes first.
  const live: (CapturedPage | null)[] = new Array(total).fill(null);

  report('capturing', 0, { stage: `Cattura di ${total} pagine`, pagesTotal: total, pagesDone: 0 });

  const capturePageTask = async (
    discPage: { url: string } | undefined,
    i: number,
  ): Promise<CapturedPage | null> => {
    const pageUrl = (discPage && discPage.url) || finalUrl;
    pushEvent(key, 'read', `Cattura pagina ${i + 1}/${total}: ${shortPath(pageUrl)}`);
    const { signal: s, done } = withTimeout(signal, CAPTURE_TIMEOUT_PER_PAGE_MS);
    let ctx: Awaited<ReturnType<typeof captureEngine.capturePage>> | null = null;
    try {
      ctx = await captureEngine.capturePage(pageUrl, {
        format: 'webp',
        quality: 82,
        signal: s,
        captureStamp,
        onStep: (label: string, delta: number, tot: number) => {
          if (delta >= 2000)
            pushEvent(
              key,
              'info',
              `⏱ ${label}: ${(delta / 1000).toFixed(1)}s (tot ${(tot / 1000).toFixed(1)}s)`,
            );
        },
      });
      const shotUrl = ctx.finalUrl || pageUrl;
      pushEvent(
        key,
        'artifact',
        `Screenshot ${ctx.width}×${ctx.height}px (webp${ctx.capped ? ', altezza limitata' : ''})`,
        { screenshotPath: ctx.screenshotPath, width: ctx.width, height: ctx.height, url: shotUrl },
      );
      // The context stays alive until dispose(): harvest live-DOM signals first.
      if (i === 0) {
        homeFinalUrl = ctx.finalUrl || finalUrl;
        try {
          // Pass the per-page signal so CAPTURE_TIMEOUT_PER_PAGE_MS can cancel the
          // ffmpeg palette fallback (web-enrich also self-bounds it to 10s).
          webMetadata = await enrich.buildWebMetadata(ctx, s);
        } catch {
          webMetadata = null;
        }
        if (webMetadata) {
          const pal = webMetadata.palette || [];
          const fnt = webMetadata.fonts || [];
          const tech = webMetadata.techStack || [];
          patchJob(key, { title: ctx.title || null, palette: pal, fonts: fnt, techStack: tech });
          pushEvent(
            key,
            'branding',
            `Branding: ${pal.length} colori, ${fnt.length} font, ${tech.length} tecnologie`,
            { palette: pal, fonts: fnt, techStack: tech },
          );
        }
      }
      let content: WebContent | null = null;
      try {
        content = enrich.extractContent(ctx.html, shotUrl);
      } catch {
        content = null;
      }
      if (content) {
        pushEvent(
          key,
          'read',
          `Contenuto: ${content.textLength || 0} caratteri${content.lang ? `, lingua ${content.lang}` : ''}`,
          { title: content.title || '', lang: content.lang || '', chars: content.textLength || 0 },
        );
      }
      live[i] = {
        url: shotUrl,
        screenshotPath: ctx.screenshotPath,
        // Vertical screenshot bands (≤2000px each): the renderer stacks several
        // light images instead of decoding one heavyweight full-page frame.
        chunks: ctxChunks(ctx),
        width: ctx.width,
        height: ctx.height,
        html: ctx.html,
        content,
        // WebGL probe from the engine — drives the per-site parallelism choice.
        webglHeavy: !!ctx.webglHeavy,
      };
      return live[i];
    } catch (err) {
      // A cancel of the WHOLE job stops everything → rethrow so the pool rejects.
      if (isAbortErr(err) && signal?.aborted) {
        try {
          await ctx?.dispose?.();
        } catch {}
        done();
        throw err;
      }
      // A single page failing (per-page timeout / nav error) just skips that page.
      const e = err as { name?: unknown; message?: unknown } | null | undefined;
      const reason =
        isAbortErr(err) || e?.name === 'TimeoutError' || /timeout/i.test(String(e?.message || ''))
          ? 'timeout'
          : (typeof e?.message === 'string' && e.message) || 'errore';
      pushEvent(key, 'info', `Pagina saltata: ${shortPath(pageUrl)} (${reason})`);
      return null;
    } finally {
      try {
        await ctx?.dispose?.();
      } catch {}
      done();
      // Progress + incremental thumbnails as each page lands (any completion order).
      pagesDone++;
      report('capturing', pagesDone / total, { pagesTotal: total, pagesDone });
      patchJob(key, { pages: live.filter(Boolean).map((p) => toJobPage(p!)) });
    }
  };

  // Home FIRST, alone: it harvests the branding metadata anyway, and its capture
  // probes whether the site is WebGL-heavy — if so, the remaining pages run at
  // reduced parallelism to avoid GPU contention (slower intros → timeouts).
  await capturePageTask(discPages[0], 0);
  if (discPages.length > 1) {
    signal?.throwIfAborted?.();
    const limit = live[0]?.webglHeavy ? CAPTURE_PAGE_CONCURRENCY_WEBGL : CAPTURE_PAGE_CONCURRENCY;
    if (limit < CAPTURE_PAGE_CONCURRENCY)
      pushEvent(key, 'info', `Sito WebGL: parallelismo ridotto a ${limit} pagine per volta`);
    await runPool(discPages.slice(1), limit, (p, i) => capturePageTask(p, i + 1), signal);
  }
  // Compact out skipped pages, preserving discovery order (hero first).
  const captured = live.filter((p): p is CapturedPage => Boolean(p));
  signal?.throwIfAborted?.();

  // ── Phase 2b: screenshot QC + re-capture ──────────────────────────────────────
  // The local VLM looks at each screenshot for the SOLE purpose of telling whether
  // the page actually rendered, or is black / blank / a loading screen / partial.
  // Any flagged page is re-captured once, opening it again and waiting 30s so a
  // slow (WebGL/lazy) page can finish. Fail-open: if the model isn't ready the
  // assessment returns 'unknown' and nothing is re-captured.

  // Part A — og:image fallback: when a page never yields a usable frame, the
  // site's own curated social preview beats a black/blank screenshot, both as
  // tagging input and as gallery thumbnail. Shared by the "re-capture still bad"
  // and "re-capture itself failed (timeout)" paths.
  const applyOgFallback = async (cap: CapturedPage): Promise<boolean> => {
    const ogImg = cap.content?.og?.image || cap.content?.twitter?.image || '';
    if (!ogImg) return false;
    const ogPath = await webcapture
      .fetchImageToWebp(ogImg, { pageUrl: cap.url, stamp: captureStamp, signal })
      .catch(() => null);
    if (!ogPath) return false;
    cap.screenshotPath = ogPath;
    // The og:image is a single light frame → one chunk.
    cap.chunks = [{ screenshotPath: ogPath, width: 0, height: 0 } as Shelfy.WebPageChunk];
    cap.usedOgImage = true;
    pushEvent(key, 'artifact', `Schermata vuota: uso l'og:image di ${shortPath(cap.url)}`, {
      screenshotPath: ogPath,
      url: cap.url,
    });
    return true;
  };

  // Assessment pre-pass, PARALLEL: the QC verdicts are independent per page and
  // the local VLM server exposes multiple slots (llama-server --parallel), so the
  // hero/mid-band checks for all pages run through a small pool instead of one
  // page at a time. Only the (rare) re-captures below stay serial.
  const assessPage = async (cap: CapturedPage | null): Promise<QcVerdict | null> => {
    if (!cap || !cap.screenshotPath) return null;
    let qc: QcVerdict;
    try {
      qc = await analyzer.assessScreenshot(cap.screenshotPath, { signal });
    } catch {
      qc = { ok: true, status: 'unknown' };
    }
    // The hero band alone can't expose a page whose below-the-fold sections were
    // captured pre-reveal (un-triggered/reversed scroll animations → blank bands):
    // on tall captures ALSO probe a middle band and treat a bad verdict there as
    // a QC failure, so the page gets its re-capture.
    if (qc.ok && Array.isArray(cap.chunks) && cap.chunks.length >= 3) {
      const mid = cap.chunks[Math.floor(cap.chunks.length / 2)];
      if (mid?.screenshotPath && mid.screenshotPath !== cap.screenshotPath) {
        let qcMid: QcVerdict;
        try {
          qcMid = await analyzer.assessScreenshot(mid.screenshotPath, { signal });
        } catch {
          qcMid = { ok: true, status: 'unknown' };
        }
        if (!qcMid.ok) qc = { ...qcMid, midBand: true };
      }
    }
    return qc;
  };
  const qcByIndex = await runPool(captured, QC_ASSESS_CONCURRENCY, assessPage, signal);
  signal?.throwIfAborted?.();

  for (let i = 0; i < captured.length; i++) {
    const cap = captured[i];
    if (!cap || !cap.screenshotPath) continue;
    signal?.throwIfAborted?.();

    const qc = qcByIndex[i] || { ok: true, status: 'unknown' };
    pushEvent(
      key,
      'info',
      `Controllo qualità ${shortPath(cap.url)}: ${qc.status}${qc.midBand ? ' (sezione centrale)' : ''}${qc.reason ? ` — ${qc.reason}` : ''}`,
    );
    if (qc.ok) continue; // 'ok' or 'unknown' (model not ready / failed) → keep as-is

    pushEvent(
      key,
      'info',
      `Schermata "${qc.status}": ricattura di ${shortPath(cap.url)} con attesa di ${RECAPTURE_WAIT_MS / 1000}s`,
    );
    report('capturing', 1, { stage: `Ricattura ${shortPath(cap.url)}` });
    const { signal: s2, done: done2 } = withTimeout(signal, RECAPTURE_TIMEOUT_MS);
    let ctx2: Awaited<ReturnType<typeof captureEngine.capturePage>> | null = null;
    try {
      ctx2 = await captureEngine.capturePage(cap.url, {
        format: 'webp',
        quality: 82,
        signal: s2,
        captureStamp,
        settleBeforeShotMs: RECAPTURE_WAIT_MS,
        onStep: (label: string, delta: number, t: number) => {
          if (delta >= 2000)
            pushEvent(
              key,
              'info',
              `⏱ ${label}: ${(delta / 1000).toFixed(1)}s (tot ${(t / 1000).toFixed(1)}s)`,
            );
        },
      });
      cap.screenshotPath = ctx2.screenshotPath;
      cap.chunks = ctxChunks(ctx2);
      cap.width = ctx2.width;
      cap.height = ctx2.height;
      cap.html = ctx2.html;
      try {
        cap.content = enrich.extractContent(ctx2.html, ctx2.finalUrl || cap.url);
      } catch {
        /* keep prior content */
      }
      // Home: re-harvest palette/fonts/tech from the now-rendered page.
      if (i === 0) {
        try {
          const m = await enrich.buildWebMetadata(ctx2, s2);
          if (m) {
            webMetadata = m;
            patchJob(key, {
              palette: m.palette || [],
              fonts: m.fonts || [],
              techStack: m.techStack || [],
            });
          }
        } catch {
          /* keep prior metadata */
        }
      }
      let qc2: QcVerdict;
      try {
        qc2 = await analyzer.assessScreenshot(cap.screenshotPath, { signal });
      } catch {
        qc2 = { status: 'unknown' };
      }
      pushEvent(
        key,
        'artifact',
        `Ricattura ${shortPath(cap.url)}: ${ctx2.width}×${ctx2.height}px (qualità: ${qc2.status})`,
        {
          screenshotPath: cap.screenshotPath,
          width: ctx2.width,
          height: ctx2.height,
          url: cap.url,
        },
      );

      // If the re-capture STILL isn't a usable frame (a page so WebGL-bound it
      // never paints a stable still even under the real browser), fall back to
      // the site's own social-preview image.
      if (!qc2.ok && qc2.status !== 'unknown') {
        await applyOgFallback(cap);
      }
    } catch (err) {
      if (isAbortErr(err) && signal?.aborted) {
        try {
          await ctx2?.dispose?.();
        } catch {}
        done2();
        throw err;
      }
      pushEvent(key, 'info', `Ricattura non riuscita: ${shortPath(cap.url)}`);
      // The original screenshot was already QC-flagged: a re-capture that FAILED
      // outright (typically the per-page timeout on a heavy site) must still fall
      // back to the og:image rather than silently keeping the black/loading frame.
      // Exception: a mid-band-only failure means the hero is fine — keeping the
      // original full capture beats degrading to a lone social-preview image.
      if (!qc.midBand) await applyOgFallback(cap);
    } finally {
      try {
        await ctx2?.dispose?.();
      } catch {}
      done2();
    }
    patchJob(key, {
      pages: captured.filter((p) => p && p.screenshotPath).map(toJobPage),
    });
  }
  signal?.throwIfAborted?.();

  const withShots = captured.filter((p) => p && p.screenshotPath);
  if (!withShots.length) {
    // Nothing usable captured AND discovery gave us nothing → hard error (retryable).
    // Tagged so runJob's error path can delete a placeholder THIS enqueue created
    // (a save failure below, by contrast, keeps the placeholder for retry).
    throw Object.assign(new Error('Cattura non riuscita: nessuno screenshot prodotto.'), {
      noScreenshot: true,
    });
  }
  const partial = withShots.length < total; // at least one page failed to capture

  // ── Phase 3: extracting (enrich + awards) ─────────────────────────────────────
  report('extracting', 0, { stage: 'Estrazione contenuti e premi…' });
  pushEvent(key, 'info', `Aggregazione di ${withShots.length} pagine catturate`);
  let aggregate: SiteAggregate | null = null;
  let awards: Shelfy.WebAward[] = [];
  let awardTagsEntities: { tags: string[]; entities: string[] } = { tags: [], entities: [] };
  {
    const { signal: s, done } = withTimeout(signal, EXTRACT_TIMEOUT_MS);
    try {
      // aggregateSiteText accepts [{ title, content }] or PageContent[]; our
      // captured entries expose { content } → use the {title, content} shape.
      const pagesForText = captured.map((p) => ({
        title: p.content?.title || '',
        content: p.content || undefined,
      }));
      try {
        aggregate = enrich.aggregateSiteText(pagesForText);
      } catch {
        aggregate = null;
      }

      // detectAwards wants [{ url, html }]; pure & deterministic on the home HTML.
      try {
        const pagesForAwards = captured
          .filter((p) => typeof p.html === 'string')
          .map((p) => ({ url: p.url, html: p.html }));
        awards = enrich.detectAwards(pagesForAwards, homeFinalUrl) || [];
        awardTagsEntities = enrich.awardsToTagsEntities(awards) || { tags: [], entities: [] };
      } catch {
        awards = [];
        awardTagsEntities = { tags: [], entities: [] };
      }
    } finally {
      done();
    }
  }
  signal?.throwIfAborted?.();
  if (aggregate) {
    pushEvent(
      key,
      'read',
      `Testo del sito: ${(aggregate.contentText || '').length} caratteri${aggregate.lang ? `, lingua ${aggregate.lang}` : ''}`,
      {
        chars: (aggregate.contentText || '').length,
        lang: aggregate.lang || '',
      },
    );
  }
  patchJob(key, { lang: aggregate?.lang || null, awards });
  pushEvent(
    key,
    'awards',
    awards.length
      ? `${awards.length} riconoscimenti: ${awards.map((a) => a.platform).join(', ')}`
      : 'Nessun riconoscimento rilevato',
    { awards },
  );
  report('extracting', 1);

  // ── Phase upsert (promote placeholder → full reference) ───────────────────────
  // aggregate.webMeta is a closed shape; widen it to the open Shelfy.WebMeta the
  // row carries (only the index signature is structurally absent — values match).
  const webMeta: Shelfy.WebMeta = (aggregate?.webMeta as Shelfy.WebMeta | undefined) || {};
  const ref = {
    // Stable identity = the id assigned at enqueue (from the pasted URL), the same
    // one the placeholder row and the live job carry. Keeping it fixed (instead of
    // re-deriving from finalUrl) is what stops a home redirect ("/" → "/it") from
    // splitting the site into two records — see webRefToPost.
    id: db.webPostId(url),
    url,
    finalUrl: homeFinalUrl,
    domain: domain || (webMeta.siteName as string | undefined) || null,
    title:
      (webMeta.title as string | undefined) ||
      (webMeta.siteName as string | undefined) ||
      domain ||
      null,
    description:
      (webMeta.description as string | undefined) || captured[0]?.content?.metaDescription || null,
    lang: aggregate?.lang || captured[0]?.content?.lang || null,
    // Same epoch used for the screenshot filename prefix, so web_captured_at and
    // the on-disk files agree.
    capturedAt: captureStamp,
    // One slide per captured page; hero first (discovery order preserved).
    pages: withShots.map((p) => ({
      url: p.url,
      screenshotPath: p.screenshotPath,
      // Vertical screenshot bands, persisted so the lightbox can lazy-stack them.
      chunks: Array.isArray(p.chunks) ? p.chunks : undefined,
      contentText: p.content?.mainText || '',
      meta: p.content ? { ogImage: p.content.og?.image || '' } : undefined,
    })),
    // Deterministic enrichment (no AI yet).
    palette: webMetadata?.palette,
    fonts: webMetadata?.fonts,
    techStack: webMetadata?.techStack,
    awards,
    // Award-derived tags/entities feed post_tags/post_entities via the AI mapping
    // later; persist them here as part of webMeta so they survive even if AI fails.
    // singlePage rides along too (→ web_meta_json → post.webSinglePage), so a
    // later "reanalyze" replays the same mode instead of a full sitemap crawl.
    meta: {
      ...webMeta,
      awardTags: awardTagsEntities.tags,
      awardEntities: awardTagsEntities.entities,
      ...(singlePage ? { singlePage: true } : {}),
    },
  };
  // The aggregated site text becomes the post caption (searchable + AI input).
  if (aggregate?.contentText && ref.pages[0]) {
    ref.pages[0].contentText = aggregate.contentText;
  }

  // ref.id pins the row to the placeholder's stable id (== the live job's postId),
  // so the enriched row REPLACES the placeholder in place — never a second row.
  let postId = ref.id;
  try {
    // db.WebReference is file-internal and intentionally loose: absent fields are
    // typed `string | undefined` (we build `string | null` — same falsy result
    // through webRefToPost's `||`), and palette is typed `string[]` while we (and
    // the renderer, via paletteHexes) carry rich { hex, role, weight } swatches.
    // Cast to the function's own parameter type so no data is lost on the way in.
    const res = db.upsertWebReference(
      ref as unknown as Parameters<typeof db.upsertWebReference>[0],
      { overwriteAi: overwrite },
    );
    postId = res.id || postId;
    pushEvent(
      key,
      'write',
      `Reference salvata: ${ref.pages.length} screenshot, ${(webMetadata?.palette || []).length} colori, ${(webMetadata?.fonts || []).length} font, ${(webMetadata?.techStack || []).length} tecnologie, ${awards.length} premi`,
      { postId },
    );
  } catch (err) {
    // Persistence itself failed — but we DID capture screenshots. Re-throw so the
    // job lands in 'error' (retryable); the placeholder row is still present.
    const e = err as { message?: unknown } | null | undefined;
    throw new Error(`Salvataggio reference non riuscito: ${e?.message || err}`);
  }
  // The promoted row replaced the placeholder in the SAME id → refresh the list.
  onListRefresh?.({ count: 0, platform: 'web', refresh: true });

  // ── Phase analyzing (delegated to the shared AI queue) ────────────────────────
  // We hand the full, freshly-persisted post to analyzer.enqueuePost (it takes a
  // post object, NOT a postId). The AI runs on the analyzer's own queue and emits
  // its own analyze:progress; our 'analyzing' phase closes as "delegated".
  report('analyzing', 0, { stage: 'Analisi AI in coda…', partial });
  try {
    const post = db.getPost(postId);
    if (post) {
      analyzer.enqueuePost(post);
      pushEvent(key, 'info', 'Analisi AI (categoria + tag) messa in coda');
    }
  } catch (err) {
    // AI delegation failing is never fatal — the reference is already saved.
    const e = err as { message?: unknown } | null | undefined;
    console.warn('[weborchestrator] analyzer enqueue failed:', e?.message);
  }

  // ── done ──────────────────────────────────────────────────────────────────────
  pushEvent(key, 'info', partial ? 'Cattura completata (parziale)' : 'Cattura completata');
  emitPhase(key, 'analyzing', 1, { partial });
  patchJob(key, {
    status: 'done',
    phase: 'done',
    progress: 1,
    phaseProgress: 1,
    partial,
    error: null,
    finishedAt: Date.now(),
    title: ref.title,
    screenshotPath: withShots[0].screenshotPath,
  });
}

// ─── Worker / queue ─────────────────────────────────────────────────────────

async function runJob(key: string): Promise<void> {
  const job = jobsMap.get(key);
  const url = urlCache.get(key);
  if (!job || !url) return;

  const ac = new AbortController();
  // One job composes MANY short-lived per-phase signals off this single job
  // signal via AbortSignal.any (discover + up to N capture pages + N QC
  // re-captures + extract), and each adds an internal 'abort' listener to it for
  // the derived signal's lifetime. On a maxPages=8 site with re-captures that
  // easily exceeds Node's default 10-listener threshold, so raise the cap to a
  // safe bound to avoid a spurious MaxListenersExceededWarning. The derived
  // signals become GC-eligible once their phase timer fires/clears.
  try {
    if (ac.signal && typeof require('events').setMaxListeners === 'function') {
      require('events').setMaxListeners(0, ac.signal); // 0 = unbounded (bounded by the job)
    }
  } catch {}
  abortMap.set(key, ac);
  runningCount++;
  patchJob(key, {
    status: 'discovering',
    phase: 'discovering',
    progress: 0,
    phaseProgress: 0,
    error: null,
    startedAt: Date.now(),
  });

  try {
    await captureWebReference(url, {
      signal: ac.signal,
      maxPages: job.maxPages,
      overwrite: job.overwrite,
      singlePage: job.singlePage,
    });
  } catch (err) {
    if (isAbortErr(err)) {
      if (pausedKeys.has(key)) {
        // Paused, not cancelled: re-queue so resume restarts from scratch.
        pausedKeys.delete(key);
        patchJob(key, {
          status: 'pending',
          phase: 'queued',
          progress: 0,
          phaseProgress: 0,
          error: null,
        });
        if (!pendingQueue.includes(key)) pendingQueue.unshift(key);
      } else {
        patchJob(key, { status: 'cancelled', phase: 'cancelled', progress: 0 });
      }
    } else {
      const e = err as { message?: unknown; noScreenshot?: unknown } | null | undefined;
      const msg = (typeof e?.message === 'string' && e.message) || String(err);
      console.warn(`[weborchestrator] ${key}: ${msg}`);
      // Hard capture failure (no usable screenshot) on a placeholder THIS enqueue
      // created → delete the orphan post so clearCompleted can't leave a blank
      // 'web' card in the gallery forever. A save-failure (screenshots captured)
      // keeps the placeholder for retry. Only delete when the post is still an
      // un-promoted placeholder (no media), never a previously-enriched reference.
      if (e?.noScreenshot && job.placeholderCreated) {
        try {
          db.deletePosts([job.postId]);
          onListRefresh?.({ count: 0, platform: 'web', refresh: true });
        } catch (e2) {
          const ee = e2 as { message?: unknown } | null | undefined;
          console.warn(`[weborchestrator] orphan placeholder cleanup failed:`, ee?.message);
        }
      }
      pushEvent(key, 'error', msg);
      patchJob(key, {
        status: 'error',
        phase: 'error',
        progress: 0,
        error: msg,
        finishedAt: Date.now(),
      });
    }
  } finally {
    abortMap.delete(key);
    runningCount--;
    pumpQueue();
  }
}

function pumpQueue(): void {
  if (isPaused) return;
  while (runningCount < WEB_CONCURRENCY && pendingQueue.length > 0) {
    const key = pendingQueue.shift();
    if (key === undefined) break;
    const job = jobsMap.get(key);
    if (job?.status === 'pending') runJob(key);
  }
}

// ─── Public: enqueue ──────────────────────────────────────────────────────────

// Placeholder-first entrypoint. Validates the URL, creates the raw web post
// immediately (gallery card appears), then queues the enrichment job. Returns
// { id, finalUrl, domain, queued } synchronously-ish (the placeholder write is
// sync; everything else is background).
function enqueueWeb(
  url: string | undefined,
  {
    maxPages = DEFAULT_MAX_PAGES,
    overwrite = false,
    recovered = false,
    singlePage,
  }: EnqueueOptions = {},
): EnqueueResult {
  if (typeof url !== 'string' || !url.trim()) throw new Error('URL non valido.');
  // SSRF guard + scheme/host validation at the gate (throws on a blocked host).
  assertSafeUrl(url);

  const postId = db.webPostId(url);
  const key = jobKey(postId);

  // Dedup: an active job for this key → no-op (idempotent paste). A re-analyze of
  // a finished post (status done/error/cancelled) falls through and re-runs.
  const existing = jobsMap.get(key);
  if (
    existing &&
    ['pending', 'discovering', 'capturing', 'extracting', 'analyzing'].includes(existing.status)
  ) {
    return {
      id: postId,
      finalUrl: existing.finalUrl || url,
      domain: existing.domain || null,
      queued: false,
    };
  }

  // Create the raw placeholder row (idempotent on the deterministic id) so the
  // gallery shows a card right away, then notify the existing list-refresh path.
  // Domain is left null on the job record until discovery resolves the finalUrl;
  // the placeholder row already derives its own domain from the URL (db side).
  const domain: string | null = null;
  // createWebPlaceholder is idempotent on the deterministic id (no new row when the
  // post already exists), so check FIRST: a genuinely-new placeholder bumps the
  // 'web' new-posts badge by 1, while a re-analyze/overwrite of an existing site
  // (or a recovered job) must NOT — it would otherwise show a spurious "N new".
  let placeholderCreated = false;
  let existingPost: Awaited<ReturnType<typeof db.getPost>> | null = null;
  try {
    existingPost = db.getPost(postId);
    placeholderCreated = !existingPost;
  } catch {
    placeholderCreated = false;
  }
  // Tri-state singlePage: when the caller doesn't specify a mode (reanalyze), the
  // persisted one (web_meta_json → webSinglePage) is REPLAYED, so a single-page
  // reference is never silently upgraded to a full sitemap crawl by a stale
  // renderer copy of the post. An explicit boolean (AddSiteModal checkbox, job
  // recovery) still wins.
  const resolvedSinglePage =
    singlePage === undefined ? !!existingPost?.webSinglePage : !!singlePage;
  try {
    db.createWebPlaceholder(url);
  } catch (err) {
    const e = err as { message?: unknown } | null | undefined;
    console.warn('[weborchestrator] placeholder creation failed:', e?.message);
    placeholderCreated = false;
  }
  // Recovered jobs (boot recovery) never bump the badge — the post already existed.
  if (placeholderCreated && !recovered) onListRefresh?.({ count: 1, platform: 'web' });
  else onListRefresh?.({ count: 0, platform: 'web', refresh: true });

  urlCache.set(key, url);
  setJob({
    key,
    postId,
    url,
    finalUrl: url,
    domain,
    maxPages: resolvedSinglePage ? 1 : clampMaxPages(maxPages),
    overwrite: !!overwrite,
    // Single-page mode skips sitemap discovery and captures only the pasted URL
    // (article/guide). Persisted on the job so a recovered scan keeps the mode.
    singlePage: !!resolvedSinglePage,
    // True only when THIS enqueue inserted a fresh placeholder row (vs re-analyzing
    // an already-existing site). On a hard capture error (no usable screenshot) the
    // error path deletes the orphan placeholder so clearCompleted can't leave a
    // blank 'web' card in the gallery forever.
    placeholderCreated,
    status: 'pending',
    phase: 'queued',
    progress: 0,
    phaseProgress: 0,
    stage: null,
    partial: false,
    pagesTotal: 0,
    pagesDone: 0,
    error: null,
    queuedAt: Date.now(),
    startedAt: null,
    finishedAt: null,
    title: null,
    screenshotPath: null,
    // Behind-the-scenes detail consumed by the Websites panel.
    source: null,
    lang: null,
    palette: [],
    fonts: [],
    techStack: [],
    awards: [],
    pages: [],
    events: [],
  });
  if (!pendingQueue.includes(key)) pendingQueue.push(key);
  pumpQueue();

  return { id: postId, finalUrl: url, domain, queued: true };
}

// ─── Controls (mirror downloader/analyzer) ────────────────────────────────────

function pauseAll(): { paused: boolean } {
  isPaused = true;
  for (const [key, job] of jobsMap) {
    if (['discovering', 'capturing', 'extracting', 'analyzing'].includes(job.status)) {
      pausedKeys.add(key);
      abortMap.get(key)?.abort();
    }
  }
  return { paused: true };
}

function resumeAll(): { paused: boolean } {
  isPaused = false;
  pumpQueue();
  return { paused: false };
}

function cancelJob(key: string | undefined): { cancelled: boolean } {
  pausedKeys.delete(key as string);
  abortMap.get(key as string)?.abort();
  const qi = pendingQueue.indexOf(key as string);
  if (qi >= 0) pendingQueue.splice(qi, 1);
  const job = jobsMap.get(key as string);
  if (job && job.status !== 'done')
    patchJob(key as string, { status: 'cancelled', phase: 'cancelled', progress: 0 });
  return { cancelled: true };
}

function cancelAll(): { cancelled: boolean } {
  const keys = new Set<string>([...pendingQueue, ...abortMap.keys()]);
  for (const key of keys) cancelJob(key);
  pendingQueue.length = 0;
  pausedKeys.clear();
  isPaused = false;
  for (const [key, job] of jobsMap) {
    if (['discovering', 'capturing', 'extracting', 'analyzing'].includes(job.status)) {
      patchJob(key, { status: 'cancelled', phase: 'cancelled', progress: 0 });
    }
  }
  return { cancelled: true };
}

function retryJob(key: string | undefined): { retried: boolean } {
  const job = jobsMap.get(key as string);
  if (!job || (job.status !== 'error' && job.status !== 'cancelled')) return { retried: false };
  patchJob(key as string, {
    status: 'pending',
    phase: 'queued',
    progress: 0,
    phaseProgress: 0,
    partial: false,
    error: null,
    queuedAt: Date.now(),
    startedAt: null,
    finishedAt: null,
    source: null,
    lang: null,
    palette: [],
    fonts: [],
    techStack: [],
    awards: [],
    pages: [],
    events: [],
  });
  if (!pendingQueue.includes(key as string)) pendingQueue.push(key as string);
  pumpQueue();
  return { retried: true };
}

function clearCompleted(): { ok: boolean } {
  for (const [key, job] of jobsMap) {
    if (job.status === 'done' || job.status === 'cancelled' || job.status === 'error') {
      jobsMap.delete(key);
      urlCache.delete(key);
      jobstore.forget(KIND, key);
    }
  }
  return { ok: true };
}

// Boot recovery: re-enqueue web scans interrupted by a previous run. enqueueWeb is
// idempotent on the URL-derived id, so re-running it upserts the existing
// placeholder instead of duplicating it; the scan restarts from discovery.
function recover(): { recovered: number } {
  // resumable() is typed JobRecord[] (the core columns shared by every queue);
  // each web row's payload also carries this queue's own fields (url/maxPages/
  // overwrite/singlePage), so narrow to WebJobRecord. They're Partial because the
  // mirror strips heavy regenerable keys (events/pages) — see jobstore.HEAVY_KEYS.
  const rows = jobstore.resumable(KIND) as Array<
    ReturnType<typeof jobstore.resumable>[number] & Partial<WebJobRecord>
  >;
  let recovered = 0;
  const keep = new Set<string>();
  for (const job of rows) {
    if (!job.url) continue;
    try {
      // Forward overwrite (persisted in the mirrored payload) so a reanalyze
      // interrupted by a crash/quit still re-runs as an overwriting capture, and
      // mark the job recovered so it doesn't bump the 'web' new-posts badge.
      if (
        enqueueWeb(job.url as string, {
          maxPages: job.maxPages as number | undefined,
          overwrite: job.overwrite as boolean | undefined,
          singlePage: job.singlePage as boolean | undefined,
          recovered: true,
        }).queued
      )
        recovered++;
    } catch (e) {
      // Failed re-enqueue: keep the durable row so the next boot retries it
      // instead of silently losing the scan.
      keep.add(job.key);
      const ee = e as { message?: unknown } | null | undefined;
      console.warn(`[weborchestrator] recover ${job.url} failed:`, ee?.message);
    }
  }
  // Re-enqueue FIRST, forget AFTER: enqueueWeb → setJob has already re-mirrored
  // every live key into the jobstore, so there is no window where a resumable
  // job lacks a durable row if the app dies mid-recovery (see jobstore.js).
  for (const key of jobsMap.keys()) keep.add(key);
  jobstore.forgetExcept(KIND, keep);
  if (recovered > 0)
    console.log(`[weborchestrator] recovered ${recovered} web scan(s) into the queue`);
  return { recovered };
}

function getJobs(): WebJobRecord[] {
  return Array.from(jobsMap.values());
}
function getIsPaused(): boolean {
  return isPaused;
}

function setProgressEmitter(fn: unknown): void {
  onJobUpdate = typeof fn === 'function' ? (fn as JobUpdateEmitter) : null;
}
function setListRefreshEmitter(fn: unknown): void {
  onListRefresh = typeof fn === 'function' ? (fn as ListRefreshEmitter) : null;
}

// Standalone discovery (preview), optional for the UI.
function discover(
  url: string | undefined,
  { maxPages = DEFAULT_MAX_PAGES }: { maxPages?: number } = {},
): ReturnType<typeof webcapture.discoverPages> {
  assertSafeUrl(url as string);
  return webcapture.discoverPages(url as string, { maxPages: clampMaxPages(maxPages) });
}

export {
  enqueueWeb,
  captureWebReference,
  getJobs,
  getIsPaused,
  cancelJob,
  cancelAll,
  pauseAll,
  resumeAll,
  retryJob,
  clearCompleted,
  recover,
  discover,
  setProgressEmitter,
  setListRefreshEmitter,
  WEB_CONCURRENCY,
};
