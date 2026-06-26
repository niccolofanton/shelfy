import { useEffect, useState } from 'react';

// Shared "view mode" preference for the post surfaces (Gallery and every
// collection / filtered view it backs). Two modes:
//   • 'grid'   — the classic row-virtualized vertical grid (date-ordered).
//   • 'canvas' — the infinite 2D canvas: free pan + zoom, content tiled
//     infinitely, with date ordering intentionally inactive.
//
// Like the grid-density preference (see useGridSize) the value lives in
// localStorage and is mirrored to every mounted hook via a module-level listener
// set, so flipping the toolbar's view toggle on one kept-alive tab updates the
// others instantly and the choice survives a restart.

export type ViewMode = 'grid' | 'canvas';

const STORAGE_KEY = 'galleryViewMode';

function clampMode(v: string | null | undefined): ViewMode {
  return v === 'canvas' ? 'canvas' : 'grid';
}

function readInitial(): ViewMode {
  if (typeof window === 'undefined') return 'grid';
  return clampMode(window.localStorage?.getItem(STORAGE_KEY));
}

let currentMode: ViewMode = readInitial();
const listeners = new Set<(mode: ViewMode) => void>();

function writeMode(next: ViewMode): void {
  const v = clampMode(next);
  if (v === currentMode) return;
  currentMode = v;
  try {
    window.localStorage?.setItem(STORAGE_KEY, v);
  } catch {
    /* localStorage unavailable (private mode / tests) — keep in-memory only */
  }
  listeners.forEach((fn) => fn(v));
}

export interface UseViewMode {
  mode: ViewMode;
  setMode: (next: ViewMode) => void;
  toggle: () => void;
}

export function useViewMode(): UseViewMode {
  const [mode, setMode] = useState<ViewMode>(currentMode);
  useEffect(() => {
    const fn = (v: ViewMode): void => setMode(v);
    listeners.add(fn);
    // Sync in case the module value changed between render and effect.
    if (currentMode !== mode) setMode(currentMode);
    return () => {
      listeners.delete(fn);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return {
    mode,
    setMode: writeMode,
    toggle: () => writeMode(currentMode === 'canvas' ? 'grid' : 'canvas'),
  };
}
