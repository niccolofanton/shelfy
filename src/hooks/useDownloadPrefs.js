import { useState, useCallback, useEffect } from 'react';

const STORAGE_KEY = 'download:assetTypes';
const ALL_TYPES = ['thumbnail', 'image', 'video'];
const DEFAULTS = { thumbnail: true, image: true, video: true };

function read() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? { ...DEFAULTS, ...JSON.parse(raw) } : { ...DEFAULTS };
  } catch {
    return { ...DEFAULTS };
  }
}

// Asset-type download preferences, persisted to localStorage and shared between
// the Settings editor and the Downloads view. A custom event keeps mounted
// instances in sync within the same window (the native `storage` event only
// fires in other tabs/windows).
export function useDownloadPrefs() {
  const [prefs, setPrefs] = useState(read);

  useEffect(() => {
    const sync = () => setPrefs(read());
    window.addEventListener('download-prefs-changed', sync);
    window.addEventListener('storage', sync);
    return () => {
      window.removeEventListener('download-prefs-changed', sync);
      window.removeEventListener('storage', sync);
    };
  }, []);

  const setType = useCallback((type, value) => {
    setPrefs((prev) => {
      const next = { ...prev, [type]: value };
      localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
      return next;
    });
    window.dispatchEvent(new Event('download-prefs-changed'));
  }, []);

  const selectedTypes = useCallback(() => ALL_TYPES.filter(t => prefs[t]), [prefs]);

  return { prefs, setType, selectedTypes };
}
