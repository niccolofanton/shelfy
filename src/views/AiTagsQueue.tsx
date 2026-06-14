import React, { useState, useEffect, useLayoutEffect, useCallback, useRef, useMemo } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import {
  Sparkles,
  CheckCircle,
  XCircle,
  Clock,
  ImageOff,
  RotateCw,
  X,
  Hourglass,
  AlertTriangle,
  Film,
  Image as ImageIcon,
  Layers,
  FileText,
  Cpu,
  Pause,
  Play,
  Trash2,
} from 'lucide-react';
import { useAnalysis, analysisSummary, liveRemainingMs } from '../hooks/useAnalysis';
import { useToast } from '../hooks/useToast';
import { assetThumbUrl } from '../lib/asset';
import { formatDuration, formatEta } from '../lib/duration';
import { useT } from '../i18n';

// The analyzer's runtime job record (analyze:getJobs → onAnalyzeProgress). It has
// no Shelfy.* domain type (it's an analyzer-internal shape, not the persisted
// `jobs` row — see types/electron-api.d.ts), so the fields the queue actually
// reads are described here. Display-only; the backend owns the authoritative state.
interface AnalyzeJob {
  key: string;
  postId: string;
  platform?: Shelfy.Platform;
  status: 'pending' | 'extracting' | 'analyzing' | 'done' | 'error' | 'cancelled';
  error?: string | null;
  authorUsername?: string | null;
  description?: string | null;
  tags?: string[] | null;
  stage?: string | null;
  mediaType?: Shelfy.MediaType | null;
  model?: string | null;
  thumbnailPath?: string | null;
  imagePath?: string | null;
  thumbnailUrl?: string | null;
  streamText?: string | null;
  startedAt?: number | null;
  finishedAt?: number | null;
  queuedAt?: number | null;
  durationMs?: number | null;
  phaseProgress?: number | null;
}

// The shape analysisSummary() returns (mirrors the JS reducer in useAnalysis).
interface AnalysisSummary {
  activeJobs: AnalyzeJob[];
  doneJobs: AnalyzeJob[];
  errorJobs: AnalyzeJob[];
  running: AnalyzeJob | null;
  runningJobs: AnalyzeJob[];
  concurrency: number;
  active: boolean;
  done: number;
  total: number;
  current: string;
  remaining: number;
  lastDurationMs: number | null;
  avgDurationMs: number | null;
  etaMs: number | null;
}

// The slice of the analysis context this view consumes.
interface AnalysisContextValue {
  jobs: AnalyzeJob[];
  modelStatus: { ready?: boolean } | null;
  isPaused: boolean;
  concurrency: number;
  cancelJob: (key: string) => void;
  clearAll: () => void;
  clearCompleted: () => void;
  pauseAll: () => void;
  resumeAll: () => void;
  retryJob: (key: string) => void;
}

// ── Platform badge ────────────────────────────────────────────────────────────

interface PlatformIconProps {
  platform?: Shelfy.Platform;
}

function PlatformIcon({ platform }: PlatformIconProps): React.JSX.Element {
  const t = useT('aiQueue');
  const meta =
    platform === 'instagram'
      ? { bg: '#e1306c22', fg: '#e1306c', label: 'IG', title: 'Instagram' }
      : platform === 'pinterest'
        ? { bg: '#e6002322', fg: '#e60023', label: 'PIN', title: 'Pinterest' }
        : platform === 'web'
          ? { bg: '#7b5cff22', fg: '#7b5cff', label: 'WEB', title: t('platformWeb') }
          : { bg: '#1da1f222', fg: '#1da1f2', label: 'TW', title: 'Twitter' };
  return (
    <span
      className="inline-flex items-center justify-center w-5 h-5 rounded text-[10px] font-bold shrink-0"
      style={{ background: meta.bg, color: meta.fg }}
      title={meta.title}
    >
      {meta.label}
    </span>
  );
}

// ── Media-type icon (explains why frame extraction happens — or doesn't) ────────

interface MediaTypeIconProps {
  mediaType?: Shelfy.MediaType | null;
}

function MediaTypeIcon({ mediaType }: MediaTypeIconProps): React.JSX.Element {
  const props = { size: 13, className: 'shrink-0' };
  if (mediaType === 'video') return <Film {...props} style={{ color: '#60a5fa' }} />;
  if (mediaType === 'carousel' || mediaType === 'images')
    return <Layers {...props} style={{ color: '#a78bfa' }} />;
  if (mediaType === 'text') return <FileText {...props} style={{ color: '#9ca3af' }} />;
  return <ImageIcon {...props} style={{ color: '#34d399' }} />;
}

// media type / status enum → message key. Translated at the use-site so the maps
// can stay module-level (i18n is per-render, not per-module).
const MEDIA_TYPE_KEY: Record<string, string> = {
  video: 'mediaVideo',
  carousel: 'mediaCarousel',
  images: 'mediaImages',
  image: 'mediaImage',
  text: 'mediaText',
};

const STATUS_KEY: Record<string, string> = {
  pending: 'statusPending',
  extracting: 'statusExtracting',
  analyzing: 'statusAnalyzing',
  done: 'statusDone',
  error: 'statusError',
  cancelled: 'statusCancelled',
};

// Every row is exactly this tall — the in-flight row grows in length, never height.
const ROW_H = 76;

// ── Phase stepper: Coda → Processing → Analisi → Descrizione → Tags → Fatto ──────
// The six pipeline phases. The last three (Analisi/Descrizione/Tags) are derived
// from the model's streamed output: as the JSON forms, the description and then
// the tags array appear, advancing the active phase.
const PHASE_KEYS = [
  'phaseQueue',
  'phaseProcessing',
  'phaseAnalysis',
  'phaseDescription',
  'phaseTags',
  'phaseDone',
];

// Map a job onto a phase index. The first two come straight from status; while
// analyzing we read the partial streamed JSON to tell whether the description is
// being written (3) or the tags have started (4). Done is the last phase.
function jobPhaseIndex(job: AnalyzeJob): number {
  switch (job.status) {
    case 'pending':
      return 0;
    case 'extracting':
      return 1;
    case 'analyzing': {
      const t = job.streamText || '';
      if (/"tags"\s*:\s*\[/.test(t)) return 4; // tags array started
      if (/"description"\s*:\s*"./.test(t)) return 3; // description has content
      return 2; // model running, nothing yet
    }
    case 'done':
      return 5;
    default:
      return -1; // error / cancelled — handled separately
  }
}

// Six phases are too many to label inline in a narrow row, so the stepper is a
// compact segmented track (filled up to the active phase) plus the active phase's
// name. Settled segments read in soft lavender, the working one in full accent.
const PHASE_DONE_COLOR = '#b9a6ff';

interface PhaseStepperProps {
  active: number;
  pulse: boolean;
}

function PhaseStepper({ active, pulse }: PhaseStepperProps): React.JSX.Element | null {
  const t = useT('aiQueue');
  if (active < 0) return null;
  const isDone = active >= PHASE_KEYS.length - 1;
  return (
    <div className="flex items-center gap-1.5 min-w-0" data-testid="ai-queue-phases">
      <div className="flex items-center gap-0.5 shrink-0">
        {PHASE_KEYS.map((key, i) => (
          <span
            key={key}
            className="h-1 rounded-full u-transition"
            style={{
              width: i === active ? 14 : 6,
              background:
                i < active ? PHASE_DONE_COLOR : i === active ? 'var(--accent)' : 'var(--border)',
            }}
          />
        ))}
      </div>
      <span
        className={['text-[10px] font-medium truncate', pulse ? 'animate-pulse' : ''].join(' ')}
        style={{ color: isDone ? PHASE_DONE_COLOR : 'var(--accent)' }}
      >
        {t(PHASE_KEYS[active])}
      </span>
    </div>
  );
}

// ── Thumbnail ───────────────────────────────────────────────────────────────

interface JobThumbProps {
  job: AnalyzeJob;
  px?: number;
  rounded?: string;
}

function JobThumb({ job, px = 48, rounded = 'rounded' }: JobThumbProps): React.JSX.Element {
  const localPath = job.thumbnailPath || job.imagePath;
  // 128px thumb (48px tile @2x DPR with margin) — never the full-res original.
  const src = localPath ? assetThumbUrl(localPath, 128) : job.thumbnailUrl || null;
  const [failed, setFailed] = useState(false);
  // Always a square box; the image is letterboxed inside it via object-contain.
  const dim = { width: px, height: px, background: 'var(--bg-hover)' };
  if (!src || failed) {
    return (
      <div className={`${rounded} shrink-0 flex items-center justify-center`} style={dim}>
        <ImageOff size={Math.round(px / 2.7)} style={{ color: 'var(--text-muted)' }} />
      </div>
    );
  }
  return (
    <img
      src={src}
      alt=""
      className={`${rounded} object-contain shrink-0`}
      style={dim}
      draggable={false}
      onError={() => setFailed(true)}
    />
  );
}

// Lenient readers that pull fields out of the model's PARTIAL streamed JSON, so
// the in-flight row can show the description forming and tags popping in before
// the object closes. Display-only — the backend does the authoritative parse.
function streamedString(text: string | null | undefined, key: string): string {
  if (!text) return '';
  const m = text.match(new RegExp(`"${key}"\\s*:\\s*"`));
  if (!m || m.index === undefined) return '';
  let out = '';
  for (let i = m.index + m[0].length; i < text.length; i++) {
    const c = text[i];
    if (c === '\\') {
      const n = text[i + 1];
      if (n === undefined) break;
      out += n === 'n' ? '\n' : n === 't' ? '\t' : n;
      i++;
      continue;
    }
    if (c === '"') break;
    out += c;
  }
  return out;
}

function streamedArray(text: string | null | undefined, key: string): string[] {
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
      else break; // partial last item — wait for it to close
    } else {
      i++;
    }
  }
  return items;
}

// ── Streaming description: a smooth "someone typing" reveal ────────────────────
// The backend pushes the whole partial description on each (throttled) token
// event, which would make the text jump in coarse chunks. Instead we keep a
// `displayed` string that catches up to the target a few characters per animation
// frame, so it reads as fluid typing. While streaming we animate; once the value
// is settled (done) we snap straight to the full text.
interface StreamingTextProps {
  text: string;
  animate: boolean;
}

function StreamingText({ text, animate }: StreamingTextProps): React.JSX.Element {
  const ref = useRef<HTMLDivElement | null>(null);
  const targetRef = useRef<string>(text);
  targetRef.current = text;
  const [displayed, setDisplayed] = useState<string>(animate ? '' : text);

  useEffect(() => {
    if (!animate) {
      setDisplayed(targetRef.current);
      return undefined;
    }
    let raf: number;
    const tick = (): void => {
      setDisplayed((cur) => {
        const target = targetRef.current;
        if (cur === target) return cur;
        // New job / diverged text → snap rather than mangle a mismatched prefix.
        if (!target.startsWith(cur)) return target;
        // Catch up faster the further behind we are, so a burst still reads as
        // brisk typing rather than lagging; the min of 1 keeps it always moving.
        const step = Math.max(1, Math.ceil((target.length - cur.length) / 4));
        return target.slice(0, cur.length + step);
      });
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [animate]);

  // Keep the newest character in view + fade the left edge only when it overflows.
  const [fadeLeft, setFadeLeft] = useState(false);
  const measure = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    el.scrollLeft = el.scrollWidth;
    setFadeLeft(el.scrollWidth - el.clientWidth > 1);
  }, []);
  useLayoutEffect(() => {
    measure();
  }, [displayed, measure]);
  useEffect(() => {
    const el = ref.current;
    if (!el) return undefined;
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [measure]);

  return (
    <div
      ref={ref}
      data-testid="ai-queue-stream"
      className={[
        'text-[12px] leading-snug whitespace-nowrap overflow-x-hidden',
        fadeLeft ? 'ai-fade-left' : '',
      ].join(' ')}
      style={{ height: 18 }}
    >
      <span style={{ color: 'var(--text-secondary)' }}>{displayed}</span>
    </div>
  );
}

// ── Single job row ──────────────────────────────────────────────────────────

interface JobRowProps {
  job: AnalyzeJob;
  now: number | null;
  onOpen?: (postId: string) => void;
  onCancel?: (key: string) => void;
  onRetry?: (key: string) => void;
}

// React.memo: il ticker da 1s aggiorna `now` solo per la riga in esecuzione (le
// altre ricevono `now={null}`), così le righe statiche non si ri-renderizzano
// ogni secondo. Le prop callback (onOpen/onCancel/onRetry) sono stabili.
const JobRow = React.memo(function JobRow({
  job,
  now,
  onOpen,
  onCancel,
  onRetry,
}: JobRowProps): React.JSX.Element {
  const t = useT('aiQueue');
  const {
    postId,
    platform,
    status,
    error,
    authorUsername,
    description,
    tags,
    stage,
    mediaType,
    model,
  } = job;
  const running = status === 'extracting' || status === 'analyzing';
  const queued = status === 'pending';
  const errored = status === 'error';
  const cancelled = status === 'cancelled';
  const isDone = status === 'done';
  const extracting = status === 'extracting';
  // The title is always the account handle — never the generated description, so
  // it stays stable as analysis fills in. Falls back to the post id only when the
  // handle is genuinely missing.
  const label = authorUsername ? `@${authorUsername}` : String(postId);
  const tagList = Array.isArray(tags) ? tags : [];

  // Running: read from the partial streamed JSON (description forming, tags
  // popping in). Done: the final fields. So a finished row keeps the exact same
  // layout — stepper + description + tags lane — just with settled values.
  const liveDesc = running
    ? streamedString(job.streamText, 'description')
    : isDone
      ? description || ''
      : '';
  const liveTags = running ? streamedArray(job.streamText, 'tags') : isDone ? tagList : [];

  // Tags overflow off the right end (the row isn't scrollable), so they fade on
  // the right — again only when there are more pills than fit.
  const tagsRef = useRef<HTMLDivElement | null>(null);
  const [tagsFadeRight, setTagsFadeRight] = useState(false);
  useLayoutEffect(() => {
    const el = tagsRef.current;
    if (!el) return undefined;
    const measure = (): void => setTagsFadeRight(el.scrollWidth - el.clientWidth > 1);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [liveTags]);

  const statusColor = isDone
    ? 'var(--success)'
    : errored
      ? 'var(--error)'
      : running
        ? 'var(--accent)'
        : /* pending/cancelled */ 'var(--text-muted)';

  // Live elapsed for the in-flight job; final duration for terminal jobs.
  const elapsedMs = running && job.startedAt && now != null ? now - job.startedAt : null;
  const durText = isDone
    ? formatDuration(job.durationMs)
    : running
      ? formatDuration(elapsedMs)
      : null;

  // Extraction reports real frame progress, shown as a % in the status column.
  const extractPct =
    status === 'extracting' && typeof job.phaseProgress === 'number'
      ? Math.round((job.phaseProgress ?? 0) * 100)
      : null;

  const clickable = !!(onOpen && postId);

  return (
    <div
      data-testid="ai-queue-job"
      data-status={status}
      onClick={clickable ? () => onOpen?.(postId) : undefined}
      className={[
        'relative flex items-center gap-3 px-4 border-b u-transition overflow-hidden',
        clickable ? 'cursor-pointer' : '',
      ].join(' ')}
      style={{
        height: ROW_H,
        borderColor: 'var(--border)',
        background: running ? 'rgba(123,92,255,0.05)' : 'transparent',
      }}
    >
      {/* Accent rail: a finished/working signal on the left edge while in flight. */}
      {running && (
        <span
          aria-hidden="true"
          className="u-bar-in absolute left-0 top-0 bottom-0 w-[3px]"
          style={{ background: 'var(--accent)', transformOrigin: 'top' }}
        />
      )}

      <JobThumb job={job} />

      {/* Identity + phase + meta (the original, left-hand elements). Fixed width
          so the phase stepper fits without overflowing into the stream lane. */}
      <div className="flex flex-col gap-1 shrink-0 w-[248px] overflow-hidden">
        <div className="flex items-center gap-1.5 min-w-0">
          <PlatformIcon platform={platform} />
          <span
            className="text-sm truncate"
            style={{ color: 'var(--text-primary)' }}
            title={postId}
          >
            {label}
          </span>
        </div>

        {/* In-flight + queued + done: phase stepper (done sits on "Fatto"), so a
            finished row keeps the same layout. Error: message. Else: status. */}
        {running || queued || isDone ? (
          <PhaseStepper active={jobPhaseIndex(job)} pulse={running} />
        ) : errored && error ? (
          <span className="text-xs truncate" style={{ color: 'var(--error)' }} title={error}>
            {error}
          </span>
        ) : (
          <span className="text-xs" style={{ color: 'var(--text-muted)' }}>
            {STATUS_KEY[status] ? t(STATUS_KEY[status]) : status}
          </span>
        )}

        {/* Context meta: media type + model. */}
        <div className="flex items-center gap-2 text-[10px]" style={{ color: 'var(--text-muted)' }}>
          <span className="flex items-center gap-1">
            <MediaTypeIcon mediaType={mediaType} />
            {mediaType && MEDIA_TYPE_KEY[mediaType]
              ? t(MEDIA_TYPE_KEY[mediaType])
              : t('mediaImage')}
          </span>
          {model && (running || isDone) && (
            <span className="flex items-center gap-1">
              <Cpu size={11} /> {model}
            </span>
          )}
        </div>
      </div>

      {/* Generation lane — to the RIGHT of the original elements. Always present
          (keeps the status column pinned right + every row the same height) and
          filled both while in flight and once done, so a finished row looks the
          same. Two single-line lanes that grow in LENGTH (scroll/overflow). */}
      <div className="flex-1 min-w-0 flex flex-col justify-center gap-1.5 self-stretch overflow-hidden">
        {(running || isDone) && (
          <>
            {/* Description — one line; typewriter reveal while streaming, snapped
                to the final text once done. */}
            {liveDesc ? (
              <StreamingText text={liveDesc} animate={running} />
            ) : running ? (
              <div className="text-[12px] leading-snug" style={{ height: 18 }}>
                <span className="italic truncate block" style={{ color: 'var(--text-muted)' }}>
                  {stage || (extracting ? t('preparingFrames') : t('generatingDescription'))}
                </span>
              </div>
            ) : (
              <div style={{ height: 18 }} />
            )}

            {/* Tags — one line, pills flow horizontally and overflow off the end */}
            <div
              ref={tagsRef}
              className={[
                'flex gap-1.5 overflow-hidden',
                tagsFadeRight ? 'ai-fade-right' : '',
              ].join(' ')}
              style={{ height: 18 }}
            >
              {liveTags.length > 0
                ? liveTags.map((tag, i) => (
                    <span
                      key={`${tag}-${i}`}
                      className="ai-tag-pop px-2 rounded-full text-[11px] leading-none h-[18px] flex items-center shrink-0"
                      style={{
                        background: 'var(--bg-hover)',
                        color: '#b9a6ff',
                        animationDelay: `${i * 40}ms`,
                      }}
                    >
                      {tag}
                    </span>
                  ))
                : running
                  ? [58, 80, 46].map((w, i) => (
                      <span
                        key={i}
                        className="ai-skel block shrink-0"
                        style={{ height: 18, width: w, borderRadius: 9999 }}
                      />
                    ))
                  : null}
            </div>
          </>
        )}
      </div>

      <div className="flex flex-col items-end shrink-0" style={{ minWidth: 64 }}>
        <span className="text-xs" style={{ color: statusColor, textAlign: 'right' }}>
          {extractPct != null
            ? `${extractPct}%`
            : STATUS_KEY[status]
              ? t(STATUS_KEY[status])
              : status}
        </span>
        {durText && (
          <span
            className="text-[11px] tabular-nums flex items-center gap-0.5"
            style={{ color: 'var(--text-muted)' }}
          >
            <Clock size={10} /> {durText}
          </span>
        )}
      </div>

      {(running || queued) && (
        <button
          data-testid="ai-queue-cancel"
          onClick={(e: React.MouseEvent<HTMLButtonElement>) => {
            e.stopPropagation();
            onCancel?.(job.key);
          }}
          title={t('cancelJob')}
          className="shrink-0 flex items-center justify-center w-7 h-7 rounded u-press"
          style={{ color: 'var(--text-muted)' }}
          onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
            e.currentTarget.style.color = 'var(--error)';
            e.currentTarget.style.background = 'var(--bg-hover)';
          }}
          onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
            e.currentTarget.style.color = 'var(--text-muted)';
            e.currentTarget.style.background = 'transparent';
          }}
        >
          <X size={15} />
        </button>
      )}
      {(errored || cancelled) && (
        <button
          data-testid="ai-queue-retry"
          onClick={(e: React.MouseEvent<HTMLButtonElement>) => {
            e.stopPropagation();
            onRetry?.(job.key);
          }}
          title={t('retryJob')}
          className="shrink-0 flex items-center justify-center w-7 h-7 rounded u-press"
          style={{ color: 'var(--text-muted)' }}
          onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
            e.currentTarget.style.color = 'var(--accent)';
            e.currentTarget.style.background = 'var(--bg-hover)';
          }}
          onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
            e.currentTarget.style.color = 'var(--text-muted)';
            e.currentTarget.style.background = 'transparent';
          }}
        >
          <RotateCw size={15} />
        </button>
      )}

      {isDone && (
        <CheckCircle
          size={16}
          className="ai-done-pop shrink-0"
          style={{ color: 'var(--success)' }}
        />
      )}
    </div>
  );
});

// ── Main view ─────────────────────────────────────────────────────────────────

// Active and pending share rank 0 on purpose: they form one stable FIFO block so
// an item never jumps when it goes pending → extracting → analyzing while other
// jobs start or finish around it (matters with parallel analysis).
const STATUS_RANK: Record<string, number> = {
  extracting: 0,
  analyzing: 0,
  pending: 0,
  error: 1,
  cancelled: 2,
  done: 3,
};

interface AiTagsQueueProps {
  onOpenPost?: (postId: string) => void;
}

export default function AiTagsQueue({ onOpenPost }: AiTagsQueueProps): React.JSX.Element {
  const t = useT('aiQueue');
  // Un-namespaced translator for the shared feedback string: the error copy lives
  // in the aiTags namespace (analyzeStartError) and is reused verbatim here.
  const tShared = useT();
  const { toast, toastClosing, showToast } = useToast();
  const {
    jobs,
    modelStatus,
    isPaused,
    concurrency,
    cancelJob,
    clearAll,
    clearCompleted,
    pauseAll,
    resumeAll,
    retryJob,
  } = useAnalysis() as AnalysisContextValue;
  // Web-reference cataloging is delegated to the SAME analyzer queue, so the raw
  // job list also carries platform==='web' jobs that belong to the AI Websites
  // view, not here. Filter them out so this view's rows, summary and counts only
  // ever reflect the social Auto-tag pipeline.
  const socialJobs = useMemo(() => jobs.filter((j) => j.platform !== 'web'), [jobs]);
  // Memoized: the 1s ticker and every streamed token re-render this view without
  // changing the job list, so the O(n) summary only recomputes on real changes.
  const summary = useMemo(
    () => analysisSummary(socialJobs, concurrency) as AnalysisSummary,
    [socialJobs, concurrency],
  );
  const { done, total, remaining, lastDurationMs, avgDurationMs, errorJobs, running } = summary;

  const modelReady = !!modelStatus?.ready;
  const hasActive = remaining > 0;

  // 1s ticker: keeps the in-flight elapsed time and the remaining estimate live.
  // Only runs while a job is actually processing, so an idle queue costs nothing.
  // NB: `running` is the in-flight job OBJECT, re-emitted with a fresh identity on
  // every streamed token (~every 100ms), so depending on it directly would tear
  // down and rebuild the interval before it can ever reach 1000ms — freezing
  // `now` for the whole generation. Depend on a stable boolean instead so the
  // interval is created once at start and actually fires every second.
  const isRunning = !!running;
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    if (!isRunning) return undefined;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [isRunning]);

  // "Quanto manca": parallel-aware countdown — spreads the work still left across
  // every busy slot and subtracts what each in-flight job has already run, so it
  // ticks down honestly whether one or N inferences are going at once.
  const remainingMs = liveRemainingMs(summary, now);

  // Single source for the list: one stable FIFO block of active + pending jobs
  // (ordered by queue time, the exact order they'll be picked), then errors and
  // history. Sorting active and pending together by queuedAt keeps each item
  // pinned to its slot through the whole pending → extracting → analyzing → done
  // lifecycle, so the list no longer re-shuffles every time a parallel slot
  // starts or finishes. The running job streams its generation inline.
  const listJobs = useMemo(
    () =>
      [...socialJobs].sort((a, b) => {
        const r = (STATUS_RANK[a.status] ?? 4) - (STATUS_RANK[b.status] ?? 4);
        if (r !== 0) return r;
        // Active + pending: stable FIFO order (oldest queued first), so an item
        // keeps its position until it leaves the block by finishing.
        if (STATUS_RANK[a.status] === 0) {
          return (a.queuedAt || 0) - (b.queuedAt || 0);
        }
        // Everything else (errors/history): most recent first.
        return (
          (b.finishedAt || b.startedAt || b.queuedAt || 0) -
          (a.finishedAt || a.startedAt || a.queuedAt || 0)
        );
      }),
    [socialJobs],
  );

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const rowVirtualizer = useVirtualizer({
    count: listJobs.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    overscan: 8,
  });
  const virtualRows = rowVirtualizer.getVirtualItems();

  async function handleAnalyzeMissing(): Promise<void> {
    try {
      await window.electronAPI.analyzeMissing();
    } catch {
      // Model unavailable, DB error or a main-process exception: surface a toast
      // so the click isn't a silent no-op (and the rejection isn't left floating).
      showToast(tShared('aiTags.analyzeStartError'));
    }
  }

  return (
    <div
      data-testid="ai-queue-view"
      className="flex flex-col h-full"
      style={{ background: 'var(--bg-primary)' }}
    >
      {/* ── Sticky header ───────────────────────────────────────────────── */}
      <div
        className="sticky top-0 z-10 flex flex-col gap-3 px-5 py-4 border-b"
        style={{ background: 'var(--bg-primary)', borderColor: 'var(--border)' }}
      >
        <div className="flex items-center gap-2">
          <Sparkles size={18} style={{ color: 'var(--accent)' }} />
          <h1
            className="text-base font-semibold font-display"
            style={{ color: 'var(--text-primary)' }}
          >
            {t('heading')}
          </h1>

          <div className="flex items-center gap-2 ml-auto">
            <button
              data-testid="ai-queue-analyze-missing"
              onClick={handleAnalyzeMissing}
              disabled={!modelReady}
              title={modelReady ? t('analyzeMissingReadyTitle') : t('analyzeMissingNotReadyTitle')}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded text-sm font-medium u-press disabled:opacity-40 disabled:cursor-not-allowed"
              style={{ background: 'var(--accent)', color: '#fff' }}
              onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
                if (!e.currentTarget.disabled)
                  e.currentTarget.style.background = 'var(--accent-hover)';
              }}
              onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
                e.currentTarget.style.background = 'var(--accent)';
              }}
            >
              <Sparkles size={14} />
              {t('analyzeMissing')}
            </button>

            <button
              data-testid="ai-queue-pause-resume"
              onClick={isPaused ? resumeAll : pauseAll}
              disabled={!hasActive}
              title={isPaused ? t('resumeTitle') : t('pauseTitle')}
              className={[
                'flex items-center gap-1.5 px-3 py-1.5 rounded text-sm font-medium u-press disabled:opacity-40 disabled:cursor-not-allowed',
                isPaused && hasActive ? 'u-glow' : '',
              ].join(' ')}
              style={{
                background: 'var(--bg-secondary)',
                color: isPaused && hasActive ? '#f0b429' : 'var(--text-primary)',
                border: '1px solid var(--border)',
              }}
              onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
                if (!e.currentTarget.disabled) e.currentTarget.style.background = 'var(--bg-hover)';
              }}
              onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
                e.currentTarget.style.background = 'var(--bg-secondary)';
              }}
            >
              {isPaused ? <Play size={14} /> : <Pause size={14} />}
              {isPaused ? t('resume') : t('pause')}
            </button>

            <button
              data-testid="ai-queue-clear-completed"
              onClick={clearCompleted}
              disabled={!socialJobs.some((j) => j.status === 'done' || j.status === 'cancelled')}
              title={t('clearCompletedTitle')}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded text-sm font-medium u-press disabled:opacity-40 disabled:cursor-not-allowed"
              style={{
                background: 'var(--bg-secondary)',
                color: 'var(--text-primary)',
                border: '1px solid var(--border)',
              }}
              onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
                if (!e.currentTarget.disabled) e.currentTarget.style.background = 'var(--bg-hover)';
              }}
              onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
                e.currentTarget.style.background = 'var(--bg-secondary)';
              }}
            >
              <Trash2 size={14} />
              {t('clearCompleted')}
            </button>

            <button
              data-testid="ai-queue-cancel-all"
              onClick={clearAll}
              disabled={!socialJobs.length}
              title={t('cancelAllTitle')}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded text-sm font-medium u-press disabled:opacity-40 disabled:cursor-not-allowed"
              style={{
                background: 'var(--bg-secondary)',
                color: 'var(--text-primary)',
                border: '1px solid var(--border)',
              }}
              onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
                if (!e.currentTarget.disabled) e.currentTarget.style.background = 'var(--bg-hover)';
              }}
              onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
                e.currentTarget.style.background = 'var(--bg-secondary)';
              }}
            >
              <Trash2 size={14} />
              {t('cancelAll')}
            </button>
          </div>
        </div>

        {!modelReady && (
          <p className="flex items-center gap-1.5 text-xs" style={{ color: 'var(--text-muted)' }}>
            <AlertTriangle size={13} style={{ color: '#f0b429' }} />
            {t('modelNotReady')}
          </p>
        )}

        {total > 0 && (
          <p
            className="flex items-center gap-2 text-xs tabular-nums"
            style={{ color: 'var(--text-muted)' }}
          >
            <span>{t('progressCount', { done, total })}</span>
            {isPaused && hasActive && (
              <span
                data-testid="ai-queue-paused"
                className="flex items-center gap-1"
                style={{ color: '#f0b429' }}
              >
                <Pause size={11} /> {t('paused')}
              </span>
            )}
          </p>
        )}
      </div>

      {/* ── Timing cards: last tag + average + estimated time remaining ─────── */}
      <div
        data-testid="ai-queue-timing"
        className="grid grid-cols-3 gap-px border-b"
        style={{ background: 'var(--border)', borderColor: 'var(--border)' }}
      >
        <TimingCard
          icon={<Clock size={13} style={{ color: 'var(--text-muted)' }} />}
          label={t('lastTag')}
          value={formatDuration(lastDurationMs) ?? '—'}
        />
        <TimingCard
          icon={<Cpu size={13} style={{ color: 'var(--text-muted)' }} />}
          label={t('avgTime')}
          value={formatDuration(avgDurationMs) ?? '—'}
        />
        <TimingCard
          icon={
            <Hourglass
              size={13}
              style={{ color: hasActive ? 'var(--accent)' : 'var(--text-muted)' }}
            />
          }
          label={t('eta')}
          value={hasActive ? (formatEta(remainingMs) ?? t('estimating')) : '—'}
          highlight={hasActive}
        />
      </div>

      {/* ── Counts bar ──────────────────────────────────────────────────── */}
      <div
        data-testid="ai-queue-stats"
        className="flex items-center gap-4 px-5 py-2.5 border-b text-xs flex-wrap"
        style={{ background: 'var(--bg-secondary)', borderColor: 'var(--border)' }}
      >
        <StatPill
          label={t('inQueue')}
          value={remaining}
          icon={<Hourglass size={11} style={{ color: 'var(--text-muted)' }} />}
        />
        <Divider />
        <StatPill
          label={t('completed')}
          value={done}
          icon={<CheckCircle size={11} style={{ color: 'var(--success)' }} />}
        />
        <Divider />
        <StatPill
          label={t('errors')}
          value={errorJobs.length}
          icon={<XCircle size={11} style={{ color: 'var(--error)' }} />}
          alert={errorJobs.length > 0}
        />
      </div>

      {/* ── Job list (virtualized) — in-flight row streams its generation inline ─ */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto">
        {socialJobs.length === 0 ? (
          <div
            data-testid="ai-queue-empty"
            className="flex flex-col items-center justify-center h-full gap-3 px-6 text-center u-fade-in"
          >
            <Sparkles size={40} style={{ color: 'var(--text-muted)' }} />
            <p className="text-sm" style={{ color: 'var(--text-muted)' }}>
              {t('empty')}
            </p>
          </div>
        ) : (
          <div style={{ height: rowVirtualizer.getTotalSize(), position: 'relative' }}>
            {virtualRows.map((vrow) => {
              const job = listJobs[vrow.index];
              return (
                <div
                  key={job.key ?? job.postId}
                  data-index={vrow.index}
                  ref={rowVirtualizer.measureElement}
                  style={{
                    position: 'absolute',
                    top: 0,
                    left: 0,
                    width: '100%',
                    transform: `translateY(${vrow.start}px)`,
                  }}
                >
                  <JobRow
                    job={job}
                    now={job.status === 'extracting' || job.status === 'analyzing' ? now : null}
                    onOpen={onOpenPost}
                    onCancel={cancelJob}
                    onRetry={retryJob}
                  />
                </div>
              );
            })}
          </div>
        )}
      </div>

      {toast && <Toast closing={toastClosing}>{toast}</Toast>}
    </div>
  );
}

// Self-dismissing feedback, mirroring AiTags/AiSearch. Used here only for the
// "Analyze missing" error path, so it carries the warning glyph rather than a
// success check.
interface ToastProps {
  children: React.ReactNode;
  closing: boolean;
}

function Toast({ children, closing }: ToastProps): React.JSX.Element {
  return (
    <div
      data-testid="ai-queue-toast"
      className={[
        'fixed bottom-6 left-1/2 -translate-x-1/2 z-50 flex items-center gap-2 rounded-lg border border-[#2e2e2e] bg-[#1a1a1a] px-4 py-2.5 text-sm text-gray-200 shadow-2xl',
        closing ? 'u-fade-out' : 'u-fade-in-up',
      ].join(' ')}
    >
      <AlertTriangle size={15} className="text-red-400" />
      {children}
    </div>
  );
}

// ── Tiny helpers ──────────────────────────────────────────────────────────────

interface TimingCardProps {
  icon: React.ReactNode;
  label: string;
  value: string;
  highlight?: boolean;
}

function TimingCard({ icon, label, value, highlight = false }: TimingCardProps): React.JSX.Element {
  return (
    <div
      className="flex flex-col gap-0.5 px-5 py-2.5"
      style={{ background: 'var(--bg-secondary)' }}
    >
      <span
        className="flex items-center gap-1 text-[10px] uppercase tracking-wide"
        style={{ color: 'var(--text-muted)' }}
      >
        {icon}
        {label}
      </span>
      <span
        className="text-base font-semibold tabular-nums font-display"
        style={{ color: highlight ? 'var(--accent)' : 'var(--text-primary)' }}
      >
        {value}
      </span>
    </div>
  );
}

interface StatPillProps {
  label: string;
  value: number;
  icon: React.ReactNode;
  alert?: boolean;
}

function StatPill({ label, value, icon, alert = false }: StatPillProps): React.JSX.Element {
  return (
    <span
      key={alert ? `err-${value}` : 'ok'}
      className={['flex items-center gap-1', alert ? 'u-shake' : ''].join(' ')}
      style={{ color: alert ? 'var(--error)' : 'var(--text-secondary)' }}
    >
      {icon}
      {label}:{' '}
      <span style={{ color: alert ? 'var(--error)' : 'var(--text-primary)', fontWeight: 600 }}>
        {value}
      </span>
    </span>
  );
}

function Divider(): React.JSX.Element {
  return <span style={{ color: 'var(--border)' }}>|</span>;
}
