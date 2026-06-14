import { useEffect, useRef, useState } from 'react';
import {
  TABS,
  IG_SAVED_URL_KEY,
  PIN_BOARD_URL_KEY,
  IG_LOGIN_PATTERN,
  SAVED_PATTERNS,
  isPostDetail,
  isAllowedUrl,
  safeUrl,
} from '../lib/browserUrls';

const FALSE = { instagram: false, twitter: false, pinterest: false };

// loadURL rejects on any interrupted navigation (ERR_ABORTED on SPA redirects
// is routine); an unhandled rejection here would surface as console noise with
// no actionable context, so log it as a warning instead.
const logLoadError = (tab) => (err) =>
  console.warn(`[browser] loadURL failed (${tab}):`, err?.message || err);

// Webview navigation lifecycle for the Browser tabs: per-tab url/loading/
// canGoBack/canGoForward state, the did-navigate/did-*-loading listeners (with
// their exact registration/cleanup order), the persisted IG-saved/Pinterest-board
// URL handling, and the back/forward/refresh actions. `stopSync` is captured by
// the mount-once listeners; like the original inline version it reads only refs
// and stable setters, so the first-render instance is safe.
export default function useBrowserWebview({
  activeTab,
  webviewRefs,
  syncingRef,
  pendingRefs,
  injectedForLoadRefs,
  stopSync,
  // Per-platform map of in-flight background source-syncs (useSourceSync's
  // runRef). While one is navigating a tab, the saved-URL auto-restore below
  // must not yank the webview to a different page out from under it.
  externalNavRef,
}) {
  const igInitialUrl = useRef(
    safeUrl('instagram', localStorage.getItem(IG_SAVED_URL_KEY), TABS[0].url),
  );
  const igPrevUrl = useRef('');
  // Pinterest reopens on the last board the user synced, like IG's saved URL.
  const pinInitialUrl = useRef(
    safeUrl('pinterest', localStorage.getItem(PIN_BOARD_URL_KEY), TABS[2].url),
  );
  const [urls, setUrls] = useState({
    instagram: igInitialUrl.current,
    twitter: TABS[1].url,
    pinterest: pinInitialUrl.current,
  });
  const [loading, setLoading] = useState({ ...FALSE });
  const [canGoBack, setCanGoBack] = useState({ ...FALSE });
  const [canGoForward, setCanGoForward] = useState({ ...FALSE });

  useEffect(() => {
    // Attach nav/loading listeners to EVERY tab, not just the active one. A tab
    // left in the background keeps syncing by design, so it still needs fresh
    // url/loading/canGoBack/canGoForward state AND the auto-stop-on-leave-saved
    // guard. Handlers are built per-tab via factories that close over `tab`.
    const makeUpdateNavState = (tab, wv) => () => {
      setCanGoBack((prev) => ({ ...prev, [tab]: wv.canGoBack() }));
      setCanGoForward((prev) => ({ ...prev, [tab]: wv.canGoForward() }));
    };

    const makeHandleNavigate = (tab, wv, updateNavState) => (e) => {
      const url = e.url || e.newURL || '';
      setUrls((prev) => ({ ...prev, [tab]: url }));
      updateNavState();

      if (tab === 'instagram') {
        if (SAVED_PATTERNS.instagram.test(url)) {
          localStorage.setItem(IG_SAVED_URL_KEY, url);
        } else if (
          url === 'https://www.instagram.com/' &&
          IG_LOGIN_PATTERN.test(igPrevUrl.current)
        ) {
          const savedUrl = localStorage.getItem(IG_SAVED_URL_KEY);
          if (savedUrl && isAllowedUrl('instagram', savedUrl)) {
            const w = webviewRefs[tab].current;
            if (w) w.loadURL(savedUrl).catch(logLoadError(tab));
          }
        }
        igPrevUrl.current = url;
      }

      // Pinterest: remember the last board page so the tab reopens on it.
      if (tab === 'pinterest' && SAVED_PATTERNS.pinterest.test(url)) {
        localStorage.setItem(PIN_BOARD_URL_KEY, url);
      }

      // Keep the pre-sync capture buffer scoped to the current listing. On a saved
      // page we (re)bind it to that URL, dropping items only when the listing
      // itself changes (a different folder / all-posts) — NOT when a post modal
      // opens (its in-page nav to /p/… is not a saved URL, so we leave it alone).
      // Off the saved page entirely, clear it.
      const buf = pendingRefs[tab]?.current;
      if (buf) {
        if (SAVED_PATTERNS[tab]?.test(url)) {
          if (buf.url !== url) {
            buf.url = url;
            buf.items = [];
          }
        } else if (!isPostDetail(url)) {
          // Left the listing for something other than a post/pin detail → reset.
          buf.url = '';
          buf.items = [];
        }
      }

      // Per-tab auto-stop: if this tab is syncing but navigated away from its
      // saved page, stop its sync (works for background tabs too, where the
      // active-tab-only effect can't observe the leave). Opening a post/pin
      // detail is an in-page nav with the listing still mounted behind the
      // modal — same exception as the buffer above — so it must not stop sync.
      if (syncingRef.current[tab] && !SAVED_PATTERNS[tab]?.test(url) && !isPostDetail(url)) {
        stopSync(tab);
      }
    };

    const navHandlers = {};
    const loadStartHandlers = {};
    const loadStopHandlers = {};
    const loadAbortHandlers = {};

    for (const t of TABS) {
      const tab = t.id;
      const wv = webviewRefs[tab].current;
      if (!wv) continue;
      const updateNavState = makeUpdateNavState(tab, wv);
      navHandlers[tab] = makeHandleNavigate(tab, wv, updateNavState);
      loadStartHandlers[tab] = () => {
        // A new full load means a fresh, script-less MAIN world: clear the
        // inject-once guard so the next dom-ready re-installs the capture/select
        // scripts. In-page navigations (did-navigate-in-page) don't reset the MAIN
        // world and don't fire did-start-loading, so the scripts stay live there.
        if (injectedForLoadRefs[tab]) injectedForLoadRefs[tab].current = false;
        setLoading((prev) => ({ ...prev, [tab]: true }));
      };
      loadStopHandlers[tab] = () => {
        setLoading((prev) => ({ ...prev, [tab]: false }));
        updateNavState();
      };
      loadAbortHandlers[tab] = (e) => {
        setLoading((prev) => ({ ...prev, [tab]: false }));
        // did-fail-load fallback (IG/Pinterest only): the tab's first load points
        // at the persisted saved/board URL from localStorage. If that folder/board
        // is no longer reachable (logout, deleted folder, revoked access) the load
        // fails and there's no further re-navigation. Detect the failure of the
        // persisted URL for the main frame, clear the stale key, and fall back to
        // the tab's default home so the webview isn't stuck on a dead page.
        // Ignore sub-frame failures (isMainFrame === false) and benign aborts.
        if (e && e.isMainFrame === false) return;
        if (e && (e.errorCode === -3 || e.errorCode === 0)) return; // ERR_ABORTED / no error
        const key =
          tab === 'instagram' ? IG_SAVED_URL_KEY : tab === 'pinterest' ? PIN_BOARD_URL_KEY : null;
        if (!key) return;
        const saved = localStorage.getItem(key);
        const failedUrl = (e && e.validatedURL) || '';
        if (saved && failedUrl && failedUrl === saved) {
          localStorage.removeItem(key);
          const w = webviewRefs[tab].current;
          if (w) w.loadURL(TABS.find((t) => t.id === tab).url).catch(logLoadError(tab));
        }
      };

      wv.addEventListener('did-navigate', navHandlers[tab]);
      wv.addEventListener('did-navigate-in-page', navHandlers[tab]);
      wv.addEventListener('did-start-loading', loadStartHandlers[tab]);
      wv.addEventListener('did-stop-loading', loadStopHandlers[tab]);
      wv.addEventListener('did-fail-load', loadAbortHandlers[tab]);
    }

    return () => {
      for (const t of TABS) {
        const tab = t.id;
        const wv = webviewRefs[tab].current;
        if (!wv) continue;
        if (navHandlers[tab]) {
          wv.removeEventListener('did-navigate', navHandlers[tab]);
          wv.removeEventListener('did-navigate-in-page', navHandlers[tab]);
        }
        if (loadStartHandlers[tab])
          wv.removeEventListener('did-start-loading', loadStartHandlers[tab]);
        if (loadStopHandlers[tab])
          wv.removeEventListener('did-stop-loading', loadStopHandlers[tab]);
        if (loadAbortHandlers[tab]) wv.removeEventListener('did-fail-load', loadAbortHandlers[tab]);
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []); // attach once on mount — handlers are per-tab and read live refs/setters

  // React to tab changes driven by the sidebar. Sync is NOT stopped on the tab we're
  // leaving — it keeps running in the background. We only restore Instagram's saved URL
  // when returning to it, and only if it isn't mid-sync (a reload would interrupt it).
  const prevTabRef = useRef(activeTab);
  useEffect(() => {
    const prevTab = prevTabRef.current;
    if (prevTab === activeTab) return;
    prevTabRef.current = activeTab;

    if (
      activeTab === 'instagram' &&
      !syncingRef.current.instagram &&
      !externalNavRef?.current?.instagram
    ) {
      const savedUrl = localStorage.getItem(IG_SAVED_URL_KEY);
      if (savedUrl && isAllowedUrl('instagram', savedUrl)) {
        const wv = webviewRefs.instagram.current;
        if (wv) wv.loadURL(savedUrl).catch(logLoadError('instagram'));
      }
    }
    if (
      activeTab === 'pinterest' &&
      !syncingRef.current.pinterest &&
      !externalNavRef?.current?.pinterest
    ) {
      const savedUrl = localStorage.getItem(PIN_BOARD_URL_KEY);
      if (savedUrl && isAllowedUrl('pinterest', savedUrl)) {
        const wv = webviewRefs.pinterest.current;
        if (wv) wv.loadURL(savedUrl).catch(logLoadError('pinterest'));
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeTab]);

  const handleRefresh = () => {
    const wv = webviewRefs[activeTab].current;
    if (wv) wv.reload();
  };

  const handleBack = () => {
    const wv = webviewRefs[activeTab].current;
    if (wv && wv.canGoBack()) wv.goBack();
  };

  const handleForward = () => {
    const wv = webviewRefs[activeTab].current;
    if (wv && wv.canGoForward()) wv.goForward();
  };

  return {
    urls,
    loading,
    canGoBack,
    canGoForward,
    igInitialUrl,
    pinInitialUrl,
    handleRefresh,
    handleBack,
    handleForward,
  };
}
