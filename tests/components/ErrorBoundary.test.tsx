// The per-view error boundary (src/components/ErrorBoundary.tsx): a crash
// shows a panel in the view's place and is reported through the client,
// without the view's props.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import React, { useState } from 'react';
import { render, screen, fireEvent } from '@testing-library/react';
import ErrorBoundary from '../../src/components/ErrorBoundary';
import { ShelfyProvider } from '../../src/api/ShelfyProvider';
import type { ShelfyClient, ViewErrorReport } from '../../src/api/ShelfyClient';

let broken = true;
function Fragile({ caption }: { caption: string }): React.JSX.Element {
  if (broken) throw new TypeError("Cannot read properties of undefined (reading 'map')");
  return <p data-testid="fragile">{caption}</p>;
}

function clientWith(reportError: (report: ViewErrorReport) => void): ShelfyClient {
  return { reportError } as unknown as ShelfyClient;
}

// React logs every error a boundary catches; keep the output readable.
let consoleError: { mockRestore: () => void };
beforeEach(() => {
  broken = true;
  consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
});
afterEach(() => consoleError.mockRestore());

describe('ErrorBoundary', () => {
  it('shows a panel in the view and reports through the client', () => {
    const reportError = vi.fn();
    render(
      <ShelfyProvider client={clientWith(reportError)}>
        <div data-testid="shell">
          <ErrorBoundary view="gallery">
            <Fragile caption="PRIVATE CAPTION" />
          </ErrorBoundary>
        </div>
      </ShelfyProvider>,
    );
    expect(screen.getByTestId('shell')).toBeInTheDocument();
    expect(screen.getByTestId('error-boundary')).toHaveAttribute('role', 'alert');
    expect(screen.getByText('Qualcosa è andato storto')).toBeInTheDocument();
    expect(reportError).toHaveBeenCalledTimes(1);
    const report = reportError.mock.calls[0][0] as ViewErrorReport;
    expect(report.view).toBe('gallery');
    expect(report.error).toBeInstanceOf(TypeError);
    expect(report.componentStack).toContain('Fragile');
    expect(JSON.stringify(report)).not.toContain('PRIVATE CAPTION');
  });

  it('renders the view again on retry', () => {
    render(
      <ShelfyProvider client={clientWith(vi.fn())}>
        <ErrorBoundary view="gallery">
          <Fragile caption="back" />
        </ErrorBoundary>
      </ShelfyProvider>,
    );
    broken = false;
    fireEvent.click(screen.getByTestId('error-retry'));
    expect(screen.getByTestId('fragile')).toHaveTextContent('back');
  });

  it('renders the view again when its reset key changes', () => {
    function Host(): React.JSX.Element {
      const [route, setRoute] = useState('/c/1');
      return (
        <ShelfyProvider client={clientWith(vi.fn())}>
          <button data-testid="go" onClick={() => setRoute('/c/2')} />
          <ErrorBoundary view="gallery" resetKey={route}>
            <Fragile caption={route} />
          </ErrorBoundary>
        </ShelfyProvider>
      );
    }
    render(<Host />);
    expect(screen.getByTestId('error-boundary')).toBeInTheDocument();
    broken = false;
    fireEvent.click(screen.getByTestId('go'));
    expect(screen.getByTestId('fragile')).toHaveTextContent('/c/2');
  });

  it('closes a crashed dialog and offers a reload for the whole page', () => {
    const onDismiss = vi.fn();
    const { unmount } = render(
      <ShelfyProvider client={clientWith(vi.fn())}>
        <ErrorBoundary view="postModal" layout="dialog" onDismiss={onDismiss}>
          <Fragile caption="x" />
        </ErrorBoundary>
      </ShelfyProvider>,
    );
    fireEvent.click(screen.getByTestId('error-dismiss'));
    expect(onDismiss).toHaveBeenCalled();
    expect(screen.queryByTestId('error-reload')).toBeNull();
    unmount();

    render(
      <ShelfyProvider client={clientWith(vi.fn())}>
        <ErrorBoundary view="app" layout="page">
          <Fragile caption="x" />
        </ErrorBoundary>
      </ShelfyProvider>,
    );
    expect(screen.getByTestId('error-reload')).toBeInTheDocument();
    expect(screen.queryByTestId('error-dismiss')).toBeNull();
  });

  it('prefers its own reporter, and survives one that throws', () => {
    const clientReport = vi.fn();
    const onError = vi.fn(() => {
      throw new Error('reporter down');
    });
    render(
      <ShelfyProvider client={clientWith(clientReport)}>
        <ErrorBoundary view="root" onError={onError}>
          <Fragile caption="x" />
        </ErrorBoundary>
      </ShelfyProvider>,
    );
    expect(onError).toHaveBeenCalledTimes(1);
    expect(clientReport).not.toHaveBeenCalled();
    expect(screen.getByTestId('error-boundary')).toBeInTheDocument();
  });
});
