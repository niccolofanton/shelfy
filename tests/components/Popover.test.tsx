// Popover's UX-2 additions: role="menu" keyboard handling, the bottom-sheet
// presentation, and Escape going to the topmost layer only.
import React, { useRef, useState } from 'react';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { describe, it, expect, vi, afterEach } from 'vitest';
import Popover, { type PopoverPresentation } from '../../src/components/Popover';
import { useDialog } from '../../src/hooks/useDialog';

function Menu({
  presentation = 'anchored',
  onPick = () => {},
}: {
  presentation?: PopoverPresentation;
  onPick?: (item: string) => void;
}): React.JSX.Element {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const pick = (item: string): void => {
    onPick(item);
    setOpen(false);
  };
  return (
    <>
      <button type="button" ref={anchor} onClick={() => setOpen((o) => !o)}>
        Actions
      </button>
      <button type="button">After</button>
      <Popover
        anchorRef={anchor}
        open={open}
        onRequestClose={() => setOpen(false)}
        presentation={presentation}
        role="menu"
        aria-label="Bulk actions"
        data-testid="menu"
        className="w-56"
      >
        {['Analyze', 'Export', 'Delete'].map((item) => (
          <button key={item} type="button" role="menuitem" onClick={() => pick(item)}>
            {item}
          </button>
        ))}
      </Popover>
    </>
  );
}

function openMenu(): HTMLElement {
  const anchor = screen.getByRole('button', { name: 'Actions' });
  anchor.focus();
  fireEvent.click(anchor);
  return anchor;
}

const item = (name: string): HTMLElement => screen.getByRole('menuitem', { name });

describe('Popover role="menu"', () => {
  it('focuses the first item and moves with the arrow keys, Home and End', () => {
    render(<Menu />);
    openMenu();
    expect(item('Analyze')).toHaveFocus();
    const menu = screen.getByTestId('menu');
    fireEvent.keyDown(menu, { key: 'ArrowDown' });
    expect(item('Export')).toHaveFocus();
    fireEvent.keyDown(menu, { key: 'End' });
    expect(item('Delete')).toHaveFocus();
    fireEvent.keyDown(menu, { key: 'ArrowDown' });
    expect(item('Analyze')).toHaveFocus();
    fireEvent.keyDown(menu, { key: 'ArrowUp' });
    expect(item('Delete')).toHaveFocus();
    fireEvent.keyDown(menu, { key: 'Home' });
    expect(item('Analyze')).toHaveFocus();
  });

  it('closes on Escape and gives focus back to the anchor', () => {
    render(<Menu />);
    const anchor = openMenu();
    fireEvent.keyDown(item('Analyze'), { key: 'Escape' });
    expect(screen.queryByTestId('menu')).toBeNull();
    expect(anchor).toHaveFocus();
  });

  it('gives focus back to the anchor after an item runs', () => {
    const onPick = vi.fn();
    render(<Menu onPick={onPick} />);
    const anchor = openMenu();
    fireEvent.click(item('Export'));
    expect(onPick).toHaveBeenCalledWith('Export');
    expect(screen.queryByTestId('menu')).toBeNull();
    expect(anchor).toHaveFocus();
  });

  it('closes on Tab, from the anchor', () => {
    render(<Menu />);
    const anchor = openMenu();
    fireEvent.keyDown(screen.getByTestId('menu'), { key: 'Tab' });
    expect(screen.queryByTestId('menu')).toBeNull();
    expect(anchor).toHaveFocus();
  });
});

describe('Popover presentation', () => {
  const original = window.matchMedia;
  afterEach(() => {
    window.matchMedia = original;
  });

  function setNarrow(narrow: boolean): void {
    window.matchMedia = ((query: string) => ({
      matches: narrow && query.includes('max-width: 899px'),
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    })) as unknown as typeof window.matchMedia;
  }

  it('"auto" stays anchored on wide screens', () => {
    setNarrow(false);
    render(<Menu presentation="auto" />);
    openMenu();
    expect(screen.getByTestId('menu').className).toContain('w-56');
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('"auto" opens a bottom sheet on narrow screens, with dialog behavior', () => {
    setNarrow(true);
    const { container } = render(<Menu presentation="auto" />);
    const anchor = openMenu();
    const sheet = screen.getByRole('dialog', { name: 'Bulk actions' });
    expect(sheet).toHaveAttribute('aria-modal', 'true');
    expect(sheet.className).toContain('u-sheet');
    const menu = screen.getByTestId('menu');
    expect(menu).toHaveAttribute('role', 'menu');
    expect(menu.className).not.toContain('w-56');
    expect(item('Analyze')).toHaveFocus();
    expect(container).toHaveAttribute('inert');

    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(container).not.toHaveAttribute('inert');
    expect(anchor).toHaveFocus();
  });

  it('closes the sheet from the scrim and from the handle', () => {
    render(<Menu presentation="sheet" />);
    openMenu();
    fireEvent.click(screen.getByTestId('popover-scrim'));
    expect(screen.queryByRole('dialog')).toBeNull();
    openMenu();
    fireEvent.click(screen.getByTestId('popover-sheet-handle'));
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});

describe('Popover inside a dialog', () => {
  it('Escape closes the menu first, then the dialog', () => {
    const closeDialog = vi.fn();
    function InDialog(): React.JSX.Element {
      const ref = useDialog<HTMLDivElement>({ onClose: closeDialog });
      return (
        <div ref={ref} role="dialog" aria-label="Post">
          <Menu />
        </div>
      );
    }
    render(<InDialog />);
    openMenu();
    act(() => {
      fireEvent.keyDown(item('Analyze'), { key: 'Escape' });
    });
    expect(screen.queryByTestId('menu')).toBeNull();
    expect(closeDialog).not.toHaveBeenCalled();
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    expect(closeDialog).toHaveBeenCalledOnce();
  });
});
