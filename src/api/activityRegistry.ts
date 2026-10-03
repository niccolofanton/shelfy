// Additive registry for P2-18 sync / P3 AI / P4 capture, video, import and
// export: registerActivityKind(kind, { labelKey, target? }) alongside the
// feature's initialization. Keys use the existing Jobs i18n namespace; an
// unknown/newer-server kind always falls back to jobKindLabel. Targets are
// optional internal app routes, never external URLs or desktop-only views.
import type { AppRoute } from './navigation';
import type { ActivityNotification } from './activity';
import { translate } from '../i18n';
import { jobKindLabel } from '../views/jobs/labels';

export interface ActivityKindDescriptor {
  labelKey: string;
  target?: string;
}
const registry = new Map<string, ActivityKindDescriptor>();
export function registerActivityKind(kind: string, descriptor: ActivityKindDescriptor): void {
  registry.set(kind, Object.freeze({ ...descriptor }));
}
for (const kind of [
  'migrate',
  'usage.recompute',
  'bulk',
  'purge',
  'archive.drain',
  'capture.site',
  'media.video',
  'import',
  'export',
  'link.hydrate',
  'ai.drain',
  'ai.run',
  'gc',
]) {
  registerActivityKind(kind, { labelKey: `jobs.kind.${kind}`, target: '/jobs' });
}
registerActivityKind('sync', { labelKey: 'activity.syncKind', target: '/settings/connections' });
export function activityKindLabel(lang: string, kind: string): string {
  const descriptor = registry.get(kind);
  if (descriptor) {
    const label = translate(lang, descriptor.labelKey);
    if (label !== descriptor.labelKey) return label;
  }
  return jobKindLabel(lang, kind);
}
export function activityKindTarget(kind: string): string {
  return registry.get(kind)?.target ?? '/jobs';
}
export function notificationLabel(lang: string, notification: ActivityNotification): string {
  const key = `activity.notif.${notification.code}`;
  const label = translate(lang, key, notification.params);
  return label === key
    ? translate(lang, 'activity.notificationGeneric', {
        kind: activityKindLabel(lang, notification.kind),
      })
    : label;
}
// The notification target is untrusted server data. Accept known internal
// destinations only; a post key is mapped to the existing post route.
export function notificationTarget(target: string | null): AppRoute | null {
  if (!target || /[\\\u0000-\u0020]/.test(target)) return null;
  if (/^[\w.-]+$/.test(target)) return { name: 'post', key: target };
  if (!target.startsWith('/') || target.startsWith('//')) return null;
  const [path, query = ''] = target.split('?');
  if (path === '/') return { name: 'library' };
  if (path === '/jobs') {
    const params = new URLSearchParams(query);
    return {
      name: 'jobs',
      kind: params.getAll('kind').slice(0, 20),
      state: params.getAll('state').slice(0, 20),
    };
  }
  if (path === '/trash') return { name: 'trash' };
  if (/^\/settings(?:\/[\w-]+)?$/.test(path))
    return { name: 'settings', section: path.split('/')[2] || 'account' };
  if (/^\/c\/[1-9]\d{0,15}$/.test(path))
    return { name: 'collection', collectionId: Number(path.slice(3)) };
  if (/^\/p\/[^/]+$/.test(path)) {
    try {
      return { name: 'post', key: decodeURIComponent(path.slice(3)) };
    } catch {
      return null;
    }
  }
  return null;
}
