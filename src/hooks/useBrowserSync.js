import { useEffect, useRef, useState } from 'react';
import { TABS } from '../lib/browserUrls';
import { SCROLL_SCRIPTS, IG_FEED_REPLAY } from '../lib/browserScripts';

const ZERO = { instagram: 0, twitter: 0, pinterest: 0 };
const FALSE = { instagram: false, twitter: false, pinterest: false };

// Sync (auto-import) lifecycle for the Browser webviews: per-tab syncing flags +
// counters, the elapsed timers, the pre-sync capture buffer, the folder-import
// collection target, and the start/stop/ingest machinery the intercept handler
// drives. All functions read only refs and stable setters, so they are safe to
// call from any closure (effects, handlers) without stale state.
export default function useBrowserSync({
  activeTab,
  webviewRefs,
  injectedScriptRef,
  onSyncingChange,
  onCollectionsChanged,
}) {
  // All sync-related counters are per-tab so Instagram and Twitter can sync concurrently.
  const [interceptedCount, setInterceptedCount] = useState({ ...ZERO });
  const [syncing, setSyncing] = useState({ ...FALSE });
  const [syncScanned, setSyncScanned] = useState({ ...ZERO });
  const [syncNew, setSyncNew] = useState({ ...ZERO });
  const [syncSkipped, setSyncSkipped] = useState({ ...ZERO });
  const [syncElapsed, setSyncElapsed] = useState({ ...ZERO });
  const [libraryTotal, setLibraryTotal] = useState(null);
  // Stable per-tab ref containers created once. Using useRef(...).current keeps a
  // fixed number of hook calls (Rules of Hooks) instead of calling useRef inside
  // an object literal that React re-evaluates every render.
  const syncTimerRefs = useRef({
    instagram: { current: null },
    twitter: { current: null },
    pinterest: { current: null },
  }).current;
  // Monotonic per-tab sync generation. startSync bumps it and its async
  // completion callbacks capture the value, so a previous sync settling late
  // can't tear down the sync that replaced it on the same tab (clearing the new
  // timer, flagging __syncStop on the new scripts, nulling the new collection
  // target). finishSync no-ops when handed a stale generation.
  const syncGenRefs = useRef({
    instagram: { current: 0 },
    twitter: { current: 0 },
    pinterest: { current: 0 },
  }).current;
  // Per-tab promise that settles once the current sync's in-page scripts have
  // finished (ran to completion or observed __syncStop). useSourceSync awaits
  // it on takeover so the old scroll loop can't keep producing into the new step.
  const syncScriptPromiseRefs = useRef({
    instagram: { current: null },
    twitter: { current: null },
    pinterest: { current: null },
  }).current;
  // Per-tab counters mirrored synchronously by ingestBatch (the state counters
  // flush on React's schedule): async consumers (useSourceSync's step
  // accounting) read exact totals here instead of sampling state after a sleep.
  const syncCountsRefs = useRef({
    instagram: { current: { scanned: 0, fresh: 0 } },
    twitter: { current: { scanned: 0, fresh: 0 } },
    pinterest: { current: { scanned: 0, fresh: 0 } },
  }).current;
  // Per-tab id of the collection the active sync/selection import should file its
  // posts into (null = no tag, plain import). Set when starting a folder import.
  const syncCollectionRefs = useRef({
    instagram: { current: null },
    twitter: { current: null },
    pinterest: { current: null },
  }).current;
  // Pre-sync capture buffer (per tab). The saved page loads its FIRST chunk as soon
  // as you open it — before Auto-import is pressed and before syncing is on — so
  // that batch would otherwise be dropped (it's never re-fetched on scroll). We
  // stash those pre-sync items here keyed by the listing URL they belong to and
  // flush them when sync starts, so the first chunk is no longer skipped.
  const pendingRefs = useRef({
    instagram: { current: { url: '', items: [] } },
    twitter: { current: { url: '', items: [] } },
    pinterest: { current: { url: '', items: [] } },
  }).current;
  const syncingRef = useRef({ ...FALSE });
  // Stable accessor for the latest onCollectionsChanged so the mount-once intercept
  // effect (which closes over finishSync) can notify the sidebar to refresh counts.
  const onCollectionsChangedRef = useRef(onCollectionsChanged);
  useEffect(() => {
    onCollectionsChangedRef.current = onCollectionsChanged;
  }, [onCollectionsChanged]);

  useEffect(() => {
    syncingRef.current = syncing;
  }, [syncing]);
  useEffect(() => {
    onSyncingChange?.(syncing);
  }, [syncing, onSyncingChange]);

  useEffect(() => {
    window.electronAPI
      .getStats()
      .then((s) => setLibraryTotal(s.total))
      .catch((err) => console.warn('getStats failed:', err));
  }, []);

  // Stop any running elapsed-timer interval on unmount so it can't fire after.
  useEffect(() => {
    return () => {
      for (const tab of TABS) {
        const ref = syncTimerRefs[tab.id];
        if (ref && ref.current != null) {
          clearInterval(ref.current);
          ref.current = null;
        }
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Idempotent sync teardown for a tab: clears the elapsed-timer interval AND
  // nulls the ref (so a second call is a no-op), tells the in-page scroll loop
  // to stop, and flips the syncing flag off. Reads only stable refs/setters, so
  // it is safe to call from any closure (effects, handlers) without stale state.
  // `gen` is passed only by startSync's async completion callbacks: when it is
  // stale (a newer sync took over the tab) the teardown is a no-op.
  const finishSync = (tab, gen = null) => {
    if (gen != null && gen !== syncGenRefs[tab]?.current) return;
    const ref = syncTimerRefs[tab];
    if (ref && ref.current != null) {
      clearInterval(ref.current);
      ref.current = null;
    }
    const wv = webviewRefs[tab]?.current;
    if (wv) wv.executeJavaScript('window.__syncStop = true').catch(() => {});
    // If this was a folder import, clear the target tag and refresh the sidebar so
    // its post count reflects what just got filed in.
    const hadCollection = syncCollectionRefs[tab]?.current != null;
    // Flip syncingRef OFF synchronously, in lockstep with nulling the collection
    // ref. setSyncing alone updates syncingRef.current only on the next render's
    // effect, so without this a batch arriving in that window would pass the
    // intercept guard (syncing still true) yet read a null collection ref — landing
    // in the library but never filed into the folder. Clearing both together keeps
    // the guard and the collection target consistent.
    syncingRef.current = { ...syncingRef.current, [tab]: false };
    if (syncCollectionRefs[tab]) syncCollectionRefs[tab].current = null;
    setSyncing((s) => (s[tab] ? { ...s, [tab]: false } : s));
    if (hadCollection) onCollectionsChangedRef.current?.();
  };

  // Persist one intercepted batch: upsert the posts, advance the per-tab sync
  // counters (only while THIS sync generation is still current) and, for a folder
  // import, file every post into the chosen tag. Shared by the live intercept
  // handler and the pre-sync buffer flush so both paths count + file identically.
  const ingestBatch = (tabId, items, platform, collectionId) => {
    const batchSize = items.length;
    // Snapshot the sync generation NOW (before the async save). The counter guard
    // below must gate on this snapshot, not on syncingRef: finishSync flips
    // syncingRef OFF synchronously (line ~139), and the intercept handler fires
    // finishSync in the SAME tick as the terminal batch's ingestBatch — so the IG
    // last page's .then would always see syncing=false and silently drop its
    // scanned/new/skipped counts. The generation only changes when a NEW sync takes
    // over the tab (startSync bumps it), so this still rejects genuinely stale
    // batches while correctly counting the batch that finishes the current sync.
    const gen = syncGenRefs[tabId]?.current;
    // Count `scanned` SYNCHRONOUSLY, before the IPC save: finishSync flips syncingRef
    // off in the same tick as the terminal batch, so useSourceSync's fold can read
    // the counters before this batch's .then runs. scanned doesn't need the save
    // result, so adding it now guarantees the terminal page lands in the run total;
    // `fresh` still waits for `inserted` from the save below.
    {
      const counts = syncCountsRefs[tabId]?.current;
      if (counts) counts.scanned += batchSize;
    }
    return window.electronAPI
      .saveInterceptedPosts(items, platform)
      .then(({ inserted, skipped }) => {
        if (gen === syncGenRefs[tabId]?.current) {
          const counts = syncCountsRefs[tabId]?.current;
          if (counts) {
            counts.fresh += inserted;
          }
          setInterceptedCount((c) => ({ ...c, [tabId]: c[tabId] + inserted }));
          setSyncScanned((c) => ({ ...c, [tabId]: c[tabId] + batchSize }));
          setSyncNew((c) => ({ ...c, [tabId]: c[tabId] + inserted }));
          setSyncSkipped((c) => ({ ...c, [tabId]: c[tabId] + skipped }));
        }
        setLibraryTotal((n) => (n === null ? inserted : n + inserted));
        if (collectionId != null) {
          const ids = items.map((it) => String(it.id)).filter(Boolean);
          if (ids.length)
            window.electronAPI.addPostsToCollections(ids, [collectionId]).catch(() => {});
        }
      })
      .catch((err) => console.error('saveInterceptedPosts failed:', err));
  };

  // Returns true if the sync actually started, false if it couldn't (no webview
  // ref, or the capture script never loaded). Callers (e.g. handleFolderConfirm)
  // use the result to surface a failure instead of silently no-op'ing.
  const startSync = (tab = activeTab, collectionId = null) => {
    const wv = webviewRefs[tab].current;
    if (!wv) return false;
    // No capture hook → the sync would scroll and capture nothing. Don't pretend.
    if (!injectedScriptRef.current) return false;
    const gen = ++syncGenRefs[tab].current;
    if (syncCollectionRefs[tab]) syncCollectionRefs[tab].current = collectionId;
    syncCountsRefs[tab].current = { scanned: 0, fresh: 0 };
    setInterceptedCount((c) => ({ ...c, [tab]: 0 }));
    setSyncScanned((c) => ({ ...c, [tab]: 0 }));
    setSyncNew((c) => ({ ...c, [tab]: 0 }));
    setSyncSkipped((c) => ({ ...c, [tab]: 0 }));
    setSyncElapsed((c) => ({ ...c, [tab]: 0 }));
    setSyncing((s) => ({ ...s, [tab]: true }));
    // Flip the ref NOW (the effect that mirrors `syncing` only runs after this
    // render): the buffer flush + any in-flight intercept below must see sync ON,
    // or their counts would be dropped by the guard in ingestBatch.
    syncingRef.current = { ...syncingRef.current, [tab]: true };
    // Clear any prior interval before starting a new one (idempotent start).
    if (syncTimerRefs[tab].current != null) clearInterval(syncTimerRefs[tab].current);
    syncTimerRefs[tab].current = setInterval(
      () => setSyncElapsed((c) => ({ ...c, [tab]: c[tab] + 1 })),
      1000,
    );
    // Flush the pre-sync buffer: the FIRST chunk loaded when the saved page opened
    // (before sync was on) lives here and is never re-fetched on scroll, so without
    // this it would be skipped. Dedupe by id so a twice-intercepted post isn't
    // double-counted.
    const pending = pendingRefs[tab]?.current;
    if (pending && pending.items.length) {
      const seen = new Set();
      const items = pending.items.filter((it) => {
        const k = String(it.id || it.shortcode || '');
        if (!k || seen.has(k)) return false;
        seen.add(k);
        return true;
      });
      pending.url = '';
      pending.items = [];
      if (items.length) ingestBatch(tab, items, tab, collectionId);
    }
    // Clear the previous sync's stop flag BEFORE the replay runs. finishSync sets
    // window.__syncStop=true in the page; SCROLL_SCRIPTS resets it to false, but it
    // is queued AFTER the replay below. IG_FEED_REPLAY's loop guard
    // (`i<100 && !window.__syncStop`) would otherwise observe the stale `true` from
    // the prior sync and exit at i=0, silently dropping the SSR-inline first page —
    // the whole point of the replay. Reset it first so both producers start clean.
    wv.executeJavaScript('window.__syncStop = false').catch(() => {});
    const script = SCROLL_SCRIPTS[tab] || SCROLL_SCRIPTS.instagram;
    if (tab === 'instagram') {
      // Instagram drives a folder import off TWO concurrent producers:
      //   1. IG_FEED_REPLAY — the deterministic source: it refetches every REST
      //      page of the listing from the top (incl. the SSR-inline first page the
      //      passive hook can't see, since it never travels over fetch/XHR). Its
      //      responses flow through the same intercept pipeline; its final
      //      more_available=false page fires finishSync via the intercept handler.
      //   2. the scroll loop — only an accelerator for IG's virtual loader.
      // We must wait for BOTH before tearing the sync down. If the scroll loop
      // alone called finishSync (the old behaviour) it would set window.__syncStop
      // and TRUNCATE the replay mid-flight: the replay's still-in-flight final
      // pages then land in the intercept handler with a now-null collection ref,
      // so those posts get saved to the library but never filed into the folder —
      // the folder count ends up short by ~one page. Awaiting both lets the replay
      // run to completion so every page is filed under a valid collection id. The
      // replay's terminal page still fires finishSync early in the common case;
      // this allSettled is the safety net for when the replay breaks on a network
      // error and never emits that signal, so the sync can't hang.
      const replay = wv.executeJavaScript(IG_FEED_REPLAY).catch(() => {});
      const scroll = wv.executeJavaScript(script).catch(() => {});
      const scripts = Promise.allSettled([replay, scroll]);
      syncScriptPromiseRefs[tab].current = scripts;
      scripts.then(() => finishSync(tab, gen));
    } else {
      if (tab === 'pinterest') {
        // Capture the SSR-inline first page of pins before the scroll loop runs.
        wv.executeJavaScript('window.__ssReplayPinterest && window.__ssReplayPinterest()').catch(
          () => {},
        );
      }
      const scripts = wv.executeJavaScript(script).catch(() => {});
      syncScriptPromiseRefs[tab].current = scripts;
      scripts.then(() => finishSync(tab, gen));
    }
    return true;
  };

  const stopSync = (tab = activeTab) => finishSync(tab);

  return {
    syncing,
    syncingRef,
    interceptedCount,
    syncScanned,
    syncNew,
    syncSkipped,
    syncElapsed,
    libraryTotal,
    setLibraryTotal,
    syncTimerRefs,
    syncCollectionRefs,
    syncScriptPromiseRefs,
    syncCountsRefs,
    pendingRefs,
    finishSync,
    ingestBatch,
    startSync,
    stopSync,
  };
}
