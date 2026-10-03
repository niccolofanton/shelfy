// useDialog() — the behavior every modal, sheet and drawer shares (UX audit
// §3.6, DLG-1, SH-4, MOD-8). Returns a ref for the dialog's container; the
// caller keeps the markup and adds the semantics:
//
//   const ref = useDialog<HTMLDivElement>({ open, onClose });
//   <div ref={ref} role="dialog" aria-modal="true" aria-labelledby={titleId} tabIndex={-1}>
//
// Options:
//   open?:          default true, for a component mounted only while open
//   onClose:        called on Escape (wire the scrim and the × yourself)
//   initialFocus?:  a ref to focus on open; otherwise the first focusable
//                   element inside, otherwise the container. Focus that is
//                   already inside (an `autoFocus` field) is left alone.
//   closeOnEscape?: default true
//   restoreFocus?:  default true: focus goes back to whatever had it before
//                   the dialog opened (the trigger) when it closes
//   inertOthers?:   default true: everything outside the dialog becomes
//                   `inert` (no focus, no clicks, hidden from screen readers),
//                   whether the dialog is portaled to <body> or not. Live
//                   regions (toasts) stay live. So put the scrim AROUND the
//                   dialog (its parent, closing when the click's target is
//                   the scrim itself), not beside it: a sibling turns inert
//                   and can't be clicked.
//
// While open, Tab and Shift+Tab cycle inside the dialog. Dialogs and popovers
// stack in the order they open (src/components/ui/layers.ts): only the
// topmost reacts to Escape and Tab, and a component inside can keep Escape
// for itself with `event.preventDefault()`. The container must be in the DOM
// when `open` turns true.
import { useEffect, useRef } from 'react';
import type React from 'react';
import { isTopLayer, popLayer, pushLayer } from '../components/ui/layers';

export interface UseDialogOptions {
  open?: boolean;
  onClose: () => void;
  initialFocus?: React.RefObject<HTMLElement | null>;
  closeOnEscape?: boolean;
  restoreFocus?: boolean;
  inertOthers?: boolean;
}

const FOCUSABLE = [
  'a[href]',
  'area[href]',
  'button:not([disabled])',
  'input:not([disabled]):not([type="hidden"])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  'iframe',
  'audio[controls]',
  'video[controls]',
  '[contenteditable]:not([contenteditable="false"])',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

// The elements Tab can reach inside `root`, in order. `checkVisibility` skips
// display:none content where the browser has it (jsdom doesn't: there, every
// element counts as visible).
export function getFocusable(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter((el) => {
    if (el.closest('[inert], [hidden]')) return false;
    const check = (el as HTMLElement & { checkVisibility?: () => boolean }).checkVisibility;
    return typeof check === 'function' ? check.call(el) : true;
  });
}

const SKIP_TAGS = new Set(['SCRIPT', 'STYLE', 'LINK', 'TEMPLATE', 'NOSCRIPT']);

// Makes every element outside `el` inert: its siblings, then its parent's
// siblings, up to <body>. Returns the undo, which touches only what it set.
function inertOutside(el: HTMLElement): () => void {
  const changed: Element[] = [];
  let node: Element | null = el;
  while (node && node !== document.body && node.parentElement) {
    for (const sibling of Array.from(node.parentElement.children)) {
      if (sibling === node || SKIP_TAGS.has(sibling.tagName) || sibling.hasAttribute('inert')) {
        continue;
      }
      // Toasts must keep announcing (and an Undo stays clickable) over a modal.
      if (sibling.matches('[aria-live], [role="status"], [role="alert"]')) continue;
      sibling.setAttribute('inert', '');
      changed.push(sibling);
    }
    node = node.parentElement;
  }
  return () => {
    for (const sibling of changed) sibling.removeAttribute('inert');
  };
}

export function useDialog<T extends HTMLElement = HTMLDivElement>({
  open = true,
  onClose,
  initialFocus,
  closeOnEscape = true,
  restoreFocus = true,
  inertOthers = true,
}: UseDialogOptions): React.RefObject<T> {
  const ref = useRef<T>(null);
  // Read through refs so inline callbacks don't re-run the effect (and steal
  // focus back) on every render of the caller.
  const latest = useRef({ onClose, initialFocus, closeOnEscape, restoreFocus, inertOthers });
  latest.current = { onClose, initialFocus, closeOnEscape, restoreFocus, inertOthers };

  useEffect(() => {
    if (!open) return undefined;
    const root = ref.current;
    if (!root) return undefined;
    const options = latest.current;
    const token = pushLayer('dialog');
    const trigger = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const undoInert = options.inertOthers ? inertOutside(root) : () => {};

    if (!root.contains(document.activeElement)) {
      const target = options.initialFocus?.current ?? getFocusable(root)[0] ?? root;
      if (target === root && !root.hasAttribute('tabindex')) root.setAttribute('tabindex', '-1');
      target.focus({ preventScroll: true });
    }

    const onKeyDown = (e: KeyboardEvent): void => {
      if (!isTopLayer(token)) return;
      if (e.key === 'Escape') {
        if (!latest.current.closeOnEscape || e.defaultPrevented) return;
        e.preventDefault();
        latest.current.onClose();
        return;
      }
      if (e.key !== 'Tab') return;
      const items = getFocusable(root);
      const active = document.activeElement;
      if (items.length === 0) {
        e.preventDefault();
        root.focus({ preventScroll: true });
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      if (!root.contains(active)) {
        e.preventDefault();
        (e.shiftKey ? last : first).focus();
      } else if (e.shiftKey && (active === first || active === root)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener('keydown', onKeyDown);

    return () => {
      document.removeEventListener('keydown', onKeyDown);
      popLayer(token);
      // Lift `inert` first: an inert trigger can't take focus back.
      undoInert();
      if (latest.current.restoreFocus && trigger?.isConnected) {
        trigger.focus({ preventScroll: true });
      }
    };
  }, [open]);

  return ref;
}
