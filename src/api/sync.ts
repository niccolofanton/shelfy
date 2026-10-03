import type { ExtensionProbe } from './account';

export type SyncPlatform = 'instagram' | 'twitter' | 'pinterest';
export const SYNC_PLATFORMS: readonly SyncPlatform[] = ['instagram', 'twitter', 'pinterest'];
export interface SyncTarget {
  platform: SyncPlatform;
  collectionId?: number;
}
export interface SyncListing {
  kind: string;
  externalId: string | null;
  name: string | null;
}
export interface SyncProgress {
  runId: string;
  platform: SyncPlatform;
  listing: SyncListing;
  trigger: string;
  state: 'running' | 'done' | 'stopped' | 'failed';
  scanned: number;
  inserted: number;
  known: number;
  updated: number;
  pages: number;
}
export interface SyncRun extends Omit<SyncProgress, 'runId'> {
  id: string;
  startedAt: number;
  finishedAt: number | null;
  errorCode: string | null;
  collectionId: number | null;
}
export interface SyncBinding {
  expectedAccountId: string;
  expectedTokenId: string;
}
export interface SyncPlannerJob {
  platform: SyncPlatform;
  status: 'navigating' | 'syncing' | 'done' | 'stopped' | 'error';
  step: number;
  total: number;
  code: string | null;
  startedAt: number;
}
export interface SyncConnection {
  accountId?: string | null;
  tokenId?: string | null;
  planner?: SyncPlannerJob[];
  code?: string;
  extension: ExtensionProbe;
  syncing: Partial<Record<SyncPlatform, boolean>>;
}
export type SyncAnswer = { ok: true } | { ok: false; code: string };
export interface SyncExtension {
  connection(): Promise<SyncConnection>;
  start(target: SyncTarget, binding: SyncBinding): Promise<SyncAnswer>;
  stop(platform: SyncPlatform, binding: SyncBinding): Promise<SyncAnswer>;
}
export interface SyncApi extends SyncExtension {
  list(page?: {
    limit?: number;
    cursor?: string;
    state?: SyncProgress['state'];
    platform?: SyncPlatform;
    signal?: AbortSignal;
  }): Promise<{ items: SyncRun[]; nextCursor: string | null }>;
  onProgress(listener: (progress: SyncProgress) => void): () => void;
  onRefresh(listener: () => void): () => void;
}
export function isSyncPlatform(value: unknown): value is SyncPlatform {
  return SYNC_PLATFORMS.includes(value as SyncPlatform);
}
export function savedPage(platform: SyncPlatform): string {
  return platform === 'instagram'
    ? 'https://www.instagram.com/'
    : platform === 'twitter'
      ? 'https://x.com/i/bookmarks'
      : 'https://www.pinterest.com/';
}
