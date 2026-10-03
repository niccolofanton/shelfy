// useToasts() + <ToastHost> (UX audit §3.6, GAL-9).
import React, { useEffect } from 'react';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { ToastHost } from '../../../src/components/ui';
import {
  useToasts,
  TOAST_DURATION_MS,
  TOAST_ACTION_DURATION_MS,
  type UseToasts,
} from '../../../src/hooks/useToast';

let api: UseToasts;

function Harness(): React.JSX.Element {
  const toasts = useToasts();
  useEffect(() => {
    api = toasts;
  });
  return <ToastHost toasts={toasts} />;
}

const EXIT_MS = 200;

describe('ToastHost', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    render(<Harness />);
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('is one polite live region, portaled to <body>, present before any toast', () => {
    const region = screen.getByTestId('toast-region');
    expect(region).toHaveAttribute('role', 'status');
    expect(region).toHaveAttribute('aria-live', 'polite');
    expect(region.parentElement).toBe(document.body);
    expect(region.className).toContain('z-toast');
    expect(region).toBeEmptyDOMElement();
  });

  it('shows a toast for 4s, plays its exit, then removes it', () => {
    act(() => {
      api.show('Saved', { variant: 'success', testId: 'saved-toast' });
    });
    const toast = screen.getByTestId('saved-toast');
    expect(toast).toHaveTextContent('Saved');
    expect(toast).toHaveAttribute('data-variant', 'success');
    expect(toast.querySelector('svg')).not.toBeNull();

    act(() => vi.advanceTimersByTime(TOAST_DURATION_MS - 1));
    expect(screen.getByTestId('saved-toast').className).not.toContain('u-fade-out');
    act(() => vi.advanceTimersByTime(1));
    expect(screen.getByTestId('saved-toast').className).toContain('u-fade-out');
    act(() => vi.advanceTimersByTime(EXIT_MS));
    expect(screen.queryByTestId('saved-toast')).toBeNull();
  });

  it('keeps a toast with an action for 8s; the action runs and dismisses it', () => {
    const undo = vi.fn();
    act(() => {
      api.show('3 posts deleted', {
        testId: 'undo-toast',
        action: { label: 'Undo', onClick: undo, testId: 'undo-action' },
      });
    });
    act(() => vi.advanceTimersByTime(TOAST_DURATION_MS + EXIT_MS));
    expect(screen.getByTestId('undo-toast')).toBeInTheDocument();
    fireEvent.click(screen.getByTestId('undo-action'));
    expect(undo).toHaveBeenCalledOnce();
    act(() => vi.advanceTimersByTime(EXIT_MS));
    expect(screen.queryByTestId('undo-toast')).toBeNull();
    expect(TOAST_ACTION_DURATION_MS).toBe(8000);
  });

  it('shows at most 3, pushing out the oldest', () => {
    act(() => {
      for (const n of [1, 2, 3, 4]) api.show(`Toast ${n}`);
    });
    const region = screen.getByTestId('toast-region');
    expect(region.children).toHaveLength(3);
    expect(region).not.toHaveTextContent('Toast 1');
    expect(region).toHaveTextContent('Toast 4');
  });

  it('replaces a toast with the same id in place, and keeps duration:null ones', () => {
    act(() => {
      api.show('Working… 10%', { id: 'job', variant: 'progress', duration: null, testId: 'job' });
    });
    act(() => {
      api.show('Working… 60%', { id: 'job', variant: 'progress', duration: null, testId: 'job' });
    });
    expect(screen.getAllByTestId('job')).toHaveLength(1);
    expect(screen.getByTestId('job')).toHaveTextContent('Working… 60%');
    expect(screen.getByTestId('job').querySelector('.animate-spin')).not.toBeNull();
    act(() => vi.advanceTimersByTime(60_000));
    expect(screen.getByTestId('job')).toBeInTheDocument();
    act(() => api.dismiss('job'));
    act(() => vi.advanceTimersByTime(EXIT_MS));
    expect(screen.queryByTestId('job')).toBeNull();
  });

  it('pauses while hovered and resumes with the time that was left', () => {
    act(() => {
      api.show('Hover me', { testId: 'hover' });
    });
    act(() => vi.advanceTimersByTime(3000));
    fireEvent.mouseEnter(screen.getByTestId('hover'));
    act(() => vi.advanceTimersByTime(10_000));
    expect(screen.getByTestId('hover').className).not.toContain('u-fade-out');
    fireEvent.mouseLeave(screen.getByTestId('hover'));
    act(() => vi.advanceTimersByTime(999));
    expect(screen.getByTestId('hover').className).not.toContain('u-fade-out');
    act(() => vi.advanceTimersByTime(1));
    expect(screen.getByTestId('hover').className).toContain('u-fade-out');
  });

  it('dismisses from its × button', () => {
    act(() => {
      api.show('Bye', { testId: 'bye' });
    });
    fireEvent.click(screen.getByRole('button', { name: 'Chiudi notifica' }));
    act(() => vi.advanceTimersByTime(EXIT_MS));
    expect(screen.queryByTestId('bye')).toBeNull();
  });
});
