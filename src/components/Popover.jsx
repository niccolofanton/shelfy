import React, { useLayoutEffect, useRef, useState, useEffect, useCallback } from 'react';
import { createPortal } from 'react-dom';

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
 * @param {object}  props
 * @param {React.RefObject} props.anchorRef   element the menu is anchored to
 * @param {boolean} props.open
 * @param {Function} [props.onRequestClose]   fired on outside pointer-down / Escape
 * @param {'left'|'right'} [props.align='left'] which edge lines up with the anchor
 * @param {'bottom'|'top'} [props.placement='bottom'] open below the anchor (default) or above it
 * @param {number}  [props.gap=4]             px between anchor edge and menu
 */
export default function Popover({
  anchorRef,
  open,
  onRequestClose,
  align = 'left',
  placement = 'bottom',
  gap = 4,
  hoverBridge = false,
  className = '',
  style,
  children,
  ...rest
}) {
  const ref = useRef(null);
  const [pos, setPos] = useState(null);

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

  // Measure before paint (avoids a flash at 0,0) and keep tracking the anchor.
  // place() reads the menu's own box (ref.current) to flip/clamp, but on the very
  // first pass the menu isn't in the DOM yet (pos null ⇒ render returns null). The
  // `pos != null` dependency below re-runs this effect once setPos mounts the menu,
  // so the ResizeObserver actually attaches to ref.current and a self re-place runs.
  useLayoutEffect(() => {
    if (!open) return undefined;
    place();
    const onMove = () => place();
    window.addEventListener('scroll', onMove, true);
    window.addEventListener('resize', onMove);
    // Re-place when the menu's own size changes (content/measurement settles) or
    // when the anchor moves/leaves the viewport (a scroll in an unrelated
    // container, or programmatic DOM changes that window 'scroll' won't catch).
    let ro;
    let io;
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
  }, [open, place, anchorRef, pos != null]);

  // Dismiss on outside click or Escape. Clicks on the anchor or inside the menu
  // are ignored so the trigger's own toggle/hover handlers stay in charge.
  // onRequestClose is read from a ref so inline-arrow callers don't cause the
  // listeners to re-bind on every parent re-render.
  useEffect(() => {
    if (!open) return undefined;
    const onDown = (e) => {
      const a = anchorRef?.current;
      if (ref.current?.contains(e.target) || a?.contains(e.target)) return;
      onRequestCloseRef.current?.();
    };
    const onKey = (e) => {
      if (e.key === 'Escape') onRequestCloseRef.current?.();
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open, anchorRef]);

  if (!open || !pos) return null;

  const horizontal = pos.right != null ? { right: pos.right } : { left: pos.left };
  const vertical = pos.top != null ? { top: pos.top } : { bottom: pos.bottom };
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
        zIndex: 200,
        ...style,
      }}
      {...rest}
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
