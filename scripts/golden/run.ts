// Writes (or checks) the golden fixtures in shared/golden/ from the desktop's
// TypeScript functions. See README.md.
//
//   pnpm exec tsx scripts/golden/run.ts             # rewrite every set
//   pnpm exec tsx scripts/golden/run.ts <name>...   # rewrite some sets (`merge` = every merge/ set)
//   pnpm exec tsx scripts/golden/run.ts --check     # fail if a file is stale or orphaned

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import edits from './edits';
import extractContentTerms from './extract-content-terms';
import hosts from './hosts';
import mergeSets from './merge';
import sanitize from './sanitize';
import { render, type GoldenSet } from './lib';

const SETS: GoldenSet[] = [extractContentTerms, edits, ...mergeSets, sanitize, hosts];

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const OUT_DIR = path.join(ROOT, 'shared/golden');

const args = process.argv.slice(2);
const check = args.includes('--check');
const names = args.filter((a) => !a.startsWith('--'));
const selected = (set: GoldenSet): boolean =>
  !names.length || names.some((n) => set.name === n || set.name.startsWith(`${n}/`));
const unknown = names.filter((n) => !SETS.some((s) => s.name === n || s.name.startsWith(`${n}/`)));
if (unknown.length) {
  console.error(`unknown golden set(s): ${unknown.join(', ')}`);
  process.exit(2);
}

let stale = 0;
for (const set of SETS) {
  if (!selected(set)) continue;
  const file = path.join(OUT_DIR, `${set.name}.jsonl`);
  const text = render(set);
  const lines = text.split('\n').length - 2;
  if (check) {
    const current = fs.existsSync(file) ? fs.readFileSync(file, 'utf8') : null;
    if (current === text) {
      console.log(`ok     ${path.relative(ROOT, file)} (${lines} cases)`);
    } else {
      stale++;
      console.error(`stale  ${path.relative(ROOT, file)}: regenerate it and update the Rust port`);
    }
  } else {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
    console.log(`wrote  ${path.relative(ROOT, file)} (${lines} cases)`);
  }
}

// A file that no set writes any more would still be checked by the Rust tests.
if (check && !names.length) {
  const expected = new Set(SETS.map((s) => path.join(OUT_DIR, `${s.name}.jsonl`)));
  for (const file of goldenFiles(OUT_DIR)) {
    if (!expected.has(file)) {
      stale++;
      console.error(`stale  ${path.relative(ROOT, file)}: no golden set writes it; delete it`);
    }
  }
}
process.exit(stale ? 1 : 0);

/** Every `.jsonl` file under `dir`, recursively. */
function goldenFiles(dir: string): string[] {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) return goldenFiles(full);
    return entry.name.endsWith('.jsonl') ? [full] : [];
  });
}
