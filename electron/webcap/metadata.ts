// Site-level design metadata (v2), computed from the captured pages:
//   palette    — pixel clusters (OKLab k-means, area weighted) joined with DOM
//                colour roles (canvas/surface backgrounds, text, CTA/accent);
//   typography — faces actually rendering VISIBLE text, with roles, weights,
//                sizes, a type scale and the provider inferred from font-file hosts;
//   tech       — fingerprints over network URLs, DOM/window markers, headers and
//                generator tags, with category and version when known;
//   awards     — award-platform links validated against the site's own domain;
//   identity   — cleaned title, site name cascade, icons, socials, credits, langs;
//   traits     — layout/motion/3D facts for the AI and the UI;
//   digest     — structured per-page text for search and the AI prompt.

import { samplePixels, type ImageAsset } from './encode';
import type { CapturedPage, PageProbe } from './capture';
import type { NetEntry } from './driver';

// ─── Colour math (sRGB ⇄ OKLab) ──────────────────────────────────────────────

type Lab = [number, number, number];

function srgbToLinear(c: number): number {
  const v = c / 255;
  return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
}
function linearToSrgb(v: number): number {
  const c = v <= 0.0031308 ? 12.92 * v : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
  return Math.max(0, Math.min(255, Math.round(c * 255)));
}
export function rgbToOklab(r: number, g: number, b: number): Lab {
  const lr = srgbToLinear(r);
  const lg = srgbToLinear(g);
  const lb = srgbToLinear(b);
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}
export function oklabToHex([L, A, B]: Lab): string {
  const l = Math.pow(L + 0.3963377774 * A + 0.2158037573 * B, 3);
  const m = Math.pow(L - 0.1055613458 * A - 0.0638541728 * B, 3);
  const s = Math.pow(L - 0.0894841775 * A - 1.291485548 * B, 3);
  const r = linearToSrgb(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s);
  const g = linearToSrgb(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s);
  const b = linearToSrgb(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s);
  return '#' + [r, g, b].map((v) => v.toString(16).padStart(2, '0')).join('');
}
export function hexToLab(hex: string): Lab | null {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return null;
  const n = parseInt(m[1], 16);
  return rgbToOklab((n >> 16) & 255, (n >> 8) & 255, n & 255);
}
function dist(a: Lab, b: Lab): number {
  return Math.hypot(a[0] - b[0], a[1] - b[1], a[2] - b[2]);
}
function chroma(a: Lab): number {
  return Math.hypot(a[1], a[2]);
}
function hueDeg(a: Lab): number {
  const h = (Math.atan2(a[2], a[1]) * 180) / Math.PI;
  return h < 0 ? h + 360 : h;
}
function relLuminance(hex: string): number {
  const n = parseInt(hex.slice(1), 16);
  const [r, g, b] = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map(srgbToLinear);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}
export function contrastRatio(a: string, b: string): number {
  const la = relLuminance(a);
  const lb = relLuminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

// Basic colour naming for search and the AI ("deep navy", "warm off-white").
export function colorName(hex: string): string {
  const lab = hexToLab(hex);
  if (!lab) return hex;
  const L = lab[0];
  const C = chroma(lab);
  if (C < 0.025) {
    if (L > 0.97) return 'white';
    if (L > 0.9) return 'off-white';
    if (L > 0.75) return 'light grey';
    if (L > 0.55) return 'grey';
    if (L > 0.3) return 'dark grey';
    if (L > 0.17) return 'charcoal';
    return 'black';
  }
  const h = hueDeg(lab);
  const hue =
    h < 15 || h >= 350
      ? 'pink-red'
      : h < 40
        ? 'red'
        : h < 70
          ? 'orange'
          : h < 100
            ? 'yellow'
            : h < 135
              ? 'lime'
              : h < 165
                ? 'green'
                : h < 200
                  ? 'teal'
                  : h < 235
                    ? 'cyan'
                    : h < 270
                      ? 'blue'
                      : h < 300
                        ? 'indigo'
                        : h < 330
                          ? 'purple'
                          : 'magenta';
  const base =
    hue === 'pink-red'
      ? L > 0.7
        ? 'pink'
        : 'crimson'
      : hue === 'orange' && L < 0.55
        ? 'brown'
        : hue === 'yellow' && L < 0.6
          ? 'olive'
          : hue;
  const tone =
    C < 0.06
      ? L > 0.8
        ? 'pale '
        : L < 0.4
          ? 'muted dark '
          : 'muted '
      : L > 0.85
        ? 'light '
        : L < 0.35
          ? 'deep '
          : C > 0.17
            ? 'vivid '
            : '';
  return `${tone}${base}`.trim();
}

// ─── Palette ─────────────────────────────────────────────────────────────────

export interface PaletteSwatch {
  hex: string;
  name: string;
  role: 'background' | 'surface' | 'text' | 'accent' | 'image';
  coverage: number; // share of sampled pixels 0..1
  source: 'pixels' | 'dom' | 'both';
  oklch: [number, number, number];
  // Legacy fields read by v1 consumers (PostCard, WebMetaPanel).
  weight: number;
}

export interface PaletteResult {
  swatches: PaletteSwatch[];
  scheme: 'light' | 'dark' | 'mixed';
  contrast: { text: string; background: string; ratio: number } | null;
}

function kmeans(points: Lab[], weights: number[], k: number, iters = 14): { c: Lab; w: number }[] {
  if (!points.length) return [];
  // k-means++ style seeding on luminance/chroma spread.
  const centers: Lab[] = [points[0]];
  while (centers.length < k) {
    let best = -1;
    let bestD = -1;
    for (let i = 0; i < points.length; i += 3) {
      const d = Math.min(...centers.map((c) => dist(points[i], c))) * weights[i];
      if (d > bestD) {
        bestD = d;
        best = i;
      }
    }
    if (best < 0 || bestD <= 1e-6) break;
    centers.push(points[best]);
  }
  const assign = new Int32Array(points.length);
  for (let it = 0; it < iters; it++) {
    for (let i = 0; i < points.length; i++) {
      let bi = 0;
      let bd = Infinity;
      for (let j = 0; j < centers.length; j++) {
        const d = dist(points[i], centers[j]);
        if (d < bd) {
          bd = d;
          bi = j;
        }
      }
      assign[i] = bi;
    }
    const sum = centers.map(() => [0, 0, 0, 0]);
    for (let i = 0; i < points.length; i++) {
      const s = sum[assign[i]];
      const w = weights[i];
      s[0] += points[i][0] * w;
      s[1] += points[i][1] * w;
      s[2] += points[i][2] * w;
      s[3] += w;
    }
    for (let j = 0; j < centers.length; j++)
      if (sum[j][3] > 0)
        centers[j] = [sum[j][0] / sum[j][3], sum[j][1] / sum[j][3], sum[j][2] / sum[j][3]];
  }
  const totals = centers.map(() => 0);
  for (let i = 0; i < points.length; i++) totals[assign[i]] += weights[i];
  const all = weights.reduce((a, b) => a + b, 0) || 1;
  return centers.map((c, j) => ({ c, w: totals[j] / all }));
}

export async function computePalette(
  pages: CapturedPage[],
  signal?: AbortSignal,
): Promise<PaletteResult> {
  // Pixels: hero of every page (×2 weight) + the full-page bands of the primary.
  const sources: { asset: ImageAsset; weight: number }[] = [];
  for (const p of pages)
    if (p.hero) sources.push({ asset: p.hero, weight: p === pages[0] ? 3 : 1.5 });
  for (const b of pages[0]?.bands || []) sources.push({ asset: b, weight: 1 });
  const points: Lab[] = [];
  const weights: number[] = [];
  for (const src of sources.slice(0, 18)) {
    const px = await samplePixels(src.asset.path, 72, signal);
    if (!px) continue;
    const { data } = px;
    for (let i = 0; i < data.length; i += 3) {
      points.push(rgbToOklab(data[i], data[i + 1], data[i + 2]));
      weights.push(src.weight);
    }
  }
  const clusters = kmeans(points, weights, 12).filter((c) => c.w > 0);
  // Merge perceptually close clusters.
  const merged: { c: Lab; w: number }[] = [];
  for (const cl of clusters.sort((a, b) => b.w - a.w)) {
    const near = merged.find((m) => dist(m.c, cl.c) < 0.045);
    if (near) {
      const w = near.w + cl.w;
      near.c = [
        (near.c[0] * near.w + cl.c[0] * cl.w) / w,
        (near.c[1] * near.w + cl.c[1] * cl.w) / w,
        (near.c[2] * near.w + cl.c[2] * cl.w) / w,
      ];
      near.w = w;
    } else merged.push({ ...cl });
  }

  // DOM roles from the probes (all pages, primary first).
  const domBg = new Map<string, number>();
  const domText = new Map<string, number>();
  const domCta = new Map<string, number>();
  for (const p of pages) {
    const pr = p.probe as PageProbe & {
      backgrounds?: { hex: string; coverage: number }[];
      textColors?: { hex: string; weight: number }[];
      ctas?: { bg: string }[];
      canvasBg?: string;
    };
    const k = p === pages[0] ? 2 : 1;
    for (const b of pr.backgrounds || [])
      domBg.set(b.hex, (domBg.get(b.hex) || 0) + b.coverage * k);
    if (pr.canvasBg) domBg.set(pr.canvasBg, (domBg.get(pr.canvasBg) || 0) + 0.3 * k);
    const tTot = (pr.textColors || []).reduce((s, t) => s + t.weight, 0) || 1;
    for (const t of pr.textColors || [])
      domText.set(t.hex, (domText.get(t.hex) || 0) + (t.weight / tTot) * k);
    for (const c of pr.ctas || []) if (c.bg) domCta.set(c.bg, (domCta.get(c.bg) || 0) + k);
  }
  const nearest = (lab: Lab, m: Map<string, number>, tol: number): number => {
    let score = 0;
    for (const [hex, w] of m) {
      const l2 = hexToLab(hex);
      if (l2 && dist(l2, lab) < tol) score += w;
    }
    return score;
  };

  const swatches: PaletteSwatch[] = [];
  for (const m of merged) {
    if (m.w < 0.004 && chroma(m.c) < 0.08) continue;
    const hex = oklabToHex(m.c);
    const bgScore = nearest(m.c, domBg, 0.05);
    const textScore = nearest(m.c, domText, 0.05);
    const ctaScore = nearest(m.c, domCta, 0.06);
    let role: PaletteSwatch['role'];
    if (ctaScore > 0 && chroma(m.c) > 0.05) role = 'accent';
    else if (bgScore > 0.05 || m.w > 0.25)
      role = swatches.some((s) => s.role === 'background') ? 'surface' : 'background';
    else if (textScore > 0.15) role = 'text';
    else if (chroma(m.c) > 0.1 && m.w < 0.12) role = bgScore > 0 ? 'surface' : 'accent';
    else role = bgScore > 0 ? 'surface' : 'image';
    const C = chroma(m.c);
    swatches.push({
      hex,
      name: colorName(hex),
      role,
      coverage: Math.round(m.w * 1000) / 1000,
      source: bgScore || textScore || ctaScore ? 'both' : 'pixels',
      oklch: [
        Math.round(m.c[0] * 1000) / 1000,
        Math.round(C * 1000) / 1000,
        Math.round(hueDeg(m.c)),
      ],
      weight: Math.round(m.w * 100) / 100,
    });
  }
  // Text colours rarely cover many pixels: add the dominant DOM text colour.
  const topText = Array.from(domText.entries()).sort((a, b) => b[1] - a[1])[0];
  if (topText) {
    const lab = hexToLab(topText[0]);
    if (lab && !swatches.some((s) => dist(hexToLab(s.hex)!, lab) < 0.04)) {
      swatches.push({
        hex: topText[0],
        name: colorName(topText[0]),
        role: 'text',
        coverage: 0,
        source: 'dom',
        oklch: [
          Math.round(lab[0] * 1000) / 1000,
          Math.round(chroma(lab) * 1000) / 1000,
          Math.round(hueDeg(lab)),
        ],
        weight: 0,
      });
    } else if (lab) {
      const s = swatches.find((x) => dist(hexToLab(x.hex)!, lab) < 0.04);
      if (s && s.role !== 'background') s.role = 'text';
    }
  }
  // Same for the main CTA colour (the brand accent), if pixels missed it.
  const topCta = Array.from(domCta.entries()).sort((a, b) => b[1] - a[1])[0];
  if (topCta) {
    const lab = hexToLab(topCta[0]);
    if (lab && chroma(lab) > 0.04 && !swatches.some((s) => dist(hexToLab(s.hex)!, lab) < 0.05)) {
      swatches.push({
        hex: topCta[0],
        name: colorName(topCta[0]),
        role: 'accent',
        coverage: 0,
        source: 'dom',
        oklch: [
          Math.round(lab[0] * 1000) / 1000,
          Math.round(chroma(lab) * 1000) / 1000,
          Math.round(hueDeg(lab)),
        ],
        weight: 0,
      });
    }
  }
  const order: Record<PaletteSwatch['role'], number> = {
    background: 0,
    surface: 1,
    text: 2,
    accent: 3,
    image: 4,
  };
  swatches.sort((a, b) => order[a.role] - order[b.role] || b.coverage - a.coverage);
  const top = swatches.slice(0, 10);

  const bg = top.find((s) => s.role === 'background');
  const darkShare = merged.filter((m) => m.c[0] < 0.45).reduce((s, m) => s + m.w, 0);
  const scheme: PaletteResult['scheme'] =
    darkShare > 0.65 ? 'dark' : darkShare < 0.3 ? 'light' : 'mixed';
  const text = top.find((s) => s.role === 'text');
  return {
    swatches: top,
    scheme,
    contrast:
      bg && text
        ? {
            text: text.hex,
            background: bg.hex,
            ratio: Math.round(contrastRatio(text.hex, bg.hex) * 10) / 10,
          }
        : null,
  };
}

// ─── Typography ──────────────────────────────────────────────────────────────

export interface FontInfo {
  family: string;
  role: 'display' | 'heading' | 'body' | 'mono' | 'ui';
  roles: string[];
  usage: string; // legacy single role (v1 consumers)
  weights: number[];
  sizes: number[];
  share: number; // share of visible characters 0..1
  provider: string; // google | adobe | fontshare | monotype | hoefler | webflow | framer | shopify | self-hosted | system | unknown
  classification: 'serif' | 'sans' | 'mono' | 'display' | 'script' | 'unknown';
  sample: string;
  italic: boolean;
}

export interface TypographyResult {
  fonts: FontInfo[];
  scale: { size: number; share: number }[];
  baseSize: number | null;
  ratio: number | null;
}

const GENERIC =
  /^(serif|sans-serif|monospace|cursive|fantasy|system-ui|ui-sans-serif|ui-serif|ui-monospace|ui-rounded|emoji|math|fangsong|-apple-system|blinkmacsystemfont|inherit|initial)$/i;
const ICON_FONT =
  /icon|awesome|material symbols|material icons|glyph|dashicons|feather|ionicons|remixicon|bootstrap-icons|lucide|swiper-icons|slick|fontello|icomoon|eicons|apple legacy|sf pro icons|apple icons|emoji|segoe ui (emoji|symbol)|noto color emoji|apple color emoji|webflow-icons/i;
const SYSTEM_FONTS =
  /^(arial|helvetica|helvetica neue|times|times new roman|georgia|verdana|tahoma|trebuchet ms|courier|courier new|segoe ui|roboto|sf pro( text| display)?|san francisco|system-ui|menlo|monaco|consolas|ubuntu|cantarell|oxygen|open sans|noto sans|lucida grande|avenir( next)?|gill sans|optima|palatino|futura|baskerville)$/i;
const MONO = /mono|code|courier|consolas|menlo|monaco|plex mono|jetbrains|fira code|source code/i;
const SERIF_HINT =
  /serif|garamond|caslon|baskerville|bodoni|didot|playfair|lora|merriweather|tiempos|georgia|times|freight|canela|editorial|ogg|recoleta|fraunces|domaine|gt super|reckless|newsreader|instrument serif|dm serif|libre caslon|cormorant|spectral|crimson|eb garamond|source serif|noto serif|pt serif|roslindale|ivar|signifier|lyon|austin|portrait|chronicle|mercury|tobias|sectra|gt alpina/i;
const SCRIPT_HINT = /script|hand|brush|signature|pacifico|caveat|dancing/i;

// next/font hashes (__Inter_a1b2c3, __Inter_Fallback_a1b2c3), Framer
// placeholders and metric fallbacks are build artefacts, not typefaces.
export function cleanFamily(raw: string): string | null {
  let f = String(raw || '')
    .trim()
    .replace(/^["']|["']$/g, '')
    .trim();
  if (!f || GENERIC.test(f)) return null;
  const nx = /^__(.+?)_(?:Fallback_)?[0-9a-f]{5,8}$/i.exec(f);
  if (nx) f = nx[1].replace(/_/g, ' ');
  if (/fallback|placeholder/i.test(f)) return null;
  f = f.replace(/\s+(Variable|VF|Var)$/i, '').replace(/[-_](Variable|VF|Var)$/i, '');
  f = f.replace(/-(Regular|Medium|Bold|Light|Book|Semibold|SemiBold|Black|Thin|Italic)$/i, '');
  f = f
    .replace(/([a-z])-([A-Z])/g, '$1 $2')
    .replace(/([a-z])(Sans|Serif|Mono|Grotesk|Grotesque|Display|Text|Plex)\b/g, '$1 $2')
    .replace(/([A-Z]{2,})(Plex|Sans|Serif|Mono)\b/g, '$1 $2');
  if (ICON_FONT.test(f)) return null;
  // CamelCase file-style names → words ("SourceCodePro" → "Source Code Pro").
  if (!/\s/.test(f))
    f = f.replace(/([a-z])([A-Z])/g, '$1 $2').replace(/([A-Z]{2,})([A-Z][a-z])/g, '$1 $2');
  // All-lowercase CSS names ("sohne") → capitalised words.
  if (f === f.toLowerCase())
    f = f.replace(/(^|[\s-])([a-z])/g, (_m, a: string, b: string) => a + b.toUpperCase());
  return f.trim() || null;
}

const PROVIDER_HOSTS: [RegExp, string][] = [
  [/fonts\.gstatic\.com|fonts\.googleapis\.com/, 'google'],
  [/use\.typekit\.net|p\.typekit\.net/, 'adobe'],
  [/fontshare\.com/, 'fontshare'],
  [/fast\.fonts\.net|fonts\.net/, 'monotype'],
  [/cloud\.typography\.com|typography\.com/, 'hoefler'],
  [/website-files\.com|webflow\.com/, 'webflow'],
  [/framerusercontent\.com|framer\.com/, 'framer'],
  [/cdn\.shopify\.com|shopifycdn/, 'shopify'],
  [/use\.fontawesome\.com/, 'fontawesome'],
  [/fonts\.bunny\.net/, 'bunny'],
  [/cdn\.jsdelivr\.net\/(npm|gh)\/@?fontsource/, 'fontsource'],
];

function providerFor(
  family: string,
  fontFiles: NetEntry[],
  siteHost: string,
  faceFamilies: Set<string>,
): string {
  const key = family.toLowerCase().replace(/[^a-z0-9]/g, '');
  // A font file whose URL mentions the family is the strongest signal.
  for (const f of fontFiles) {
    const u = f.url.toLowerCase().replace(/[^a-z0-9./]/g, '');
    if (key.length >= 3 && u.includes(key)) {
      for (const [re, name] of PROVIDER_HOSTS) if (re.test(f.url)) return name;
      try {
        const h = new URL(f.url).hostname.replace(/^www\./, '');
        if (h === siteHost || h.endsWith(`.${siteHost}`)) return 'self-hosted';
      } catch {}
      return 'self-hosted';
    }
  }
  if (!faceFamilies.has(family.toLowerCase()))
    return SYSTEM_FONTS.test(family) ? 'system' : 'unknown';
  // Declared via @font-face but no file name match: single third-party font host?
  const hosts = new Set<string>();
  for (const f of fontFiles)
    for (const [re, name] of PROVIDER_HOSTS) if (re.test(f.url)) hosts.add(name);
  if (hosts.size === 1 && fontFiles.every((f) => PROVIDER_HOSTS.some(([re]) => re.test(f.url))))
    return Array.from(hosts)[0];
  return fontFiles.length ? 'self-hosted' : 'unknown';
}

export function computeTypography(pages: CapturedPage[], siteHost: string): TypographyResult {
  interface Acc {
    family: string;
    chars: number;
    weights: Map<number, number>;
    sizes: Map<number, number>;
    tags: Map<string, number>;
    maxSize: number;
    firstViewportMax: number;
    sample: string;
    italic: number;
  }
  const acc = new Map<string, Acc>();
  const sizeShare = new Map<number, number>();
  let total = 0;
  const faceFamilies = new Set<string>();
  const fontFiles: NetEntry[] = [];
  for (const p of pages) {
    const pr = p.probe as PageProbe & {
      typeStyles?: {
        family: string;
        weight: string;
        style: string;
        size: number;
        chars: number;
        minTop: number | null;
        tags: Record<string, number>;
        sample: string;
      }[];
      fontFaces?: { family: string; status: string }[];
    };
    for (const f of pr.fontFaces || [])
      if (f.status === 'loaded')
        faceFamilies.add(
          String(f.family)
            .replace(/^["']|["']$/g, '')
            .toLowerCase(),
        );
    fontFiles.push(
      ...p.network.filter((n) => n.type === 'font' || /\.(woff2?|ttf|otf)(\?|$)/i.test(n.url)),
    );
    const k = p === pages[0] ? 2 : 1;
    for (const st of pr.typeStyles || []) {
      // The rendered face = first family of the stack that is a loaded web font,
      // else the first non-generic family (assumed installed).
      const stack = String(st.family || '')
        .split(',')
        .map((s) => s.trim().replace(/^["']|["']$/g, ''));
      let chosen: string | null = null;
      for (const f of stack) {
        if (faceFamilies.has(f.toLowerCase())) {
          chosen = f;
          break;
        }
      }
      if (!chosen) chosen = stack.find((f) => !GENERIC.test(f)) || null;
      const family = chosen ? cleanFamily(chosen) : null;
      if (!family) continue;
      const chars = st.chars * k;
      total += chars;
      const size = Math.round(st.size);
      sizeShare.set(size, (sizeShare.get(size) || 0) + chars);
      let a = acc.get(family.toLowerCase());
      if (!a) {
        a = {
          family,
          chars: 0,
          weights: new Map(),
          sizes: new Map(),
          tags: new Map(),
          maxSize: 0,
          firstViewportMax: 0,
          sample: '',
          italic: 0,
        };
        acc.set(family.toLowerCase(), a);
      }
      a.chars += chars;
      const w = Number(st.weight) || 400;
      a.weights.set(w, (a.weights.get(w) || 0) + chars);
      a.sizes.set(size, (a.sizes.get(size) || 0) + chars);
      for (const [t, n] of Object.entries(st.tags || {}))
        a.tags.set(t, (a.tags.get(t) || 0) + n * k);
      a.maxSize = Math.max(a.maxSize, st.size);
      if (st.minTop !== null && st.minTop < 900)
        a.firstViewportMax = Math.max(a.firstViewportMax, st.size);
      if (st.style === 'italic') a.italic += chars;
      if (!a.sample || (st.size >= 28 && st.sample.length > 6)) a.sample = st.sample;
    }
  }
  const list = Array.from(acc.values())
    .filter((a) => a.chars >= Math.max(8, total * 0.004))
    .sort((a, b) => b.chars - a.chars);
  const bodyFam = list.slice().sort((a, b) => (b.tags.get('p') || 0) - (a.tags.get('p') || 0))[0];
  const displayFam = list
    .slice()
    .sort((a, b) => b.firstViewportMax - a.firstViewportMax || b.maxSize - a.maxSize)[0];
  const fonts: FontInfo[] = list.slice(0, 6).map((a) => {
    const roles: string[] = [];
    const tagged = (t: string): number => a.tags.get(t) || 0;
    if (MONO.test(a.family) || tagged('code') > a.chars * 0.3) roles.push('mono');
    if (a === displayFam && a.maxSize >= 32) roles.push('display');
    if (tagged('h1') + tagged('h2') + tagged('h3') > a.chars * 0.15 || a.maxSize >= 28)
      roles.push('heading');
    if (a === bodyFam || tagged('p') > a.chars * 0.3) roles.push('body');
    if (tagged('button') + tagged('nav') > a.chars * 0.3) roles.push('ui');
    if (!roles.length) roles.push(a.maxSize >= 24 ? 'heading' : 'body');
    const role =
      (['display', 'heading', 'body', 'mono', 'ui'] as const).find((r) => roles.includes(r)) ||
      'body';
    const classification: FontInfo['classification'] = MONO.test(a.family)
      ? 'mono'
      : SCRIPT_HINT.test(a.family)
        ? 'script'
        : SERIF_HINT.test(a.family)
          ? 'serif'
          : /display|poster|condensed|compressed|black|fat/i.test(a.family)
            ? 'display'
            : 'sans';
    return {
      family: a.family,
      role,
      roles,
      usage: role === 'display' ? 'heading' : role === 'ui' ? 'other' : role,
      weights: Array.from(a.weights.entries())
        .sort((x, y) => y[1] - x[1])
        .map(([w]) => w)
        .slice(0, 5)
        .sort((x, y) => x - y),
      sizes: Array.from(a.sizes.entries())
        .sort((x, y) => y[1] - x[1])
        .map(([s]) => s)
        .slice(0, 6)
        .sort((x, y) => x - y),
      share: Math.round((a.chars / (total || 1)) * 1000) / 1000,
      provider: providerFor(a.family, fontFiles, siteHost, faceFamilies),
      classification,
      sample: a.sample.slice(0, 60),
      italic: a.italic > a.chars * 0.4,
    };
  });
  const scale = Array.from(sizeShare.entries())
    .map(([size, n]) => ({ size, share: Math.round((n / (total || 1)) * 1000) / 1000 }))
    .filter((s) => s.share >= 0.005)
    .sort((a, b) => a.size - b.size);
  // Base = the most used size among body-copy sizes (a display-heavy page can
  // have more characters at 40px than at 16px).
  const base =
    scale.filter((s) => s.size >= 12 && s.size <= 22).sort((a, b) => b.share - a.share)[0]?.size ||
    scale.slice().sort((a, b) => b.share - a.share)[0]?.size ||
    null;
  const big = scale.filter((s) => base && s.size > base * 1.15);
  let ratio: number | null = null;
  if (base && big.length >= 2) {
    const r = big.map((s, i) => Math.pow(s.size / base, 1 / (i + 1)));
    ratio = Math.round((r.reduce((x, y) => x + y, 0) / r.length) * 100) / 100;
  }
  return { fonts, scale, baseSize: base, ratio };
}

// ─── Tech stack ──────────────────────────────────────────────────────────────

export interface TechInfo {
  name: string;
  category: string;
  version?: string;
  confidence: number;
}

type Fp = {
  name: string;
  category: string;
  url?: RegExp;
  marker?: string;
  header?: [string, RegExp];
  generator?: RegExp;
  html?: RegExp;
};

const FINGERPRINTS: Fp[] = [
  // Frameworks
  {
    name: 'Next.js',
    category: 'framework',
    marker: 'next',
    url: /\/_next\/static\//,
    header: ['x-powered-by', /next\.js/i],
  },
  { name: 'Nuxt', category: 'framework', marker: 'nuxt', url: /\/_nuxt\// },
  { name: 'Gatsby', category: 'framework', marker: 'gatsby', url: /\/page-data\/|gatsby-/ },
  { name: 'Astro', category: 'framework', marker: 'astro', url: /\/_astro\// },
  { name: 'Remix', category: 'framework', marker: 'remix' },
  { name: 'SvelteKit', category: 'framework', marker: 'sveltekit', url: /\/_app\/immutable\// },
  { name: 'React', category: 'ui-library', marker: 'react' },
  { name: 'Vue', category: 'ui-library', marker: 'vue' },
  { name: 'Angular', category: 'ui-library', marker: 'angular' },
  { name: 'jQuery', category: 'ui-library', marker: 'jquery', url: /jquery(\.min)?\.js/ },
  { name: 'Tailwind CSS', category: 'css', marker: 'tailwind' },
  // Builders / CMS / commerce
  {
    name: 'Webflow',
    category: 'site-builder',
    marker: 'webflow',
    url: /website-files\.com|webflow\.com\/js/,
    generator: /webflow/i,
  },
  {
    name: 'Framer',
    category: 'site-builder',
    marker: 'framer',
    url: /framerusercontent\.com|framer\.com\/m\//,
    generator: /framer/i,
  },
  {
    name: 'Wix',
    category: 'site-builder',
    marker: 'wix',
    url: /static\.wixstatic\.com|parastorage\.com/,
    generator: /wix/i,
  },
  {
    name: 'Squarespace',
    category: 'site-builder',
    marker: 'squarespace',
    url: /squarespace(-cdn)?\.com|static1\.squarespace/,
    generator: /squarespace/i,
  },
  {
    name: 'Shopify',
    category: 'e-commerce',
    marker: 'shopify',
    url: /cdn\.shopify\.com/,
    header: ['x-shopify-stage', /./],
  },
  {
    name: 'WordPress',
    category: 'cms',
    marker: 'wordpress',
    url: /\/wp-content\/|\/wp-includes\//,
    generator: /wordpress/i,
  },
  { name: 'Elementor', category: 'site-builder', marker: 'elementor', generator: /elementor/i },
  { name: 'WooCommerce', category: 'e-commerce', url: /woocommerce/, generator: /woocommerce/i },
  { name: 'Ghost', category: 'cms', generator: /ghost/i },
  { name: 'Drupal', category: 'cms', generator: /drupal/i, header: ['x-generator', /drupal/i] },
  { name: 'Joomla', category: 'cms', generator: /joomla/i },
  {
    name: 'HubSpot CMS',
    category: 'cms',
    url: /hs-sites\.com|hubspotusercontent/,
    generator: /hubspot/i,
  },
  { name: 'Contentful', category: 'headless-cms', url: /ctfassets\.net/ },
  { name: 'Sanity', category: 'headless-cms', url: /cdn\.sanity\.io/ },
  { name: 'Prismic', category: 'headless-cms', url: /prismic\.io|images\.prismic/ },
  { name: 'Storyblok', category: 'headless-cms', url: /storyblok\.com/ },
  { name: 'DatoCMS', category: 'headless-cms', url: /datocms-assets\.com/ },
  { name: 'Strapi', category: 'headless-cms', url: /strapi/ },
  { name: 'Hygraph', category: 'headless-cms', url: /graphassets\.com|hygraph/ },
  { name: 'Payload', category: 'headless-cms', url: /\/api\/media\/file\// },
  { name: 'Magento', category: 'e-commerce', url: /\/static\/version\d+\/frontend\// },
  { name: 'BigCommerce', category: 'e-commerce', url: /bigcommerce\.com/ },
  { name: 'Hydrogen', category: 'e-commerce', header: ['powered-by', /hydrogen/i] },
  // Motion / scroll
  {
    name: 'GSAP',
    category: 'animation',
    marker: 'gsap',
    url: /gsap(\.min)?\.js|\/gsap@|greensock/,
  },
  {
    name: 'ScrollTrigger',
    category: 'animation',
    marker: 'scrolltrigger',
    url: /ScrollTrigger(\.min)?\.js/i,
  },
  { name: 'ScrollSmoother', category: 'scroll', marker: 'scrollsmoother' },
  {
    name: 'Lenis',
    category: 'scroll',
    marker: 'lenis',
    url: /lenis(\.min)?\.js|@studio-freight\/lenis|\/lenis@/,
  },
  { name: 'Locomotive Scroll', category: 'scroll', marker: 'locomotive', url: /locomotive-scroll/ },
  {
    name: 'Barba.js',
    category: 'page-transitions',
    marker: 'barba',
    url: /barba(\.umd)?(\.min)?\.js|@barba\/core/,
  },
  { name: 'Swup', category: 'page-transitions', marker: 'swup', url: /swup/ },
  { name: 'Highway', category: 'page-transitions', marker: 'highway' },
  {
    name: 'Lottie',
    category: 'animation',
    marker: 'lottie',
    url: /lottie(\.min)?\.js|lottie-player|dotlottie|\.lottie(\?|$)/,
  },
  { name: 'Rive', category: 'animation', marker: 'rive', url: /@rive-app|\.riv(\?|$)/ },
  { name: 'AOS', category: 'animation', marker: 'aos', url: /aos(\.min)?\.js|\/aos@/ },
  { name: 'SplitType', category: 'animation', marker: 'splittext', url: /split-type|SplitText/ },
  { name: 'Framer Motion', category: 'animation', url: /framer-motion/ },
  { name: 'Motion One', category: 'animation', url: /@motionone|\/motion@/ },
  { name: 'Anime.js', category: 'animation', url: /anime(\.min)?\.js|animejs/ },
  // 3D / WebGL
  {
    name: 'Three.js',
    category: '3d',
    marker: 'three',
    url: /three(\.module)?(\.min)?\.js|\/three@|three\.core/,
  },
  { name: 'React Three Fiber', category: '3d', url: /@react-three\/fiber/ },
  { name: 'OGL', category: '3d', marker: 'ogl', url: /\/ogl@|ogl(\.min)?\.js/ },
  { name: 'PixiJS', category: '3d', marker: 'pixi', url: /pixi(\.min)?\.js|\/pixi\.js@/ },
  { name: 'Babylon.js', category: '3d', marker: 'babylon', url: /babylon(\.max)?(\.min)?\.js/ },
  { name: 'Spline', category: '3d', marker: 'spline', url: /prod\.spline\.design|@splinetool/ },
  {
    name: 'Unicorn Studio',
    category: '3d',
    marker: 'unicornstudio',
    url: /unicornstudio|unicorn\.studio/,
  },
  { name: 'PlayCanvas', category: '3d', url: /playcanvas/ },
  // Sliders / media
  {
    name: 'Swiper',
    category: 'ui-component',
    marker: 'swiper',
    url: /swiper(-bundle)?(\.min)?\.js|\/swiper@/,
  },
  { name: 'Splide', category: 'ui-component', marker: 'splide', url: /splide(\.min)?\.js/ },
  { name: 'Vimeo', category: 'video', url: /player\.vimeo\.com|vimeocdn\.com/ },
  { name: 'YouTube', category: 'video', url: /youtube(-nocookie)?\.com\/(embed|iframe_api)/ },
  { name: 'Mux', category: 'video', url: /stream\.mux\.com|image\.mux\.com/ },
  { name: 'Cloudinary', category: 'media-cdn', url: /res\.cloudinary\.com/ },
  { name: 'imgix', category: 'media-cdn', url: /\.imgix\.net/ },
  // Hosting / CDN
  { name: 'Vercel', category: 'hosting', header: ['server', /vercel/i] },
  { name: 'Netlify', category: 'hosting', header: ['server', /netlify/i] },
  { name: 'Cloudflare', category: 'cdn', header: ['server', /cloudflare/i] },
  { name: 'Fastly', category: 'cdn', header: ['x-served-by', /cache-/i] },
  { name: 'Amazon CloudFront', category: 'cdn', header: ['via', /cloudfront/i] },
  { name: 'GitHub Pages', category: 'hosting', header: ['server', /github\.com/i] },
  // Analytics / marketing
  {
    name: 'Google Tag Manager',
    category: 'analytics',
    marker: 'gtm',
    url: /googletagmanager\.com/,
  },
  { name: 'Plausible', category: 'analytics', marker: 'plausible', url: /plausible\.io/ },
  { name: 'PostHog', category: 'analytics', marker: 'posthog', url: /posthog/ },
  { name: 'Fathom', category: 'analytics', url: /usefathom\.com/ },
  { name: 'Segment', category: 'analytics', url: /cdn\.segment\.com/ },
  { name: 'HubSpot', category: 'marketing', url: /js\.hs-scripts\.com|hs-analytics/ },
  { name: 'Intercom', category: 'support', marker: 'intercom', url: /intercom/ },
  { name: 'Stripe', category: 'payments', url: /js\.stripe\.com/ },
];

export function computeTech(pages: CapturedPage[]): TechInfo[] {
  const found = new Map<string, TechInfo>();
  const add = (fp: Fp, confidence: number, version?: string): void => {
    const prev = found.get(fp.name);
    if (!prev || prev.confidence < confidence)
      found.set(fp.name, {
        name: fp.name,
        category: fp.category,
        confidence,
        ...(version || prev?.version ? { version: version || prev?.version } : {}),
      });
  };
  for (const p of pages) {
    const pr = p.probe as PageProbe & {
      markers?: Record<string, unknown>;
      head?: { generator?: string[] };
    };
    const markers = pr.markers || {};
    const gens = pr.head?.generator || [];
    const urls = p.network.map((n) => n.url);
    for (const fp of FINGERPRINTS) {
      if (fp.marker && markers[fp.marker] !== undefined) {
        const v = markers[fp.marker];
        add(fp, 0.95, typeof v === 'string' && /^\d/.test(v) ? v : undefined);
      }
      if (fp.url && urls.some((u) => fp.url!.test(u))) add(fp, 0.85);
      if (fp.header) {
        const h = p.headers[fp.header[0]];
        if (h && fp.header[1].test(h)) add(fp, 0.8);
      }
      if (fp.generator && gens.some((g) => fp.generator!.test(g))) {
        const v = gens.map((g) => /(\d+(?:\.\d+){0,2})/.exec(g)?.[1]).find(Boolean);
        add(fp, 0.9, v);
      }
    }
  }
  const ctx =
    (pages[0]?.probe as { canvasContexts?: Record<string, number> })?.canvasContexts || {};
  if ((ctx.webgl || ctx.webgl2) && !Array.from(found.values()).some((t) => t.category === '3d')) {
    found.set('WebGL', { name: 'WebGL', category: '3d', confidence: 0.9 });
  }
  return Array.from(found.values()).sort(
    (a, b) => b.confidence - a.confidence || a.name.localeCompare(b.name),
  );
}

// ─── Awards ──────────────────────────────────────────────────────────────────

export interface AwardInfo {
  platform: string;
  level?: string;
  date?: string;
  profileUrl?: string;
  evidence: 'badge' | 'ribbon' | 'self-link';
  confidence: number;
}

const AWARD_PLATFORMS: [RegExp, string][] = [
  [/awwwards\.com/i, 'awwwards'],
  [/cssdesignawards\.com|cssda\./i, 'cssda'],
  [/thefwa\.com/i, 'fwa'],
  [/godly\.website/i, 'godly'],
  [/land-book\.com/i, 'landbook'],
  [/siteinspire\.com/i, 'siteinspire'],
  [/onepagelove\.com/i, 'onepagelove'],
  [/webbyawards\.com/i, 'webby'],
  [/csswinner\.com/i, 'csswinner'],
];

function slugTokens(s: string): string[] {
  return s
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .filter((t) => t.length >= 3);
}

// An award counts for THIS site only when the link points at an entry whose
// slug matches the site's own domain/name (or is the platform ribbon). Links to
// other sites' entries (case studies, blog posts, the studio's own profile) are
// mentions, not awards.
export function computeAwards(
  pages: CapturedPage[],
  siteHost: string,
  siteName: string,
): AwardInfo[] {
  const hostCore =
    siteHost
      .replace(/^www\./, '')
      .split('.')
      .slice(0, -1)
      .join('.') || siteHost;
  const own = new Set([...slugTokens(hostCore), ...slugTokens(siteName)]);
  const out = new Map<string, AwardInfo>();
  for (const p of pages) {
    const pr = p.probe as {
      awardLinks?: { href: string; text: string; region: string; fixed: boolean; img: string }[];
    };
    for (const l of pr.awardLinks || []) {
      const plat = AWARD_PLATFORMS.find(([re]) => re.test(l.href));
      if (!plat) continue;
      let u: URL;
      try {
        u = new URL(l.href);
      } catch {
        continue;
      }
      const path = u.pathname.toLowerCase();
      const isEntry =
        /\/(sites|websites|cases|winners?|gallery|website|site)\//.test(path) ||
        plat[1] === 'godly' ||
        plat[1] === 'landbook';
      const slug = path.split('/').filter(Boolean).pop() || '';
      const matches = slugTokens(slug).some((t) => own.has(t));
      const ribbon = l.fixed || l.img === 'ribbon';
      if (!ribbon && !(isEntry && matches)) continue;
      const text = `${l.text} ${l.img} ${path}`.toLowerCase();
      const level = /site of the year|soty/.test(text)
        ? 'site-of-the-year'
        : /site of the month|sotm/.test(text)
          ? 'site-of-the-month'
          : /site of the day|sotd|\bsod\b/.test(text)
            ? 'site-of-the-day'
            : /developer/.test(text)
              ? 'developer-award'
              : /honou?rable|mention|\bhm\b/.test(text)
                ? 'honorable-mention'
                : /nominee/.test(text)
                  ? 'nominee'
                  : /fwa of the (day|month)|\bfotd\b/.test(text)
                    ? 'fwa-of-the-day'
                    : undefined;
      const key = `${plat[1]}:${level || ''}`;
      if (!out.has(key)) {
        out.set(key, {
          platform: plat[1],
          ...(level ? { level } : {}),
          profileUrl: l.href,
          evidence: ribbon ? 'ribbon' : l.img ? 'badge' : 'self-link',
          confidence: ribbon ? 0.9 : l.img ? 0.85 : 0.7,
        });
      }
    }
  }
  return Array.from(out.values());
}

// ─── Identity, traits, digest ────────────────────────────────────────────────

export function cleanTitle(raw: string, siteName: string): string {
  const parts = String(raw || '')
    .split(/\s+[|·•–—-]\s+|\s+::\s+/)
    .map((s) => s.trim())
    .filter(Boolean);
  const keep = parts.filter((p) => !/^(home|homepage|start|welcome|benvenuti|index)$/i.test(p));
  const nonSite = keep.filter((p) => p.toLowerCase() !== siteName.toLowerCase());
  return (nonSite[0] || keep[0] || raw || '').slice(0, 160);
}

export interface PageDigest {
  url: string;
  pageType: string;
  title: string;
  h1: string;
  headings: string[];
  ctas: string[];
  text: string;
}

export function pageDigest(p: CapturedPage, maxChars: number): PageDigest {
  const pr = p.probe as PageProbe & {
    hero?: { headline?: string; ctas?: { text: string }[] };
    headings?: { level: number; text: string }[];
    blocks?: { tag: string; text: string }[];
    ctas?: { text: string }[];
  };
  const headings = (pr.headings || [])
    .filter((h) => h.level <= 2)
    .map((h) => h.text)
    .slice(0, 10);
  const ctas = Array.from(
    new Set((pr.ctas || []).map((c) => c.text).filter((t) => t && t.length < 40)),
  ).slice(0, 8);
  const parts: string[] = [];
  let n = 0;
  for (const b of pr.blocks || []) {
    if (/^h[1-3]$/.test(b.tag)) continue;
    if (n + b.text.length > maxChars) break;
    parts.push(b.text);
    n += b.text.length + 1;
  }
  return {
    url: p.url,
    pageType: p.pageType,
    title: p.title,
    h1: String(pr.hero?.headline || headings[0] || ''),
    headings,
    ctas,
    text: parts.join('\n'),
  };
}

export interface SiteTraits {
  scheme?: string;
  fixedHeader: boolean;
  stickyElements: number;
  grid: boolean;
  glass: boolean;
  blendModes: boolean;
  textGradient: boolean;
  customCursor: boolean;
  marquee: boolean;
  horizontalScroll: boolean;
  containerMaxWidth: string | null;
  cornerRadius: string | null;
  shadows: boolean;
  smoothScroll: string | null;
  scrollJacked: boolean;
  webgl: boolean;
  videoBackground: boolean;
  pageTransitions: string | null;
  heroMedia: string[];
  pageHeights: number[];
}

export function computeTraits(pages: CapturedPage[], tech: TechInfo[]): SiteTraits {
  const t0 = ((pages[0]?.probe as { traits?: Record<string, unknown> })?.traits || {}) as Record<
    string,
    unknown
  >;
  const sumN = (k: string): number =>
    pages.reduce(
      (s, p) =>
        s + Number(((p.probe as { traits?: Record<string, unknown> }).traits || {})[k] || 0),
      0,
    );
  const top = (m: unknown): string | null => {
    const e = Object.entries((m || {}) as Record<string, number>).sort((a, b) => b[1] - a[1])[0];
    return e ? e[0] : null;
  };
  const names = new Set(tech.map((t) => t.name));
  const ctx = ((pages[0]?.probe as { canvasContexts?: Record<string, number> })?.canvasContexts ||
    {}) as Record<string, number>;
  const hero = ((pages[0]?.probe as { hero?: { media?: string[] } })?.hero || {}) as {
    media?: string[];
  };
  return {
    fixedHeader: !!t0.fixedHeader,
    stickyElements: sumN('sticky'),
    grid: sumN('grid') > 2,
    glass: sumN('backdrop') > 0,
    blendModes: sumN('blend') > 0,
    textGradient: sumN('textGradient') > 0,
    customCursor: !!t0.cursorNone,
    marquee: sumN('marquee') > 0,
    horizontalScroll: sumN('horizontalScroll') > 0,
    containerMaxWidth: top(t0.maxWidths),
    cornerRadius: top(t0.radius),
    shadows: sumN('shadows') > 6,
    smoothScroll: names.has('Lenis')
      ? 'Lenis'
      : names.has('Locomotive Scroll')
        ? 'Locomotive Scroll'
        : names.has('ScrollSmoother')
          ? 'ScrollSmoother'
          : null,
    scrollJacked: pages.some((p) => p.jacked),
    webgl: !!(ctx.webgl || ctx.webgl2 || ctx.webgpu) || tech.some((t) => t.category === '3d'),
    videoBackground: (hero.media || []).includes('video'),
    pageTransitions: names.has('Barba.js')
      ? 'Barba.js'
      : names.has('Swup')
        ? 'Swup'
        : names.has('Highway')
          ? 'Highway'
          : null,
    heroMedia: hero.media || [],
    pageHeights: pages.map((p) => p.heightCss),
  };
}

const AWARD_NAMES: Record<string, string> = {
  awwwards: 'Awwwards',
  cssda: 'CSS Design Awards',
  fwa: 'The FWA',
  godly: 'Godly',
  landbook: 'Land-book',
  siteinspire: 'SiteInspire',
  onepagelove: 'One Page Love',
  webby: 'The Webby Awards',
  csswinner: 'CSS Winner',
};

export function awardTags(awards: AwardInfo[]): { tags: string[]; entities: string[] } {
  const tags = new Set<string>();
  const entities = new Set<string>();
  for (const a of awards) {
    if (a.confidence < 0.7) continue;
    tags.add(a.platform === 'landbook' ? 'land-book' : a.platform);
    tags.add('award-winning');
    if (a.level) tags.add(`${a.platform}-${a.level}`);
    entities.add(AWARD_NAMES[a.platform] || a.platform);
  }
  return { tags: Array.from(tags), entities: Array.from(entities) };
}

// JSON-LD → normalized schema.org types + the organization block.
export function parseJsonLd(raw: string[]): {
  types: string[];
  organization: { name?: string; logo?: string; sameAs?: string[]; description?: string } | null;
} {
  const types = new Set<string>();
  let organization: {
    name?: string;
    logo?: string;
    sameAs?: string[];
    description?: string;
  } | null = null;
  const visit = (node: unknown, depth: number): void => {
    if (!node || typeof node !== 'object' || depth > 6) return;
    if (Array.isArray(node)) {
      for (const n of node.slice(0, 50)) visit(n, depth + 1);
      return;
    }
    const o = node as Record<string, unknown>;
    const t = o['@type'];
    for (const ty of Array.isArray(t) ? t : t ? [t] : []) {
      const name = String(ty)
        .replace(/^https?:\/\/schema\.org\//i, '')
        .trim();
      if (name && types.size < 30) types.add(name);
      if (
        !organization &&
        /organization|corporation|localbusiness|store|restaurant|brand|ngo|educationalorganization|professionalservice/i.test(
          name,
        )
      ) {
        const logo =
          o.logo && typeof o.logo === 'object' ? (o.logo as Record<string, unknown>).url : o.logo;
        organization = {
          name: typeof o.name === 'string' ? o.name.slice(0, 120) : undefined,
          logo: typeof logo === 'string' ? logo : undefined,
          sameAs: Array.isArray(o.sameAs)
            ? (o.sameAs as unknown[]).filter((x): x is string => typeof x === 'string').slice(0, 12)
            : undefined,
          description: typeof o.description === 'string' ? o.description.slice(0, 300) : undefined,
        };
      }
    }
    if (o['@graph']) visit(o['@graph'], depth + 1);
  };
  for (const r of raw) {
    try {
      visit(JSON.parse(r), 0);
    } catch {
      /* malformed block */
    }
  }
  return { types: Array.from(types), organization };
}
