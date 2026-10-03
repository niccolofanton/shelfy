import { useState, useCallback, useEffect, useRef } from 'react';

const STORAGE_KEY = 'download:assetTypes';

// Per-type on/off map persisted to localStorage. Keyed by asset-type string so
// the Settings editor (which iterates its own DOWNLOAD_TYPES list) and the
// Downloads view stay decoupled from a fixed key union.
type DownloadPrefs = Record<string, boolean>;

const ALL_TYPES = ['thumbnail', 'image', 'video'] as const;
const DEFAULTS: DownloadPrefs = { thumbnail: true, image: true, video: true };

function read(): DownloadPrefs {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? { ...DEFAULTS, ...(JSON.parse(raw) as DownloadPrefs) } : { ...DEFAULTS };
  } catch {
    return { ...DEFAULTS };
  }
}

export interface UseDownloadPrefs {
  prefs: DownloadPrefs;
  setType: (type: string, value: boolean) => void;
  selectedTypes: () => string[];
}

// Asset-type download preferences, persisted to localStorage and shared between
// the Settings editor and the Downloads view. A custom event keeps mounted
// instances in sync within the same window (the native `storage` event only
// fires in other tabs/windows).
//
// The state updater below is pure — no localStorage access inside it. Two
// toggles fired before React re-renders (e.g. two checkboxes clicked in quick
// succession) must thread through the same setState queue and both land in the
// next `prefs`; a side effect inside the updater risked that, because the
// updater can run more than once for one commit (StrictMode double-invokes it)
// and because `sync` below used to call the non-functional `setPrefs(read())`,
// which reads a stale value when it races a just-queued, not-yet-applied
// toggle. Persisting happens in its own effect, after `prefs` actually changes.
// A ref of the last-persisted JSON stops that effect from re-triggering itself
// through the event it dispatches (dispatch → this hook's own `sync` listener
// → setPrefs → effect again).
export function useDownloadPrefs(): UseDownloadPrefs {
  const [prefs, setPrefs] = useState<DownloadPrefs>(read);
  const lastPersisted = useRef<string>(JSON.stringify(prefs));

  useEffect(() => {
    const sync = (): void => {
      const fresh = read();
      lastPersisted.current = JSON.stringify(fresh);
      setPrefs(fresh);
    };
    window.addEventListener('download-prefs-changed', sync);
    window.addEventListener('storage', sync);
    return () => {
      window.removeEventListener('download-prefs-changed', sync);
      window.removeEventListener('storage', sync);
    };
  }, []);

  useEffect(() => {
    const json = JSON.stringify(prefs);
    if (json === lastPersisted.current) return; // already on disk (mount, or echoed from `sync`)
    lastPersisted.current = json;
    localStorage.setItem(STORAGE_KEY, json);
    window.dispatchEvent(new Event('download-prefs-changed'));
  }, [prefs]);

  const setType = useCallback((type: string, value: boolean): void => {
    setPrefs((prev) => ({ ...prev, [type]: value }));
  }, []);

  const selectedTypes = useCallback((): string[] => ALL_TYPES.filter((t) => prefs[t]), [prefs]);

  return { prefs, setType, selectedTypes };
}
