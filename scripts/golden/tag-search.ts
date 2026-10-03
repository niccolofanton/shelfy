// P3-05: execute the real desktop searchPostsByTags on synthetic libraries.
// Unique timestamps make desktop ties deterministic. The core's final id
// tie-break for identical timestamps is checked separately in Rust.
import { openDesktopDb, withDesktopClock, type GoldenSet } from './lib';
import { NOW_MS, type Alias } from './edits';

type Post = [string, string, number, string[], string[]];
const aliases: Alias[] = [
  ['lampade', 'lamp', 'Lamp', 'accepted'],
  ['lights', 'lampade', 'Lampade', 'accepted'],
  ['vetri', 'glass', 'Glass', 'proposed'],
];
const posts: Post[] = [
  ['ig_1', 'instagram', NOW_MS, ['design'], []],
  ['ig_2', 'instagram', NOW_MS - 1, ['lamp', 'design'], []],
  ['ig_3', 'instagram', NOW_MS - 2, ['lamp', 'glass'], []],
  ['web_a', 'web', NOW_MS - 3, ['design', 'glass'], []],
  ['m_1', 'manual', NOW_MS - 4, [], ['lamp']],
  ['ig_4', 'instagram', NOW_MS - 5, ['Città', 'interior design'], []],
  ['ig_5', 'instagram', NOW_MS - 6, ['glass'], []],
  ...Array.from(
    { length: 23 },
    (_, i): Post => [`ig_${i + 10}`, 'instagram', NOW_MS - i - 10, ['design'], []],
  ),
];
const cases: [
  string,
  string[],
  { mode?: 'or' | 'and'; source?: string; limit?: number; offset?: number },
][] = [
  ['or-idf', ['design', 'lamp'], {}],
  ['and', ['design', 'lamp'], { mode: 'and' }],
  ['and-missing', ['lamp', 'absent'], { mode: 'and' }],
  ['or-missing', ['lamp', 'absent'], {}],
  ['alias-chain-dedup', [' LIGHTS ', 'lampade', 'Lamp', 'design', 'design'], {}],
  ['and-alias-dedup', ['lampade', 'lamp', 'design'], { mode: 'and' }],
  ['proposed-unresolved', ['vetri'], {}],
  ['sites', ['glass', 'design', 'lamp'], { source: 'web' }],
  ['social-includes-manual', ['glass', 'lamp'], { source: 'social' }],
  ['unicode', ['\uFEFFCITTÀ\uFEFF', 'INTERIOR DESIGN'], {}],
  ['exact-tag-only', ['interior', 'designs', 'lam'], {}],
  ['page', ['design', 'lamp'], { offset: 2, limit: 3 }],
  ['empty', [], {}],
  ['blank', [' ', '\uFEFF'], {}],
  ['punctuation', ['" OR * % _'], {}],
];
const set: GoldenSet = {
  name: 'tag-search',
  source: 'electron/db.ts#searchPostsByTags',
  generator: 'scripts/golden/tag-search.ts',
  build() {
    const { db, sql } = openDesktopDb();
    try {
      return withDesktopClock(sql, NOW_MS, () => {
        const alias = sql.prepare(
          'INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status) VALUES (?, ?, ?, ?)',
        );
        for (const row of aliases) alias.run(...row);
        const insert = sql.prepare('INSERT INTO posts (id, platform, timestamp) VALUES (?, ?, ?)');
        for (const [key, platform, time, tags, manual] of posts) {
          insert.run(key, platform, new Date(time).toISOString());
          db.updateAiAnalysis(key, {
            tags,
            generalTags: ['design'],
            specificTags: ['lamp', 'glass'],
          });
          db.updateUserContent(key, { manualTags: manual });
        }
        db.invalidateGlobalCaches();
        return cases.map(([id, tags, options]) => {
          const result = db.searchPostsByTags(tags, options);
          return {
            id,
            args: [posts, aliases, tags, options],
            output: { keys: result.posts.map((p) => p.id), total: result.total },
          };
        });
      });
    } finally {
      sql.close();
    }
  },
};
export default set;
