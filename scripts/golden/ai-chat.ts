// Execute the desktop's offline chat helpers on an isolated synthetic library.
import { createRequire } from 'module';
import { openDesktopDb, type GoldenCase, type GoldenSet } from './lib';

export const chatPosts: [string, string, string[], string[], string[]][] = [
  ['a', 'Walnut desk lamp design', ['design'], ['desk lamp', 'walnut'], ['walnut desk lamp']],
  ['b', 'Glass desk lamp', ['design'], ['desk lamp', 'glass'], ['glass desk lamp']],
  ['c', 'Garden chair design', ['design'], ['chair', 'garden'], ['garden chair']],
  ['d', 'Città architecture', ['architecture'], ['città'], ['urban architecture']],
  ['e', 'Empty caption', [], ['lamp shade'], []],
];
export const chatAliases = [
  ['lamps', 'desk lamp', 'Desk Lamp', 'accepted'],
  ['seats', 'chair', 'Chair', 'proposed'],
];
export function openChatDesktop() {
  const handle = openDesktopDb();
  const require = createRequire(import.meta.url);
  const file = require.resolve('../../electron/analyzer.ts');
  delete require.cache[file];
  const analyzer = require(file) as typeof import('../../electron/analyzer');
  return { ...handle, analyzer };
}
function setup() {
  const h = openChatDesktop();
  for (const [id, text, general, specific, keywords] of chatPosts) {
    h.sql
      .prepare("INSERT INTO posts(id,platform,text,timestamp) VALUES (?,'instagram',?,?)")
      .run(id, text, `2026-01-0${chatPosts.findIndex((p) => p[0] === id) + 1}T00:00:00Z`);
    h.db.updateAiAnalysis(id, {
      tags: [...general, ...specific],
      generalTags: general,
      specificTags: specific,
      keywords,
    });
  }
  for (const row of chatAliases)
    h.sql
      .prepare(
        'INSERT INTO tag_alias(alias_norm,canonical_norm,canonical_form,status) VALUES (?,?,?,?)',
      )
      .run(...row);
  h.db.invalidateGlobalCaches();
  return h;
}
const set = (name: string, build: () => GoldenCase[]): GoldenSet => ({
  name: `ai/chat/${name}`,
  source: `electron/analyzer.ts#${name}`,
  generator: 'scripts/golden/ai-chat.ts',
  build,
});
const markerCases: [string, unknown, string[]][] = [
  ['missing', 'no block', ['lamp']],
  ['normal', 'reply [[X]] #LAMP, glass\nlamp [[/X]] ignored', ['lamp', 'glass']],
  ['unterminated', '[[X]] lamp  glass\n#unknown', ['lamp', 'glass']],
  [
    'multiword',
    '[[X]] desk lamp, interior design\n#città [[/X]]',
    ['desk lamp', 'interior design', 'città'],
  ],
  ['first-block', '[[X]]lamp[[/X]][[X]]glass[[/X]]', ['lamp', 'glass']],
  ['null', null, ['lamp']],
  [
    'js-not-unicode-space',
    '[[X]]lamp\u0085\u0085glass,lamp\ufeff\ufeffglass[[/X]]',
    ['lamp', 'glass', 'lamp\u0085\u0085glass'],
  ],
  ['spacing', '[[X]]\ufeff#CITTÀ\u00a0,desk lamp\t\tglass[[/X]]', ['città', 'desk lamp', 'glass']],
  [
    'cap-composition',
    `[[X]]${Array.from({ length: 35 }, (_, i) => `tag ${i}`).join(',')}[[/X]]`,
    Array.from({ length: 35 }, (_, i) => `tag ${i}`),
  ],
];
const keywordTexts: unknown[] = [
  null,
  '',
  'no marker',
  '[[X]]#Desk   lamp, glass, x, café, desk lamp\nwood floor[[/X]]',
  '[[X]]one two three four five, ok, ai, brass, lamp, desk, chair, ninth[[/X]]',
  '[[X]]' + '🎧'.repeat(21) + ', café, città',
  '[[X]]a\nbb\ncc[[/X]]tail',
  '[[X]]café\u0085glass, café\ufeffglass[[/X]]',
];
const queries = [
  '',
  'lamp',
  'desk lamp',
  'walnut',
  'garden',
  'città',
  'missing',
  '" OR *',
  'architecture',
];
export default [
  set('parseTagBlock', () => {
    const h = setup();
    try {
      return markerCases.map(([id, text, vocab]) => ({
        id,
        args: [text, '[[X]]', '[[/X]]', vocab],
        output: h.analyzer.parseTagBlock(text, '[[X]]', '[[/X]]', new Set(vocab)),
      }));
    } finally {
      h.db.close();
    }
  }),
  set('parseKeywordBlock', () => {
    const h = setup();
    try {
      return keywordTexts.map((text, i) => ({
        id: `kw-${i}`,
        args: [text, '[[X]]', '[[/X]]'],
        output: h.analyzer.parseKeywordBlock(text, '[[X]]', '[[/X]]'),
      }));
    } finally {
      h.db.close();
    }
  }),
  set('deterministicKeywords', () => {
    const h = setup();
    try {
      return [
        ...queries,
        'cerco una lampada in legno e vetro',
        '3D AI UX design café brass walnut glass chair garden',
        'İstanbul città 日本語',
      ].map((text, i) => ({
        id: `terms-${i}`,
        args: [text],
        output: h.analyzer.deterministicKeywords(text),
      }));
    } finally {
      h.db.close();
    }
  }),
  set('intersectWithVocab', () => {
    const h = setup();
    try {
      const vocab = h.db.getTagStats({ limit: 100000 }).map((t) => t.tag.toLowerCase());
      return [
        [' LAMPS ', 'desk lamp', 'lamps', 'seats', 'chair'],
        ['desk', 'walnut', 'missing'],
        ['CITTÀ', 'glass', 'shade'],
        [],
      ].map((values, i) => ({
        id: `intersect-${i}`,
        args: [values],
        output: h.analyzer.intersectWithVocab(values, new Set(vocab)),
      }));
    } finally {
      h.db.close();
    }
  }),
  set('expandQueryToVocab', () => {
    const h = setup();
    try {
      return queries.map((text, i) => ({
        id: `expand-${i}`,
        args: [text, 60],
        output: h.analyzer.expandQueryToVocab(text, { limit: 60 }),
      }));
    } finally {
      h.db.close();
    }
  }),
  set('deterministicTagMatches', () => {
    const h = setup();
    try {
      return queries.flatMap((text, i) =>
        [[], ['design'], ['desk lamp', 'garden']].map((active, j) => ({
          id: `matches-${i}-${j}`,
          args: [text, active, ['design', 'architecture']],
          output: h.analyzer.deterministicTagMatches(text, active, ['design', 'architecture']),
        })),
      );
    } finally {
      h.db.close();
    }
  }),
];
