import { useEffect, useState } from 'react';

// Shared "grid density" preference for every post surface (Gallery, AI Search,
// Tags Explorer). It's a single integer offset applied to the responsive column
// count: negative → fewer columns / larger cards, positive → more columns /
// smaller cards. The value lives in localStorage and is mirrored to every
// mounted hook via a module-level listener set, so changing it on one surface
// instantly updates the others (and persists across restarts).

const STORAGE_KEY = 'gridSizeStep';
export const MIN_STEP = -3; // largest cards (fewest columns)
export const MAX_STEP = 4; // smallest cards (most columns)

function clampStep(n: number): number {
  if (!Number.isFinite(n)) return 0;
  return Math.max(MIN_STEP, Math.min(MAX_STEP, Math.round(n)));
}

function readInitial(): number {
  if (typeof window === 'undefined') return 0;
  const raw = window.localStorage?.getItem(STORAGE_KEY);
  return raw == null ? 0 : clampStep(Number(raw));
}

let currentStep = readInitial();
const listeners = new Set<(step: number) => void>();

function writeStep(next: number): void {
  const v = clampStep(next);
  if (v === currentStep) return;
  currentStep = v;
  try {
    window.localStorage?.setItem(STORAGE_KEY, String(v));
  } catch {
    /* localStorage unavailable (private mode / tests) — keep in-memory only */
  }
  listeners.forEach((fn) => fn(v));
}

// Apply the shared density offset to a breakpoint-derived column count. Cards
// never drop below 1 column.
export function applyStep(baseCols: number, step: number = currentStep): number {
  return Math.max(1, baseCols + step);
}

// ── Keyboard shortcuts ──────────────────────────────────────────────────────
// Cmd +/- on macOS, Ctrl +/- elsewhere. Installed once (refcounted) regardless
// of how many GridSizeControl instances mount, so a keypress fires a single
// step change. We preventDefault to override Electron's native page zoom.
export const isMac =
  typeof navigator !== 'undefined' &&
  /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent || '');

export const shortcutHint = isMac ? '⌘' : 'Ctrl';

let shortcutRefs = 0;
let keyHandler: ((e: KeyboardEvent) => void) | null = null;

function installShortcuts(): () => void {
  shortcutRefs += 1;
  if (shortcutRefs === 1 && typeof window !== 'undefined') {
    keyHandler = (e: KeyboardEvent) => {
      const mod = isMac ? e.metaKey : e.ctrlKey;
      if (!mod || e.altKey) return;
      // '+'/'=' (and numpad add) → bigger cards; '-'/'_' (and numpad sub) → smaller.
      const k = e.key;
      if (k === '+' || k === '=' || k === 'Add') {
        e.preventDefault();
        writeStep(currentStep - 1);
      } else if (k === '-' || k === '_' || k === 'Subtract') {
        e.preventDefault();
        writeStep(currentStep + 1);
      }
    };
    window.addEventListener('keydown', keyHandler);
  }
  return () => {
    shortcutRefs -= 1;
    if (shortcutRefs <= 0 && keyHandler) {
      window.removeEventListener('keydown', keyHandler);
      keyHandler = null;
      shortcutRefs = 0;
    }
  };
}

// Mount once (e.g. in GridSizeControl) to enable the global keyboard shortcuts.
export function useGridShortcuts(): void {
  useEffect(() => installShortcuts(), []);
}

export interface UseGridSize {
  step: number;
  setStep: (next: number) => void;
  larger: () => void;
  smaller: () => void;
  canEnlarge: boolean;
  canShrink: boolean;
}

export function useGridSize(): UseGridSize {
  const [step, setStep] = useState<number>(currentStep);
  useEffect(() => {
    const fn = (v: number): void => setStep(v);
    listeners.add(fn);
    // Sync in case the module value changed between render and effect.
    if (currentStep !== step) setStep(currentStep);
    return () => {
      listeners.delete(fn);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return {
    step,
    setStep: writeStep,
    larger: () => writeStep(currentStep - 1), // fewer columns → bigger cards
    smaller: () => writeStep(currentStep + 1), // more columns → smaller cards
    canEnlarge: step > MIN_STEP,
    canShrink: step < MAX_STEP,
  };
}
