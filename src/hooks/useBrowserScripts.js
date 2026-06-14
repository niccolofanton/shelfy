import { useEffect, useRef, useState } from 'react';

// Loads the two MAIN-world scripts the Browser webviews need (the fetch/XHR
// capture hook and the selection overlay) from the main process, caching them in
// refs so dom-ready/did-finish-load injection stays synchronous. Also owns the
// per-tab inject-once-per-document-load guard shared by the injector and the
// navigation listeners (did-start-loading clears it on a fresh MAIN world).
export default function useBrowserScripts() {
  // MAIN-world selection-overlay script (webview-select.js), fetched once.
  const selectScriptRef = useRef('');
  // MAIN-world capture script, fetched once from the main process (async IPC)
  // and cached so dom-ready/did-finish-load injection stays synchronous.
  const injectedScriptRef = useRef('');
  // Per-tab guard so the capture + select scripts are injected ONCE per document
  // load instead of twice (dom-ready AND did-finish-load both fire each load). The
  // in-page idempotency guards still protect correctness; this avoids re-sending +
  // re-parsing the scripts over IPC on every page transition. Reset on a new load.
  const injectedForLoadRefs = useRef({
    instagram: { current: false },
    twitter: { current: false },
    pinterest: { current: false },
  }).current;

  // Load status of the MAIN-world scripts. Until both are populated the capture
  // hook / selection overlay can't be installed, so Auto-import and Select must be
  // disabled — otherwise a sync flips on, scrolls, and silently captures nothing.
  // 'loading' | 'ready' | 'error' (IPC failed or returned empty).
  const [scriptsStatus, setScriptsStatus] = useState('loading');
  const [scriptsReloadKey, setScriptsReloadKey] = useState(0);

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

  const retryLoadScripts = () => setScriptsReloadKey((k) => k + 1);

  return {
    injectedScriptRef,
    selectScriptRef,
    injectedForLoadRefs,
    scriptsStatus,
    retryLoadScripts,
  };
}
