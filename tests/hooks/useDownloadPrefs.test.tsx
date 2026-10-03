import React from 'react';
import { describe, it, expect, beforeEach } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { useDownloadPrefs } from '../../src/hooks/useDownloadPrefs';

const STORAGE_KEY = 'download:assetTypes';

function stored(): unknown {
  const raw = localStorage.getItem(STORAGE_KEY);
  return raw ? JSON.parse(raw) : null;
}

describe('useDownloadPrefs', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('defaults every asset type to enabled', () => {
    const { result } = renderHook(() => useDownloadPrefs());
    expect(result.current.prefs).toEqual({ thumbnail: true, image: true, video: true });
    expect(result.current.selectedTypes().sort()).toEqual(['image', 'thumbnail', 'video']);
  });

  it('setType flips one type and persists it', () => {
    const { result } = renderHook(() => useDownloadPrefs());
    act(() => result.current.setType('video', false));
    expect(result.current.prefs).toEqual({ thumbnail: true, image: true, video: false });
    expect(stored()).toEqual({ thumbnail: true, image: true, video: false });
  });

  // P1-08's regression (found while code-splitting Settings into its own chunk,
  // which shifted timing enough to lose this reliably): the old hook wrote
  // localStorage inside setType, then dispatched 'download-prefs-changed' right
  // after. A listener reacting to that event — including this hook's own
  // instance — could read localStorage before the write had actually landed.
  it('setType persists before dispatching: a listener reading localStorage mid-call sees the new value, not the stale one', () => {
    const { result } = renderHook(() => useDownloadPrefs());
    const seen: Array<string | null> = [];
    const listener = () => seen.push(localStorage.getItem(STORAGE_KEY));
    window.addEventListener('download-prefs-changed', listener);

    act(() => result.current.setType('video', false));

    window.removeEventListener('download-prefs-changed', listener);
    expect(seen).toHaveLength(1);
    expect(JSON.parse(seen[0]!)).toMatchObject({ video: false });
    expect(result.current.prefs.video).toBe(false);
  });

  // The regression this guards: the old hook wrote localStorage as a side
  // effect of its setState updater, so a second toggle queued before React
  // flushed the first could be dropped from the hook's own `prefs` — even
  // though, by coincidence, localStorage could still end up correct. Toggling
  // two different types inside one `act()` reproduces "before React
  // re-renders" without depending on real timers.
  it('keeps both changes when two types are toggled in quick succession', () => {
    const { result } = renderHook(() => useDownloadPrefs());
    act(() => {
      result.current.setType('thumbnail', false);
      result.current.setType('video', false);
    });
    expect(result.current.prefs).toEqual({ thumbnail: false, image: true, video: false });
    expect(stored()).toEqual({ thumbnail: false, image: true, video: false });
  });

  it('survives React StrictMode double-invoking the updater', () => {
    const { result } = renderHook(() => useDownloadPrefs(), {
      wrapper: ({ children }) => <React.StrictMode>{children}</React.StrictMode>,
    });
    act(() => {
      result.current.setType('thumbnail', false);
      result.current.setType('video', false);
    });
    expect(result.current.prefs).toEqual({ thumbnail: false, image: true, video: false });
    expect(stored()).toEqual({ thumbnail: false, image: true, video: false });
  });

  it('a toggle survives three back-to-back calls, in order (thumbnail, image, video)', () => {
    const { result } = renderHook(() => useDownloadPrefs());
    act(() => result.current.setType('thumbnail', false));
    act(() => result.current.setType('image', false));
    act(() => result.current.setType('video', false));
    expect(result.current.prefs).toEqual({ thumbnail: false, image: false, video: false });
    expect(result.current.selectedTypes()).toEqual([]);
  });

  it('persists across a remount', () => {
    const { result, unmount } = renderHook(() => useDownloadPrefs());
    act(() => result.current.setType('image', false));
    unmount();

    const { result: reopened } = renderHook(() => useDownloadPrefs());
    expect(reopened.current.prefs).toEqual({ thumbnail: true, image: false, video: true });
    expect(reopened.current.selectedTypes().sort()).toEqual(['thumbnail', 'video']);
  });

  it('keeps two mounted instances (e.g. Settings and Downloads) in sync via the shared event', () => {
    const { result: a } = renderHook(() => useDownloadPrefs());
    const { result: b } = renderHook(() => useDownloadPrefs());

    act(() => a.current.setType('thumbnail', false));

    expect(a.current.prefs.thumbnail).toBe(false);
    expect(b.current.prefs).toEqual({ thumbnail: false, image: true, video: true });
  });
});
