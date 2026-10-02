// Settings → Storage on a server (plan §2.13 Quota, §2.19 Settings; P1-20):
// the space the account uses (media and database, against its quota), and
// which assets of a post the server archives. Kept videos join in P4.
//
// The use is the last count of the server's `usage.recompute` job: nightly,
// after an install or a purge, and on the first visit. A count that ends
// while the section is open reloads it (`job.updated`).
import React, { useCallback, useEffect, useState } from 'react';
import { Database, Film, HardDrive, Image, Layers } from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import type { AccountApi, ArchiveAssetTypes, StorageUsage } from '../../api/account';
import { useFailureText } from '../../hooks/useFailureText';
import { useLang, useT } from '../../i18n';
import { formatBytes, formatDateTime } from './format';
import { Card, CardHeader, InlineNote, Loading } from './ui';

function UsageCard({ account }: { account: AccountApi }): React.JSX.Element {
  const t = useT('settings');
  const { lang } = useLang();
  const failure = useFailureText();
  const [usage, setUsage] = useState<StorageUsage | null>(null);
  const [error, setError] = useState<unknown>(null);

  const load = useCallback(async () => {
    try {
      setUsage(await account.getUsage());
      setError(null);
    } catch (err) {
      setError(err);
    }
  }, [account]);

  useEffect(() => {
    void load();
    return account.onUsageChanged(() => void load());
  }, [account, load]);

  const unlimited = usage ? usage.quotaBytes <= 0 : true;
  // The bar spans the quota, or the use itself without one.
  const span = usage ? (unlimited ? usage.usedBytes : usage.quotaBytes) : 0;
  const share = (bytes: number): string =>
    span > 0 ? `${Math.min(100, (bytes / span) * 100).toFixed(2)}%` : '0%';

  return (
    <Card testId="storage-usage">
      <CardHeader icon={HardDrive} title={t('storageTitle')} />
      <div className="mt-4">
        {!usage && error == null && <Loading />}
        {error != null && <InlineNote tone="error">{failure(error)}</InlineNote>}
        {usage && (
          <>
            <p className="flex flex-wrap items-baseline gap-x-2 text-sm text-gray-100">
              <span data-testid="storage-used" className="text-lg font-semibold tabular-nums">
                {unlimited
                  ? t('storageUsed', { used: formatBytes(usage.usedBytes, lang) })
                  : t('storageOfQuota', {
                      used: formatBytes(usage.usedBytes, lang),
                      quota: formatBytes(usage.quotaBytes, lang),
                    })}
              </span>
              {unlimited && <span className="text-xs text-gray-500">{t('storageNoLimit')}</span>}
            </p>
            <div
              className="mt-3 flex h-2 w-full overflow-hidden rounded-full bg-[#242424]"
              role="img"
              aria-label={t('storageBarLabel')}
            >
              <div className="h-full bg-[#7B5CFF]" style={{ width: share(usage.mediaBytes) }} />
              <div className="h-full bg-[#34d399]" style={{ width: share(usage.dbBytes) }} />
            </div>
            <ul className="mt-3 flex flex-wrap gap-x-5 gap-y-1 text-xs text-gray-400">
              <li className="flex items-center gap-1.5">
                <span className="h-2 w-2 rounded-full bg-[#7B5CFF]" />
                {t('storageMedia')}
                <span className="tabular-nums text-gray-300" data-testid="storage-media">
                  {formatBytes(usage.mediaBytes, lang)}
                </span>
              </li>
              <li className="flex items-center gap-1.5">
                <span className="h-2 w-2 rounded-full bg-[#34d399]" />
                {t('storageDatabase')}
                <span className="tabular-nums text-gray-300" data-testid="storage-db">
                  {formatBytes(usage.dbBytes, lang)}
                </span>
              </li>
            </ul>
            <p className="mt-3 text-[11px] text-gray-600" data-testid="storage-counted">
              {usage.updatedAt
                ? t('storageCounted', { date: formatDateTime(usage.updatedAt, lang) })
                : t('storageCounting')}
            </p>
          </>
        )}
      </div>
    </Card>
  );
}

interface AssetType {
  key: keyof ArchiveAssetTypes;
  labelKey: string;
  descKey: string;
  Icon: LucideIcon;
  color: string;
}

// Same order and colors as the desktop's download types.
const ASSET_TYPES: AssetType[] = [
  {
    key: 'thumbnail',
    labelKey: 'archiveThumbnail',
    descKey: 'archiveThumbnailDesc',
    Icon: Layers,
    color: '#a78bfa',
  },
  {
    key: 'image',
    labelKey: 'archiveImage',
    descKey: 'archiveImageDesc',
    Icon: Image,
    color: '#34d399',
  },
  {
    key: 'video',
    labelKey: 'archiveVideo',
    descKey: 'archiveVideoDesc',
    Icon: Film,
    color: '#60a5fa',
  },
];

function ArchiveCard({ account }: { account: AccountApi }): React.JSX.Element {
  const t = useT('settings');
  const failure = useFailureText();
  const [types, setTypes] = useState<ArchiveAssetTypes | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    account.getSettings().then(
      (settings) => alive && setTypes(settings.archiveAssetTypes),
      (err: unknown) => alive && setError(failure(err)),
    );
    return () => {
      alive = false;
    };
  }, [account, failure]);

  const toggle = async (key: keyof ArchiveAssetTypes, value: boolean): Promise<void> => {
    if (!types) return;
    const previous = types;
    const next = { ...types, [key]: value };
    setTypes(next);
    setError(null);
    try {
      const saved = await account.updateSettings({ archiveAssetTypes: next });
      setTypes(saved.archiveAssetTypes);
    } catch (err) {
      setTypes(previous);
      setError(failure(err));
    }
  };

  return (
    <Card testId="storage-archive">
      <CardHeader icon={Database} title={t('archiveTitle')} description={t('archiveDesc')} />
      <div className="mt-3 divide-y divide-[#2a2a2a]">
        {!types && !error && <Loading />}
        {types &&
          ASSET_TYPES.map(({ key, labelKey, descKey, Icon, color }) => (
            <label
              key={key}
              className="u-press flex items-center gap-3 py-2.5 px-1 -mx-1 rounded-md cursor-pointer select-none hover:bg-[#1c1c1c]"
            >
              <Icon size={16} className="shrink-0" style={{ color }} />
              <div className="flex-1 min-w-0">
                <p className="text-white text-sm font-medium">{t(labelKey)}</p>
                <p className="text-gray-500 text-xs">{t(descKey)}</p>
              </div>
              <input
                type="checkbox"
                data-testid={`archive-${key}`}
                aria-label={t(labelKey)}
                checked={types[key]}
                onChange={(e) => void toggle(key, e.target.checked)}
                className="accent-[var(--accent)] w-4 h-4 shrink-0 cursor-pointer"
              />
            </label>
          ))}
      </div>
      {error && (
        <InlineNote tone="error" testId="archive-error">
          {error}
        </InlineNote>
      )}
    </Card>
  );
}

export default function StorageSection({ account }: { account: AccountApi }): React.JSX.Element {
  return (
    <div
      className="grid grid-cols-1 lg:grid-cols-2 gap-4 items-start"
      data-testid="settings-storage"
    >
      <UsageCard account={account} />
      <ArchiveCard account={account} />
    </div>
  );
}
