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
  configured?: boolean;
  last4?: string | null;
  baseUrl?: string | null;
  taskModels?: Partial<Record<AiTask, string | null>> | null;
  prices?: AiProviderPrices | null;
  consentVersion?: string;
  consent?: { version: string; acceptedAt: number } | null;
  test?: AiProviderTest | null;
}
export interface AiProviderPrices {
  inputPerMillionUsd: number;
  outputPerMillionUsd: number;
}
export interface AiProbeResult {
  ok: boolean;
  skipped: boolean;
  error?: string | null;
}
export interface AiProviderTest {
  testedAt: number;
  models: AiProbeResult;
  text: AiProbeResult;
  vision: AiProbeResult;
  schema: AiProbeResult;
}
export interface AiProviderInput {
  kind: 'openai_compatible' | 'anthropic';
  label: string;
  baseUrl: string;
  models: Partial<Record<AiTask, string>>;
  prices?: AiProviderPrices;
  key?: string;
}
export interface AiProviderManagement {
  save(id: string, input: AiProviderInput): Promise<void>;
  delete(id: string): Promise<void>;
  test(id: string): Promise<AiProviderTest>;
  consent(id: string, version: string): Promise<void>;
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
  management?: AiProviderManagement;
  list(): Promise<AiProviderSummary[]>;
  getSettings(): Promise<AiProviderSettings>;
  updateSettings(settings: AiProviderSettings): Promise<AiProviderSettings>;
  usage(days?: number): Promise<AiUsageDay[]>;
  onStatus(listener: (status: { providerId: string; state: ProviderState }) => void): () => void;
  onResync(listener: () => void): () => void;
}
