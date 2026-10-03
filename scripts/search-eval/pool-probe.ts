// Refresh only the chat-pool desktop aggregates of a paired eval report.
// All paths are explicit: this runner never discovers the user's library.
// pnpm exec tsx scripts/search-eval/pool-probe.ts --db=/tmp/synthetic.sqlite \
//   --baseline=crates/core/tests/search_eval/synthetic-report.json --out=/tmp/report.json
import fs from 'fs';
import Database from 'better-sqlite3';
import cases from './cases';
import { openChatDesktop } from '../golden/ai-chat';

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
const { db, sql, analyzer } = openChatDesktop();
try {
  // Copy only search inputs into an isolated desktop connection. The production
  // analyzer supplies the real offline pools; the oracle stays raw read-only SQL.
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
    const goldCounts = new Map(counts.map((r) => [r.tag, r.n]));
    // Match chatSearch/getBroadVocab: general-tier first, legacy fallback.
    const general = db.getTagStats({ limit: 150, tier: 'general' });
    const broad = (general.length ? general : db.getTagStats({ limit: 150 })).map((t) =>
      t.tag.toLowerCase(),
    );
    const specific = analyzer.retrieveSpecificTags(c.query, new Set(broad));
    const keywords = analyzer.retrieveKeywords(c.query, { limit: 12 });
    const fraction = (n: number, total: number) => (total ? n / total : 0);
    baseline.metrics.poolRelevance = fraction(
      specific.filter(
        (t) =>
          (goldCounts.get(t.toLowerCase()) || 0) >= 1 &&
          (goldCounts.get(t.toLowerCase()) || 0) / (global.get(t.toLowerCase()) || 1) >= 0.15,
      ).length,
      specific.length,
    );
    baseline.metrics.poolNoise = fraction(
      specific.filter((t) => !goldCounts.has(t.toLowerCase())).length,
      specific.length,
    );
    baseline.metrics.keywordRelevance = fraction(
      keywords.filter((k) =>
        c.goldTerms.some((t) => k.toLowerCase().includes(t) || t.includes(k.toLowerCase())),
      ).length,
      keywords.length,
    );
  }
  fs.writeFileSync(argument('out'), JSON.stringify(report, null, 2) + '\n');
  console.log(`updated pool aggregates for ${report.results.length} paired cases`);
} finally {
  sql.close();
  source.close();
}
