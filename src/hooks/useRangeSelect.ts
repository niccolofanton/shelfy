import { useState, useRef, useCallback } from 'react';

// Shared multi-select with Shift+click range support (Gallery grid, Websites
// list). `items` is the currently rendered, ordered list and `getId` maps an
// item to the id stored in the selection Set — return a nullish id to skip an
// entry (e.g. live-only web jobs with no persisted post row yet). Both are
// read through refs so every returned callback is stable across renders and
// doesn't defeat React.memo'd children.
//
// Semantics (unchanged from the previous per-view copies):
// - Plain click toggles the id and moves the range anchor to that index.
// - Shift+click selects the whole anchor→index range inclusive (additive — it
//   never deselects) and moves the anchor. With no anchor yet (or an
//   unresolvable index) it falls back to a plain toggle.
// - The anchor is an index into `items`, so callers must resetAnchor()
//   whenever the array is refetched/reordered (a stored index would point
//   into the old array), and clearSelection() when the result set changes.

export interface UseRangeSelect<Id> {
  selected: Set<Id>;
  setSelected: React.Dispatch<React.SetStateAction<Set<Id>>>;
  toggleSelected: (id: Id) => void;
  selectRange: (toIndex: number) => boolean;
  toggleAt: (id: Id, index: number, shiftKey: boolean) => void;
  resetAnchor: () => void;
  clearSelection: () => void;
  // "Select all matching" (Gallery, Trash — a filtered view that can hold far
  // more posts than are loaded at once, P1-14). While true, `selected` holds
  // the ids explicitly EXCLUDED from "every post the view matches", instead
  // of the ids included — the toggle/range primitives above don't change at
  // all (they only ever add/remove a Set member), only what membership MEANS
  // does. Callers that never turn it on keep the original semantics exactly.
  selectAllMatching: boolean;
  setSelectAllMatching: React.Dispatch<React.SetStateAction<boolean>>;
  // A loaded item's checked state, honouring selectAllMatching either way.
  isSelected: (id: Id) => boolean;
}

export function useRangeSelect<T, Id>(
  items: T[],
  getId: (item: T) => Id | null | undefined,
): UseRangeSelect<Id> {
  const [selected, setSelected] = useState<Set<Id>>(() => new Set<Id>());
  const [selectAllMatching, setSelectAllMatching] = useState<boolean>(false);
  // Index of the last plain-toggled item — the Shift+click range anchor.
  const lastIndexRef = useRef<number | null>(null);
  const itemsRef = useRef<T[]>(items);
  itemsRef.current = items;
  const getIdRef = useRef<(item: T) => Id | null | undefined>(getId);
  getIdRef.current = getId;

  const toggleSelected = useCallback((id: Id) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  // Select every item between the anchor and `toIndex` (inclusive). Returns
  // false when there's no anchor, so the caller can fall back to a plain toggle.
  const selectRange = useCallback((toIndex: number): boolean => {
    const from = lastIndexRef.current;
    if (from == null) return false;
    const [lo, hi] = from <= toIndex ? [from, toIndex] : [toIndex, from];
    setSelected((prev) => {
      const next = new Set(prev);
      const list = itemsRef.current;
      for (let i = lo; i <= hi; i++) {
        const item = list[i];
        const id = item != null ? getIdRef.current(item) : null;
        if (id != null) next.add(id);
      }
      return next;
    });
    return true;
  }, []);

  // The click-handler body shared by every view: range on Shift (when an
  // anchor exists and the index resolved), plain toggle otherwise. `index` may
  // be -1 when the clicked item isn't in `items` (the toggle still applies,
  // and the stale anchor is overwritten).
  const toggleAt = useCallback(
    (id: Id, index: number, shiftKey: boolean) => {
      if (shiftKey && index >= 0 && selectRange(index)) {
        lastIndexRef.current = index;
        return;
      }
      toggleSelected(id);
      lastIndexRef.current = index;
    },
    [selectRange, toggleSelected],
  );

  // Drop the range anchor only — for when the items array changes shape but
  // the selected ids remain valid (e.g. re-sorts, refetches mid-selection).
  const resetAnchor = useCallback(() => {
    lastIndexRef.current = null;
  }, []);

  // Drop both the selection and the anchor (the result set itself changed,
  // or selection mode was exited) — and exit "select all matching" too: a
  // changed result set invalidates what it was matching against.
  const clearSelection = useCallback(() => {
    setSelected(new Set<Id>());
    setSelectAllMatching(false);
    lastIndexRef.current = null;
  }, []);

  const isSelected = (id: Id): boolean =>
    selectAllMatching ? !selected.has(id) : selected.has(id);

  return {
    selected,
    setSelected,
    toggleSelected,
    selectRange,
    toggleAt,
    resetAnchor,
    clearSelection,
    selectAllMatching,
    setSelectAllMatching,
    isSelected,
  };
}
