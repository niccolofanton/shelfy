// One-off post-hoc tag cleanup over the real archive DB.
//
// Mirrors electron/db.js mergeTags(): posts.ai_tags is the SOURCE OF TRUTH, the
// post_tags index is rebuilt from it, and tag_cluster_membership is kept
// coherent. Adds deleteTags() (drop a tag entirely, no remap).
//
// RUNTIME: better-sqlite3 is built for Electron's ABI, so run under Electron node:
//   ELECTRON_RUN_AS_NODE=1 ./node_modules/.bin/electron scripts/tag-cleanup.cjs [--apply]
// Without --apply it's a DRY RUN (reports counts, writes nothing).

const os = require('os');
const path = require('path');
const Database = require('better-sqlite3');

const APPLY = process.argv.includes('--apply');
const DB_PATH = path.join(os.homedir(), 'Library', 'Application Support', 'Shelfy', 'shelfy.sqlite');

// --- cleanup config ---------------------------------------------------------
// Run 2: total deletion of over-applied function tags (user decision). The
// diagnostic showed 'annuncio di lavoro' had real hiring signals in only ~9 of
// 1530 captions (~98% over-application). 'bitter00000' was already removed in
// run 1; 'bitter sweet symphony' is a legit tag and is intentionally kept.
const DELETE = [];
const MERGES = [];
// Run 3: drop malformed tags where the model echoed the prompt's instruction
// labels into the tag itself (e.g. "soggetto: squalo con bikini",
// "natura post: annuncio di lavoro"). Matched by prefix, case-insensitive.
const DELETE_PREFIXES = [
  'natura post:',
  'soggetto concreto:',
  'natura:',
  'soggetto:',
  'natura/funzione',
  'funzione:',
  'tipologia:',
  'che cosa è',
];

// --- helpers (ported from electron/db.js) ----------------------------------
function parseTags(raw) {
  if (Array.isArray(raw)) return raw;
  if (typeof raw !== 'string') return [];
  try {
    const v = JSON.parse(raw);
    return Array.isArray(v) ? v : [];
  } catch {
    return [];
  }
}
function normalizeTagRows(tags) {
  const seen = new Set();
  const out = [];
  for (const t of tags) {
    if (typeof t !== 'string') continue;
    const form = t.trim();
    if (!form) continue;
    const norm = form.toLowerCase();
    if (seen.has(norm)) continue;
    seen.add(norm);
    out.push({ norm, form });
  }
  return out;
}
// Rewrite a JSON tag list: remap keys in `map` (norm->target) and drop keys in
// `drop` (Set of norms). Dedupes case-insensitively. Returns new array or null
// if unchanged.
function rewrite(rawTags, map, drop) {
  const tags = parseTags(rawTags);
  const out = [];
  const seen = new Set();
  let changed = false;
  for (const t of tags) {
    if (typeof t !== 'string') { out.push(t); continue; }
    const key = t.trim().toLowerCase();
    if (drop.has(key)) { changed = true; continue; }
    let val = t;
    if (map.has(key)) { val = map.get(key); changed = true; }
    const vk = val.trim().toLowerCase();
    if (seen.has(vk)) { changed = true; continue; }
    seen.add(vk);
    out.push(val);
  }
  return changed ? out : null;
}

// --- run --------------------------------------------------------------------
const db = new Database(DB_PATH, { fileMustExist: true });

const map = new Map();
for (const [from, to] of MERGES) map.set(from.toLowerCase(), to);
const drop = new Set(DELETE.map((d) => d.toLowerCase()));

// Expand DELETE_PREFIXES into exact tag_norm keys so the rewrite/report/cluster
// logic below works unchanged.
if (DELETE_PREFIXES && DELETE_PREFIXES.length) {
  const allNorms = db.prepare('SELECT DISTINCT tag_norm FROM post_tags').all().map((r) => r.tag_norm);
  const prefixes = DELETE_PREFIXES.map((p) => p.toLowerCase());
  for (const norm of allNorms) {
    if (prefixes.some((p) => norm.startsWith(p))) drop.add(norm);
  }
}
const allSourceKeys = [...drop, ...map.keys()];

const rows = db.prepare('SELECT id, ai_tags FROM posts WHERE ai_tags IS NOT NULL').all();
const upd = db.prepare('UPDATE posts SET ai_tags = ? WHERE id = ?');
const delTags = db.prepare('DELETE FROM post_tags WHERE post_id = ?');
const insTag = db.prepare('INSERT OR IGNORE INTO post_tags (post_id, tag_norm, tag_form) VALUES (?, ?, ?)');

let updated = 0;
const tx = db.transaction(() => {
  for (const row of rows) {
    const next = rewrite(row.ai_tags, map, drop);
    if (next) {
      upd.run(JSON.stringify(next), row.id);
      delTags.run(row.id);
      for (const { norm, form } of normalizeTagRows(next)) insTag.run(row.id, norm, form);
      updated++;
    }
  }
  // Drop defunct source tags from cluster membership (targets keep their own).
  const delMem = db.prepare('DELETE FROM tag_cluster_membership WHERE tag_norm = ?');
  for (const k of allSourceKeys) delMem.run(k);
});

// Before-counts for the report.
const countOf = (norm) =>
  db.prepare('SELECT COUNT(DISTINCT post_id) c FROM post_tags WHERE tag_norm = ?').get(norm).c;
const before = {};
for (const k of allSourceKeys) before[k] = countOf(k);

console.log(`Mode: ${APPLY ? 'APPLY (writing)' : 'DRY RUN (no writes)'}`);
console.log(`Posts with ai_tags: ${rows.length}`);

if (APPLY) {
  tx();
  console.log(`Posts rewritten: ${updated}`);
  console.log('\nResidual counts for source tags (should be 0):');
  for (const k of allSourceKeys) console.log(`  ${k.padEnd(24)} ${before[k]} -> ${countOf(k)}`);
} else {
  let touched = 0;
  for (const row of rows) if (rewrite(row.ai_tags, map, drop)) touched++;
  console.log(`Posts that WOULD be rewritten: ${touched}`);
  console.log('\nSource tag counts (pre):');
  for (const k of allSourceKeys) console.log(`  ${k.padEnd(24)} ${before[k]}`);
}

db.close();
