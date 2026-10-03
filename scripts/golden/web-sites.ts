// Golden sets for the Websites listing's colour math and "similar sites"
// (P4-05, plan §2.14, WEB-45): the parts the port must reproduce byte for
// byte because the desktop's algorithm, unlike its free-text search (D10,
// ported onto FTS5 on purpose, never golden), is not changed by the plan.
//
//   shared/golden/web/hex-to-lab.jsonl    electron/webcap/metadata.ts#hexToLab
//   shared/golden/web/color-filter.jsonl  electron/db.ts#queryWebReferences (color, sort)
//   shared/golden/web/facets.jsonl        electron/db.ts#getWebFacetCounts
//   shared/golden/web/similar.jsonl       electron/db.ts#similarWebReferences
//
// `color-filter`, `facets` and `similar` write `posts` and `post_facets`
// rows directly with the fixture library's own connection (`sql`, from
// `openDesktopDb()`), rather than going through `upsertWebReference` /
// `applyAiAnalysis`: those also run capture ingest and AI-write bookkeeping
// that is unrelated to the three functions under test here, and raw rows
// give the generator full, deterministic control over the palette and facet
// shapes (bare v1 hex strings, v2 `{hex, role}` swatches, …). The Rust port
// reads live from `ai_web_json` instead of a `post_facets` table (PG6); the
// fixtures carry only what both sides can express: one value per matching
// facet (see `crates/core/src/web/similar.rs`'s module docs on why
// `similar`'s shared SQL row order is not part of this contract).
//
// `q` and the domain-prefix match are deliberately NOT golden here (see
// `crates/core/src/web/sites.rs`'s module docs): the port's FTS5 search
// replaces the desktop's `LIKE` scan on purpose, the same way `repo::posts`'s
// own search is plain-Rust-tested, never golden, against the desktop.

import { createRequire } from 'module';
import path from 'path';
import { fileURLToPath } from 'url';
import type BetterSqlite3 from 'better-sqlite3';
import type { GoldenCase, GoldenSet } from './lib';
import { installDesktopShims, openDesktopDb, withDesktopClock, type DesktopDbModule } from './lib';

/** 2026-10-02T00:00:00Z: this generator's fixed "now" (none of the functions
 * under test read the clock; kept for a stable, documented fixture time). */
const NOW = Date.UTC(2026, 9, 2);

// ── A minimal post row: just enough for the functions under test ───────────

interface FixtureSite {
  id: string;
  domain: string;
  title: string;
  /** `web_palette_json`, exactly as it would be stored: bare hex strings, v2
   * `{hex, role}` swatches, or omitted for a placeholder. */
  palette?: unknown[];
  /** `post_facets` rows for this site: facet name to its values, stored with
   * their given casing (matching is case-insensitive at query time). */
  facets?: Record<string, string[]>;
  /** How many days before `NOW` this site's capture ran, so a listing's
   * recency order is unambiguous (unlike a real tie, left to SQLite's own,
   * unspecified order, which a query with no `color` filter never reaches
   * anyway — see `color-filter`'s `invalid-hex-is-no-filter` case). */
  daysAgo?: number;
}

function insertSite(sql: BetterSqlite3.Database, site: FixtureSite): void {
  const capturedAt = Math.floor(NOW / 1000) - (site.daysAgo ?? 0) * 86_400;
  sql
    .prepare(
      `INSERT INTO posts (id, platform, media_type, timestamp, author_name, web_domain,
                           web_palette_json, web_captured_at)
       VALUES (?, 'web', 'website', ?, ?, ?, ?, ?)`,
    )
    .run(
      site.id,
      new Date(capturedAt * 1000).toISOString(),
      site.title,
      site.domain,
      site.palette ? JSON.stringify(site.palette) : null,
      capturedAt,
    );
  const insertFacet = sql.prepare(
    'INSERT INTO post_facets (post_id, facet, value) VALUES (?, ?, ?)',
  );
  for (const [facet, values] of Object.entries(site.facets ?? {})) {
    for (const value of values) insertFacet.run(site.id, facet, value);
  }
}

/** A fresh desktop library seeded with `sites`, handed to `fn` as `db` (the
 * exported functions) within the desktop's pinned clock. */
function withSeededDb<T>(sites: FixtureSite[], fn: (db: DesktopDbModule) => T): T {
  const { db, sql } = openDesktopDb();
  try {
    return withDesktopClock(sql, NOW, () => {
      for (const site of sites) insertSite(sql, site);
      return fn(db);
    });
  } finally {
    db.close();
  }
}

// ── `hex-to-lab`: the pure OKLab conversion ─────────────────────────────────

const requireHere = createRequire(import.meta.url);
const METADATA_FILE = requireHere.resolve(
  path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../electron/webcap/metadata.ts'),
);

function hexToLabCases(): GoldenCase[] {
  installDesktopShims(); // `metadata.ts` → `./encode` → `../webcapture` imports `electron`
  const meta = requireHere(METADATA_FILE) as {
    hexToLab: (hex: string) => [number, number, number] | null;
  };
  const inputs = [
    '#533afd',
    '533AFD', // no hash, uppercase
    '  #0a0a0a  ', // padded
    '#000000',
    '#ffffff',
    '#ff0000',
    '#00ff00',
    '#0000ff',
    '#53afd', // too short
    '#533afd12', // alpha channel
    '#533afg', // not hex
    '', // blank
    'not a colour',
  ];
  return inputs.map((hex, i) => ({
    id: `case-${i}-${JSON.stringify(hex)}`,
    args: [hex],
    output: meta.hexToLab(hex),
  }));
}

// ── `color-filter`: `queryWebReferences({color, sort})` ────────────────────

const COLOR_SITES: FixtureSite[] = [
  {
    id: 'web_near_black',
    domain: 'near-black.test',
    title: 'Near Black',
    palette: ['#010101'],
    daysAgo: 0,
  },
  {
    id: 'web_mid_grey',
    domain: 'mid-grey.test',
    title: 'Mid Grey',
    palette: [{ hex: '#808080', role: 'surface' }],
    daysAgo: 1,
  },
  {
    id: 'web_white',
    domain: 'white.test',
    title: 'White',
    palette: [{ hex: '#ffffff', role: 'background' }],
    daysAgo: 2,
  },
  {
    // A near-white "text" swatch must not win over the darker eligible one.
    id: 'web_decoy_text',
    domain: 'decoy-text.test',
    title: 'Decoy Text',
    palette: [
      { hex: '#fefefe', role: 'text' },
      { hex: '#303030', role: 'surface' },
    ],
    daysAgo: 3,
  },
  { id: 'web_no_palette', domain: 'no-palette.test', title: 'No Palette', daysAgo: 4 },
  {
    id: 'web_empty_palette',
    domain: 'empty-palette.test',
    title: 'Empty Palette',
    palette: [],
    daysAgo: 5,
  },
];

function colorFilterCases(): GoldenCase[] {
  return withSeededDb(COLOR_SITES, (db) => {
    const run = (color: string, sort?: 'recent' | 'name' | 'color') => {
      const { posts, total } = db.queryWebReferences({ color, sort, limit: 10 });
      return { ids: posts.map((p) => p.id), total };
    };
    const cases: [string, string, ('recent' | 'name' | 'color' | undefined)?][] = [
      ['black-sort-color', '#000000', 'color'],
      ['black-sort-recent', '#000000', 'recent'],
      ['black-sort-name', '#000000', 'name'],
      ['black-no-sort', '#000000', undefined],
      ['white-sort-color', '#ffffff', 'color'],
      ['far-color-matches-none', '#00ff00', 'color'],
      ['invalid-hex-is-no-filter', 'not-a-colour', 'color'],
    ];
    return cases.map(([id, color, sort]) => ({
      id,
      args: [{ color, sort }],
      output: run(color, sort),
    }));
  });
}

// ── `facets`: `getWebFacetCounts(query)` ────────────────────────────────────

const FACET_SITES: FixtureSite[] = [
  {
    id: 'web_a',
    domain: 'a.test',
    title: 'A',
    facets: { style: ['Minimal', 'Bold'], siteType: ['portfolio'] },
  },
  { id: 'web_b', domain: 'b.test', title: 'B', facets: { style: ['minimal'] } },
  { id: 'web_c', domain: 'c.test', title: 'C', facets: { style: ['bold'], siteType: ['blog'] } },
  { id: 'web_d', domain: 'd.test', title: 'D', facets: {} }, // analyzed, no facets written
  { id: 'web_e', domain: 'e.test', title: 'E' }, // placeholder: no facets at all
];

function facetsCases(): GoldenCase[] {
  return withSeededDb(FACET_SITES, (db) => {
    const queries: [string, Parameters<typeof db.getWebFacetCounts>[0]][] = [
      ['unfiltered', null],
      ['style-bold-ignores-its-own-selection', { facets: { style: ['bold'] } }],
      ['style-minimal-or-bold', { facets: { style: ['minimal', 'bold'] } }],
      ['and-across-facets', { facets: { style: ['bold'], siteType: ['portfolio'] } }],
    ];
    return queries.map(([id, query]) => ({
      id,
      args: [query],
      output: db.getWebFacetCounts(query),
    }));
  });
}

// ── `similar`: `similarWebReferences(id, limit)` ───────────────────────────

const SIMILAR_SITES: FixtureSite[] = [
  {
    id: 'web_target',
    domain: 'target.test',
    title: 'Target',
    palette: [{ hex: '#000000', role: 'background' }],
    facets: { style: ['minimal', 'bold'], siteType: ['portfolio'], colorMood: ['dark'] },
  },
  {
    // style: {minimal,bold} ∩ {minimal} = 1, union 2 -> 3 * 1/2 = 1.5
    id: 'web_close',
    domain: 'close.test',
    title: 'Close',
    palette: [{ hex: '#050505', role: 'background' }],
    facets: { style: ['minimal'] },
  },
  {
    // siteType: {portfolio} ∩ {portfolio} = 1, union 1 -> 2 * 1/1 = 2.0: a
    // higher raw score than `web_close`'s, so the palette bonus (below) never
    // has to break a tie between these two.
    id: 'web_closer',
    domain: 'closer.test',
    title: 'Closer',
    facets: { siteType: ['Portfolio'] },
  },
  {
    // Ties `web_tie_far` on style (score 1.5) but has a much closer palette.
    id: 'web_tie_near',
    domain: 'tie-near.test',
    title: 'Tie Near',
    palette: [{ hex: '#000000', role: 'background' }],
    facets: { style: ['bold'] },
  },
  {
    id: 'web_tie_far',
    domain: 'tie-far.test',
    title: 'Tie Far',
    palette: [{ hex: '#ffffff', role: 'background' }],
    facets: { style: ['bold'] },
  },
  {
    id: 'web_unrelated',
    domain: 'unrelated.test',
    title: 'Unrelated',
    facets: { tech: ['wordpress'] },
  },
  {
    id: 'web_no_facets',
    domain: 'no-facets.test',
    title: 'No Facets',
    palette: [{ hex: '#000000', role: 'background' }],
  },
];

function similarCases(): GoldenCase[] {
  return withSeededDb(SIMILAR_SITES, (db) => {
    const project = (id: string, limit: number) =>
      db.similarWebReferences(id, limit).map((r) => ({
        id: r.post.id,
        score: r.score,
        shared: r.shared,
        sharedFacets: r.sharedFacets,
      }));
    const cases: [string, string, number][] = [
      ['target-default-limit', 'web_target', 12],
      ['target-limit-2', 'web_target', 2],
      ['target-limit-40', 'web_target', 40],
      ['no-facets-target-has-no-matches', 'web_no_facets', 12],
      ['unknown-id-has-no-matches', 'web_missing', 12],
    ];
    return cases.map(([id, target, limit]) => ({
      id,
      args: [target, limit],
      output: project(target, limit),
    }));
  });
}

// ── The sets ────────────────────────────────────────────────────────────────

const sets: GoldenSet[] = [
  {
    name: 'web/hex-to-lab',
    source: 'electron/webcap/metadata.ts#hexToLab',
    generator: 'scripts/golden/web-sites.ts',
    build: hexToLabCases,
  },
  {
    name: 'web/color-filter',
    source: 'electron/db.ts#queryWebReferences',
    generator: 'scripts/golden/web-sites.ts',
    build: colorFilterCases,
  },
  {
    name: 'web/facets',
    source: 'electron/db.ts#getWebFacetCounts',
    generator: 'scripts/golden/web-sites.ts',
    build: facetsCases,
  },
  {
    name: 'web/similar',
    source: 'electron/db.ts#similarWebReferences',
    generator: 'scripts/golden/web-sites.ts',
    build: similarCases,
  },
];

export default sets;
