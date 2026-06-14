import { useEffect } from 'react';
import { TABS } from '../lib/browserUrls';
import { sanitizeInterceptedBatch } from '../lib/browserSanitize';

// Webview → host message handling (the cross-world trust boundary) plus the
// MAIN-world script injection. Attached ONCE on mount; every handler reads only
// live refs and stable setters, and the captured `ingestBatch`/`finishSync`
// (first-render instances, like the original inline effect) do the same, so
// there is no stale-closure risk.
export default function useBrowserIntercept({
  webviewRefs,
  injectedScriptRef,
  selectScriptRef,
  selectModeRef,
  injectedForLoadRefs,
  syncingRef,
  pendingRefs,
  syncCollectionRefs,
  ingestBatch,
  finishSync,
  setSelectedCount,
  setActivePost,
}) {
  useEffect(() => {
    const handleIpcMessage = (tabId) => (e) => {
      if (e.channel === 'intercepted') {
        const { items: rawItems, platform, hasNextPage } = e.args[0] || {};
        // Validate the cross-world payload at the boundary: a malformed message
        // must not crash the handler (items.length / items.map / save call).
        if (!Array.isArray(rawItems)) return;
        if (!['instagram', 'twitter', 'pinterest'].includes(platform)) return;
        // The MAIN world is page-controlled, so clamp/validate every item before
        // it can reach the DB (bounded batch size, sane id, capped fields, http(s)
        // media URLs only) and stamp the validated batch platform onto each item
        // (per-item `platform` from the page is never trusted). See
        // sanitizeInterceptedBatch.
        const items = sanitizeInterceptedBatch(rawItems, platform);
        if (!syncingRef.current[tabId]) {
          // Not syncing yet: stash this batch so the first chunk (which loads as
          // soon as the saved page opens) survives until Auto-import flushes it.
          if (items.length) {
            const buf = pendingRefs[tabId]?.current;
            if (buf) {
              for (const it of items) {
                if (buf.items.length >= 3000) break; // bound memory; keep earliest
                buf.items.push(it);
              }
            }
          }
          return;
        }
        // An end-of-feed signal (hasNextPage===false) can arrive with zero items;
        // only short-circuit on an empty batch when it carries no items AND isn't
        // the terminal page, so finishSync below still fires.
        // Snapshot the folder-import target NOW, before the async save resolves.
        // finishSync (fired below on the last batch) nulls this ref synchronously,
        // so re-reading it inside the .then would miss filing the final batch.
        const cid = syncCollectionRefs[tabId]?.current;
        if (items.length) ingestBatch(tabId, items, platform, cid);
        if (hasNextPage === false && syncingRef.current[tabId]) {
          finishSync(tabId);
        }
      } else if (e.channel === 'ss-select') {
        // Selection overlay (webview-select.js) → host.
        const msg = e.args[0] || {};
        if (msg.type === 'count') {
          setSelectedCount((c) => ({ ...c, [tabId]: msg.count || 0 }));
        } else if (msg.type === 'check' && Array.isArray(msg.keys) && msg.keys.length) {
          // Resolve which of these posts already live in the local DB (matching on
          // shortcode/id), then tell the overlay so it can flag them as
          // "Già in database" (disabled). Matching by key catches posts saved in
          // earlier sessions, not just ones re-intercepted right now.
          if (import.meta.env.DEV)
            console.log('[ss-select]', tabId, 'check keys:', msg.keys.length, msg.keys.slice(0, 3));
          window.electronAPI
            .savedByKeys(msg.keys)
            .then((pairs) => {
              if (import.meta.env.DEV)
                console.log(
                  '[ss-select]',
                  tabId,
                  'savedByKeys →',
                  pairs ? pairs.length : 'null',
                  (pairs || []).slice(0, 3),
                );
              if (!pairs || !pairs.length) return;
              const wv = webviewRefs[tabId]?.current;
              if (wv) {
                wv.executeJavaScript(
                  `window.__ssSelect && window.__ssSelect.markSaved(${JSON.stringify(pairs)})`,
                ).catch((err) => console.warn('[ss-select] markSaved failed', err));
              }
            })
            .catch((err) => console.warn('[ss-select] savedByKeys failed', err));
        } else if (msg.type === 'open' && msg.id != null) {
          // "Già in database" clicked → open the saved post's modal.
          window.electronAPI
            .getPostsByIds([String(msg.id)])
            .then((posts) => {
              if (posts && posts[0]) setActivePost(posts[0]);
            })
            .catch(() => {});
        } else if (msg.type === 'debug') {
          // Diagnostics from the overlay (dev-only).
          if (import.meta.env.DEV) {
            if (msg.found != null) console.log('[ss-select]', tabId, 'tiles found:', msg.found);
            if (msg.marked != null) {
              console.log(
                '[ss-select]',
                tabId,
                'markSaved: received',
                msg.marked,
                '· matching decorated tiles',
                msg.matchedDecor,
                '· sample decorated key:',
                msg.sampleDecorKey,
              );
            }
          }
        }
      }
    };

    // Inject the fetch/XHR-patching script AND the selection-overlay script into
    // the MAIN world via executeJavaScript (bypasses the page CSP, unlike a DOM
    // <script> tag). Both scripts self-guard against double-injection. After a
    // navigation/reload the MAIN world is fresh, so if select mode is on for this
    // tab we re-enable the overlay once the new page's script is in place.
    const injectInterceptor = (tabId) => () => {
      const wv = webviewRefs[tabId]?.current;
      if (!wv) return;
      // Inject once per document load: dom-ready and did-finish-load both fire for
      // the same load, and the scripts self-guard in-page, but re-sending +
      // re-parsing them over IPC on every page transition is wasted work. The flag
      // is cleared on did-start-loading when a fresh (script-less) MAIN world begins.
      const guard = injectedForLoadRefs[tabId];
      if (guard?.current) return;
      // Don't burn the once-per-load guard until the interceptor script is actually
      // available: the script is fetched async at mount, so if dom-ready fires first
      // we must let the later did-finish-load retry instead of marking this load
      // "injected" with nothing in it (which would leave the page with no interceptor).
      const script = injectedScriptRef.current;
      if (!script) return;
      if (guard) guard.current = true;
      wv.executeJavaScript(script).catch(() => {});
      const sel = selectScriptRef.current;
      if (sel) {
        wv.executeJavaScript(sel)
          .then(() => {
            if (selectModeRef.current[tabId]) {
              wv.executeJavaScript('window.__ssSelect && window.__ssSelect.enable()').catch(
                () => {},
              );
            }
          })
          .catch(() => {});
      }
    };

    const handlers = {};
    const domReadyHandlers = {};
    const loadHandlers = {};
    for (const tab of TABS) {
      const wv = webviewRefs[tab.id].current;
      if (wv) {
        handlers[tab.id] = handleIpcMessage(tab.id);
        wv.addEventListener('ipc-message', handlers[tab.id]);
        domReadyHandlers[tab.id] = injectInterceptor(tab.id);
        wv.addEventListener('dom-ready', domReadyHandlers[tab.id]);
        loadHandlers[tab.id] = injectInterceptor(tab.id);
        wv.addEventListener('did-finish-load', loadHandlers[tab.id]);
      }
    }

    return () => {
      for (const tab of TABS) {
        const wv = webviewRefs[tab.id].current;
        if (wv) {
          if (handlers[tab.id]) wv.removeEventListener('ipc-message', handlers[tab.id]);
          if (domReadyHandlers[tab.id])
            wv.removeEventListener('dom-ready', domReadyHandlers[tab.id]);
          if (loadHandlers[tab.id]) wv.removeEventListener('did-finish-load', loadHandlers[tab.id]);
        }
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []); // intentionally empty — attach once on mount
}
