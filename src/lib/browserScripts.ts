// MAIN-world scripts the Browser view drives a sync with (scroll loops + the
// Instagram REST-feed replay), plus the folder-title reader. The script strings
// are executed in the webview's page context via executeJavaScript.

// The minimal Electron <webview> surface the folder helpers touch: just the
// page-context JS runner. Both the Browser view's ElectronWebview and the sync
// hook's SyncWebview are structurally assignable to this.
export interface ScriptWebview {
  executeJavaScript(code: string): Promise<unknown>;
}

// Per-platform GRADUAL scroll scripts. A jump-to-last-element scroll is fast at
// driving the virtual loader for DATA (JSON), but it skips past the intermediate
// posts so their <img> never enter the viewport and never load — leaving the
// on-view capture nothing to grab. These instead step DOWN ~0.55 viewport at a
// time and pause, so every lazy-loaded image passes through the viewport and is
// fetched by the browser (the capture then reads it from the network buffer — no
// extra download). When a step can't advance (bottom of mounted content) we pull
// the last tile into view to nudge the virtual loader, then wait a touch longer.
//
// TWO PASSES: the 1st loads ~all images top→bottom; the 2nd jumps back to the top
// and repeats, catching the few the 1st skipped — IG virtualises aggressively (some
// posts mount/unmount before their <img> finishes), and the very first screen can
// load before the CDP capture is armed. Crucially those misses NEVER loaded, so
// they're NOT cached: on the 2nd pass they load for real and get captured. Images
// already grabbed ARE cached → no new request → harmlessly not re-captured.
//
// A pass ends at the real bottom (no new captures AND no scroll progress); we never
// stall while still advancing. Stall-detects via window.__lastInterceptAt.
function gradualScroll(lastSelector: string, settleMs: number): string {
  return `
    window.__syncStop = false;
    window.__lastInterceptAt = window.__lastInterceptAt || Date.now();
    (async () => {
      const SETTLE_MS = ${settleMs};
      const STALL_MS = 20000;
      const MAX_STALLS = 3;
      const MAX_RUN_MS = 30 * 60 * 1000;   // hard wall-clock ceiling (30 min)
      const MAX_ITERS = 16000;             // shared across passes
      const NO_GROWTH_LIMIT = 60;          // a pass ends after this many stuck steps
      const PASSES = 2;
      const sel = ${JSON.stringify(lastSelector)};
      const sleep = (ms) => new Promise(r => setTimeout(r, ms));
      const startedAt = Date.now();
      let iters = 0, done = false;
      for (let pass = 0; pass < PASSES && !window.__syncStop && !done; pass++) {
        if (pass > 0) { window.scrollTo(0, 0); await sleep(900); } // 2nd pass: re-scan from top
        let stalls = 0, noGrowth = 0;
        let lastCount = (window.__ssCapturedOrder || []).length;
        while (!window.__syncStop) {
          if (++iters > MAX_ITERS || Date.now() - startedAt > MAX_RUN_MS) { done = true; break; }
          const beforeY = window.scrollY;
          window.scrollBy(0, Math.floor(window.innerHeight * 0.55));
          let extra = 0;
          if (window.scrollY <= beforeY + 2) {
            const items = document.querySelectorAll(sel);
            const last = items[items.length - 1];
            if (last) last.scrollIntoView({ behavior: 'instant', block: 'end' });
            extra = 500;
          }
          await sleep(SETTLE_MS + extra);
          const count = (window.__ssCapturedOrder || []).length;
          const advanced = window.scrollY > beforeY + 2;
          const grew = count > lastCount || advanced;
          if (count > lastCount) lastCount = count;
          if (grew) noGrowth = 0;
          else if (++noGrowth >= NO_GROWTH_LIMIT) break; // bottom of this pass
          // Stall only when stuck AND no fresh data intercepts — never while still
          // advancing (the data replay finishes long before the image scroll does).
          if (!advanced && Date.now() - window.__lastInterceptAt > STALL_MS) {
            if (++stalls >= MAX_STALLS) break;
          } else {
            stalls = 0;
          }
        }
      }
    })()
  `;
}

export const SCROLL_SCRIPTS: Record<'instagram' | 'twitter' | 'pinterest', string> = {
  instagram: gradualScroll('a[href^="/p/"]', 650),
  twitter: gradualScroll('article[data-testid="tweet"]', 750),
  pinterest: gradualScroll(
    'div[data-test-id="pin"], div[data-grid-item="true"], a[href*="/pin/"]',
    650,
  ),
};

// Instagram server-renders the FIRST page of a saved listing INLINE in the page
// HTML (no fetch/XHR), so the passive fetch/XHR interceptor never sees it — only
// the scroll-triggered pages 2..N get captured, and the most-recently-saved posts
// sitting at the top of the list are silently dropped (e.g. a 65-item folder
// imports as 53). At sync start we replay the listing's REST feed from the top
// THROUGH the already-installed fetch hook, so the existing capture pipeline
// ingests page 1 too — and every page deterministically, independent of how far
// the fragile scroll loop manages to drive IG's virtual loader. Overlap with the
// scroll path is harmless: posts dedupe on upsert. The final page reports
// more_available=false, which the host's intercept handler turns into finishSync.
export const IG_FEED_REPLAY = `
  (async () => {
    try {
      const seg = location.pathname.split('/').filter(Boolean);
      const si = seg.indexOf('saved');
      if (si < 0) return;
      let base = null;
      const id = seg[si + 2];
      if (id && /^[0-9]+$/.test(id)) base = '/api/v1/feed/collection/' + id + '/posts/';
      else if (seg[si + 1] === 'all-posts') base = '/api/v1/feed/saved/posts/';
      if (!base) return; // unknown listing shape — leave it to the scroll loop
      // The REST feed requires the web App ID header (the page sends it on its own
      // requests). Read it from the embedded config, fall back to the long-stable
      // public web value.
      const html = document.documentElement.innerHTML;
      const m = html.match(/"X-IG-App-ID"\\s*:\\s*"(\\d+)"/) || html.match(/"APP_ID"\\s*:\\s*"(\\d+)"/);
      const appId = (m && m[1]) || '936619743392459';
      let maxId = '';
      for (let i = 0; i < 100 && !window.__syncStop; i++) {
        let r;
        try {
          r = await fetch(base + '?max_id=' + encodeURIComponent(maxId), {
            headers: { 'X-IG-App-ID': appId },
            credentials: 'include',
          });
        } catch (_) { break; }
        if (!r.ok) break;
        // The fetch hook clones+relays this response on its own; we only read the
        // body here to follow the cursor.
        const j = await r.json().catch(() => null);
        if (!j || j.more_available !== true || !j.next_max_id) break;
        maxId = j.next_max_id;
        await new Promise((res) => setTimeout(res, 700));
      }
    } catch (_) {}
  })()
`;

// Reads the logged-in Instagram username via IG's own REST endpoint, executed in
// the page origin (cookies ride along). The background source-sync needs it to
// build /<username>/saved/… URLs when no saved-page URL was ever persisted.
// Returns '' when logged out or on any failure — callers treat '' as "login
// required". App-ID extraction mirrors IG_FEED_REPLAY above.
export const IG_GET_USERNAME = `
  (async () => {
    try {
      const html = document.documentElement.innerHTML;
      const m = html.match(/"X-IG-App-ID"\\s*:\\s*"(\\d+)"/) || html.match(/"APP_ID"\\s*:\\s*"(\\d+)"/);
      const appId = (m && m[1]) || '936619743392459';
      const r = await fetch('/api/v1/accounts/current_user/', {
        headers: { 'X-IG-App-ID': appId },
        credentials: 'include',
      });
      if (!r.ok) return '';
      const j = await r.json().catch(() => null);
      return (j && j.user && j.user.username) || '';
    } catch (_) {
      return '';
    }
  })()
`;

// Finds the href of a saved folder on the saved INDEX page (/<user>/saved/) by
// its numeric folder id — collections only persist the rename-safe id, never the
// URL slug, so the real link must be discovered from the DOM. Scrolls between
// attempts because the folder grid is lazy-rendered. Returns null when the
// folder isn't there (deleted on IG) or the id is malformed.
export async function findIgFolderHref(
  wv: ScriptWebview,
  externalId: string | number | null | undefined,
  { tries = 12, delayMs = 500 }: { tries?: number; delayMs?: number } = {},
): Promise<string | null> {
  const id = String(externalId || '');
  if (!/^\d+$/.test(id)) return null;
  for (let i = 0; i < tries; i++) {
    let href: unknown = null;
    try {
      href = await wv.executeJavaScript(
        `(() => {
          const a = document.querySelector('a[href*="/saved/"][href$="/${id}/"]');
          if (a) return a.getAttribute('href');
          window.scrollBy(0, window.innerHeight);
          return null;
        })()`,
      );
    } catch {
      return null;
    }
    if (typeof href === 'string' && href) return href;
    await new Promise((r) => setTimeout(r, delayMs));
  }
  return null;
}

// Best-effort read of the folder's real title from the page header; falls back to
// the de-slugified URL name when the heading isn't found.
export async function readIgFolderName(wv: ScriptWebview, fallback: string): Promise<string> {
  try {
    const name = await wv.executeJavaScript(
      `(() => { const h = document.querySelector('main h1') || document.querySelector('h1'); return h && h.textContent ? h.textContent.trim() : ''; })()`,
    );
    return typeof name === 'string' && name.trim() ? name.trim() : fallback;
  } catch {
    return fallback;
  }
}
