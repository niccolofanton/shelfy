import { useEffect, useState } from 'react';

// Persisted toggle for the AI search-suggestion chips (the local LLM that
// proposes related filter tags under the search bar). Spinning an LLM on every
// search is costly, so this defaults to OFF — users opt in from Settings.
//
// Same module-level store + listeners pattern as useViewMode, so the Gallery and
// the Settings toggle stay in sync within the window without prop-drilling.
const STORAGE_KEY = 'aiSearchSuggestions';

function readInitial(): boolean {
  if (typeof window === 'undefined') return false;
  try {
    // Default OFF: only the explicit string 'true' enables it.
    return window.localStorage?.getItem(STORAGE_KEY) === 'true';
  } catch {
    return false;
  }
}

let current = readInitial();
const listeners = new Set<(v: boolean) => void>();

function write(next: boolean): void {
  if (next === current) return;
  current = next;
  try {
    window.localStorage?.setItem(STORAGE_KEY, next ? 'true' : 'false');
  } catch {
    /* localStorage unavailable — keep in-memory only */
  }
  listeners.forEach((fn) => fn(next));
}

export interface UseAiSuggestions {
  enabled: boolean;
  setEnabled: (next: boolean) => void;
  toggle: () => void;
}

export function useAiSuggestions(): UseAiSuggestions {
  const [enabled, setEnabled] = useState<boolean>(current);
  useEffect(() => {
    const fn = (v: boolean): void => setEnabled(v);
    listeners.add(fn);
    if (current !== enabled) setEnabled(current);
    return () => {
      listeners.delete(fn);
    };
  }, []);

  return {
    enabled,
    setEnabled: write,
    toggle: () => write(!current),
  };
}
