import React, { useState, useEffect, useMemo } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import PostCard from './PostCard';
import { useGridSize, applyStep } from '../hooks/useGridSize';

// Row-virtualized grid of PostCards, shared by every post surface (Gallery,
// AI Search results, Tags Explorer results). A grid of thousands of cards keeps
// every <img>/<video> alive and stutters scrolling; here only the rows in (or
// near) the viewport are mounted. The column count mirrors the responsive
// breakpoints used across the app and is re-derived on resize.
//
// The parent owns the scroll container and passes its ref via `scrollRef` (so it
// can also render loading/empty states inside the same scroller); this component
// renders only the windowed rows.

// Grid spacing, in px — must mirror the Tailwind classes used in renderRow:
// p-2 (container padding, 8px each side) and gap-2 / pb-2 (8px column & row gaps).
const GRID_PAD = 8;
const GRID_GAP = 8;

export function colsForWidth(w: number): number {
  if (w >= 1280) return 6; // xl
  if (w >= 1024) return 5; // lg
  if (w >= 768) return 4; // md
  if (w >= 640) return 3; // sm
  return 2;
}

interface VirtualPostGridProps {
  posts: Shelfy.Post[];
  scrollRef: React.RefObject<HTMLDivElement | null> | null;
  onOpen: (post: Shelfy.Post, event?: React.SyntheticEvent) => void;
  selectable?: boolean;
  selected?: Set<string>; // Set<id> | undefined
  // Moderate overscan (~1.5 viewports per side at default density): rows mount —
  // and their images start fetching, see PostCard's eager loading — before they
  // scroll into view. Higher values multiply offscreen mounts per column (16
  // rows ≈ 100-160 cards per side at high density) and turn fast flings into
  // request/decode bursts that defeat the pre-warm they were meant to provide.
  overscan?: number;
  testId?: string;
  onGridMouseDownCapture?: (e: React.MouseEvent) => void;
  // Bubbling mouseover across the grid — used by the Gallery's drag-select to
  // extend the selection while the mouse sweeps over cards (iPhone-Photos-like).
  onGridMouseOver?: (e: React.MouseEvent) => void;
  // Optional hover-checkbox shortcut (Google-Photos-like): forwarded to each
  // PostCard so a surface can enter select mode straight from a card without
  // first toggling the toolbar's Select button.
  onQuickSelect?: (post: Shelfy.Post, event: React.SyntheticEvent) => void;
}

function VirtualPostGrid({
  posts,
  scrollRef,
  onOpen,
  selectable = false,
  selected, // Set<id> | undefined
  overscan = 6,
  testId,
  onGridMouseDownCapture,
  onGridMouseOver,
  onQuickSelect,
}: VirtualPostGridProps): React.JSX.Element | null {
  // Shared density offset (zoom in/out) — applied on top of the responsive
  // breakpoint columns so the choice is consistent across every post surface.
  const { step } = useGridSize();
  const [baseCols, setBaseCols] = useState<number>(() =>
    colsForWidth(typeof window !== 'undefined' ? window.innerWidth : 1280),
  );
  // The scroller's CONTENT width (not window.innerWidth): it shrinks when the
  // sidebar opens or the scrollbar appears, and it's what the grid columns
  // actually divide. A ResizeObserver keeps it exact across every layout change,
  // so the deterministic row height below stays correct without re-measuring.
  const [containerWidth, setContainerWidth] = useState<number>(
    () =>
      scrollRef?.current?.clientWidth || (typeof window !== 'undefined' ? window.innerWidth : 1280),
  );
  useEffect(() => {
    const el = scrollRef?.current;
    const update = (): void => {
      setBaseCols(colsForWidth(window.innerWidth));
      if (el) setContainerWidth(el.clientWidth);
    };
    update();
    const ro = el && typeof ResizeObserver !== 'undefined' ? new ResizeObserver(update) : null;
    if (ro && el) ro.observe(el);
    window.addEventListener('resize', update);
    return () => {
      ro?.disconnect();
      window.removeEventListener('resize', update);
    };
  }, [scrollRef]);
  const colsPerRow = applyStep(baseCols, step);

  // Cards are square and every row is identical, so the row height is fully
  // DETERMINISTIC — there is nothing to measure at runtime. Compute it exactly
  // from the layout the grid uses: content width minus the p-2 padding (8px each
  // side) and the gap-2 column gaps (8px × cols-1), divided by the column count,
  // plus the pb-2 (8px) that carries the vertical row gap. `ceil` on the card side
  // keeps the fixed size from ever being SMALLER than the browser's rounded
  // render, so rows can never overlap; the residual is a sub-pixel hairline.
  // A fixed size (with measureElement dropped below) removes the per-row
  // ResizeObserver and the measure→reflow→reposition churn that made fast scrolls
  // visibly shift/overlap cards — and the old estimateSize read clientWidth on
  // every call, forcing a reflow per unmeasured row on each scroll update.
  const rowHeight = useMemo(() => {
    const inner = Math.max(1, containerWidth - GRID_PAD * 2 - GRID_GAP * (colsPerRow - 1));
    return Math.ceil(inner / colsPerRow) + GRID_GAP;
  }, [containerWidth, colsPerRow]);

  const rowCount = Math.ceil(posts.length / colsPerRow);
  const rowVirtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => scrollRef?.current ?? null,
    // Constant: no layout read per call. (The previous version read clientWidth
    // here, forcing a synchronous reflow for every unmeasured row on each scroll
    // tick — a real jank source on fast flings.)
    estimateSize: () => rowHeight,
    overscan,
  });

  // Column count changed (breakpoint OR zoom step) → row composition + heights
  // changed; discard cached row measurements so they re-measure at the new size.
  // When it's a zoom change (not the first mount / a plain resize) we briefly
  // flip `zooming` on so rows transition to their new positions and cards play a
  // soft scale-in, instead of snapping. The window matches --dur-3 (320ms).
  const [zooming, setZooming] = useState<boolean>(false);
  const didMount = React.useRef<boolean>(false);
  useEffect(() => {
    rowVirtualizer.measure();
    if (!didMount.current) {
      didMount.current = true;
      return undefined;
    }
    setZooming(true);
    const t = setTimeout(() => setZooming(false), 340);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [colsPerRow]);

  // Width-only changes (sidebar/resize, no column change) still need the
  // virtualizer to adopt the new fixed row height — but without the zoom anim.
  useEffect(() => {
    rowVirtualizer.measure();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rowHeight]);

  // First-paint stagger gate. The virtualizer mounts/unmounts row elements as the
  // user scrolls, so a blanket `u-fade-in` on every card would re-trigger the
  // entrance animation mid-scroll (cards "popping in"). We only animate the very
  // first painted rows, then disable the stagger as soon as the user scrolls (or
  // after a short grace window) so subsequently mounted rows appear instantly.
  const [firstPaint, setFirstPaint] = useState<boolean>(true);
  useEffect(() => {
    if (!firstPaint) return undefined;
    const el = scrollRef?.current;
    const stop = (): void => setFirstPaint(false);
    const t = setTimeout(stop, 600);
    el?.addEventListener('scroll', stop, { once: true, passive: true });
    return () => {
      clearTimeout(t);
      el?.removeEventListener('scroll', stop);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [firstPaint]);

  // No scroll-velocity placeholder swap: the real native-scroll jank wasn't the
  // PostCard mount cost (a fast fling reveals <1 row/frame — trivial). It was the
  // two `backdrop-filter` chips PostCard paints at rest: under the COMPOSITOR
  // scroll path (real wheel/trackpad, not the old scrollTop= harness) the backdrop
  // is re-sampled and re-blurred every frame for ~200 chip regions, pinning frames
  // at ~25ms. Dropping backdrop-filter (see PostCard's identity chips) restored a
  // steady ~95-110fps with full cards always mounted — so the placeholder gate
  // (which also janked on its own velocity oscillation: every threshold cross
  // remounted the whole viewport) is gone. Measured in e2e/perf-gallery.spec.ts.
  if (posts.length === 0) return null;

  const renderRow = (rowIndex: number): React.JSX.Element => {
    const start = rowIndex * colsPerRow;
    const rowPosts = posts.slice(start, start + colsPerRow);
    return (
      // pb-2 carries the VERTICAL gap: rows are absolutely positioned from their
      // measured height, so margins/space-y between siblings would be ignored —
      // the gap must live inside the measured row box. gap-2 spaces the columns.
      <div
        className="grid gap-2 pb-2"
        style={{ gridTemplateColumns: `repeat(${colsPerRow}, minmax(0, 1fr))` }}
      >
        {rowPosts.map((post, col) => (
          <div
            key={post.id}
            // Global index of the card in `posts` — lets the drag-select resolve
            // which card the pointer is over from the event target alone.
            data-post-index={start + col}
            // Only the initial rows get a soft staggered entrance, keyed off the
            // column so a row ripples in left→right. Once `firstPaint` clears we
            // render plain wrappers so scroll-mounted rows don't re-animate —
            // except during a zoom change, when cards scale-in to the new size.
            className={firstPaint ? 'u-fade-in' : zooming ? 'u-grid-zoom' : undefined}
            style={firstPaint ? { animationDelay: `${col * 30}ms` } : undefined}
          >
            <PostCard
              post={post}
              onOpen={onOpen}
              selectable={selectable}
              selected={selected ? selected.has(post.id) : false}
              onQuickSelect={onQuickSelect}
            />
          </div>
        ))}
      </div>
    );
  };

  const virtualRows = rowVirtualizer.getVirtualItems();
  // The virtualizer only yields a range once it has measured a non-zero viewport
  // height (e.g. jsdom under test, or the first frame before measurement yields
  // nothing) — fall back to rendering every row so the grid is never blank.
  const windowed = virtualRows.length > 0;

  return (
    <div
      data-testid={testId}
      // select-none while in select mode: a drag-select sweep must not also
      // start a native text/image selection.
      className={selectable ? 'p-2 select-none' : 'p-2'}
      onMouseDownCapture={onGridMouseDownCapture}
      onMouseOver={onGridMouseOver}
    >
      {windowed ? (
        <div style={{ height: rowVirtualizer.getTotalSize(), position: 'relative', width: '100%' }}>
          {virtualRows.map((vrow) => (
            <div
              key={vrow.key}
              data-index={vrow.index}
              // No measureElement ref: rows are a uniform, deterministic height
              // (see rowHeight above), so they're positioned purely by index ×
              // size — no per-row measurement, no reflow, no reposition churn.
              style={{
                position: 'absolute',
                top: 0,
                left: 0,
                width: '100%',
                transform: `translateY(${vrow.start}px)`,
                // Slide rows to their new offsets on a zoom change only; never
                // during scroll (would lag the virtualizer).
                transition: zooming ? 'transform var(--dur-3) var(--ease-emphasized)' : undefined,
              }}
            >
              {renderRow(vrow.index)}
            </div>
          ))}
        </div>
      ) : (
        // Bounded fallback: never mount more than ~24 rows before measurement
        // (covers the first frame and any zero-height moment) so it can't blow up.
        <div>
          {Array.from({ length: Math.min(rowCount, 24) }, (_, i) => (
            <div key={i}>{renderRow(i)}</div>
          ))}
        </div>
      )}
    </div>
  );
}

// Memoized: parents re-render on every keystroke / toolbar toggle, but the grid
// only needs to update when the posts, the selection or the handlers change.
export default React.memo(VirtualPostGrid);
