// Building blocks of the account's Settings sections, in the look of the other
// Settings cards.
import React, { useState } from 'react';
import { AlertTriangle, Check, Loader } from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import { useT } from '../../i18n';

export function Card({
  children,
  testId,
  className = '',
}: {
  children: React.ReactNode;
  testId?: string;
  className?: string;
}): React.JSX.Element {
  return (
    <div
      data-testid={testId}
      className={`rounded-xl border border-[#242424] bg-[#161616] p-5 ${className}`}
    >
      {children}
    </div>
  );
}

export function CardHeader({
  icon: Icon,
  title,
  description,
  aside,
}: {
  icon: LucideIcon;
  title: string;
  description?: React.ReactNode;
  aside?: React.ReactNode;
}): React.JSX.Element {
  return (
    <div className="flex items-start gap-3">
      <Icon size={18} className="text-gray-400 mt-0.5 shrink-0" />
      <div className="flex-1 min-w-0">
        <p className="text-white text-sm font-medium">{title}</p>
        {description && <p className="text-gray-500 text-xs mt-1 leading-relaxed">{description}</p>}
      </div>
      {aside && <div className="shrink-0">{aside}</div>}
    </div>
  );
}

// A one-line message under a card's content.
export function InlineNote({
  tone,
  children,
  testId,
}: {
  tone: 'error' | 'ok' | 'info';
  children: React.ReactNode;
  testId?: string;
}): React.JSX.Element {
  const color =
    tone === 'error' ? 'text-red-400' : tone === 'ok' ? 'text-emerald-400' : 'text-gray-400';
  return (
    <p
      data-testid={testId}
      role={tone === 'error' ? 'alert' : 'status'}
      className={`u-fade-in flex items-start gap-1.5 text-xs mt-3 leading-relaxed ${color}`}
    >
      {tone === 'error' ? (
        <AlertTriangle size={13} className="shrink-0 mt-px" />
      ) : tone === 'ok' ? (
        <Check size={13} className="u-pop-in shrink-0 mt-px" />
      ) : null}
      <span>{children}</span>
    </p>
  );
}

export function Loading({ label }: { label?: string }): React.JSX.Element {
  const tc = useT('common');
  return (
    <p className="flex items-center gap-2 text-xs text-gray-500 py-2">
      <Loader size={13} className="animate-spin shrink-0" /> {label ?? tc('loading')}
    </p>
  );
}

export const BUTTON =
  'u-press inline-flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium bg-[#2a2a2a] text-gray-200 hover:bg-[#333] hover:text-white cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed';

export const PRIMARY_BUTTON =
  'u-press inline-flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium bg-[#7B5CFF] text-white hover:bg-[#5A3DDE] cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed';

export const DANGER_BUTTON =
  'u-press inline-flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium border border-red-900/50 text-red-400 hover:bg-red-950/50 hover:text-red-300 hover:border-red-800 cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed';

export const INPUT =
  'h-8 w-full rounded-lg border border-[#2e2e2e] bg-[#1c1c1c] px-2.5 text-sm text-gray-100 placeholder-gray-600 outline-none transition-colors focus:border-[#7B5CFF]/70';

// A destructive action in two steps: the button, then Confirm / Cancel.
// `onConfirm` shows its own failure: a rejection only ends the busy state.
export function ConfirmAction({
  label,
  busyLabel,
  onConfirm,
  testId,
  icon: Icon,
  disabled = false,
}: {
  label: string;
  busyLabel?: string;
  onConfirm: () => Promise<void>;
  testId?: string;
  icon?: LucideIcon;
  disabled?: boolean;
}): React.JSX.Element {
  const tc = useT('common');
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);

  const run = async (): Promise<void> => {
    setBusy(true);
    try {
      await onConfirm();
    } catch {
      /* shown by onConfirm */
    } finally {
      setBusy(false);
      setConfirming(false);
    }
  };

  if (!confirming) {
    return (
      <button
        type="button"
        data-testid={testId}
        disabled={disabled}
        onClick={() => setConfirming(true)}
        className={DANGER_BUTTON}
      >
        {Icon && <Icon size={13} className="shrink-0" />}
        {label}
      </button>
    );
  }
  return (
    <span className="u-fade-in inline-flex items-center gap-2">
      <button
        type="button"
        data-testid={testId ? `${testId}-confirm` : undefined}
        onClick={run}
        disabled={busy}
        className="u-press inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium bg-red-700 text-white hover:bg-red-600 cursor-pointer disabled:opacity-50"
      >
        {busy && <Loader size={12} className="animate-spin shrink-0" />}
        {busy ? (busyLabel ?? tc('inProgress')) : tc('confirm')}
      </button>
      <button
        type="button"
        onClick={() => setConfirming(false)}
        disabled={busy}
        className="u-press px-3 py-1.5 rounded-lg text-xs bg-[#222] text-gray-300 hover:bg-[#2a2a2a] cursor-pointer disabled:opacity-50"
      >
        {tc('cancel')}
      </button>
    </span>
  );
}
