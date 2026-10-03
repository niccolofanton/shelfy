#!/usr/bin/env node
// scripts/live/subset-library.mjs — X2 (owner decision E6): a seeded subset
// of a desktop library, for the live host's mock account.
//
// E6 seeds the mock account with about 500 of the owner's posts, so lanes
// can sign in to it and run end-to-end web checks without touching the
// owner's own account or library (P1 lane rule 8, "no owner session in
// automation" — the mock account is the one exception, by E6). This tool
// builds that subset *before* `shelfy-migrate` ever runs: it never talks to
// a server, it only produces a smaller desktop-schema SQLite file.
//
// Usage:
//   node subset-library.mjs --db <desktop shelfy.sqlite> --out <dir> \
//     --count 500 --seed <n> [--platform-mix ig:0.6,x:0.35,web:0.05]
//
// `--db` is read-only and immutable (`file:…?mode=ro&immutable=1`), and this
// tool never writes next to it. A `-wal` or `-journal` file beside it means a
// connection has it open, or had it and did not close cleanly: reading it
// `immutable` straight away could silently miss committed WAL content, the
// trap `crates/migrate/src/snapshot.rs` is built around. This tool resolves
// it the same way `snapshot.rs` (and `shelfy-migrate run`) do: a `-wal` or
// `-journal` file means it first copies `--db` with SQLite's online backup
// API into a scratch file elsewhere (never beside `--db`) — one consistent
// snapshot of whatever is committed, safe with or without another live
// reader or writer — and reads *that* immutably instead; the scratch file is
// removed once this tool exits. Without one, `--db` is read immutably in
// place, no copy needed.
//
// `--out <dir>` gets one file, `shelfy.sqlite`, replaced if it exists. The
// schema (every `CREATE TABLE`/`CREATE INDEX`, whatever desktop schema
// version `--db` happens to be) is copied verbatim from the source's own
// `sqlite_master` via `ATTACH DATABASE … AS src`, so this tool never goes
// stale against `electron/db.ts`'s own schema. `PRAGMA user_version` is
// copied too.
//
// **The sample.** `posts` keeps a random, seed-deterministic subset of
// `--count` rows (same `--db`, `--count` and `--seed` → the same post ids,
// every run, on any machine). Without `--platform-mix`, the sample is drawn
// uniformly from the whole library. With it, each listed platform (`ig` →
// `instagram`, `x` → `twitter`, `pin` → `pinterest`, `web`, `manual`; the
// full desktop name also works) gets its share of `--count`, apportioned by
// the largest-remainder method and clamped to what that platform actually
// has; any shortfall is handed to the other listed platforms that still
// have spare posts. A platform left out of `--platform-mix` contributes
// none of the sample.
//
// **Cascades.** Every desktop table that references `posts(id)` —
// `post_media`, `post_collections`, `post_tags`, `post_entities`,
// `post_facets`, `web_snapshots`, `downloads` — declares `ON DELETE
// CASCADE` (`electron/db.ts`'s `SCHEMA`), so one `DELETE FROM posts WHERE id
// NOT IN (kept)` under `PRAGMA foreign_keys = ON` drops a dropped post's
// rows everywhere, in one step. Two things cascades do not reach, handled
// explicitly:
//
//   - `collections` has no FK pointing *at* `post_collections`, so a
//     collection emptied by the cascade (or already empty in the source)
//     is deleted by hand afterwards — "keep the collections that still have
//     members".
//   - `jobs.post_id` carries no FK at all (`electron/db.ts`: "Not tied to
//     posts(id) by FK: web jobs are keyed by URL and may outlive a
//     placeholder"), so its rows naming a dropped post are deleted by hand
//     too; a `NULL` `post_id` (a non-post job) is left alone.
//
// `tag_alias`, `tag_cluster` and `tag_cluster_membership` are small,
// post-independent vocabulary tables (canonical tag names and clusters,
// not per-post rows): they are copied wholesale, untouched by the sample.
//
// Before anything is reported, the copy is checked with `PRAGMA
// foreign_key_check` (must be empty) and `PRAGMA quick_check` (must be
// `ok`), then `VACUUM`ed.
//
// **Media.** This tool never touches media files: `posts.thumbnail_path`,
// `post_media.local_path` and the rest keep whatever path the desktop wrote,
// untouched by the copy or the subsetting. `shelfy-migrate run --db <copy>
// --media-root <userData dir>` resolves exactly those paths itself
// (`crates/migrate/src/files.rs`'s `FileRefs`, built only from rows it
// reads); it never scans `--media-root` to decide what to upload, so a
// smaller `posts` table alone is what makes it upload less.
//
// **Output.** Aggregate counts only, on stdout: no caption, URL, key or
// username ever appears (lane rule 9).

import { existsSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync, backup } from 'node:sqlite';
import { parseArgs } from 'node:util';

import { log, usageError } from './lib.mjs';

/** `--platform-mix`'s short codes, and the desktop's own platform names
 * (`posts.platform`, verbatim: "instagram, twitter, pinterest, web, manual",
 * `crates/core/src/legacy/catalog.rs`). */
const PLATFORM_ALIASES = {
  ig: 'instagram',
  instagram: 'instagram',
  x: 'twitter',
  twitter: 'twitter',
  pin: 'pinterest',
  pinterest: 'pinterest',
  web: 'web',
  manual: 'manual',
};

/** Desktop tables that reference `posts(id) ON DELETE CASCADE`
 * (`electron/db.ts`'s `SCHEMA`): the cascade handles these once `posts`
 * itself is trimmed. Listed only for the report; the DELETE itself needs no
 * table list, the cascade follows the schema's own FKs. */
const CASCADED_ON_POSTS = [
  'post_media',
  'post_collections',
  'post_tags',
  'post_entities',
  'post_facets',
  'web_snapshots',
  'downloads',
];

/** Post-independent vocabulary tables, copied wholesale. */
const VOCAB_TABLES = ['tag_alias', 'tag_cluster', 'tag_cluster_membership'];

function parseOptions(argv) {
  const { values } = parseArgs({
    args: argv,
    options: {
      db: { type: 'string' },
      out: { type: 'string' },
      count: { type: 'string' },
      seed: { type: 'string' },
      'platform-mix': { type: 'string' },
    },
  });
  if (!values.db || !values.out || !values.count || values.seed === undefined) {
    usageError(
      'subset-library.mjs --db <desktop shelfy.sqlite> --out <dir> --count <n> --seed <n> ' +
        '[--platform-mix ig:0.6,x:0.35,web:0.05]',
    );
  }
  const count = Number.parseInt(values.count, 10);
  if (!Number.isInteger(count) || count <= 0) {
    usageError(`--count must be a positive integer, got ${values.count}`);
  }
  const seed = Number.parseInt(values.seed, 10);
  if (!Number.isInteger(seed)) {
    usageError(`--seed must be an integer, got ${values.seed}`);
  }
  return {
    db: path.resolve(values.db),
    out: path.resolve(values.out),
    count,
    seed,
    mix: parsePlatformMix(values['platform-mix']),
  };
}

function parsePlatformMix(raw) {
  if (!raw) return null;
  const mix = new Map();
  for (const part of raw.split(',')) {
    const trimmed = part.trim();
    if (!trimmed) continue;
    const at = trimmed.indexOf(':');
    if (at === -1) {
      usageError(`--platform-mix entries look like "ig:0.6", not ${JSON.stringify(trimmed)}`);
    }
    const rawPlatform = trimmed.slice(0, at).trim().toLowerCase();
    const rawShare = trimmed.slice(at + 1).trim();
    const platform = PLATFORM_ALIASES[rawPlatform];
    if (!platform) {
      usageError(
        `--platform-mix: unknown platform ${JSON.stringify(rawPlatform)} ` +
          '(try ig, x, pin, web or manual)',
      );
    }
    const share = Number(rawShare);
    if (!(share > 0 && share <= 1)) {
      usageError(`--platform-mix: ${rawPlatform}'s share must be in (0, 1], got ${rawShare}`);
    }
    mix.set(platform, share);
  }
  const total = [...mix.values()].reduce((a, b) => a + b, 0);
  if (total > 1.000_001) {
    usageError(`--platform-mix: shares add up to ${total}, more than 1`);
  }
  return mix.size > 0 ? mix : null;
}

/** A `-wal` or `-journal` file next to `dbPath`: a connection has the
 * library open, or had it and did not close cleanly (mirrors
 * `crates/migrate/src/snapshot.rs::is_live`). Opening such a file
 * `immutable` could silently read stale data: [`resolveSafeSource`] copies
 * it first instead of opening it directly. */
function isLive(dbPath) {
  return ['-wal', '-journal'].some((suffix) => existsSync(dbPath + suffix));
}

/** The path to read `dbPath` from, and a cleanup to call afterwards
 * (removes the scratch copy, if one was made; a no-op otherwise).
 *
 * When `dbPath` is live ([`isLive`]), copies it with SQLite's online backup
 * API into a scratch file in the OS temp directory — one consistent
 * snapshot of whatever is committed, safe whether or not another reader or
 * writer holds `dbPath` open right now — and returns that path instead, so
 * every later read (the sample, and the schema/data copy into `--out`) can
 * safely open it `immutable`. Never writes next to `dbPath` itself. */
async function resolveSafeSource(dbPath) {
  if (!isLive(dbPath)) {
    return { path: dbPath, cleanup: () => {} };
  }
  log(`${dbPath} has a -wal or -journal file next to it: copying a consistent snapshot first`);
  const scratchDir = mkdtempSync(path.join(os.tmpdir(), 'shelfy-subset-'));
  const scratchPath = path.join(scratchDir, 'source.sqlite');
  // Not immutable: the source may genuinely be live, and the backup API
  // (unlike a plain read) is always safe and consistent either way.
  const live = new DatabaseSync(`file:${escapeSqliteUri(dbPath)}?mode=ro`, { readOnly: true });
  try {
    await backup(live, scratchPath);
  } finally {
    live.close();
  }
  return {
    path: scratchPath,
    cleanup: () => rmSync(scratchDir, { recursive: true, force: true }),
  };
}

/** `raw` safe to interpolate into a `file:` URI's path component. */
function escapeSqliteUri(raw) {
  return raw.replace(/'/g, "''").replace(/\?/g, '%3f').replace(/#/g, '%23');
}

/** A small, fast, deterministic PRNG (mulberry32): the same seed always
 * draws the same sequence, on any machine. */
function makeRng(seed) {
  let a = seed >>> 0;
  return function next() {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t = (t + Math.imul(t ^ (t >>> 7), t | 61)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A new array, `items` in a deterministic shuffle (Fisher-Yates on `next`). */
function shuffled(items, next) {
  const out = items.slice();
  for (let i = out.length - 1; i > 0; i -= 1) {
    const j = Math.floor(next() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

/** Integer shares of `count` for each key of `shares` (a `Map<string,
 * number>` of 0–1 fractions), by the largest-remainder method: floor each,
 * then give the leftover units to the largest fractional remainders first,
 * ties broken by key so the result is deterministic. */
function apportion(shares, count) {
  const keys = [...shares.keys()].sort();
  const raw = keys.map((k) => shares.get(k) * count);
  const base = raw.map(Math.floor);
  let used = base.reduce((a, b) => a + b, 0);
  const byRemainder = keys
    .map((k, i) => ({ k, i, frac: raw[i] - base[i] }))
    .sort((a, b) => b.frac - a.frac || a.k.localeCompare(b.k));
  let next = 0;
  while (used < count && next < byRemainder.length) {
    base[byRemainder[next].i] += 1;
    used += 1;
    next += 1;
  }
  return new Map(keys.map((k, i) => [k, base[i]]));
}

/** The post ids to keep: `count` of them, deterministic for `seed`, drawn
 * uniformly or (with `mix`) apportioned across platforms. Returns the ids
 * and, per platform, how many were requested/available/kept — the report's
 * numbers, never the ids themselves. */
function pickSample(src, { count, seed, mix }) {
  const rows = src.prepare('SELECT id, platform FROM posts ORDER BY id').all();
  const next = makeRng(seed);
  const byPlatform = new Map();
  for (const row of rows) {
    if (!byPlatform.has(row.platform)) byPlatform.set(row.platform, []);
    byPlatform.get(row.platform).push(row.id);
  }
  const sourceCounts = new Map([...byPlatform].map(([p, ids]) => [p, ids.length]));

  if (!mix) {
    const pool = shuffled(
      rows.map((r) => r.id),
      next,
    );
    const keep = new Set(pool.slice(0, Math.min(count, pool.length)));
    return { keep, sourceCounts, keptCounts: tallyByPlatform(rows, keep) };
  }

  const buckets = new Map();
  for (const [platform, ids] of byPlatform) {
    if (mix.has(platform)) buckets.set(platform, shuffled(ids, next));
  }
  const target = apportion(mix, count);
  const keep = new Set();
  const taken = new Map();
  for (const [platform, wanted] of target) {
    const available = buckets.get(platform) ?? [];
    const take = Math.min(wanted, available.length);
    for (let i = 0; i < take; i += 1) keep.add(available[i]);
    taken.set(platform, take);
  }
  // Redistribute any shortfall to the other listed platforms' spare posts,
  // round-robin over platforms that still have some, by key order.
  let short = [...target].reduce((sum, [p, wanted]) => sum + (wanted - taken.get(p)), 0);
  if (short > 0) {
    const spare = [...buckets]
      .map(([platform, ids]) => ({ platform, rest: ids.slice(taken.get(platform) ?? 0) }))
      .filter((s) => s.rest.length > 0);
    let i = 0;
    while (short > 0 && spare.some((s) => s.rest.length > 0)) {
      const bucket = spare[i % spare.length];
      if (bucket.rest.length > 0) {
        keep.add(bucket.rest.shift());
        taken.set(bucket.platform, (taken.get(bucket.platform) ?? 0) + 1);
        short -= 1;
      }
      i += 1;
    }
  }
  return {
    keep,
    sourceCounts,
    keptCounts: taken,
    requestedCounts: target,
  };
}

function tallyByPlatform(rows, keep) {
  const counts = new Map();
  for (const row of rows) {
    if (keep.has(row.id)) counts.set(row.platform, (counts.get(row.platform) ?? 0) + 1);
  }
  return counts;
}

/** The user tables of `db` (every `sqlite_master` row of type `table`, in
 * `sqlite_master`'s own order, `sqlite_*` bookkeeping excluded). */
function userTables(db, schema = 'main') {
  return db
    .prepare(
      `SELECT name FROM ${schema}.sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'`,
    )
    .all()
    .map((r) => r.name);
}

/** Creates `outPath` (replacing it) with the exact schema of `srcPath`
 * (every table, index, trigger and view, verbatim from its own
 * `sqlite_master`) and every row copied, through one read-only, immutable
 * `ATTACH`. Returns the open destination handle and the table names copied. */
function copyDatabase(srcPath, outPath) {
  rmSync(outPath, { force: true });
  rmSync(`${outPath}-wal`, { force: true });
  rmSync(`${outPath}-shm`, { force: true });
  const dest = new DatabaseSync(outPath);
  dest.exec(`ATTACH DATABASE 'file:${escapeSqliteUri(srcPath)}?mode=ro&immutable=1' AS src`);
  try {
    const objects = dest
      .prepare(
        'SELECT sql FROM src.sqlite_master WHERE sql IS NOT NULL AND type IN ' +
          "('table', 'index', 'trigger', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' " +
          "ORDER BY (type = 'table') DESC",
      )
      .all();
    for (const { sql } of objects) dest.exec(sql);
    const tables = userTables(dest, 'src');
    dest.exec('PRAGMA foreign_keys = OFF'); // bulk copy only; turned on for the subsetting DELETE
    for (const table of tables) {
      dest.exec(`INSERT INTO main."${table}" SELECT * FROM src."${table}"`);
    }
    const userVersion = dest.prepare('PRAGMA src.user_version').get().user_version;
    dest.exec(`PRAGMA user_version = ${Number(userVersion)}`);
    return { dest, tables };
  } finally {
    dest.exec('DETACH DATABASE src');
  }
}

/** Drops every post not in `keep` (cascading to its dependent rows per the
 * schema's own `ON DELETE CASCADE`), then the two things the cascade does
 * not reach: `jobs` rows of a dropped post (no FK at all) and `collections`
 * left with no member. Returns what was dropped, for the report. */
function applySubset(dest, keep) {
  dest.exec('DROP TABLE IF EXISTS _subset_keep');
  dest.exec('CREATE TEMP TABLE _subset_keep (id TEXT PRIMARY KEY)');
  const insertKeep = dest.prepare('INSERT INTO _subset_keep (id) VALUES (?)');
  for (const id of keep) insertKeep.run(id);

  dest.exec('PRAGMA foreign_keys = ON');
  const before = dest.prepare('SELECT count(*) AS n FROM posts').get().n;
  dest.exec('DELETE FROM posts WHERE id NOT IN (SELECT id FROM _subset_keep)');
  const afterPosts = dest.prepare('SELECT count(*) AS n FROM posts').get().n;

  const hasJobs = userTables(dest).includes('jobs');
  let droppedJobs = 0;
  if (hasJobs) {
    droppedJobs = dest
      .prepare(
        'DELETE FROM jobs WHERE post_id IS NOT NULL AND post_id NOT IN (SELECT id FROM _subset_keep) RETURNING 1',
      )
      .all().length;
  }

  const collectionsBefore = dest.prepare('SELECT count(*) AS n FROM collections').get().n;
  dest.exec(
    'DELETE FROM collections WHERE id NOT IN (SELECT DISTINCT collection_id FROM post_collections)',
  );
  const collectionsAfter = dest.prepare('SELECT count(*) AS n FROM collections').get().n;

  dest.exec('DROP TABLE _subset_keep');

  return {
    postsBefore: before,
    postsAfter: afterPosts,
    droppedJobs,
    collectionsBefore,
    collectionsAfter,
  };
}

/** `PRAGMA foreign_key_check` (must be empty) and `quick_check` (must be
 * `ok`): the copy is schema-consistent before it is reported or used. */
function verifyIntegrity(dest) {
  const violations = dest.prepare('PRAGMA foreign_key_check').all();
  if (violations.length > 0) {
    throw new Error(
      `the subset has ${violations.length} foreign-key violation(s) (first: ${JSON.stringify(violations[0])}); this is a bug in subset-library.mjs, not the source library`,
    );
  }
  const check = dest.prepare('PRAGMA quick_check').all();
  const ok = check.length === 1 && check[0].quick_check === 'ok';
  if (!ok) {
    throw new Error(`the subset failed its integrity check: ${JSON.stringify(check)}`);
  }
}

function countRows(dest, table) {
  return dest.prepare(`SELECT count(*) AS n FROM "${table}"`).get().n;
}

function formatPlatformCounts(counts) {
  return [...counts]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([platform, n]) => `${platform} ${n}`)
    .join(', ');
}

async function main() {
  const options = parseOptions(process.argv.slice(2));
  const safeSource = await resolveSafeSource(options.db);
  try {
    const src = new DatabaseSync(`file:${escapeSqliteUri(safeSource.path)}?mode=ro&immutable=1`, {
      readOnly: true,
    });
    let sample;
    try {
      sample = pickSample(src, options);
    } finally {
      src.close();
    }

    mkdirSync(options.out, { recursive: true });
    const outPath = path.join(options.out, 'shelfy.sqlite');
    const { dest, tables } = copyDatabase(safeSource.path, outPath);
    try {
      const subset = applySubset(dest, sample.keep);
      verifyIntegrity(dest);
      dest.exec('VACUUM');

      const report = {
        source: {
          posts: subset.postsBefore,
          byPlatform: Object.fromEntries(sample.sourceCounts),
          collections: subset.collectionsBefore,
        },
        kept: {
          posts: subset.postsAfter,
          byPlatform: Object.fromEntries(sample.keptCounts),
          collections: subset.collectionsAfter,
        },
        dependentTables: Object.fromEntries(
          [...CASCADED_ON_POSTS, ...VOCAB_TABLES]
            .filter((t) => tables.includes(t))
            .map((t) => [t, countRows(dest, t)]),
        ),
        droppedJobRows: subset.droppedJobs,
      };

      console.log(`subset of ${options.db}`);
      console.log(
        `  source: ${report.source.posts} posts (${formatPlatformCounts(sample.sourceCounts)}), ` +
          `${report.source.collections} collections`,
      );
      console.log(
        `  kept:   ${report.kept.posts} posts (${formatPlatformCounts(sample.keptCounts)}), ` +
          `${report.kept.collections} collections (${report.source.collections - report.kept.collections} dropped, had no member left)`,
      );
      if (sample.requestedCounts) {
        const shortfalls = [...sample.requestedCounts]
          .filter(([p, wanted]) => (sample.keptCounts.get(p) ?? 0) < wanted)
          .map(
            ([p, wanted]) =>
              `${p} (wanted ${wanted}, only ${sample.keptCounts.get(p) ?? 0} available)`,
          );
        if (shortfalls.length > 0) {
          console.log(
            `  note: --platform-mix could not be matched exactly: ${shortfalls.join(', ')}`,
          );
        }
      }
      console.log(
        `  dependent rows kept: ${Object.entries(report.dependentTables)
          .map(([t, n]) => `${t} ${n}`)
          .join(', ')}`,
      );
      if (report.droppedJobRows > 0) {
        console.log(`  jobs rows dropped (no FK, matched by hand): ${report.droppedJobRows}`);
      }
      console.log(`  wrote ${outPath}`);
    } finally {
      dest.close();
    }
  } finally {
    safeSource.cleanup();
  }
}

main().catch((err) => {
  console.error(`error: ${err.message}`);
  process.exitCode = 1;
});
