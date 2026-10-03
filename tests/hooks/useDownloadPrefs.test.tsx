import { describe, it, expect, beforeEach } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { useDownloadPrefs } from '../../src/hooks/useDownloadPrefs';

const STORAGE_KEY = 'download:assetTypes';

// setType must write localStorage synchronously, BEFORE it dispatches
// 'download-prefs-changed' — found while splitting Settings into its own
// chunk (P1-08): once that landed, a second mounted instance's listener
// (itself included — see below) sometimes re-read localStorage before the
// write had landed and reapplied the stale value right back over the
// toggle, because the write used to happen inside the setPrefs updater,
// whose timing React does not guarantee (it only runs a functional updater
// eagerly, as an internal bail-out check, when no update is already queued).
describe('useDownloadPrefs', () => {
  beforeEach(() => {
    localStorage.removeItem(STORAGE_KEY);
  });

  it('defaults every type to enabled with nothing stored', () => {
    const { result } = renderHook(() => useDownloadPrefs());
    expect(result.current.prefs).toEqual({ thumbnail: true, image: true, video: true });
    expect(result.current.selectedTypes()).toEqual(['thumbnail', 'image', 'video']);
  });

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

  it('a toggle survives three back-to-back calls, in order (thumbnail, image, video)', () => {
    const { result } = renderHook(() => useDownloadPrefs());
    act(() => result.current.setType('thumbnail', false));
    act(() => result.current.setType('image', false));
    act(() => result.current.setType('video', false));
    expect(result.current.prefs).toEqual({ thumbnail: false, image: false, video: false });
    expect(result.current.selectedTypes()).toEqual([]);
  });

  it('keeps two mounted instances (e.g. Settings and Downloads) in sync via the shared event', () => {
    const a = renderHook(() => useDownloadPrefs());
    const b = renderHook(() => useDownloadPrefs());

    act(() => a.result.current.setType('image', false));

    expect(a.result.current.prefs.image).toBe(false);
    expect(b.result.current.prefs.image).toBe(false);
  });
});
