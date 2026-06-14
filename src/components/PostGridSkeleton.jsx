import React, { useState, useEffect } from 'react';
import { colsForWidth } from './VirtualPostGrid';
import { useGridSize, applyStep } from '../hooks/useGridSize';

// Shimmering placeholder grid shown while the first page of a post grid loads,
// so the view paints content-shaped immediately instead of flashing empty area
// then a spinner. Same responsive columns (incl. the shared zoom density) + gap
// as VirtualPostGrid, and the tiles are aspect-square like PostCard so there's
// no layout shift when the real cards replace them.
export default function PostGridSkeleton({ count = 18 }) {
  const { step } = useGridSize();
  const [baseCols, setBaseCols] = useState(() =>
    colsForWidth(typeof window !== 'undefined' ? window.innerWidth : 1280),
  );
  useEffect(() => {
    const update = () => setBaseCols(colsForWidth(window.innerWidth));
    update();
    window.addEventListener('resize', update);
    return () => window.removeEventListener('resize', update);
  }, []);
  const cols = applyStep(baseCols, step);

  return (
    <div
      data-testid="post-grid-skeleton"
      aria-hidden="true"
      className="grid gap-2 p-2 u-fade-in"
      style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))` }}
    >
      {Array.from({ length: count }, (_, i) => (
        <div key={i} className="aspect-square rounded-sm skeleton-tile" />
      ))}
    </div>
  );
}
