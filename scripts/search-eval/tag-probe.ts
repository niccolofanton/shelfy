// Refresh only the tag-only desktop aggregates of a paired eval report.
// All paths are explicit: this runner never discovers the user's library.
// pnpm exec tsx scripts/search-eval/tag-probe.ts --db=/tmp/synthetic.sqlite \
//   --baseline=crates/core/tests/search_eval/synthetic-report.json --out=/tmp/report.json
import fs from 'fs';
import Database from 'better-sqlite3';
import cases from './cases';
import { orderMetrics } from './order-metrics';
import { openDesktopDb } from '../golden/lib';

const argument = (name: string): string => {
  const value = process.argv
    .slice(2)
    .find((arg) => arg.startsWith(`--${name}=`))
    ?.slice(name.length + 3);
  if (!value) throw new Error(`--${name}=<path> is required`);
  return value;
};
const source = new Database(argument('db'), { readonly: true, fileMustExist: true });
const report = JSON.parse(fs.readFileSync(argument('baseline'), 'utf8')) as {
  results: { id: string; goldPostCount: number; metrics: Record<string, number | null> }[];
};
const { db, sql } = openDesktopDb();
try {
  // Copy only search inputs into an isolated desktop connection. The production
  // searchPostsByTags supplies the ranked ids; the oracle stays raw read-only SQL.
  for (const table of ['posts', 'tag_alias', 'post_tags']) {
    const target = new Set(
      (sql.prepare(`PRAGMA table_info(${table})`).all() as { name: string }[]).map((c) => c.name),
    );
    const columns = (source.prepare(`PRAGMA table_info(${table})`).all() as { name: string }[])
      .map((c) => c.name)
      .filter((name) => target.has(name));
    const names = columns.map((name) => `"${name}"`).join(',');
    const insert = sql.prepare(
      `INSERT INTO ${table} (${names}) VALUES (${columns.map(() => '?').join(',')})`,
    );
    sql.transaction(() => {
      for (const row of source.prepare(`SELECT ${names} FROM ${table}`).all() as Record<
        string,
        unknown
      >[])
        insert.run(...columns.map((name) => row[name]));
    })();
  }
  db.invalidateGlobalCaches();
  const global = new Map(
    (
      source
        .prepare('SELECT tag_norm AS tag, COUNT(*) AS n FROM post_tags GROUP BY tag_norm')
        .all() as { tag: string; n: number }[]
    ).map((r) => [r.tag, r.n]),
  );
  for (const c of cases) {
    const cols = ['text', 'ai_description', 'ai_keywords', 'ai_tags'];
    const where = c.goldTerms
      .map(() => `(${cols.map((name) => `${name} LIKE ?`).join(' OR ')})`)
      .join(' OR ');
    const gold = new Set(
      (
        source
          .prepare(`SELECT id FROM posts WHERE ${where}`)
          .all(...c.goldTerms.flatMap((term) => cols.map(() => `%${term}%`))) as { id: string }[]
      ).map((r) => r.id),
    );
    const baseline = report.results.find((r) => r.id === c.id);
    if (!baseline || baseline.goldPostCount !== gold.size)
      throw new Error(`${c.id}: baseline belongs to another library`);
    const counts = source
      .prepare(
        'SELECT tag_norm AS tag, COUNT(*) AS n FROM post_tags WHERE post_id IN (SELECT value FROM json_each(?)) GROUP BY tag_norm',
      )
      .all(JSON.stringify([...gold])) as { tag: string; n: number }[];
    const candidates = counts.map((r) => ({ ...r, lift: r.n / (global.get(r.tag) || r.n) }));
    const main = candidates.filter((r) => r.n >= 2 && r.lift >= 0.03);
    const goldTags = (main.length ? main : candidates.filter((r) => r.lift >= 0.25))
      .sort((a, b) => b.n * b.lift - a.n * a.lift)
      .slice(0, 12)
      .map((r) => r.tag);
    const tags = c.tagProbeOverride?.length
      ? c.tagProbeOverride.map((t) => t.toLowerCase())
      : goldTags.slice(0, 5);
    if (!tags.length) continue;
    const ranked = db.searchPostsByTags(tags, { limit: 60 }).posts.map((p) => p.id);
    for (const [name, value] of Object.entries(orderMetrics(ranked, gold)))
      baseline.metrics[`tag_${name}`] = value;
  }
  fs.writeFileSync(argument('out'), JSON.stringify(report, null, 2) + '\n');
  console.log(`updated tag-only aggregates for ${report.results.length} paired cases`);
} finally {
  sql.close();
  source.close();
}
