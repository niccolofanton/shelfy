import React, { useLayoutEffect, useRef } from 'react';
import { Grid3X3, ListChecks, Search, Settings, Sparkles } from 'lucide-react';
import { useT } from '../i18n';

// Mobile bottom navigation (web port plan §2.17, under 900px): Library,
// Search and Settings, with the AI tab hidden by capability until P3 (G7's
// sibling decision for the shell — see IMPLEMENTATION-PLAN.md §2.17 and the
// P1-02 card), and the Jobs tab (P4-09) shown only on the web, where there is
// no Downloads. A normal (non-fixed) flex child, shrink-0 at the bottom of
// App's narrow column — the surrounding layout reserves its height, so it
// never floats over the content above it.
//
// Hidden on the desktop app and on the web at ≥900px via the `narrow:` Tailwind
// screen (tailwind.config.ts) — `hidden narrow:flex` only ADDS the narrow
// behavior, so nothing here can affect the ≥900px layout.
export type BottomNavTarget = 'library' | 'search' | 'settings' | 'ai' | 'jobs';

interface BottomNavProps {
  active: BottomNavTarget | null;
  onNavigate: (target: BottomNavTarget) => void;
  // The AI tab exists in the markup but stays hidden until the client reports
  // the `ai` capability (P3) — mirrors how the Sidebar's own AI group is gated.
  aiVisible?: boolean;
  // The Jobs tab: shown only when the client reports the `jobs` capability
  // (the web, once signed in) — mirrors the Sidebar's own Jobs row.
  jobsVisible?: boolean;
  // Out of reach while the menu drawer is open (a modal dialog, UX-1).
  inert?: boolean;
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
const JOBS_TAB: Tab = { id: 'jobs', Icon: ListChecks };

function BottomNav({
  active,
  onNavigate,
  aiVisible = false,
  jobsVisible = false,
  inert = false,
}: BottomNavProps): React.JSX.Element {
  const t = useT('nav');
  // React 18 has no `inert` prop: set the DOM property.
  const navRef = useRef<HTMLElement>(null);
  useLayoutEffect(() => {
    if (navRef.current) navRef.current.inert = inert;
  }, [inert]);
  const tabs = [
    ...TABS.slice(0, 2),
    ...(aiVisible ? [AI_TAB] : []),
    ...(jobsVisible ? [JOBS_TAB] : []),
    TABS[2],
  ];

  return (
    <nav
      ref={navRef}
      data-testid="bottom-nav"
      aria-label={t('navLabel')}
      className="hidden narrow:flex w-full shrink-0 items-stretch bg-[#111111] border-t border-[#2e2e2e] select-none"
      style={{ paddingBottom: 'env(safe-area-inset-bottom)' }}
    >
      {tabs.map(({ id, Icon }) => {
        const isActive = active === id;
        return (
          <button
            key={id}
            type="button"
            data-testid={`bottom-nav-${id}`}
            aria-current={isActive ? 'page' : undefined}
            onClick={() => onNavigate(id)}
            // Labels at 12px (SH-12). The inactive gray-500 becomes the audit's
            // accessible muted grey with UX-2's palette remap (§3.1, O3).
            className={[
              'u-press flex-1 flex flex-col items-center justify-center gap-0.5 py-2 min-h-11 text-xs transition-colors',
              isActive ? 'text-white' : 'text-gray-500 hover:text-gray-300',
            ].join(' ')}
          >
            <Icon size={20} strokeWidth={isActive ? 2.25 : 1.75} aria-hidden />
            {t(id)}
          </button>
        );
      })}
    </nav>
  );
}

export default React.memo(BottomNav);
