import type { AiProviderSummary, AiTask } from '../../api/aiProviders';
export const PROVIDER_CONNECTION = 'shelfy:provider-connection';
export const PROVIDER_SETTINGS_CHANGED = 'shelfy:provider-settings-changed';
export interface ProviderConnectionRequest {
  task?: AiTask;
  provider?: AiProviderSummary;
}
export interface ProviderSettingsChange {
  task?: AiTask;
  providerId?: string;
  deletedId?: string;
}
// A local UI event carries descriptors and task ids, never a credential.
export function requestProviderConnection(task?: AiTask, provider?: AiProviderSummary): void {
  window.dispatchEvent(
    new CustomEvent<ProviderConnectionRequest>(PROVIDER_CONNECTION, { detail: { task, provider } }),
  );
}
export function notifyProviderSettingsChanged(change: ProviderSettingsChange): void {
  window.dispatchEvent(
    new CustomEvent<ProviderSettingsChange>(PROVIDER_SETTINGS_CHANGED, { detail: change }),
  );
}
