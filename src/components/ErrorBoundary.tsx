import React from 'react';
import { AlertTriangle, RotateCcw, type LucideIcon } from 'lucide-react';
import { useT } from '../i18n';
import { useShelfy } from '../api/ShelfyProvider';
import type { ViewErrorReport } from '../api/ShelfyClient';

// One error boundary per view (web port plan §2.19 Robustness, §1.2 #11): a
// view that throws while rendering shows a panel in its place instead of
// taking the whole app down, and the error goes to the ShelfyClient's
// reportError (the web client posts it to `POST /api/v1/client-errors`, the
// desktop logs it). A report carries the error and React's component stack,
// never the view's props or state, so no post content leaves with it.

// Where the panel goes: in the view's place (`view`), over the whole window
// with a reload (`page`: the app-wide boundary), or over the app as a dialog
// that can be closed (`dialog`: a modal's boundary, with `onDismiss`).
export type ErrorBoundaryLayout = 'view' | 'page' | 'dialog';

export interface ErrorBoundaryProps {
  // The view's name in reports: 'gallery', 'postModal', 'settings'…
  view: string;
  children?: React.ReactNode;
  // A change of this value clears a caught error: pass what the view shows
  // (its route, its folder), so moving on renders it again.
  resetKey?: unknown;
  // Where a caught error goes. Default: the ShelfyClient's reportError.
  onError?: (report: ViewErrorReport) => void;
  layout?: ErrorBoundaryLayout;
  // Closes what crashed (a dialog's Close).
  onDismiss?: () => void;
}

interface BoundaryProps extends ErrorBoundaryProps {
  report: (report: ViewErrorReport) => void;
}

interface BoundaryState {
  failed: boolean;
  resetKey: unknown;
}

class Boundary extends React.Component<BoundaryProps, BoundaryState> {
  override state: BoundaryState = { failed: false, resetKey: this.props.resetKey };

  static getDerivedStateFromError(): Partial<BoundaryState> {
    return { failed: true };
  }

  static getDerivedStateFromProps(
    props: BoundaryProps,
    state: BoundaryState,
  ): Partial<BoundaryState> | null {
    return Object.is(props.resetKey, state.resetKey)
      ? null
      : { failed: false, resetKey: props.resetKey };
  }

  override componentDidCatch(error: unknown, info: React.ErrorInfo): void {
    try {
      this.props.report({
        view: this.props.view,
        error,
        componentStack: info.componentStack ?? null,
      });
    } catch {
      /* a failing report must not take the panel down too */
    }
  }

  private retry = (): void => this.setState({ failed: false });

  override render(): React.ReactNode {
    if (!this.state.failed) return this.props.children;
    return (
      <CrashPanel
        layout={this.props.layout ?? 'view'}
        onRetry={this.retry}
        onDismiss={this.props.onDismiss}
      />
    );
  }
}

export default function ErrorBoundary(props: ErrorBoundaryProps): React.JSX.Element {
  const client = useShelfy();
  return <Boundary {...props} report={props.onError ?? client.reportError} />;
}

function CrashPanel({
  layout,
  onRetry,
  onDismiss,
}: {
  layout: ErrorBoundaryLayout;
  onRetry: () => void;
  onDismiss?: () => void;
}): React.JSX.Element {
  const t = useT('errors');
  const actions: ErrorPanelAction[] = [
    { label: t('retry'), onClick: onRetry, testId: 'error-retry', primary: true },
  ];
  if (layout === 'page') {
    actions.push({
      label: t('reload'),
      onClick: () => window.location.reload(),
      testId: 'error-reload',
    });
  }
  if (onDismiss) actions.push({ label: t('close'), onClick: onDismiss, testId: 'error-dismiss' });
  const panel = (
    <ErrorPanel
      testId="error-boundary"
      tone="error"
      fullPage={layout === 'page'}
      title={t('crashedTitle')}
      message={t('crashed')}
      actions={actions}
    />
  );
  if (layout !== 'dialog') return panel;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-6">
      <div className="w-full max-w-md rounded-xl border border-[#2e2e2e] bg-[#161616] shadow-2xl">
        {panel}
      </div>
    </div>
  );
}

export interface ErrorPanelAction {
  label: string;
  onClick: () => void;
  testId?: string;
  primary?: boolean;
}

// What a view shows instead of its content: after a crash (tone `error`), or
// for an address with nothing behind it (tone `info`).
export function ErrorPanel({
  title,
  message,
  actions = [],
  tone = 'info',
  fullPage = false,
  icon: Icon = AlertTriangle,
  testId,
}: {
  title: string;
  message: string;
  actions?: ErrorPanelAction[];
  tone?: 'error' | 'info';
  fullPage?: boolean;
  icon?: LucideIcon;
  testId?: string;
}): React.JSX.Element {
  return (
    <div
      data-testid={testId}
      role={tone === 'error' ? 'alert' : 'status'}
      className={[
        'u-fade-in-up flex flex-col items-center justify-center gap-3 px-6 py-10 text-center',
        fullPage ? 'h-screen w-screen bg-[#0f0f0f]' : 'h-full min-h-[40vh]',
      ].join(' ')}
    >
      <Icon size={36} className="text-[#3a3a3a]" strokeWidth={1} />
      <h2 className="font-display text-[15px] font-medium text-gray-200">{title}</h2>
      <p className="max-w-xs text-sm leading-relaxed text-[#777]">{message}</p>
      {actions.length > 0 && (
        <div className="mt-1 flex flex-wrap items-center justify-center gap-2">
          {actions.map((action) => (
            <button
              key={action.label}
              type="button"
              data-testid={action.testId}
              onClick={action.onClick}
              className={[
                'u-press inline-flex h-8 items-center gap-2 rounded-md px-3 text-[12px] font-medium transition-colors',
                action.primary
                  ? 'bg-[#7B5CFF] text-white hover:bg-[#5A3DDE]'
                  : 'bg-[#1f1f1f] text-gray-200 hover:bg-[#272727]',
              ].join(' ')}
            >
              {action.primary && tone === 'error' && <RotateCcw size={13} />}
              {action.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
