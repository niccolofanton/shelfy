// Synthetic desktop clustering and review fixtures; no model or personal data.
import { createRequire } from 'node:module';
import { buildTagCommunities, cosineSim } from '../../electron/cluster-core';
import {
  openDesktopDb,
  installDesktopShims,
  withDesktopClock,
  type GoldenCase,
  type GoldenSet,
} from './lib';
installDesktopShims();
const { parseRefineResponse, validateRefinedGroups } = await import('../../electron/analyzer');
const GENERATOR = 'scripts/golden/ai-clusters.ts';
const set = (name: string, source: string, build: GoldenSet['build']): GoldenSet => ({
  name: `ai/clusters/${name}`,
  source,
  generator: GENERATOR,
  build,
});

interface GraphInput {
  freq: [string, number][];
  edges: { a: string; b: string; c: number }[];
  vectors: Record<string, number[]> | null;
  options: { maxGroupSize?: number; iterations?: number; minJaccard?: number; alpha?: number };
}
const graphs: [string, GraphInput][] = [
  ['empty', { freq: [], edges: [], vectors: null, options: {} }],
  [
    'isolated',
    {
      freq: [
        ['a', 3],
        ['b', 3],
      ],
      edges: [],
      vectors: null,
      options: {},
    },
  ],
];
let seed = 919;
const random = () => {
  seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
  return seed / 2 ** 32;
};
for (let k = 0; k < 32; k++) {
  const n = 4 + k;
  const freq: [string, number][] = Array.from({ length: n }, (_, i) => [
    `tag ${String(i).padStart(2, '0')}`,
    2 + Math.floor(random() * 20),
  ]);
  const edges: GraphInput['edges'] = [];
  for (let a = 0; a < n; a++)
    for (let b = a + 1; b < n; b++)
      if (random() > 0.5)
        edges.push({
          a: freq[a][0],
          b: freq[b][0],
          c: Math.min(freq[a][1], freq[b][1], 2 + Math.floor(random() * 8)),
        });
  const vectors =
    k % 2
      ? Object.fromEntries(freq.map(([name], i) => [name, i % 2 ? [0, 1, 0] : [1, 0, 0]]))
      : null;
  graphs.push([`seeded-${k}`, { freq, edges, vectors, options: { maxGroupSize: 4 + (k % 7) } }]);
}
for (const n of [14, 15, 28, 29, 50]) {
  const freq: [string, number][] = Array.from({ length: n }, (_, i) => [`dense ${i}`, 5]);
  const edges: GraphInput['edges'] = [];
  for (let a = 0; a < n; a++)
    for (let b = a + 1; b < n; b++) edges.push({ a: freq[a][0], b: freq[b][0], c: 5 });
  graphs.push([`dense-split-${n}`, { freq, edges, vectors: null, options: {} }]);
}
graphs.push([
  'utf16-ties',
  {
    freq: [
      ['\ue000', 2],
      ['🎧', 2],
      ['a', 2],
    ],
    edges: [
      { a: '\ue000', b: '🎧', c: 2 },
      { a: 'a', b: '🎧', c: 2 },
    ],
    vectors: null,
    options: {},
  },
]);
const parseInputs = [
  null,
  '',
  ' ',
  '{"groups":[],"outliers":[]}',
  '{"groups":[{"name":"Theme","tags":["a","b"]}],"outliers":[]}',
  '{"groups":[{"name":"Theme","tags":["a","b"]}, {"name":"Other","tags":["c","d"]}',
  'noise {"name":"Theme","tags":["a","b"]} tail',
  '{"groups":[{"name":"{ odd }","tags":["a","b"]}',
];
const refined: [string, string[], unknown][] = [
  ['valid', ['a', 'b', 'c'], { groups: [{ name: ' Theme ', tags: ['A', 'b', 'invented', 'a'] }] }],
  [
    'cross-group-first-wins',
    ['a', 'b', 'c', 'd'],
    {
      groups: [
        { name: 'one', tags: ['a', 'b'] },
        { name: 'two', tags: ['b', 'c', 'd'] },
      ],
    },
  ],
  [
    'singleton-consumes',
    ['a', 'b', 'c'],
    {
      groups: [
        { name: 'one', tags: ['a'] },
        { name: 'two', tags: ['a', 'b', 'c'] },
      ],
    },
  ],
  [
    'invalid',
    ['a', 'b'],
    {
      groups: [
        null,
        { name: '', tags: ['a', 'b'] },
        { name: 4, tags: ['a', 'b'] },
        { name: 'two', tags: [42, 'a', 'b'] },
      ],
    },
  ],
  ['empty', ['a', 'b'], null],
];
// The embedding module is replaced only at db.ts's lazy seam; workers receive
// this fixed table. No model file or network endpoint is consulted.
const requireHere = createRequire(import.meta.url);
const loader = requireHere('module') as {
  _load: (request: string, parent: unknown, isMain: boolean) => unknown;
};
const candidateCases: GoldenCase[] = [];
const tailPosts = [
  ['a', 'b'],
  ['a', 'b'],
  ['c', 'd'],
  ['c', 'd'],
  ['a', 'tail'],
  ['a', 'tail'],
  ...Array.from({ length: 20 }, () => ['tail']),
  ['solo'],
];
const scenes = [
  [],
  tailPosts,
  [
    Array.from({ length: 16 }, (_, i) => `dense-${i}`),
    Array.from({ length: 16 }, (_, i) => `dense-${i}`),
  ],
];
for (const [i, posts] of scenes.entries()) {
  for (const fused of [false, true]) {
    const vectors = fused
      ? Object.fromEntries(
          [...new Set(posts.flat())].map((n) => [n, n === 'tail' ? [1, 0] : [0, 1]]),
        )
      : null;
    const { db, sql } = openDesktopDb();
    const originalLoad = loader._load;
    const originalLog = console.log;
    try {
      posts.forEach((tags, n) => {
        const id = `candidate-${n}`;
        sql.prepare("INSERT INTO posts (id,platform) VALUES (?,'instagram')").run(id);
        tags.forEach((t) =>
          sql
            .prepare('INSERT INTO post_tags (post_id,tag_norm,tag_form) VALUES (?,?,?)')
            .run(id, t, t),
        );
      });
      loader._load = function (request, parent, isMain) {
        if (
          request === './embeddings' &&
          (parent as { filename?: string })?.filename?.endsWith('/electron/db.ts')
        ) {
          return {
            isEmbeddingReady: () => vectors !== null,
            embedTexts: async (forms: string[]) => forms.map((f) => vectors?.[f]),
            cosineSim,
          };
        }
        return originalLoad.call(this, request, parent, isMain);
      };
      console.log = () => {}; // desktop fallback diagnostics, not fixture content
      const output = await db.getTagCandidateGroups();
      candidateCases.push({
        id: `candidates-${i}-${fused ? 'vectors' : 'jaccard'}`,
        args: [posts, vectors],
        output,
      });
    } finally {
      loader._load = originalLoad;
      console.log = originalLog;
      db.close();
    }
  }
}
export default [
  set('candidates', 'electron/db.ts#getTagCandidateGroups', () => candidateCases),
  set('graph', 'electron/cluster-core.ts#buildTagCommunities', () =>
    graphs.map(([id, g]) => ({
      id,
      args: [g],
      output: buildTagCommunities(new Map(g.freq), g.edges, {
        ...g.options,
        cosSim: g.vectors ? (a, b) => cosineSim(g.vectors![a] || [], g.vectors![b] || []) : null,
      }),
    })),
  ),
  set('parse', 'electron/analyzer.ts#parseRefineResponse', () =>
    parseInputs.map((value, i) => ({
      id: `parse-${i}`,
      args: [value],
      output: parseRefineResponse(value),
    })),
  ),
  set('refine', 'electron/analyzer.ts#validateRefinedGroups', () =>
    refined.map(([id, tags, parsed]) => ({
      id,
      args: [tags, parsed],
      output: validateRefinedGroups(tags, parsed),
    })),
  ),
  set('save', 'electron/db.ts#saveClusterRun', () => {
    const scenarios = [
      [
        { label: 'One', tags: ['a', 'b'] },
        { label: 'Two', tags: ['c', 'd'] },
      ],
      [
        { label: ' Fresh ', tags: ['A', 'a', 'b', 'c', 'd'] },
        { label: '', tags: ['e', 'f'] },
        { label: 'Single', tags: ['g'] },
      ],
      [],
    ];
    return scenarios.map((groups, i) => {
      const { db, sql } = openDesktopDb();
      try {
        return withDesktopClock(sql, 1234, () => {
          db.saveClusterRun([
            { label: 'Accepted', tags: ['a', 'b'] },
            { label: 'Old', tags: ['x', 'y'] },
          ]);
          const accepted = sql
            .prepare("SELECT id FROM tag_cluster WHERE label='Accepted'")
            .get() as { id: number };
          db.setClusterStatus(accepted.id, 'accepted');
          const saved = db.saveClusterRun(groups);
          const stored = sql
            .prepare('SELECT label, status, run_id AS runId FROM tag_cluster ORDER BY label')
            .all() as { label: string; status: string; runId: number }[];
          const rows = stored.map((c) => ({
            ...c,
            tags: (
              sql
                .prepare(
                  'SELECT m.tag_norm AS norm FROM tag_cluster_membership m JOIN tag_cluster c ON c.id=m.cluster_id WHERE c.label=? ORDER BY m.tag_norm',
                )
                .all(c.label) as { norm: string }[]
            ).map((t) => t.norm),
          }));
          return { id: `save-${i}`, args: [groups], output: { saved, rows } };
        });
      } finally {
        db.close();
      }
    });
  }),
] satisfies GoldenSet[];
