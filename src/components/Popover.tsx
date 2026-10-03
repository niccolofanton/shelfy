import React, { useLayoutEffect, useRef, useState, useEffect, useCallback } from 'react';
import { createPortal } from 'react-dom';
import { useDialog } from '../hooks/useDialog';
import { useT } from '../i18n';
import { isTopLayer, popLayer, pushLayer } from './ui/layers';
import { Z } from './ui/tokens';
import { NARROW_QUERY, useMediaQuery } from './ui/useMediaQuery';

type PopoverAlign = 'left' | 'right';
type PopoverPlacement = 'bottom' | 'top';
export type PopoverPresentation = 'anchored' | 'auto' | 'sheet';

interface PopoverPos {
  top: number | null;
  bottom: number | null;
  left: number | null;
  right: number | null;
  maxHeight: number;
}

interface PopoverProps extends React.HTMLAttributes<HTMLDivElement> {
  anchorRef: React.RefObject<HTMLElement | null>;
  open: boolean;
  onRequestClose?: () => void;
  align?: PopoverAlign;
  placement?: PopoverPlacement;
  gap?: number;
  hoverBridge?: boolean;
  presentation?: PopoverPresentation;
  sheetClassName?: string;
  className?: string;
  style?: React.CSSProperties;
  children?: React.ReactNode;
}

const MENU_ITEM = '[role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"]';

function menuItems(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(MENU_ITEM)).filter(
    (el) => !el.hasAttribute('disabled') && el.getAttribute('aria-disabled') !== 'true',
  );
}

// A drag on the sheet's handle further than this (px) closes it.
const SHEET_DISMISS_PX = 80;

/**
 * Anchored dropdown rendered in a portal at <body>. Living at the document root
 * means no ancestor stacking context (sticky bars, transformed wrappers) can
 * trap it and no `overflow:hidden` can clip it — the menu is always on top.
 * Position is `fixed`, computed from the anchor's bounding box and re-measured
 * on scroll/resize (plus Resize/IntersectionObserver) so it tracks the trigger.
 * After measuring, placement/alignment flip and the coordinates clamp against
 * the viewport, and a max-height with overflow:auto keeps tall menus on-screen.
 * If the anchor unmounts while open, the menu requests close instead of leaving
 * a stale floating copy behind.
 *
 * UX-2 additions (UX audit §3.6):
 * - `presentation="auto"` opens the content as a bottom sheet on narrow
 *   screens (<900px) and anchored above that; "sheet" always uses the sheet.
 *   The sheet has a scrim, a drag handle (tap or drag down to close), 48px
 *   menu rows, safe-area padding, and dialog behavior (useDialog: focus in,
 *   trap, restore, Escape, the page behind inert). In the sheet, `className`
 *   and `style` (anchored-menu sizing) are ignored; `sheetClassName` styles
 *   the content instead. The other props (role, aria-*, data-testid,
 *   handlers) go on the content in both presentations.
 * - `role="menu"`: the first menu item takes focus on open; ArrowUp/ArrowDown,
 *   Home and End move between items; Tab closes the menu and moves on from
 *   the anchor; Escape closes it and focus goes back to the anchor.
 * - Popovers and dialogs share one stack: Escape closes only the topmost.
 * - z-index is `Z.popover` (70) from the layering scale.
 *
 * @param {object}  props
 * @param {React.RefObject} props.anchorRef   element the menu is anchored to
 * @param {boolean} props.open
 * @param {Function} [props.onRequestClose]   fired on outside pointer-down / Escape
 * @param {'left'|'right'} [props.align='left'] which edge lines up with the anchor
 * @param {'bottom'|'top'} [props.placement='bottom'] open below the anchor (default) or above it
 * @param {number}  [props.gap=4]             px between anchor edge and menu
 * @param {'anchored'|'auto'|'sheet'} [props.presentation='anchored']
 * @param {string}  [props.sheetClassName]    classes for the content in the sheet
 */
export default function Popover({
  anchorRef,
  open,
  onRequestClose,
  align = 'left',
  placement = 'bottom',
  gap = 4,
  hoverBridge = false,
  presentation = 'anchored',
  sheetClassName = '',
  className = '',
  style,
  children,
  ...rest
}: PopoverProps): React.ReactPortal | null {
  const ref = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState<PopoverPos | null>(null);
  const narrow = useMediaQuery(NARROW_QUERY);
  const sheet = presentation === 'sheet' || (presentation === 'auto' && narrow);
  const isMenu = rest.role === 'menu';

  // Keep the latest onRequestClose in a ref so the dismiss/observer effects can
  // call it without listing it as a dependency. Callers routinely pass an inline
  // arrow (new identity every render); without this the document-level listeners
  // would be torn down and re-added on each parent re-render while open.
  const onRequestCloseRef = useRef(onRequestClose);
  onRequestCloseRef.current = onRequestClose;

  const place = useCallback(() => {
    const a = anchorRef?.current;
    // The anchor vanished (conditionally rendered away, list item removed) while
    // we were open: ask to close instead of silently leaving a stale floating
    // menu pinned at its last coordinates.
    if (!a) {
      onRequestCloseRef.current?.();
      return;
    }
    const r = a.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    // Measure the menu's own box so we can flip/clamp against the viewport.
    const m = ref.current?.getBoundingClientRect();
    const mw = m?.width || 0;
    const mh = m?.height || 0;

    // Vertical: flip to the opposite side if there isn't room on the preferred
    // one but there is on the other.
    let vPlacement = placement;
    if (mh) {
      const roomBelow = vh - r.bottom - gap;
      const roomAbove = r.top - gap;
      if (placement === 'bottom' && roomBelow < mh && roomAbove > roomBelow) vPlacement = 'top';
      else if (placement === 'top' && roomAbove < mh && roomBelow > roomAbove)
        vPlacement = 'bottom';
    }

    // Horizontal: flip alignment if the chosen edge would push the menu off the
    // opposite side of the viewport.
    let hAlign = align;
    if (mw) {
      if (align === 'left' && r.left + mw > vw && r.right - mw >= 0) hAlign = 'right';
      else if (align === 'right' && r.right - mw < 0 && r.left + mw <= vw) hAlign = 'left';
    }

    // Compute fixed coords, then clamp into the viewport so a too-tall/too-wide
    // menu (or one near an edge) can't render off-screen.
    let top = vPlacement === 'top' ? null : r.bottom + gap;
    let bottom = vPlacement === 'top' ? vh - r.top + gap : null;
    let left = hAlign === 'right' ? null : r.left;
    let right = hAlign === 'right' ? vw - r.right : null;

    if (mw) {
      if (left != null) left = Math.max(0, Math.min(left, vw - mw));
      if (right != null) right = Math.max(0, Math.min(right, vw - mw));
    }
    if (mh) {
      if (top != null) top = Math.max(0, Math.min(top, vh - mh));
      if (bottom != null) bottom = Math.max(0, Math.min(bottom, vh - mh));
    }

    setPos({ top, bottom, left, right, maxHeight: Math.max(0, vh - gap * 2) });
  }, [anchorRef, gap, placement, align]);

  const anchored = open && !sheet;

  // Measure before paint (avoids a flash at 0,0) and keep tracking the anchor.
  // place() reads the menu's own box (ref.current) to flip/clamp, but on the very
  // first pass the menu isn't in the DOM yet (pos null ⇒ render returns null). The
  // `hasPosition` dependency below re-runs this effect once setPos mounts the menu,
  // so the ResizeObserver actually attaches to ref.current and a self re-place runs.
  const hasPosition = pos != null;
  useLayoutEffect(() => {
    if (!anchored) return undefined;
    place();
    const onMove = (): void => place();
    window.addEventListener('scroll', onMove, true);
    window.addEventListener('resize', onMove);
    // Re-place when the menu's own size changes (content/measurement settles) or
    // when the anchor moves/leaves the viewport (a scroll in an unrelated
    // container, or programmatic DOM changes that window 'scroll' won't catch).
    let ro: ResizeObserver | undefined;
    let io: IntersectionObserver | undefined;
    if (typeof ResizeObserver !== 'undefined') {
      ro = new ResizeObserver(onMove);
      if (ref.current) ro.observe(ref.current);
      if (anchorRef?.current) ro.observe(anchorRef.current);
    }
    if (typeof IntersectionObserver !== 'undefined' && anchorRef?.current) {
      io = new IntersectionObserver(onMove);
      io.observe(anchorRef.current);
    }
    return () => {
      window.removeEventListener('scroll', onMove, true);
      window.removeEventListener('resize', onMove);
      ro?.disconnect();
      io?.disconnect();
    };
    // `pos != null` is intentional: it flips false→true exactly once (after the
    // first setPos mounts the menu), re-running the effect so ro.observe(ref.current)
    // attaches to the now-mounted node. It can't loop — the boolean stays true.
  }, [anchored, place, anchorRef, hasPosition]);

  // Dismiss on outside click or Escape. Clicks on the anchor or inside the menu
  // are ignored so the trigger's own toggle/hover handlers stay in charge.
  // onRequestClose is read from a ref so inline-arrow callers don't cause the
  // listeners to re-bind on every parent re-render. Escape goes to the topmost
  // layer only (a menu over a dialog closes before the dialog).
  useEffect(() => {
    if (!anchored) return undefined;
    const token = pushLayer('popover');
    const onDown = (e: MouseEvent): void => {
      const a = anchorRef?.current;
      const target = e.target as Node | null;
      if ((target && ref.current?.contains(target)) || (target && a?.contains(target))) return;
      onRequestCloseRef.current?.();
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== 'Escape' || !isTopLayer(token)) return;
      if (isMenu && ref.current?.contains(document.activeElement)) anchorRef?.current?.focus();
      onRequestCloseRef.current?.();
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
      popLayer(token);
    };
  }, [anchored, anchorRef, isMenu]);

  // role="menu", anchored: the first item takes focus once the menu is placed,
  // and focus goes back to the anchor if the menu closes while it holds focus
  // (an item ran, Escape) — not when the user clicked elsewhere.
  const focusInside = useRef(false);
  useEffect(() => {
    if (!anchored || !isMenu || !hasPosition) return undefined;
    focusInside.current = false;
    menuItems(ref.current)[0]?.focus({ preventScroll: true });
    const anchor = anchorRef?.current;
    return () => {
      const active = document.activeElement;
      if (focusInside.current && (!active || active === document.body)) {
        anchor?.focus({ preventScroll: true });
      }
    };
  }, [anchored, isMenu, hasPosition, anchorRef]);

  // Sheet: dialog behavior on the panel.
  const sheetRef = useDialog<HTMLDivElement>({
    open: open && sheet,
    onClose: () => onRequestCloseRef.current?.(),
  });
  const tc = useT('common');
  const drag = useRef<{ id: number; y: number; dy: number } | null>(null);
  const dragged = useRef(false);

  // Leaving the anchored presentation (closed, or the window crossed the
  // breakpoint into the sheet) drops the stale position, so the next anchored
  // open measures again.
  useEffect(() => {
    if (!anchored) setPos(null);
  }, [anchored]);

  const onMenuKeyDown = (e: React.KeyboardEvent<HTMLDivElement>): void => {
    rest.onKeyDown?.(e);
    if (!isMenu || e.defaultPrevented) return;
    if (e.key === 'Tab') {
      // Close, and let the browser move on from the anchor (APG menu button).
      if (!sheet) {
        anchorRef?.current?.focus();
        onRequestCloseRef.current?.();
      }
      return;
    }
    const items = menuItems(e.currentTarget);
    if (!items.length) return;
    const i = items.indexOf(document.activeElement as HTMLElement);
    let next: number;
    if (e.key === 'ArrowDown') next = i < 0 ? 0 : (i + 1) % items.length;
    else if (e.key === 'ArrowUp')
      next = i < 0 ? items.length - 1 : (i - 1 + items.length) % items.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = items.length - 1;
    else return;
    e.preventDefault();
    items[next].focus();
  };

  const trackFocus = {
    onFocus: (e: React.FocusEvent<HTMLDivElement>) => {
      focusInside.current = true;
      rest.onFocus?.(e);
    },
    onBlur: (e: React.FocusEvent<HTMLDivElement>) => {
      const to = e.relatedTarget as Node | null;
      if (to && !e.currentTarget.contains(to)) focusInside.current = false;
      rest.onBlur?.(e);
    },
  };

  if (!open) return null;

  if (sheet) {
    const setOffset = (dy: number, animate: boolean): void => {
      const panel = sheetRef.current;
      if (!panel) return;
      panel.style.transition = animate ? 'transform var(--dur-2) var(--ease-out)' : 'none';
      panel.style.transform = dy > 0 ? `translateY(${dy}px)` : '';
    };
    return createPortal(
      <div className="fixed inset-0" style={{ zIndex: Z.popover }}>
        <div
          aria-hidden="true"
          data-testid="popover-scrim"
          className="u-backdrop-in absolute inset-0 bg-black/50"
          onClick={() => onRequestCloseRef.current?.()}
        />
        <div
          ref={sheetRef}
          role="dialog"
          aria-modal="true"
          aria-label={rest['aria-label'] ?? tc('options')}
          tabIndex={-1}
          className="u-sheet u-sheet-in absolute inset-x-0 bottom-0 flex max-h-[85dvh] flex-col rounded-t-xl border-t border-strong bg-elevated shadow-2xl"
          style={{ paddingBottom: 'env(safe-area-inset-bottom)' }}
        >
          <div
            {...rest}
            {...trackFocus}
            onKeyDown={onMenuKeyDown}
            className={`min-h-0 flex-1 overflow-y-auto overscroll-contain px-2 pb-2 ${sheetClassName}`}
          >
            {children}
          </div>
          {/* After the content in DOM order, so focus lands on the first item;
              shown first. Tap closes; dragging down past 80px closes. */}
          <button
            type="button"
            aria-label={tc('close')}
            data-testid="popover-sheet-handle"
            className="order-first flex h-7 w-full shrink-0 cursor-grab touch-none items-center justify-center rounded-t-xl"
            onPointerDown={(e) => {
              drag.current = { id: e.pointerId, y: e.clientY, dy: 0 };
              dragged.current = false;
              e.currentTarget.setPointerCapture?.(e.pointerId);
            }}
            onPointerMove={(e) => {
              const d = drag.current;
              if (!d || d.id !== e.pointerId) return;
              d.dy = Math.max(0, e.clientY - d.y);
              if (d.dy > 4) dragged.current = true;
              setOffset(d.dy, false);
            }}
            onPointerUp={(e) => {
              const d = drag.current;
              drag.current = null;
              if (!d || d.id !== e.pointerId) return;
              if (d.dy > SHEET_DISMISS_PX) onRequestCloseRef.current?.();
              else setOffset(0, true);
            }}
            onPointerCancel={() => {
              drag.current = null;
              setOffset(0, true);
            }}
            onClick={() => {
              if (dragged.current) {
                dragged.current = false;
                return;
              }
              onRequestCloseRef.current?.();
            }}
          >
            <span aria-hidden="true" className="h-1 w-9 rounded-full bg-[#4a4a4a]" />
          </button>
        </div>
      </div>,
      document.body,
    );
  }

  if (!pos) return null;

  // place() always sets exactly one of left/right (and one of top/bottom) to a
  // number and the other to null, so the non-null branch here is guaranteed
  // present; the `!` reflects that invariant and keeps the value out of CSS's
  // null-incompatible property types.
  const horizontal: React.CSSProperties =
    pos.right != null ? { right: pos.right } : { left: pos.left! };
  const vertical: React.CSSProperties =
    pos.top != null ? { top: pos.top } : { bottom: pos.bottom! };
  // Coherent entrance: if the caller didn't already supply one of the shared
  // motion entrances, fall back to a subtle directional fade matching the open
  // direction (slides up from a 'top'-placed anchor, down otherwise).
  const hasEntrance = /\bu-(fade|scale|pop)-in/.test(className);
  const entrance = hasEntrance ? '' : placement === 'top' ? 'u-fade-in-up' : 'u-fade-in-down';
  return createPortal(
    <div
      ref={ref}
      className={[entrance, className].filter(Boolean).join(' ')}
      style={{
        position: 'fixed',
        ...vertical,
        ...horizontal,
        // Cap tall menus to the viewport and let them scroll, so a long list can
        // never extend past the top/bottom edge. Callers can still override.
        maxHeight: pos.maxHeight,
        overflow: 'auto',
        zIndex: Z.popover,
        ...style,
      }}
      {...rest}
      {...(isMenu ? { ...trackFocus, onKeyDown: onMenuKeyDown } : null)}
    >
      {/* Invisible strip spanning the gap to the anchor, so a hover-opened menu
          doesn't close while the cursor crosses the empty space between them. */}
      {hoverBridge && (
        <span
          aria-hidden="true"
          className="absolute left-0 right-0"
          style={
            placement === 'top'
              ? { bottom: -(gap + 4), height: gap + 4 }
              : { top: -(gap + 4), height: gap + 4 }
          }
        />
      )}
      {children}
    </div>,
    document.body,
  );
}
