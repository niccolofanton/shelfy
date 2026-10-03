// Synthetic explorer and maintenance fixtures from the real desktop database.
import { openDesktopDb, type GoldenSet } from './lib';
const generator = 'scripts/golden/ai-tags.ts';
const set = (name: string, source: string, build: GoldenSet['build']): GoldenSet => ({
  name: `ai/tags/${name}`,
  source: `electron/db.ts#${source}`,
  generator,
  build,
});
interface Post {
  tags: string[];
  manual: string[];
  general: string[];
  specific: string[];
  entities: string[];
  category: string | null;
  type: string | null;
  language: string | null;
  status: string | null;
  timestamp: number;
}
const posts: Post[] = [
  {
    tags: ['Lamp', 'chair'],
    manual: ['desk'],
    general: ['Lamp'],
    specific: ['chair'],
    entities: ['Studio One'],
    category: 'architecture',
    type: 'photo',
    language: 'it',
    status: 'done',
    timestamp: 1000,
  },
  {
    tags: ['Lamp', 'chairs'],
    manual: [],
    general: ['Lamp'],
    specific: ['chairs'],
    entities: ['studio one'],
    category: 'architecture',
    type: 'photo',
    language: 'en',
    status: 'done',
    timestamp: 2000,
  },
  {
    tags: ['café', 'chaire'],
    manual: [],
    general: [],
    specific: ['café', 'chaire'],
    entities: ['Studio Two'],
    category: 'design',
    type: 'graphic',
    language: 'fr',
    status: 'done',
    timestamp: 3000,
  },
  {
    tags: ['cafe', 'desk'],
    manual: [],
    general: ['desk'],
    specific: ['cafe'],
    entities: [],
    category: 'design',
    type: 'graphic',
    language: 'fr',
    status: 'done',
    timestamp: 4000,
  },
  {
    tags: [],
    manual: [],
    general: [],
    specific: [],
    entities: [],
    category: null,
    type: null,
    language: null,
    status: 'done',
    timestamp: 5000,
  },
  {
    tags: [],
    manual: [],
    general: [],
    specific: [],
    entities: [],
    category: null,
    type: null,
    language: null,
    status: null,
    timestamp: 6000,
  },
];
function seed(sql: ReturnType<typeof openDesktopDb>['sql'], dataset: Post[] = posts) {
  for (const [i, p] of dataset.entries()) {
    sql
      .prepare(
        "INSERT INTO posts(id,platform,timestamp,ai_tags,user_tags,ai_entities,ai_category,ai_content_type,ai_language,ai_status) VALUES (?,'instagram',?,?,?,?,?,?,?,?)",
      )
      .run(
        String(i + 1),
        new Date(p.timestamp).toISOString(),
        JSON.stringify(p.tags),
        JSON.stringify(p.manual),
        JSON.stringify(p.entities),
        p.category,
        p.type,
        p.language,
        p.status,
      );
    for (const t of p.tags)
      sql
        .prepare('INSERT INTO post_tags(post_id,tag_norm,tag_form,tier) VALUES(?,?,?,?)')
        .run(
          String(i + 1),
          t.toLowerCase(),
          t,
          p.specific.includes(t) ? 'specific' : p.general.includes(t) ? 'general' : null,
        );
    for (const t of p.manual)
      sql
        .prepare("INSERT INTO post_tags(post_id,tag_norm,tag_form,tier) VALUES(?,?,?,'manual')")
        .run(String(i + 1), t.toLowerCase(), t);
    for (const t of p.entities)
      sql
        .prepare('INSERT INTO post_entities(post_id,ent_norm,ent_form) VALUES(?,?,?)')
        .run(String(i + 1), t.toLowerCase(), t);
  }
  sql
    .prepare("INSERT INTO tag_cluster(id,label,status,run_id) VALUES(1,'Furniture','accepted',1)")
    .run();
  sql.prepare("INSERT INTO tag_cluster_membership(cluster_id,tag_norm) VALUES(1,'chair')").run();
}
const read = (fn: (db: ReturnType<typeof openDesktopDb>['db']) => unknown) => {
  const { db, sql } = openDesktopDb();
  try {
    seed(sql);
    return fn(db);
  } finally {
    db.close();
  }
};
const suggestionSets = [
  posts,
  [
    'aaaa',
    'aabb',
    'bbbb',
    'distantlong',
    'cafe',
    'café',
    'caffè',
    'CAFETERIA',
    'ab',
    'abc',
    'abcd',
    'aXcd',
    '😀aaaa',
    '😀aabb',
    'é',
    'e\u0301',
    '☃',
    'a'.repeat(256),
    'a'.repeat(255) + 'b',
  ].map(
    (tag, i): Post => ({
      tags: [tag],
      manual: [],
      general: [],
      specific: [],
      entities: [],
      category: null,
      type: null,
      language: null,
      status: 'done',
      timestamp: i + 1,
    }),
  ),
  [],
];
const operations = [
  { sources: ['chairs', 'chaire'], target: 'chair' },
  { sources: ['Lamp'], target: 'Lighting' },
  { sources: ['café', 'cafe'], target: 'Coffee' },
  { sources: ['unknown'], target: 'unknown' },
  { sources: ['desk'], target: 'Table' },
];
export default [
  set('overview', 'getAiOverview', () => [
    { id: 'library', args: [posts], output: read((db) => db.getAiOverview()) },
  ]),
  set('stats', 'getTagStats', () =>
    ([null, 'general', 'specific'] as const).map((tier) => ({
      id: `tier-${tier ?? 'all'}`,
      args: [posts, tier ?? 'all'],
      output: read((db) =>
        db
          .getTagStats({ tier, limit: 200 })
          .map((r) => ({ ...r, lastUsed: r.lastUsed ? Date.parse(r.lastUsed) : null })),
      ),
    })),
  ),
  set('entities', 'getEntityStats', () => [
    { id: 'forms', args: [posts], output: read((db) => db.getEntityStats({ limit: 60 })) },
  ]),
  set('related', 'getTagCooccurrence', () =>
    ['lamp', 'CHAIR ', 'absent', 'café'].map((tag) => ({
      id: tag,
      args: [posts, tag],
      output: read((db) => db.getTagCooccurrence(tag, { limit: 12 })),
    })),
  ),
  set('health', 'getTagHealth', () => [
    { id: 'library', args: [posts], output: read((db) => db.getTagHealth()) },
  ]),
  set('suggestions', 'getTagMergeSuggestions', () =>
    suggestionSets.map((dataset, i) => {
      const { db, sql } = openDesktopDb();
      try {
        seed(sql, dataset);
        return {
          id: `distance-${i}`,
          args: [dataset],
          output: db.getTagMergeSuggestions({ limit: 100 }),
        };
      } finally {
        db.close();
      }
    }),
  ),
  set('post-keys', 'getPostIdsByTags', () =>
    [['lamp'], ['lamp', 'chair'], [], ['absent'], ['lamp', 'desk']].flatMap((tags) =>
      ['and', 'or'].map((mode) => ({
        id: `${JSON.stringify(tags)}-${mode}`,
        args: [posts, tags, mode],
        output: read((db) => db.getPostIdsByTags(tags, mode)),
      })),
    ),
  ),
  set('merge', 'mergeTags,renameTag', () =>
    operations.map((op, i) => {
      const { db, sql } = openDesktopDb();
      try {
        seed(sql);
        const result =
          i === 1 ? db.renameTag(op.sources[0], op.target) : db.mergeTags(op.sources, op.target);
        return {
          id: `merge-${i}`,
          args: [posts, op.sources, op.target],
          output: {
            updated: result.updated,
            posts: (
              sql
                .prepare('SELECT id,ai_tags AS ai,user_tags AS manual FROM posts ORDER BY id')
                .all() as { id: string; ai: string; manual: string }[]
            ).map((r) => ({ id: r.id, ai: JSON.parse(r.ai), manual: JSON.parse(r.manual) })),
            rows: sql
              .prepare(
                'SELECT post_id AS id,tag_norm AS norm,tag_form AS form,tier FROM post_tags ORDER BY post_id,tag_norm',
              )
              .all(),
            members: sql
              .prepare(
                'SELECT cluster_id AS cluster,tag_norm AS norm FROM tag_cluster_membership ORDER BY tag_norm',
              )
              .all(),
          },
        };
      } finally {
        db.close();
      }
    }),
  ),
] satisfies GoldenSet[];
