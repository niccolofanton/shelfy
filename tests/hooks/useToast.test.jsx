import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { useToast } from '../../src/hooks/useToast';

describe('useToast', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('shows a message, plays the exit step, then clears', () => {
    const { result } = renderHook(() => useToast());
    expect(result.current.toast).toBeNull();

    act(() => result.current.showToast('fatto'));
    expect(result.current.toast).toBe('fatto');
    expect(result.current.toastClosing).toBe(false);

    act(() => vi.advanceTimersByTime(3000));
    expect(result.current.toast).toBe('fatto');
    expect(result.current.toastClosing).toBe(true);

    act(() => vi.advanceTimersByTime(200));
    expect(result.current.toast).toBeNull();
    expect(result.current.toastClosing).toBe(false);
  });

  it('re-arms the timer when a new message replaces the current one', () => {
    const { result } = renderHook(() => useToast());
    act(() => result.current.showToast('a'));
    act(() => vi.advanceTimersByTime(2900));
    act(() => result.current.showToast('b'));

    // The old timer must not close the new toast.
    act(() => vi.advanceTimersByTime(2900));
    expect(result.current.toast).toBe('b');
    expect(result.current.toastClosing).toBe(false);

    act(() => vi.advanceTimersByTime(100));
    expect(result.current.toastClosing).toBe(true);
  });

  it('interrupts the exit animation when a new message arrives', () => {
    const { result } = renderHook(() => useToast());
    act(() => result.current.showToast('a'));
    act(() => vi.advanceTimersByTime(3100)); // mid fade-out
    expect(result.current.toastClosing).toBe(true);

    act(() => result.current.showToast('b'));
    expect(result.current.toast).toBe('b');
    expect(result.current.toastClosing).toBe(false);
  });

  it('clears its timers on unmount', () => {
    const { result, unmount } = renderHook(() => useToast());
    act(() => result.current.showToast('a'));
    unmount();
    expect(() => vi.runAllTimers()).not.toThrow();
  });
});
