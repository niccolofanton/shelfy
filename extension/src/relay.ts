// Window-message relay used by bridge.ts (ISOLATED world). Desktop counterpart:
// electron/webview-preload.ts, which accepts the hook's postMessage fallback only when
// `event.source === window` and forwards it with ipcRenderer.sendToHost. Here the destination is
// the service worker over chrome.runtime, and each batch is tagged with its capture source.

import {
  RUNTIME,
  ScopeTracker,
  parseCensusMessage,
  parseInterceptMessage,
  parseScopeMessage,
  toBatchMessage,
  toMarkerMessage,
} from './protocol';

export interface RelayDeps {
  /** Delivers a message to the service worker; rejects when it cannot. */
  send(message: unknown): Promise<unknown>;
  pageUrl(): string;
  now(): number;
  warn(message: string): void;
  schedule?(callback: () => void, ms: number): void;
  maxAttempts?: number;
  retryDelayMs?: number;
}

/**
 * Builds the `message` event listener. One-shot runtime messages (rather than a long-lived
 * port) are used on purpose: each delivery is acknowledged, wakes a suspended service worker,
 * and is retried when the worker was being torn down at the time.
 */
export function createRelay(win: Window, deps: RelayDeps): (event: MessageEvent) => void {
  const scopes = new ScopeTracker();
  const schedule = deps.schedule ?? ((callback, ms) => void setTimeout(callback, ms));
  const maxAttempts = deps.maxAttempts ?? 3;
  const retryDelayMs = deps.retryDelayMs ?? 400;
  let disabled = false;

  const forward = (message: unknown, attempt = 1): void => {
    deps.send(message).catch((err: unknown) => {
      const text = err instanceof Error ? err.message : String(err);
      if (/context invalidated/i.test(text)) {
        // The extension was reloaded or removed: this content script is orphaned.
        if (!disabled) deps.warn('extension reloaded: refresh this tab to resume capture');
        disabled = true;
        return;
      }
      if (attempt < maxAttempts)
        schedule(() => forward(message, attempt + 1), retryDelayMs * attempt);
      else deps.warn(`dropped a message after ${attempt} attempts: ${text}`);
    });
  };

  return (event: MessageEvent): void => {
    if (disabled || event.source !== win) return;
    const data: unknown = event.data;

    const scope = parseScopeMessage(data);
    if (scope) {
      scopes.apply(scope);
      // Replay start/end go to the log; SSR reads and DOM scans are too chatty to log.
      if (scope.source === 'replay') forward(toMarkerMessage(scope, deps.pageUrl(), deps.now()));
      return;
    }

    const census = parseCensusMessage(data);
    if (census) {
      forward({
        kind: RUNTIME.census,
        counts: census,
        pageUrl: deps.pageUrl(),
        sentAt: deps.now(),
      });
      return;
    }

    const intercept = parseInterceptMessage(data);
    if (intercept)
      forward(
        toBatchMessage(intercept, {
          pageUrl: deps.pageUrl(),
          source: scopes.current(),
          sentAt: deps.now(),
        }),
      );
  };
}
