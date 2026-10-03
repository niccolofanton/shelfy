import type { AiProvidersApi, AiProviderSettings } from '@ui/api/aiProviders';
import type { EventStream } from './events';
import type { Http } from './http';
import type { components } from './schema';
type Schemas = components['schemas'];

function settings(value: Schemas['Settings']): AiProviderSettings {
  return {
    aiRouting: { ...value.aiRouting },
    aiConcurrency: value.aiConcurrency,
    aiSuggestions: value.aiSuggestions,
    aiVisionQc: value.aiVisionQc,
    aiAutoAnalyzeWebsites: value.aiAutoAnalyzeWebsites,
    aiDictationInterim: value.aiDictationInterim,
  };
}
export function createAiProvidersApi(http: Http, events: Pick<EventStream, 'on'>): AiProvidersApi {
  return {
    list: () => http.get<Schemas['ProviderSummary'][]>('/api/v1/me/providers'),
    management: {
      async save(id, input) {
        await http.send('PUT', `/api/v1/me/providers/${encodeURIComponent(id)}`, input);
      },
      async delete(id) {
        await http.send('DELETE', `/api/v1/me/providers/${encodeURIComponent(id)}`);
      },
      async test(id) {
        return (
          await http.send('POST', `/api/v1/me/providers/${encodeURIComponent(id)}/test`)
        ).json();
      },
      async consent(id, version) {
        await http.send('POST', `/api/v1/me/providers/${encodeURIComponent(id)}/consent`, {
          version,
        });
      },
    },
    async getSettings() {
      return settings(await http.get<Schemas['Settings']>('/api/v1/me/settings'));
    },
    async updateSettings(patch) {
      // Only the six allowlisted preferences: provider credentials never enter this request.
      const body: Schemas['SettingsUpdate'] = {
        aiRouting: { ...patch.aiRouting },
        aiConcurrency: patch.aiConcurrency,
        aiSuggestions: patch.aiSuggestions,
        aiVisionQc: patch.aiVisionQc,
        aiAutoAnalyzeWebsites: patch.aiAutoAnalyzeWebsites,
        aiDictationInterim: patch.aiDictationInterim,
      };
      return settings(await (await http.send('PUT', '/api/v1/me/settings', body)).json());
    },
    async usage(days = 30) {
      return (
        await http.get<Schemas['AiUsage']>(
          '/api/v1/me/usage/ai',
          new URLSearchParams({ days: String(days) }),
        )
      ).days;
    },
    onStatus: (listener) => events.on('provider.status', listener),
    onResync: (listener) => events.on('resync', listener),
  };
}
