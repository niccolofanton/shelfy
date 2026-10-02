import React, { useState, useEffect, useMemo, useRef, useCallback } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import {
  Download,
  CheckCircle,
  XCircle,
  Loader,
  Image,
  Film,
  Layers,
  Trash2,
  ImageOff,
  Pause,
  Play,
  RotateCw,
  ChevronDown,
  X,
} from 'lucide-react';
import { useDownloadPrefs } from '../hooks/useDownloadPrefs';
import { useToast } from '../hooks/useToast';
import { assetThumbUrl } from '../lib/asset';
import SourceIcon, { PLATFORM_COLORS, PLATFORM_LABELS } from '../components/SourceIcon';
import { useT, useLang, localeTag } from '../i18n';

// Translator returned by useT — namespaced key + optional interpolation vars.
type Translate = (key: string, vars?: Record<string, string | number>) => string;

// The downloader's runtime job record (download:getStatus → onDownloadProgress).
// It has no Shelfy.* domain type (it's a downloader-internal shape, not the
// persisted `jobs` row or the Shelfy.DownloadJob DB row — see
// types/electron-api.d.ts), so the fields the queue actually reads are described
// here. Display-only; the backend owns the authoritative state.
interface DownloadJob {
  key: string;
  postId: string;
  platform: string;
  assetType: string;
  mediaPosition?: number | null;
  status: string;
  progress: number;
  error?: string | null;
  authorUsername?: string | null;
  thumbnailUrl?: string | null;
  thumbnailPath?: string | null;
  imagePath?: string | null;
}

// The slice of the downloads hook this view consumes (returned by useDownloads).
interface DownloadsApi {
  jobs: DownloadJob[];
  stats: { total: number; thumbnails: number; images: number; videos: number };
  refresh: () => void;
  clearAll: () => void;
  clearCompleted: () => void;
  cancelJob: (key: string) => void;
  retryJob: (key: string) => void;
  isPaused: boolean;
  pauseAll: () => void;
  resumeAll: () => void;
}

// Map the backend asset/status enums to localized labels. Unknown values fall
// back to the raw enum so a new backend state never renders blank.
const ASSET_KEYS: Record<string, string> = {
  thumbnail: 'assetThumbnail',
  image: 'assetImage',
  video: 'assetVideo',
};
const STATUS_KEYS: Record<string, string> = {
  pending: 'statusPending',
  downloading: 'statusDownloading',
  done: 'statusDone',
  error: 'statusError',
  cancelled: 'statusCancelled',
};

// ── Platform icon ──────────────────────────────────────────────────────────────

interface PlatformIconProps {
  platform: string;
}

// The real brand glyph (shared SourceIcon), tinted with the brand colour — the
// same source icon the gallery and post modal use, not a text "IG/PIN/TW" badge.
function PlatformIcon({ platform }: PlatformIconProps): React.JSX.Element {
  // `platform` is a backend string; the colour/label maps and SourceIcon are
  // keyed by Shelfy.Platform. Unknown values fall back at runtime (`||` here,
  // `null` inside SourceIcon), so cast the lookup key to the keyed type.
  const key = platform as Shelfy.Platform;
  const color = PLATFORM_COLORS[key] || 'var(--text-muted)';
  return (
    <span
      className="inline-flex items-center justify-center w-5 h-5 shrink-0"
      style={{ color }}
      title={PLATFORM_LABELS[key] || platform}
    >
      <SourceIcon platform={key} size={15} className="shrink-0" />
    </span>
  );
}

// ── Asset type icon ───────────────────────────────────────────────────────────

interface AssetIconProps {
  type: string;
}

function AssetIcon({ type }: AssetIconProps): React.JSX.Element | null {
  const props = { size: 14, className: 'shrink-0' };
  let icon: React.JSX.Element | null = null;
  if (type === 'thumbnail') icon = <Layers {...props} style={{ color: '#a78bfa' }} />;
  else if (type === 'image') icon = <Image {...props} style={{ color: '#34d399' }} />;
  else if (type === 'video') icon = <Film {...props} style={{ color: '#60a5fa' }} />;
  if (!icon) return null;
  // Same 20px centred box as PlatformIcon so the asset icon lines up vertically
  // under the platform icon (and the labels share the same left edge).
  return <span className="inline-flex items-center justify-center w-5 h-5 shrink-0">{icon}</span>;
}

// ── Status indicator ──────────────────────────────────────────────────────────

interface StatusIndicatorProps {
  status: string;
}

function StatusIndicator({ status }: StatusIndicatorProps): React.JSX.Element {
  if (status === 'done')
    return (
      <CheckCircle size={16} className="u-pop-in shrink-0" style={{ color: 'var(--success)' }} />
    );
  if (status === 'error')
    return <XCircle size={16} className="u-pop-in shrink-0" style={{ color: 'var(--error)' }} />;
  if (status === 'downloading')
    return (
      <Loader size={16} className="shrink-0 animate-spin" style={{ color: 'var(--accent)' }} />
    );
  return (
    <span
      className="inline-block w-4 h-4 rounded-full shrink-0"
      style={{ background: 'var(--bg-hover)', border: '2px solid var(--border)' }}
    />
  );
}

// ── Progress bar ──────────────────────────────────────────────────────────────

interface ProgressBarProps {
  progress?: number | null;
}

function ProgressBar({ progress }: ProgressBarProps): React.JSX.Element {
  const pct = Math.round((progress ?? 0) * 100);
  return (
    <div
      className="w-full rounded-full overflow-hidden"
      style={{ height: 3, background: 'var(--bg-hover)' }}
    >
      <div
        className="h-full rounded-full u-progress"
        style={{ width: `${pct}%`, background: 'var(--accent)' }}
      />
    </div>
  );
}

// ── Single job row ─────────────────────────────────────────────────────────────

interface JobThumbProps {
  job: DownloadJob;
}

function JobThumb({ job }: JobThumbProps): React.JSX.Element {
  const localPath = job.thumbnailPath || job.imagePath;
  // 128px thumb (40px tile @2x DPR with margin) — never the full-res original.
  const src = localPath ? assetThumbUrl(localPath, 128) : job.thumbnailUrl || null;
  const [failedSrc, setFailedSrc] = useState<string | null>(null);

  if (!src || failedSrc === src) {
    return (
      <div
        className="w-10 h-10 rounded shrink-0 flex items-center justify-center"
        style={{ background: 'var(--bg-hover)' }}
      >
        <ImageOff size={16} style={{ color: 'var(--text-muted)' }} />
      </div>
    );
  }
  return (
    <img
      src={src}
      alt=""
      className="w-10 h-10 rounded object-cover shrink-0"
      style={{ background: 'var(--bg-hover)' }}
      draggable={false}
      onError={() => setFailedSrc(src)}
    />
  );
}

interface JobRowProps {
  job: DownloadJob;
  isPaused: boolean;
  onCancel?: (key: string) => void;
  onRetry?: (key: string) => void;
}

// Memoized: progress flushes replace only the touched job objects (upsert by
// key in useDownloads), so untouched rows bail out on identity. onCancel/onRetry
// are stable useCallbacks from the hook.
const JobRow = React.memo(function JobRow({
  job,
  isPaused,
  onCancel,
  onRetry,
}: JobRowProps): React.JSX.Element {
  const t: Translate = useT('downloads');
  const { key, assetType, mediaPosition, status, progress, error } = job;
  const isActive = status === 'downloading';
  const isError = status === 'error';

  // Per-row controls: cancel a queued/active job, or retry a failed/cancelled
  // one. The backend key (job.key) is required for the IPC call.
  const canCancel = status === 'pending' || status === 'downloading';
  const canRetry = status === 'error' || status === 'cancelled';

  const statusColor =
    status === 'done'
      ? 'var(--success)'
      : status === 'error'
        ? 'var(--error)'
        : status === 'downloading'
          ? 'var(--accent)'
          : /* pending */ 'var(--text-secondary)';

  // Queued/active rows dim while the queue is paused (done/error keep full opacity).
  const dimmed = isPaused && (status === 'pending' || status === 'downloading');

  return (
    <div
      data-testid="download-job"
      data-status={status}
      className={`flex flex-col gap-1.5 py-2 pl-14 pr-4 border-t u-transition${isError ? ' u-shake' : ''}`}
      style={{
        borderColor: 'var(--border)',
        background: isError ? 'var(--error)14' : isActive ? 'var(--accent)0d' : 'transparent',
        opacity: dimmed ? 0.5 : 1,
      }}
    >
      <div className="flex items-center gap-3 min-w-0">
        <div className="flex items-center gap-1.5 min-w-0 flex-1">
          <AssetIcon type={assetType} />
          <span className="text-xs capitalize truncate" style={{ color: 'var(--text-secondary)' }}>
            {ASSET_KEYS[assetType] ? t(ASSET_KEYS[assetType]) : assetType}
            {mediaPosition != null ? ` ${mediaPosition + 1}` : ''}
          </span>
        </div>

        <span
          key={status}
          className="u-swap-in text-xs capitalize shrink-0 transition-colors tabular-nums"
          style={{ color: statusColor, minWidth: 52, textAlign: 'right' }}
        >
          {status === 'downloading'
            ? `${Math.round((progress ?? 0) * 100)}%`
            : STATUS_KEYS[status]
              ? t(STATUS_KEYS[status])
              : status}
        </span>

        <StatusIndicator status={status} />

        {canCancel && (
          <button
            data-testid="job-cancel"
            onClick={() => onCancel?.(key)}
            title={t('cancelJob')}
            className="u-press shrink-0 p-1 rounded u-transition"
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
            <X size={14} />
          </button>
        )}

        {canRetry && (
          <button
            data-testid="job-retry"
            onClick={() => onRetry?.(key)}
            title={t('retryJob')}
            className="u-press shrink-0 p-1 rounded u-transition"
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
            <RotateCw size={14} />
          </button>
        )}
      </div>

      {status === 'downloading' && <ProgressBar progress={progress} />}

      {status === 'error' && error && (
        <p className="u-fade-in text-xs truncate" style={{ color: 'var(--error)' }} title={error}>
          {error}
        </p>
      )}
    </div>
  );
});

// ── Main view ─────────────────────────────────────────────────────────────────

// Active downloads on top, queued next, finished last (done at the very end).
const STATUS_RANK: Record<string, number> = {
  downloading: 0,
  pending: 1,
  error: 2,
  cancelled: 3,
  done: 4,
};

interface DownloadPostGroup {
  postId: string;
  jobs: DownloadJob[];
  rank: number;
}

function groupDownloadJobs(jobs: DownloadJob[]): DownloadPostGroup[] {
  const byPost = new Map<string, DownloadPostGroup>();
  for (const job of jobs) {
    let group = byPost.get(job.postId);
    if (!group) {
      group = { postId: job.postId, jobs: [], rank: STATUS_RANK[job.status] ?? 5 };
      byPost.set(job.postId, group);
    }
    group.jobs.push(job);
    group.rank = Math.min(group.rank, STATUS_RANK[job.status] ?? 5);
  }
  return Array.from(byPost.values()).sort((a, b) => a.rank - b.rank);
}

function DownloadStage({
  label,
  jobs,
}: {
  label: string;
  jobs: DownloadJob[];
}): React.JSX.Element | null {
  if (jobs.length === 0) return null;
  const done = jobs.filter((j) => j.status === 'done').length;
  const failed = jobs.some((j) => j.status === 'error' || j.status === 'cancelled');
  const active = jobs.some((j) => j.status === 'downloading');
  const color =
    done === jobs.length
      ? 'var(--success)'
      : failed
        ? 'var(--error)'
        : active
          ? 'var(--accent)'
          : 'var(--text-secondary)';
  return (
    <span className="inline-flex items-center gap-1.5 text-[11px] tabular-nums" style={{ color }}>
      <span className="w-1.5 h-1.5 rounded-full shrink-0" style={{ background: color }} />
      {label} {done}/{jobs.length}
    </span>
  );
}

interface PostGroupRowProps {
  group: DownloadPostGroup;
  expanded: boolean;
  onToggle: (postId: string) => void;
  isPaused: boolean;
  onCancel: (key: string) => void;
  onRetry: (key: string) => void;
}

const PostGroupRow = React.memo(function PostGroupRow({
  group,
  expanded,
  onToggle,
  isPaused,
  onCancel,
  onRetry,
}: PostGroupRowProps): React.JSX.Element {
  const t: Translate = useT('downloads');
  const { jobs, postId } = group;
  const thumbnailJobs = jobs.filter((j) => j.assetType === 'thumbnail');
  const contentJobs = jobs.filter((j) => j.assetType !== 'thumbnail');
  const done = jobs.filter((j) => j.status === 'done').length;
  const active = jobs.some((j) => j.status === 'downloading');
  const failed = jobs.some((j) => j.status === 'error' || j.status === 'cancelled');
  const progress =
    jobs.reduce(
      (sum, j) => sum + (j.status === 'done' ? 1 : j.status === 'downloading' ? j.progress : 0),
      0,
    ) / jobs.length;
  const representative = thumbnailJobs[0] || jobs[0];
  const label = representative.authorUsername
    ? `@${representative.authorUsername}`
    : String(postId);

  return (
    <div
      data-testid="download-post-group"
      data-post-id={postId}
      className="border-b"
      style={{
        borderColor: 'var(--border)',
        background: failed ? 'var(--error)08' : active ? 'var(--accent)08' : 'transparent',
      }}
    >
      <button
        type="button"
        className="w-full text-left px-4 pt-3 pb-2 u-transition hover:bg-white/[0.03]"
        onClick={() => onToggle(postId)}
        aria-expanded={expanded}
        aria-controls={`download-post-${postId}`}
      >
        <div className="flex items-center gap-3 min-w-0">
          <JobThumb job={representative} />
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-1.5 min-w-0">
              <PlatformIcon platform={representative.platform} />
              <span
                className="text-sm truncate"
                style={{ color: 'var(--text-primary)' }}
                title={postId}
              >
                {label}
              </span>
            </div>
            <div className="flex items-center gap-3 mt-1.5 flex-wrap">
              <DownloadStage label={t('stagePreview')} jobs={thumbnailJobs} />
              <DownloadStage label={t('stageContent')} jobs={contentJobs} />
            </div>
          </div>
          <span
            className="text-xs tabular-nums shrink-0"
            style={{ color: 'var(--text-secondary)' }}
          >
            {done}/{jobs.length}
          </span>
          <ChevronDown
            size={15}
            className={`shrink-0 u-transition ${expanded ? 'rotate-180' : ''}`}
            style={{ color: 'var(--text-muted)' }}
          />
        </div>
      </button>
      <div
        className="h-0.5 mx-4 mb-2 rounded-full overflow-hidden"
        style={{ background: 'var(--bg-hover)' }}
      >
        <div
          className="h-full u-progress"
          style={{
            width: `${Math.round(progress * 100)}%`,
            background: failed
              ? 'var(--error)'
              : done === jobs.length
                ? 'var(--success)'
                : 'var(--accent)',
          }}
        />
      </div>
      <div id={`download-post-${postId}`}>
        {expanded &&
          jobs.map((job) => (
            <JobRow
              key={job.key}
              job={job}
              isPaused={isPaused}
              onCancel={onCancel}
              onRetry={onRetry}
            />
          ))}
      </div>
    </div>
  );
});

interface DownloadsProps {
  downloads: DownloadsApi;
}

// Memoized (see export below): kept alive by App, it only needs to re-render
// when the downloads slice itself changes, not on every unrelated App update.
function Downloads({ downloads }: DownloadsProps): React.JSX.Element {
  const t: Translate = useT('downloads');
  const tc: Translate = useT('common');
  const { lang } = useLang();
  const {
    jobs,
    stats,
    refresh,
    clearAll,
    clearCompleted,
    cancelJob,
    retryJob,
    isPaused,
    pauseAll,
    resumeAll,
  } = downloads;
  const { selectedTypes } = useDownloadPrefs();
  // Inline feedback for the bulk download buttons — shared toast hook (same as
  // Gallery). The download:all IPC can reject (DB locked, disk error during
  // ensureDirs) and without this the button would silently appear to do nothing.
  const { toast: feedback, showToast: showFeedback } = useToast();
  const [expandedPostIds, setExpandedPostIds] = useState<Set<string>>(() => new Set());
  const togglePost = useCallback((postId: string) => {
    setExpandedPostIds((current) => {
      const next = new Set(current);
      if (next.has(postId)) next.delete(postId);
      else next.add(postId);
      return next;
    });
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  async function handleDownloadAll(): Promise<void> {
    const types = selectedTypes();
    if (!types.length) return;
    try {
      await window.electronAPI.downloadAll(types, false);
    } catch (err) {
      console.error('[Downloads] downloadAll error:', err);
      showFeedback(tc('genericError'));
    } finally {
      refresh();
    }
  }

  async function handleDownloadMissing(): Promise<void> {
    const types = selectedTypes();
    if (!types.length) return;
    try {
      await window.electronAPI.downloadAll(types, true);
    } catch (err) {
      console.error('[Downloads] downloadMissing error:', err);
      showFeedback(tc('genericError'));
    } finally {
      refresh();
    }
  }

  const totalJobs = jobs.length;
  // "Finished" = terminal rows the clear-finished control can dismiss without
  // touching anything still queued/active. All three counters come out of one
  // memoized pass instead of three full scans per render.
  const { doneCount, hasQueue, finishedCount } = useMemo(() => {
    let done = 0;
    let finished = 0;
    let queued = false;
    for (const j of jobs) {
      if (j.status === 'done') {
        done++;
        finished++;
      } else if (j.status === 'cancelled') {
        finished++;
      } else if (j.status === 'pending' || j.status === 'downloading') {
        queued = true;
      }
    }
    return { doneCount: done, hasQueue: queued, finishedCount: finished };
  }, [jobs]);

  const sortedGroups = useMemo(() => groupDownloadJobs(jobs), [jobs]);

  // Virtualize post groups; expanded groups contain their individual asset jobs.
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const rowVirtualizer = useVirtualizer({
    count: sortedGroups.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 92,
    overscan: 8,
    getItemKey: (index) => sortedGroups[index]?.postId ?? index,
  });
  const virtualRows = rowVirtualizer.getVirtualItems();

  return (
    <div
      data-testid="downloads-view"
      className="flex flex-col h-full"
      style={{ background: 'var(--bg-primary)' }}
    >
      {/* ── Sticky header ───────────────────────────────────────────────── */}
      <div
        className="u-fade-in-down sticky top-0 z-10 flex flex-col gap-3 px-5 py-4 border-b"
        style={{ background: 'var(--bg-primary)', borderColor: 'var(--border)' }}
      >
        {/* Title row */}
        <div className="flex items-center gap-2">
          <Download size={18} style={{ color: 'var(--accent)' }} />
          <h1
            className="text-base font-semibold font-display"
            style={{ color: 'var(--text-primary)' }}
          >
            {t('title')}
          </h1>
        </div>

        {/* Controls row */}
        <div className="flex flex-wrap items-center gap-3">
          <div className="flex items-center gap-2 ml-auto">
            <button
              data-testid="download-all"
              onClick={handleDownloadAll}
              disabled={!selectedTypes().length}
              className="u-press flex items-center gap-1.5 px-3 py-1.5 rounded text-sm font-medium disabled:opacity-40 disabled:cursor-not-allowed"
              style={{ background: 'var(--accent)', color: '#fff' }}
              onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
                if (!e.currentTarget.disabled)
                  e.currentTarget.style.background = 'var(--accent-hover)';
              }}
              onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
                e.currentTarget.style.background = 'var(--accent)';
              }}
            >
              <Download size={14} />
              {t('downloadAll')}
            </button>

            <button
              data-testid="download-missing"
              onClick={handleDownloadMissing}
              disabled={!selectedTypes().length}
              className="u-press flex items-center gap-1.5 px-3 py-1.5 rounded text-sm font-medium disabled:opacity-40 disabled:cursor-not-allowed"
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
              <Download size={14} />
              {t('downloadMissing')}
            </button>

            <button
              data-testid="pause-resume"
              onClick={isPaused ? resumeAll : pauseAll}
              disabled={!hasQueue}
              title={isPaused ? t('pauseResumeTitlePaused') : t('pauseResumeTitleActive')}
              className={`u-press u-transition flex items-center gap-1.5 px-3 py-1.5 rounded text-sm font-medium disabled:opacity-40 disabled:cursor-not-allowed${isPaused ? ' u-glow' : ''}`}
              style={{
                background: 'var(--bg-secondary)',
                color: isPaused ? 'var(--accent)' : 'var(--text-primary)',
                border: '1px solid var(--border)',
              }}
              onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
                if (!e.currentTarget.disabled) e.currentTarget.style.background = 'var(--bg-hover)';
              }}
              onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
                e.currentTarget.style.background = 'var(--bg-secondary)';
              }}
            >
              <span
                key={isPaused ? 'play' : 'pause'}
                className="u-swap-in inline-flex items-center gap-1.5"
              >
                {isPaused ? <Play size={14} /> : <Pause size={14} />}
                {isPaused ? tc('resume') : tc('pause')}
              </span>
            </button>

            <button
              data-testid="clear-finished"
              onClick={clearCompleted}
              disabled={!finishedCount}
              title={t('clearFinishedTitle')}
              className="u-press flex items-center gap-1.5 px-3 py-1.5 rounded text-sm font-medium disabled:opacity-40 disabled:cursor-not-allowed"
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
              <CheckCircle size={14} />
              {t('clearFinished')}
            </button>

            <button
              data-testid="clear-queue"
              onClick={clearAll}
              disabled={!totalJobs}
              title={t('clearQueueTitle')}
              className="u-press flex items-center gap-1.5 px-3 py-1.5 rounded text-sm font-medium disabled:opacity-40 disabled:cursor-not-allowed"
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
              {t('clearQueue')}
            </button>
          </div>
        </div>

        {/* Progress summary */}
        {totalJobs > 0 && (
          <p className="u-fade-in text-xs tabular-nums" style={{ color: 'var(--text-secondary)' }}>
            {t('progressSummary', {
              done: doneCount.toLocaleString(localeTag(lang)),
              total: totalJobs.toLocaleString(localeTag(lang)),
              posts: sortedGroups.length.toLocaleString(localeTag(lang)),
              count: totalJobs,
            })}
          </p>
        )}

        {/* Bulk-download feedback (only shown when an enqueue request fails) */}
        {feedback && (
          <p
            key={feedback}
            data-testid="downloads-feedback"
            className="u-pop-in text-xs whitespace-nowrap"
            style={{ color: 'var(--error)' }}
          >
            {feedback}
          </p>
        )}
      </div>

      {/* ── Stats bar ───────────────────────────────────────────────────── */}
      <div
        data-testid="downloads-stats"
        className="u-fade-in-down flex items-center gap-4 px-5 py-2.5 border-b text-xs"
        style={{ background: 'var(--bg-secondary)', borderColor: 'var(--border)' }}
      >
        <StatPill index={0} label={t('statTotal')} value={stats?.total ?? 0} />
        <Divider />
        <StatPill
          index={1}
          label={t('statThumbnails')}
          value={stats?.thumbnails ?? 0}
          icon={<Layers size={11} style={{ color: '#a78bfa' }} />}
        />
        <Divider />
        <StatPill
          index={2}
          label={t('statImages')}
          value={stats?.images ?? 0}
          icon={<Image size={11} style={{ color: '#34d399' }} />}
        />
        <Divider />
        <StatPill
          index={3}
          label={t('statVideos')}
          value={stats?.videos ?? 0}
          icon={<Film size={11} style={{ color: '#60a5fa' }} />}
        />
      </div>

      {/* ── Post groups (virtualized) ──────────────────────────────────── */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto">
        {jobs.length === 0 ? (
          <div
            data-testid="downloads-empty"
            className="u-fade-in-up flex flex-col items-center justify-center h-full gap-3"
          >
            <Download size={40} style={{ color: 'var(--text-muted)' }} />
            <p className="text-sm" style={{ color: 'var(--text-muted)' }}>
              {t('empty')}
            </p>
          </div>
        ) : (
          <div style={{ height: rowVirtualizer.getTotalSize(), position: 'relative' }}>
            {virtualRows.map((vrow) => {
              const group = sortedGroups[vrow.index];
              return (
                <div
                  key={group.postId}
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
                  <PostGroupRow
                    group={group}
                    expanded={expandedPostIds.has(group.postId)}
                    onToggle={togglePost}
                    isPaused={isPaused}
                    onCancel={cancelJob}
                    onRetry={retryJob}
                  />
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

export default React.memo(Downloads);

// ── Tiny helpers ──────────────────────────────────────────────────────────────

interface StatPillProps {
  label: string;
  value: number;
  icon?: React.ReactNode;
  index?: number;
}

function StatPill({ label, value, icon, index = 0 }: StatPillProps): React.JSX.Element {
  return (
    <span
      className="u-fade-in flex items-center gap-1"
      style={{ color: 'var(--text-secondary)', animationDelay: `${index * 30}ms` }}
    >
      {icon}
      {label}: <span style={{ color: 'var(--text-primary)', fontWeight: 600 }}>{value}</span>
    </span>
  );
}

function Divider(): React.JSX.Element {
  return <span style={{ color: 'var(--border)' }}>|</span>;
}
