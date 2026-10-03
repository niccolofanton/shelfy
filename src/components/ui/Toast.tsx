// <ToastHost toasts={useToasts()} /> — renders a view's toasts (UX audit §3.6,
// GAL-9). The API (show, dismiss, variants, Undo actions, durations) is
// documented with useToasts() in src/hooks/useToast.ts.
//
//   toasts:  the object useToasts() returns
//   testId?: data-testid of the region (default "toast-region")
//
// One live region (`role="status"`, polite), portaled to <body> so no view's
// stacking context can cover it, at `z-toast`: bottom center, above the
// BottomNav and the home indicator on narrow screens. Each toast wraps its
// text, carries an icon for its variant (never color alone) and a dismiss ×
// (44px on narrow).
import React from 'react';
import { createPortal } from 'react-dom';
import { AlertCircle, CheckCircle2, X } from 'lucide-react';
import type { ToastItem, UseToasts } from '../../hooks/useToast';
import { useT } from '../../i18n';
import IconButton from './IconButton';
import Spinner from './Spinner';

export interface ToastHostProps {
  toasts: UseToasts;
  testId?: string;
}

function VariantIcon({ variant }: { variant: ToastItem['variant'] }): React.JSX.Element | null {
  if (variant === 'success') {
    return <CheckCircle2 size={14} aria-hidden="true" className="mt-px shrink-0 text-success" />;
  }
  if (variant === 'error') {
    return <AlertCircle size={14} aria-hidden="true" className="mt-px shrink-0 text-error" />;
  }
  if (variant === 'progress') return <Spinner size={14} className="mt-px text-accent" />;
  return null;
}

export function ToastHost({ toasts, testId = 'toast-region' }: ToastHostProps): React.ReactPortal {
  const tc = useT('common');
  const { toasts: items, dismiss, pause, resume } = toasts;
  return createPortal(
    <div
      role="status"
      aria-live="polite"
      aria-label={tc('notifications')}
      data-testid={testId}
      className="pointer-events-none fixed bottom-6 left-1/2 z-toast flex w-[min(92vw,420px)] -translate-x-1/2 flex-col items-center gap-2 narrow:bottom-[calc(64px_+_env(safe-area-inset-bottom))]"
    >
      {items.map((toast) => (
        <div
          key={toast.id}
          data-testid={toast.testId}
          data-variant={toast.variant}
          onMouseEnter={() => pause(toast.id)}
          onMouseLeave={() => resume(toast.id)}
          onFocus={() => pause(toast.id)}
          onBlur={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node | null)) resume(toast.id);
          }}
          className={`${toast.closing ? 'u-fade-out' : 'u-pop-in'} pointer-events-auto flex max-w-full items-start gap-2.5 rounded-lg border border-subtle bg-secondary py-1.5 pl-3.5 pr-1.5 text-xs leading-5 text-primary shadow-lg`}
        >
          <span className="flex min-w-0 flex-1 items-start gap-2 py-1">
            <VariantIcon variant={toast.variant} />
            <span className="min-w-0 break-words">{toast.message}</span>
          </span>
          {toast.action && (
            <button
              type="button"
              data-testid={toast.action.testId}
              onClick={() => {
                toast.action?.onClick();
                dismiss(toast.id);
              }}
              className="u-press shrink-0 rounded-md px-2 py-1 font-medium text-accent hover:bg-hover narrow:min-h-11"
            >
              {toast.action.label}
            </button>
          )}
          <IconButton
            label={tc('dismiss')}
            icon={X}
            size="sm"
            iconSize={13}
            onClick={() => dismiss(toast.id)}
          />
        </div>
      ))}
    </div>,
    document.body,
  );
}

export default ToastHost;
