import type { Config } from 'tailwindcss';
import { COLORS, GRAY, Z } from './src/components/ui/tokens';

// The design tokens (UX audit §3.1) live in src/components/ui/tokens.ts; see
// its header for the class names they produce. Colors are literal hex values
// (not `var(--…)`) so opacity modifiers such as `bg-error/10` keep working;
// tests/components/ui/tokens.test.ts holds them equal to src/index.css.
export default {
  content: ['./index.html', './src/**/*.{js,jsx,ts,tsx}'],
  theme: {
    extend: {
      screens: {
        // The responsive-shell breakpoint (web port plan §2.17, P1-02): a
        // max-width range, so `narrow:` utilities only ADD mobile overrides —
        // the unprefixed (desktop) utilities are untouched and stay
        // pixel-identical at ≥900px, regardless of class order.
        narrow: { max: '899px' },
      },
      colors: {
        gray: GRAY,
        accent: {
          DEFAULT: COLORS.accent,
          hover: COLORS.accentHover,
          fill: COLORS.accentFill,
          text: COLORS.accentText,
        },
        focus: COLORS.focusRing,
        success: COLORS.success,
        error: COLORS.error,
        warning: COLORS.warning,
      },
      textColor: {
        primary: COLORS.textPrimary,
        secondary: COLORS.textSecondary,
        muted: COLORS.textMuted,
        disabled: COLORS.textDisabled,
        // Accent as text, links and icons on dark: the accessible tint.
        accent: { DEFAULT: COLORS.accentText },
      },
      backgroundColor: {
        primary: COLORS.bgPrimary,
        sidebar: COLORS.bgSidebar,
        secondary: COLORS.bgSecondary,
        card: COLORS.bgCard,
        hover: COLORS.bgHover,
        elevated: COLORS.bgElevated,
      },
      borderColor: {
        subtle: COLORS.border,
        strong: COLORS.borderStrong,
      },
      fontSize: {
        '2xs': ['10px', { lineHeight: '14px' }],
        caption: ['11px', { lineHeight: '16px' }],
      },
      zIndex: {
        raised: String(Z.raised),
        toolbar: String(Z.toolbar),
        scrim: String(Z.scrim),
        drawer: String(Z.drawer),
        modal: String(Z.modal),
        'modal-over': String(Z.modalOver),
        popover: String(Z.popover),
        toast: String(Z.toast),
        critical: String(Z.critical),
      },
      boxShadow: {
        pill: '0 6px 20px -6px rgba(0, 0, 0, 0.6)',
      },
    },
  },
  plugins: [],
} satisfies Config;
