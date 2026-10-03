// The actual desktop rich catalog contract and mapper, never a copied oracle.
import {
  mapCatalog,
  WEB_CATALOG_FORMAT,
  WEB_CATALOG_SYSTEM,
} from '../../electron/webcap/ai-catalog';
import type { GoldenSet } from './lib';
const canonical = (v: unknown): unknown =>
  Array.isArray(v)
    ? v.map(canonical)
    : v && typeof v === 'object'
      ? Object.fromEntries(
          Object.entries(v)
            .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
            .map(([k, x]) => [k, canonical(x)]),
        )
      : v;
const raw = {
  observations: '  Large left heading  ',
  site_type: 'portfolio-studio',
  site_type_secondary: 'agency',
  industry: 'design-creative',
  audience: 'Art directors',
  style: ['minimal', 'type-led', 'minimal', 'bad'],
  theme: 'dark',
  color_mood: ['monochrome'],
  density: 'sparse',
  layout_patterns: ['bento', 'grid', 'other'],
  hero_type: 'typographic',
  imagery: ['photography'],
  typography: ['serif editorial', 'monospace accents', 'script accents'],
  components: ['cards'],
  craft: 'polished',
  notable_details: ['One detail', 'Two detail'],
  reference_for: ['Borrow type for portfolios'],
  tags: ['#HELLO', 'Detail', 'Detail', 'bad'.repeat(30)],
  search_keywords: ['studio references'],
  summary: '  Summary  ',
  description: ' Description ',
};
const context = {
  webMeta: {
    scheme: 'dark',
    lang: 'it',
    siteName: 'Studio',
    organization: { name: 'Acme' },
    tech: [
      { name: 'React', confidence: 0.9 },
      { name: 'Wrong', confidence: 0.79 },
    ],
    traits: { smoothScroll: 'lenis', webgl: true, glass: true, videoBackground: true },
    awardEntities: ['Awwwards'],
  },
  webFonts: [
    { family: 'Inter', classification: 'sans' },
    { family: 'Playfair', classification: 'serif' },
    { family: 'Inter', classification: 'sans' },
  ],
  webPalette: [
    { name: 'ink', role: 'background' },
    { name: 'red', role: 'accent' },
    { name: 'ink', role: 'accent' },
    { name: 'white', role: 'text' },
  ],
  webAwards: [{ platform: 'Awwwards' }],
};
const cases: [[unknown, unknown, string], ...Array<[unknown, unknown, string]>] = [
  [{}, {}, 'stub'],
  [raw, context, 'vision'],
  [
    {
      ...raw,
      theme: 'invalid',
      site_type: 'invalid',
      style: ['invalid'],
      summary: 'a'.repeat(500),
    },
    context,
    'vision',
  ],
  [raw, { ...context, webFonts: [] }, 'vision'],
  [
    raw,
    {
      webMeta: { scheme: 'system' },
      webFonts: [
        { family: 'Mono', classification: 'mono' },
        { family: 'Script', classification: 'script' },
      ],
    },
    'vision',
  ],
];
const sets: GoldenSet[] = [
  {
    name: 'ai/web-design/contract',
    source: 'electron/webcap/ai-catalog.ts#WEB_CATALOG_FORMAT+WEB_CATALOG_SYSTEM',
    generator: 'scripts/golden/ai-web-design.ts',
    build: () => [
      {
        id: 'rich-v2',
        args: [],
        output: canonical({
          system: WEB_CATALOG_SYSTEM,
          schema: WEB_CATALOG_FORMAT.json_schema.schema,
          name: WEB_CATALOG_FORMAT.json_schema.name,
        }),
      },
    ],
  },
  {
    name: 'ai/web-design/map',
    source: 'electron/webcap/ai-catalog.ts#mapCatalog',
    generator: 'scripts/golden/ai-web-design.ts',
    build: () =>
      cases.map(([raw, post, model], i) => ({
        id: `design-${i}`,
        args: [raw, post, model],
        output: canonical(
          mapCatalog(raw as Parameters<typeof mapCatalog>[0], post as Shelfy.Post, model),
        ),
      })),
  },
];
export default sets;
