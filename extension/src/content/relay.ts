// Window-message relay used by bridge.ts (ISOLATED world). Desktop counterpart:
// electron/webview-preload.ts, which accepts the hook's postMessage fallback only when
// `event.source === window` and forwards it with ipcRenderer.sendToHost. Here the destination is
// the service worker over chrome.runtime. Each hook message travels whole, captions and authors
// included (they are library data, P2-G15), with the context only the content script knows:
// the page URL, the document id and sequence number, the capture scope and the Pinterest viewer.

import {
  MSG,
  parseCensusMessage,
  parseInterceptMessage,
  parseReplayPageMessage,
  parseScopeMessage,
  toCaptureMessage,
  type CaptureSource,
  type InterceptMessage,
  type Platform,
  type ReplayPageMessage,
  type ScopeMessage,
} from '../shared/protocol';
import { ScopeTracker } from './scoping';

/** What the sync controller (content/sync/controller.ts, P2-13) sees of the page's messages. */
export interface RelayObserver {
  intercept(message: InterceptMessage, capture: CaptureSource): void;
  scope(message: ScopeMessage): void;
  replayPage(message: ReplayPageMessage): void;
}

export type RelayListener = ((event: MessageEvent) => void) & {
  /** Resolves once every message forwarded so far was delivered (or given up). */
  idle(): Promise<void>;
};

export interface RelayDeps {
  /** Delivers a message to the service worker; rejects when it cannot. */
  send(message: unknown): Promise<unknown>;
  pageUrl(): string;
  now(): number;
  warn(message: string): void;
  /** Random id of this document (bridge.ts makes one per page load). */
  docId: string;
  /** The signed-in user of the page, for Pinterest; null when unknown or not read. */
  viewer(platform: Platform): string | null;
  schedule?(callback: () => void, ms: number): void;
  maxAttempts?: number;
  retryDelayMs?: number;
  observer?: RelayObserver;
  /** A worker-authorized task may collect a refresh instead of passive ingest. */
  consume?(message: InterceptMessage, capture: CaptureSource): boolean;
}

/**
 * Builds the `message` event listener. One-shot runtime messages (rather than a long-lived
 * port) are used on purpose: each delivery is acknowledged once the worker has queued it, wakes
 * a suspended worker, and is retried when the worker was being torn down at the time. A retried
 * delivery keeps its sequence number, so the worker can drop a copy it already queued.
 */
export function createRelay(win: Window, deps: RelayDeps): RelayListener {
  const scopes = new ScopeTracker();
  const schedule = deps.schedule ?? ((callback, ms) => void setTimeout(callback, ms));
  const maxAttempts = deps.maxAttempts ?? 3;
  const retryDelayMs = deps.retryDelayMs ?? 400;
  const inFlight = new Set<Promise<void>>();
  let disabled = false;
  let seq = 0;

  const deliver = async (message: unknown): Promise<void> => {
    for (let attempt = 1; ; attempt++) {
      try {
        await deps.send(message);
        return;
      } catch (err) {
        const text = err instanceof Error ? err.message : String(err);
        if (/context invalidated/i.test(text)) {
          // The extension was reloaded or removed: this content script is orphaned.
          if (!disabled) deps.warn('extension reloaded: refresh this tab to resume capture');
          disabled = true;
          return;
        }
        if (attempt >= maxAttempts) {
          deps.warn(`dropped a message after ${attempt} attempts: ${text}`);
          return;
        }
        await new Promise<void>((resolve) => schedule(resolve, retryDelayMs * attempt));
      }
    }
  };

  const forward = (message: unknown): void => {
    const delivery = deliver(message);
    inFlight.add(delivery);
    void delivery.finally(() => inFlight.delete(delivery));
  };

  const listener = (event: MessageEvent): void => {
    if (disabled || event.source !== win) return;
    const data: unknown = event.data;

    const scope = parseScopeMessage(data);
    if (scope) {
      scopes.apply(scope);
      deps.observer?.scope(scope);
      return;
    }

    const page = parseReplayPageMessage(data);
    if (page) {
      deps.observer?.replayPage(page);
      return;
    }

    const census = parseCensusMessage(data);
    if (census) {
      forward({ kind: MSG.census, counts: census, pageUrl: deps.pageUrl(), sentAt: deps.now() });
      return;
    }

    const intercept = parseInterceptMessage(data);
    if (intercept) {
      const capture = scopes.current();
      if (deps.consume?.(intercept, capture)) return;
      forward(
        toCaptureMessage(intercept, {
          pageUrl: deps.pageUrl(),
          docId: deps.docId,
          seq: seq++,
          capture,
          viewer: deps.viewer(intercept.platform),
          sentAt: deps.now(),
        }),
      );
      deps.observer?.intercept(intercept, capture);
    }
  };

  return Object.assign(listener, {
    idle: async (): Promise<void> => {
      while (inFlight.size) await Promise.all([...inFlight]);
    },
  });
}
