import { describe, it, expect } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { useRangeSelect } from '../../src/hooks/useRangeSelect.js';

// Five-item list mirroring the Gallery shape (id) — the getId mapper adapts it.
const ITEMS = [{ id: 'a' }, { id: 'b' }, { id: 'c' }, { id: 'd' }, { id: 'e' }];
const getId = (p) => p.id;

function setup(items = ITEMS) {
  return renderHook(({ list }) => useRangeSelect(list, getId), {
    initialProps: { list: items },
  });
}

// ─── 1. Plain click (toggle) ─────────────────────────────────────────────────

describe('useRangeSelect — plain toggle', () => {
  it('selects an unselected id and deselects it on the second click', () => {
    const { result } = setup();

    act(() => result.current.toggleAt('b', 1, false));
    expect([...result.current.selected]).toEqual(['b']);

    act(() => result.current.toggleAt('b', 1, false));
    expect(result.current.selected.size).toBe(0);
  });

  it('toggles independently across different ids', () => {
    const { result } = setup();

    act(() => result.current.toggleAt('a', 0, false));
    act(() => result.current.toggleAt('d', 3, false));
    expect([...result.current.selected].sort()).toEqual(['a', 'd']);

    act(() => result.current.toggleAt('a', 0, false));
    expect([...result.current.selected]).toEqual(['d']);
  });

  it('still toggles when the index is unresolved (-1)', () => {
    const { result } = setup();

    act(() => result.current.toggleAt('ghost', -1, false));
    expect(result.current.selected.has('ghost')).toBe(true);
  });
});

// ─── 2. Shift+click range ────────────────────────────────────────────────────

describe('useRangeSelect — shift range', () => {
  it('selects the inclusive anchor→index range after a plain click', () => {
    const { result } = setup();

    act(() => result.current.toggleAt('b', 1, false)); // anchor = 1
    act(() => result.current.toggleAt('d', 3, true)); // range 1..3
    expect([...result.current.selected].sort()).toEqual(['b', 'c', 'd']);
  });

  it('works backwards (clicking above the anchor)', () => {
    const { result } = setup();

    act(() => result.current.toggleAt('d', 3, false)); // anchor = 3
    act(() => result.current.toggleAt('b', 1, true)); // range 1..3
    expect([...result.current.selected].sort()).toEqual(['b', 'c', 'd']);
  });

  it('is additive: a range never removes already-selected ids', () => {
    const { result } = setup();

    act(() => result.current.toggleAt('e', 4, false));
    act(() => result.current.toggleAt('a', 0, false)); // anchor = 0
    act(() => result.current.toggleAt('b', 1, true)); // range 0..1
    expect([...result.current.selected].sort()).toEqual(['a', 'b', 'e']);
  });

  it('moves the anchor to the shift-clicked index for chained ranges', () => {
    const { result } = setup();

    act(() => result.current.toggleAt('a', 0, false)); // anchor = 0
    act(() => result.current.toggleAt('b', 1, true)); // range 0..1, anchor = 1
    act(() => result.current.toggleAt('d', 3, true)); // range 1..3
    expect([...result.current.selected].sort()).toEqual(['a', 'b', 'c', 'd']);
  });

  it('falls back to a plain toggle when there is no anchor yet', () => {
    const { result } = setup();

    act(() => result.current.toggleAt('c', 2, true));
    expect([...result.current.selected]).toEqual(['c']);

    // The fallback set the anchor, so the next shift-click ranges from it.
    act(() => result.current.toggleAt('e', 4, true));
    expect([...result.current.selected].sort()).toEqual(['c', 'd', 'e']);
  });

  it('falls back to a plain toggle when the index is unresolved (-1)', () => {
    const { result } = setup();

    act(() => result.current.toggleAt('a', 0, false));
    act(() => result.current.toggleAt('ghost', -1, true));
    expect([...result.current.selected].sort()).toEqual(['a', 'ghost']);
  });

  it('skips entries whose getId is nullish (live-only rows without a postId)', () => {
    const items = [{ id: 'a' }, { id: null }, { id: 'c' }];
    const { result } = setup(items);

    act(() => result.current.toggleAt('a', 0, false));
    act(() => result.current.toggleAt('c', 2, true));
    expect([...result.current.selected].sort()).toEqual(['a', 'c']);
  });

  it('reads the latest items array after a rerender (no stale closure)', () => {
    const { result, rerender } = setup();

    act(() => result.current.toggleAt('a', 0, false)); // anchor = 0
    rerender({ list: [{ id: 'x' }, { id: 'y' }, { id: 'z' }] });

    act(() => result.current.toggleAt('z', 2, true)); // range over the NEW list
    expect([...result.current.selected].sort()).toEqual(['a', 'x', 'y', 'z']);
  });
});

// ─── 3. Resets ───────────────────────────────────────────────────────────────

describe('useRangeSelect — resets', () => {
  it('resetAnchor drops only the anchor; the selection survives', () => {
    const { result } = setup();

    act(() => result.current.toggleAt('b', 1, false));
    act(() => result.current.resetAnchor());

    expect([...result.current.selected]).toEqual(['b']);
    // No anchor → shift-click degrades to a plain toggle, not a range.
    act(() => result.current.toggleAt('e', 4, true));
    expect([...result.current.selected].sort()).toEqual(['b', 'e']);
  });

  it('clearSelection empties the set and drops the anchor', () => {
    const { result } = setup();

    act(() => result.current.toggleAt('a', 0, false));
    act(() => result.current.toggleAt('c', 2, true));
    act(() => result.current.clearSelection());

    expect(result.current.selected.size).toBe(0);
    act(() => result.current.toggleAt('d', 3, true)); // no anchor → toggle
    expect([...result.current.selected]).toEqual(['d']);
  });

  it('setSelected replaces the whole set (select-all flow)', () => {
    const { result } = setup();

    act(() => result.current.setSelected(new Set(['a', 'b', 'c', 'd', 'e'])));
    expect(result.current.selected.size).toBe(5);

    act(() => result.current.setSelected(new Set()));
    expect(result.current.selected.size).toBe(0);
  });
});
