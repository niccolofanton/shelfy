import { MSG, type BridgePong } from '../../shared/protocol';
import type { ExtensionTask } from './contracts';
import { TaskError } from './contracts';

export type RefreshAnswer =
  | { outcome: 'refreshed'; items: unknown[] }
  | { outcome: 'gone' }
  | { outcome: 'blocked'; code: 'rate_limited' | 'checkpoint' | 'login_required' }
  | { outcome: 'failed'; code: string };
type RefreshStatus = { outcome: 'refreshed' } | Exclude<RefreshAnswer, { outcome: 'refreshed' }>;
export interface InstagramTab {
  id: number;
  docId: string;
}
export interface TaskInstagram {
  find(excluded?: number[]): Promise<InstagramTab | null>;
  refresh(tab: InstagramTab, task: ExtensionTask, requestId: string): Promise<RefreshAnswer>;
}
// Serialized into MAIN by chrome.scripting. No imported constants or helpers.
export async function refreshInstagramPost(
  nativeId: string,
  requestId: string,
): Promise<RefreshStatus> {
  if (!/^\d{1,32}$/.test(nativeId) || location.origin !== 'https://www.instagram.com')
    return { outcome: 'failed', code: 'bad_post' };
  if (/\/(?:challenge|checkpoint)\//.test(location.pathname))
    return { outcome: 'blocked', code: 'checkpoint' };
  if (/\/accounts\/(?:login|signup)\//.test(location.pathname))
    return { outcome: 'blocked', code: 'login_required' };
  const win = window as Window & { __ssEmitInstagramRest?: (body: unknown) => { count: number } };
  if (!win.__ssEmitInstagramRest) return { outcome: 'failed', code: 'reload_tab' };
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 20_000);
  try {
    const response = await fetch(`/api/v1/media/${nativeId}/info/`, {
      credentials: 'same-origin',
      redirect: 'manual',
      signal: controller.signal,
      headers: { 'X-IG-App-ID': '936619743392459' },
    });
    if (response.status === 429) return { outcome: 'blocked', code: 'rate_limited' };
    if (response.status === 404 || response.status === 410) return { outcome: 'gone' };
    if (response.type === 'opaqueredirect' || response.status === 401)
      return { outcome: 'blocked', code: 'login_required' };
    const raw = await response.text();
    if (raw.length > 8 * 1024 * 1024) return { outcome: 'failed', code: 'response_too_large' };
    let data: Record<string, unknown>;
    try {
      data = JSON.parse(raw) as Record<string, unknown>;
    } catch {
      return { outcome: 'blocked', code: 'login_required' };
    }
    if (
      data.challenge ||
      data.checkpoint_url ||
      /challenge|checkpoint/i.test(String(data.message ?? ''))
    )
      return { outcome: 'blocked', code: 'checkpoint' };
    if (
      /login_required|login required|not logged/i.test(String(data.message ?? '')) ||
      response.status === 403
    )
      return { outcome: 'blocked', code: 'login_required' };
    if (!response.ok) return { outcome: 'failed', code: `ig_http_${response.status}` };
    window.postMessage(
      { type: 'SHELFY_SCOPE', phase: 'start', source: 'refresh', id: requestId, detail: {} },
      location.origin,
    );
    try {
      const summary = win.__ssEmitInstagramRest(data);
      return summary.count
        ? { outcome: 'refreshed' }
        : { outcome: 'failed', code: 'empty_refresh' };
    } finally {
      window.postMessage(
        { type: 'SHELFY_SCOPE', phase: 'end', source: 'refresh', id: requestId, detail: {} },
        location.origin,
      );
    }
  } catch {
    return { outcome: 'failed', code: 'ig_network' };
  } finally {
    clearTimeout(timeout);
  }
}
export function createTaskInstagram(): TaskInstagram {
  return {
    async find(excluded = []) {
      const tabs = await chrome.tabs.query({});
      for (const tab of tabs) {
        if (
          tab.id == null ||
          excluded.includes(tab.id) ||
          !tab.url?.startsWith('https://www.instagram.com/')
        )
          continue;
        const pong = await chrome.tabs
          .sendMessage<BridgePong>(tab.id, { kind: MSG.bridgePing }, { frameId: 0 })
          .catch(() => null);
        if (pong?.ok && !pong.syncing) return { id: tab.id, docId: pong.docId };
      }
      return null;
    },
    async refresh(tab, task, requestId) {
      const prepared = await chrome.tabs
        .sendMessage<{ ok: boolean; docId?: string }>(
          tab.id,
          {
            kind: MSG.taskPrepare,
            requestId,
            nativeId: task.nativeId,
          },
          { frameId: 0 },
        )
        .catch(() => null);
      if (!prepared?.ok || prepared.docId !== tab.docId)
        return { outcome: 'failed', code: 'reload_tab' };
      let answer: RefreshStatus = { outcome: 'failed', code: 'ig_network' };
      let collected: { ok: boolean; items?: unknown[]; docId?: string } | null = null;
      try {
        const results = await chrome.scripting.executeScript({
          target: { tabId: tab.id, frameIds: [0] },
          world: 'MAIN',
          func: refreshInstagramPost,
          args: [task.nativeId, requestId],
        });
        answer = (results[0]?.result as RefreshStatus) ?? answer;
      } finally {
        collected = await chrome.tabs
          .sendMessage<{ ok: boolean; items?: unknown[]; docId?: string }>(
            tab.id,
            {
              kind: MSG.taskCollect,
              requestId,
              discard: answer.outcome !== 'refreshed',
            },
            { frameId: 0 },
          )
          .catch(() => null);
      }
      if (answer.outcome !== 'refreshed') return answer;
      if (!collected?.ok || collected.docId !== tab.docId) throw new TaskError('reload_tab');
      return { outcome: 'refreshed', items: collected.items ?? [] };
    },
  };
}
