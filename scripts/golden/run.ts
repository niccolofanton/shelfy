// Writes (or checks) the golden fixtures in shared/golden/ from the desktop's
// TypeScript functions. See README.md.
//
//   pnpm exec tsx scripts/golden/run.ts             # rewrite every set
//   pnpm exec tsx scripts/golden/run.ts <name>...   # rewrite some sets
//   pnpm exec tsx scripts/golden/run.ts --check     # fail if a file is stale

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import extractContentTerms from './extract-content-terms';
import { render, type GoldenSet } from './lib';

const SETS: GoldenSet[] = [extractContentTerms];

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const OUT_DIR = path.join(ROOT, 'shared/golden');

const args = process.argv.slice(2);
const check = args.includes('--check');
const names = args.filter((a) => !a.startsWith('--'));
const unknown = names.filter((n) => !SETS.some((s) => s.name === n));
if (unknown.length) {
  console.error(`unknown golden set(s): ${unknown.join(', ')}`);
  process.exit(2);
}

let stale = 0;
for (const set of SETS) {
  if (names.length && !names.includes(set.name)) continue;
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
    fs.mkdirSync(OUT_DIR, { recursive: true });
    fs.writeFileSync(file, text);
    console.log(`wrote  ${path.relative(ROOT, file)} (${lines} cases)`);
  }
}
process.exit(stale ? 1 : 0);
