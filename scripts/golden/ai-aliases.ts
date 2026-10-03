// Synthetic allowlist, candidate and alias persistence fixtures.
import { installDesktopShims, openDesktopDb, type GoldenSet } from './lib';
installDesktopShims();
const { validateAliasPairs } = await import('../../electron/analyzer');
const GENERATOR = 'scripts/golden/ai-aliases.ts';
const set = (name: string, source: string, build: GoldenSet['build']): GoldenSet => ({
  name: `ai/aliases/${name}`,
  source,
  generator: GENERATOR,
  build,
});
const batch = [
  { norm: 'lamps', form: 'Lamps', count: 2 },
  { norm: 'chairs', form: 'Chairs', count: 3 },
];
const vocab = [
  { norm: 'lamp', form: 'Lamp', count: 8 },
  { norm: 'chair', form: 'Chair', count: 9 },
  ...batch,
];
const validations = [
  null,
  {
    aliases: [
      { alias: ' LAMPS ', canonical: 'LAMP' },
      { alias: 'chairs', canonical: 'chair' },
    ],
  },
  {
    aliases: [
      { alias: 'lamps', canonical: 'lamp' },
      { alias: 'lamps', canonical: 'chair' },
    ],
  },
  {
    aliases: [
      { alias: 'lamps', canonical: 'lamps' },
      { alias: 'lamps', canonical: 'chairs' },
      { alias: 'unknown', canonical: 'lamp' },
      { alias: 'chairs', canonical: 'invented' },
      { alias: 'chairs', canonical: 'chair' },
    ],
  },
];
const datasets = [
  [
    ['Lamp', 'lamps', 'chair'],
    ['Lamp', 'chairs'],
    ['chair', 'chairs'],
    ['Lamp', 'lamps'],
  ],
  [['a', 'b'], ['a', 'B'], ['c']],
  [],
];
const pairs = [
  { aliasNorm: 'lamps', aliasForm: 'Lamps', canonicalNorm: 'lamp', canonicalForm: 'Lamp' },
  { aliasNorm: 'chairs', aliasForm: 'Chairs', canonicalNorm: 'chair', canonicalForm: 'Chair' },
];
export default [
  set('validate', 'electron/analyzer.ts#validateAliasPairs', () =>
    validations.map((parsed, i) => ({
      id: `pairs-${i}`,
      args: [batch, vocab, parsed],
      output: validateAliasPairs(batch, vocab, parsed),
    })),
  ),
  set('candidates', 'electron/db.ts#getUnaliasedTags,getCanonicalVocab', () =>
    datasets.map((posts, i) => {
      const { db, sql } = openDesktopDb();
      try {
        for (const [n, tags] of posts.entries()) {
          const id = `post-${n}`;
          sql.prepare("INSERT INTO posts (id,platform) VALUES (?,'instagram')").run(id);
          for (const form of tags)
            sql
              .prepare('INSERT INTO post_tags (post_id,tag_norm,tag_form) VALUES (?,?,?)')
              .run(id, form.toLowerCase(), form);
        }
        sql
          .prepare(
            "INSERT INTO tag_alias (alias_norm,canonical_norm,canonical_form,status) VALUES ('lamps','lamp','Lamp','proposed')",
          )
          .run();
        return {
          id: `vocab-${i}`,
          args: [posts],
          output: {
            unaliased: db.getUnaliasedTags({ limit: 400 }),
            canonical: db.getCanonicalVocab({ limit: 300 }),
          },
        };
      } finally {
        db.close();
      }
    }),
  ),
  set('save', 'electron/db.ts#saveTagAliases', () => {
    const { db, sql } = openDesktopDb();
    try {
      sql.prepare("INSERT INTO posts (id,platform) VALUES ('post','instagram')").run();
      sql
        .prepare(
          "INSERT INTO post_tags (post_id,tag_norm,tag_form) VALUES ('post','lamps','Lamps')",
        )
        .run();
      const result = db.saveTagAliases(pairs);
      const aliases = db.getTagAliases({ status: 'proposed' });
      return [
        {
          id: 'proposed-leaves-posts',
          args: [pairs],
          output: {
            result,
            aliases,
            norms: (
              sql.prepare('SELECT tag_norm AS norm FROM post_tags').all() as { norm: string }[]
            ).map((r) => r.norm),
          },
        },
      ];
    } finally {
      db.close();
    }
  }),
] satisfies GoldenSet[];
