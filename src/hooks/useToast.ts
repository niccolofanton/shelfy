import { useState, useEffect, useCallback, useRef } from 'react';
import type React from 'react';

// Two APIs live here:
//
// - useToasts() + <ToastHost> (src/components/ui/Toast.tsx), the shared toast
//   system (UX audit §3.6, GAL-9). New code uses this one.
// - useToast(), the original single-message state that the AI views render
//   themselves. Kept as is until those views move to useToasts().

// ─── useToasts ────────────────────────────────────────────────────────────────
//
//   const toasts = useToasts();
//   toasts.show(t('saved'), { variant: 'success' });
//   toasts.show(t('deleted', { count }), { action: { label: t('undo'), onClick: undo } });
//   toasts.show(t('jobRunning'), { id: 'job', variant: 'progress', duration: null });
//   …
//   <ToastHost toasts={toasts} />          // once per view
//
// show(message, options?) returns the toast's id. Options:
//   id?:       reuse an id to replace that toast in place (progress updates)
//   variant?:  'neutral' (default) | 'success' | 'error' | 'progress' (spinner)
//   action?:   { label, onClick, testId? } — one button, such as Undo; the
//              toast is dismissed after it runs
//   duration?: ms on screen: 4000 by default, 8000 with an action; null keeps
//              it until dismiss(id) or a replacement (progress)
//   testId?:   data-testid on the toast
// At most 3 show at once: a fourth pushes out the oldest. The timer pauses
// while the pointer or focus is on a toast.

export type ToastVariant = 'neutral' | 'success' | 'error' | 'progress';

export interface ToastAction {
  label: string;
  onClick: () => void;
  testId?: string;
}

export interface ToastOptions {
  id?: string;
  variant?: ToastVariant;
  action?: ToastAction;
  duration?: number | null;
  testId?: string;
}

export interface ToastItem {
  id: string;
  message: React.ReactNode;
  variant: ToastVariant;
  action?: ToastAction;
  testId?: string;
  // Playing its exit animation; it leaves the DOM TOAST_EXIT_MS later.
  closing: boolean;
}

export interface UseToasts {
  toasts: ToastItem[];
  show: (message: React.ReactNode, options?: ToastOptions) => string;
  dismiss: (id: string) => void;
  pause: (id: string) => void;
  resume: (id: string) => void;
}

export const TOAST_MAX = 3;
export const TOAST_DURATION_MS = 4000;
export const TOAST_ACTION_DURATION_MS = 8000;
// Both APIs: how long the exit animation plays before a toast leaves the DOM.
const TOAST_EXIT_MS = 200;

interface Timer {
  handle: ReturnType<typeof setTimeout> | null;
  remaining: number | null;
  startedAt: number;
}

let nextToastId = 0;

export function useToasts(): UseToasts {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  // The list's source of truth, so show() knows synchronously which toast a
  // fourth one pushes out.
  const list = useRef<ToastItem[]>([]);
  const timers = useRef(new Map<string, Timer>());
  const exits = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  const update = useCallback((next: ToastItem[]) => {
    list.current = next;
    setToasts(next);
  }, []);

  const clearTimers = useCallback((id: string) => {
    const timer = timers.current.get(id);
    if (timer?.handle) clearTimeout(timer.handle);
    timers.current.delete(id);
    const exit = exits.current.get(id);
    if (exit) clearTimeout(exit);
    exits.current.delete(id);
  }, []);

  const dismiss = useCallback(
    (id: string) => {
      if (exits.current.has(id) || !list.current.some((t) => t.id === id)) return;
      clearTimers(id);
      update(list.current.map((t) => (t.id === id ? { ...t, closing: true } : t)));
      exits.current.set(
        id,
        setTimeout(() => {
          exits.current.delete(id);
          update(list.current.filter((t) => t.id !== id));
        }, TOAST_EXIT_MS),
      );
    },
    [clearTimers, update],
  );

  const show = useCallback(
    (message: React.ReactNode, options: ToastOptions = {}): string => {
      const id = options.id ?? `toast-${++nextToastId}`;
      const duration =
        options.duration === undefined
          ? options.action
            ? TOAST_ACTION_DURATION_MS
            : TOAST_DURATION_MS
          : options.duration;
      clearTimers(id);
      const item: ToastItem = {
        id,
        message,
        variant: options.variant ?? 'neutral',
        action: options.action,
        testId: options.testId,
        closing: false,
      };
      if (list.current.some((t) => t.id === id)) {
        update(list.current.map((t) => (t.id === id ? item : t)));
      } else {
        const next = [...list.current, item];
        for (const old of next.slice(0, Math.max(0, next.length - TOAST_MAX))) {
          clearTimers(old.id);
        }
        update(next.slice(-TOAST_MAX));
      }
      const timer: Timer = { handle: null, remaining: duration, startedAt: Date.now() };
      if (duration != null) timer.handle = setTimeout(() => dismiss(id), duration);
      timers.current.set(id, timer);
      return id;
    },
    [clearTimers, update, dismiss],
  );

  const pause = useCallback((id: string) => {
    const timer = timers.current.get(id);
    if (!timer || timer.handle == null || timer.remaining == null) return;
    clearTimeout(timer.handle);
    timer.handle = null;
    timer.remaining = Math.max(0, timer.remaining - (Date.now() - timer.startedAt));
  }, []);

  const resume = useCallback(
    (id: string) => {
      const timer = timers.current.get(id);
      if (!timer || timer.handle != null || timer.remaining == null) return;
      timer.startedAt = Date.now();
      timer.handle = setTimeout(() => dismiss(id), timer.remaining);
    },
    [dismiss],
  );

  useEffect(() => {
    const allTimers = timers.current;
    const allExits = exits.current;
    return () => {
      for (const timer of allTimers.values()) if (timer.handle) clearTimeout(timer.handle);
      for (const exit of allExits.values()) clearTimeout(exit);
    };
  }, []);

  return { toasts, show, dismiss, pause, resume };
}

// ─── useToast (legacy) ────────────────────────────────────────────────────────

// Transient feedback state shared by the views that show a self-dismissing
// toast (AI Tags, AI Search; the Gallery's bulk-action feedback can adopt it
// as-is). `showToast` (re)arms the display timer; before unmounting, the toast
// gets one motion step with `toastClosing: true` so it can play its fade-out.
// Views that don't animate the exit can simply ignore `toastClosing`.
const TOAST_LEGACY_DURATION_MS = 3000;

export interface UseToast {
  toast: string | null;
  toastClosing: boolean;
  showToast: (msg: string) => void;
}

export function useToast(): UseToast {
  const [toast, setToast] = useState<string | null>(null);
  const [toastClosing, setToastClosing] = useState<boolean>(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const exitTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const showToast = useCallback((msg: string) => {
    if (timer.current) clearTimeout(timer.current);
    if (exitTimer.current) clearTimeout(exitTimer.current);
    setToastClosing(false);
    setToast(msg);
    timer.current = setTimeout(() => {
      setToastClosing(true);
      exitTimer.current = setTimeout(() => {
        setToast(null);
        setToastClosing(false);
      }, TOAST_EXIT_MS);
    }, TOAST_LEGACY_DURATION_MS);
  }, []);

  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
      if (exitTimer.current) clearTimeout(exitTimer.current);
    },
    [],
  );

  return { toast, toastClosing, showToast };
}
