import { useEffect, useRef, useState } from 'react';
import { SAVED_PATTERNS, LOGIN_PATTERNS, IG_SAVED_URL_KEY, isAllowedUrl } from '../lib/browserUrls';
import { IG_GET_USERNAME, findIgFolderHref } from '../lib/browserScripts';

// Background "source sync" launched from the Library sidebar: drives the
// always-mounted Browser webviews to the right listing page (IG all-posts / IG
// saved folder / X bookmarks / Pinterest board) and runs the existing
// startSync/intercept pipeline there, with no Browser view on screen.
//
// A run is a SEQUENCE of steps (platform-level sync = all-posts + every native
// folder, one at a time; Pinterest = every board) and is interruptible at any
// point via stop(). One run per platform at a time — the underlying sync
// machinery (syncing flags, collection target, counters) is per-tab.
//
// Job state (one per platform) is reported up to App, which feeds it to the
// Activity Center (live progress + completion/error log) and to the sidebar
// buttons (spinner on the row being synced).

const TW_BOOKMARKS_URL = 'https://x.com/i/bookmarks';

// Error taxonomy for a failed step. 'login' aborts the whole run (every later
// step needs the same session); the others skip just that step.
const err = (code) => Object.assign(new Error(code), { code });

// Pure step planner, exported for tests. A native source is a collection that
// carries the platform's own folder/board id (externalId); custom folders
// (platform == null) and "all posts" are never syncable.
export function buildSyncSteps(target, collections = []) {
  const native = (p) => collections.filter((c) => c.platform === p && c.externalId != null);
  if (target?.type === 'collection') {
    const c = collections.find((x) => x.id === target.collectionId);
    if (!c || c.externalId == null) return [];
    if (c.platform === 'instagram') return [{ type: 'ig-folder', collection: c }];
    if (c.platform === 'pinterest') return [{ type: 'pin-board', collection: c }];
    return [];
  }
  switch (target?.platform) {
    case 'instagram':
      return [
        { type: 'ig-all' },
        ...native('instagram').map((c) => ({ type: 'ig-folder', collection: c })),
      ];
    case 'twitter':
      return [{ type: 'tw-bookmarks' }];
    case 'pinterest':
      return native('pinterest').map((c) => ({ type: 'pin-board', collection: c }));
    default:
      return [];
  }
}

export default function useSourceSync({
  webviewRefs,
  syncingRef,
  startSync,
  stopSync,
  syncScriptPromiseRefs,
  syncCountsRefs,
  scriptsStatus,
  injectedForLoadRefs,
  syncScanned,
  syncNew,
  collections,
  registerApi,
  onJobsChange,
}) {
  const [jobs, setJobs] = useState({}); // platform → job
  const runRef = useRef({}); // platform → { aborted } while a run is in flight
  const accumRef = useRef({}); // platform → counters accumulated over FINISHED steps
  const igUserRef = useRef(''); // cached IG username (cleared on login failure)

  // Mirrors so the async run loop (mount-once closures) reads live values.
  const scriptsStatusRef = useRef(scriptsStatus);
  useEffect(() => {
    scriptsStatusRef.current = scriptsStatus;
  }, [scriptsStatus]);
  const collectionsRef = useRef(collections);
  useEffect(() => {
    collectionsRef.current = collections;
  }, [collections]);
  const onJobsChangeRef = useRef(onJobsChange);
  useEffect(() => {
    onJobsChangeRef.current = onJobsChange;
  }, [onJobsChange]);

  useEffect(() => {
    onJobsChangeRef.current?.(jobs);
  }, [jobs]);

  // Live counters: while a step is syncing, the job shows the run total =
  // accumulated finished steps + the current step's per-tab counters.
  useEffect(() => {
    setJobs((prev) => {
      let changed = false;
      const next = { ...prev };
      for (const p of Object.keys(prev)) {
        const j = prev[p];
        if (!j || j.status !== 'syncing') continue;
        const acc = accumRef.current[p] || { scanned: 0, fresh: 0 };
        const scanned = acc.scanned + (syncScanned[p] || 0);
        const fresh = acc.fresh + (syncNew[p] || 0);
        if (scanned !== j.scanned || fresh !== j.fresh) {
          next[p] = { ...j, scanned, fresh };
          changed = true;
        }
      }
      return changed ? next : prev;
    });
  }, [syncScanned, syncNew]);

  const patchJob = (platform, patch) =>
    setJobs((prev) =>
      prev[platform] ? { ...prev, [platform]: { ...prev[platform], ...patch } } : prev,
    );

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const waitFor = async (fn, { timeout = 15000, interval = 300 } = {}) => {
    const t0 = Date.now();
    for (;;) {
      const v = fn();
      if (v) return v;
      if (Date.now() - t0 >= timeout) return null;
      await sleep(interval);
    }
  };
  const getUrl = (wv) => {
    try {
      return wv.getURL();
    } catch {
      return '';
    }
  };

  // Loads `url` (if not already there) and waits until the webview settles on a
  // URL matching `expected` — or on the platform's login page, which means the
  // session expired. The settle loop, not loadURL's promise, is the arbiter:
  // these SPAs routinely redirect client-side after the load "finishes".
  const navigateTo = async (platform, url, run, expected) => {
    const wv = webviewRefs[platform]?.current;
    if (!wv || !isAllowedUrl(platform, url)) throw err('nav');
    if (getUrl(wv) !== url) {
      try {
        await wv.loadURL(url);
      } catch {
        /* ERR_ABORTED on SPA redirects is routine — judged by the settle loop */
      }
    }
    const landed = await waitFor(
      () => {
        if (run.aborted) return 'aborted';
        const u = getUrl(wv);
        if (LOGIN_PATTERNS[platform]?.test(u)) return 'login';
        if (expected.test(u)) return 'ok';
        return null;
      },
      { timeout: 45000, interval: 400 },
    );
    if (landed === 'aborted' || run.aborted) throw err('aborted');
    if (landed === 'login') throw err('login');
    if (landed !== 'ok') throw err('nav');
  };

  // A sync can only capture once the MAIN-world hook is installed for the
  // current load. The script string is fetched async at mount: if both
  // dom-ready and did-finish-load fired before it was available, this load has
  // no hook — one reload re-runs the injection path now that the string exists.
  const ensureCapture = async (platform, run) => {
    const ready = await waitFor(() => run.aborted || scriptsStatusRef.current === 'ready', {
      timeout: 20000,
    });
    if (run.aborted) throw err('aborted');
    if (!ready) throw err('scripts');
    const injectedNow = () => run.aborted || injectedForLoadRefs[platform]?.current === true;
    let injected = await waitFor(injectedNow, { timeout: 8000, interval: 250 });
    if (run.aborted) throw err('aborted');
    if (!injected) {
      const wv = webviewRefs[platform]?.current;
      if (!wv) throw err('nav');
      try {
        wv.reload();
      } catch {
        /* judged by the wait below */
      }
      injected = await waitFor(injectedNow, { timeout: 20000 });
      if (run.aborted) throw err('aborted');
      if (!injected) throw err('scripts');
    }
  };

  // Starts the per-tab sync toward `collectionId` and waits for it to finish.
  // A manual sync left running on that tab is taken over (the user's Library
  // click is the most recent intent). The 35-minute cap is a safety net above
  // the scroll scripts' own 30-minute ceiling.
  const beginStep = async (platform, collectionId, run) => {
    await ensureCapture(platform, run);
    // Takeover: stop the previous sync and wait for its in-page scripts to
    // actually settle (i.e. observe __syncStop), not a guessed sleep — a still
    // running scroll loop would double-produce batches into the new step. The
    // race caps the wait in case the webview is wedged mid-script.
    if (syncingRef.current[platform]) stopSync(platform);
    const prior = syncScriptPromiseRefs?.[platform]?.current;
    if (prior) await Promise.race([prior, sleep(10000)]);
    if (run.aborted) throw err('aborted');
    if (!startSync(platform, collectionId)) throw err('start');
    patchJob(platform, { status: 'syncing' });
    await waitFor(() => run.aborted || !syncingRef.current[platform], {
      timeout: 35 * 60 * 1000,
      interval: 500,
    });
    if (syncingRef.current[platform]) stopSync(platform); // abort or timeout: tear down
  };

  const resolveIgUsername = async (run) => {
    if (igUserRef.current) return igUserRef.current;
    // Cheapest source: the persisted saved-page URL (anyone who imported a
    // folder has been there). Fallback: ask IG itself from the page origin.
    const saved = localStorage.getItem(IG_SAVED_URL_KEY) || '';
    const m = /instagram\.com\/([^/?#]+)\/saved(?:\/|$)/.exec(saved);
    if (m && m[1]) {
      igUserRef.current = m[1];
      return m[1];
    }
    const wv = webviewRefs.instagram?.current;
    if (!wv) throw err('nav');
    if (!/instagram\.com/.test(getUrl(wv))) {
      await navigateTo('instagram', 'https://www.instagram.com/', run, /instagram\.com/);
    }
    let user = '';
    try {
      user = String((await wv.executeJavaScript(IG_GET_USERNAME)) || '').trim();
    } catch {
      user = '';
    }
    if (run.aborted) throw err('aborted');
    if (!user) throw err('login');
    igUserRef.current = user;
    return user;
  };

  const runStep = async (platform, step, run) => {
    if (step.type === 'tw-bookmarks') {
      await navigateTo('twitter', TW_BOOKMARKS_URL, run, SAVED_PATTERNS.twitter);
      await beginStep('twitter', null, run);
      return;
    }
    if (step.type === 'pin-board') {
      // externalId is "<user>/<board-slug>" — exactly the board's URL path.
      const extId = String(step.collection.externalId || '');
      const path = extId.split('/').filter(Boolean).map(encodeURIComponent).join('/');
      await navigateTo(
        'pinterest',
        `https://www.pinterest.com/${path}/`,
        run,
        SAVED_PATTERNS.pinterest,
      );
      await beginStep('pinterest', step.collection.id, run);
      return;
    }
    const user = await resolveIgUsername(run);
    const base = `https://www.instagram.com/${encodeURIComponent(user)}/saved/`;
    if (step.type === 'ig-all') {
      await navigateTo('instagram', `${base}all-posts/`, run, SAVED_PATTERNS.instagram);
      await beginStep('instagram', null, run);
      return;
    }
    // ig-folder: collections persist only the rename-safe numeric folder id, not
    // the URL slug — discover the folder's real href from the saved index page.
    await navigateTo('instagram', base, run, SAVED_PATTERNS.instagram);
    const wv = webviewRefs.instagram?.current;
    const href = wv ? await findIgFolderHref(wv, step.collection.externalId) : null;
    if (run.aborted) throw err('aborted');
    if (!href) throw err('not-found');
    const folderUrl = new URL(href, 'https://www.instagram.com/').toString();
    await navigateTo('instagram', folderUrl, run, SAVED_PATTERNS.instagram);
    await beginStep('instagram', step.collection.id, run);
  };

  const start = (target) => {
    const platform = target?.platform;
    if (!platform || !webviewRefs[platform]) return false;
    if (runRef.current[platform]) return false; // one run per platform
    const steps = buildSyncSteps(target, collectionsRef.current);
    if (!steps.length) return false;
    const run = { aborted: false };
    runRef.current[platform] = run;
    accumRef.current[platform] = { scanned: 0, fresh: 0 };
    setJobs((prev) => ({
      ...prev,
      [platform]: {
        platform,
        status: 'navigating',
        error: null,
        stepIndex: 0,
        stepCount: steps.length,
        currentLabel: steps[0].collection?.name ?? null,
        currentCollectionId: steps[0].collection?.id ?? null,
        scanned: 0,
        fresh: 0,
        skipped: [],
        startedAt: Date.now(),
        finishedAt: null,
      },
    }));
    (async () => {
      const skipped = [];
      let loginError = false;
      for (let i = 0; i < steps.length && !run.aborted; i++) {
        const step = steps[i];
        patchJob(platform, {
          status: 'navigating',
          stepIndex: i,
          currentLabel: step.collection?.name ?? null,
          currentCollectionId: step.collection?.id ?? null,
        });
        try {
          await runStep(platform, step, run);
        } catch (e) {
          if (e?.code === 'aborted') break;
          if (e?.code === 'login') {
            // A stale cached username can also land here — drop it so the next
            // attempt re-resolves instead of failing forever.
            igUserRef.current = '';
            loginError = true;
            break;
          }
          skipped.push(step.collection?.name ?? step.type);
          continue; // pre-start failure: counters never ran for this step
        }
        // The step ran (possibly partially, on abort): fold its counters in.
        // ingestBatch counts `scanned` synchronously (before its IPC save), so the
        // run total includes the terminal page even though that page's save may still
        // be settling when syncingRef flips off. (`fresh` trails by the save.)
        const acc = accumRef.current[platform];
        const counts = syncCountsRefs?.[platform]?.current;
        acc.scanned += counts?.scanned || 0;
        acc.fresh += counts?.fresh || 0;
      }
      delete runRef.current[platform];
      const acc = accumRef.current[platform] || { scanned: 0, fresh: 0 };
      const failedAll = !run.aborted && !loginError && skipped.length >= steps.length;
      const status = run.aborted ? 'stopped' : loginError || failedAll ? 'error' : 'done';
      patchJob(platform, {
        status,
        error: status === 'error' ? (loginError ? 'login' : 'failed') : null,
        scanned: acc.scanned,
        fresh: acc.fresh,
        skipped,
        currentLabel: null,
        currentCollectionId: null,
        finishedAt: Date.now(),
      });
      // done/stopped jobs leave the map shortly after the Activity log effect
      // has seen the terminal state, so later MANUAL browser syncs fall back to
      // the legacy logging path. Error jobs persist as a live row (with the
      // login CTA) until dismissed or replaced by a new run.
      if (status !== 'error') {
        setTimeout(() => {
          setJobs((prev) => {
            const j = prev[platform];
            if (!j || (j.status !== 'done' && j.status !== 'stopped')) return prev;
            const next = { ...prev };
            delete next[platform];
            return next;
          });
        }, 4000);
      }
    })();
    return true;
  };

  const stop = (platform) => {
    const run = runRef.current[platform];
    if (run) run.aborted = true;
    if (syncingRef.current[platform]) stopSync(platform);
  };

  const dismiss = (platform) =>
    setJobs((prev) => {
      if (!prev[platform] || runRef.current[platform]) return prev;
      const next = { ...prev };
      delete next[platform];
      return next;
    });

  // Imperative API for App (sidebar buttons + Activity actions). Registered
  // once: start/stop/dismiss read only refs and stable setters, and the
  // captured startSync/stopSync are the first-render instances, which — like
  // everywhere else in the Browser hooks — read only refs/stable setters too.
  useEffect(() => {
    registerApi?.({ start, stop, dismiss });
    return () => registerApi?.(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return { jobs, runRef, stop };
}
