// The web's durable queue: explicit estimate/confirm, server counts and cursor
// pages. Local-model downloads remain on the desktop's AiQueueApi.
import type { BulkSelector } from '../ShelfyClient';
import type { ProviderState } from '../aiProviders';
export type AiQueueItemState = 'pending' | 'analyzing' | 'done' | 'error';
export interface WebAiQueueItem {
  postKey: string;
  status: string;
  attempts: number;
  nextAt: number | null;
  error: string | null;
}
export interface WebAiQueuePage {
  counts: { unanalyzed: number; pending: number; analyzing: number; done: number; error: number };
  items: WebAiQueueItem[];
  cursor: string | null;
  etaMs: number | null;
  providerState: ProviderState | null;
  paused: boolean;
}
export interface AnalyzeRequest {
  selector: BulkSelector;
  mode: 'missing' | 'selected' | 'all';
  deep?: boolean;
}
export interface AnalyzeResult {
  counts: { analyzable: number; waitingForMedia: number; alreadyQueued: number };
  estimate: {
    inputTokens: number;
    outputTokens: number;
    etaMs: number | null;
    costUsd: number | null;
  };
  queued: boolean;
  enqueued: number;
  confirmToken: string | null;
}
export interface WebAiQueueApi {
  get(
    options?: { state?: AiQueueItemState; cursor?: string },
    signal?: AbortSignal,
  ): Promise<WebAiQueuePage>;
  estimate(request: AnalyzeRequest): Promise<AnalyzeResult>;
  confirm(request: AnalyzeRequest, token: string): Promise<AnalyzeResult>;
  cancel(keys?: string[]): Promise<number>;
  retry(keys?: string[]): Promise<number>;
  pause(): Promise<void>;
  resume(): Promise<void>;
  onChanged(listener: () => void): () => void;
  // Opts into live-only ai.stream only for the lifetime of this subscription.
  onStream(listener: (frame: { postKey: string; text: string }) => void): () => void;
}
