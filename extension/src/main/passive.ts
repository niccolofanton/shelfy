// MAIN-world helpers that let passive capture see what the desktop sync sees, without a
// single extra request. The desktop calls the same two hook functions from its sync loop
// (useBrowserSync / SCROLL_SCRIPTS in src/lib/browserScripts.ts); here page events call them:
//
// - Pinterest server-renders the first page of a board inline, so it never crosses fetch/XHR:
//   window.__ssReplayPinterest() reads it from the page's JSON <script> blobs at DOMContentLoaded.
// - X: window.__ssScanTwitterBookmarks() reads rendered bookmark cards while the owner scrolls
//   (the desktop runs it on every scroll step).
//
// Each call is wrapped in a scope message so the bridge tags the resulting batch (ssr / dom).

import { SAVED_PATTERNS } from '../../../src/lib/browserUrls';
import { platformForUrl } from '../hosts';
import { classifyListing } from '../listing';
import { SCOPE_MESSAGE, type CaptureSource } from '../protocol';

/** Same settle time as the X scroll loop (SCROLL_SCRIPTS.twitter in browserScripts.ts). */
export const X_DOM_SCAN_INTERVAL_MS = 750;

/** The globals electron/webview-injected.ts exposes on window. */
export type HookWindow = Window & {
  __ssReplayPinterest?: () => void;
  __ssScanTwitterBookmarks?: () => void;
};

let scopeSeq = 0;

/** Runs `fn` between scope start/end messages; a batch it emits is posted in between. */
export function runScoped(win: Window, source: CaptureSource, fn: () => void): void {
  const id = `${source}-${Date.now().toString(36)}-${++scopeSeq}`;
  const post = (phase: 'start' | 'end'): void =>
    win.postMessage({ type: SCOPE_MESSAGE, phase, source, id, detail: {} }, win.location.origin);
  post('start');
  try {
    fn();
  } catch {
    /* best effort, like the desktop's executeJavaScript calls */
  } finally {
    post('end');
  }
}

export function onDomReady(doc: Document, fn: () => void): void {
  if (doc.readyState === 'loading')
    doc.addEventListener('DOMContentLoaded', () => fn(), { once: true });
  else fn();
}

/** Leading + trailing throttle: runs at most once per `intervalMs`, and once after the last call. */
export function createThrottle(
  fn: () => void,
  intervalMs: number,
  now: () => number = () => Date.now(),
  schedule: (callback: () => void, ms: number) => void = (callback, ms) =>
    void setTimeout(callback, ms),
): () => void {
  let last = -Infinity;
  let pending = false;
  return () => {
    const wait = intervalMs - (now() - last);
    if (wait <= 0) {
      last = now();
      fn();
    } else if (!pending) {
      pending = true;
      schedule(() => {
        pending = false;
        last = now();
        fn();
      }, wait);
    }
  };
}

export function installPassiveHelpers(win: HookWindow): void {
  const platform = platformForUrl(win.location.href);
  if (platform === 'pinterest') {
    onDomReady(win.document, () => {
      // Only on a hard load of a board page: after client-side navigation the inline blob
      // would still describe the first page that was loaded, not the current board.
      if (classifyListing('pinterest', win.location.href))
        runScoped(win, 'ssr', () => win.__ssReplayPinterest?.());
    });
  } else if (platform === 'twitter') {
    const scan = createThrottle(() => {
      if (SAVED_PATTERNS.twitter.test(win.location.href))
        runScoped(win, 'dom', () => win.__ssScanTwitterBookmarks?.());
    }, X_DOM_SCAN_INTERVAL_MS);
    win.addEventListener('scroll', scan, { passive: true, capture: true });
  }
}
