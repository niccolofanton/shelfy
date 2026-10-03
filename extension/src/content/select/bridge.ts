import { MSG, isRecord } from '../../shared/protocol';

/** Page messages carry no secrets; the worker checks the sender and platform again. */
export function selectionRelay(event: MessageEvent): void {
  if (
    event.source !== window ||
    event.origin !== location.origin ||
    !isRecord(event.data) ||
    event.data.type !== 'SOCIAL_SAVED_SELECT' ||
    !isRecord(event.data.payload)
  )
    return;
  const payload = event.data.payload;
  if (payload.type === 'check' && Array.isArray(payload.keys)) {
    const keys = payload.keys
      .filter((key): key is string => typeof key === 'string' && /^[A-Za-z0-9_-]{1,200}$/.test(key))
      .slice(0, 16_000);
    if (keys.length)
      void chrome.runtime.sendMessage({ kind: MSG.selectLookup, keys }).catch(() => undefined);
  } else if (payload.type === 'open' && typeof payload.id === 'string') {
    void chrome.runtime
      .sendMessage({ kind: MSG.selectOpen, key: payload.id })
      .catch(() => undefined);
  } else if (payload.type === 'count') {
    // Wake the panel's existing state refresh without forwarding page-controlled counts.
    void chrome.runtime.sendMessage({ kind: MSG.selectLookup, keys: [] }).catch(() => undefined);
  }
}
