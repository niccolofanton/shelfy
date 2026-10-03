// Generates shared/ai/index.ts: the AI prompts and schemas of shared/ai/ as one TypeScript
// module, which is how the desktop and the scripts read them (the Rust core reads the files
// themselves through include_str!). run.ts writes it with the golden sets and `--check`
// fails while it is stale, so an edited prompt cannot reach one product and not the other.
//
// It also checks the directory: every file the manifest names exists, has LF line ends and,
// for a schema, parses as a JSON object; every prompt or schema file in shared/ai/ is named
// by the manifest. The output is formatted with the repository's prettier settings, as the
// pre-commit hook would format it, so a fresh run and the committed file compare byte for
// byte.

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import * as prettier from 'prettier';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const DIR = path.join(ROOT, 'shared/ai');
const OUTPUT = path.join(DIR, 'index.ts');

interface TaskEntry {
  system?: unknown;
  user?: unknown;
  schema?: { file?: unknown } | undefined;
}

function fail(message: string): never {
  throw new Error(`shared/ai: ${message}`);
}

/** The files the manifest names, in a stable order, after checking the directory. */
function namedFiles(manifest: { schemaVersion?: unknown; tasks?: unknown }): string[] {
  if (!Number.isInteger(manifest.schemaVersion) || (manifest.schemaVersion as number) < 1) {
    fail('manifest.json: schemaVersion must be a positive integer');
  }
  const tasks = manifest.tasks as Record<string, TaskEntry> | undefined;
  if (!tasks || typeof tasks !== 'object') fail('manifest.json: no tasks');
  const files = new Set<string>();
  for (const [name, entry] of Object.entries(tasks)) {
    for (const file of [entry.system, entry.user, entry.schema?.file]) {
      if (file === undefined) continue;
      if (
        typeof file !== 'string' ||
        !/^[a-z_]+\.(system|user)\.md$|^[a-z_]+\.schema\.json$/.test(file)
      ) {
        fail(`manifest.json: task ${name} names "${String(file)}"`);
      }
      files.add(file);
    }
    if (typeof entry.system !== 'string') fail(`manifest.json: task ${name} has no system prompt`);
  }
  for (const file of files) {
    const full = path.join(DIR, file);
    if (!fs.existsSync(full)) fail(`${file} is named by the manifest but does not exist`);
    const text = fs.readFileSync(full, 'utf8');
    if (text.includes('\r')) fail(`${file} has CR line ends; use LF`);
    if (file.endsWith('.json')) {
      const value = JSON.parse(text) as unknown;
      if (!value || typeof value !== 'object' || Array.isArray(value)) {
        fail(`${file} is not a JSON object`);
      }
    }
  }
  for (const entry of fs.readdirSync(DIR, { withFileTypes: true })) {
    if (!entry.isFile() || !/\.(md|json)$/.test(entry.name)) continue;
    if (entry.name === 'manifest.json' || entry.name === 'README.md') continue;
    if (!files.has(entry.name)) fail(`${entry.name} is not named by manifest.json`);
  }
  return [...files].sort();
}

/** The text of shared/ai/index.ts, formatted. */
export async function renderSharedAiIndex(): Promise<string> {
  const manifestText = fs.readFileSync(path.join(DIR, 'manifest.json'), 'utf8');
  const manifest = JSON.parse(manifestText) as { schemaVersion?: unknown; tasks?: unknown };
  const files = namedFiles(manifest);
  const entries = files.map(
    (file) =>
      `${JSON.stringify(file)}: ${JSON.stringify(fs.readFileSync(path.join(DIR, file), 'utf8'))},`,
  );
  const source = [
    '// Generated from shared/ai/manifest.json and the files it names by',
    '// `pnpm exec tsx scripts/golden/run.ts`, which fails with `--check` while this file is',
    '// stale. Edit those files, never this one: see shared/ai/README.md.',
    '',
    '/** shared/ai/manifest.json. */',
    `export const MANIFEST = ${JSON.stringify(manifest)} as const;`,
    '',
    '/** The text of each file the manifest names, by file name. */',
    'export const FILES: Readonly<Record<string, string>> = {',
    ...entries,
    '};',
    '',
  ].join('\n');
  const config = (await prettier.resolveConfig(OUTPUT)) ?? {};
  return prettier.format(source, { ...config, filepath: OUTPUT });
}

/** A file generated from sources, written and checked by run.ts like a golden set. */
export interface GeneratedFile {
  name: string;
  file: string;
  render(): Promise<string>;
}

export const sharedAiIndex: GeneratedFile = {
  name: 'shared-ai-index',
  file: OUTPUT,
  render: renderSharedAiIndex,
};
