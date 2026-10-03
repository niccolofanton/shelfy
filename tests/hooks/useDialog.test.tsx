// useDialog() (UX audit §3.6): focus in, trap, restore, Escape, inert.
import React, { useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import { useDialog } from '../../src/hooks/useDialog';

function Dialog({
  onClose,
  portal = true,
  withInitialFocus = false,
  testId = 'dialog',
  children,
}: {
  onClose: () => void;
  portal?: boolean;
  withInitialFocus?: boolean;
  testId?: string;
  children?: React.ReactNode;
}): React.JSX.Element {
  const initial = useRef<HTMLButtonElement>(null);
  const ref = useDialog<HTMLDivElement>({
    onClose,
    initialFocus: withInitialFocus ? initial : undefined,
  });
  const body = (
    <div ref={ref} role="dialog" aria-modal="true" aria-label="Test" data-testid={testId}>
      <button type="button">First</button>
      <button type="button" ref={initial}>
        Second
      </button>
      <button type="button">Last</button>
      {children}
    </div>
  );
  return portal ? createPortal(body, document.body) : body;
}

function Page({
  portal = true,
  withInitialFocus = false,
  onClose,
}: {
  portal?: boolean;
  withInitialFocus?: boolean;
  onClose?: () => void;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <button type="button" onClick={() => setOpen(true)}>
        Open
      </button>
      <a href="#behind">Behind</a>
      {open && (
        <Dialog
          portal={portal}
          withInitialFocus={withInitialFocus}
          onClose={() => {
            onClose?.();
            setOpen(false);
          }}
        />
      )}
    </div>
  );
}

function openDialog(): HTMLElement {
  const trigger = screen.getByRole('button', { name: 'Open' });
  trigger.focus();
  fireEvent.click(trigger);
  return trigger;
}

const tab = (shift = false): void => {
  fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Tab', shiftKey: shift });
};

describe('useDialog', () => {
  it('moves focus to the first focusable element, or to initialFocus', () => {
    const { unmount } = render(<Page />);
    openDialog();
    expect(screen.getByRole('button', { name: 'First' })).toHaveFocus();
    unmount();

    render(<Page withInitialFocus />);
    openDialog();
    expect(screen.getByRole('button', { name: 'Second' })).toHaveFocus();
  });

  it('traps Tab and Shift+Tab inside the dialog', () => {
    render(<Page />);
    openDialog();
    const first = screen.getByRole('button', { name: 'First' });
    const last = screen.getByRole('button', { name: 'Last' });
    last.focus();
    tab();
    expect(first).toHaveFocus();
    tab(true);
    expect(last).toHaveFocus();
  });

  it('closes on Escape and gives focus back to the trigger', () => {
    const onClose = vi.fn();
    render(<Page onClose={onClose} />);
    const trigger = openDialog();
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledOnce();
    expect(screen.queryByTestId('dialog')).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it('leaves Escape to a component inside that handled it', () => {
    const onClose = vi.fn();
    render(<Page onClose={onClose} />);
    openDialog();
    const first = screen.getByRole('button', { name: 'First' });
    first.addEventListener('keydown', (e) => e.preventDefault());
    fireEvent.keyDown(first, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
  });

  it('makes everything outside inert while open, live regions excepted', () => {
    const live = document.createElement('div');
    live.setAttribute('role', 'status');
    document.body.appendChild(live);
    const { container } = render(<Page />);
    openDialog();
    expect(container).toHaveAttribute('inert');
    expect(live).not.toHaveAttribute('inert');
    expect(screen.getByTestId('dialog').closest('[inert]')).toBeNull();
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    expect(container).not.toHaveAttribute('inert');
    live.remove();
  });

  it('inerts the siblings along the path when the dialog is not portaled', () => {
    render(<Page portal={false} />);
    openDialog();
    const dialog = screen.getByTestId('dialog');
    expect(dialog.closest('[inert]')).toBeNull();
    expect(screen.getByRole('button', { name: 'Open', hidden: true })).toHaveAttribute('inert');
    expect(screen.getByText('Behind')).toHaveAttribute('inert');
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    expect(screen.getByText('Behind')).not.toHaveAttribute('inert');
  });

  it('with a dialog opened from a dialog, Escape closes only the top one', () => {
    const outerClose = vi.fn();
    const innerClose = vi.fn();
    function Nested(): React.JSX.Element {
      const [inner, setInner] = useState(false);
      return (
        <Dialog testId="outer" onClose={outerClose}>
          <button type="button" onClick={() => setInner(true)}>
            Open inner
          </button>
          {inner && (
            <Dialog
              testId="inner"
              onClose={() => {
                innerClose();
                setInner(false);
              }}
            />
          )}
        </Dialog>
      );
    }
    render(<Nested />);
    fireEvent.click(screen.getByRole('button', { name: 'Open inner' }));
    expect(screen.getByTestId('inner')).toBeInTheDocument();
    act(() => {
      fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Escape' });
    });
    expect(innerClose).toHaveBeenCalledOnce();
    expect(outerClose).not.toHaveBeenCalled();
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Escape' });
    expect(outerClose).toHaveBeenCalledOnce();
  });
});
