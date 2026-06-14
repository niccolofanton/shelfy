import { useState, useEffect, useCallback, useRef } from 'react';

// Transient feedback state shared by the views that show a self-dismissing
// toast (AI Tags, AI Search; the Gallery's bulk-action feedback can adopt it
// as-is). `showToast` (re)arms the display timer; before unmounting, the toast
// gets one motion step with `toastClosing: true` so it can play its fade-out.
// Views that don't animate the exit can simply ignore `toastClosing`.
const TOAST_DURATION_MS = 3000;
const TOAST_EXIT_MS = 200;

export function useToast() {
  const [toast, setToast] = useState(null);
  const [toastClosing, setToastClosing] = useState(false);
  const timer = useRef(null);
  const exitTimer = useRef(null);

  const showToast = useCallback((msg) => {
    clearTimeout(timer.current);
    clearTimeout(exitTimer.current);
    setToastClosing(false);
    setToast(msg);
    timer.current = setTimeout(() => {
      setToastClosing(true);
      exitTimer.current = setTimeout(() => {
        setToast(null);
        setToastClosing(false);
      }, TOAST_EXIT_MS);
    }, TOAST_DURATION_MS);
  }, []);

  useEffect(
    () => () => {
      clearTimeout(timer.current);
      clearTimeout(exitTimer.current);
    },
    [],
  );

  return { toast, toastClosing, showToast };
}
