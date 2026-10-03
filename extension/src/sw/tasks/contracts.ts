import {
  boundedString,
  isPlatform,
  isRecord,
  PLATFORMS,
  type Platform,
} from '../../shared/protocol';

export const TASK_KINDS = ['upload_media', 'refresh_media', 'hydrate_link'] as const;
export interface ExtensionTask {
  id: string;
  kind: (typeof TASK_KINDS)[number];
  platform: Platform;
  postKey: string;
  nativeId: string;
  postUrl: string;
  shortcode: string | null;
  position: number | null;
  url: string | null;
  expiresAt: number | null;
  leaseId: string;
  leaseUntil: number;
}
export type TaskOutcome = 'uploaded' | 'refreshed' | 'gone' | 'failed';
export const TASK_API = {
  poll: '/api/v1/ingest/tasks?wait=25&limit=20',
  next: '/api/v1/ingest/tasks?wait=0&limit=20',
  complete: (id: string) => `/api/v1/ingest/tasks/${encodeURIComponent(id)}/complete`,
};
export function parseTasks(value: unknown): {
  tasks: ExtensionTask[];
  waiting: Record<Platform, number>;
} | null {
  if (!isRecord(value) || !Array.isArray(value.tasks) || !isRecord(value.waiting)) return null;
  const waiting = {} as Record<Platform, number>;
  for (const platform of PLATFORMS) {
    const n = value.waiting[platform];
    if (typeof n !== 'number' || !Number.isSafeInteger(n) || n < 0) return null;
    waiting[platform] = n;
  }
  const tasks: ExtensionTask[] = [];
  for (const raw of value.tasks.slice(0, 20)) {
    if (
      !isRecord(raw) ||
      !isPlatform(raw.platform) ||
      !TASK_KINDS.includes(raw.kind as ExtensionTask['kind'])
    )
      continue;
    const id = boundedString(raw.id, 256);
    const leaseId = boundedString(raw.leaseId, 256);
    const nativeId = boundedString(raw.nativeId, 128);
    const postKey = boundedString(raw.postKey, 256);
    const postUrl = boundedString(raw.postUrl, 4096);
    if (
      !id ||
      !leaseId ||
      !nativeId ||
      !postKey ||
      !postUrl ||
      typeof raw.leaseUntil !== 'number' ||
      !Number.isFinite(raw.leaseUntil)
    )
      continue;
    if (
      raw.kind !== 'upload_media' &&
      (raw.platform !== 'instagram' || !/^\d{1,32}$/.test(nativeId))
    )
      continue;
    tasks.push({
      id,
      leaseId,
      nativeId,
      postKey,
      postUrl,
      platform: raw.platform,
      kind: raw.kind as ExtensionTask['kind'],
      leaseUntil: raw.leaseUntil,
      position:
        typeof raw.position === 'number' && Number.isSafeInteger(raw.position) && raw.position >= 0
          ? raw.position
          : null,
      url: typeof raw.url === 'string' && raw.url.length <= 4096 ? raw.url : null,
      shortcode: typeof raw.shortcode === 'string' ? raw.shortcode.slice(0, 128) : null,
      expiresAt:
        typeof raw.expiresAt === 'number' && Number.isFinite(raw.expiresAt) ? raw.expiresAt : null,
    });
  }
  return { tasks, waiting };
}
export class TaskError extends Error {
  constructor(readonly code: string) {
    super(code);
  }
}
