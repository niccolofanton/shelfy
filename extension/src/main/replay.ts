// Instagram REST replay: a port of IG_FEED_REPLAY (src/lib/browserScripts.ts) for
// chrome.scripting.executeScript({ world: 'MAIN', func }), kept from the SPIKE-3 build (T5), where
// it ran 123/123 items of an IG folder. It recovers the first page of a saved listing, which
// Instagram server-renders inline so that no hook can see it, and walks the rest of the feed.
// The sync controller (P2-13) runs it for every IG listing (SPIKE-3: the passive walker reads
// nothing from today's folder GraphQL): the worker injects it, the controller gates it.
//
// MV3 accepts only functions (no code strings), and Chrome serializes `func` with
// Function.prototype.toString(): the body must not reference anything outside itself, which is
// why its constants are inlined. A unit test pins them to the desktop script and to
// shared/protocol.ts.
//
// The replayed responses travel through the page's patched fetch, so the hook parses and relays
// them like any other response. Start/end scope messages make the bridge tag those batches
// `replay` (SCOPE_MESSAGE). After each page it posts SHELFY_REPLAY_PAGE with the next cursor;
// a gated replay then waits for the controller's SHELFY_REPLAY_GATE answer (P2-13):
//   continue  read the next page;
//   stop      end here (the incremental stop, the user, a time cap);
//   jump      continue from the given cursor (a resumed walk, P2-G2, after the known head).
// No answer within `gateTimeoutMs` ends the replay (`gate_timeout`): the controller is gone.

export const IG_REPLAY_MAX_PAGES = 100; // = the desktop's page cap
export const IG_REPLAY_GAP_MS = 700; // = the desktop's gap between pages

export interface ReplayOptions {
  maxPages: number;
  gapMs: number;
  /** The replay's id, in its scope, page and gate messages. */
  runId: string;
  /** Wait for the controller's answer at each page boundary (P2-13). Default false. */
  gate?: boolean;
  gateTimeoutMs?: number;
}

export interface ReplayResult {
  started: boolean;
  /** end_of_feed | page_cap | stopped | gate_timeout | http_error | network_error | invalid_json | not_a_saved_listing | unknown_listing_shape */
  reason: string;
  pages: number;
  status: number | null;
  endpoint: string | null;
  appIdSource: 'page' | 'fallback' | null;
  /** The cursor of the next page when the walk stopped before the end; null at the end. */
  cursor: string | null;
}

export async function igFeedReplay(options: ReplayOptions): Promise<ReplayResult> {
  const scopeType = 'SHELFY_SCOPE';
  const pageType = 'SHELFY_REPLAY_PAGE';
  const gateType = 'SHELFY_REPLAY_GATE';
  const maxCursorLen = 4096;
  const w = window as Window & { __syncStop?: boolean };
  const post = (phase: 'start' | 'end', detail: Record<string, string | number | null>): void => {
    window.postMessage(
      { type: scopeType, phase, source: 'replay', id: options.runId, detail },
      window.location.origin,
    );
  };
  const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

  // Posts the page boundary and, when gated, waits for the controller's answer.
  const boundary = (
    page: number,
    cursor: string,
    wait: boolean,
  ): Promise<{ action: string; cursor: string | null }> =>
    new Promise((resolve) => {
      if (!wait) {
        window.postMessage(
          { type: pageType, id: options.runId, page, cursor, wait: false },
          window.location.origin,
        );
        resolve({ action: 'continue', cursor: null });
        return;
      }
      const onMessage = (event: MessageEvent): void => {
        const data = event.data as {
          type?: unknown;
          id?: unknown;
          action?: unknown;
          cursor?: unknown;
        };
        if (event.source !== window || !data || data.type !== gateType || data.id !== options.runId)
          return;
        clearTimeout(timer);
        window.removeEventListener('message', onMessage);
        const next =
          typeof data.cursor === 'string' &&
          data.cursor.length > 0 &&
          data.cursor.length <= maxCursorLen
            ? data.cursor
            : null;
        resolve({ action: typeof data.action === 'string' ? data.action : 'stop', cursor: next });
      };
      const timer = setTimeout(() => {
        window.removeEventListener('message', onMessage);
        resolve({ action: 'timeout', cursor: null });
      }, options.gateTimeoutMs ?? 30_000);
      window.addEventListener('message', onMessage);
      window.postMessage(
        { type: pageType, id: options.runId, page, cursor, wait: true },
        window.location.origin,
      );
    });

  const segments = window.location.pathname.split('/').filter(Boolean);
  const savedAt = segments.indexOf('saved');
  const notStarted = (reason: string): ReplayResult => ({
    started: false,
    reason,
    pages: 0,
    status: null,
    endpoint: null,
    appIdSource: null,
    cursor: null,
  });
  if (savedAt < 0) return notStarted('not_a_saved_listing');
  let endpoint: string | null = null;
  const folderId = segments[savedAt + 2];
  if (folderId && /^[0-9]+$/.test(folderId))
    endpoint = '/api/v1/feed/collection/' + folderId + '/posts/';
  else if (segments[savedAt + 1] === 'all-posts') endpoint = '/api/v1/feed/saved/posts/';
  if (!endpoint) return notStarted('unknown_listing_shape');

  // The REST feed needs the web App ID header the page sends on its own requests: read it from
  // the embedded config, else use the long-stable public web value (same as the desktop).
  const html = document.documentElement.innerHTML;
  const appIdMatch =
    html.match(/"X-IG-App-ID"\s*:\s*"(\d+)"/) || html.match(/"APP_ID"\s*:\s*"(\d+)"/);
  const appId = (appIdMatch && appIdMatch[1]) || '936619743392459';

  w.__syncStop = false;
  post('start', { endpoint });
  let pages = 0;
  let status: number | null = null;
  let reason = 'end_of_feed';
  let maxId = '';
  try {
    for (let i = 0; ; i++) {
      if (i >= options.maxPages) {
        reason = 'page_cap';
        break;
      }
      if (w.__syncStop) {
        reason = 'stopped';
        break;
      }
      let response: Response;
      try {
        response = await fetch(endpoint + '?max_id=' + encodeURIComponent(maxId), {
          headers: { 'X-IG-App-ID': appId },
          credentials: 'include',
        });
      } catch {
        reason = 'network_error';
        break;
      }
      status = response.status;
      if (!response.ok) {
        reason = 'http_error';
        break;
      }
      pages++;
      const body = (await response.json().catch(() => null)) as {
        more_available?: unknown;
        next_max_id?: unknown;
      } | null;
      if (!body) {
        reason = 'invalid_json';
        break;
      }
      if (body.more_available !== true || !body.next_max_id) {
        maxId = '';
        reason = 'end_of_feed';
        break;
      }
      maxId = String(body.next_max_id);
      // The gap first: it paces the walk, and lets the hook relay this page (an async
      // postMessage from inside the patched fetch) before the boundary message follows it.
      await sleep(options.gapMs);
      const gate = await boundary(pages, maxId, !!options.gate && pages < options.maxPages);
      if (gate.action === 'stop') {
        reason = 'stopped';
        break;
      }
      if (gate.action === 'timeout') {
        reason = 'gate_timeout';
        break;
      }
      if (gate.action === 'jump' && gate.cursor) maxId = gate.cursor;
    }
  } finally {
    // The hook relays each page from inside the patched fetch, before this loop reads it, but
    // the relay is an async postMessage: wait a beat so `end` is queued after the last batch.
    await sleep(300);
    post('end', { pages, reason, status });
  }
  return {
    started: true,
    reason,
    pages,
    status,
    endpoint,
    appIdSource: appIdMatch ? 'page' : 'fallback',
    cursor: reason === 'end_of_feed' ? null : maxId || null,
  };
}

/**
 * Pinterest's server-rendered first page of a board, read by the hook (`__ssReplayPinterest`)
 * inside a scope, as main/passive.ts does at load. For chrome.scripting: self-contained.
 */
export function pinterestSsrRead(scopeId: string): boolean {
  const w = window as Window & { __ssReplayPinterest?: () => void };
  const post = (phase: 'start' | 'end'): void =>
    window.postMessage(
      { type: 'SHELFY_SCOPE', phase, source: 'ssr', id: scopeId, detail: {} },
      window.location.origin,
    );
  if (typeof w.__ssReplayPinterest !== 'function') return false;
  post('start');
  try {
    w.__ssReplayPinterest();
  } catch {
    /* best effort, like the desktop */
  } finally {
    post('end');
  }
  return true;
}

/** Sets the stop flag the replay loop checks before each page (the desktop's __syncStop). */
export function stopIgFeedReplay(): void {
  (window as Window & { __syncStop?: boolean }).__syncStop = true;
}

/** True when the MAIN-world capture hook is installed in the page (loaded at document_start). */
export function isCaptureHookInstalled(): boolean {
  return (window as Window & { __socialSavedInjected?: boolean }).__socialSavedInjected === true;
}
