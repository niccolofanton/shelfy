// SPIKE-3 parity: the extension's capture vs the desktop's, per listing. CLI: compare.ts.
//
// Desktop data read (nothing else, never written):
//   DB (read-only, node:sqlite):
//     posts(id, platform, shortcode, post_url)            platforms instagram, twitter, pinterest
//     collections(id, platform, external_id, name)        platform instagram (IG folders) or
//                                                         pinterest (boards), external_id set
//     post_collections(post_id, collection_id)            membership of those collections
//   Export JSON (Settings → export, `{posts, collections}`):
//     posts[].id / platform / shortcode / postUrl / collections ("x:<externalId>" keys)
//     collections[].platform / externalId / name
//
// Listing mapping:
//   instagram:ig_saved            desktop: every instagram post       extension: every IG item
//                                                                     on ig_saved/ig_collection
//   instagram:ig_collection:<id>  desktop: posts in the collection with platform 'instagram'
//                                 and external_id <id>                extension: that listing
//   twitter:x_bookmarks           desktop: every twitter post         extension: every X item
//   pinterest:pin_board:<u>/<b>   desktop: posts in the collection with platform 'pinterest'
//                                 and external_id '<u>/<b>'; without one, every pinterest post
//                                 (and then every Pinterest item on the extension side too)
//
// Matching uses canonical keys plus aliases (src/identity.ts), so `<pk>_<owner>`, `<pk>` and
// shortcode ids of one IG post match each other. Desktop rows sharing an alias count as one post.
// Parity = matched desktop posts / desktop posts; pass when parity >= threshold (default 99 %).

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { parseArgs } from 'node:util';
import { parseExportFile, type ExportFile, type ExportItem } from '../src/export-format';
import { canonicalIdentity } from '../src/identity';
import { listingKey, parseListingKey, type ListingKind } from '../src/listing';
import {
  SOURCES,
  isPlatform,
  isRecord,
  isSource,
  type CaptureSource,
  type Platform,
} from '../src/protocol';

export const DEFAULT_THRESHOLD = 0.99;
export const DEFAULT_SHOW = 20;

// ── Desktop data ────────────────────────────────────────────────────────────

export interface DesktopRow {
  id: string;
  platform: Platform;
  shortcode: string;
  postUrl: string;
  /** Listing keys of the desktop collections (IG folder / Pinterest board) holding the post. */
  memberships: string[];
}

export interface DesktopCollection {
  platform: Platform;
  externalId: string;
  name: string;
  listingKey: string;
}

export interface DesktopData {
  kind: 'db' | 'export';
  path: string;
  rows: DesktopRow[];
  collections: DesktopCollection[];
}

export const DESKTOP_DB_QUERIES = {
  posts:
    "SELECT id, platform, shortcode, post_url FROM posts WHERE platform IN ('instagram', 'twitter', 'pinterest')",
  collections:
    "SELECT id, platform, external_id, name FROM collections WHERE platform IN ('instagram', 'pinterest') AND external_id IS NOT NULL AND external_id <> ''",
  memberships: 'SELECT post_id, collection_id FROM post_collections',
} as const;

/** Listing key of a desktop collection, or null when it is not a synced folder/board. */
export function collectionListingKey(platform: unknown, externalId: unknown): string | null {
  const ext = typeof externalId === 'number' ? String(externalId) : externalId;
  if (typeof ext !== 'string' || !ext) return null;
  if (platform === 'instagram' && /^\d+$/.test(ext))
    return listingKey('instagram', 'ig_collection', ext);
  if (platform === 'pinterest' && /^[^/]+\/[^/]+$/.test(ext))
    return listingKey('pinterest', 'pin_board', ext);
  return null;
}

const str = (value: unknown): string => (typeof value === 'string' ? value : '');

interface SqliteStatement {
  all(...params: unknown[]): unknown[];
}
interface SqliteDatabase {
  exec(sql: string): void;
  prepare(sql: string): SqliteStatement;
  close(): void;
}
interface NodeSqlite {
  DatabaseSync: new (path: string, options?: { readOnly?: boolean }) => SqliteDatabase;
}

/** node:sqlite (Node >= 22.13), loaded lazily so the export path works on any Node. */
export function loadNodeSqlite(): NodeSqlite {
  try {
    return createRequire(import.meta.url)('node:sqlite') as NodeSqlite;
  } catch (err) {
    throw new Error(
      `reading the desktop DB needs node:sqlite (Node >= 22.13); use --desktop-export instead ` +
        `(${err instanceof Error ? err.message : String(err)})`,
    );
  }
}

export function loadDesktopDb(path: string): DesktopData {
  const { DatabaseSync } = loadNodeSqlite();
  const db = new DatabaseSync(path, { readOnly: true });
  try {
    db.exec('PRAGMA query_only = 1');
    const collections: DesktopCollection[] = [];
    const keyByCollectionId = new Map<string, string>();
    for (const row of db.prepare(DESKTOP_DB_QUERIES.collections).all()) {
      if (!isRecord(row) || !isPlatform(row.platform)) continue;
      const key = collectionListingKey(row.platform, row.external_id);
      if (!key) continue;
      keyByCollectionId.set(String(row.id), key);
      collections.push({
        platform: row.platform,
        externalId: String(row.external_id),
        name: str(row.name),
        listingKey: key,
      });
    }
    const memberships = new Map<string, string[]>();
    for (const row of db.prepare(DESKTOP_DB_QUERIES.memberships).all()) {
      if (!isRecord(row)) continue;
      const key = keyByCollectionId.get(String(row.collection_id));
      if (!key) continue;
      const postId = String(row.post_id);
      memberships.set(postId, [...(memberships.get(postId) ?? []), key]);
    }
    const rows: DesktopRow[] = [];
    for (const row of db.prepare(DESKTOP_DB_QUERIES.posts).all()) {
      if (!isRecord(row) || !isPlatform(row.platform) || row.id == null) continue;
      const id = String(row.id);
      rows.push({
        id,
        platform: row.platform,
        shortcode: str(row.shortcode),
        postUrl: str(row.post_url),
        memberships: memberships.get(id) ?? [],
      });
    }
    return { kind: 'db', path, rows, collections };
  } finally {
    db.close();
  }
}

export function parseDesktopExport(json: unknown, path: string): DesktopData {
  const posts = Array.isArray(json)
    ? json
    : isRecord(json) && Array.isArray(json.posts)
      ? json.posts
      : null;
  if (!posts) throw new Error('not a desktop export: expected {"posts": [...]} or an array');
  const collections: DesktopCollection[] = [];
  const keyByExternalId = new Map<string, string>();
  const defs = isRecord(json) && Array.isArray(json.collections) ? json.collections : [];
  for (const def of defs) {
    if (!isRecord(def) || !isPlatform(def.platform)) continue;
    const key = collectionListingKey(def.platform, def.externalId);
    if (!key) continue;
    keyByExternalId.set(String(def.externalId), key);
    collections.push({
      platform: def.platform,
      externalId: String(def.externalId),
      name: str(def.name),
      listingKey: key,
    });
  }
  const rows: DesktopRow[] = [];
  for (const post of posts) {
    if (!isRecord(post) || !isPlatform(post.platform) || post.id == null) continue;
    const keys = Array.isArray(post.collections) ? post.collections : [];
    rows.push({
      id: String(post.id),
      platform: post.platform,
      shortcode: str(post.shortcode),
      postUrl: str(post.postUrl),
      // Collection keys are "x:<externalId>" (see collectionKey in electron/db.ts).
      memberships: keys
        .filter((k): k is string => typeof k === 'string' && k.startsWith('x:'))
        .map((k) => keyByExternalId.get(k.slice(2)))
        .filter((k): k is string => !!k),
    });
  }
  return { kind: 'export', path, rows, collections };
}

export function loadDesktopExport(path: string): DesktopData {
  return parseDesktopExport(JSON.parse(readFileSync(path, 'utf8')) as unknown, path);
}

export function loadExtensionExport(path: string): ExportFile {
  return parseExportFile(JSON.parse(readFileSync(path, 'utf8')) as unknown);
}

// ── Listing selection ───────────────────────────────────────────────────────

export interface ListingSpec {
  key: string;
  platform: Platform;
  kind: ListingKind;
  externalId: string | null;
  /** 'platform': every post of the platform on both sides; 'listing': one folder/board. */
  scope: 'platform' | 'listing';
}

/** Listings both sides can be compared on; ig_saved_index is not a desktop sync target. */
export function specForKey(key: string): ListingSpec | null {
  const parsed = parseListingKey(key);
  if (!parsed) return null;
  if (parsed.kind === 'ig_saved' || parsed.kind === 'x_bookmarks')
    return { key, ...parsed, scope: 'platform' };
  if (parsed.kind === 'ig_collection' || parsed.kind === 'pin_board')
    return { key, ...parsed, scope: 'listing' };
  return null;
}

const SYNC_KINDS: Record<Platform, readonly ListingKind[]> = {
  instagram: ['ig_saved', 'ig_collection'],
  twitter: ['x_bookmarks'],
  pinterest: ['pin_board'],
};

export interface DesktopSelection {
  rows: DesktopRow[];
  mode: 'platform' | 'collection' | 'platform-fallback';
  note: string | null;
}

export function selectDesktop(data: DesktopData, spec: ListingSpec): DesktopSelection {
  const ofPlatform = data.rows.filter((row) => row.platform === spec.platform);
  if (spec.scope === 'platform') return { rows: ofPlatform, mode: 'platform', note: null };
  if (data.collections.some((c) => c.listingKey === spec.key))
    return {
      rows: data.rows.filter((row) => row.memberships.includes(spec.key)),
      mode: 'collection',
      note: null,
    };
  if (spec.platform === 'pinterest')
    return {
      rows: ofPlatform,
      mode: 'platform-fallback',
      note: 'the desktop has no collection for this board: comparing every Pinterest post on both sides',
    };
  return {
    rows: [],
    mode: 'collection',
    note: 'the desktop has no collection for this IG folder: file it into a tag when running Auto-import',
  };
}

export function selectExtension(
  file: ExportFile,
  spec: ListingSpec,
  mode: DesktopSelection['mode'],
  excludeSources: ReadonlySet<CaptureSource>,
): ExportItem[] {
  const wholePlatform = spec.scope === 'platform' || mode === 'platform-fallback';
  return file.items.filter(
    (item) =>
      item.platform === spec.platform &&
      item.listings.some((membership) => {
        const parsed = parseListingKey(membership.key);
        if (!parsed) return false;
        const inScope = wholePlatform
          ? SYNC_KINDS[spec.platform].includes(parsed.kind)
          : membership.key === spec.key;
        return inScope && membership.sources.some((source) => !excludeSources.has(source));
      }),
  );
}

// ── Matching ────────────────────────────────────────────────────────────────

export interface MissingPost {
  key: string;
  ids: string[];
  postUrl: string;
}

export interface ExtraItem {
  key: string;
  postUrl: string;
  sources: CaptureSource[];
}

export interface ListingComparison {
  key: string;
  desktopMode: DesktopSelection['mode'];
  notes: string[];
  desktop: { rows: number; posts: number; unmatchable: number; collapsedRows: number };
  extension: { items: number; duplicates: number };
  matched: number;
  missing: MissingPost[];
  extra: ExtraItem[];
  /** matched / desktop posts; null when the desktop side is empty. */
  parity: number | null;
  pass: boolean;
}

interface DesktopGroup {
  key: string;
  rows: DesktopRow[];
}

function findRoot(parent: number[], i: number): number {
  while (parent[i] !== i) {
    parent[i] = parent[parent[i]];
    i = parent[i];
  }
  return i;
}

/** Groups desktop rows that are the same post (any shared alias), e.g. IG pk vs pk_owner rows. */
export function groupDesktopRows(rows: readonly DesktopRow[]): {
  groups: DesktopGroup[];
  aliasToGroup: Map<string, number>;
  unmatchable: DesktopRow[];
} {
  const identities = rows.map((row) =>
    canonicalIdentity(row.platform, {
      ids: [row.id],
      shortcode: row.shortcode,
      postUrl: row.postUrl,
    }),
  );
  const parent = rows.map((_, i) => i);
  const firstRowByAlias = new Map<string, number>();
  identities.forEach((identity, i) => {
    for (const alias of identity?.aliases ?? []) {
      const seen = firstRowByAlias.get(alias);
      if (seen === undefined) firstRowByAlias.set(alias, i);
      else parent[findRoot(parent, i)] = findRoot(parent, seen);
    }
  });
  const groupByRoot = new Map<number, number>();
  const groups: DesktopGroup[] = [];
  const unmatchable: DesktopRow[] = [];
  rows.forEach((row, i) => {
    const identity = identities[i];
    if (!identity) {
      unmatchable.push(row);
      return;
    }
    const root = findRoot(parent, i);
    let index = groupByRoot.get(root);
    if (index === undefined) {
      index = groups.length;
      groupByRoot.set(root, index);
      groups.push({ key: identity.key, rows: [] });
    }
    groups[index].rows.push(row);
  });
  const aliasToGroup = new Map<string, number>();
  for (const [alias, rowIndex] of firstRowByAlias) {
    const group = groupByRoot.get(findRoot(parent, rowIndex));
    if (group !== undefined) aliasToGroup.set(alias, group);
  }
  return { groups, aliasToGroup, unmatchable };
}

function itemAliases(item: ExportItem): string[] {
  const identity = canonicalIdentity(item.platform, {
    ids: [...item.rawIds, item.nativeId],
    shortcode: item.shortcode,
    postUrl: item.postUrl,
  });
  return [item.key, ...(identity?.aliases ?? [])];
}

export interface CompareOptions {
  threshold: number;
  excludeSources: ReadonlySet<CaptureSource>;
}

export function compareListing(
  spec: ListingSpec,
  desktop: DesktopData,
  extension: ExportFile,
  options: CompareOptions,
): ListingComparison {
  const selection = selectDesktop(desktop, spec);
  const items = selectExtension(extension, spec, selection.mode, options.excludeSources);
  const { groups, aliasToGroup, unmatchable } = groupDesktopRows(selection.rows);
  const matchedGroups = new Set<number>();
  const extra: ExtraItem[] = [];
  let duplicates = 0;
  for (const item of items) {
    const group = itemAliases(item)
      .map((alias) => aliasToGroup.get(alias))
      .find((g): g is number => g !== undefined);
    if (group === undefined) {
      const sources = new Set<CaptureSource>();
      for (const membership of item.listings) for (const s of membership.sources) sources.add(s);
      extra.push({ key: item.key, postUrl: item.postUrl, sources: [...sources] });
    } else if (matchedGroups.has(group)) duplicates += 1;
    else matchedGroups.add(group);
  }
  const missing: MissingPost[] = groups
    .filter((_, index) => !matchedGroups.has(index))
    .map((group) => ({
      key: group.key,
      ids: group.rows.map((row) => row.id),
      postUrl: group.rows.find((row) => row.postUrl)?.postUrl ?? '',
    }));
  const parity = groups.length ? matchedGroups.size / groups.length : null;
  const notes = selection.note ? [selection.note] : [];
  if (unmatchable.length)
    notes.push(`${unmatchable.length} desktop rows have no usable id and were left out`);
  return {
    key: spec.key,
    desktopMode: selection.mode,
    notes,
    desktop: {
      rows: selection.rows.length,
      posts: groups.length,
      unmatchable: unmatchable.length,
      collapsedRows: selection.rows.length - unmatchable.length - groups.length,
    },
    extension: { items: items.length, duplicates },
    matched: matchedGroups.size,
    missing,
    extra,
    parity,
    pass: parity !== null && parity >= options.threshold,
  };
}

// ── Report ──────────────────────────────────────────────────────────────────

export interface CompareReport {
  generatedAt: string;
  threshold: number;
  excludeSources: CaptureSource[];
  desktop: { kind: DesktopData['kind']; path: string; rows: number };
  extension: { path: string; exportedAt: string; items: number };
  listings: ListingComparison[];
  /** Desktop folders/boards the extension export never visited (informational). */
  desktopOnlyCollections: string[];
  pass: boolean;
}

export function defaultListingKeys(extension: ExportFile): string[] {
  const keys = new Set<string>();
  for (const listing of extension.listings) if (specForKey(listing.key)) keys.add(listing.key);
  return [...keys].sort();
}

export function buildReport(
  desktop: DesktopData,
  extension: ExportFile,
  extensionPath: string,
  listingKeys: readonly string[],
  options: CompareOptions,
  now: Date = new Date(),
): CompareReport {
  const listings: ListingComparison[] = [];
  for (const key of listingKeys) {
    const spec = specForKey(key);
    if (!spec) throw new Error(`cannot compare listing "${key}"`);
    listings.push(compareListing(spec, desktop, extension, options));
  }
  const visited = new Set(extension.listings.map((l) => l.key));
  return {
    generatedAt: now.toISOString(),
    threshold: options.threshold,
    excludeSources: [...options.excludeSources],
    desktop: { kind: desktop.kind, path: desktop.path, rows: desktop.rows.length },
    extension: {
      path: extensionPath,
      exportedAt: extension.exportedAt,
      items: extension.items.length,
    },
    listings,
    desktopOnlyCollections: desktop.collections
      .map((c) => c.listingKey)
      .filter((key) => !visited.has(key))
      .sort(),
    pass: listings.length > 0 && listings.every((listing) => listing.pass),
  };
}

function percent(value: number | null): string {
  return value === null ? 'n/a' : `${(value * 100).toFixed(2)} %`;
}

export function formatReport(report: CompareReport, show = DEFAULT_SHOW): string {
  const lines: string[] = [];
  lines.push('SPIKE-3 parity: extension export vs desktop');
  lines.push(
    `  desktop:   ${report.desktop.kind} ${report.desktop.path} (${report.desktop.rows} posts)`,
  );
  lines.push(
    `  extension: ${report.extension.path} (${report.extension.items} items, exported ${report.extension.exportedAt})`,
  );
  lines.push(
    `  threshold: ${percent(report.threshold)}` +
      (report.excludeSources.length
        ? ` · excluding sources: ${report.excludeSources.join(', ')}`
        : ''),
  );
  lines.push('');
  const header = [
    'listing',
    'desktop',
    'extension',
    'matched',
    'missing',
    'extra',
    'parity',
    'result',
  ];
  const rows = report.listings.map((l) => [
    l.key,
    String(l.desktop.posts),
    String(l.extension.items),
    String(l.matched),
    String(l.missing.length),
    String(l.extra.length),
    percent(l.parity),
    l.pass ? 'PASS' : 'FAIL',
  ]);
  const widths = header.map((h, i) => Math.max(h.length, ...rows.map((r) => r[i].length)));
  const fmt = (cells: string[]): string =>
    cells
      .map((cell, i) => (i === 0 ? cell.padEnd(widths[i]) : cell.padStart(widths[i])))
      .join('  ');
  lines.push(fmt(header));
  for (const row of rows) lines.push(fmt(row));
  if (!rows.length) lines.push('(no comparable listings in the extension export)');

  for (const listing of report.listings) {
    const details: string[] = [...listing.notes];
    if (listing.desktop.collapsedRows)
      details.push(
        `${listing.desktop.collapsedRows} duplicate desktop rows collapsed into their post`,
      );
    if (listing.extension.duplicates)
      details.push(
        `${listing.extension.duplicates} extension items matched an already matched post`,
      );
    if (listing.missing.length) {
      details.push(`missing (desktop only), first ${Math.min(show, listing.missing.length)}:`);
      for (const m of listing.missing.slice(0, show))
        details.push(`  ${m.key}  ${m.postUrl || m.ids.join(', ')}`);
    }
    if (listing.extra.length) {
      details.push(`extra (extension only), first ${Math.min(show, listing.extra.length)}:`);
      for (const e of listing.extra.slice(0, show))
        details.push(`  ${e.key}  ${e.postUrl}  [${e.sources.join(', ')}]`);
    }
    if (details.length) {
      lines.push('');
      lines.push(`${listing.key} (desktop: ${listing.desktopMode})`);
      for (const detail of details) lines.push(`  ${detail}`);
    }
  }
  if (report.desktopOnlyCollections.length) {
    lines.push('');
    lines.push(
      `desktop collections the extension did not visit: ${report.desktopOnlyCollections.join(', ')}`,
    );
  }
  lines.push('');
  lines.push(report.pass ? 'RESULT: PASS' : 'RESULT: FAIL');
  return `${lines.join('\n')}\n`;
}

// ── CLI ─────────────────────────────────────────────────────────────────────

export const USAGE = `Usage:
  pnpm exec tsx extension/scripts/compare.ts --extension <export.json>
      (--desktop-db <shelfy.sqlite> | --desktop-export <saved-posts.json>)
      [--listing <key>]... [--exclude-source <source>]... [--threshold <0-1 or %>]
      [--json <report.json>] [--show <n>]

  --extension       "Export JSON" file from the extension side panel
  --desktop-db      desktop library DB, opened read-only (quit the desktop app first)
  --desktop-export  desktop JSON export ({posts, collections})
  --listing         listing key to compare (repeatable); default: every comparable listing in
                    the extension export, e.g. instagram:ig_saved, instagram:ig_collection:<id>,
                    twitter:x_bookmarks, pinterest:pin_board:<user>/<board>
  --exclude-source  ignore extension captures from a source (${SOURCES.join(', ')}), e.g.
                    --exclude-source replay for passive-only parity
  --threshold       pass threshold, default 0.99 (99 %)
  --json            also write the full report (all missing/extra ids) as JSON
  --show            ids listed per category in the text output, default ${DEFAULT_SHOW}

Exit codes: 0 every listing passes, 1 a listing fails, 2 usage or input error.`;

export interface CliIo {
  stdout(text: string): void;
  stderr(text: string): void;
  writeFile(path: string, content: string): void;
  now?(): Date;
}

function parseThreshold(raw: string | undefined): number {
  if (raw === undefined) return DEFAULT_THRESHOLD;
  const value = Number(raw.replace(/%$/, ''));
  const threshold = raw.endsWith('%') || value > 1 ? value / 100 : value;
  if (!Number.isFinite(threshold) || threshold <= 0 || threshold > 1)
    throw new Error(`invalid --threshold "${raw}"`);
  return threshold;
}

const CLI_OPTIONS = {
  extension: { type: 'string' },
  'desktop-db': { type: 'string' },
  'desktop-export': { type: 'string' },
  listing: { type: 'string', multiple: true },
  'exclude-source': { type: 'string', multiple: true },
  threshold: { type: 'string' },
  json: { type: 'string' },
  show: { type: 'string' },
  help: { type: 'boolean', short: 'h' },
} as const;

function parseCli(argv: readonly string[]) {
  return parseArgs({ args: [...argv], options: CLI_OPTIONS, strict: true, allowPositionals: false })
    .values;
}

export function runCompareCli(argv: readonly string[], io: CliIo): number {
  let values: ReturnType<typeof parseCli>;
  try {
    values = parseCli(argv);
  } catch (err) {
    io.stderr(`${err instanceof Error ? err.message : String(err)}\n\n${USAGE}\n`);
    return 2;
  }
  if (values.help) {
    io.stdout(`${USAGE}\n`);
    return 0;
  }
  try {
    if (!values.extension) throw new Error('--extension is required');
    const dbPath = values['desktop-db'];
    const exportPath = values['desktop-export'];
    if (!dbPath === !exportPath)
      throw new Error('pass exactly one of --desktop-db or --desktop-export');
    const excludeSources = new Set<CaptureSource>();
    for (const source of values['exclude-source'] ?? []) {
      if (!isSource(source))
        throw new Error(`unknown source "${source}" (use ${SOURCES.join(', ')})`);
      excludeSources.add(source);
    }
    const threshold = parseThreshold(values.threshold);
    const show = values.show === undefined ? DEFAULT_SHOW : Number(values.show);
    if (!Number.isInteger(show) || show < 0) throw new Error(`invalid --show "${values.show}"`);

    const extension = loadExtensionExport(values.extension);
    const desktop = dbPath ? loadDesktopDb(dbPath) : loadDesktopExport(exportPath as string);
    const keys = values.listing?.length ? values.listing : defaultListingKeys(extension);
    for (const key of keys)
      if (!specForKey(key)) throw new Error(`cannot compare listing "${key}"`);
    const report = buildReport(
      desktop,
      extension,
      values.extension,
      keys,
      { threshold, excludeSources },
      io.now?.() ?? new Date(),
    );
    io.stdout(formatReport(report, show));
    if (values.json) io.writeFile(values.json, `${JSON.stringify(report, null, 2)}\n`);
    return report.pass ? 0 : 1;
  } catch (err) {
    io.stderr(`compare: ${err instanceof Error ? err.message : String(err)}\n`);
    return 2;
  }
}
