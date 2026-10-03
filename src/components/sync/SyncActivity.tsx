import { useWebSync, isInteractiveSync } from '../../hooks/useWebSync';
import { savedPage, type SyncPlatform } from '../../api/sync';
import { useShelfy } from '../../api/ShelfyProvider';
import { useT } from '../../i18n';
import { Button, Spinner } from '../ui';

const labels = { instagram: 'Instagram', twitter: 'X', pinterest: 'Pinterest' };
export function SyncActivity() {
  const sync = useWebSync();
  const client = useShelfy();
  const t = useT('activity');
  if (!sync?.enabled) return null;
  const runs = sync.runs.filter(
    (run) =>
      isInteractiveSync(run) &&
      (run.state === 'running' ||
        (run.state === 'failed' &&
          run.errorCode === 'login_required' &&
          sync.latest[run.platform]?.id === run.id)),
  );
  const awaiting = (Object.keys(sync.active) as SyncPlatform[]).filter(
    (platform) => sync.active[platform] && !runs.some((run) => run.platform === platform),
  );
  if (!runs.length && !awaiting.length) return null;
  return (
    <section data-testid="activity-sync" className="border-b border-subtle p-3">
      <h3 className="mb-2 text-xs text-secondary">{t('syncKind')}</h3>
      {runs.map((run) => {
        const step = sync.step(run);
        return (
          <div
            key={run.id}
            data-testid={`activity-sync-${run.id}`}
            className="mb-3 flex flex-col gap-1 text-xs"
          >
            <div className="flex items-center gap-2 text-sm text-primary">
              {run.state === 'running' && <Spinner size={13} />}
              {labels[run.platform]}
              {run.listing.name ? ` · ${run.listing.name}` : ''}
            </div>
            <span className="text-secondary tabular-nums">
              {t('syncCounters', {
                scanned: run.scanned,
                inserted: run.inserted,
                known: run.known,
              })}
            </span>
            {step && (
              <span className="text-secondary">
                {t('syncStep', { step: step.index, total: step.total })}
              </span>
            )}
            {run.state === 'running' ? (
              <Button
                size="sm"
                variant="ghost"
                data-testid={`activity-sync-stop-${run.platform}`}
                loading={sync.busy.has(run.platform)}
                onClick={() => void sync.stop(run.platform)}
              >
                {t('actionStop')}
              </Button>
            ) : (
              <>
                <span className="text-warning">{t('syncLogin')}</span>
                <Button
                  size="sm"
                  variant="ghost"
                  data-testid={`activity-sync-open-${run.platform}`}
                  onClick={() => void client.openExternal(savedPage(run.platform))}
                >
                  {t('syncOpen', { platform: labels[run.platform] })}
                </Button>
              </>
            )}
          </div>
        );
      })}
      {awaiting.map((platform) => (
        <div
          key={platform}
          data-testid={`activity-sync-awaiting-${platform}`}
          className="mb-3 flex flex-col gap-2 text-xs"
        >
          <span className="flex items-center gap-2">
            <Spinner size={13} />
            {labels[platform]} · {t('syncNavigating')}
          </span>
          <Button
            size="sm"
            variant="ghost"
            loading={sync.busy.has(platform)}
            onClick={() => void sync.stop(platform)}
          >
            {t('actionStop')}
          </Button>
        </div>
      ))}
      {sync.error && (
        <p role="alert" className="text-xs text-error">
          {t('syncError', { code: sync.error })}
        </p>
      )}
    </section>
  );
}
