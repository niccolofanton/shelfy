import React, { useEffect, useState } from 'react';
import type { AiProvidersApi } from '../../api/aiProviders';
import ConnectProviderWizard from './ConnectProviderWizard';
import { PROVIDER_CONNECTION, type ProviderConnectionRequest } from './providerConnection';
export default function ProviderConnectionHost({
  api,
}: {
  api: AiProvidersApi;
}): React.JSX.Element | null {
  const [request, setRequest] = useState<(ProviderConnectionRequest & { serial: number }) | null>(
    null,
  );
  useEffect(() => {
    if (!api.management) return;
    let serial = 0;
    const open = (event: Event): void => {
      const detail = (event as CustomEvent<ProviderConnectionRequest>).detail;
      if (detail?.provider?.managed) return;
      setRequest({ ...detail, serial: ++serial });
    };
    window.addEventListener(PROVIDER_CONNECTION, open);
    return () => window.removeEventListener(PROVIDER_CONNECTION, open);
  }, [api]);
  return request && api.management ? (
    <ConnectProviderWizard
      key={request.serial}
      api={api}
      task={request.task}
      provider={request.provider}
      onClose={() => setRequest(null)}
    />
  ) : null;
}
