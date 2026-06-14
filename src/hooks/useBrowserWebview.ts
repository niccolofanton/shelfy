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
import type { SanitizedItem } from '../lib/browserSanitize';

// The three Browser platforms; all per-tab state/refs are keyed by these ids
// (mirrors BrowserTab in views/Browser.tsx).
type BrowserTab = 'instagram' | 'twitter' | 'pinterest';

// Minimal imperative surface of the Electron <webview> guest element this hook
// touches. Mirrors ElectronWebview in views/Browser.tsx so the shared per-tab
// ref containers are assignable across the boundary.
interface ElectronWebview extends HTMLElement {
  getURL(): string;
  loadURL(url: string): Promise<void>;
  reload(): void;
  goBack(): void;
  goForward(): void;
  canGoBack(): boolean;
  canGoForward(): boolean;
  executeJavaScript(code: string): Promise<unknown>;
}

type WebviewRefs = Record<BrowserTab, React.MutableRefObject<ElectronWebview | null>>;
type PerTabRefMap<T> = Record<BrowserTab, { current: T }>;
type PerTabState<T> = Record<BrowserTab, T>;

// The <webview> 'did-navigate' / 'did-navigate-in-page' event: the destination
// URL (full nav uses `url`, in-page uses `newURL`).
interface WebviewNavigateEvent extends Event {
  url?: string;
  newURL?: string;
}

// The <webview> 'did-fail-load' event: a failed load with its error code, the
// frame it occurred in, and the URL that failed.
interface WebviewFailLoadEvent extends Event {
  isMainFrame?: boolean;
  errorCode?: number;
  validatedURL?: string;
}

export interface UseBrowserWebviewOptions {
  activeTab: BrowserTab;
  webviewRefs: WebviewRefs;
  syncingRef: React.MutableRefObject<PerTabState<boolean>>;
  pendingRefs: PerTabRefMap<{ url: string; items: SanitizedItem[] }>;
  injectedForLoadRefs: PerTabRefMap<boolean>;
  stopSync: (tab?: BrowserTab) => void;
  // Per-platform map of in-flight background source-syncs (useSourceSync's
  // runRef). While one is navigating a tab, the saved-URL auto-restore below
  // must not yank the webview to a different page out from under it. Read-only
  // here (truthiness check per platform); the value is owned by useSourceSync.
  externalNavRef?: { readonly current: Record<string, unknown> | null | undefined };
}

export interface UseBrowserWebview {
  urls: PerTabState<string>;
  loading: PerTabState<boolean>;
  canGoBack: PerTabState<boolean>;
  canGoForward: PerTabState<boolean>;
  igInitialUrl: React.MutableRefObject<string>;
  pinInitialUrl: React.MutableRefObject<string>;
  handleRefresh: () => void;
  handleBack: () => void;
  handleForward: () => void;
}

const FALSE: PerTabState<boolean> = { instagram: false, twitter: false, pinterest: false };

// loadURL rejects on any interrupted navigation (ERR_ABORTED on SPA redirects
// is routine); an unhandled rejection here would surface as console noise with
// no actionable context, so log it as a warning instead.
const logLoadError =
  (tab: BrowserTab) =>
  (err: unknown): void =>
    console.warn(`[browser] loadURL failed (${tab}):`, (err as Error)?.message || err);

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
  externalNavRef,
}: UseBrowserWebviewOptions): UseBrowserWebview {
  const igInitialUrl = useRef<string>(
    safeUrl('instagram', localStorage.getItem(IG_SAVED_URL_KEY), TABS[0].url),
  );
  const igPrevUrl = useRef<string>('');
  // Pinterest reopens on the last board the user synced, like IG's saved URL.
  const pinInitialUrl = useRef<string>(
    safeUrl('pinterest', localStorage.getItem(PIN_BOARD_URL_KEY), TABS[2].url),
  );
  const [urls, setUrls] = useState<PerTabState<string>>({
    instagram: igInitialUrl.current,
    twitter: TABS[1].url,
    pinterest: pinInitialUrl.current,
  });
  const [loading, setLoading] = useState<PerTabState<boolean>>({ ...FALSE });
  const [canGoBack, setCanGoBack] = useState<PerTabState<boolean>>({ ...FALSE });
  const [canGoForward, setCanGoForward] = useState<PerTabState<boolean>>({ ...FALSE });

  useEffect(() => {
    // Attach nav/loading listeners to EVERY tab, not just the active one. A tab
    // left in the background keeps syncing by design, so it still needs fresh
    // url/loading/canGoBack/canGoForward state AND the auto-stop-on-leave-saved
    // guard. Handlers are built per-tab via factories that close over `tab`.
    const makeUpdateNavState = (tab: BrowserTab, wv: ElectronWebview) => (): void => {
      setCanGoBack((prev) => ({ ...prev, [tab]: wv.canGoBack() }));
      setCanGoForward((prev) => ({ ...prev, [tab]: wv.canGoForward() }));
    };

    const makeHandleNavigate =
      (tab: BrowserTab, wv: ElectronWebview, updateNavState: () => void) =>
      (e: WebviewNavigateEvent): void => {
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

    const navHandlers: Partial<Record<BrowserTab, EventListener>> = {};
    const loadStartHandlers: Partial<Record<BrowserTab, EventListener>> = {};
    const loadStopHandlers: Partial<Record<BrowserTab, EventListener>> = {};
    const loadAbortHandlers: Partial<Record<BrowserTab, EventListener>> = {};

    for (const t of TABS) {
      const tab = t.id as BrowserTab;
      const wv = webviewRefs[tab].current;
      if (!wv) continue;
      const updateNavState = makeUpdateNavState(tab, wv);
      const navHandler = makeHandleNavigate(tab, wv, updateNavState) as unknown as EventListener;
      const loadStartHandler: EventListener = (): void => {
        // A new full load means a fresh, script-less MAIN world: clear the
        // inject-once guard so the next dom-ready re-installs the capture/select
        // scripts. In-page navigations (did-navigate-in-page) don't reset the MAIN
        // world and don't fire did-start-loading, so the scripts stay live there.
        if (injectedForLoadRefs[tab]) injectedForLoadRefs[tab].current = false;
        setLoading((prev) => ({ ...prev, [tab]: true }));
      };
      const loadStopHandler: EventListener = (): void => {
        setLoading((prev) => ({ ...prev, [tab]: false }));
        updateNavState();
      };
      const loadAbortHandler = ((e: WebviewFailLoadEvent): void => {
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
          const home = TABS.find((tt) => tt.id === tab)?.url;
          if (w && home) w.loadURL(home).catch(logLoadError(tab));
        }
      }) as unknown as EventListener;

      navHandlers[tab] = navHandler;
      loadStartHandlers[tab] = loadStartHandler;
      loadStopHandlers[tab] = loadStopHandler;
      loadAbortHandlers[tab] = loadAbortHandler;

      wv.addEventListener('did-navigate', navHandler);
      wv.addEventListener('did-navigate-in-page', navHandler);
      wv.addEventListener('did-start-loading', loadStartHandler);
      wv.addEventListener('did-stop-loading', loadStopHandler);
      wv.addEventListener('did-fail-load', loadAbortHandler);
    }

    return () => {
      for (const t of TABS) {
        const tab = t.id as BrowserTab;
        const wv = webviewRefs[tab].current;
        if (!wv) continue;
        const navHandler = navHandlers[tab];
        if (navHandler) {
          wv.removeEventListener('did-navigate', navHandler);
          wv.removeEventListener('did-navigate-in-page', navHandler);
        }
        const loadStartHandler = loadStartHandlers[tab];
        if (loadStartHandler) wv.removeEventListener('did-start-loading', loadStartHandler);
        const loadStopHandler = loadStopHandlers[tab];
        if (loadStopHandler) wv.removeEventListener('did-stop-loading', loadStopHandler);
        const loadAbortHandler = loadAbortHandlers[tab];
        if (loadAbortHandler) wv.removeEventListener('did-fail-load', loadAbortHandler);
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []); // attach once on mount — handlers are per-tab and read live refs/setters

  // React to tab changes driven by the sidebar. Sync is NOT stopped on the tab we're
  // leaving — it keeps running in the background. We only restore Instagram's saved URL
  // when returning to it, and only if it isn't mid-sync (a reload would interrupt it).
  const prevTabRef = useRef<BrowserTab>(activeTab);
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

  const handleRefresh = (): void => {
    const wv = webviewRefs[activeTab].current;
    if (wv) wv.reload();
  };

  const handleBack = (): void => {
    const wv = webviewRefs[activeTab].current;
    if (wv && wv.canGoBack()) wv.goBack();
  };

  const handleForward = (): void => {
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
