// Instagram REST replay: a port of IG_FEED_REPLAY (src/lib/browserScripts.ts) for
// chrome.scripting.executeScript({ world: 'MAIN', func }), kept from the SPIKE-3 build (T5), where
// it ran 123/123 items of an IG folder. It recovers the first page of a saved listing, which
// Instagram server-renders inline so that no hook can see it. The sync controller (P2-13)
// productizes it; P2-06 bundles it nowhere.
//
// MV3 accepts only functions (no code strings), and Chrome serializes `func` with
// Function.prototype.toString(): the body must not reference anything outside itself, which is
// why its constants are inlined. A unit test pins them to the desktop script.
//
// The replayed responses travel through the page's patched fetch, so the hook parses and relays
// them like any other response. Start/end scope messages make the bridge tag those batches
// `replay` (SCOPE_MESSAGE in shared/protocol.ts; inlined below, a unit test pins the two).

export const IG_REPLAY_MAX_PAGES = 100; // = the desktop's page cap
export const IG_REPLAY_GAP_MS = 700; // = the desktop's gap between pages

export interface ReplayOptions {
  maxPages: number;
  gapMs: number;
  runId: string;
}

export interface ReplayResult {
  started: boolean;
  /** end_of_feed | page_cap | stopped | http_error | network_error | invalid_json | not_a_saved_listing | unknown_listing_shape */
  reason: string;
  pages: number;
  status: number | null;
  endpoint: string | null;
  appIdSource: 'page' | 'fallback' | null;
}

export async function igFeedReplay(options: ReplayOptions): Promise<ReplayResult> {
  const scopeType = 'SHELFY_SCOPE';
  const w = window as Window & { __syncStop?: boolean };
  const post = (phase: 'start' | 'end', detail: Record<string, string | number | null>): void => {
    window.postMessage(
      { type: scopeType, phase, source: 'replay', id: options.runId, detail },
      window.location.origin,
    );
  };

  const segments = window.location.pathname.split('/').filter(Boolean);
  const savedAt = segments.indexOf('saved');
  const notStarted = (reason: string): ReplayResult => ({
    started: false,
    reason,
    pages: 0,
    status: null,
    endpoint: null,
    appIdSource: null,
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
        reason = 'end_of_feed';
        break;
      }
      maxId = String(body.next_max_id);
      await new Promise((resolve) => setTimeout(resolve, options.gapMs));
    }
  } finally {
    // The hook relays each page from inside the patched fetch, before this loop reads it, but
    // the relay is an async postMessage: wait a beat so `end` is queued after the last batch.
    await new Promise((resolve) => setTimeout(resolve, 300));
    post('end', { pages, reason, status });
  }
  return {
    started: true,
    reason,
    pages,
    status,
    endpoint,
    appIdSource: appIdMatch ? 'page' : 'fallback',
  };
}

/** Sets the stop flag the replay loop checks before each page (the desktop's __syncStop). */
export function stopIgFeedReplay(): void {
  (window as Window & { __syncStop?: boolean }).__syncStop = true;
}

/** True when the MAIN-world capture hook is installed in the page (loaded at document_start). */
export function isCaptureHookInstalled(): boolean {
  return (window as Window & { __socialSavedInjected?: boolean }).__socialSavedInjected === true;
}
