// <EmptyState> — what a list or view shows when it has nothing to show (UX
// audit §3.6; the 404 page is the model). Centered in the space it's given:
// put it in a flex container, or pass a `className` with a height.
//
//   icon:             a lucide icon (36px, stroke 1.5, muted)
//   title:            one short line, sentence case ("Trash is empty")
//   body?:            one or two lines on what to do next
//   action?:          { label, onClick, icon?, testId? } — a secondary Button
//   secondaryAction?: { label, onClick, testId? } — a text link under it
//   testId?, className?
//
// Pick copy per situation (empty library vs. no search results vs. no filter
// results vs. empty folder) instead of one generic message, and never point
// the web at desktop-only features.
import React from 'react';
import type { LucideIcon } from 'lucide-react';
import Button from './Button';

export interface EmptyStateAction {
  label: string;
  onClick: () => void;
  icon?: LucideIcon;
  testId?: string;
}

export interface EmptyStateProps {
  icon: LucideIcon;
  title: string;
  body?: React.ReactNode;
  action?: EmptyStateAction;
  secondaryAction?: Omit<EmptyStateAction, 'icon'>;
  testId?: string;
  className?: string;
}

export default function EmptyState({
  icon: Icon,
  title,
  body,
  action,
  secondaryAction,
  testId,
  className = '',
}: EmptyStateProps): React.JSX.Element {
  return (
    <div
      data-testid={testId}
      className={`u-fade-in-up flex flex-1 flex-col items-center justify-center gap-3 px-6 py-12 text-center ${className}`}
    >
      <Icon size={36} strokeWidth={1.5} aria-hidden="true" className="text-muted" />
      <div className="flex flex-col items-center gap-1">
        <p className="text-sm font-medium text-primary">{title}</p>
        {body && <p className="max-w-xs text-sm leading-relaxed text-secondary">{body}</p>}
      </div>
      {(action || secondaryAction) && (
        <div className="mt-1 flex flex-col items-center gap-2">
          {action && (
            <Button
              variant="secondary"
              icon={action.icon}
              onClick={action.onClick}
              data-testid={action.testId}
            >
              {action.label}
            </Button>
          )}
          {secondaryAction && (
            <button
              type="button"
              onClick={secondaryAction.onClick}
              data-testid={secondaryAction.testId}
              className="rounded text-xs text-accent underline-offset-2 hover:underline narrow:min-h-11"
            >
              {secondaryAction.label}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
