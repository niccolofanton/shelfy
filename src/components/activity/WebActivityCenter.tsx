import { useRef, useState } from 'react';
import { Bell } from 'lucide-react';
import { useNavigation } from '../../api/navigation';
import { useLang, useT } from '../../i18n';
import {
  activityKindLabel,
  activityKindTarget,
  notificationLabel,
  notificationTarget,
} from '../../api/activityRegistry';
import { jobErrorLabel, jobStageLabel, jobStateLabel } from '../../views/jobs/labels';
import { useWebActivity } from '../../hooks/useWebActivity';
import Popover from '../Popover';
import { Button, Notice, Spinner } from '../ui';

export default function WebActivityCenter({ onOpen }: { onOpen?: () => void }) {
  const activity = useWebActivity();
  const { lang } = useLang();
  const t = useT('activity');
  const navigation = useNavigation();
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const activeCount = activity.jobs.filter(
    (job) => job.state === 'queued' || job.state === 'running',
  ).length;
  const go = (target: string | null) => {
    const route = notificationTarget(target);
    if (route) {
      navigation?.navigate(route);
      setOpen(false);
    }
  };
  return (
    <div className="mx-2 mb-0.5">
      <button
        ref={anchor}
        type="button"
        data-testid="activity-strip"
        aria-expanded={open}
        aria-haspopup="dialog"
        onClick={() => {
          if (!open) onOpen?.();
          setOpen(!open);
        }}
        className="u-press flex w-full items-center gap-3 rounded-md px-4 py-2.5 narrow:min-h-11 text-left text-sm text-secondary hover:bg-hover hover:text-primary"
      >
        <Bell size={16} aria-hidden="true" />
        <span className="flex-1">{t('webTitleCenter')}</span>
        {activeCount > 0 && (
          <span
            data-testid="activity-badge"
            className="rounded-full bg-accent-fill px-1.5 text-xs text-white"
            aria-label={t('activeCount', { count: activeCount })}
          >
            {activeCount}
          </span>
        )}
        {activity.unread > 0 && (
          <span
            data-testid="activity-unread"
            aria-label={t('unreadCount', { count: activity.unread })}
            className="text-xs text-accent-text"
          >
            {activity.unread}
          </span>
        )}
      </button>
      <Popover
        anchorRef={anchor}
        open={open}
        onRequestClose={() => setOpen(false)}
        placement="top"
        presentation="auto"
        data-testid="activity-popover"
        aria-label={t('webTitleCenter')}
        className="w-[360px] max-w-[calc(100vw-24px)] max-h-[70vh] overflow-y-auto rounded-lg border border-strong bg-panel shadow-2xl"
        sheetClassName="overflow-y-auto bg-panel"
      >
        <header className="flex items-center justify-between gap-2 border-b border-subtle p-3">
          <h2 className="text-sm text-primary">{t('webTitleCenter')}</h2>
          <Button
            size="sm"
            variant="ghost"
            data-testid="activity-mark-all-read"
            disabled={activity.unread === 0}
            loading={activity.reading}
            onClick={() => void activity.markAllRead()}
          >
            {t('markAllRead')}
          </Button>
        </header>
        {activity.error != null && (
          <div className="p-3">
            <Notice tone="error">
              {t('loadError')}
              <Button size="sm" onClick={activity.refresh}>
                {t('actionRetry')}
              </Button>
            </Notice>
          </div>
        )}
        {activity.loading && (
          <div role="status" className="flex items-center gap-2 p-3">
            <Spinner size={14} />
            {t('loading')}
          </div>
        )}
        {!activity.loading && activity.jobs.length === 0 && activity.notifications.length === 0 && (
          <p className="p-4 text-sm text-secondary">{t('empty')}</p>
        )}
        {activity.queues.length > 0 && (
          <section className="border-b border-subtle p-3" aria-label={t('queues')}>
            {activity.queues.map((queue) => (
              <div key={queue.kind} className="flex items-center justify-between gap-2">
                <span className="text-xs text-secondary">
                  {activityKindLabel(lang, queue.kind)}
                  {queue.paused ? ` · ${t('pausedSuffix')}` : ''}
                </span>
                <Button
                  size="sm"
                  variant="ghost"
                  data-testid={`activity-queue-${queue.kind}`}
                  loading={activity.busyKinds.has(queue.kind)}
                  onClick={() =>
                    void (queue.paused
                      ? activity.resumeQueue(queue.kind)
                      : activity.pauseQueue(queue.kind))
                  }
                >
                  {t(queue.paused ? 'actionResume' : 'actionPause')}
                </Button>
              </div>
            ))}
          </section>
        )}
        {activity.jobs.length > 0 && (
          <section data-testid="activity-live" className="p-3">
            <h3 className="mb-2 text-xs text-secondary">{t('sectionLive')}</h3>
            {activity.jobs.map((job) => (
              <div
                key={job.id}
                data-testid={`activity-item-job-${job.id}`}
                className="mb-3 flex flex-col gap-1"
              >
                <Button
                  size="sm"
                  variant="ghost"
                  className="justify-start whitespace-normal text-left"
                  onClick={() =>
                    go(
                      job.postKey
                        ? `/p/${encodeURIComponent(job.postKey)}`
                        : activityKindTarget(job.kind),
                    )
                  }
                >
                  {activityKindLabel(lang, job.kind)}
                </Button>
                <span className="text-xs text-secondary">
                  {jobStateLabel(lang, job.state)}
                  {job.stage ? ` · ${jobStageLabel(lang, job.stage)}` : ''}
                </span>
                {job.progress != null && (
                  <div className="flex items-center gap-2">
                    <progress
                      aria-label={activityKindLabel(lang, job.kind)}
                      max={1}
                      value={Math.min(1, Math.max(0, job.progress))}
                      className="h-1 flex-1 accent-[var(--accent-fill)]"
                    />
                    <span className="text-xs tabular-nums">
                      {Math.round(Math.min(1, Math.max(0, job.progress)) * 100)}%
                    </span>
                  </div>
                )}
                {job.errorCode && (
                  <span className="text-xs text-amber-400">
                    {jobErrorLabel(lang, job.errorCode)}
                  </span>
                )}
                <div className="flex gap-2">
                  <Button
                    size="sm"
                    variant="ghost"
                    loading={activity.busyIds.has(job.id)}
                    data-testid={`activity-action-${job.state === 'failed' || job.state === 'cancelled' ? 'retry' : 'cancel'}-${job.id}`}
                    onClick={() =>
                      void (job.state === 'failed' || job.state === 'cancelled'
                        ? activity.retry(job.id)
                        : activity.cancel(job.id))
                    }
                  >
                    {t(
                      job.state === 'failed' || job.state === 'cancelled'
                        ? 'actionRetry'
                        : 'actionCancel',
                    )}
                  </Button>
                </div>
              </div>
            ))}
          </section>
        )}
        {activity.notifications.length > 0 && (
          <section data-testid="activity-recent" className="border-t border-subtle p-3">
            <h3 className="mb-2 text-xs text-secondary">{t('sectionRecent')}</h3>
            {activity.notifications.map((notification) => (
              <div
                key={notification.id}
                data-testid={`activity-log-${notification.id}`}
                data-read={notification.readAt != null}
                className="mb-3 flex flex-col gap-1"
              >
                <Button
                  size="sm"
                  variant="ghost"
                  className="justify-start whitespace-normal text-left"
                  disabled={!notificationTarget(notification.target)}
                  onClick={() => go(notification.target)}
                >
                  {notificationLabel(lang, notification)}
                </Button>
                <time
                  className="text-xs text-secondary"
                  dateTime={new Date(notification.createdAt).toISOString()}
                >
                  {new Date(notification.createdAt).toLocaleString(lang)}
                </time>
                {notification.readAt == null && (
                  <Button
                    size="sm"
                    variant="ghost"
                    data-testid={`activity-mark-read-${notification.id}`}
                    loading={activity.reading}
                    onClick={() => void activity.markRead(notification.id)}
                  >
                    {t('markRead')}
                  </Button>
                )}
              </div>
            ))}
            {activity.hasMore && (
              <Button
                size="sm"
                loading={activity.loadingMore}
                onClick={() => void activity.loadMore()}
              >
                {t('loadMore')}
              </Button>
            )}
          </section>
        )}
      </Popover>
    </div>
  );
}
