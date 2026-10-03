import { useState, useCallback, useEffect } from 'react';

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
export function useDownloadPrefs(): UseDownloadPrefs {
  const [prefs, setPrefs] = useState<DownloadPrefs>(read);

  useEffect(() => {
    const sync = (): void => setPrefs(read());
    window.addEventListener('download-prefs-changed', sync);
    window.addEventListener('storage', sync);
    return () => {
      window.removeEventListener('download-prefs-changed', sync);
      window.removeEventListener('storage', sync);
    };
  }, []);

  const setType = useCallback(
    (type: string, value: boolean): void => {
      // The write happens here, synchronously, BEFORE the event below —
      // not inside the setPrefs updater. React may defer invoking a
      // functional updater until its next render pass (it only runs it
      // eagerly, as an internal bail-out optimization, when there is no
      // already-queued update for this state — not guaranteed); dispatching
      // first would let every 'download-prefs-changed' listener's read()
      // (including this hook's own, below) race the write and reapply the
      // pre-toggle value right back over it.
      const next = { ...prefs, [type]: value };
      localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
      setPrefs(next);
      window.dispatchEvent(new Event('download-prefs-changed'));
    },
    [prefs],
  );

  const selectedTypes = useCallback((): string[] => ALL_TYPES.filter((t) => prefs[t]), [prefs]);

  return { prefs, setType, selectedTypes };
}
