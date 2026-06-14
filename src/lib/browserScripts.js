// MAIN-world scripts the Browser view drives a sync with (scroll loops + the
// Instagram REST-feed replay), plus the folder-title reader. The script strings
// are executed in the webview's page context via executeJavaScript.

// Per-platform DOM-aware scroll scripts — mirrors the extension's approach.
// Uses scrollIntoView() on specific elements to trigger virtual-scroll loading,
// and stall-detects via window.__lastInterceptAt (set by webview-preload on each intercept).
export const SCROLL_SCRIPTS = {
  instagram: `
    window.__syncStop = false;
    window.__lastInterceptAt = window.__lastInterceptAt || Date.now();
    (async () => {
      const STALL_MS = 20000;
      const MAX_STALLS = 3;
      const MAX_RUN_MS = 30 * 60 * 1000;   // hard wall-clock ceiling (30 min)
      const MAX_ITERS = 4000;              // hard iteration ceiling
      const NO_GROWTH_LIMIT = 40;          // stop after this many iters with no new captures
      const startedAt = Date.now();
      let stalls = 0, iters = 0, noGrowth = 0;
      let lastCount = (window.__ssCapturedOrder || []).length;
      while (!window.__syncStop) {
        if (++iters > MAX_ITERS || Date.now() - startedAt > MAX_RUN_MS) break;
        const loader = document.querySelector('[data-visualcompletion="loading-state"]');
        if (loader) {
          loader.scrollIntoView({ behavior: 'instant', block: 'center' });
        } else {
          const posts = document.querySelectorAll('a[href^="/p/"]');
          const last = posts[posts.length - 1];
          if (last) last.scrollIntoView({ behavior: 'instant', block: 'start' });
          else window.scrollBy(0, window.innerHeight * 0.8);
        }
        await new Promise(r => setTimeout(r, 1000));
        const count = (window.__ssCapturedOrder || []).length;
        if (count > lastCount) { lastCount = count; noGrowth = 0; }
        else if (++noGrowth >= NO_GROWTH_LIMIT) break;
        if (Date.now() - window.__lastInterceptAt > STALL_MS) {
          if (++stalls >= MAX_STALLS) break;
        } else {
          stalls = 0;
        }
      }
    })()
  `,
  twitter: `
    window.__syncStop = false;
    window.__lastInterceptAt = window.__lastInterceptAt || Date.now();
    (async () => {
      const STALL_MS = 20000;
      const MAX_STALLS = 3;
      const MAX_RUN_MS = 30 * 60 * 1000;   // hard wall-clock ceiling (30 min)
      const MAX_ITERS = 4000;              // hard iteration ceiling
      const NO_GROWTH_LIMIT = 40;          // stop after this many iters with no new captures
      const startedAt = Date.now();
      let stalls = 0, iters = 0, noGrowth = 0;
      let lastCount = (window.__ssCapturedOrder || []).length;
      while (!window.__syncStop) {
        if (++iters > MAX_ITERS || Date.now() - startedAt > MAX_RUN_MS) break;
        const articles = document.querySelectorAll('article[data-testid="tweet"]');
        if (articles.length > 0) {
          articles[articles.length - 1].scrollIntoView({ behavior: 'instant', block: 'end' });
        } else {
          window.scrollBy(0, window.innerHeight * 0.8);
        }
        await new Promise(r => setTimeout(r, 1200));
        const count = (window.__ssCapturedOrder || []).length;
        if (count > lastCount) { lastCount = count; noGrowth = 0; }
        else if (++noGrowth >= NO_GROWTH_LIMIT) break;
        if (Date.now() - window.__lastInterceptAt > STALL_MS) {
          if (++stalls >= MAX_STALLS) break;
        } else {
          stalls = 0;
        }
      }
    })()
  `,
  pinterest: `
    window.__syncStop = false;
    window.__lastInterceptAt = window.__lastInterceptAt || Date.now();
    (async () => {
      const STALL_MS = 20000;
      const MAX_STALLS = 3;
      const MAX_RUN_MS = 30 * 60 * 1000;   // hard wall-clock ceiling (30 min)
      const MAX_ITERS = 4000;              // hard iteration ceiling
      const NO_GROWTH_LIMIT = 40;          // stop after this many iters with no new captures
      const startedAt = Date.now();
      let stalls = 0, iters = 0, noGrowth = 0;
      let lastCount = (window.__ssCapturedOrder || []).length;
      while (!window.__syncStop) {
        if (++iters > MAX_ITERS || Date.now() - startedAt > MAX_RUN_MS) break;
        // Pinterest's masonry grid is virtualized; scroll the last mounted pin
        // tile into view to drive the next BoardFeedResource fetch.
        const pins = document.querySelectorAll('div[data-test-id="pin"], div[data-grid-item="true"], a[href*="/pin/"]');
        const last = pins[pins.length - 1];
        if (last) last.scrollIntoView({ behavior: 'instant', block: 'end' });
        else window.scrollBy(0, window.innerHeight * 0.8);
        await new Promise(r => setTimeout(r, 1000));
        const count = (window.__ssCapturedOrder || []).length;
        if (count > lastCount) { lastCount = count; noGrowth = 0; }
        else if (++noGrowth >= NO_GROWTH_LIMIT) break;
        if (Date.now() - window.__lastInterceptAt > STALL_MS) {
          if (++stalls >= MAX_STALLS) break;
        } else {
          stalls = 0;
        }
      }
    })()
  `,
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
export async function findIgFolderHref(wv, externalId, { tries = 12, delayMs = 500 } = {}) {
  const id = String(externalId || '');
  if (!/^\d+$/.test(id)) return null;
  for (let i = 0; i < tries; i++) {
    let href = null;
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
    if (href) return href;
    await new Promise((r) => setTimeout(r, delayMs));
  }
  return null;
}

// Best-effort read of the folder's real title from the page header; falls back to
// the de-slugified URL name when the heading isn't found.
export async function readIgFolderName(wv, fallback) {
  try {
    const name = await wv.executeJavaScript(
      `(() => { const h = document.querySelector('main h1') || document.querySelector('h1'); return h && h.textContent ? h.textContent.trim() : ''; })()`,
    );
    return name && name.trim() ? name.trim() : fallback;
  } catch {
    return fallback;
  }
}
