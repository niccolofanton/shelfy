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
import { discoverPages, fetchImageToWebp } from './webcap/sitefetch';
import { captureSite, BlockedError, type CapturedPage, type SiteCapture } from './webcap/capture';
import { assembleSite, pickIcon } from './webcap/assemble';
import { formatCaptureEvent } from './webcap/codes';
import type { SiteSession } from './webcap/driver';
import * as meta from './webcap/metadata';
import { ElectronSession } from './webcap/electron-driver';
import { SystemChromeUnblock } from './webcap/system-chrome';
import { JS_DETECT_BLOCKED } from './webcap/scripts';
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
type WebPalette = meta.PaletteSwatch[];
type WebFonts = meta.FontInfo[];

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
  | 'blocked'
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
  // Set when the site answered with an anti-bot check: the user can pass it in a
  // visible window (unblockJob) and the capture resumes in that session.
  blocked?: { vendor: string; url: string; reason: string } | null;
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
  session?: SiteSession; // visible-window session from the unblock flow
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
const DEFAULT_MAX_PAGES = 6;
const MAX_PAGES_CLAMP = 8;

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
// Sessions opened by unblockJob (the user's real Chrome, or a visible Electron
// window as fallback), consumed by the next run of that job.
interface UnblockHandle {
  session: SiteSession | null;
  dispose: () => Promise<void>;
}
const unblockSessions = new Map<string, UnblockHandle>();
const unblockWaiting = new Set<string>();

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

function isAbortErr(err: unknown): boolean {
  const e = err as { name?: unknown; message?: unknown } | null | undefined;
  return e?.name === 'AbortError' || e?.message === 'AbortError';
}

function clampMaxPages(n: unknown): number {
  const v = Math.floor(Number(n));
  if (!Number.isFinite(v)) return DEFAULT_MAX_PAGES;
  return Math.max(1, Math.min(MAX_PAGES_CLAMP, v));
}

// ─── Pipeline executor ────────────────────────────────────────────────────────

// The single job run by the queue (v2 capture). Placeholder already exists.
async function captureWebReference(
  url: string,
  {
    signal,
    maxPages = DEFAULT_MAX_PAGES,
    onProgress,
    overwrite = false,
    singlePage = false,
    session,
  }: CaptureOptions = {},
): Promise<void> {
  const key = jobKey(db.webPostId(url));
  const cap = singlePage ? 1 : clampMaxPages(maxPages);
  // One epoch for the whole capture: web_captured_at AND the asset filename
  // prefix, so this version's files never collide with a prior capture's.
  const captureStamp = Math.floor(Date.now() / 1000);
  const report = (phase: WeightedPhase, frac: number, extra?: PhaseExtra): void => {
    emitPhase(key, phase, frac, extra);
    try {
      onProgress?.({ postId: db.webPostId(url), url, phase, fraction: frac, ...extra });
    } catch {}
  };

  // ── Phase 1+2: capture (primary page, page selection, inner pages) ──────────
  report('discovering', 0, { stage: singlePage ? 'Pagina singola…' : 'Apertura del sito…' });
  const livePages: JobPage[] = [];
  let site: SiteCapture;
  try {
    site = await captureSite(url, {
      maxPages: cap,
      singlePage,
      stamp: captureStamp,
      video: true,
      signal,
      session,
      // On the desktop a Playwright launch failure falls back to a hidden Electron
      // window (the service passes no fallback, so it fails instead).
      fallbackSession: () => ElectronSession.create({ visible: false }),
      hooks: {
        // The pipeline now emits stable codes + params (P4 lane rule 10); render
        // the Italian narration the Websites panel shows from the shared contract.
        onEvent: (e) =>
          pushEvent(
            key,
            e.kind === 'error' ? 'error' : e.kind,
            formatCaptureEvent(e.code, e.params),
            e.params,
          ),
        onStage: (stage, frac) => {
          if (stage === 'primary')
            report('discovering', 0.5, { stage: 'Cattura della pagina principale…' });
          else report('capturing', frac, { stage: 'Cattura delle pagine interne…' });
        },
        onPage: (p, done, total) => {
          if (!livePages.some((x) => x.url === p.url)) livePages.push(toJobPageV2(p));
          if (done === 0) {
            report('discovering', 1, {
              finalUrl: p.url,
              domain: webHost(p.url),
              title: p.title || null,
              screenshotPath: p.hero?.path || null,
            });
          }
          report('capturing', Math.min(1, (done + 1) / Math.max(1, total)), {
            pagesTotal: total,
            pagesDone: done + 1,
            pages: livePages.slice(),
          });
        },
      },
    });
  } catch (err) {
    if (err instanceof BlockedError) throw err;
    if (isAbortErr(err)) throw err;
    const e = err as { message?: unknown } | null | undefined;
    throw Object.assign(new Error(`Cattura non riuscita: ${e?.message || err}`), {
      noScreenshot: true,
    });
  }
  signal?.throwIfAborted?.();
  const pages = site.pages;
  const primary = site.primary;
  const finalUrl = primary.url;
  const domain = webHost(finalUrl);
  patchJob(key, {
    finalUrl,
    domain,
    source: site.discoverySource,
    pagesTotal: pages.length,
    pagesDone: pages.length,
    pages: pages.map(toJobPageV2),
  });

  // ── Phase 3: design metadata (shared with the capture service via assemble.ts) ─
  report('extracting', 0, { stage: 'Analisi di colori, font e tecnologie…' });
  const head = (primary.probe.head || {}) as ProbeHead;
  // og:image + favicon fetch (network): feed the assembly's cover choice and meta.
  const ogLocal = head.ogImage
    ? await fetchImageToWebp(head.ogImage, {
        pageUrl: finalUrl,
        stamp: captureStamp,
        signal,
      }).catch(() => null)
    : null;
  report('extracting', 0.4);
  const iconUrl = pickIcon(head.icons || [], finalUrl);
  const faviconLocal = iconUrl
    ? await fetchImageToWebp(iconUrl, {
        pageUrl: finalUrl,
        stamp: captureStamp,
        quality: 92,
        signal,
      }).catch(() => null)
    : null;
  signal?.throwIfAborted?.();

  // The pure, deterministic site assembly — palette, typography, tech, awards,
  // digests and the site meta — shared with the P4 capture service (parity).
  const assembly = await assembleSite(site, {
    ogImageUrl: head.ogImage,
    ogFetched: !!ogLocal,
    singlePage,
    signal,
  });
  const title = assembly.title;
  const description = assembly.description;
  const lang = assembly.lang;
  const video = primary.video;

  pushEvent(
    key,
    'branding',
    `Design: ${assembly.palette.length} colori (${assembly.scheme || '—'}), ${assembly.fonts.length} font, ${assembly.meta.tech.length} tecnologie`,
    { palette: assembly.palette, fonts: assembly.fonts, techStack: assembly.techNames },
  );
  patchJob(key, {
    title,
    lang,
    palette: assembly.palette as unknown as WebPalette,
    fonts: assembly.fonts as unknown as WebFonts,
    techStack: assembly.techNames,
    awards: assembly.awards as unknown as Shelfy.WebAward[],
  });
  pushEvent(
    key,
    'awards',
    assembly.awards.length
      ? `${assembly.awards.length} riconoscimenti: ${assembly.awards.map((a) => a.platform).join(', ')}`
      : 'Nessun riconoscimento rilevato',
    { awards: assembly.awards },
  );
  report('extracting', 1);

  // ── Phase upsert ───────────────────────────────────────────────────────────
  const cover = assembly.cover === 'og' ? ogLocal : primary.hero?.path || primary.bands[0]?.path;
  const webPages = pages.map((p, i) => {
    const ap = assembly.pages[i];
    return {
      url: p.url,
      requestedUrl: p.requestedUrl,
      pageType: p.pageType,
      title: ap.title,
      status: p.status,
      // Legacy single image (gallery slide, v1 consumers) = the untouched hero.
      screenshotPath: (i === 0 ? cover : p.hero?.path) || p.bands[0]?.path || '',
      hero: p.hero,
      chunks: p.bands.map((b) => ({
        screenshotPath: b.path,
        width: b.width,
        height: b.height,
        top: b.top,
        cssHeight: b.cssHeight,
      })),
      footer: p.footer,
      sections: p.sections.map((s) => ({
        kind: s.kind,
        heading: s.heading,
        top: s.top,
        cssHeight: s.cssHeight,
        path: s.path,
        width: s.width,
        height: s.height,
      })),
      heightCss: p.heightCss,
      capped: p.capped,
      jacked: p.jacked,
      qc: p.qc,
      contentText: ap.contentText,
      digest: ap.digest,
      meta: { ogImage: ap.ogImage },
    };
  });
  const webMeta: Shelfy.WebMeta = {
    ...assembly.meta,
    ogImagePath: ogLocal || null,
    favicon: faviconLocal || null,
    video,
  };
  const ref = {
    id: db.webPostId(url),
    url,
    finalUrl,
    domain,
    title,
    description: description || null,
    lang,
    capturedAt: captureStamp,
    pages: webPages,
    palette: assembly.palette,
    fonts: assembly.fonts,
    techStack: assembly.techNames,
    awards: assembly.awards,
    meta: webMeta,
  };
  let postId = ref.id;
  try {
    const res = db.upsertWebReference(
      ref as unknown as Parameters<typeof db.upsertWebReference>[0],
      {
        overwriteAi: overwrite,
      },
    );
    postId = res.id || postId;
    pushEvent(
      key,
      'write',
      `Reference salvata: ${pages.length} pagine, ${pages.reduce((s, p) => s + p.sections.length, 0)} sezioni${video ? ', video di scroll' : ''}`,
      { postId },
    );
  } catch (err) {
    const e = err as { message?: unknown } | null | undefined;
    throw new Error(`Salvataggio reference non riuscito: ${e?.message || err}`);
  }
  onListRefresh?.({ count: 0, platform: 'web', refresh: true });

  // ── Phase analyzing (delegated to the shared AI queue) ────────────────────────
  const partial = site.skipped.length > 0;
  report('analyzing', 0, { stage: 'Analisi AI in coda…', partial });
  try {
    const post = db.getPost(postId);
    if (post) {
      analyzer.enqueuePost(post);
      pushEvent(key, 'info', 'Analisi AI messa in coda');
    }
  } catch (err) {
    const e = err as { message?: unknown } | null | undefined;
    console.warn('[weborchestrator] analyzer enqueue failed:', e?.message);
  }
  pushEvent(key, 'info', partial ? 'Cattura completata (parziale)' : 'Cattura completata');
  emitPhase(key, 'analyzing', 1, { partial });
  patchJob(key, {
    status: 'done',
    phase: 'done',
    progress: 1,
    phaseProgress: 1,
    partial,
    error: null,
    blocked: null,
    finishedAt: Date.now(),
    title,
    screenshotPath: cover || null,
  });
}

// ─── v2 helpers ───────────────────────────────────────────────────────────────

interface ProbeHead {
  title?: string;
  lang?: string;
  description?: string;
  ogTitle?: string;
  ogDescription?: string;
  ogImage?: string;
  ogSiteName?: string;
  applicationName?: string;
  themeColor?: string;
  canonical?: string;
  rss?: string;
  icons?: { href: string; rel: string; sizes: string; type: string }[];
  hreflang?: { lang?: string; href?: string }[];
  jsonld?: string[];
}

function webHost(u: string): string {
  try {
    return new URL(u).hostname.toLowerCase().replace(/^www\./, '');
  } catch {
    return '';
  }
}

function toJobPageV2(p: CapturedPage): JobPage {
  return {
    url: p.url,
    screenshotPath: p.hero?.path || p.bands[0]?.path || '',
    chunks: p.bands.map((b) => ({
      screenshotPath: b.path,
      width: b.width,
      height: b.height,
    })) as Shelfy.WebPageChunk[],
    width: p.hero?.width || p.bands[0]?.width || 0,
    height: p.hero?.height || p.bands[0]?.height || 0,
  };
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
      session: unblockSessions.get(key)?.session || undefined,
    });
  } catch (err) {
    if (err instanceof BlockedError) {
      // Anti-bot interstitial: not an error the user can fix by retrying. Keep
      // the placeholder; the panel offers a visible window to pass the check.
      pushEvent(
        key,
        'error',
        `${err.message}: aprila in una finestra visibile per superare la verifica`,
        {
          vendor: err.vendor,
        },
      );
      patchJob(key, {
        status: 'blocked',
        phase: 'blocked',
        progress: 0,
        error: err.message,
        blocked: { vendor: err.vendor, url: err.url, reason: err.reason },
        finishedAt: Date.now(),
      });
      return;
    }
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
    const h = unblockSessions.get(key);
    if (h) {
      unblockSessions.delete(key);
      h.dispose().catch(() => {});
    }
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
  if (!job || (job.status !== 'error' && job.status !== 'cancelled' && job.status !== 'blocked'))
    return { retried: false };
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
    if (
      job.status === 'done' ||
      job.status === 'cancelled' ||
      job.status === 'error' ||
      job.status === 'blocked'
    ) {
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

// Anti-bot unblock: open the blocked page in a VISIBLE window of the app, wait
// for the user to pass the check, then re-run the capture in that very window
// (same browser + cookies + fingerprint, so the clearance stays valid). Shelfy
// never solves the challenge itself.
const UNBLOCK_TIMEOUT_MS = 5 * 60_000;
async function unblockJob(key: string | undefined): Promise<{ ok: boolean; reason?: string }> {
  const job = jobsMap.get(key as string);
  if (!key || !job || job.status !== 'blocked' || !job.blocked)
    return { ok: false, reason: 'not-blocked' };
  if (unblockWaiting.has(key)) return { ok: true }; // already waiting for the user
  unblockWaiting.add(key);
  const blockedUrl = job.blocked.url;
  const domain = job.domain || webHost(blockedUrl);
  try {
    // 1) The user's real Chrome (dedicated profile): checks pass there.
    const chrome = await SystemChromeUnblock.open(blockedUrl);
    let passed = false;
    let handle: UnblockHandle | null = null;
    patchJob(key, { stage: 'In attesa che tu superi la verifica nella finestra del browser…' });
    if (chrome) {
      pushEvent(
        key,
        'info',
        `Aperto ${domain} nel browser: completa la verifica, poi la cattura riparte da sola`,
      );
      passed = await chrome.waitUntilUnblocked({ timeoutMs: UNBLOCK_TIMEOUT_MS });
      if (passed) {
        try {
          handle = { session: await chrome.attach(), dispose: () => chrome.close() };
        } catch (err) {
          passed = false;
          pushEvent(
            key,
            'error',
            `Collegamento al browser non riuscito: ${(err as Error)?.message || err}`,
          );
        }
      }
      if (!passed) await chrome.close().catch(() => {});
    } else {
      // 2) No Chrome-family browser installed: a visible window of the app.
      const win = await ElectronSession.create({
        visible: true,
        url: blockedUrl,
        title: `Shelfy — ${domain}: supera la verifica, la cattura riprenderà da sola`,
      });
      pushEvent(
        key,
        'info',
        'Finestra di verifica aperta: completa il controllo, poi la cattura riparte',
      );
      passed = await win.waitUntilUnblocked(JS_DETECT_BLOCKED, { timeoutMs: UNBLOCK_TIMEOUT_MS });
      if (passed) handle = { session: win, dispose: () => win.close() };
      else await win.close().catch(() => {});
    }
    if (!passed || !handle) {
      patchJob(key, { stage: null });
      pushEvent(
        key,
        'info',
        'Verifica non completata: la finestra è stata chiusa o il tempo è scaduto',
      );
      return { ok: false, reason: 'not-passed' };
    }
    unblockSessions.set(key, handle);
    pushEvent(key, 'info', 'Verifica superata: riprendo la cattura nella stessa sessione');
    patchJob(key, {
      status: 'pending',
      phase: 'queued',
      progress: 0,
      phaseProgress: 0,
      error: null,
      stage: null,
      queuedAt: Date.now(),
      startedAt: null,
      finishedAt: null,
    });
    if (!pendingQueue.includes(key)) pendingQueue.unshift(key);
    pumpQueue();
    return { ok: true };
  } finally {
    unblockWaiting.delete(key);
  }
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
): ReturnType<typeof discoverPages> {
  assertSafeUrl(url as string);
  return discoverPages(url as string, { maxPages: clampMaxPages(maxPages) });
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
  unblockJob,
  clearCompleted,
  recover,
  discover,
  setProgressEmitter,
  setListRefreshEmitter,
  WEB_CONCURRENCY,
};
