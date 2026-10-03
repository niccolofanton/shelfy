import React, { useRef, useState } from 'react';
import { Ban, MoreHorizontal, Pause, Play, Trash2 } from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import Popover from '../../components/Popover';
import { IconButton } from '../../components/ui';
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

interface QueueRowProps extends Omit<QueueBarProps, 'queues' | 'selectedKinds' | 'busyKinds'> {
  queue: QueueSummary;
  selected: boolean;
  busy: boolean;
}

const ITEM =
  'u-press flex w-full items-center gap-2.5 px-3 py-2 text-left text-sm text-primary hover:bg-hover disabled:cursor-not-allowed disabled:opacity-40 narrow:min-h-12';

function MenuItem({
  icon: Icon,
  label,
  disabled,
  testId,
  onClick,
}: {
  icon: LucideIcon;
  label: string;
  disabled?: boolean;
  testId: string;
  onClick: () => void;
}): React.JSX.Element {
  return (
    <button
      type="button"
      role="menuitem"
      data-testid={testId}
      disabled={disabled}
      onClick={onClick}
      className={ITEM}
    >
      <Icon size={15} aria-hidden="true" className="shrink-0 text-secondary" />
      {label}
    </button>
  );
}

// One queue: its chip (the kind filter) and a "⋯" menu with the operator
// controls spelled out in words (JOB-1) — they used to be four unlabeled
// 24px icons.
function QueueRow({
  queue,
  selected,
  busy,
  onToggleKind,
  onPause,
  onResume,
  onCancelAll,
  onClearFinished,
}: QueueRowProps): React.JSX.Element {
  const t = useT('jobs');
  const { lang } = useLang();
  const anchorRef = useRef<HTMLSpanElement | null>(null);
  const [open, setOpen] = useState(false);
  const active = queue.queued + queue.running;
  const finished = queue.succeeded + queue.failed + queue.cancelled;
  // Closes the menu, then runs the action.
  const run = (fn: (kind: string) => void) => (): void => {
    setOpen(false);
    fn(queue.kind);
  };

  return (
    <div
      data-testid="jobs-queue-row"
      data-kind={queue.kind}
      data-selected={selected}
      className={[
        'flex shrink-0 items-center gap-0.5 rounded-lg border pl-2.5 text-xs',
        selected ? 'border-accent bg-accent-fill/20' : 'border-subtle bg-card',
      ].join(' ')}
    >
      <button
        type="button"
        data-testid="jobs-queue-kind-toggle"
        aria-pressed={selected}
        onClick={() => onToggleKind(queue.kind)}
        className="u-press flex items-center gap-1.5 py-1.5 text-primary narrow:min-h-11"
      >
        <span>{jobKindLabel(lang, queue.kind)}</span>
        <span className="tabular-nums text-muted">{t('queueActive', { n: active })}</span>
        {queue.paused && (
          <span data-testid="jobs-queue-paused" className="text-warning">
            {t('queuePausedBadge')}
          </span>
        )}
      </button>
      <span ref={anchorRef} className="inline-flex">
        <IconButton
          data-testid="jobs-queue-menu"
          label={t('queueActions', { kind: jobKindLabel(lang, queue.kind) })}
          icon={MoreHorizontal}
          size="sm"
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
        />
      </span>
      <Popover
        anchorRef={anchorRef}
        open={open}
        align="right"
        presentation="auto"
        role="menu"
        aria-label={t('queueActions', { kind: jobKindLabel(lang, queue.kind) })}
        onRequestClose={() => setOpen(false)}
        data-testid="jobs-queue-menu-list"
        className="w-56 rounded-lg border border-strong bg-elevated py-1 shadow-2xl"
      >
        <MenuItem
          testId="jobs-queue-pause-toggle"
          icon={queue.paused ? Play : Pause}
          label={queue.paused ? t('resumeQueue') : t('pauseQueue')}
          disabled={busy}
          onClick={run(queue.paused ? onResume : onPause)}
        />
        <MenuItem
          testId="jobs-queue-cancel-all"
          icon={Ban}
          label={t('cancelQueued')}
          disabled={busy || active === 0}
          onClick={run(onCancelAll)}
        />
        <MenuItem
          testId="jobs-queue-clear-finished"
          icon={Trash2}
          label={t('clearFinished')}
          disabled={busy || finished === 0}
          onClick={run(onClearFinished)}
        />
      </Popover>
    </div>
  );
}

export default function QueueBar({
  queues,
  selectedKinds,
  busyKinds,
  ...handlers
}: QueueBarProps): React.JSX.Element | null {
  // A queue shows only while it has work (queued or running), or while it is
  // paused or used as the list filter — otherwise it is an empty operator
  // control on a user screen (JOB-1).
  const shown = queues.filter(
    (q) => q.queued + q.running > 0 || q.paused || selectedKinds.includes(q.kind),
  );
  if (!shown.length) return null;

  return (
    <div
      data-testid="jobs-queue-bar"
      className="flex gap-2 overflow-x-auto border-b border-subtle px-4 py-3 narrow:px-3"
    >
      {shown.map((queue) => (
        <QueueRow
          key={queue.kind}
          queue={queue}
          selected={selectedKinds.includes(queue.kind)}
          busy={busyKinds.has(queue.kind)}
          {...handlers}
        />
      ))}
    </div>
  );
}
