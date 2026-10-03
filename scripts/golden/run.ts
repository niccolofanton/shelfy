// Writes (or checks) the golden fixtures in shared/golden/ from the desktop's
// TypeScript functions, and the generated module shared/ai/index.ts (the AI
// prompts and schemas as the desktop reads them). See README.md.
//
//   pnpm exec tsx scripts/golden/run.ts             # rewrite every set and generated file
//   pnpm exec tsx scripts/golden/run.ts <name>...   # rewrite some (`merge` = every merge/ set)
//   pnpm exec tsx scripts/golden/run.ts --check     # fail if a file is stale or orphaned

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import edits from './edits';
import tagSearch from './tag-search';
import importSets from './import';
import extractContentTerms from './extract-content-terms';
import hosts from './hosts';
import mergeSets from './merge';
import sanitize from './sanitize';
import webSiteSets from './web-sites';
import { render, type GoldenSet } from './lib';
import { sharedAiIndex, type GeneratedFile } from './shared-ai';

const GENERATED: GeneratedFile[] = [sharedAiIndex];

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const OUT_DIR = path.join(ROOT, 'shared/golden');

const args = process.argv.slice(2);
const check = args.includes('--check');
const names = args.filter((a) => !a.startsWith('--'));
const matches = (name: string): boolean =>
  !names.length || names.some((n) => name === n || name.startsWith(`${n}/`));

let stale = 0;

// Generated files first: a set that reads shared/ai/ loads it through
// shared/ai/index.ts, so such sets are imported only once it is current.
for (const generated of GENERATED) {
  if (!matches(generated.name)) continue;
  const text = await generated.render();
  const relative = path.relative(ROOT, generated.file);
  if (check) {
    const current = fs.existsSync(generated.file) ? fs.readFileSync(generated.file, 'utf8') : null;
    if (current === text) {
      console.log(`ok     ${relative}`);
    } else {
      stale++;
      console.error(`stale  ${relative}: regenerate it with scripts/golden/run.ts`);
    }
  } else {
    fs.writeFileSync(generated.file, text);
    console.log(`wrote  ${relative}`);
  }
}

const { default: aiCatalogSets } = await import('./ai-catalog');
const { default: aiClusterSets } = await import('./ai-clusters');
const { default: aiAliasSets } = await import('./ai-aliases');
const { default: aiChatSets } = await import('./ai-chat');
const { default: taxonomyPromptSets } = await import('./ai-taxonomy-prompts');
const { default: aiTagSets } = await import('./ai-tags');
const { default: webDesignSets } = await import('./ai-web-design');
const { default: webSanitize } = await import('./ai-web-sanitize');
const SETS: GoldenSet[] = [
  webSanitize,
  ...webDesignSets,
  ...taxonomyPromptSets,
  ...aiClusterSets,
  ...aiAliasSets,
  ...importSets,
  ...aiTagSets,
  extractContentTerms,
  edits,
  tagSearch,
  ...mergeSets,
  sanitize,
  hosts,
  ...webSiteSets,
  ...aiCatalogSets,
  ...aiChatSets,
];

const known = [...SETS.map((s) => s.name), ...GENERATED.map((g) => g.name)];
const unknown = names.filter((n) => !known.some((k) => k === n || k.startsWith(`${n}/`)));
if (unknown.length) {
  console.error(`unknown golden set(s): ${unknown.join(', ')}`);
  process.exit(2);
}

for (const set of SETS) {
  if (!matches(set.name)) continue;
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
