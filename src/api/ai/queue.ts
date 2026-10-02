// The AI analyze queue (web port plan §2.19, App. A's `analyze:*` row): local
// VLM cataloging on the desktop today, the operator-provider `ai.drain` on the
// web once P3-13/P3-20 land. Maps to `POST /ai/analyze`, `GET /ai/queue`,
// `/jobs`, `/queues/ai.drain/*`.
//
// The desktop confirms an analyze call at once (no estimate step): P3-08
// Assumption (G3-21). The web adds the estimate + confirmation token from
// `POST /ai/analyze` (P3-13's acceptance) without changing this interface's
// shape — `analyzeAll`/`analyzeMissing` just resolve once the server has
// confirmed internally.
import type { AnalyzePostResult, ConcurrencyInfo, QueuedResult } from '../../../types/electron-api';

// A queue item's live progress (desktop: analyzer's runtime job record, see
// src/hooks/useAnalysis.tsx's AnalyzeJob; web: a `job.updated` / `ai.stream`
// payload, P3-13). Opaque here — callers narrow it, as they did with the
// bridge's `unknown` before the seam.
export type AiQueueProgress = unknown;
// The cataloging model's readiness / download progress (desktop: the local
// VLM; web: the configured provider's status, P3-09/P3-18). Opaque for the
// same reason.
export type AiModelStatus = unknown;
export type AiModelProgress = unknown;

export interface AiQueueApi {
  // `GET /ai/queue` (desktop: analyzer.getJobs()).
  getStatus(): Promise<unknown[]>;
  getIsPaused(): Promise<boolean>;
  getConcurrency(): Promise<ConcurrencyInfo>;
  // Live per-item progress while a queue consumer (AnalysisProvider, the AI
  // Tags queue view) is mounted.
  onProgress(cb: (job: AiQueueProgress) => void): () => void;

  // The cataloging model's readiness (desktop: the local VLM download/ready
  // state that gates `analyzePost`/`analyzeAll`).
  getModelStatus(): Promise<AiModelStatus>;
  onModelProgress(cb: (progress: AiModelProgress) => void): () => void;
  downloadModel(): Promise<unknown>;

  // `POST /ai/analyze` (mode: one | all | missing).
  analyzePost(postId: string): Promise<AnalyzePostResult>;
  analyzeAll(): Promise<QueuedResult>;
  analyzeMissing(): Promise<QueuedResult>;

  // `/queues/ai.drain/*` (per-item and global queue controls).
  cancelJob(key: string): Promise<unknown>;
  cancelAll(): Promise<unknown>;
  retryJob(key: string): Promise<unknown>;
  pauseAll(): Promise<unknown>;
  resumeAll(): Promise<unknown>;
  clearAll(): Promise<unknown>;
  clearCompleted(): Promise<unknown>;

  // A manual AI edit (`PATCH` through `update_ai`, P1-03) and the bulk
  // "clear AI description" action (P1-11).
  updateManual(id: string, fields: unknown): Promise<void>;
  clearDescriptions(ids: string[]): Promise<number>;
}
