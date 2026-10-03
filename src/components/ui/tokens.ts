// Design tokens (UX audit §3.1, UX-2): one source for tailwind.config.ts, the
// TypeScript side (z-index for inline styles) and the contrast test
// (tests/components/ui/tokens.test.ts). The same colors are CSS variables in
// src/index.css's :root block; the test fails if the two drift apart.
//
// Tailwind classes (tailwind.config.ts):
//   text-primary | text-secondary | text-muted | text-disabled | text-accent
//   bg-primary | bg-sidebar | bg-secondary | bg-card | bg-hover | bg-elevated
//   bg-accent (brand, indicators) | bg-accent-fill (behind white labels)
//   border-subtle | border-strong | ring-focus | outline-focus
//   text-success | text-error | text-warning (and bg-*/10 tints)
//   text-2xs (10px, uppercase eyebrows only) | text-caption (11px, meta, badges)
//   z-raised … z-critical (Z below) | shadow-pill
//
// `text-accent` is the accent for text, links and icons on dark (#a48fff):
// the brand violet itself is 4.40:1 on the canvas and fails as text.

export const COLORS = {
  // Surfaces
  bgPrimary: '#0f0f0f',
  bgSidebar: '#111111',
  bgSecondary: '#1a1a1a',
  bgCard: '#202020',
  bgHover: '#2a2a2a',
  bgElevated: '#1c1c1e',
  // Text
  textPrimary: '#f0f0f0',
  textSecondary: '#a0a0a0',
  // Was #606060 (3.05:1 on the canvas). ≥ 4.5:1 on every surface up to bgCard.
  textMuted: '#8a8a8a',
  // Disabled controls only: exempt from the contrast minimum.
  textDisabled: '#5c5c5c',
  // Accent
  accent: '#7b5cff',
  accentHover: '#5a3dde',
  // Filled controls with a white label (owner decision O2): 5.05:1.
  accentFill: '#6d4dff',
  accentText: '#a48fff',
  focusRing: '#a48fff',
  // Lines
  border: '#2e2e2e',
  borderStrong: '#3a3a3a',
  // Status
  success: '#4caf50',
  error: '#ef5350',
  warning: '#f5a524',
} as const;

// The CSS variable that carries each color in src/index.css.
export const CSS_VARS: Record<keyof typeof COLORS, string> = {
  bgPrimary: '--bg-primary',
  bgSidebar: '--bg-sidebar',
  bgSecondary: '--bg-secondary',
  bgCard: '--bg-card',
  bgHover: '--bg-hover',
  bgElevated: '--bg-elevated',
  textPrimary: '--text-primary',
  textSecondary: '--text-secondary',
  textMuted: '--text-muted',
  textDisabled: '--text-disabled',
  accent: '--accent',
  accentHover: '--accent-hover',
  accentFill: '--accent-fill',
  accentText: '--accent-text',
  focusRing: '--focus-ring',
  border: '--border',
  borderStrong: '--border-strong',
  success: '--success',
  error: '--error',
  warning: '--warning',
};

// Tailwind's `gray`, remapped (owner decision O3). The app uses gray only for
// text and placeholders, so remapping fixes every `text-gray-*` without
// touching the files: neutral instead of Tailwind's blue-tinted cool grays,
// and the shades used for secondary text made accessible. 600 was #4b5563
// (2.3–2.5:1) and is used for real copy (hints, empty states, eyebrows), so
// it takes the muted value too. 700 and darker stay dim: new code uses
// `text-muted` / `text-disabled`, not gray.
export const GRAY = {
  50: '#fafafa',
  100: '#f5f5f5',
  200: '#e5e5e5',
  300: '#d4d4d4',
  400: '#a3a3a3',
  500: COLORS.textMuted,
  600: COLORS.textMuted,
  700: '#404040',
  800: '#262626',
  900: '#171717',
  950: '#0a0a0a',
} as const;

// The layering scale (§3.1). Portaled layers use these instead of ad hoc
// values; `Z.popover` is Popover's own.
export const Z = {
  raised: 10, // inside isolated cards and carousels
  toolbar: 20, // the floating gallery toolbar
  scrim: 30, // the drawer's and sheets' backdrop
  drawer: 40, // menu drawer, filter sheet
  modal: 50, // PostModal and dialogs
  modalOver: 60, // lightbox, and dialogs opened from a modal
  popover: 70, // menus, popovers and menu sheets
  toast: 80, // toasts: an Undo shows above a modal
  critical: 100, // ReauthDialog, DisclaimerGate
} as const;
