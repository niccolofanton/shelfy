import React from 'react';
import Logo from '@ui/components/Logo';

// The frame of the sign-in pages: the brand above one centered card.
export default function AuthLayout({
  title,
  children,
}: {
  title?: string;
  children?: React.ReactNode;
}): React.JSX.Element {
  return (
    <div className="flex h-full w-full items-center justify-center overflow-y-auto bg-[#0f0f0f] px-5 py-10">
      <div className="u-fade-in-up w-full max-w-sm">
        <div className="mb-6 flex items-center justify-center gap-2.5">
          <Logo size={22} />
          <span className="font-display text-[17px] font-semibold tracking-wide text-white">
            SHELFY
          </span>
        </div>
        {(title || children) && (
          <div className="rounded-xl border border-[#2e2e2e] bg-[#161616] px-6 py-6 shadow-2xl">
            {title && <h1 className="mb-4 text-[17px] font-semibold text-white">{title}</h1>}
            {children}
          </div>
        )}
      </div>
    </div>
  );
}

// A message box inside the card: `error` for failures, `info` for the rest.
export function Notice({
  tone,
  children,
  testId,
}: {
  tone: 'error' | 'info';
  children: React.ReactNode;
  testId?: string;
}): React.JSX.Element {
  return (
    <p
      data-testid={testId}
      role={tone === 'error' ? 'alert' : 'status'}
      className={[
        'u-fade-in rounded-lg px-3 py-2.5 text-[13px] leading-relaxed',
        tone === 'error'
          ? 'border border-red-500/25 bg-red-500/10 text-red-200'
          : 'border border-[#7B5CFF]/25 bg-[#7B5CFF]/10 text-[#d9d0ff]',
      ].join(' ')}
    >
      {children}
    </p>
  );
}

export const PRIMARY_BUTTON =
  'u-press flex h-10 w-full items-center justify-center gap-2 rounded-lg bg-[#7B5CFF] px-4 text-sm font-medium text-white transition-colors hover:bg-[#5A3DDE] disabled:cursor-not-allowed disabled:opacity-60';
