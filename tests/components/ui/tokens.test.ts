// The design tokens (UX audit §3.1, UX-2): src/index.css and
// src/components/ui/tokens.ts agree, Tailwind exposes them, and every text
// token clears WCAG AA on the surfaces it is used on.
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, it, expect } from 'vitest';
import resolveConfig from 'tailwindcss/resolveConfig';
import tailwindConfig from '../../../tailwind.config';
import { COLORS, CSS_VARS, GRAY, Z } from '../../../src/components/ui/tokens';

function channel(c: number): number {
  const s = c / 255;
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

function luminance(hex: string): number {
  const h = hex.replace('#', '');
  const [r, g, b] = [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16));
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

// WCAG 2.x contrast ratio.
function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

// The surfaces text sits on: canvas, sidebar/BottomNav, panels and modals,
// pills and sheets, cards.
const SURFACES = {
  canvas: COLORS.bgPrimary,
  sidebar: COLORS.bgSidebar,
  panel: COLORS.bgSecondary,
  elevated: COLORS.bgElevated,
  card: COLORS.bgCard,
};

const AA_TEXT = 4.5;
const AA_UI = 3;

describe('tokens: src/index.css and tokens.ts agree', () => {
  const css = readFileSync(path.join(__dirname, '../../../src/index.css'), 'utf8');
  const root = css.slice(css.indexOf(':root {'), css.indexOf('}', css.indexOf(':root {')));

  it.each(Object.entries(CSS_VARS))('%s is %s in :root', (key, cssVar) => {
    const match = root.match(new RegExp(`${cssVar}:\\s*(#[0-9a-fA-F]{6})\\s*;`));
    expect(match, `${cssVar} missing from :root`).not.toBeNull();
    expect(match![1].toLowerCase()).toBe(COLORS[key as keyof typeof COLORS]);
  });
});

describe('tokens: Tailwind exposes them', () => {
  const theme = resolveConfig(tailwindConfig).theme as unknown as Record<
    string,
    Record<string, unknown>
  >;

  it('maps the text, surface and border tokens to class names', () => {
    expect(theme.textColor.muted).toBe(COLORS.textMuted);
    expect(theme.textColor.primary).toBe(COLORS.textPrimary);
    expect((theme.textColor.accent as Record<string, string>).DEFAULT).toBe(COLORS.accentText);
    expect(theme.backgroundColor.elevated).toBe(COLORS.bgElevated);
    expect((theme.backgroundColor.accent as Record<string, string>).fill).toBe(COLORS.accentFill);
    expect(theme.borderColor.strong).toBe(COLORS.borderStrong);
    expect(theme.ringColor.focus).toBe(COLORS.focusRing);
  });

  it('remaps gray to neutral, accessible values (O3)', () => {
    const gray = theme.colors.gray as Record<string, string>;
    expect(gray['400']).toBe('#a3a3a3');
    expect(gray['500']).toBe(COLORS.textMuted);
    expect(gray['600']).toBe(COLORS.textMuted);
  });

  it('has the layering scale, in order', () => {
    expect(theme.zIndex.modal).toBe(String(Z.modal));
    const order = [
      Z.raised,
      Z.toolbar,
      Z.scrim,
      Z.drawer,
      Z.modal,
      Z.modalOver,
      Z.popover,
      Z.toast,
      Z.critical,
    ];
    expect([...order].sort((a, b) => a - b)).toEqual(order);
  });
});

describe('tokens: contrast (WCAG AA)', () => {
  const text = {
    'text-primary': COLORS.textPrimary,
    'text-secondary': COLORS.textSecondary,
    'text-muted': COLORS.textMuted,
    'text-accent': COLORS.accentText,
    'gray-400': GRAY[400],
    'gray-500': GRAY[500],
    'gray-600': GRAY[600],
  };
  for (const [name, fg] of Object.entries(text)) {
    for (const [surface, bg] of Object.entries(SURFACES)) {
      it(`${name} on ${surface} is at least ${AA_TEXT}:1`, () => {
        expect(contrast(fg, bg)).toBeGreaterThanOrEqual(AA_TEXT);
      });
    }
  }

  it('white on the accent fill and its hover is at least 4.5:1 (O2)', () => {
    expect(contrast('#ffffff', COLORS.accentFill)).toBeGreaterThanOrEqual(AA_TEXT);
    expect(contrast('#ffffff', COLORS.accentHover)).toBeGreaterThanOrEqual(AA_TEXT);
  });

  it('status colors read as text on the canvas and panels', () => {
    for (const fg of [COLORS.error, COLORS.success, COLORS.warning]) {
      for (const bg of [SURFACES.canvas, SURFACES.panel, SURFACES.elevated]) {
        expect(contrast(fg, bg)).toBeGreaterThanOrEqual(AA_TEXT);
      }
    }
  });

  it('the focus ring is at least 3:1 on every surface', () => {
    for (const bg of [...Object.values(SURFACES), COLORS.bgHover]) {
      expect(contrast(COLORS.focusRing, bg)).toBeGreaterThanOrEqual(AA_UI);
    }
  });

  it('the values it replaces failed (audit §3.1)', () => {
    expect(contrast('#606060', SURFACES.canvas)).toBeLessThan(AA_TEXT); // old --text-muted
    expect(contrast('#6b7280', SURFACES.canvas)).toBeLessThan(AA_TEXT); // old gray-500
    expect(contrast('#4b5563', SURFACES.canvas)).toBeLessThan(AA_TEXT); // old gray-600
    expect(contrast('#ffffff', COLORS.accent)).toBeLessThan(AA_TEXT); // white on the brand fill
    expect(contrast(COLORS.accent, SURFACES.panel)).toBeLessThan(AA_TEXT); // brand as text
  });
});
