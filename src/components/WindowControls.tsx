import React, { useEffect, useState } from 'react';
import { Minus, Square, Copy, X } from 'lucide-react';

// Custom min / maximize-restore / close cluster for the frameless window on
// Windows and Linux (macOS keeps its native traffic lights). Rendered top-right
// of the app shell; the buttons are `no-drag` (via the global rule in index.css)
// so they stay clickable inside the draggable title strip. The maximize icon
// flips to a "restore" glyph while the window is maximized, kept in sync with the
// native state pushed from the main process.
export default function WindowControls(): React.JSX.Element {
  const [maximized, setMaximized] = useState<boolean>(false);

  useEffect(() => {
    const api = window.electronAPI;
    if (!api) return undefined;
    let alive = true;
    api
      .windowIsMaximized()
      .then((m) => {
        if (alive) setMaximized(m);
      })
      .catch(() => {});
    const off = api.onWindowMaximizeChange((m) => setMaximized(m));
    return () => {
      alive = false;
      off?.();
    };
  }, []);

  const btn =
    'flex items-center justify-center w-12 h-full text-gray-400 hover:text-white transition-colors';

  return (
    <div data-testid="window-controls" className="flex items-stretch h-9 shrink-0 select-none">
      <button
        type="button"
        title="Minimize"
        aria-label="Minimize"
        onClick={() => window.electronAPI?.windowMinimize()}
        className={`${btn} hover:bg-white/10`}
      >
        <Minus size={15} />
      </button>
      <button
        type="button"
        title={maximized ? 'Restore' : 'Maximize'}
        aria-label={maximized ? 'Restore' : 'Maximize'}
        onClick={() =>
          window.electronAPI
            ?.windowMaximizeToggle()
            .then((m) => setMaximized(m))
            .catch(() => {})
        }
        className={`${btn} hover:bg-white/10`}
      >
        {maximized ? <Copy size={13} /> : <Square size={12} />}
      </button>
      <button
        type="button"
        title="Close"
        aria-label="Close"
        onClick={() => window.electronAPI?.windowClose()}
        className={`${btn} hover:bg-[#e81123] hover:text-white`}
      >
        <X size={16} />
      </button>
    </div>
  );
}
