import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import PostCard from './PostCard';
import { useGridSize } from '../hooks/useGridSize';
import type { SearchPhase } from '../hooks/useSearchTransition';

// ─── Infinite 2D canvas ───────────────────────────────────────────────────────
// A free-pan / zoom surface that tiles the loaded posts INFINITELY in every
// direction (the "cosmos" wall). It's the date-order-OFF alternative to the
// row-virtualized VirtualPostGrid and shares the same PostCard, so a tile keeps
// the hover preview, identity chips and selection behaviour.
//
// Smoothness is the whole point, so the hot path never touches React:
//   • ONE compositor transform — `translate(tx,ty) scale(s)` on a single "world"
//     layer — carries every pan and zoom. Cards are never re-laid-out or
//     re-painted while moving; only that one transform changes on the GPU.
//   • A single requestAnimationFrame loop integrates inertia (drag-release
//     momentum) and eases the zoom toward its target, writing the transform
//     IMPERATIVELY (style.transform) — no setState per frame.
//   • Tiles are culled to the viewport (+ overscan). React only re-renders when
//     the pan/zoom crosses a CELL boundary and the visible tile SET changes, not
//     on every frame — so a pan within a cell is pure compositor work.
//   • Every rate (friction, zoom ease) is integrated against the real frame `dt`,
//     so the feel is identical at 60Hz and 120Hz and never "jumps" after a stall.
//
// Per the perf note shared with the grid (see PostCard / VirtualPostGrid): no
// per-card backdrop-filter exists, so a wall of ~100 mounted tiles composites
// cheaply under continuous motion.

interface InfiniteCanvasProps {
  posts: Shelfy.Post[];
  // Ordered search transition phase (Gallery's useSearchSequence): 'out' fades the
  // whole wall to 0, 'in'/'idle' shows it. Gallery swaps the pool to the new
  // results while we're at 0, so the set never changes on screen mid-fade.
  transitionPhase?: SearchPhase;
  onOpen: (post: Shelfy.Post, event?: React.SyntheticEvent) => void;
  selectable?: boolean;
  selected?: Set<string>;
  onQuickSelect?: (post: Shelfy.Post, event: React.SyntheticEvent) => void;
  testId?: string;
}

// ── Geometry / feel constants ────────────────────────────────────────────────
const GAP = 14; // px gap between tiles at scale 1
const OVERSCAN = 2; // extra rings of cells mounted beyond the viewport edge
const MIN_SCALE = 0.45;
const MAX_SCALE = 2.6;
// Drag-release momentum: fraction of velocity KEPT per 1/60s frame. Integrated
// frame-rate-independently below (pow(FRICTION, dt*60)).
const FRICTION = 0.935;
const STOP_SPEED = 0.012; // px/ms — below this the fling is considered stopped
// Exponential zoom smoothing rate (higher = snappier settle). Applied as
// 1 - exp(-ZOOM_EASE * dt) so it's frame-rate independent.
const ZOOM_EASE = 18;
const WHEEL_ZOOM = 0.0015; // mouse-wheel → scale sensitivity
const PINCH_ZOOM = 0.012; // trackpad pinch (ctrlKey wheel) is finer-grained
const CLICK_SLOP = 5; // px of movement below which a press counts as a click, not a pan

// Base tile size (at scale 1) for each shared density step. Mirrors the spirit
// of the grid's density control: a higher step → smaller tiles.
function cardSizeForStep(step: number): number {
  return Math.max(120, Math.min(360, Math.round(248 - step * 26)));
}

const posMod = (n: number, m: number): number => ((n % m) + m) % m;
const clamp = (v: number, lo: number, hi: number): number => Math.max(lo, Math.min(hi, v));

const prefersReducedMotion = (): boolean =>
  typeof window !== 'undefined' &&
  typeof window.matchMedia === 'function' &&
  window.matchMedia('(prefers-reduced-motion: reduce)').matches;

// A single rendered tile. Memoized so a pan that only adds/removes EDGE tiles
// never re-renders the interior ones (stable key + stable numeric props).
interface TileProps {
  post: Shelfy.Post;
  left: number;
  top: number;
  size: number;
  selectable: boolean;
  selected: boolean;
  firstPaint: boolean;
  onOpen: (post: Shelfy.Post, event?: React.SyntheticEvent) => void;
  onQuickSelect?: (post: Shelfy.Post, event: React.SyntheticEvent) => void;
}
const Tile = React.memo(function Tile({
  post,
  left,
  top,
  size,
  selectable,
  selected,
  firstPaint,
  onOpen,
  onQuickSelect,
}: TileProps): React.JSX.Element {
  return (
    <div
      // `grid` stretches the single child to fill the box so PostCard's aspect-square
      // resolves to exactly `size` (mirrors VirtualPostGrid). Search transitions are
      // the world-level opacity crossfade (see below), not per-tile; tiles only carry
      // the soft first-paint entrance.
      className={`absolute grid ${firstPaint ? 'u-canvas-tile' : ''}`}
      style={{ left, top, width: size, height: size }}
    >
      <PostCard
        post={post}
        onOpen={onOpen}
        selectable={selectable}
        selected={selected}
        onQuickSelect={onQuickSelect}
      />
    </div>
  );
});

function InfiniteCanvas({
  posts,
  transitionPhase = 'idle',
  onOpen,
  selectable = false,
  selected,
  onQuickSelect,
  testId,
}: InfiniteCanvasProps): React.JSX.Element | null {
  const { step } = useGridSize();
  const cardSize = cardSizeForStep(step);
  const cell = cardSize + GAP;

  const viewportRef = useRef<HTMLDivElement | null>(null);
  const worldRef = useRef<HTMLDivElement | null>(null);

  // Camera (world→screen): a world point (wx,wy) shows at (tx + wx*s, ty + wy*s).
  // Kept in a ref and mutated in place — the rAF loop owns it, React never reads
  // it for the transform.
  const camRef = useRef({ tx: 0, ty: 0, scale: 1 });
  const targetScaleRef = useRef(1);
  const zoomAnchorRef = useRef({ x: 0, y: 0 }); // viewport-local screen px the zoom pivots around
  const velRef = useRef({ vx: 0, vy: 0 }); // px/ms, drag-release momentum
  const sizeRef = useRef({ w: 0, h: 0 });

  // Per-column vertical phase — a deterministic stagger that gives the uniform
  // square lattice a Pinterest "bricks don't line up" feel WITHOUT breaking the
  // (per-column) culling math. Periodic in COLS so the infinite tiling stays
  // seamless. Recomputed only when the pool size changes.
  const n = posts.length;
  const cols = useMemo(() => Math.max(1, Math.ceil(Math.sqrt(Math.max(1, n)))), [n]);
  const colPhase = useMemo(() => {
    const arr = new Array<number>(cols);
    for (let k = 0; k < cols; k++) {
      // Golden-ratio multiplier → well-spread, repeatable fractions in [0, 1);
      // scaled to [0, cell) at cull time. Periodic in COLS → seamless tiling.
      arr[k] = posMod(k * 0.61803398875, 1);
    }
    return arr;
  }, [cols]);

  // The culled tile set currently in the DOM. Rebuilt only on a boundary cross.
  interface TileDesc {
    key: string;
    index: number;
    left: number;
    top: number;
  }
  const [tiles, setTiles] = useState<TileDesc[]>([]);
  const boundsRef = useRef<{ x0: number; x1: number; y0: number; y1: number } | null>(null);

  // First-paint entrance gate: only the tiles present on mount get the soft
  // fade-in. Tiles revealed later by panning appear instantly, so the wall
  // doesn't "pop" at its edges mid-fling. Cleared on first interaction / after a
  // short grace window. Held in a ref for the loop, mirrored to state for render.
  const [firstPaint, setFirstPaint] = useState(true);
  const firstPaintRef = useRef(true);
  firstPaintRef.current = firstPaint;

  // Latest props the loop / handlers read without re-subscribing.
  const postsRef = useRef(posts);
  postsRef.current = posts;
  const selectedRef = useRef(selected);
  selectedRef.current = selected;
  const cellRef = useRef(cell);
  const colsRef = useRef(cols);
  const colPhaseRef = useRef(colPhase);
  colsRef.current = cols;
  colPhaseRef.current = colPhase;

  // ── Transform write ─────────────────────────────────────────────────────────
  const applyTransform = useCallback(() => {
    const el = worldRef.current;
    if (!el) return;
    const { tx, ty, scale } = camRef.current;
    // translate before scale (transform-origin 0 0) keeps the maths above exact.
    el.style.transform = `translate3d(${tx}px, ${ty}px, 0) scale(${scale})`;
  }, []);

  // ── Tile culling ─────────────────────────────────────────────────────────────
  // Compute the integer cell window visible (plus overscan) for the current
  // camera; rebuild `tiles` only when that window actually changed.
  const retile = useCallback(() => {
    const { w, h } = sizeRef.current;
    if (w === 0 || h === 0) return;
    const { tx, ty, scale } = camRef.current;
    const C = cellRef.current;
    // Visible world rectangle.
    const wx0 = -tx / scale;
    const wy0 = -ty / scale;
    const wx1 = (w - tx) / scale;
    const wy1 = (h - ty) / scale;
    const x0 = Math.floor(wx0 / C) - OVERSCAN;
    const x1 = Math.floor(wx1 / C) + OVERSCAN;
    // Columns are phase-shifted by up to ~1 cell, so widen the row range by one.
    const y0 = Math.floor(wy0 / C) - OVERSCAN - 1;
    const y1 = Math.floor(wy1 / C) + OVERSCAN;

    const prev = boundsRef.current;
    if (prev && prev.x0 === x0 && prev.x1 === x1 && prev.y0 === y0 && prev.y1 === y1) {
      return; // window unchanged — no React work
    }
    boundsRef.current = { x0, x1, y0, y1 };

    const N = postsRef.current.length;
    const COLS = colsRef.current;
    const phase = colPhaseRef.current;
    const next: TileDesc[] = [];
    for (let cx = x0; cx <= x1; cx++) {
      const ph = phase[posMod(cx, COLS)] * C;
      for (let cy = y0; cy <= y1; cy++) {
        // Modular cell→post mapping: fills every cell, with a per-row horizontal
        // shift so duplicates never line up as obvious tiling seams.
        const index = posMod(cy * COLS + cx, N);
        next.push({
          key: `${cx}:${cy}`,
          index,
          left: cx * C,
          top: cy * C + ph,
        });
      }
    }
    setTiles(next);
  }, []);

  // ── rAF engine ───────────────────────────────────────────────────────────────
  // Runs only while there's work (dragging, momentum, or an in-flight zoom), then
  // parks itself. `wake()` (re)starts it after any interaction.
  const rafRef = useRef<number | null>(null);
  const lastTsRef = useRef(0);
  const draggingRef = useRef(false);
  const reduceRef = useRef(false);

  const tick = useCallback(
    (ts: number) => {
      const last = lastTsRef.current || ts;
      // Clamp dt so a background-tab stall (huge gap) doesn't teleport the camera.
      const dt = Math.min(50, ts - last);
      lastTsRef.current = ts;
      const cam = camRef.current;
      let busy = false;

      // 1) Inertia (only when not actively dragging).
      const vel = velRef.current;
      if (
        !draggingRef.current &&
        (Math.abs(vel.vx) > STOP_SPEED || Math.abs(vel.vy) > STOP_SPEED)
      ) {
        cam.tx += vel.vx * dt;
        cam.ty += vel.vy * dt;
        const decay = Math.pow(FRICTION, dt / (1000 / 60));
        vel.vx *= decay;
        vel.vy *= decay;
        busy = true;
      } else if (!draggingRef.current) {
        vel.vx = 0;
        vel.vy = 0;
      }

      // 2) Zoom easing toward target, pinned at the anchor screen point so the
      //    content under the cursor stays put as the scale glides.
      const target = targetScaleRef.current;
      if (Math.abs(cam.scale - target) > 0.0006) {
        const a = reduceRef.current ? 1 : 1 - Math.exp(-ZOOM_EASE * (dt / 1000));
        const newScale = cam.scale + (target - cam.scale) * a;
        const k = newScale / cam.scale;
        const { x: ax, y: ay } = zoomAnchorRef.current;
        cam.tx = ax - (ax - cam.tx) * k;
        cam.ty = ay - (ay - cam.ty) * k;
        cam.scale = newScale;
        busy = true;
      } else if (cam.scale !== target) {
        // Final settle of the last sub-0.0006 gap: snap scale EXACTLY to target
        // (so this branch can't re-fire on a later unrelated wake) without
        // re-anchoring — at k≈1 the position compensation is sub-pixel, and using
        // the stale zoom anchor here would nudge the camera during a plain pan.
        cam.scale = target;
        busy = true;
      }

      applyTransform();
      retile();

      if (busy || draggingRef.current) {
        rafRef.current = requestAnimationFrame(tick);
      } else {
        rafRef.current = null;
        lastTsRef.current = 0;
      }
    },
    [applyTransform, retile],
  );

  const wake = useCallback(() => {
    if (rafRef.current == null) {
      lastTsRef.current = 0;
      rafRef.current = requestAnimationFrame(tick);
    }
  }, [tick]);

  // ── Viewport sizing ──────────────────────────────────────────────────────────
  useEffect(() => {
    reduceRef.current = prefersReducedMotion();
    const el = viewportRef.current;
    if (!el) return undefined;
    const measure = (): void => {
      const r = el.getBoundingClientRect();
      const first = sizeRef.current.w === 0;
      sizeRef.current = { w: r.width, h: r.height };
      if (first && r.width > 0) {
        // Center the very first cell cluster so the wall opens balanced, not from
        // a corner.
        camRef.current.tx = r.width / 2;
        camRef.current.ty = r.height / 2;
      }
      applyTransform();
      // Force a retile against the new size.
      boundsRef.current = null;
      retile();
    };
    measure();
    const ro = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(measure) : null;
    ro?.observe(el);
    return () => ro?.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Pool changed (new posts loaded / density changed the cell): keep the view
  // centered and re-cull. On a density change, scale the camera around the
  // viewport centre so tiles resize in place instead of drifting to a corner.
  const prevCellRef = useRef(cell);
  useEffect(() => {
    cellRef.current = cell;
    const { w, h } = sizeRef.current;
    const prev = prevCellRef.current;
    if (prev !== cell && w > 0) {
      const cam = camRef.current;
      const k = cell / prev;
      const cx = w / 2;
      const cy = h / 2;
      cam.tx = cx - (cx - cam.tx) * k;
      cam.ty = cy - (cy - cam.ty) * k;
      applyTransform();
    }
    prevCellRef.current = cell;
    boundsRef.current = null; // pool/cell change → membership changed
    retile();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cell, n]);

  // First-paint grace: clear the entrance stagger after a short window or the
  // first interaction so pan-revealed tiles never re-animate.
  useEffect(() => {
    const t = setTimeout(() => setFirstPaint(false), 650);
    return () => clearTimeout(t);
  }, []);
  const endFirstPaint = useCallback(() => {
    if (firstPaintRef.current) setFirstPaint(false);
  }, []);

  // ── Pointer (drag) pan ───────────────────────────────────────────────────────
  // Velocity is estimated from a short trailing window of moves so a flick hands
  // a believable speed to the inertia integrator.
  const dragRef = useRef<{
    id: number;
    lastX: number;
    lastY: number;
    moved: number;
    samples: { t: number; x: number; y: number }[];
  } | null>(null);
  // Set when a press turned into a pan, so the trailing click doesn't open a post.
  const suppressClickRef = useRef(false);

  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (e.button !== 0) return;
      endFirstPaint();
      velRef.current.vx = 0;
      velRef.current.vy = 0;
      draggingRef.current = true;
      suppressClickRef.current = false;
      dragRef.current = {
        id: e.pointerId,
        lastX: e.clientX,
        lastY: e.clientY,
        moved: 0,
        samples: [{ t: e.timeStamp, x: e.clientX, y: e.clientY }],
      };
      try {
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      } catch {
        /* capture unsupported — the window pointerup/cancel net still ends the drag */
      }
      (e.currentTarget as HTMLElement).style.cursor = 'grabbing';
      wake();
    },
    [endFirstPaint, wake],
  );

  const onPointerMove = useCallback((e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d || e.pointerId !== d.id) return;
    const dx = e.clientX - d.lastX;
    const dy = e.clientY - d.lastY;
    d.lastX = e.clientX;
    d.lastY = e.clientY;
    d.moved += Math.abs(dx) + Math.abs(dy);
    const cam = camRef.current;
    cam.tx += dx;
    cam.ty += dy;
    // Apply immediately for zero-latency tracking; the loop handles re-culling.
    applyTransform();
    // Keep ~80ms of samples for the release-velocity estimate.
    const s = d.samples;
    s.push({ t: e.timeStamp, x: e.clientX, y: e.clientY });
    while (s.length > 2 && e.timeStamp - s[0].t > 80) s.shift();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Core drag termination — clears the drag, estimates release velocity, restores
  // the cursor. Idempotent (no-op if no drag is active) so it can be driven both
  // by the pointer handlers and by the window-level safety net below.
  const finishDrag = useCallback(() => {
    const d = dragRef.current;
    if (!d) return;
    dragRef.current = null;
    draggingRef.current = false;
    if (d.moved > CLICK_SLOP) suppressClickRef.current = true;
    // Velocity over the RECENT window only: walk back from the last sample while
    // within ~80ms, so a drag-pause-then-flick measures the flick and not a stale
    // pre-pause baseline (which would otherwise yield near-zero, dead momentum).
    const s = d.samples;
    if (!reduceRef.current && s.length >= 2) {
      const b = s[s.length - 1];
      let a = b;
      for (let i = s.length - 1; i >= 0; i--) {
        if (b.t - s[i].t > 80) break;
        a = s[i];
      }
      const dtMs = b.t - a.t;
      if (dtMs > 0) {
        velRef.current.vx = (b.x - a.x) / dtMs;
        velRef.current.vy = (b.y - a.y) / dtMs;
      }
    }
    const el = viewportRef.current;
    if (el) el.style.cursor = 'grab';
    wake();
  }, [wake]);

  const endDrag = useCallback(
    (e: React.PointerEvent) => {
      const d = dragRef.current;
      if (!d || e.pointerId !== d.id) return;
      try {
        (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
      } catch {
        /* nothing captured */
      }
      finishDrag();
    },
    [finishDrag],
  );

  // Safety net: end any active drag on a window-level pointerup/cancel or a focus
  // loss. A pointerup the viewport never receives — pointer capture denied, an
  // alt-tab or context menu mid-drag — would otherwise leave draggingRef stuck
  // true and the rAF loop spinning forever in a permanent grab.
  useEffect(() => {
    const end = (): void => finishDrag();
    window.addEventListener('pointerup', end);
    window.addEventListener('pointercancel', end);
    window.addEventListener('blur', end);
    return () => {
      window.removeEventListener('pointerup', end);
      window.removeEventListener('pointercancel', end);
      window.removeEventListener('blur', end);
    };
  }, [finishDrag]);

  // ── Wheel: pan (two-finger) + zoom (pinch / ⌘+wheel) ─────────────────────────
  // Attached natively (non-passive) so preventDefault actually suppresses the
  // page/native scroll — React's synthetic onWheel is passive and can't.
  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return undefined;
    const onWheel = (e: WheelEvent): void => {
      e.preventDefault();
      endFirstPaint();
      const rect = el.getBoundingClientRect();
      if (e.ctrlKey || e.metaKey) {
        // Pinch (trackpad → ctrlKey) or ⌘/Ctrl+wheel (mouse) → zoom at cursor.
        const sens = e.ctrlKey ? PINCH_ZOOM : WHEEL_ZOOM;
        const factor = Math.exp(-e.deltaY * sens);
        targetScaleRef.current = clamp(targetScaleRef.current * factor, MIN_SCALE, MAX_SCALE);
        zoomAnchorRef.current = { x: e.clientX - rect.left, y: e.clientY - rect.top };
      } else {
        // Plain wheel / two-finger swipe → pan. macOS already supplies momentum
        // (events keep arriving as fingers lift), so we translate directly.
        const cam = camRef.current;
        cam.tx -= e.deltaX;
        cam.ty -= e.deltaY;
        velRef.current.vx = 0;
        velRef.current.vy = 0;
        applyTransform();
      }
      wake();
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [endFirstPaint, wake]);

  // Stop the loop on unmount.
  useEffect(() => {
    return () => {
      if (rafRef.current != null) cancelAnimationFrame(rafRef.current);
    };
  }, []);

  // A press that became a pan swallows the trailing click so panning never opens
  // a post; a genuine click falls through to the tile's PostCard handler.
  const onClickCapture = useCallback((e: React.MouseEvent) => {
    if (suppressClickRef.current) {
      suppressClickRef.current = false;
      e.stopPropagation();
      e.preventDefault();
    }
  }, []);

  if (n === 0) return null;

  const sel = selected;
  return (
    <div
      ref={viewportRef}
      data-testid={testId}
      className="relative w-full h-full overflow-hidden u-canvas-in select-none"
      style={{ touchAction: 'none', cursor: 'grab' }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onClickCapture={onClickCapture}
    >
      <div
        ref={worldRef}
        className="absolute top-0 left-0"
        // The rAF loop owns `transform` (written imperatively); React only sets
        // opacity/transition here, so the two never fight. The whole wall fades to 0
        // on 'out' and back on 'in'; Gallery (useSearchSequence) swaps the pool while
        // we're hidden, so the set never changes on screen mid-fade.
        style={{
          transformOrigin: '0 0',
          willChange: 'transform',
          opacity: transitionPhase === 'out' ? 0 : 1,
          transition: 'opacity 190ms ease',
        }}
      >
        {tiles.map((tl) => {
          const post = posts[tl.index];
          if (!post) return null;
          return (
            <Tile
              key={tl.key}
              post={post}
              left={tl.left}
              top={tl.top}
              size={cardSize}
              selectable={selectable}
              selected={sel ? sel.has(post.id) : false}
              firstPaint={firstPaint}
              onOpen={onOpen}
              onQuickSelect={onQuickSelect}
            />
          );
        })}
      </div>
      {/* Soft edge vignette — keeps the infinite wall from ending in a hard cut,
        and hints "there's more out there". Non-interactive. */}
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0"
        style={{
          boxShadow: 'inset 0 0 120px 40px rgba(15,15,15,0.55)',
        }}
      />
    </div>
  );
}

export default React.memo(InfiniteCanvas);
