import { useWebSync } from '../../hooks/useWebSync';
import { SYNC_PLATFORMS, savedPage } from '../../api/sync';
import { useShelfy } from '../../api/ShelfyProvider';
import { useT, useLang } from '../../i18n';
import { Spinner } from '../ui';

const labels = { instagram: 'Instagram', twitter: 'X', pinterest: 'Pinterest' };
export function WebConnections() {
  const sync = useWebSync();
  const client = useShelfy();
  const t = useT('sidebar');
  const { lang } = useLang();
  if (!sync?.enabled) return null;
  return (
    <div data-testid="web-connections">
      {SYNC_PLATFORMS.map((platform) => {
        const last = sync.latest[platform];
        const active = sync.active[platform];
        const failedPlan = sync.planner.find(
          (job) =>
            job.platform === platform &&
            job.status === 'error' &&
            (!last || job.startedAt >= last.startedAt),
        );
        const state = failedPlan
          ? t('webSyncState_failed')
          : last
            ? t(`webSyncState_${last.state}`)
            : t('webSyncNever');
        return (
          <div key={platform} className="mx-2 rounded-md py-1.5 pl-9 pr-3">
            <div className="flex items-center gap-2 text-sm text-primary">
              <span className="flex-1">{labels[platform]}</span>
              {active && (
                <span data-testid={`connection-syncing-${platform}`}>
                  <Spinner size={13} />
                </span>
              )}
            </div>
            <p
              data-testid={`connection-state-${platform}`}
              className="mt-1 text-[11px] text-secondary"
            >
              {active ? t('webSyncState_running') : state}
              {last && !active
                ? ` · ${new Date(last.finishedAt ?? last.startedAt).toLocaleString(lang)}`
                : ''}
            </p>
            <div className="flex flex-wrap gap-2 mt-1">
              <button
                type="button"
                data-testid={`connection-sync-${platform}`}
                disabled={sync.busy.has(platform)}
                className="min-h-8 narrow:min-h-11 text-xs text-accent-text hover:underline disabled:opacity-50"
                onClick={() => void (active ? sync.stop(platform) : sync.start({ platform }))}
              >
                {t(active ? 'webSyncStop' : 'webSyncNow')}
              </button>
              <button
                type="button"
                data-testid={`connection-open-${platform}`}
                className="min-h-8 narrow:min-h-11 text-xs text-secondary hover:text-primary"
                onClick={() => void client.openExternal(savedPage(platform))}
              >
                {platform === 'twitter'
                  ? t('webSyncOpen')
                  : t('webSyncOpenPlatform', { platform: labels[platform] })}
              </button>
            </div>
          </div>
        );
      })}
      {sync.error && (
        <p role="alert" className="mx-3 text-xs text-error">
          {t('webSyncError', { code: sync.error })}
        </p>
      )}
    </div>
  );
}
