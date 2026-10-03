// <PageHeader> — the top row of a full view such as Trash or Jobs (UX audit
// §3.6, TR-4): 56px tall, the title on the left, actions on the right.
//
//   title:      the view's name (an <h1> unless `titleAs` says otherwise)
//   leading?:   before the title — the view passes UX-1's MenuButton, so this
//               component doesn't depend on the shell
//   count?:     a number or short text after the title, in muted text
//   subtitle?:  a line under the title (instead of, or with, the count)
//   actions?:   buttons on the right
//   titleAs?:   'h1' (default) | 'h2'
//   testId?, className?
import React from 'react';

export interface PageHeaderProps {
  title: React.ReactNode;
  leading?: React.ReactNode;
  count?: React.ReactNode;
  subtitle?: React.ReactNode;
  actions?: React.ReactNode;
  titleAs?: 'h1' | 'h2';
  testId?: string;
  className?: string;
}

export default function PageHeader({
  title,
  leading,
  count,
  subtitle,
  actions,
  titleAs: Title = 'h1',
  testId,
  className = '',
}: PageHeaderProps): React.JSX.Element {
  return (
    <header
      data-testid={testId}
      className={`flex h-14 shrink-0 items-center gap-3 px-4 narrow:px-3 ${className}`}
    >
      {leading}
      <div className="flex min-w-0 flex-1 flex-col justify-center">
        <div className="flex min-w-0 items-baseline gap-2">
          <Title className="truncate font-display text-lg font-semibold text-primary">
            {title}
          </Title>
          {count != null && (
            <span className="shrink-0 text-sm tabular-nums text-muted">{count}</span>
          )}
        </div>
        {subtitle && <p className="truncate text-xs text-secondary">{subtitle}</p>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </header>
  );
}
