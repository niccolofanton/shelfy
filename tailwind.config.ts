import type { Config } from 'tailwindcss';

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
    },
  },
  plugins: [],
} satisfies Config;
