import React from 'react';
import { Grid3X3, Search, Settings, Sparkles } from 'lucide-react';
import { useT } from '../i18n';

// Mobile bottom navigation (web port plan §2.17, under 900px): Library,
// Search and Settings, with the AI tab hidden by capability until P3 (G7's
// sibling decision for the shell — see IMPLEMENTATION-PLAN.md §2.17 and the
// P1-02 card). A normal (non-fixed) flex child, shrink-0 at the bottom of
// App's narrow column — the surrounding layout reserves its height, so it
// never floats over the content above it.
//
// Hidden on the desktop app and on the web at ≥900px via the `narrow:` Tailwind
// screen (tailwind.config.ts) — `hidden narrow:flex` only ADDS the narrow
// behavior, so nothing here can affect the ≥900px layout.
export type BottomNavTarget = 'library' | 'search' | 'settings' | 'ai';

interface BottomNavProps {
  active: BottomNavTarget | null;
  onNavigate: (target: BottomNavTarget) => void;
  // The AI tab exists in the markup but stays hidden until the client reports
  // the `ai` capability (P3) — mirrors how the Sidebar's own AI group is gated.
  aiVisible?: boolean;
}

interface Tab {
  id: BottomNavTarget;
  Icon: typeof Grid3X3;
}

const TABS: Tab[] = [
  { id: 'library', Icon: Grid3X3 },
  { id: 'search', Icon: Search },
  { id: 'settings', Icon: Settings },
];

const AI_TAB: Tab = { id: 'ai', Icon: Sparkles };

function BottomNav({ active, onNavigate, aiVisible = false }: BottomNavProps): React.JSX.Element {
  const t = useT('nav');
  const tabs = aiVisible ? [...TABS.slice(0, 2), AI_TAB, TABS[2]] : TABS;

  return (
    <nav
      data-testid="bottom-nav"
      className="hidden narrow:flex w-full shrink-0 items-stretch bg-[#111111] border-t border-[#2e2e2e] select-none"
      style={{ paddingBottom: 'env(safe-area-inset-bottom)' }}
    >
      {tabs.map(({ id, Icon }) => {
        const isActive = active === id;
        return (
          <button
            key={id}
            data-testid={`bottom-nav-${id}`}
            aria-current={isActive ? 'page' : undefined}
            onClick={() => onNavigate(id)}
            className={[
              'u-press flex-1 flex flex-col items-center justify-center gap-0.5 py-2 text-[11px] transition-colors',
              isActive ? 'text-white' : 'text-gray-500 hover:text-gray-300',
            ].join(' ')}
          >
            <Icon size={20} strokeWidth={isActive ? 2.25 : 1.75} />
            {t(id)}
          </button>
        );
      })}
    </nav>
  );
}

export default React.memo(BottomNav);
