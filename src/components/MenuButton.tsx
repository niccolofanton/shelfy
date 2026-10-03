import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { Menu } from 'lucide-react';
import { useT } from '../i18n';
import { useNavigation } from '../api/navigation';

// The narrow (<900px) menu button and the shell state it shares (UX-1, audit
// SH-1). The menu button used to float over every screen (`fixed`, z-40, in the
// root stacking context), covering the first card, the view headers and even
// the open post modal. It now sits in the layout flow: each screen renders
// <MenuButton /> in its own top row (the gallery's pill row, the Trash, Jobs
// and Settings headers), and ShellProvider (around App) owns the drawer state.
//
// Hand-off: until a view renders its own MenuButton, App shows one in a
// narrow-only top bar above the view (in flow, so it covers nothing). A
// MenuButton mounted inside a view claims that view (App marks each view layer
// with `data-shell-view`), and App drops its bar for it — so a lane that adds
// the button to its header never has to touch App.tsx.

// The drawer's element id, for `aria-controls`.
export const DRAWER_ID = 'app-drawer';

// The narrow breakpoint (tailwind.config.ts `narrow:`), for the shell's JS.
const NARROW_QUERY = '(max-width: 899px)';

export interface ShellApi {
  // Whether the drawer (the sidebar under 900px) is open.
  menuOpen: boolean;
  // Opens the drawer; focus returns to `trigger` when it closes.
  openMenu: (trigger?: HTMLElement | null) => void;
  closeMenu: () => void;
  // The element focus returns to when the drawer closes.
  menuTriggerRef: React.RefObject<HTMLElement | null>;
  // Registers a MenuButton rendered inside `view`; returns the unregister.
  claimMenuButton: (view: string) => () => void;
  // Whether `view` renders a MenuButton of its own.
  hasMenuButton: (view: string) => boolean;
}

const ShellContext = createContext<ShellApi | null>(null);

export function ShellProvider({ children }: { children: React.ReactNode }): React.JSX.Element {
  const [menuOpen, setMenuOpen] = useState<boolean>(false);
  const menuTriggerRef = useRef<HTMLElement | null>(null);
  const openMenu = useCallback((trigger?: HTMLElement | null): void => {
    menuTriggerRef.current = trigger ?? null;
    setMenuOpen(true);
  }, []);
  const closeMenu = useCallback((): void => setMenuOpen(false), []);

  // MenuButtons per view, counted (a view could render two).
  const [claims, setClaims] = useState<ReadonlyMap<string, number>>(() => new Map());
  const claimMenuButton = useCallback((view: string): (() => void) => {
    setClaims((prev) => new Map(prev).set(view, (prev.get(view) ?? 0) + 1));
    return () =>
      setClaims((prev) => {
        const next = new Map(prev);
        const left = (prev.get(view) ?? 1) - 1;
        if (left > 0) next.set(view, left);
        else next.delete(view);
        return next;
      });
  }, []);
  const hasMenuButton = useCallback((view: string): boolean => claims.has(view), [claims]);

  // The drawer exists only under 900px: widening the window closes it, so the
  // shell's `inert` never outlives the narrow layout.
  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return;
    const mq = window.matchMedia(NARROW_QUERY);
    const onChange = (): void => {
      if (!mq.matches) setMenuOpen(false);
    };
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);
  // The address changed under the open drawer (back, forward, a link): close it.
  const route = useNavigation()?.route ?? null;
  useEffect(() => {
    setMenuOpen(false);
  }, [route]);

  const value = useMemo<ShellApi>(
    () => ({ menuOpen, openMenu, closeMenu, menuTriggerRef, claimMenuButton, hasMenuButton }),
    [menuOpen, openMenu, closeMenu, claimMenuButton, hasMenuButton],
  );
  return <ShellContext.Provider value={value}>{children}</ShellContext.Provider>;
}

// The shell, or null outside ShellProvider (a component test).
export function useShell(): ShellApi | null {
  return useContext(ShellContext);
}

interface MenuButtonProps {
  // Extra classes from the host row (spacing, a pill material…).
  className?: string;
}

// 44×44, narrow only (`hidden narrow:flex`): at ≥900px the sidebar is a
// permanent column and there is nothing to open. `pointer-events-auto` because
// the gallery's floating toolbar row is `pointer-events-none` (each pill
// re-enables it). Keeps the `sidebar-open` test id of the trigger it replaces.
export default function MenuButton({ className = '' }: MenuButtonProps): React.JSX.Element | null {
  const t = useT('sidebar');
  const shell = useContext(ShellContext);
  const ref = useRef<HTMLButtonElement>(null);
  const claim = shell?.claimMenuButton;
  // Claims the view layer it renders in. A layout effect: App drops its
  // fallback bar before the first paint, so a view never shows two menu
  // buttons, not even for a frame.
  useLayoutEffect(() => {
    const view = ref.current?.closest<HTMLElement>('[data-shell-view]')?.dataset.shellView;
    return claim && view ? claim(view) : undefined;
  }, [claim]);
  if (!shell) return null;
  return (
    <button
      ref={ref}
      type="button"
      data-testid="sidebar-open"
      onClick={(e) => shell.openMenu(e.currentTarget)}
      aria-label={t('openMenu')}
      aria-haspopup="dialog"
      aria-expanded={shell.menuOpen}
      aria-controls={DRAWER_ID}
      className={[
        'hidden narrow:flex pointer-events-auto items-center justify-center w-11 h-11 shrink-0 rounded-full bg-[#1c1c1e] ring-1 ring-white/10 text-white u-press',
        className,
      ].join(' ')}
    >
      <Menu size={20} aria-hidden />
    </button>
  );
}
