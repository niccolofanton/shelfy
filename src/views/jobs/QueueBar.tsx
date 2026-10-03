import React from 'react';
import { Ban, Pause, Play, Trash2 } from 'lucide-react';
import { useLang, useT } from '../../i18n';
import type { QueueSummary } from '../../api/jobs';
import { jobKindLabel } from './labels';

export interface QueueBarProps {
  queues: QueueSummary[];
  // The list's own kind filter: a selected chip doubles as that filter
  // (clicking it again clears it) — one set of controls, not two.
  selectedKinds: readonly string[];
  busyKinds: ReadonlySet<string>;
  onToggleKind: (kind: string) => void;
  onPause: (kind: string) => void;
  onResume: (kind: string) => void;
  onCancelAll: (kind: string) => void;
  onClearFinished: (kind: string) => void;
}

export default function QueueBar({
  queues,
  selectedKinds,
  busyKinds,
  onToggleKind,
  onPause,
  onResume,
  onCancelAll,
  onClearFinished,
}: QueueBarProps): React.JSX.Element | null {
  const t = useT('jobs');
  const { lang } = useLang();
  if (!queues.length) return null;

  return (
    <div
      data-testid="jobs-queue-bar"
      className="flex flex-wrap gap-2 px-4 py-3 border-b border-[#222]"
    >
      {queues.map((queue) => {
        const selected = selectedKinds.includes(queue.kind);
        const busy = busyKinds.has(queue.kind);
        const active = queue.queued + queue.running;
        const finished = queue.succeeded + queue.failed + queue.cancelled;
        return (
          <div
            key={queue.kind}
            data-testid="jobs-queue-row"
            data-kind={queue.kind}
            data-selected={selected}
            className={[
              'flex items-center gap-1.5 rounded-lg border px-2 py-1 text-[12px]',
              selected ? 'border-[#7B5CFF] bg-[#7B5CFF22]' : 'border-[#2a2a2a] bg-[#161616]',
            ].join(' ')}
          >
            <button
              type="button"
              data-testid="jobs-queue-kind-toggle"
              aria-pressed={selected}
              onClick={() => onToggleKind(queue.kind)}
              className="u-press flex items-center gap-1.5 text-[#ddd]"
            >
              <span>{jobKindLabel(lang, queue.kind)}</span>
              <span className="tabular-nums text-[#7a7a7a]">
                {active}/{active + finished}
              </span>
              {queue.paused && (
                <span data-testid="jobs-queue-paused" className="text-[#f0b429]">
                  {t('queuePausedBadge')}
                </span>
              )}
            </button>
            <button
              type="button"
              data-testid="jobs-queue-pause-toggle"
              disabled={busy}
              title={queue.paused ? t('resumeQueueTitle') : t('pauseQueueTitle')}
              aria-label={queue.paused ? t('resumeQueueTitle') : t('pauseQueueTitle')}
              onClick={() => (queue.paused ? onResume(queue.kind) : onPause(queue.kind))}
              className="u-press flex items-center justify-center w-6 h-6 rounded text-[#9a9a9a] hover:text-white hover:bg-[#222] disabled:opacity-50"
            >
              {queue.paused ? <Play size={12} /> : <Pause size={12} />}
            </button>
            <button
              type="button"
              data-testid="jobs-queue-cancel-all"
              disabled={busy || active === 0}
              title={t('cancelAllTitle')}
              aria-label={t('cancelAllTitle')}
              onClick={() => onCancelAll(queue.kind)}
              className="u-press flex items-center justify-center w-6 h-6 rounded text-[#9a9a9a] hover:text-white hover:bg-[#222] disabled:opacity-30"
            >
              <Ban size={12} />
            </button>
            <button
              type="button"
              data-testid="jobs-queue-clear-finished"
              disabled={busy || finished === 0}
              title={t('clearFinishedTitle')}
              aria-label={t('clearFinishedTitle')}
              onClick={() => onClearFinished(queue.kind)}
              className="u-press flex items-center justify-center w-6 h-6 rounded text-[#9a9a9a] hover:text-white hover:bg-[#222] disabled:opacity-30"
            >
              <Trash2 size={12} />
            </button>
          </div>
        );
      })}
    </div>
  );
}
