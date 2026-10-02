import React, { useState } from 'react';
import {
  AlertTriangle,
  ChevronDown,
  ChevronUp,
  Loader2,
  RotateCw,
  ShieldAlert,
  Sparkles,
  X,
  ExternalLink,
} from 'lucide-react';
import { useT } from '../../i18n';
import type { AiJobView, SiteView, WebJob } from './model';
import { ACTIVE_STATUSES, hostOf, streamPreview } from './model';
import { ACCENT, SiteFavicon } from './ui';

// Compact live-work area above the grid: captures in progress, sites stuck on an
// anti-bot check (with the "pass the check" action), failed captures, and web
// references whose AI catalog is being written right now (streamed preview).

export interface QueueItem {
  key: string;
  postId: string | null;
  job: WebJob | null;
  ai: AiJobView | null;
  site: SiteView | null;
}

interface JobQueueProps {
  items: QueueItem[];
  unblocking: Set<string>;
  onOpen: (postId: string) => void;
  onCancel: (key: string) => void;
  onRetry: (key: string) => void;
  onUnblock: (key: string) => void;
  onAiCancel: (key: string) => void;
  onClear?: () => void;
  canClear: boolean;
}

export default function JobQueue({
  items,
  unblocking,
  onOpen,
  onCancel,
  onRetry,
  onUnblock,
  onAiCancel,
  onClear,
  canClear,
}: JobQueueProps): React.ReactElement | null {
  const t = useT('aiWebsites');
  const [collapsed, setCollapsed] = useState(false);
  if (!items.length) return null;

  const blocked = items.filter((i) => i.job?.status === 'blocked').length;
  const errors = items.filter((i) => i.job?.status === 'error').length;
  const running = items.length - blocked - errors;

  // Blocked first (they need the user), then running, then failures.
  const rank = (i: QueueItem): number =>
    i.job?.status === 'blocked'
      ? 0
      : i.job?.status === 'error' || i.job?.status === 'cancelled'
        ? 2
        : 1;
  const sorted = [...items].sort((a, b) => rank(a) - rank(b));

  return (
    <section
      data-testid="aiweb-queue"
      className="mx-6 mt-4 rounded-xl border border-[#262626] bg-[#141414] overflow-hidden u-fade-in"
    >
      <header className="flex items-center gap-3 px-4 h-10 border-b border-[#222]">
        <span className="text-[11px] font-semibold uppercase tracking-[0.12em] text-[#8a8a8a]">
          {t('queueTitle')}
        </span>
        <div className="flex items-center gap-3 text-[12px] text-[#8a8a8a]">
          {running > 0 && (
            <span className="flex items-center gap-1.5">
              <Loader2 size={12} className="u-spin" style={{ color: ACCENT }} />
              {t('queueRunning', { n: running })}
            </span>
          )}
          {blocked > 0 && (
            <span className="flex items-center gap-1.5 text-[#f0b429]">
              <ShieldAlert size={12} /> {t('queueBlocked', { n: blocked })}
            </span>
          )}
          {errors > 0 && (
            <span className="flex items-center gap-1.5 text-[#ef5350]">
              <AlertTriangle size={12} /> {t('queueErrors', { n: errors })}
            </span>
          )}
        </div>
        <div className="ml-auto flex items-center gap-1">
          {canClear && (
            <button
              type="button"
              data-testid="aiweb-clear"
              onClick={onClear}
              title={t('clearTitle')}
              className="px-2 h-7 rounded-md text-[12px] text-[#9a9a9a] hover:text-white hover:bg-[#1f1f1f] u-press"
            >
              {t('clear')}
            </button>
          )}
          <button
            type="button"
            onClick={() => setCollapsed((c) => !c)}
            aria-expanded={!collapsed}
            title={collapsed ? t('queueExpand') : t('queueCollapse')}
            className="flex items-center justify-center w-7 h-7 rounded-md text-[#9a9a9a] hover:text-white hover:bg-[#1f1f1f] u-press"
          >
            {collapsed ? <ChevronDown size={15} /> : <ChevronUp size={15} />}
          </button>
        </div>
      </header>
      {!collapsed && (
        <div className="flex flex-wrap gap-px bg-[#222]">
          {sorted.map((item) => (
            <QueueRow
              key={item.key}
              item={item}
              unblocking={!!item.job && unblocking.has(item.job.key)}
              onOpen={onOpen}
              onCancel={onCancel}
              onRetry={onRetry}
              onUnblock={onUnblock}
              onAiCancel={onAiCancel}
            />
          ))}
        </div>
      )}
    </section>
  );
}

interface QueueRowProps {
  item: QueueItem;
  unblocking: boolean;
  onOpen: (postId: string) => void;
  onCancel: (key: string) => void;
  onRetry: (key: string) => void;
  onUnblock: (key: string) => void;
  onAiCancel: (key: string) => void;
}

function QueueRow({
  item,
  unblocking,
  onOpen,
  onCancel,
  onRetry,
  onUnblock,
  onAiCancel,
}: QueueRowProps): React.ReactElement {
  const t = useT('aiWebsites');
  const tc = useT('common');
  const { job, ai, site } = item;
  const domain = site?.domain || job?.domain || hostOf(job?.url) || job?.url || '';
  const name = site?.name || domain || ai?.label || '';
  const status = job?.status;
  const isBlocked = status === 'blocked';
  const isError = status === 'error' || status === 'cancelled';
  const isRunning = !!status && ACTIVE_STATUSES.has(status);
  const aiRunning = !!ai && ['pending', 'extracting', 'analyzing'].includes(ai.status);
  const preview = aiRunning && ai ? streamPreview(ai.streamText) : null;

  let label = '';
  if (status && status !== 'done') label = t(`status.${status}`);
  else if (aiRunning && ai) label = t(`aiStatus.${ai.status}`);
  const stage = job && !isError ? job.stage : '';
  const pct = job ? Math.round(job.progress * 100) : 0;

  return (
    <div
      data-testid="aiweb-queue-row"
      data-status={status || ai?.status || ''}
      className={`relative flex-1 basis-[360px] min-w-0 flex flex-col gap-2 px-4 py-3 ${isBlocked ? 'bg-[#1d1808]' : 'bg-[#141414]'}`}
    >
      <div className="flex items-center gap-2.5 min-w-0">
        <SiteFavicon path={site?.favicon || ''} name={name} size={18} />
        <button
          type="button"
          disabled={!item.postId}
          onClick={() => item.postId && onOpen(item.postId)}
          className="flex-1 min-w-0 flex items-baseline gap-1.5 text-left u-press disabled:cursor-default"
        >
          <span className="truncate text-[13px] font-medium text-[#ececec]">{name}</span>
          {domain && domain !== name && (
            <span className="truncate text-[11.5px] text-[#666]">{domain}</span>
          )}
        </button>
        {isRunning && job && (
          <button
            type="button"
            data-testid="aiweb-queue-cancel"
            onClick={() => onCancel(job.key)}
            title={tc('cancel')}
            aria-label={tc('cancel')}
            className="shrink-0 flex items-center justify-center w-6 h-6 rounded-md text-[#7a7a7a] hover:text-white hover:bg-[#222] u-press"
          >
            <X size={13} />
          </button>
        )}
        {!isRunning && aiRunning && ai && (
          <button
            type="button"
            onClick={() => onAiCancel(ai.key)}
            title={tc('cancel')}
            aria-label={tc('cancel')}
            className="shrink-0 flex items-center justify-center w-6 h-6 rounded-md text-[#7a7a7a] hover:text-white hover:bg-[#222] u-press"
          >
            <X size={13} />
          </button>
        )}
      </div>

      <span
        className={`flex items-center gap-1.5 text-[11.5px] min-w-0 ${
          isBlocked ? 'text-[#f0b429]' : isError ? 'text-[#ef5350]' : 'text-[#a0a0a0]'
        }`}
      >
        {isBlocked ? (
          <ShieldAlert size={12} />
        ) : isError ? (
          <AlertTriangle size={12} />
        ) : aiRunning && !isRunning ? (
          <Sparkles size={12} style={{ color: ACCENT }} />
        ) : (
          <Loader2 size={12} className="u-spin" style={{ color: ACCENT }} />
        )}
        {label}
        {isRunning && <span className="tabular-nums text-[#6b6b6b]">{pct}%</span>}
        {stage && <span className="truncate text-[#7a7a7a]">· {stage}</span>}
      </span>

      {isRunning && job && (
        <div className="h-[3px] rounded-full bg-[#242424] overflow-hidden">
          <div
            className="h-full rounded-full u-progress"
            style={{ width: `${Math.max(2, pct)}%`, background: ACCENT }}
          />
        </div>
      )}

      {preview && preview.text && (
        <p
          data-testid="aiweb-queue-stream"
          className="text-[11.5px] leading-snug text-[#9d9d9d] line-clamp-2"
        >
          <span className="text-[#8b74ff]">{t(`streamKey.${preview.key}`)} · </span>
          {preview.text}
          <span className="opacity-60">▋</span>
        </p>
      )}

      {isBlocked && job && (
        <div className="flex flex-col gap-2" data-testid="aiweb-blocked">
          <p className="text-[12px] leading-relaxed text-[#d9c58f]">
            {t('blockedExplain', { vendor: job.blocked?.vendor || t('blockedVendorUnknown') })}
          </p>
          <div className="flex items-center gap-2">
            <button
              type="button"
              data-testid="aiweb-unblock"
              disabled={unblocking}
              onClick={() => onUnblock(job.key)}
              className="flex items-center gap-1.5 h-8 px-3 rounded-lg bg-[#f0b429] text-black text-[12.5px] font-semibold u-press hover:bg-[#ffc53d] disabled:opacity-70"
            >
              {unblocking ? <Loader2 size={13} className="u-spin" /> : <ExternalLink size={13} />}
              {unblocking ? t('unblockWaiting') : t('unblockAction')}
            </button>
            <button
              type="button"
              onClick={() => onCancel(job.key)}
              className="h-8 px-2.5 rounded-lg text-[12px] text-[#bba86f] hover:text-white hover:bg-white/5 u-press"
            >
              {tc('cancel')}
            </button>
          </div>
        </div>
      )}

      {isError && job && (
        <div className="flex items-center gap-2">
          {job.error && (
            <p className="flex-1 min-w-0 text-[11.5px] text-[#d77] truncate" title={job.error}>
              {job.error}
            </p>
          )}
          <button
            type="button"
            data-testid="aiweb-queue-retry"
            onClick={() => onRetry(job.key)}
            className="ml-auto shrink-0 flex items-center gap-1.5 h-7 px-2.5 rounded-md bg-[#1f1f1f] text-[12px] text-[#ddd] hover:bg-[#272727] u-press"
          >
            <RotateCw size={12} /> {tc('retry')}
          </button>
        </div>
      )}
    </div>
  );
}
