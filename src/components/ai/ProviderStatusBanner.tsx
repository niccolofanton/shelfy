import React from 'react';
import { AlertTriangle } from 'lucide-react';
import type { AiProvidersApi } from '../../api/aiProviders';
import { useNavigation } from '../../api/navigation';
import { useT } from '../../i18n';
import { useAiProviders } from './useAiProviders';
import ProviderConnectionHost from './ProviderConnectionHost';

export default function ProviderStatusBanner({
  api,
}: {
  api: AiProvidersApi;
}): React.JSX.Element | null {
  const { providers } = useAiProviders(api);
  const navigation = useNavigation();
  const t = useT('aiProviders');
  const affected = providers?.filter((p) => p.status !== 'ok') ?? [];
  return (
    <>
      <ProviderConnectionHost api={api} />
      {affected.length > 0 && (
        <div
          role="status"
          data-testid="provider-status-banner"
          className="relative z-10 flex items-center gap-2 border-b border-amber-900/50 bg-amber-950/70 px-4 py-2 text-xs text-amber-200"
        >
          <AlertTriangle size={15} className="shrink-0" />
          <span className="flex-1">
            {affected
              .map((p) =>
                t(p.managed && p.status === 'offline' ? 'operatorOffline' : `banner.${p.status}`, {
                  label: p.label,
                }),
              )
              .join(' ')}
          </span>
          <a
            href="/settings/ai"
            onClick={(event) => {
              if (navigation) {
                event.preventDefault();
                navigation.navigate({ name: 'settings', section: 'ai' });
              }
            }}
            className="underline underline-offset-2"
          >
            {t('settings')}
          </a>
        </div>
      )}
    </>
  );
}
