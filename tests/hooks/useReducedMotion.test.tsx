// useReducedMotion() (UX audit §3.2) follows prefers-reduced-motion live.
import { renderHook, act } from '@testing-library/react';
import { describe, it, expect, afterEach } from 'vitest';
import { useReducedMotion, REDUCED_MOTION_QUERY } from '../../src/hooks/useReducedMotion';

type Listener = () => void;

function mockMatchMedia(initial: boolean): { set: (v: boolean) => void } {
  let matches = initial;
  const listeners = new Set<Listener>();
  window.matchMedia = ((query: string) => ({
    get matches() {
      return query === REDUCED_MOTION_QUERY ? matches : false;
    },
    media: query,
    addEventListener: (_: string, fn: Listener) => listeners.add(fn),
    removeEventListener: (_: string, fn: Listener) => listeners.delete(fn),
  })) as unknown as typeof window.matchMedia;
  return {
    set(v: boolean) {
      matches = v;
      listeners.forEach((fn) => fn());
    },
  };
}

describe('useReducedMotion', () => {
  const original = window.matchMedia;
  afterEach(() => {
    window.matchMedia = original;
  });

  it('is false where matchMedia is missing', () => {
    // jsdom has none.
    (window as { matchMedia?: unknown }).matchMedia = undefined;
    const { result } = renderHook(() => useReducedMotion());
    expect(result.current).toBe(false);
  });

  it('reads the setting and follows its changes', () => {
    const media = mockMatchMedia(true);
    const { result } = renderHook(() => useReducedMotion());
    expect(result.current).toBe(true);
    act(() => media.set(false));
    expect(result.current).toBe(false);
  });
});
