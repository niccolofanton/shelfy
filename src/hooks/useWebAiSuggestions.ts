import { useEffect, useState } from 'react';
import type { AiProvidersApi } from '../api/aiProviders';
import {
  PROVIDER_SETTINGS_CHANGED,
  type ProviderSettingsChange,
} from '../components/ai/providerConnection';

// Web preferences belong to the signed-in user. Until they load, suggestions
// stay off; a desktop localStorage opt-in cannot authorize a web provider call.
export function useWebAiSuggestions(api?: AiProvidersApi): boolean {
  const [enabled, setEnabled] = useState(false);
  useEffect(() => {
    setEnabled(false);
    if (!api) return;
    let active = true;
    let generation = 0;
    const refresh = async () => {
      const request = ++generation;
      try {
        const settings = await api.getSettings();
        if (active && request === generation) setEnabled(settings.aiSuggestions);
      } catch {
        if (active && request === generation) setEnabled(false);
      }
    };
    void refresh();
    const changed = (event: Event) => {
      const preference = (event as CustomEvent<ProviderSettingsChange>).detail?.aiSuggestions;
      if (preference !== undefined) {
        generation++;
        setEnabled(preference);
      } else void refresh();
    };
    const off = api.onResync(() => {
      void refresh();
    });
    window.addEventListener(PROVIDER_SETTINGS_CHANGED, changed);
    return () => {
      active = false;
      off();
      window.removeEventListener(PROVIDER_SETTINGS_CHANGED, changed);
    };
  }, [api]);
  return enabled;
}
