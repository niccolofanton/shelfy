// Golden-fixture plumbing shared by the generators in this directory.
//
// A golden set runs one desktop function on fixed inputs and records each
// output. The file format is JSON Lines, so every case is one line and the Rust
// side can read each `output` back as the exact bytes `JSON.stringify` wrote:
//
//   {"golden":"<name>","source":"<file>#<export>","generator":"<path>","format":1}
//   {"id":"<case id>","args":[...],"output":...}
//   ...
//
// See README.md for the workflow.

import { createRequire } from 'module';
import path from 'path';
import { fileURLToPath } from 'url';
import type BetterSqlite3 from 'better-sqlite3';

export interface GoldenCase {
  /** Stable, unique id; it names the case in Rust test failures. */
  id: string;
  /** Arguments of the call, as JSON. */
  args: unknown[];
  /** What the desktop function returned, as JSON. */
  output: unknown;
}

export interface GoldenSet {
  /** File name under shared/golden/, without `.jsonl`; `<dir>/<name>` for a file in a subdirectory. */
  name: string;
  /** The desktop function: `<repo path>#<export name>`. */
  source: string;
  /** This generator's path, relative to the repo root. */
  generator: string;
  /** Runs the desktop function on every input. */
  build(): GoldenCase[];
}

/** The JSONL text of a set. Throws on duplicate case ids. */
export function render(set: GoldenSet): string {
  const cases = set.build();
  const seen = new Set<string>();
  for (const c of cases) {
    if (seen.has(c.id)) throw new Error(`${set.name}: duplicate case id "${c.id}"`);
    seen.add(c.id);
  }
  const header = { golden: set.name, source: set.source, generator: set.generator, format: 1 };
  const lines = [header, ...cases.map((c) => ({ id: c.id, args: c.args, output: c.output }))];
  return lines.map((l) => JSON.stringify(l)).join('\n') + '\n';
}

// ── The desktop database ─────────────────────────────────────────────────────
//
// Generators that exercise `electron/db.ts` writes (bulkUpsert, …) need its
// module-level connection, which only `initialize()` opens, on the file
// `<userData>/shelfy.sqlite`. `openDesktopDb()` loads a private copy of the
// module whose `electron` import answers `app.getPath()` and whose
// `better-sqlite3` opens `:memory:` whatever the path, then initializes it: a
// fresh, empty desktop library on the real schema and migrations, with the
// module code unchanged.
//
// It runs under plain Node, where `pnpm install` builds better-sqlite3 for
// Node's ABI (CI's `test` job included). If the module was rebuilt for
// Electron's ABI, run the generators through Electron's Node instead:
//
//   ELECTRON_RUN_AS_NODE=1 NODE_OPTIONS=--import=tsx electron scripts/golden/run.ts

/** `electron/db.ts`, as loaded by `openDesktopDb()`. */
export type DesktopDbModule = typeof import('../../electron/db');

/** A private copy of `electron/db.ts` and its in-memory connection. */
export interface DesktopDb {
  db: DesktopDbModule;
  sql: BetterSqlite3.Database;
}

const requireHere = createRequire(import.meta.url);
const DESKTOP_DB_FILE = requireHere.resolve(
  path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../electron/db.ts'),
);
let desktopShimsInstalled = false;

/** Opens a fresh desktop library in memory. Each call returns a new one. */
export function openDesktopDb(): DesktopDb {
  installDesktopShims();
  // A new copy of the module: earlier imports and earlier calls keep theirs.
  delete requireHere.cache[DESKTOP_DB_FILE];
  const db = requireHere(DESKTOP_DB_FILE) as DesktopDbModule;
  const sql = db.initialize();
  if (!sql.memory) throw new Error('openDesktopDb: the desktop library is not in memory');
  return { db, sql };
}

/**
 * Runs `fn` with the desktop's clocks pinned to `nowMs`: JavaScript's
 * `Date.now()` and SQLite's `strftime('%Y-%m-%dT%H:%M:%fZ', 'now')`, the two
 * that the write paths read. Any other two-argument `strftime` call throws, and
 * so does any call once `fn` has returned (SQLite keeps the override), so a new
 * use of the SQLite clock cannot reach a golden file unpinned.
 */
export function withDesktopClock<T>(sql: BetterSqlite3.Database, nowMs: number, fn: () => T): T {
  const iso = new Date(nowMs).toISOString();
  sql.function('strftime', (format: unknown, value: unknown) => {
    if (format === '%Y-%m-%dT%H:%M:%fZ' && value === 'now') return iso;
    throw new Error(`withDesktopClock: unpinned strftime(${String(format)}, ${String(value)})`);
  });
  const realNow = Date.now;
  Date.now = () => nowMs;
  try {
    return fn();
  } finally {
    Date.now = realNow;
    // Two parameters, like the pinned function, so it replaces that one.
    sql.function('strftime', (_format: unknown, _value: unknown) => {
      throw new Error('withDesktopClock: strftime after the pinned call');
    });
  }
}

type ModuleLoad = (request: string, parent: unknown, isMain: boolean) => unknown;

/** Routes `electron`, and `better-sqlite3` as electron/db.ts requires it, to stand-ins. */
function installDesktopShims(): void {
  if (desktopShimsInstalled) return;
  desktopShimsInstalled = true;
  const RealDatabase = requireHere('better-sqlite3') as typeof BetterSqlite3;
  class MemoryDatabase extends RealDatabase {
    constructor(_file?: string, options?: BetterSqlite3.Options) {
      super(':memory:', options);
    }
  }
  const electron = {
    app: {
      getPath: (): string => '/nonexistent/shelfy-golden',
      getName: (): string => 'ShelfyGolden',
      getVersion: (): string => '0.0.0',
      isPackaged: false,
    },
  };
  const Module = requireHere('module') as { _load: ModuleLoad };
  const load = Module._load;
  Module._load = function (this: unknown, request, parent, isMain) {
    if (request === 'electron') return electron;
    const from = (parent as { filename?: string } | null)?.filename;
    if (request === 'better-sqlite3' && from === DESKTOP_DB_FILE) return MemoryDatabase;
    return load.call(this, request, parent, isMain);
  };
}
