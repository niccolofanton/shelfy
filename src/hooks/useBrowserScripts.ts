import { useEffect, useRef, useState } from 'react';

// The three Browser platforms; all per-tab refs/state in the browser hooks are
// keyed by these ids (mirrors BrowserTab in views/Browser.tsx).
type BrowserTab = 'instagram' | 'twitter' | 'pinterest';

// Per-tab inject-once guard: a fixed map of mutable boxes (created once via
// useRef(...).current), each toggled by the injector and the nav listeners.
export type PerTabRefMap<T> = Record<BrowserTab, { current: T }>;

// Load status of the MAIN-world scripts. 'loading' while fetching, 'ready' once
// both arrive, 'error' if the IPC failed or returned empty.
export type ScriptsStatus = 'loading' | 'ready' | 'error';

export interface UseBrowserScripts {
  injectedScriptRef: React.MutableRefObject<string>;
  selectScriptRef: React.MutableRefObject<string>;
  injectedForLoadRefs: PerTabRefMap<boolean>;
  scriptsStatus: ScriptsStatus;
  retryLoadScripts: () => void;
}

// Loads the two MAIN-world scripts the Browser webviews need (the fetch/XHR
// capture hook and the selection overlay) from the main process, caching them in
// refs so dom-ready/did-finish-load injection stays synchronous. Also owns the
// per-tab inject-once-per-document-load guard shared by the injector and the
// navigation listeners (did-start-loading clears it on a fresh MAIN world).
export default function useBrowserScripts(): UseBrowserScripts {
  // MAIN-world selection-overlay script (webview-select.js), fetched once.
  const selectScriptRef = useRef<string>('');
  // MAIN-world capture script, fetched once from the main process (async IPC)
  // and cached so dom-ready/did-finish-load injection stays synchronous.
  const injectedScriptRef = useRef<string>('');
  // Per-tab guard so the capture + select scripts are injected ONCE per document
  // load instead of twice (dom-ready AND did-finish-load both fire each load). The
  // in-page idempotency guards still protect correctness; this avoids re-sending +
  // re-parsing the scripts over IPC on every page transition. Reset on a new load.
  const injectedForLoadRefs = useRef<PerTabRefMap<boolean>>({
    instagram: { current: false },
    twitter: { current: false },
    pinterest: { current: false },
  }).current;

  // Load status of the MAIN-world scripts. Until both are populated the capture
  // hook / selection overlay can't be installed, so Auto-import and Select must be
  // disabled — otherwise a sync flips on, scrolls, and silently captures nothing.
  // 'loading' | 'ready' | 'error' (IPC failed or returned empty).
  const [scriptsStatus, setScriptsStatus] = useState<ScriptsStatus>('loading');
  const [scriptsReloadKey, setScriptsReloadKey] = useState<number>(0);

  useEffect(() => {
    let cancelled = false;
    setScriptsStatus('loading');
    Promise.all([
      window.electronAPI?.getWebviewInjectedScript?.() ?? Promise.resolve(''),
      window.electronAPI?.getWebviewSelectScript?.() ?? Promise.resolve(''),
    ])
      .then(([injected, select]) => {
        if (cancelled) return;
        injectedScriptRef.current = injected || '';
        selectScriptRef.current = select || '';
        // Both scripts are required: the capture hook AND the selection overlay.
        setScriptsStatus(injected && select ? 'ready' : 'error');
      })
      .catch(() => {
        if (!cancelled) setScriptsStatus('error');
      });
    return () => {
      cancelled = true;
    };
  }, [scriptsReloadKey]);

  const retryLoadScripts = (): void => setScriptsReloadKey((k) => k + 1);

  return {
    injectedScriptRef,
    selectScriptRef,
    injectedForLoadRefs,
    scriptsStatus,
    retryLoadScripts,
  };
}
