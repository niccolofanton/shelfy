import React from 'react';
import { ZoomOut, ZoomIn } from 'lucide-react';
import { useGridSize, useGridShortcuts, shortcutHint } from '../hooks/useGridSize';
import { useT } from '../i18n';

// Translator returned by useT — namespaced key + optional interpolation vars.
type Translate = (key: string, vars?: Record<string, string | number>) => string;

interface GridSizeControlProps {
  className?: string;
}

// Two-button zoom control for the post grid density. Shared across Gallery, AI
// Search and Tags Explorer via useGridSize, so the preference is global and
// persisted. ZoomOut → smaller cards (more columns); ZoomIn → bigger cards
// (fewer columns). Also wires the global Cmd/Ctrl +/- keyboard shortcuts.
export default function GridSizeControl({
  className = '',
}: GridSizeControlProps): React.JSX.Element {
  const tc: Translate = useT('common');
  const { larger, smaller, canEnlarge, canShrink } = useGridSize();
  useGridShortcuts();

  const btn =
    'p-1 rounded-md text-muted hover:text-primary hover:bg-hover disabled:opacity-30 disabled:hover:bg-transparent disabled:hover:text-muted u-press';

  // Density is a desktop/toolbar control; on narrow it moves into the filter
  // sheet's View section (audit GAL-1), so the toolbar instance hides itself.
  return (
    <div
      className={`flex items-center gap-0.5 narrow:hidden ${className}`}
      data-testid="grid-size-control"
    >
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
