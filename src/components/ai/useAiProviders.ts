import { useEffect, useState } from 'react';
import type { AiProvidersApi, AiProviderSummary, ProviderState } from '../../api/aiProviders';
import { PROVIDER_SETTINGS_CHANGED } from './providerConnection';

// Snapshots recover missed SSE events; statuses received during a fetch win
// over its older snapshot. Every listener belongs to the client's shared stream.
export function useAiProviders(api: AiProvidersApi): {
  providers: AiProviderSummary[] | null;
  error: unknown;
} {
  const [providers, setProviders] = useState<AiProviderSummary[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => {
    let active = true;
    let request = 0;
    const statuses = new Map<string, ProviderState>();
    const refresh = async (): Promise<void> => {
      const current = ++request;
      try {
        const result = await api.list();
        if (active && current === request) {
          setProviders(result.map((p) => ({ ...p, status: statuses.get(p.id) ?? p.status })));
          setError(null);
        }
      } catch (failure) {
        if (active && current === request) setError(failure);
      }
    };
    const offStatus = api.onStatus(({ providerId, state }) => {
      statuses.set(providerId, state);
      setProviders(
        (previous) =>
          previous?.map((p) => (p.id === providerId ? { ...p, status: state } : p)) ?? null,
      );
    });
    const offResync = api.onResync(() => {
      statuses.clear();
      void refresh();
    });
    const changed = (): void => {
      statuses.clear();
      void refresh();
    };
    window.addEventListener(PROVIDER_SETTINGS_CHANGED, changed);
    void refresh();
    return () => {
      active = false;
      offStatus();
      offResync();
      window.removeEventListener(PROVIDER_SETTINGS_CHANGED, changed);
    };
  }, [api]);
  return { providers, error };
}
