// Account-scoped provider settings. The desktop keeps its own provider UI.
export const AI_TASKS = [
  'catalog',
  'qc',
  'chat',
  'suggest',
  'cluster',
  'alias',
  'embed',
  'stt',
] as const;
export type AiTask = (typeof AI_TASKS)[number];
export type ProviderState = 'ok' | 'degraded' | 'offline' | 'down' | 'invalid_key';
export interface AiProviderSummary {
  id: string;
  kind: string;
  label: string;
  managed: boolean;
  models: { text: string | null; vision: string | null; embed: string | null };
  stt: boolean;
  status: ProviderState;
}
export interface AiProviderSettings {
  aiRouting: Partial<Record<AiTask, string>>;
  aiConcurrency: number;
  aiSuggestions: boolean;
  aiVisionQc: boolean;
  aiAutoAnalyzeWebsites: boolean;
  aiDictationInterim: boolean;
}
export interface AiUsageDay {
  day: string;
  calls: number;
  inputTokens: number;
  outputTokens: number;
  cost: number | null;
}
export interface AiProvidersApi {
  list(): Promise<AiProviderSummary[]>;
  getSettings(): Promise<AiProviderSettings>;
  updateSettings(settings: AiProviderSettings): Promise<AiProviderSettings>;
  usage(days?: number): Promise<AiUsageDay[]>;
  onStatus(listener: (status: { providerId: string; state: ProviderState }) => void): () => void;
  onResync(listener: () => void): () => void;
}
