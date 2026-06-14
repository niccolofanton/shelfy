import React, { useState, useEffect, useMemo, useRef } from 'react';
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
  X,
} from 'lucide-react';
import { useDownloadPrefs } from '../hooks/useDownloadPrefs';
import { useToast } from '../hooks/useToast';
import { assetThumbUrl } from '../lib/asset';
import SourceIcon, { PLATFORM_COLORS, PLATFORM_LABELS } from '../components/SourceIcon';
import { useT } from '../i18n';

// Map the backend asset/status enums to localized labels. Unknown values fall
// back to the raw enum so a new backend state never renders blank.
const ASSET_KEYS = { thumbnail: 'assetThumbnail', image: 'assetImage', video: 'assetVideo' };
const STATUS_KEYS = {
  pending: 'statusPending',
  downloading: 'statusDownloading',
  done: 'statusDone',
  error: 'statusError',
  cancelled: 'statusCancelled',
};

// ── Platform icon ──────────────────────────────────────────────────────────────

// The real brand glyph (shared SourceIcon), tinted with the brand colour — the
// same source icon the gallery and post modal use, not a text "IG/PIN/TW" badge.
function PlatformIcon({ platform }) {
  const color = PLATFORM_COLORS[platform] || 'var(--text-muted)';
  return (
    <span
      className="inline-flex items-center justify-center w-5 h-5 shrink-0"
      style={{ color }}
      title={PLATFORM_LABELS[platform] || platform}
    >
      <SourceIcon platform={platform} size={15} className="shrink-0" />
    </span>
  );
}

// ── Asset type icon ───────────────────────────────────────────────────────────

function AssetIcon({ type }) {
  const props = { size: 14, className: 'shrink-0' };
  let icon = null;
  if (type === 'thumbnail') icon = <Layers {...props} style={{ color: '#a78bfa' }} />;
  else if (type === 'image') icon = <Image {...props} style={{ color: '#34d399' }} />;
  else if (type === 'video') icon = <Film {...props} style={{ color: '#60a5fa' }} />;
  if (!icon) return null;
  // Same 20px centred box as PlatformIcon so the asset icon lines up vertically
  // under the platform icon (and the labels share the same left edge).
  return <span className="inline-flex items-center justify-center w-5 h-5 shrink-0">{icon}</span>;
}

// ── Status indicator ──────────────────────────────────────────────────────────

function StatusIndicator({ status }) {
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

function ProgressBar({ progress }) {
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

function JobThumb({ job }) {
  const localPath = job.thumbnailPath || job.imagePath;
  // 128px thumb (40px tile @2x DPR with margin) — never the full-res original.
  const src = localPath ? assetThumbUrl(localPath, 128) : job.thumbnailUrl || null;
  const [failedSrc, setFailedSrc] = useState(null);

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

// Memoized: progress flushes replace only the touched job objects (upsert by
// key in useDownloads), so untouched rows bail out on identity. onCancel/onRetry
// are stable useCallbacks from the hook.
const JobRow = React.memo(function JobRow({ job, isPaused, onCancel, onRetry }) {
  const t = useT('downloads');
  const { key, postId, platform, assetType, status, progress, error, authorUsername } = job;
  const label = authorUsername ? `@${authorUsername}` : String(postId);
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
          : /* pending */ 'var(--text-muted)';

  // Queued/active rows dim while the queue is paused (done/error keep full opacity).
  const dimmed = isPaused && (status === 'pending' || status === 'downloading');

  return (
    <div
      data-testid="download-job"
      data-status={status}
      className={`flex flex-col gap-1.5 px-4 py-3 border-b u-transition${isError ? ' u-shake' : ''}`}
      style={{
        borderColor: 'var(--border)',
        background: isError ? 'var(--error)14' : isActive ? 'var(--accent)0d' : 'transparent',
        opacity: dimmed ? 0.5 : 1,
      }}
    >
      <div className="flex items-center gap-3 min-w-0">
        <JobThumb job={job} />

        <div className="flex flex-col min-w-0 flex-1 gap-0.5">
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
          <div className="flex items-center gap-1.5">
            <AssetIcon type={assetType} />
            <span className="text-xs capitalize" style={{ color: 'var(--text-muted)' }}>
              {ASSET_KEYS[assetType] ? t(ASSET_KEYS[assetType]) : assetType}
            </span>
          </div>
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
            onMouseEnter={(e) => {
              e.currentTarget.style.color = 'var(--error)';
              e.currentTarget.style.background = 'var(--bg-hover)';
            }}
            onMouseLeave={(e) => {
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
            onMouseEnter={(e) => {
              e.currentTarget.style.color = 'var(--accent)';
              e.currentTarget.style.background = 'var(--bg-hover)';
            }}
            onMouseLeave={(e) => {
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
const STATUS_RANK = { downloading: 0, pending: 1, error: 2, cancelled: 3, done: 4 };

// Memoized (see export below): kept alive by App, it only needs to re-render
// when the downloads slice itself changes, not on every unrelated App update.
function Downloads({ downloads }) {
  const t = useT('downloads');
  const tc = useT('common');
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

  useEffect(() => {
    refresh();
  }, [refresh]);

  async function handleDownloadAll() {
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

  async function handleDownloadMissing() {
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

  const sortedJobs = useMemo(
    () => [...jobs].sort((a, b) => (STATUS_RANK[a.status] ?? 5) - (STATUS_RANK[b.status] ?? 5)),
    [jobs],
  );

  // Virtualize the job list: the queue can hold thousands of rows, and mounting
  // them all stutters scrolling. Row heights vary (progress bar / error line),
  // so we let the virtualizer measure each rendered row instead of assuming a
  // fixed size.
  const scrollRef = useRef(null);
  const rowVirtualizer = useVirtualizer({
    count: sortedJobs.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 65,
    overscan: 8,
    // Key the measured-size cache by job identity (matching the React key on the
    // wrapper div below), not by index. Rows have variable height and the list
    // re-sorts as jobs change status, so an index-keyed cache would hold the
    // previous occupant's height and cause a brief jitter on reorder.
    getItemKey: (index) =>
      sortedJobs[index]?.key ??
      `${sortedJobs[index].postId}:${sortedJobs[index].assetType}:${sortedJobs[index].mediaPosition ?? 0}`,
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
              onMouseEnter={(e) => {
                if (!e.currentTarget.disabled)
                  e.currentTarget.style.background = 'var(--accent-hover)';
              }}
              onMouseLeave={(e) => {
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
              onMouseEnter={(e) => {
                if (!e.currentTarget.disabled) e.currentTarget.style.background = 'var(--bg-hover)';
              }}
              onMouseLeave={(e) => {
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
              onMouseEnter={(e) => {
                if (!e.currentTarget.disabled) e.currentTarget.style.background = 'var(--bg-hover)';
              }}
              onMouseLeave={(e) => {
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
              onMouseEnter={(e) => {
                if (!e.currentTarget.disabled) e.currentTarget.style.background = 'var(--bg-hover)';
              }}
              onMouseLeave={(e) => {
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
              onMouseEnter={(e) => {
                if (!e.currentTarget.disabled) e.currentTarget.style.background = 'var(--bg-hover)';
              }}
              onMouseLeave={(e) => {
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
          <p className="u-fade-in text-xs tabular-nums" style={{ color: 'var(--text-muted)' }}>
            {t('progressSummary', { done: doneCount, total: totalJobs, count: totalJobs })}
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

      {/* ── Job list (virtualized) ──────────────────────────────────────── */}
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
              const job = sortedJobs[vrow.index];
              return (
                <div
                  key={job.key ?? `${job.postId}:${job.assetType}:${job.mediaPosition ?? 0}`}
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
                  <JobRow job={job} isPaused={isPaused} onCancel={cancelJob} onRetry={retryJob} />
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

function StatPill({ label, value, icon, index = 0 }) {
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

function Divider() {
  return <span style={{ color: 'var(--border)' }}>|</span>;
}
