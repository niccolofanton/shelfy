import React from 'react';
import { ZoomOut, ZoomIn } from 'lucide-react';
import { useGridSize, useGridShortcuts, shortcutHint } from '../hooks/useGridSize';
import { useT } from '../i18n';

// Two-button zoom control for the post grid density. Shared across Gallery, AI
// Search and Tags Explorer via useGridSize, so the preference is global and
// persisted. ZoomOut → smaller cards (more columns); ZoomIn → bigger cards
// (fewer columns). Also wires the global Cmd/Ctrl +/- keyboard shortcuts.
export default function GridSizeControl({ className = '' }) {
  const tc = useT('common');
  const { larger, smaller, canEnlarge, canShrink } = useGridSize();
  useGridShortcuts();

  const btn =
    'p-1 rounded-md text-[#888] hover:text-white hover:bg-[#222] disabled:opacity-30 disabled:hover:bg-transparent disabled:hover:text-[#888] u-press';

  return (
    <div className={`flex items-center gap-0.5 ${className}`} data-testid="grid-size-control">
      <button
        type="button"
        onClick={smaller}
        disabled={!canShrink}
        title={`${tc('gridShrink')} (${shortcutHint} −)`}
        aria-label={tc('gridShrink')}
        className={btn}
      >
        <ZoomOut size={16} strokeWidth={1.75} />
      </button>
      <button
        type="button"
        onClick={larger}
        disabled={!canEnlarge}
        title={`${tc('gridEnlarge')} (${shortcutHint} +)`}
        aria-label={tc('gridEnlarge')}
        className={btn}
      >
        <ZoomIn size={16} strokeWidth={1.75} />
      </button>
    </div>
  );
}
