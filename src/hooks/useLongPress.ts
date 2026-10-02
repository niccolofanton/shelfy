import { useCallback, useRef } from 'react';

// Long-press to select (P1-02, touch only): a mouse keeps hover + click + the
// existing quick-select checkbox (PostCard); a touch/pen pointer held in place
// for DELAY_MS fires `onLongPress` instead. Moving past MOVE_TOLERANCE_PX
// before the timer fires cancels it — a scroll/pan in progress is a different
// gesture than holding still, and must never flip into a selection mid-swipe.
const DELAY_MS = 500;
const MOVE_TOLERANCE_PX = 10;

export interface LongPressHandlers {
  onPointerDown: (e: React.PointerEvent) => void;
  onPointerMove: (e: React.PointerEvent) => void;
  onPointerUp: (e: React.PointerEvent) => void;
  onPointerCancel: (e: React.PointerEvent) => void;
}

export interface LongPressResult extends LongPressHandlers {
  // True exactly once, right after a long-press fired for the gesture
  // currently ending — the browser still dispatches a trailing `click` after
  // the finger lifts, which the caller should swallow (reading this from its
  // click handler) so a long-press-select never ALSO opens the item.
  consumeFired: () => boolean;
}

export function useLongPress(
  onLongPress: (event: React.PointerEvent) => void,
  delayMs: number = DELAY_MS,
): LongPressResult {
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const originRef = useRef<{ x: number; y: number } | null>(null);
  const firedRef = useRef<boolean>(false);
  // Latest callback without re-subscribing the pointer handlers on every render.
  const onLongPressRef = useRef(onLongPress);
  onLongPressRef.current = onLongPress;

  const clearTimer = useCallback((): void => {
    if (timerRef.current != null) clearTimeout(timerRef.current);
    timerRef.current = null;
    originRef.current = null;
  }, []);

  const onPointerDown = useCallback(
    (e: React.PointerEvent): void => {
      // Mouse/other pointers keep today's desktop behavior untouched.
      if (e.pointerType !== 'touch' && e.pointerType !== 'pen') return;
      clearTimer();
      originRef.current = { x: e.clientX, y: e.clientY };
      timerRef.current = setTimeout(() => {
        timerRef.current = null;
        firedRef.current = true;
        onLongPressRef.current(e);
      }, delayMs);
    },
    [clearTimer, delayMs],
  );

  const onPointerMove = useCallback(
    (e: React.PointerEvent): void => {
      const origin = originRef.current;
      if (!origin) return;
      const dx = Math.abs(e.clientX - origin.x);
      const dy = Math.abs(e.clientY - origin.y);
      if (dx > MOVE_TOLERANCE_PX || dy > MOVE_TOLERANCE_PX) clearTimer();
    },
    [clearTimer],
  );

  const onPointerUp = useCallback((): void => clearTimer(), [clearTimer]);
  const onPointerCancel = useCallback((): void => clearTimer(), [clearTimer]);

  const consumeFired = useCallback((): boolean => {
    const was = firedRef.current;
    firedRef.current = false;
    return was;
  }, []);

  return { onPointerDown, onPointerMove, onPointerUp, onPointerCancel, consumeFired };
}
