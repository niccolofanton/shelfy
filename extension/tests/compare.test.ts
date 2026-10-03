// compare.ts on a synthetic desktop library (SQLite DB and JSON export) and a synthetic
// extension export. No real library data is involved.

import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import {
  DESKTOP_DB_QUERIES,
  buildReport,
  defaultListingKeys,
  formatReport,
  loadDesktopDb,
  parseDesktopExport,
  runCompareCli,
  type CompareOptions,
  type DesktopData,
} from '../scripts/compare-lib';
import {
  EXPORT_FORMAT,
  EXPORT_VERSION,
  emptyDiagnostics,
  emptyPlatformStats,
  type ExportFile,
  type ExportItem,
} from '../scripts/export-format';
import { parseListingKey } from '../src/shared/listing';
import type { CaptureSource } from '../src/shared/protocol';

const FOLDER = 'instagram:ig_collection:17890000000000001';
const BOARD = 'pinterest:pin_board:someone/recipes';
const T0 = 1_760_000_000_000;

// ── Synthetic desktop library ───────────────────────────────────────────────

const DESKTOP_POSTS = [
  [
    '3400000000000000001_9000000001',
    'instagram',
    'C8vOfxsVAAB',
    'https://www.instagram.com/p/C8vOfxsVAAB/',
  ],
  [
    '3400000000000000002_9000000002',
    'instagram',
    'C8vOfxsVAAC',
    'https://www.instagram.com/p/C8vOfxsVAAC/',
  ],
  [
    '3400000000000000003_9000000001',
    'instagram',
    'C8vOfxsVAAD',
    'https://www.instagram.com/p/C8vOfxsVAAD/',
  ],
  // Same post as the row above, captured through GraphQL (bare pk): one post, two rows.
  ['3400000000000000003', 'instagram', 'C8vOfxsVAAD', 'https://www.instagram.com/p/C8vOfxsVAAD/'],
  [
    '3400000000000000005_9000000003',
    'instagram',
    'C8vOfxsVAAF',
    'https://www.instagram.com/p/C8vOfxsVAAF/',
  ],
  [
    '1800000000000000001',
    'twitter',
    '',
    'https://x.com/synthetic_x_user/status/1800000000000000001',
  ],
  [
    '1800000000000000002',
    'twitter',
    '',
    'https://x.com/synthetic_x_other/status/1800000000000000002',
  ],
  ['900000000000000001', 'pinterest', '', 'https://www.pinterest.com/pin/900000000000000001/'],
  ['900000000000000002', 'pinterest', '', 'https://www.pinterest.com/pin/900000000000000002/'],
  ['900000000000000003', 'pinterest', '', 'https://www.pinterest.com/pin/900000000000000003/'],
  ['m_manual', 'manual', '', ''],
] as const;

const DESKTOP_COLLECTIONS = [
  [1, 'Recipes', 'instagram', '17890000000000001'],
  [2, 'Recipes board', 'pinterest', 'someone/recipes'],
  [3, 'Other board', 'pinterest', 'someone/other'],
  [4, 'Manual tag', null, null],
] as const;

const DESKTOP_MEMBERSHIPS = [
  ['3400000000000000001_9000000001', 1],
  ['3400000000000000003_9000000001', 1],
  ['900000000000000001', 2],
  ['900000000000000002', 2],
  ['900000000000000003', 3],
  ['1800000000000000001', 4],
] as const;

function writeDesktopDb(path: string): void {
  const { DatabaseSync } = createRequire(import.meta.url)('node:sqlite') as {
    DatabaseSync: new (p: string) => {
      exec(sql: string): void;
      prepare(sql: string): { run(...args: unknown[]): void };
      close(): void;
    };
  };
  const db = new DatabaseSync(path);
  // Subset of the desktop schema (electron/db.ts) with the columns compare.ts reads.
  db.exec(`
    CREATE TABLE posts (id TEXT PRIMARY KEY, platform TEXT NOT NULL, shortcode TEXT, post_url TEXT,
      text TEXT, imported_at INTEGER DEFAULT (unixepoch()));
    CREATE TABLE collections (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
      color TEXT NOT NULL DEFAULT '#3d5afe', platform TEXT, external_id TEXT, ig_name TEXT,
      created_at INTEGER DEFAULT (unixepoch()));
    CREATE TABLE post_collections (post_id TEXT NOT NULL, collection_id INTEGER NOT NULL,
      added_at INTEGER DEFAULT (unixepoch()), PRIMARY KEY (post_id, collection_id));
  `);
  const post = db.prepare(
    'INSERT INTO posts (id, platform, shortcode, post_url, text) VALUES (?, ?, ?, ?, ?)',
  );
  for (const [id, platform, shortcode, url] of DESKTOP_POSTS)
    post.run(id, platform, shortcode, url, 'caption');
  const collection = db.prepare(
    'INSERT INTO collections (id, name, platform, external_id) VALUES (?, ?, ?, ?)',
  );
  for (const row of DESKTOP_COLLECTIONS) collection.run(...row);
  const membership = db.prepare(
    'INSERT INTO post_collections (post_id, collection_id) VALUES (?, ?)',
  );
  for (const row of DESKTOP_MEMBERSHIPS) membership.run(...row);
  db.close();
}

function desktopExportJson(withPinterestCollections = true): unknown {
  const extById = new Map<number, string>();
  for (const [id, , , ext] of DESKTOP_COLLECTIONS) if (ext) extById.set(id, ext);
  return {
    posts: DESKTOP_POSTS.map(([id, platform, shortcode, postUrl]) => ({
      id,
      platform,
      shortcode,
      postUrl,
      text: 'caption',
      collections: DESKTOP_MEMBERSHIPS.filter(([postId]) => postId === id).map(([, cid]) =>
        extById.has(cid) ? `x:${extById.get(cid)}` : 'n:Manual tag',
      ),
    })),
    collections: DESKTOP_COLLECTIONS.filter(
      ([, , platform]) => withPinterestCollections || platform !== 'pinterest',
    ).map(([, name, platform, externalId]) => ({
      name,
      color: '#3d5afe',
      platform,
      externalId,
      igName: null,
    })),
  };
}

// ── Synthetic extension export ──────────────────────────────────────────────

function item(
  key: string,
  rawIds: string[],
  listings: Array<[string, CaptureSource[]]>,
  extra: Partial<ExportItem> = {},
): ExportItem {
  const [prefix, nativeId] = key.split('_');
  const platform = prefix === 'ig' ? 'instagram' : prefix === 'x' ? 'twitter' : 'pinterest';
  return {
    key,
    platform,
    nativeId,
    rawIds,
    shortcode: '',
    postUrl: '',
    mediaType: 'image',
    mediaCount: 1,
    postedAt: '',
    firstCapturedAt: T0,
    lastCapturedAt: T0,
    captureCount: 1,
    listings: listings.map(([listingKey, sources]) => ({
      key: listingKey,
      sources,
      firstCapturedAt: T0,
      lastCapturedAt: T0,
    })),
    media: [],
    ...extra,
  };
}

function extensionExport(items: ExportItem[]): ExportFile {
  const keys = new Set(items.flatMap((i) => i.listings.map((l) => l.key)));
  return {
    format: EXPORT_FORMAT,
    version: EXPORT_VERSION,
    exportedAt: new Date(T0).toISOString(),
    extension: { version: '0.1.0', userAgent: 'test' },
    storeCreatedAt: T0,
    platforms: {
      instagram: emptyPlatformStats(),
      twitter: emptyPlatformStats(),
      pinterest: emptyPlatformStats(),
    },
    diagnostics: emptyDiagnostics(),
    listings: [...keys].map((key) => {
      const parsed = parseListingKey(key);
      if (!parsed) throw new Error(key);
      return {
        key,
        ...parsed,
        name: null,
        account: null,
        firstSeenAt: T0,
        lastSeenAt: T0,
        batches: 1,
        itemsReceived: 1,
        uniqueItems: 1,
        bySource: { passive: 0, replay: 0, ssr: 0, dom: 0 },
        endOfFeedSeen: true,
        lastHasNextPage: false,
      };
    }),
    items,
  };
}

const EXTENSION = extensionExport([
  // GraphQL form of a post the desktop stored as <pk>_<owner>.
  item(
    'ig_3400000000000000001',
    ['3400000000000000001'],
    [
      ['instagram:ig_saved', ['passive']],
      [FOLDER, ['replay']],
    ],
  ),
  item(
    'ig_3400000000000000002',
    ['3400000000000000002_9000000002'],
    [['instagram:ig_saved', ['replay']]],
  ),
  item(
    'ig_3400000000000000003',
    ['3400000000000000003_9000000001'],
    [
      ['instagram:ig_saved', ['passive']],
      [FOLDER, ['passive']],
    ],
  ),
  item('ig_3400000000000000004', ['3400000000000000004'], [['instagram:ig_saved', ['passive']]], {
    postUrl: 'https://www.instagram.com/p/C8vOfxsVAAE/',
  }),
  // Seen only on the folder index page: never compared.
  item(
    'ig_3400000000000000006',
    ['3400000000000000006'],
    [['instagram:ig_saved_index', ['passive']]],
  ),
  item('x_1800000000000000001', ['1800000000000000001'], [['twitter:x_bookmarks', ['passive']]]),
  item('x_1800000000000000002', ['1800000000000000002'], [['twitter:x_bookmarks', ['dom']]]),
  item('pin_900000000000000001', ['900000000000000001'], [[BOARD, ['ssr']]]),
  item('pin_900000000000000002', ['900000000000000002'], [[BOARD, ['passive']]]),
]);

const ALL_SOURCES: CompareOptions = { threshold: 0.99, excludeSources: new Set() };

let dir = '';
let dbPath = '';
const hasNodeSqlite = (() => {
  try {
    createRequire(import.meta.url)('node:sqlite');
    return true;
  } catch {
    return false;
  }
})();

beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), 'shelfy-spike3-compare-'));
  dbPath = join(dir, 'shelfy.sqlite');
  if (hasNodeSqlite) writeDesktopDb(dbPath);
  writeFileSync(join(dir, 'extension.json'), JSON.stringify(EXTENSION));
  writeFileSync(join(dir, 'desktop-export.json'), JSON.stringify(desktopExportJson()));
});

afterAll(() => {
  rmSync(dir, { recursive: true, force: true });
});

function byKey(report: ReturnType<typeof buildReport>) {
  return Object.fromEntries(report.listings.map((l) => [l.key, l]));
}

function expectStandardResults(desktop: DesktopData): void {
  const report = buildReport(
    desktop,
    EXTENSION,
    'extension.json',
    defaultListingKeys(EXTENSION),
    ALL_SOURCES,
  );
  const listings = byKey(report);
  expect(Object.keys(listings)).toEqual([
    FOLDER,
    'instagram:ig_saved',
    BOARD,
    'twitter:x_bookmarks',
  ]);

  expect(listings['instagram:ig_saved']).toMatchObject({
    desktopMode: 'platform',
    desktop: { rows: 5, posts: 4, collapsedRows: 1, unmatchable: 0 },
    extension: { items: 4, duplicates: 0 },
    matched: 3,
    parity: 0.75,
    pass: false,
  });
  expect(listings['instagram:ig_saved'].missing).toEqual([
    {
      key: 'ig_3400000000000000005',
      ids: ['3400000000000000005_9000000003'],
      postUrl: 'https://www.instagram.com/p/C8vOfxsVAAF/',
    },
  ]);
  expect(listings['instagram:ig_saved'].extra).toEqual([
    {
      key: 'ig_3400000000000000004',
      postUrl: 'https://www.instagram.com/p/C8vOfxsVAAE/',
      sources: ['passive'],
    },
  ]);
  expect(listings[FOLDER]).toMatchObject({
    desktopMode: 'collection',
    desktop: { posts: 2 },
    matched: 2,
    parity: 1,
    pass: true,
  });
  expect(listings['twitter:x_bookmarks']).toMatchObject({ matched: 2, parity: 1, pass: true });
  expect(listings[BOARD]).toMatchObject({
    desktopMode: 'collection',
    matched: 2,
    parity: 1,
    pass: true,
  });
  expect(report.desktopOnlyCollections).toEqual(['pinterest:pin_board:someone/other']);
  expect(report.pass).toBe(false);
}

describe.skipIf(!hasNodeSqlite)('desktop DB (read-only)', () => {
  it('reads only posts, collections and memberships of synced folders/boards', () => {
    const desktop = loadDesktopDb(dbPath);
    expect(desktop.rows).toHaveLength(10);
    expect(desktop.rows.some((row) => (row.platform as string) === 'manual')).toBe(false);
    expect(desktop.collections.map((c) => c.listingKey)).toEqual([
      FOLDER,
      BOARD,
      'pinterest:pin_board:someone/other',
    ]);
    expect(desktop.rows.find((row) => row.id === '1800000000000000001')?.memberships).toEqual([]);
    expect(Object.values(DESKTOP_DB_QUERIES).join(' ')).not.toMatch(/\btext\b|thumbnail|_path/);
  });

  it('never modifies the database file', () => {
    const hash = () => createHash('sha256').update(readFileSync(dbPath)).digest('hex');
    const before = hash();
    loadDesktopDb(dbPath);
    expect(hash()).toBe(before);
  });

  it('computes parity per listing', () => {
    expectStandardResults(loadDesktopDb(dbPath));
  });
});

describe('desktop JSON export', () => {
  it('gives the same results as the DB', () => {
    expectStandardResults(parseDesktopExport(desktopExportJson(), 'desktop-export.json'));
  });

  it('accepts a bare array of posts', () => {
    const desktop = parseDesktopExport(
      [{ id: '1800000000000000001', platform: 'twitter', postUrl: '' }],
      'array.json',
    );
    expect(desktop.rows).toHaveLength(1);
    expect(desktop.collections).toEqual([]);
  });

  it('falls back to every Pinterest post when the board has no desktop collection', () => {
    const desktop = parseDesktopExport(desktopExportJson(false), 'desktop-export.json');
    const [board] = buildReport(desktop, EXTENSION, 'x', [BOARD], ALL_SOURCES).listings;
    expect(board).toMatchObject({ desktopMode: 'platform-fallback', matched: 2, parity: 2 / 3 });
    expect(board.notes[0]).toMatch(/no collection for this board/);
  });

  it('fails an IG folder the desktop never filed into a collection', () => {
    const desktop = parseDesktopExport({ posts: [], collections: [] }, 'empty.json');
    const [folder] = buildReport(desktop, EXTENSION, 'x', [FOLDER], ALL_SOURCES).listings;
    expect(folder).toMatchObject({ parity: null, pass: false });
    expect(folder.notes[0]).toMatch(/file it into a tag/);
  });

  it('computes passive-only parity by excluding the replay source', () => {
    const desktop = parseDesktopExport(desktopExportJson(), 'desktop-export.json');
    const report = buildReport(desktop, EXTENSION, 'x', ['instagram:ig_saved', FOLDER], {
      threshold: 0.99,
      excludeSources: new Set(['replay']),
    });
    const listings = byKey(report);
    expect(listings['instagram:ig_saved']).toMatchObject({ matched: 2, parity: 0.5 });
    expect(listings[FOLDER]).toMatchObject({ matched: 1, parity: 0.5 });
    expect(formatReport(report)).toContain('excluding sources: replay');
  });
});

describe('CLI', () => {
  function run(args: string[]) {
    let stdout = '';
    let stderr = '';
    const written: Record<string, string> = {};
    const code = runCompareCli(args, {
      stdout: (text) => (stdout += text),
      stderr: (text) => (stderr += text),
      writeFile: (path, content) => (written[path] = content),
      now: () => new Date(T0),
    });
    return { code, stdout, stderr, written };
  }

  it('prints the table, details and result, and writes the JSON report', () => {
    const extension = join(dir, 'extension.json');
    const { code, stdout, written } = run([
      '--extension',
      extension,
      '--desktop-export',
      join(dir, 'desktop-export.json'),
      '--json',
      'report.json',
    ]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/instagram:ig_saved\s+4\s+4\s+3\s+1\s+1\s+75\.00 %\s+FAIL/);
    expect(stdout).toMatch(/twitter:x_bookmarks\s+2\s+2\s+2\s+0\s+0\s+100\.00 %\s+PASS/);
    expect(stdout).toContain('ig_3400000000000000005  https://www.instagram.com/p/C8vOfxsVAAF/');
    expect(stdout).toContain('RESULT: FAIL');
    const report = JSON.parse(written['report.json']);
    expect(report.generatedAt).toBe(new Date(T0).toISOString());
    expect(report.listings).toHaveLength(4);
  });

  it('exits 0 when every selected listing passes', () => {
    const { code, stdout } = run([
      '--extension',
      join(dir, 'extension.json'),
      '--desktop-export',
      join(dir, 'desktop-export.json'),
      '--listing',
      'twitter:x_bookmarks',
      '--listing',
      BOARD,
      '--threshold',
      '99%',
    ]);
    expect(code).toBe(0);
    expect(stdout).toContain('RESULT: PASS');
  });

  it.skipIf(!hasNodeSqlite)('reads the desktop DB path', () => {
    const { code, stdout } = run([
      '--extension',
      join(dir, 'extension.json'),
      '--desktop-db',
      dbPath,
    ]);
    expect(code).toBe(1);
    expect(stdout).toContain(`desktop:   db ${dbPath} (10 posts)`);
  });

  it('exits 2 on usage and input errors', () => {
    const extension = join(dir, 'extension.json');
    const desktop = join(dir, 'desktop-export.json');
    expect(run([]).code).toBe(2);
    expect(run(['--extension', extension]).stderr).toMatch(/exactly one of --desktop-db/);
    expect(
      run(['--extension', extension, '--desktop-export', desktop, '--desktop-db', 'x']).code,
    ).toBe(2);
    expect(
      run(['--extension', extension, '--desktop-export', desktop, '--exclude-source', 'magic'])
        .stderr,
    ).toMatch(/unknown source/);
    expect(
      run([
        '--extension',
        extension,
        '--desktop-export',
        desktop,
        '--listing',
        'instagram:ig_saved_index',
      ]).stderr,
    ).toMatch(/cannot compare listing/);
    expect(
      run(['--extension', extension, '--desktop-export', desktop, '--threshold', '2']).code,
    ).toBe(0);
    expect(
      run(['--extension', extension, '--desktop-export', desktop, '--threshold', '0']).code,
    ).toBe(2);
    expect(run(['--extension', desktop, '--desktop-export', desktop]).stderr).toMatch(
      /not a SPIKE-3 extension export/,
    );
    expect(run(['--bogus']).code).toBe(2);
    expect(run(['--help']).code).toBe(0);
  });
});
