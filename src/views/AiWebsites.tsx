import React, { useState, useEffect, useMemo, useRef, useCallback } from 'react';
import {
  Globe,
  Plus,
  Trash2,
  X,
  RotateCw,
  Eye,
  Image as ImageIcon,
  Palette,
  Type,
  Cpu,
  Award,
  Save,
  Info,
  CheckCircle2,
  AlertTriangle,
  Clock,
  ExternalLink,
  Hourglass,
  Loader2,
  Layers,
  FileText,
  Sparkles,
  Tags,
  Search,
  Archive,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import { assetThumbUrl, assetUrl } from '../lib/asset';
import { useAnalysis } from '../hooks/useAnalysis';
import { useRangeSelect } from '../hooks/useRangeSelect';
import ImageLightbox from '../components/ImageLightbox';
import { useT, useLang, localeTag } from '../i18n';

// ════════════════════════════════════════════════════════════════════════════
//  Websites — the behind-the-scenes panel for the web-reference capture pipeline.
//
//  Left: the live queue (one row per job). Right: the selected job's detail —
//  the site's own branding (favicon + palette), a phase stepper, a real-time
//  timeline of EVERY step (what we read, the artefacts we create, what we write),
//  and the harvested artefacts (screenshots, palette, fonts, tech stack, awards).
//  All driven by the rich job record emitted on web:progress (see weborchestrator).
// ════════════════════════════════════════════════════════════════════════════

// ── Local shapes ─────────────────────────────────────────────────────────────
// The web capture queue streams loosely-typed records over IPC (onWebProgress is
// `unknown`); these interfaces describe the shape this view actually consumes —
// a job-like "entry" that unifies a live job with a post-derived history row.

type WebStatus =
  | 'pending'
  | 'queued'
  | 'discovering'
  | 'capturing'
  | 'extracting'
  | 'analyzing'
  | 'done'
  | 'error'
  | 'cancelled';

interface PaletteEntry {
  hex: string;
  role: string;
}

// The capture's font/award records carry extra display fields beyond the loose
// persisted domain shapes (Shelfy.WebFont/WebAward): the enrichment writes a
// `provider` per font and an `evidence` string per award (see web-enrich.ts),
// surfaced read-only in the Detail panel.
interface WebFontView extends Shelfy.WebFont {
  provider?: string;
}
interface WebAwardView extends Shelfy.WebAward {
  evidence?: string;
}

// One screenshotted page as rendered in the Detail/lightbox.
interface PageView {
  url: string;
  screenshotPath?: string | null;
  chunks?: Shelfy.WebPageChunk[];
  width?: number;
  height?: number;
}

interface TimelineEventData {
  pages?: string[];
  [key: string]: unknown;
}

// A single behind-the-scenes timeline event emitted during capture.
interface TimelineEvent {
  id: string | number;
  kind: string;
  text: string;
  ts: number;
  data?: TimelineEventData;
}

// A live capture job as merged by useWebJobs (loose, IPC-sourced). Only the
// fields this view reads are described.
interface WebJob {
  key: string;
  postId?: string | null;
  status: WebStatus;
  url?: string;
  domain?: string;
  finalUrl?: string;
  title?: string;
  stage?: string;
  progress?: number;
  error?: string;
  partial?: boolean;
  palette?: PaletteEntry[];
  fonts?: WebFontView[];
  techStack?: string[];
  awards?: WebAwardView[];
  pages?: PageView[];
  events?: TimelineEvent[];
  queuedAt?: number | null;
  startedAt?: number | null;
  finishedAt?: number | null;
}

// The unified job-like entry the Detail/QueueRow render: a live job overlaid on
// (or derived from) a persisted web post.
interface WebEntry {
  key: string;
  postId?: string | null;
  isHistory?: boolean;
  status: WebStatus;
  url?: string;
  finalUrl?: string;
  domain?: string;
  title?: string;
  palette?: PaletteEntry[];
  fonts?: WebFontView[];
  techStack?: string[];
  awards?: WebAwardView[];
  pages?: PageView[];
  events?: TimelineEvent[];
  queuedAt?: number | null;
  startedAt?: number | null;
  finishedAt?: number | null;
  screenshotPath?: string | null;
  partial?: boolean;
  stage?: string;
  progress?: number;
  error?: string;
  post?: Shelfy.Post | null;
}

// The analyzer job surfaced by useAnalysis().jobFor(). Display-only shape.
interface AnalyzerJob {
  key: string;
  status: 'pending' | 'extracting' | 'analyzing' | 'done' | 'error';
  streamText?: string;
  description?: string;
  tags?: string[];
  model?: string;
  error?: string;
}

// Persisted analysis fallback (no live job) extracted from the post.
interface PersistedAnalysis {
  description: string;
  tags: string[];
  model: string | null;
}

// A version chip: the current capture (id=null) or an archived snapshot.
interface VersionEntry {
  id: number | null;
  capturedAt: number | null;
  isCurrent: boolean;
}

const DEFAULT_ACCENT = '#7B5CFF';

// ── Status / phase vocabulary ───────────────────────────────────────────────
// Status enum → i18n key (resolved at render time via the `aiWebsites` namespace
// so the queue-row stage fallback follows the active language).
const STATUS_KEY: Partial<Record<WebStatus, string>> = {
  pending: 'statusPending',
  queued: 'statusQueued',
  discovering: 'statusDiscovering',
  capturing: 'statusCapturing',
  extracting: 'statusExtracting',
  analyzing: 'statusAnalyzing',
  done: 'statusDone',
  error: 'statusError',
  cancelled: 'statusCancelled',
};

// Capture stepper phases as i18n keys (order matters; see phaseIndex). Resolved
// to localized labels inside the components that render the stepper.
const PHASE_KEYS = [
  'phaseQueue',
  'phaseDiscovery',
  'phaseCapture',
  'phaseExtraction',
  'phaseAnalysis',
  'phaseDone',
];
function phaseIndex(status: WebStatus): number {
  switch (status) {
    case 'pending':
    case 'queued':
      return 0;
    case 'discovering':
      return 1;
    case 'capturing':
      return 2;
    case 'extracting':
      return 3;
    case 'analyzing':
      return 4;
    case 'done':
      return 5;
    default:
      return -1; // error / cancelled
  }
}

const ACTIVE = new Set<WebStatus>([
  'pending',
  'queued',
  'discovering',
  'capturing',
  'extracting',
  'analyzing',
]);

// ── Branding helpers ────────────────────────────────────────────────────────
function faviconUrl(domain: string | undefined | null): string | null {
  if (!domain) return null;
  return `https://www.google.com/s2/favicons?sz=64&domain=${encodeURIComponent(domain)}`;
}

// Palette entries persisted on a post may be hex strings OR { hex, role } — the
// Detail panel expects { hex, role }, so normalise defensively.
function normPalette(pal: unknown): PaletteEntry[] {
  if (!Array.isArray(pal)) return [];
  return pal
    .map((c: unknown): PaletteEntry | null => {
      if (typeof c === 'string') return { hex: c, role: '' };
      if (c && typeof c === 'object' && typeof (c as { hex?: unknown }).hex === 'string') {
        const obj = c as { hex: string; role?: unknown };
        return { hex: obj.hex, role: typeof obj.role === 'string' ? obj.role : '' };
      }
      return null;
    })
    .filter((c): c is PaletteEntry => c !== null);
}

// A persisted web reference (a platform==='web' post) rendered as a job-like
// entry, so the same Detail panel shows historical analyses — they NEVER vanish
// on completion; the live job (when one is running) overlays this with its
// real-time event timeline.
function postToEntry(post: Shelfy.Post): WebEntry {
  const finalUrl = post.webFinalUrl || post.postUrl || post.webUrl || '';
  const domain = post.webDomain || '';
  const pages: PageView[] = (Array.isArray(post.webPages) ? post.webPages : [])
    .filter((p) => p && p.screenshotPath)
    .map((p: Shelfy.WebPage & { width?: number; height?: number }) => ({
      url: p.url || finalUrl,
      screenshotPath: p.screenshotPath,
      // Tall captures persist as vertical bands; without carrying chunks here the
      // lightbox falls back to the single top band and the page looks "cut off".
      chunks: Array.isArray(p.chunks) ? p.chunks : undefined,
      width: p.width,
      height: p.height,
    }));
  const at = (post.webCapturedAt || 0) * 1000;
  return {
    key: `web:${post.id}`,
    postId: post.id,
    isHistory: true,
    status: 'done',
    url: post.webUrl || finalUrl,
    finalUrl,
    domain,
    title: post.authorName || domain || finalUrl,
    palette: normPalette(post.webPalette),
    fonts: Array.isArray(post.webFonts) ? post.webFonts : [],
    techStack: Array.isArray(post.webTech) ? post.webTech : [],
    awards: Array.isArray(post.webAwards) ? post.webAwards : [],
    pages,
    events: [],
    queuedAt: at,
    startedAt: null,
    finishedAt: at,
    screenshotPath: pages[0]?.screenshotPath || post.imagePath || null,
    partial: false,
    post, // kept so the AI section can show the persisted category/tags
  };
}

// A web_snapshots row (an ARCHIVED, older version) rendered as a job-like entry
// for the Detail panel: a static view, no live timeline. Mirrors postToEntry.
function snapshotToView(snap: Shelfy.WebSnapshot, base: WebEntry): WebEntry {
  const pages: PageView[] = (Array.isArray(snap.webPages) ? snap.webPages : [])
    .filter((p) => p && p.screenshotPath)
    .map((p: Shelfy.WebPage & { width?: number; height?: number }) => ({
      url: p.url || base.finalUrl || '',
      screenshotPath: p.screenshotPath,
      chunks: Array.isArray(p.chunks) ? p.chunks : undefined,
      width: p.width,
      height: p.height,
    }));
  const at = (snap.capturedAt || 0) * 1000;
  return {
    key: base.key,
    postId: base.postId,
    isHistory: true,
    status: 'done',
    url: base.url,
    finalUrl: base.finalUrl,
    domain: base.domain,
    title: snap.title || base.title,
    palette: normPalette(snap.webPalette),
    fonts: Array.isArray(snap.webFonts) ? snap.webFonts : [],
    techStack: Array.isArray(snap.webTech) ? snap.webTech : [],
    awards: Array.isArray(snap.webAwards) ? snap.webAwards : [],
    pages,
    events: [],
    queuedAt: at,
    startedAt: null,
    finishedAt: at,
    screenshotPath: pages[0]?.screenshotPath || null,
    partial: false,
  };
}

// A web_snapshots row as a post-like object, so the AI block + HistorySummary
// show the analysis frozen at that capture (not the current posts-row one).
function snapshotToPost(snap: Shelfy.WebSnapshot, base: WebEntry): Shelfy.Post {
  return {
    ...((base.post || {}) as Shelfy.Post),
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
    webCapturedAt: snap.capturedAt || null,
  };
}

// Compact date label for a snapshot version chip (epoch ms). `lang` selects the
// BCP-47 locale so the date follows the active UI language.
function snapDate(ts: number | null | undefined, lang: string): string {
  if (!ts) return '';
  try {
    return new Date(ts).toLocaleString(localeTag(lang), {
      day: '2-digit',
      month: 'short',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return '';
  }
}

function entryMatchesQuery(entry: WebEntry, q: string): boolean {
  if (!q) return true;
  const hay = `${entry.domain || ''} ${entry.title || ''} ${entry.url || ''}`.toLowerCase();
  return hay.includes(q.toLowerCase());
}

// The accent is ALWAYS the application accent — never a per-site colour. The
// site's own palette is still shown as swatches (that's data), but stepper /
// progress / favicon ring / buttons stay on the app accent for consistency.
const ACCENT = DEFAULT_ACCENT;

// ── Time formatting ─────────────────────────────────────────────────────────
// `lang` selects the BCP-47 locale for the timeline timestamps.
function clock(ts: number | null | undefined, lang: string): string {
  if (!ts) return '';
  try {
    return new Date(ts).toLocaleTimeString(localeTag(lang), {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    });
  } catch {
    return '';
  }
}
function elapsed(job: WebEntry, now: number): string | null {
  const start = job.startedAt;
  const end = job.finishedAt || now;
  if (!start || !end || end < start) return null;
  const s = Math.round((end - start) / 1000);
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m ${s % 60}s`;
}

// ── Event kind → icon + colour ──────────────────────────────────────────────
interface EventIconProps {
  kind: string;
  size?: number;
}
function EventIcon({ kind, size = 14 }: EventIconProps): React.ReactElement {
  const common = { size, className: 'shrink-0' };
  switch (kind) {
    case 'read':
      return <Eye {...common} style={{ color: '#60a5fa' }} />;
    case 'artifact':
      return <ImageIcon {...common} style={{ color: '#a78bfa' }} />;
    case 'branding':
      return <Palette {...common} style={{ color: '#f0b429' }} />;
    case 'awards':
      return <Award {...common} style={{ color: '#f0b429' }} />;
    case 'write':
      return <Save {...common} style={{ color: 'var(--success)' }} />;
    case 'error':
      return <AlertTriangle {...common} style={{ color: 'var(--error)' }} />;
    default:
      return <Info {...common} style={{ color: 'var(--text-muted)' }} />;
  }
}

// ════════════════════════════════════════════════════════════════════════════
//  Segmented stepper, themed with the app accent. No secondary progress bar:
//  the CURRENT segment carries an indeterminate sweep (idle/progress) and its
//  label animates in on every phase change (entrata); deactivating segments
//  transition their colour (uscita). Shared by the capture + AI sub-steppers.
// ════════════════════════════════════════════════════════════════════════════
interface SegmentedStepperProps {
  phases: string[];
  active: number;
  accent: string;
  running?: boolean;
  labelClass?: string;
}
function SegmentedStepper({
  phases,
  active,
  accent,
  running = true,
  labelClass = 'text-[10px]',
}: SegmentedStepperProps): React.ReactElement {
  const failed = active < 0;
  return (
    <div
      className="flex items-stretch gap-1.5"
      data-testid="aiweb-stepper"
      style={{ '--accent': accent } as React.CSSProperties}
    >
      {phases.map((label, i) => {
        const current = i === active && running && !failed;
        const done = !failed && i <= active && !current;
        return (
          <div key={label} className="flex-1 min-w-0 flex flex-col items-center gap-1.5">
            <div
              className="w-full h-1 rounded-full overflow-hidden u-transition"
              style={{
                background: done ? accent : current ? `${accent}33` : 'var(--border)',
                opacity: done ? 0.85 : 1,
              }}
            >
              {current && <div className="ai-progress-track w-full h-full" />}
            </div>
            {current ? (
              <span
                key={`cur-${i}`}
                className={`aiweb-step-current font-semibold truncate max-w-full ${labelClass}`}
                style={{ color: accent }}
              >
                {label}
              </span>
            ) : (
              <span
                className={`font-medium truncate max-w-full u-transition ${labelClass}`}
                style={{ color: done ? 'var(--text-secondary)' : 'var(--text-muted)' }}
              >
                {label}
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}

interface PhaseStepperProps {
  status: WebStatus;
  accent: string;
}
function PhaseStepper({ status, accent }: PhaseStepperProps): React.ReactElement {
  const t = useT('aiWebsites');
  const phases = PHASE_KEYS.map((k) => t(k));
  return (
    <SegmentedStepper
      phases={phases}
      active={phaseIndex(status)}
      accent={accent}
      running={ACTIVE.has(status)}
    />
  );
}

// ════════════════════════════════════════════════════════════════════════════
//  AI analysis (delegated to the shared analyzer queue) — surfaced INLINE here
//  so the whole pipeline (capture → category + tags) is visible on this screen
//  without jumping to the AI Tags queue. Reads the analyzer job by postId.
// ════════════════════════════════════════════════════════════════════════════

// Lenient readers over the model's PARTIAL streamed JSON, so the description and
// tags appear as they form (mirrors AiTagsQueue; display-only).
function streamedString(text: string | undefined, key: string): string {
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
function streamedArray(text: string | undefined, key: string): string[] {
  if (!text) return [];
  const m = text.match(new RegExp(`"${key}"\\s*:\\s*\\[`));
  if (!m || m.index === undefined) return [];
  const items: string[] = [];
  let i = m.index + m[0].length;
  while (i < text.length) {
    const c = text[i];
    if (c === ']') break;
    if (c === '"') {
      let s = '';
      let closed = false;
      i++;
      for (; i < text.length; i++) {
        const ch = text[i];
        if (ch === '\\') {
          s += text[i + 1] ?? '';
          i++;
          continue;
        }
        if (ch === '"') {
          closed = true;
          i++;
          break;
        }
        s += ch;
      }
      if (closed) items.push(s);
      else break;
    } else {
      i++;
    }
  }
  return items;
}

// AI stepper phases as i18n keys (order matters; see aiPhaseIndex). Resolved to
// localized labels inside AiAnalysis.
const AI_PHASE_KEYS = [
  'aiPhaseQueue',
  'aiPhaseExtraction',
  'aiPhaseAnalysis',
  'aiPhaseDescription',
  'aiPhaseTags',
  'aiPhaseDone',
];
function aiPhaseIndex(job: AnalyzerJob | null | undefined): number {
  switch (job?.status) {
    case 'pending':
      return 0;
    case 'extracting':
      return 1;
    case 'analyzing': {
      const t = job.streamText || '';
      if (/"tags"\s*:\s*\[/.test(t)) return 4;
      if (/"description"\s*:\s*"./.test(t)) return 3;
      return 2;
    }
    case 'done':
      return 5;
    default:
      return -1;
  }
}

// AI analysis is ALWAYS rendered (even before capture finishes) so it never
// pops in and shifts the layout: it shows an inactive skeleton (grey stepper +
// shimmer placeholders) that "lights up" in place once the analyzer job starts.
interface AiAnalysisProps {
  aiJob: AnalyzerJob | null;
  post: Shelfy.Post | null | undefined;
  modelReady: boolean;
  captureDone: boolean;
  accent: string;
  onCancel?: (key: string) => void;
  onRetry?: (key: string) => void;
}

interface AiHint {
  icon: LucideIcon;
  color: string;
  text: string;
}

function AiAnalysis({
  aiJob,
  post,
  modelReady,
  captureDone,
  accent,
  onCancel,
  onRetry,
}: AiAnalysisProps): React.ReactElement {
  const t = useT('aiWebsites');
  const tc = useT('common');
  const aiPhases = AI_PHASE_KEYS.map((k) => t(k));
  const running = !!aiJob && (aiJob.status === 'extracting' || aiJob.status === 'analyzing');
  const queued = !!aiJob && aiJob.status === 'pending';
  const isDone = !!aiJob && aiJob.status === 'done';
  const errored = !!aiJob && aiJob.status === 'error';

  // No live analyzer job, but the post already carries a persisted analysis →
  // show it as a completed block (history). This is what makes an archived
  // reference's category/tags consultable forever.
  const persisted: PersistedAnalysis | null =
    !aiJob && post && (post.aiDescription || (Array.isArray(post.aiTags) && post.aiTags.length))
      ? { description: post.aiDescription || '', tags: post.aiTags || [], model: post.aiModel }
      : null;

  const inactive = (!aiJob && !persisted) || queued; // skeletons only with nothing to show yet

  const desc = aiJob
    ? running
      ? streamedString(aiJob.streamText, 'description')
      : aiJob.description || ''
    : persisted
      ? persisted.description
      : '';
  const tags: string[] = aiJob
    ? running
      ? streamedArray(aiJob.streamText, 'tags')
      : Array.isArray(aiJob.tags)
        ? aiJob.tags
        : []
    : persisted
      ? persisted.tags
      : [];
  // -1 = no job & no persisted → all-inactive; persisted → show as "done".
  const idx = aiJob ? aiPhaseIndex(aiJob) : persisted ? aiPhases.length - 1 : -1;

  // Richer AI fields the gallery modal also surfaces — entities and search
  // keywords. They're persisted on the post (they don't stream), so they appear
  // once the analysis is saved; restoring them keeps the Websites detail as
  // informative as the modal.
  const entities = Array.isArray(post?.aiEntities) ? post.aiEntities.filter(Boolean) : [];
  const keywords = Array.isArray(post?.aiKeywords) ? post.aiKeywords.filter(Boolean) : [];

  // One short status line, kept in a fixed slot so its presence never moves the
  // description/tags below it. Persisted history shows no hint (it's complete).
  let hint: AiHint | null = null;
  if (errored && aiJob?.error)
    hint = { icon: AlertTriangle, color: 'var(--error)', text: aiJob.error };
  else if (persisted) hint = null;
  else if (!aiJob && !modelReady)
    hint = {
      icon: AlertTriangle,
      color: '#f0b429',
      text: t('aiModelNotReady'),
    };
  else if (queued || (!aiJob && captureDone))
    hint = { icon: Hourglass, color: 'var(--text-muted)', text: t('aiQueuedForAnalysis') };
  else if (!aiJob)
    hint = { icon: Hourglass, color: 'var(--text-muted)', text: t('aiStartsAfterCapture') };

  return (
    <div className="shrink-0 px-5 py-3 border-b" style={{ borderColor: 'var(--border)' }}>
      <div className="flex items-center gap-2 mb-2.5">
        <Sparkles size={14} style={{ color: aiJob || persisted ? accent : 'var(--text-muted)' }} />
        <span
          className="text-[10px] font-semibold uppercase tracking-widest"
          style={{ color: 'var(--text-muted)' }}
        >
          {t('aiTitle')}
        </span>
        {(aiJob?.model && (running || isDone) ? aiJob.model : persisted?.model) && (
          <span
            className="flex items-center gap-1 text-[10px]"
            style={{ color: 'var(--text-muted)' }}
          >
            <Cpu size={10} /> {aiJob?.model || persisted?.model}
          </span>
        )}
        <div className="flex-1" />
        {running && (
          <button
            onClick={() => onCancel?.(aiJob.key)}
            className="flex items-center gap-1 px-2 py-0.5 rounded text-[11px] u-press"
            style={{ color: 'var(--error)', background: 'var(--bg-hover)' }}
          >
            <X size={12} /> {tc('cancel')}
          </button>
        )}
        {errored && (
          <button
            onClick={() => onRetry?.(aiJob.key)}
            className="flex items-center gap-1 px-2 py-0.5 rounded text-[11px] u-press"
            style={{ color: accent, background: 'var(--bg-hover)' }}
          >
            <RotateCw size={12} /> {tc('retry')}
          </button>
        )}
      </div>

      <div className="flex flex-col gap-3">
        {/* Stepper — always present; inactive (idx<0) renders all-grey. */}
        <SegmentedStepper
          phases={aiPhases}
          active={idx}
          accent={accent}
          running={!!running}
          labelClass="text-[9px]"
        />

        {hint && (
          <p className="flex items-center gap-1.5 text-xs" style={{ color: hint.color }}>
            <hint.icon size={13} /> {hint.text}
          </p>
        )}

        {/* Description — skeleton until the model streams it; clamped so the panel
            height stays stable as text fills in. */}
        <div>
          <div
            className="flex items-center gap-1.5 mb-1 text-[10px] uppercase tracking-widest"
            style={{ color: 'var(--text-muted)' }}
          >
            <FileText size={11} /> {t('aiDescription')}
          </div>
          {desc ? (
            <p
              className="text-xs leading-relaxed"
              style={{
                color: 'var(--text-secondary)',
                display: '-webkit-box',
                WebkitLineClamp: 3,
                WebkitBoxOrient: 'vertical',
                overflow: 'hidden',
              }}
            >
              {desc}
              {running && <span className="opacity-60">▋</span>}
            </p>
          ) : (
            <div className="flex flex-col gap-1.5">
              <span
                className={`${inactive ? 'ai-skel-static' : 'ai-skel'} block`}
                style={{ height: 10, width: '92%' }}
              />
              <span
                className={`${inactive ? 'ai-skel-static' : 'ai-skel'} block`}
                style={{ height: 10, width: '70%' }}
              />
            </div>
          )}
        </div>

        {/* Tags — skeleton chips until they stream in. */}
        <div>
          <div
            className="flex items-center gap-1.5 mb-1.5 text-[10px] uppercase tracking-widest"
            style={{ color: 'var(--text-muted)' }}
          >
            <Tags size={11} /> {t('aiTags')}{' '}
            {tags.length > 0 && <span className="tabular-nums">{tags.length}</span>}
          </div>
          {tags.length > 0 ? (
            <div className="flex flex-wrap gap-1.5">
              {tags.map((tag, i) => (
                <span
                  key={`${tag}-${i}`}
                  className="ai-tag-pop px-2 py-0.5 rounded-full text-[11px]"
                  style={{
                    background: 'var(--bg-hover)',
                    color: '#b9a6ff',
                    animationDelay: `${Math.min(i, 12) * 40}ms`,
                  }}
                >
                  {tag}
                </span>
              ))}
            </div>
          ) : (
            <div className="flex gap-1.5">
              {[58, 80, 46].map((w, i) => (
                <span
                  key={i}
                  className={`${inactive ? 'ai-skel-static' : 'ai-skel'} block`}
                  style={{ height: 18, width: w, borderRadius: 9999 }}
                />
              ))}
            </div>
          )}
        </div>

        {/* Entities — non-clickable chips (mirrors the gallery modal). */}
        {entities.length > 0 && (
          <div>
            <div
              className="flex items-center gap-1.5 mb-1.5 text-[10px] uppercase tracking-widest"
              style={{ color: 'var(--text-muted)' }}
            >
              <Layers size={11} /> {t('aiEntities')}{' '}
              <span className="tabular-nums">{entities.length}</span>
            </div>
            <div className="flex flex-wrap gap-1.5">
              {entities.map((e, i) => (
                <span
                  key={`${e}-${i}`}
                  className="px-2 py-0.5 rounded-full text-[11px]"
                  style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
                >
                  {e}
                </span>
              ))}
            </div>
          </div>
        )}

        {/* Keywords — discreet text (mirrors the gallery modal "Cerca anche"). */}
        {keywords.length > 0 && (
          <p className="text-[11px] leading-relaxed" style={{ color: 'var(--text-muted)' }}>
            <span style={{ color: 'var(--text-secondary)' }}>{t('aiSearchAlso')}</span>
            {keywords.join(', ')}
          </p>
        )}
      </div>
    </div>
  );
}

// ════════════════════════════════════════════════════════════════════════════
//  Queue row (left list)
// ════════════════════════════════════════════════════════════════════════════
interface QueueRowProps {
  job: WebEntry;
  selected: boolean;
  onSelect: (key: string) => void;
  onCancel: (key: string) => void;
  onRetry: (key: string) => void;
  selectMode?: boolean;
  checked?: boolean;
  onToggle?: (job: WebEntry, e: React.MouseEvent) => void;
  snapshotCount?: number;
}
function QueueRow({
  job,
  selected,
  onSelect,
  onCancel,
  onRetry,
  selectMode = false,
  checked = false,
  onToggle,
  snapshotCount = 1,
}: QueueRowProps): React.ReactElement {
  const t = useT('aiWebsites');
  const tc = useT('common');
  const accent = ACCENT;
  const fav = faviconUrl(job.domain);
  const [favErr, setFavErr] = useState<boolean>(false);
  const isActive = ACTIVE.has(job.status);
  const errored = job.status === 'error';
  const cancelled = job.status === 'cancelled';
  const isDone = job.status === 'done';
  const label =
    job.domain ||
    (() => {
      try {
        return new URL(job.url || '').hostname;
      } catch {
        return job.url;
      }
    })();

  return (
    <button
      data-testid="aiweb-queue-row"
      onClick={(e) => (selectMode ? onToggle?.(job, e) : onSelect(job.key))}
      className="relative w-full text-left flex items-center gap-2.5 px-3 py-2.5 border-b u-transition"
      style={{
        borderColor: 'var(--border)',
        background: checked ? `${accent}1f` : selected ? 'var(--bg-hover)' : 'transparent',
      }}
    >
      {(selected || checked) && (
        <span
          aria-hidden
          className="absolute left-0 inset-y-0 my-auto h-6 w-[3px] rounded-r-full"
          style={{ background: accent }}
        />
      )}
      {selectMode && (
        <span
          aria-hidden
          className="shrink-0 flex items-center justify-center w-4 h-4 rounded u-transition"
          style={{
            background: checked ? accent : 'transparent',
            border: '1.5px solid',
            borderColor: checked ? accent : 'var(--border)',
          }}
        >
          {checked && <CheckCircle2 size={12} style={{ color: '#fff' }} />}
        </span>
      )}
      <span
        className="shrink-0 flex items-center justify-center w-7 h-7 rounded overflow-hidden"
        style={{ background: 'var(--bg-hover)' }}
      >
        {fav && !favErr ? (
          <img src={fav} alt="" width={18} height={18} onError={() => setFavErr(true)} />
        ) : (
          <Globe size={15} style={{ color: 'var(--text-muted)' }} />
        )}
      </span>
      <div className="flex-1 min-w-0">
        <div className="text-sm truncate" style={{ color: 'var(--text-primary)' }}>
          {label}
        </div>
        <div
          className="flex items-center gap-1.5 text-[11px]"
          style={{ color: 'var(--text-muted)' }}
        >
          {isActive && <Loader2 size={10} className="u-spin" style={{ color: accent }} />}
          <span>
            {(() => {
              const statusKey = STATUS_KEY[job.status];
              return job.stage || (statusKey ? t(statusKey) : job.status);
            })()}
          </span>
        </div>
        {isActive && (
          <div
            className="mt-1 h-0.5 rounded-full overflow-hidden"
            style={{ background: 'var(--bg-hover)' }}
          >
            <div
              className="h-full rounded-full u-progress"
              style={{ width: `${Math.round((job.progress || 0) * 100)}%`, background: accent }}
            />
          </div>
        )}
      </div>
      {snapshotCount > 1 && (
        <span
          data-testid="aiweb-snapshot-badge"
          title={t('snapshotVersions', { n: snapshotCount })}
          className="shrink-0 flex items-center gap-0.5 rounded-full px-1.5 py-0.5 text-[10px] tabular-nums font-medium"
          style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
        >
          <Layers size={10} /> {snapshotCount}
        </span>
      )}
      {isDone && !selectMode && (
        <CheckCircle2
          size={15}
          className="shrink-0"
          style={{ color: job.partial ? '#f0b429' : 'var(--success)' }}
        />
      )}
      {isActive && !selectMode && (
        <span
          role="button"
          tabIndex={0}
          data-testid="aiweb-queue-cancel"
          onClick={(e) => {
            e.stopPropagation();
            onCancel(job.key);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.stopPropagation();
              onCancel(job.key);
            }
          }}
          title={tc('cancel')}
          className="shrink-0 flex items-center justify-center w-6 h-6 rounded u-press"
          style={{ color: 'var(--text-muted)' }}
        >
          <X size={14} />
        </span>
      )}
      {(errored || cancelled) && !selectMode && (
        <span
          role="button"
          tabIndex={0}
          data-testid="aiweb-queue-retry"
          onClick={(e) => {
            e.stopPropagation();
            onRetry(job.key);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.stopPropagation();
              onRetry(job.key);
            }
          }}
          title={tc('retry')}
          className="shrink-0 flex items-center justify-center w-6 h-6 rounded u-press"
          style={{ color: 'var(--text-muted)' }}
        >
          <RotateCw size={14} />
        </span>
      )}
    </button>
  );
}

// ════════════════════════════════════════════════════════════════════════════
//  Detail: live timeline
// ════════════════════════════════════════════════════════════════════════════
interface TimelineProps {
  events: TimelineEvent[] | undefined;
  active: boolean;
}
function Timeline({ events, active }: TimelineProps): React.ReactElement {
  const t = useT('aiWebsites');
  const { lang } = useLang();
  const scrollRef = useRef<HTMLDivElement | null>(null);
  // Auto-follow the newest event while the job is running.
  useEffect(() => {
    if (active && scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [events, active]);

  if (!events || events.length === 0) {
    return (
      <p className="text-xs px-1 py-2" style={{ color: 'var(--text-muted)' }}>
        {t('timelineEmpty')}
      </p>
    );
  }
  return (
    <div
      ref={scrollRef}
      data-testid="aiweb-timeline"
      className="flex flex-col gap-0 h-full overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] pr-1"
    >
      {events.map((e) => (
        <div key={e.id} className="flex items-start gap-2.5 py-1.5 u-fade-in-up">
          <div className="flex flex-col items-center pt-0.5">
            <EventIcon kind={e.kind} />
          </div>
          <div className="flex-1 min-w-0">
            <div
              className="text-[12px] leading-snug"
              style={{ color: e.kind === 'error' ? 'var(--error)' : 'var(--text-secondary)' }}
            >
              {e.text}
            </div>
            {/* The page list discovered in phase 1 is shown as compact sub-items. */}
            {e.kind === 'info' && Array.isArray(e.data?.pages) && e.data.pages.length > 0 && (
              <ul className="mt-1 flex flex-col gap-0.5">
                {e.data.pages.map((p, i) => (
                  <li
                    key={i}
                    className="text-[10px] truncate"
                    style={{ color: 'var(--text-muted)' }}
                  >
                    {(() => {
                      try {
                        return new URL(p).pathname || '/';
                      } catch {
                        return p;
                      }
                    })()}
                  </li>
                ))}
              </ul>
            )}
          </div>
          <span
            className="text-[10px] tabular-nums shrink-0 pt-0.5"
            style={{ color: 'var(--text-muted)' }}
          >
            {clock(e.ts, lang)}
          </span>
        </div>
      ))}
    </div>
  );
}

// ── Small section wrapper ───────────────────────────────────────────────────
interface SectionProps {
  icon?: LucideIcon;
  title: string;
  count?: number;
  children: React.ReactNode;
}
function Section({ icon: Icon, title, count, children }: SectionProps): React.ReactElement {
  return (
    <section className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        {Icon && <Icon size={13} style={{ color: 'var(--text-muted)' }} />}
        <span
          className="text-[10px] font-semibold uppercase tracking-widest"
          style={{ color: 'var(--text-muted)' }}
        >
          {title}
        </span>
        {typeof count === 'number' && (
          <span className="text-[10px] tabular-nums" style={{ color: 'var(--text-muted)' }}>
            {count}
          </span>
        )}
      </div>
      {children}
    </section>
  );
}

// For archived references there's no live "behind the scenes" event log (it's
// ephemeral) and the artefact counts already live in the right column, so we
// surface ONLY the analysis facts that aren't shown elsewhere — scopo / settore /
// lingua and the "why save it" note — as a compact section inside that column.
interface AnalysisMetaProps {
  post: Shelfy.Post | null | undefined;
}
function AnalysisMeta({ post }: AnalysisMetaProps): React.ReactElement | null {
  const t = useT('aiWebsites');
  const meta = [
    post?.aiContentType && { label: t('metaPurpose'), value: post.aiContentType },
    post?.aiCategory && { label: t('metaSector'), value: post.aiCategory },
    post?.aiLanguage && { label: t('metaLanguage'), value: post.aiLanguage },
  ].filter((m): m is { label: string; value: string } => Boolean(m));

  if (!meta.length && !post?.aiSaveReason) return null;

  return (
    <Section icon={Info} title={t('analysisTitle')}>
      {meta.length > 0 && (
        <div className="flex flex-col gap-1.5">
          {meta.map((m) => (
            <div key={m.label} className="flex items-center gap-2 text-xs">
              <span
                className="uppercase tracking-wide text-[10px]"
                style={{ color: 'var(--text-muted)' }}
              >
                {m.label}
              </span>
              <span style={{ color: 'var(--text-secondary)' }}>{m.value}</span>
            </div>
          ))}
        </div>
      )}

      {post?.aiSaveReason && (
        <p className="text-xs leading-relaxed" style={{ color: 'var(--text-muted)' }}>
          <span style={{ color: 'var(--text-secondary)' }}>{t('whySave')}</span>
          {post.aiSaveReason}
        </p>
      )}
    </Section>
  );
}

// ════════════════════════════════════════════════════════════════════════════
//  Version bar — switch between the current capture and dated archived snapshots
// ════════════════════════════════════════════════════════════════════════════
interface VersionBarProps {
  versions: VersionEntry[];
  activeId: number | null;
  onSelect: (id: number | null) => void;
  onDelete: (id: number) => void;
}
function VersionBar({
  versions,
  activeId,
  onSelect,
  onDelete,
}: VersionBarProps): React.ReactElement {
  const t = useT('aiWebsites');
  const { lang } = useLang();
  return (
    <div
      data-testid="aiweb-versions"
      className="shrink-0 flex items-center gap-2 px-5 py-2.5 border-b overflow-x-auto scrollbar-thin scrollbar-thumb-[#2e2e2e]"
      style={{ borderColor: 'var(--border)' }}
    >
      <span
        className="shrink-0 flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-widest"
        style={{ color: 'var(--text-muted)' }}
      >
        <Layers size={12} /> {t('versions')}
      </span>
      <div className="flex items-center gap-1.5">
        {versions.map((v) => {
          const active = v.id === activeId;
          return (
            <div key={v.id ?? 'current'} className="group relative shrink-0">
              <button
                type="button"
                data-testid="aiweb-version-chip"
                onClick={() => onSelect(v.id)}
                className="flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[11px] u-press u-transition"
                style={{
                  background: active ? ACCENT : 'var(--bg-hover)',
                  color: active ? '#fff' : 'var(--text-secondary)',
                  border: '1px solid',
                  borderColor: active ? ACCENT : 'var(--border)',
                }}
                title={v.isCurrent ? t('versionCurrentTitle') : t('versionArchivedTitle')}
              >
                {v.isCurrent && (
                  <span
                    className="w-1.5 h-1.5 rounded-full"
                    style={{ background: active ? '#fff' : 'var(--success)' }}
                  />
                )}
                <span className="tabular-nums whitespace-nowrap">
                  {v.isCurrent ? t('versionCurrent') : snapDate(v.capturedAt, lang)}
                </span>
              </button>
              {!v.isCurrent && v.id !== null && (
                <button
                  type="button"
                  data-testid="aiweb-version-delete"
                  onClick={(e) => {
                    e.stopPropagation();
                    onDelete(v.id as number);
                  }}
                  title={t('versionDeleteTitle')}
                  className="absolute -top-1.5 -right-1.5 hidden group-hover:flex items-center justify-center w-4 h-4 rounded-full u-press"
                  style={{ background: 'var(--error)', color: '#fff' }}
                >
                  <X size={9} />
                </button>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ════════════════════════════════════════════════════════════════════════════
//  Detail panel (right)
// ════════════════════════════════════════════════════════════════════════════
interface DetailProps {
  job: WebEntry;
  post: Shelfy.Post | null | undefined;
  now: number;
  aiJob: AnalyzerJob | null;
  modelReady: boolean;
  onOpenPost?: (postId: string) => void;
  onCancel: (key: string) => void;
  onRetry: (key: string) => void;
  onAiCancel?: (key: string) => void;
  onAiRetry?: (key: string) => void;
  versions?: VersionEntry[];
  activeVersionId?: number | null;
  onSelectVersion: (id: number | null) => void;
  onDeleteSnapshot: (id: number) => void;
}
function Detail({
  job,
  post,
  now,
  aiJob,
  modelReady,
  onOpenPost,
  onCancel,
  onRetry,
  onAiCancel,
  onAiRetry,
  versions = [],
  activeVersionId = null,
  onSelectVersion,
  onDeleteSnapshot,
}: DetailProps): React.ReactElement {
  const t = useT('aiWebsites');
  const tc = useT('common');
  const accent = ACCENT;
  const fav = faviconUrl(job.domain);
  const [favErr, setFavErr] = useState<boolean>(false);
  // Open screenshot index in the full-screen lightbox (null = closed).
  const [shotIndex, setShotIndex] = useState<number | null>(null);
  useEffect(() => {
    setFavErr(false);
    // A different site OR a different version (current ↔ archived snapshot) swaps
    // the `pages` array — close any open viewer so a stale/clamped index can't
    // surface the wrong screenshot for the newly selected version.
    setShotIndex(null);
  }, [job.domain, activeVersionId]);

  const isActive = ACTIVE.has(job.status);
  const isDone = job.status === 'done';
  const errored = job.status === 'error';
  const cancelled = job.status === 'cancelled';
  // An archived reference has no live event log, and its artefact counts already
  // live in the right column — so we drop the "behind the scenes" pane and let the
  // artefacts fill the width, surfacing the unique analysis facts there too.
  const isHistoryView = job.isHistory && !job.events?.length;
  const title = job.title || job.domain || job.url;
  const link = job.finalUrl || job.url || '';
  const dur = elapsed(job, now);
  const palette = Array.isArray(job.palette) ? job.palette : [];
  const fonts = Array.isArray(job.fonts) ? job.fonts : [];
  const tech = Array.isArray(job.techStack) ? job.techStack : [];
  const awards = Array.isArray(job.awards) ? job.awards : [];
  const pages = Array.isArray(job.pages) ? job.pages : [];

  // The detected/classified data (screenshots + branding). Shared between the live
  // layout (right column) and the archive layout (left column).
  const artefacts = (
    <>
      {/* Screenshots */}
      <Section icon={ImageIcon} title={t('sectionScreenshots')} count={pages.length}>
        {pages.length === 0 ? (
          <p className="text-xs" style={{ color: 'var(--text-muted)' }}>
            {t('screenshotsEmpty')}
          </p>
        ) : (
          <div className="grid grid-cols-3 gap-2">
            {pages.map((p, i) => (
              <button
                key={`${p.url}-${i}`}
                type="button"
                data-testid="aiweb-shot"
                onClick={() => p.screenshotPath && setShotIndex(i)}
                className="text-left rounded-md overflow-hidden border u-fade-in u-press cursor-zoom-in"
                style={{ borderColor: 'var(--border)', background: 'var(--bg-hover)' }}
                title={t('screenshotZoomTitle', { url: p.url })}
              >
                {p.screenshotPath ? (
                  <img
                    // Downscaled preview tile; the zoom view below keeps the
                    // full-resolution original.
                    src={assetThumbUrl(p.screenshotPath, 480) ?? undefined}
                    alt=""
                    className="w-full h-28 object-cover object-top"
                    draggable={false}
                  />
                ) : (
                  <div className="w-full h-28 flex items-center justify-center">
                    <ImageIcon size={20} style={{ color: 'var(--text-muted)' }} />
                  </div>
                )}
                <div
                  className="px-1.5 py-1 text-[9px] tabular-nums truncate"
                  style={{ color: 'var(--text-muted)' }}
                >
                  {p.width && p.height ? `${p.width}×${p.height}` : ''}{' '}
                  {(() => {
                    try {
                      return new URL(p.url).pathname;
                    } catch {
                      return '';
                    }
                  })()}
                </div>
              </button>
            ))}
          </div>
        )}
      </Section>

      {/* Palette */}
      {palette.length > 0 && (
        <Section icon={Palette} title={t('sectionPalette')} count={palette.length}>
          <div className="flex flex-wrap gap-2">
            {palette.map((c, i) => (
              <div
                key={`${c.hex}-${i}`}
                className="flex items-center gap-1.5 rounded-md pl-1 pr-2 py-1 border"
                style={{ borderColor: 'var(--border)' }}
              >
                <span className="w-5 h-5 rounded" style={{ background: c.hex }} />
                <div className="flex flex-col leading-tight">
                  <span
                    className="text-[10px] tabular-nums"
                    style={{ color: 'var(--text-secondary)' }}
                  >
                    {c.hex}
                  </span>
                  <span className="text-[9px]" style={{ color: 'var(--text-muted)' }}>
                    {c.role}
                  </span>
                </div>
              </div>
            ))}
          </div>
        </Section>
      )}

      {/* Fonts */}
      {fonts.length > 0 && (
        <Section icon={Type} title={t('sectionFonts')} count={fonts.length}>
          <div className="flex flex-col gap-1.5">
            {fonts.map((f, i) => (
              <div key={`${f.family}-${i}`} className="flex items-center gap-2 text-xs">
                <span style={{ color: 'var(--text-primary)' }}>{f.family}</span>
                <span
                  className="px-1.5 py-0.5 rounded text-[9px] uppercase tracking-wide"
                  style={{ background: 'var(--bg-hover)', color: 'var(--text-muted)' }}
                >
                  {f.usage}
                </span>
                {f.provider && f.provider !== 'unknown' && (
                  <span className="text-[10px]" style={{ color: 'var(--text-muted)' }}>
                    {f.provider}
                  </span>
                )}
              </div>
            ))}
          </div>
        </Section>
      )}

      {/* Tech stack */}
      {tech.length > 0 && (
        <Section icon={Cpu} title={t('sectionTech')} count={tech.length}>
          <div className="flex flex-wrap gap-1.5">
            {tech.map((techName) => (
              <span
                key={techName}
                className="px-2 py-1 rounded-full text-[11px]"
                style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
              >
                {techName}
              </span>
            ))}
          </div>
        </Section>
      )}

      {/* Awards */}
      {awards.length > 0 && (
        <Section icon={Award} title={t('sectionAwards')} count={awards.length}>
          <div className="flex flex-col gap-1.5">
            {awards.map((a, i) => (
              <div
                key={`${a.platform}-${i}`}
                className="flex items-center gap-2 text-xs rounded-md px-2 py-1.5 border"
                style={{ borderColor: 'var(--border)' }}
              >
                <Award size={13} style={{ color: '#f0b429' }} />
                <span style={{ color: 'var(--text-primary)' }}>{a.platform}</span>
                {a.level && (
                  <span
                    className="px-1.5 py-0.5 rounded text-[9px]"
                    style={{ background: 'var(--bg-hover)', color: 'var(--text-muted)' }}
                  >
                    {a.level}
                  </span>
                )}
                <span className="ml-auto text-[10px]" style={{ color: 'var(--text-muted)' }}>
                  {a.evidence}
                </span>
              </div>
            ))}
          </div>
        </Section>
      )}
    </>
  );

  return (
    <div className="flex flex-col h-full overflow-hidden">
      {/* ── Branding header (themed with the site's own colours) ─────────── */}
      <div
        className="relative shrink-0 px-5 py-4 border-b"
        style={{
          borderColor: 'var(--border)',
          background: `linear-gradient(180deg, ${accent}14, transparent)`,
        }}
      >
        <span
          aria-hidden
          className="absolute left-0 top-0 bottom-0 w-[3px]"
          style={{ background: accent }}
        />
        <div className="flex items-start gap-3">
          <span
            className="shrink-0 flex items-center justify-center w-11 h-11 rounded-lg overflow-hidden"
            style={{ background: 'var(--bg-hover)', boxShadow: `0 0 0 2px ${accent}33` }}
          >
            {fav && !favErr ? (
              <img src={fav} alt="" width={28} height={28} onError={() => setFavErr(true)} />
            ) : (
              <Globe size={22} style={{ color: accent }} />
            )}
          </span>
          <div className="flex-1 min-w-0">
            <h2
              className="text-base font-semibold truncate font-display"
              style={{ color: 'var(--text-primary)' }}
            >
              {title}
            </h2>
            <a
              href={link}
              onClick={(e) => {
                e.preventDefault();
                window.electronAPI?.openExternal?.(link);
              }}
              className="inline-flex items-center gap-1 text-xs truncate u-press"
              style={{ color: 'var(--text-muted)' }}
              title={link}
            >
              {job.domain || link}
              <ExternalLink size={11} />
            </a>
            {/* Palette strip — the site's brand colours. */}
            {palette.length > 0 && (
              <div className="flex items-center gap-1 mt-2">
                {palette.slice(0, 8).map((c, i) => (
                  <span
                    key={`${c.hex}-${i}`}
                    title={`${c.hex} · ${c.role}`}
                    className="w-5 h-5 rounded"
                    style={{
                      background: c.hex,
                      boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.08)',
                    }}
                  />
                ))}
              </div>
            )}
          </div>
          <div className="shrink-0 flex items-center gap-2">
            {dur && (
              <span
                className="flex items-center gap-1 text-[11px] tabular-nums"
                style={{ color: 'var(--text-muted)' }}
              >
                <Clock size={11} /> {dur}
              </span>
            )}
            {isActive && (
              <button
                data-testid="aiweb-detail-cancel"
                onClick={() => onCancel(job.key)}
                title={tc('cancel')}
                className="flex items-center gap-1 px-2 py-1 rounded text-xs u-press"
                style={{ color: 'var(--error)', background: 'var(--bg-hover)' }}
              >
                <X size={13} /> {tc('cancel')}
              </button>
            )}
            {(errored || cancelled) && (
              <button
                data-testid="aiweb-detail-retry"
                onClick={() => onRetry(job.key)}
                title={tc('retry')}
                className="flex items-center gap-1 px-2 py-1 rounded text-xs u-press"
                style={{ color: accent, background: 'var(--bg-hover)' }}
              >
                <RotateCw size={13} /> {tc('retry')}
              </button>
            )}
            {isDone && job.postId && (
              <button
                onClick={() => onOpenPost?.(job.postId as string)}
                className="flex items-center gap-1 px-2 py-1 rounded text-xs u-press"
                style={{ color: '#fff', background: accent }}
              >
                {t('openReference')}
              </button>
            )}
          </div>
        </div>
      </div>

      {/* ── Version bar (only when there's history) ─────────────────────── */}
      {versions.length > 1 && (
        <VersionBar
          versions={versions}
          activeId={activeVersionId}
          onSelect={onSelectVersion}
          onDelete={onDeleteSnapshot}
        />
      )}

      {/* ── Stepper ─────────────────────────────────────────────────────── */}
      <div className="shrink-0 px-5 py-4 border-b" style={{ borderColor: 'var(--border)' }}>
        <PhaseStepper status={job.status} accent={accent} />
        {errored && job.error && (
          <p className="mt-3 flex items-center gap-1.5 text-xs" style={{ color: 'var(--error)' }}>
            <AlertTriangle size={13} /> {job.error}
          </p>
        )}
        {isDone && (
          <p
            className="mt-3 flex items-center gap-1.5 text-xs"
            style={{ color: job.partial ? '#f0b429' : 'var(--success)' }}
          >
            <CheckCircle2 size={13} />
            {job.partial ? t('captureDonePartial') : t('captureDone')}
            {!isHistoryView && t('captureDoneAiContinues')}
          </p>
        )}
      </div>

      {/* ── AI analysis band — LIVE only. For an archived reference the analysis
          moves into the right column below (next to the detected data). ────── */}
      {!isHistoryView && (
        <AiAnalysis
          aiJob={aiJob}
          post={post}
          modelReady={modelReady}
          captureDone={isDone}
          accent={accent}
          onCancel={onAiCancel}
          onRetry={onAiRetry}
        />
      )}

      {/* ── Body ──────────────────────────────────────────────────────────── */}
      <div className="flex-1 min-h-0 flex overflow-hidden">
        {isHistoryView ? (
          <>
            {/* LEFT: the detected & classified data (screenshots + branding). */}
            <div
              className="flex-1 min-w-0 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] border-r px-5 py-4 flex flex-col gap-5"
              style={{ borderColor: 'var(--border)' }}
            >
              {artefacts}
            </div>
            {/* RIGHT: the full AI analysis (description, tags, entities, keywords,
                scopo / settore / lingua, perché salvarlo). */}
            <div className="w-[380px] shrink-0 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] flex flex-col">
              <AiAnalysis
                aiJob={aiJob}
                post={post}
                modelReady={modelReady}
                captureDone={isDone}
                accent={accent}
                onCancel={onAiCancel}
                onRetry={onAiRetry}
              />
              <div className="px-5 py-4">
                <AnalysisMeta post={post} />
              </div>
            </div>
          </>
        ) : (
          <>
            {/* LEFT: live timeline — all the available height, scrolls internally. */}
            <div
              className="flex-1 min-w-0 flex flex-col border-r px-5 py-4"
              style={{ borderColor: 'var(--border)' }}
            >
              <div
                className="shrink-0 flex items-center gap-2 mb-2"
                style={{ color: 'var(--text-muted)' }}
              >
                <Layers size={13} />
                <span className="text-[10px] font-semibold uppercase tracking-widest">
                  {t('behindTheScenes')}
                </span>
                {job.events && job.events.length > 0 && (
                  <span className="text-[10px] tabular-nums">{job.events.length}</span>
                )}
              </div>
              <div className="flex-1 min-h-0">
                <Timeline events={job.events} active={isActive} />
              </div>
            </div>
            {/* RIGHT: detected data */}
            <div className="w-[360px] shrink-0 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] px-5 py-4 flex flex-col gap-5">
              {artefacts}
            </div>
          </>
        )}
      </div>

      {shotIndex != null && (
        <ImageLightbox
          images={pages.map((p) => ({
            src: assetUrl(p.screenshotPath ?? null) ?? '',
            // Tall captures arrive pre-sliced into vertical bands → hand the lightbox
            // the full chunk list so it stacks light, lazy-loaded images.
            chunks: Array.isArray(p.chunks)
              ? p.chunks
                  .map((c) => assetUrl(c.screenshotPath ?? null))
                  .filter((c): c is string => Boolean(c))
              : undefined,
            label: (() => {
              try {
                return `${job.domain || ''}${new URL(p.url).pathname}`;
              } catch {
                return p.url;
              }
            })(),
            href: p.url,
          }))}
          index={shotIndex}
          onClose={() => setShotIndex(null)}
          onIndexChange={setShotIndex}
        />
      )}
    </div>
  );
}

// ════════════════════════════════════════════════════════════════════════════
//  Main view
// ════════════════════════════════════════════════════════════════════════════
interface WebJobsApi {
  jobs?: WebJob[];
  cancelJob?: (key: string) => void;
  cancelAll?: () => void;
  retryJob?: (key: string) => void;
  clearCompleted?: () => void;
}
interface AiWebsitesProps {
  webJobs?: WebJobsApi;
  onAddSite?: () => void;
  onOpenPost?: (postId: string) => void;
}
export default function AiWebsites({
  webJobs,
  onAddSite,
  onOpenPost,
}: AiWebsitesProps): React.ReactElement {
  const t = useT('aiWebsites');
  const tc = useT('common');
  const { jobs = [], cancelJob, cancelAll, retryJob, clearCompleted } = webJobs || {};
  const [selectedKey, setSelectedKey] = useState<string | null>(null);

  // Shared analyzer queue — lets us surface the web post's AI analysis (category
  // + tags) INLINE here, by matching the analyzer job to the web postId, so the
  // user never has to switch to the AI Tags screen mid-task.
  const analysis = useAnalysis();
  const modelReady = !!analysis?.modelStatus?.ready;

  // ── Persistent library ──────────────────────────────────────────────────────
  // Every analysed site lives on as a platform==='web' post. We load those so the
  // panel becomes a permanent, searchable archive: completed analyses NEVER vanish
  // (a live job, when running, just overlays the matching entry with its timeline).
  const [posts, setPosts] = useState<Shelfy.Post[]>([]);
  const [searchQuery, setSearchQuery] = useState<string>('');
  const [effectiveQuery, setEffectiveQuery] = useState<string>('');
  // Archived-snapshot count per postId (current capture excluded), for the "⧉N"
  // badge. Bumping reloadSig forces a re-fetch of posts + counts after a delete.
  const [snapshotCounts, setSnapshotCounts] = useState<Record<string, number>>({});
  const [reloadSig, setReloadSig] = useState<number>(0);
  const reload = useCallback(() => setReloadSig((n) => n + 1), []);

  // Debounce the search input (mirrors the gallery's FilterBar feel).
  useEffect(() => {
    const timer = setTimeout(() => setEffectiveQuery(searchQuery.trim()), 250);
    return () => clearTimeout(timer);
  }, [searchQuery]);

  // Refetch when the query changes, a capture finishes, or an AI analysis lands.
  const doneSig = useMemo(
    () =>
      jobs
        .filter((j) => j.status === 'done')
        .map((j) => j.postId)
        .join(','),
    [jobs],
  );
  // `useAnalysis()` returns the raw context (no `done` summary), so derive our own
  // signature from the analyzer jobs: it changes the instant a job completes —
  // exactly when a web post gains its persisted description/tags. Without this the
  // panel stayed stuck on "In coda per l'analisi…" after the AI actually finished.
  const aiDoneSig = useMemo(
    () =>
      ((analysis?.jobs || []) as unknown as AnalyzerJob[])
        .filter((j) => j.status === 'done' || j.status === 'error')
        .map((j) => `${j.key}:${j.status}`)
        .join(','),
    [analysis?.jobs],
  );
  useEffect(() => {
    let alive = true;
    window.electronAPI
      ?.getPosts?.({
        platform: 'web',
        search: effectiveQuery || undefined,
        sortOrder: 'oldest',
        limit: 500,
      })
      .then((res) => {
        if (alive) setPosts(res?.posts || []);
      })
      .catch(() => {});
    window.electronAPI
      ?.getWebSnapshotCounts?.()
      .then((map) => {
        if (alive) setSnapshotCounts(map || {});
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [effectiveQuery, doneSig, aiDoneSig, reloadSig]);

  // 1s ticker — only while something is running (keeps the live elapsed honest).
  const anyActive = jobs.some((j) => ACTIVE.has(j.status));
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    if (!anyActive) return undefined;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [anyActive]);

  // Merge persisted references (history) with the live job queue. Live jobs win
  // for a given postId (they carry the real-time timeline + progress); the
  // post-derived entry supplies the archived fallback + persisted AI fields.
  const { entries, totalEntries } = useMemo(() => {
    const fromPosts = posts.map(postToEntry);
    const postKeys = new Set(fromPosts.map((e) => e.key));
    const byKey = new Map<string, WebEntry>(fromPosts.map((e): [string, WebEntry] => [e.key, e]));
    for (const j of jobs) {
      const key = j.postId ? `web:${j.postId}` : j.key;
      const base = byKey.get(key);
      byKey.set(key, { ...(base || {}), ...j, key, post: base?.post, isHistory: false });
    }
    let list = Array.from(byKey.values());
    const total = list.length;
    if (effectiveQuery) {
      // Post-backed entries already matched the DB full-text query; live-only
      // entries (no persisted post yet) are matched against domain/title/url.
      list = list.filter((e) => postKeys.has(e.key) || entryMatchesQuery(e, effectiveQuery));
    }
    return { entries: list, totalEntries: total };
  }, [posts, jobs, effectiveQuery]);

  // Active jobs pinned to the top (you're watching them now); everything else in
  // order of addition (oldest first), as requested.
  const sorted = useMemo(
    () =>
      [...entries].sort((a, b) => {
        const aA = ACTIVE.has(a.status);
        const bA = ACTIVE.has(b.status);
        if (aA !== bA) return aA ? -1 : 1;
        return (a.queuedAt || a.finishedAt || 0) - (b.queuedAt || b.finishedAt || 0);
      }),
    [entries],
  );

  // Auto-select a sensible default when the current pick disappears / is unset:
  // the running job if any, else the top of the list.
  useEffect(() => {
    if (!sorted.length) {
      if (selectedKey) setSelectedKey(null);
      return;
    }
    if (!selectedKey || !sorted.some((j) => j.key === selectedKey)) {
      const running = sorted.find((j) => ACTIVE.has(j.status));
      setSelectedKey((running || sorted[0]).key);
    }
  }, [sorted, selectedKey]);

  const selected = sorted.find((j) => j.key === selectedKey) || null;
  // The analyzer job for the selected web reference (key = `${postId}:analyze`).
  const aiJob: AnalyzerJob | null =
    selected && selected.postId && analysis?.jobFor
      ? ((analysis.jobFor(selected.postId) as AnalyzerJob | null) ?? null)
      : null;

  // ── Snapshot version history for the selected site ──────────────────────────
  const [snapshots, setSnapshots] = useState<Shelfy.WebSnapshot[]>([]);
  // Which postId `snapshots` were loaded for. The reset of activeSnapshotId and
  // the refetch happen in a post-render effect, so on the first render after a
  // site switch both `snapshots` and `activeSnapshotId` still hold the PREVIOUS
  // site's data. Tracking the owner lets the render derivation ignore them until
  // they've been reloaded for the current site (no stale frame).
  const [snapshotsPostId, setSnapshotsPostId] = useState<string | null>(null);
  const [activeSnapshotId, setActiveSnapshotId] = useState<number | null>(null);
  const selectedPostId = selected?.postId || null;

  useEffect(() => {
    setActiveSnapshotId(null); // switching site (or after a reload) → back to current
    if (!selectedPostId) {
      setSnapshots([]);
      setSnapshotsPostId(null);
      return undefined;
    }
    let alive = true;
    window.electronAPI
      ?.getWebSnapshots?.(selectedPostId)
      .then((list) => {
        if (alive) {
          setSnapshots(Array.isArray(list) ? list : []);
          setSnapshotsPostId(selectedPostId);
        }
      })
      .catch(() => {
        if (alive) {
          setSnapshots([]);
          setSnapshotsPostId(selectedPostId);
        }
      });
    return () => {
      alive = false;
    };
  }, [selectedPostId, reloadSig]);

  // Versions = current capture (id=null) first, then archived snapshots (newest
  // first). Only rendered when there's more than one.
  // Normalized to epoch MS. post.webCapturedAt is epoch SECONDS (×1000), while
  // the live-job finishedAt fallback is already MS (Date.now() in the
  // orchestrator) — both branches yield ms so the chip date is consistent.
  const currentCapturedAt = selected?.post?.webCapturedAt
    ? selected.post.webCapturedAt * 1000
    : selected?.finishedAt || null;
  // Only trust `snapshots` once they've been (re)loaded for the current site —
  // otherwise they still belong to the previously selected one.
  const snapshotsValid = snapshotsPostId === selectedPostId;
  const versions = useMemo<VersionEntry[]>(() => {
    const archived: VersionEntry[] = snapshotsValid
      ? snapshots.map((s) => ({
          id: s.id,
          capturedAt: (s.capturedAt || 0) * 1000,
          isCurrent: false,
        }))
      : [];
    return [{ id: null, capturedAt: currentCapturedAt, isCurrent: true }, ...archived];
  }, [snapshots, snapshotsValid, currentCapturedAt]);

  // What the Detail renders: the live current capture, or an archived snapshot
  // (static view + its frozen AI). The snapshot is used only when it belongs to
  // the currently selected site (guards the one stale frame after a site switch,
  // before the reset/refetch effect runs).
  const activeSnapshot =
    snapshotsValid && activeSnapshotId
      ? snapshots.find((s) => s.id === activeSnapshotId) || null
      : null;
  const detailJob =
    activeSnapshot && selected ? snapshotToView(activeSnapshot, selected) : selected;
  const detailPost =
    activeSnapshot && selected ? snapshotToPost(activeSnapshot, selected) : selected?.post;
  const detailAiJob = activeSnapshot ? null : aiJob;

  // ── Multi-select (mirrors the Gallery toolbar) ──────────────────────────────
  // Selection set + Shift+click range logic shared with Gallery; entries without
  // a postId (live-only jobs not persisted yet) are skipped by the getId mapper.
  const [selectMode, setSelectMode] = useState<boolean>(false);
  const {
    selected: selectedIds,
    toggleAt,
    clearSelection,
    resetAnchor,
  } = useRangeSelect(sorted, (e: WebEntry) => e.postId);
  const sortedRef = useRef<WebEntry[]>(sorted);
  sortedRef.current = sorted;
  const [deleteDialog, setDeleteDialog] = useState<{ count: number } | null>(null);
  const [busy, setBusy] = useState<boolean>(false);

  // `sorted` reorders continuously (active jobs pinned to the top, refetch on
  // done/AI/reload), so the stored Shift+click anchor — an INDEX into `sorted` —
  // would point into the old ordering and select the wrong rows. Mirror Gallery:
  // drop the anchor on every reorder. The selection set is keyed by stable
  // postId, so it survives; only the index anchor must die (a stale Shift+click
  // then degrades to a safe plain toggle).
  useEffect(() => {
    resetAnchor();
  }, [sorted, resetAnchor]);

  const exitSelectMode = useCallback(() => {
    setSelectMode(false);
    clearSelection();
  }, [clearSelection]);

  // Changing the search filter re-derives `sorted`: the previously selected ids
  // may now be off-screen, so clear the selection (the banner count + delete
  // would otherwise act on hidden sites). Mirrors Gallery's query reconciliation.
  useEffect(() => {
    if (selectMode) exitSelectMode();
    else clearSelection();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [effectiveQuery, exitSelectMode, clearSelection]);

  const handleRowToggle = useCallback(
    (job: WebEntry, evt: React.MouseEvent) => {
      if (!job.postId) return; // a live-only entry with no persisted row yet
      const index = sortedRef.current.findIndex((e) => e.key === job.key);
      toggleAt(job.postId, index, evt?.shiftKey);
    },
    [toggleAt],
  );

  const runDelete = useCallback(
    async (mode: 'complete' | 'report') => {
      // The selection set only ever holds persisted postIds (the getId mapper
      // skips live-only entries), so this filter just satisfies the typed
      // string-only delete API without changing what's sent.
      const ids = [...selectedIds].filter((id): id is string => typeof id === 'string');
      if (!ids.length) {
        setDeleteDialog(null);
        return;
      }
      setBusy(true);
      try {
        if (mode === 'complete') await window.electronAPI?.deleteWebSites?.(ids);
        else await window.electronAPI?.deleteWebLatestReport?.(ids);
      } catch {
        /* surfaced via reload below */
      }
      setBusy(false);
      setDeleteDialog(null);
      exitSelectMode();
      reload();
    },
    [selectedIds, exitSelectMode, reload],
  );

  const handleDeleteSnapshot = useCallback(
    async (snapshotId: number) => {
      try {
        await window.electronAPI?.deleteWebSnapshot?.(snapshotId);
      } catch {
        /* surfaced via reload below */
      }
      if (activeSnapshotId === snapshotId) setActiveSnapshotId(null);
      reload();
    },
    [activeSnapshotId, reload],
  );

  const selectedCount = selectedIds.size;

  const counts = useMemo(
    () => ({
      queued: jobs.filter((j) => ACTIVE.has(j.status)).length,
      done: sorted.filter((j) => j.status === 'done').length,
      error: jobs.filter((j) => j.status === 'error').length,
    }),
    [jobs, sorted],
  );

  const hasTerminal = jobs.some((j) => ['done', 'error', 'cancelled'].includes(j.status));
  const isOnboarding = totalEntries === 0;
  const noResults = !isOnboarding && sorted.length === 0;

  return (
    <div
      data-testid="aiweb-view"
      className="flex flex-col h-full"
      style={{ background: 'var(--bg-primary)' }}
    >
      {/* ── Header ──────────────────────────────────────────────────────── */}
      <div
        className="flex items-center gap-2 px-5 py-4 border-b"
        style={{ borderColor: 'var(--border)' }}
      >
        <Globe size={18} style={{ color: 'var(--accent)' }} />
        <h1
          className="text-base font-semibold font-display"
          style={{ color: 'var(--text-primary)' }}
        >
          {t('headerTitle')}
        </h1>
        <div
          className="flex items-center gap-3 ml-3 text-xs"
          style={{ color: 'var(--text-muted)' }}
        >
          <span className="flex items-center gap-1">
            <Hourglass size={11} /> {t('headerQueued', { n: counts.queued })}
          </span>
          <span className="flex items-center gap-1" title={t('headerArchiveTitle')}>
            <Archive size={11} style={{ color: 'var(--success)' }} /> {counts.done}
          </span>
          {counts.error > 0 && (
            <span className="flex items-center gap-1" style={{ color: 'var(--error)' }}>
              <AlertTriangle size={11} /> {counts.error}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2 ml-auto">
          <button
            data-testid="aiweb-add"
            onClick={() => onAddSite?.()}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded text-sm font-medium u-press"
            style={{ background: 'var(--accent)', color: '#fff' }}
          >
            <Plus size={14} /> {t('addSite')}
          </button>
          {!isOnboarding &&
            (selectMode ? (
              <button
                data-testid="aiweb-select-done"
                onClick={exitSelectMode}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded text-sm u-press"
                style={{
                  background: 'var(--bg-secondary)',
                  color: 'var(--text-primary)',
                  border: '1px solid var(--border)',
                }}
              >
                <X size={14} /> {tc('cancel')}
              </button>
            ) : (
              <button
                data-testid="aiweb-select"
                onClick={() => setSelectMode(true)}
                title={t('selectMultipleTitle')}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded text-sm u-press"
                style={{
                  background: 'var(--bg-secondary)',
                  color: 'var(--text-primary)',
                  border: '1px solid var(--border)',
                }}
              >
                <CheckCircle2 size={14} /> {t('selectAction')}
              </button>
            ))}
          <button
            data-testid="aiweb-clear"
            onClick={() => clearCompleted?.()}
            disabled={!hasTerminal}
            title={t('clearTitle')}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded text-sm u-press disabled:opacity-40 disabled:cursor-not-allowed"
            style={{
              background: 'var(--bg-secondary)',
              color: 'var(--text-primary)',
              border: '1px solid var(--border)',
            }}
          >
            <Trash2 size={14} /> {t('clear')}
          </button>
        </div>
      </div>

      {/* ── Search bar (archive of analysed sites) ──────────────────────── */}
      {!isOnboarding && (
        <div
          className="flex items-center gap-3 px-5 py-2.5 border-b"
          style={{ borderColor: 'var(--border)' }}
        >
          <div className="relative w-[300px]">
            <Search
              size={14}
              className="absolute left-2.5 top-1/2 -translate-y-1/2 pointer-events-none"
              style={{ color: 'var(--text-muted)' }}
            />
            <input
              data-testid="aiweb-search"
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder={t('searchPlaceholder')}
              className="w-full rounded-md pl-7 pr-7 py-1.5 text-sm outline-none u-transition"
              style={{
                background: 'var(--bg-secondary)',
                border: '1px solid var(--border)',
                color: 'var(--text-primary)',
              }}
            />
            {searchQuery && (
              <button
                onClick={() => setSearchQuery('')}
                title={t('clearSearchTitle')}
                className="absolute right-1.5 top-1/2 -translate-y-1/2 flex items-center justify-center w-5 h-5 rounded u-press"
                style={{ color: 'var(--text-muted)' }}
              >
                <X size={13} />
              </button>
            )}
          </div>
          <span className="ml-auto text-xs tabular-nums" style={{ color: 'var(--text-muted)' }}>
            {sorted.length} {sorted.length === 1 ? t('siteOne') : t('siteOther')}
          </span>
        </div>
      )}

      {/* ── Selection toolbar (multi-delete) ────────────────────────────── */}
      {!isOnboarding && selectMode && (
        <div
          data-testid="aiweb-select-toolbar"
          className="flex items-center gap-3 px-5 py-2.5 border-b u-fade-in"
          style={{ borderColor: 'var(--border)', background: 'var(--bg-secondary)' }}
        >
          <span className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>
            {selectedCount === 1
              ? t('siteSelectedOne', { n: selectedCount })
              : t('siteSelectedOther', { n: selectedCount })}
          </span>
          <span className="text-xs" style={{ color: 'var(--text-muted)' }}>
            {t('shiftRangeHint')}
          </span>
          <button
            data-testid="aiweb-delete-selected"
            disabled={selectedCount === 0 || busy}
            onClick={() => setDeleteDialog({ count: selectedCount })}
            className="ml-auto flex items-center gap-1.5 px-3 py-1.5 rounded text-sm u-press disabled:opacity-40 disabled:cursor-not-allowed"
            style={{ background: 'var(--error)', color: '#fff' }}
          >
            <Trash2 size={14} /> {tc('delete')}
          </button>
        </div>
      )}

      {/* ── Body: queue (left) + detail (right) ─────────────────────────── */}
      {isOnboarding ? (
        <div
          data-testid="aiweb-empty"
          className="flex flex-col items-center justify-center flex-1 gap-4 px-6 text-center u-fade-in-up"
        >
          <Globe size={42} style={{ color: 'var(--text-muted)' }} strokeWidth={1.25} />
          <div>
            <p className="text-base font-medium" style={{ color: 'var(--text-primary)' }}>
              {t('emptyTitle')}
            </p>
            <p className="text-sm mt-1 max-w-sm" style={{ color: 'var(--text-muted)' }}>
              {t('emptyHint')}
            </p>
          </div>
          <button
            onClick={() => onAddSite?.()}
            className="flex items-center gap-1.5 px-4 py-2 rounded text-sm font-medium u-press"
            style={{ background: 'var(--accent)', color: '#fff' }}
          >
            <Plus size={15} /> {t('addSite')}
          </button>
        </div>
      ) : (
        <div className="flex-1 flex overflow-hidden">
          <div
            className="w-[300px] min-w-[300px] border-r overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e]"
            style={{ borderColor: 'var(--border)' }}
          >
            {noResults ? (
              <div className="flex flex-col items-center justify-center h-full gap-2 px-5 text-center u-fade-in">
                <Search size={24} style={{ color: 'var(--text-muted)' }} strokeWidth={1.25} />
                <p className="text-sm" style={{ color: 'var(--text-muted)' }}>
                  {t('noResults', { query: effectiveQuery })}
                </p>
              </div>
            ) : (
              sorted.map((job) => (
                <QueueRow
                  key={job.key}
                  job={job}
                  selected={job.key === selectedKey}
                  onSelect={setSelectedKey}
                  onCancel={cancelJob ?? (() => {})}
                  onRetry={retryJob ?? (() => {})}
                  selectMode={selectMode}
                  checked={job.postId ? selectedIds.has(job.postId) : false}
                  onToggle={handleRowToggle}
                  snapshotCount={(job.postId ? snapshotCounts[job.postId] || 0 : 0) + 1}
                />
              ))
            )}
          </div>
          <div className="flex-1 overflow-hidden">
            {selected && detailJob ? (
              <Detail
                job={detailJob}
                post={detailPost}
                now={now}
                aiJob={detailAiJob}
                modelReady={modelReady}
                onOpenPost={onOpenPost}
                onCancel={cancelJob ?? (() => {})}
                onRetry={retryJob ?? (() => {})}
                onAiCancel={analysis?.cancelJob}
                onAiRetry={analysis?.retryJob}
                versions={versions}
                activeVersionId={activeSnapshotId}
                onSelectVersion={setActiveSnapshotId}
                onDeleteSnapshot={handleDeleteSnapshot}
              />
            ) : (
              <div className="flex flex-col items-center justify-center h-full gap-2 text-center px-6 u-fade-in">
                <Sparkles size={30} style={{ color: 'var(--text-muted)' }} strokeWidth={1} />
                <p className="text-sm" style={{ color: 'var(--text-muted)' }}>
                  {t('selectSitePrompt')}
                </p>
              </div>
            )}
          </div>
        </div>
      )}

      {/* ── Delete confirmation: complete vs report-only ────────────────── */}
      {deleteDialog && (
        <div
          data-testid="aiweb-delete-dialog"
          className="fixed inset-0 z-[200] flex items-center justify-center p-6 u-fade-in"
          style={{ background: 'rgba(0,0,0,0.55)' }}
          onClick={() => !busy && setDeleteDialog(null)}
        >
          <div
            className="w-full max-w-md rounded-xl border p-5 u-fade-in-up"
            style={{ background: 'var(--bg-secondary)', borderColor: 'var(--border)' }}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-start gap-3">
              <span
                className="shrink-0 flex items-center justify-center w-9 h-9 rounded-lg"
                style={{ background: 'var(--bg-hover)', color: 'var(--error)' }}
              >
                <Trash2 size={18} />
              </span>
              <div className="min-w-0">
                <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
                  {deleteDialog.count === 1
                    ? t('deleteTitleOne', { n: deleteDialog.count })
                    : t('deleteTitleOther', { n: deleteDialog.count })}
                </h3>
                <p className="text-xs mt-0.5" style={{ color: 'var(--text-muted)' }}>
                  {t('deleteSubtitle')}
                </p>
              </div>
            </div>
            <div className="mt-4 flex flex-col gap-2">
              <button
                data-testid="aiweb-delete-report"
                disabled={busy}
                onClick={() => runDelete('report')}
                className="text-left rounded-lg border px-3 py-2.5 u-press disabled:opacity-50"
                style={{ borderColor: 'var(--border)', background: 'var(--bg-hover)' }}
              >
                <div className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>
                  {t('deleteReportTitle')}
                </div>
                <div className="text-xs mt-0.5" style={{ color: 'var(--text-muted)' }}>
                  {t('deleteReportHint')}
                </div>
              </button>
              <button
                data-testid="aiweb-delete-complete"
                disabled={busy}
                onClick={() => runDelete('complete')}
                className="text-left rounded-lg border px-3 py-2.5 u-press disabled:opacity-50"
                style={{ borderColor: 'var(--error)', background: 'var(--bg-hover)' }}
              >
                <div className="text-sm font-medium" style={{ color: 'var(--error)' }}>
                  {t('deleteCompleteTitle')}
                </div>
                <div className="text-xs mt-0.5" style={{ color: 'var(--text-muted)' }}>
                  {t('deleteCompleteHint')}
                </div>
              </button>
            </div>
            <div className="mt-3 flex justify-end">
              <button
                disabled={busy}
                onClick={() => setDeleteDialog(null)}
                className="px-3 py-1.5 rounded text-sm u-press disabled:opacity-50"
                style={{ color: 'var(--text-secondary)' }}
              >
                {tc('cancel')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
