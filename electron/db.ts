import Database from 'better-sqlite3';
import type { Database as DatabaseType, RunResult, Statement } from 'better-sqlite3';
import path from 'path';
import crypto from 'crypto';
import { app } from 'electron';
import fs from 'fs';
import { normalizeExportedPost as normalizeIg, igDateFromShortcode } from './ig-parser';
import { normalizeExportedPost as normalizeTw } from './tw-parser';
import { Worker } from 'worker_threads';
// Clustering core lives in its own module so it can be shared verbatim between the
// in-process / fallback path here and the worker_threads offload (cluster-worker.js).
import { buildTagCommunities, cosineSim } from './cluster-core';

// ── Local helper types ─────────────────────────────────────────────────────────

// A normalized, write-path post: the loose camelCase/snake_case shape the parsers
// and web-ref mapper emit, consumed by postToRow / deriveMedia / extractAiFields.
// Everything is optional because the upsert path tolerates partial records (the
// ?? chains pick whichever spelling/shape is present) and AI fields use undefined
// to mean "not provided" (column left untouched). Indexed signature keeps it open
// to the snake_case aliases the ?? chains read.
interface PostInput {
  id: string;
  platform?: string;
  shortcode?: string | null;
  postUrl?: string | null;
  post_url?: string | null;
  profileUrl?: string | null;
  profile_url?: string | null;
  authorUsername?: string | null;
  author_username?: string | null;
  authorName?: string | null;
  author_name?: string | null;
  text?: string | null;
  thumbnailUrl?: string | null;
  thumbnail_url?: string | null;
  mediaType?: string | null;
  media_type?: string | null;
  timestamp?: string | null;
  thumbnailPath?: string | null;
  thumbnail_path?: string | null;
  imagePath?: string | null;
  image_path?: string | null;
  videoPath?: string | null;
  video_path?: string | null;
  media?: PostInputMedia[];
  // AI fields (undefined = not provided).
  aiDescription?: string;
  aiTags?: string[];
  aiStatus?: string;
  aiModel?: string;
  aiCategory?: string;
  aiContentType?: string;
  aiEntities?: string[];
  aiKeywords?: string[];
  aiLanguage?: string;
  aiSaveReason?: string;
  aiAnalyzedAt?: number;
  aiGeneralTags?: string[];
  aiSpecificTags?: string[];
  // Web-reference columns (camelCase or pre-serialized snake_case JSON).
  webUrl?: string | null;
  web_url?: string | null;
  webDomain?: string | null;
  web_domain?: string | null;
  webFinalUrl?: string | null;
  web_final_url?: string | null;
  webPalette?: unknown;
  web_palette_json?: unknown;
  webFonts?: unknown;
  web_fonts_json?: unknown;
  webTech?: unknown;
  web_tech_json?: unknown;
  webAwards?: unknown;
  web_awards_json?: unknown;
  webPages?: unknown;
  web_pages_json?: unknown;
  webMeta?: unknown;
  web_meta_json?: unknown;
  webCapturedAt?: number | null;
  web_captured_at?: number | null;
}

// A media entry as it arrives on a write-path post (parsers / web-ref mapper).
interface PostInputMedia {
  type?: string;
  url?: string | null;
  localPath?: string | null;
  local_path?: string | null;
}

// The snake_case parameter object postToRow produces for the INSERT/UPDATE
// prepared statements (named @-parameters).
interface PostRowParams {
  id: string;
  platform: string | undefined;
  shortcode: string | null;
  post_url: string | null;
  profile_url: string | null;
  author_username: string | null;
  author_name: string | null;
  text: string | null;
  thumbnail_url: string | null;
  media_type: string | null;
  timestamp: string | null;
  thumbnail_path: string | null;
  image_path: string | null;
  video_path: string | null;
  media_count: number;
  web_url: string | null;
  web_domain: string | null;
  web_final_url: string | null;
  web_palette_json: string | null;
  web_fonts_json: string | null;
  web_tech_json: string | null;
  web_awards_json: string | null;
  web_pages_json: string | null;
  web_meta_json: string | null;
  web_captured_at: number | null;
}

// The { type, url, localPath? } shape deriveMedia returns and replace/merge consume.
interface DerivedMedia {
  type: Shelfy.PostMediaType;
  url: string;
  localPath?: string | null;
}

// The AI-analysis fields applyAiAnalysis / updateAiAnalysis accept. Each key is
// optional: absent = leave the column untouched; explicit null = write NULL.
interface AiFields {
  description?: string | null;
  tags?: string[] | null;
  status?: string | null;
  model?: string | null;
  category?: string | null;
  contentType?: string | null;
  entities?: string[] | null;
  keywords?: string[] | null;
  language?: string | null;
  saveReason?: string | null;
  analyzedAt?: number | null;
  generalTags?: string[] | null;
  specificTags?: string[] | null;
}

// A { norm, form } pair from normalizeTagRows.
interface NormForm {
  norm: string;
  form: string;
}

// A canonical alias target ({ norm, form }) as stored in the in-memory alias map.
interface ResolvedAliasEntry {
  norm: string;
  form: string;
}

// One alias pair as passed to saveTagAliases (display forms optional).
interface AliasPair {
  aliasNorm?: string;
  aliasForm?: string;
  canonicalNorm?: string;
  canonicalForm?: string;
}

// One raw co-occurrence edge fed to the clustering core: tags a, b co-occur c times.
interface CoEdge {
  a: string;
  b: string;
  c: number;
}

// Options forwarded to buildTagCommunities (subset built by getTagCandidateGroups).
interface ClusterOpts {
  minJaccard: number;
  maxGroupSize: number;
  alpha: number;
}

// The serializable payload handed to the cluster worker over workerData.
interface ClusterWorkerPayload {
  freq: [string, number][];
  edges: CoEdge[];
  vecByNorm: [string, number[]][] | null;
  opts: ClusterOpts;
}

// Filters accepted by buildPostFilter / getPosts / getPostIds.
interface PostFilters {
  platform?: Shelfy.Platform | string;
  source?: 'all' | 'web' | 'social' | string;
  mediaType?: string;
  search?: string;
  missingOnly?: boolean;
  downloadStatus?: 'missing' | 'downloaded' | string;
  collectionId?: number;
  category?: string;
  contentType?: string;
  tag?: string;
  tags?: string[];
  tagMode?: 'and' | 'or' | string;
  entity?: string;
  analyzedStatus?: 'analyzed' | 'unanalyzed' | string;
  aiTagged?: 'tagged' | 'untagged' | string;
  concepts?: string[];
  conceptMode?: 'and' | 'or' | string;
  limit?: number;
  offset?: number;
  sortOrder?: 'newest' | 'oldest' | string;
}

// One match-unit's clause + score fragment, with the params for each.
interface MatchUnit {
  clause: string;
  clauseParams: string[];
  score: string;
  scoreParams: string[];
}

// The pieces buildPostFilter returns: the WHERE clause, its params, and an
// optional relevance expression (free-text search) with its own params.
interface PostFilterResult {
  where: string;
  filterParams: (string | number)[];
  relevanceExpr: string | null;
  relevanceParams: string[];
}

// The conditional embeddings module (lazy-required). Typed locally so the optional
// require() doesn't leak `any` into the clustering path.
interface EmbeddingsModule {
  isEmbeddingReady?: () => boolean;
  embedTexts?: (texts: string[]) => Promise<(number[] | null | undefined)[] | null | undefined>;
  cosineSim?: (a: number[], b: number[]) => number;
}

// A jobs-table upsert row (the in-memory queue managers hand these in).
interface JobUpsertRow {
  kind: Shelfy.JobKind | string;
  key: string;
  post_id?: string | null;
  payload?: string | null;
  status: string;
  progress?: number;
  error?: string | null;
  attempts?: number;
}

// A WebReference (§1.1 of spec 06) as consumed by webRefToPost. Open-ended where
// the capture data is, so only the fields the mapper reads are named.
interface WebReference {
  id?: string;
  url?: string;
  finalUrl?: string;
  domain?: string;
  title?: string;
  description?: string;
  lang?: string;
  capturedAt?: number;
  pages?: Shelfy.WebPage[];
  palette?: string[];
  fonts?: Shelfy.WebFont[];
  techStack?: string[];
  awards?: Shelfy.WebAward[];
  meta?: Shelfy.WebMeta;
  ai?: WebReferenceAi;
}

// The AI sub-object of a WebReference (snake_case, as produced by the analyzer).
interface WebReferenceAi {
  description?: string;
  tags?: string[];
  general_tags?: string[];
  specific_tags?: string[];
  entities?: string[];
  search_keywords?: string[];
  industry?: string;
  purpose?: string;
  language?: string;
  save_reason?: string;
  model?: string;
}

// A collection definition (export/import key carriers). Accepts both camelCase
// (export shape) and snake_case (raw row) for external_id, like collectionKey.
interface CollectionDef {
  name?: string;
  color?: string;
  platform?: Shelfy.Platform | string | null;
  externalId?: string | number | null;
  external_id?: string | number | null;
  igName?: string | null;
}

// A raw post record from a JSON export (carries the portable `collections` keys).
interface RawImportPost {
  id?: string | number;
  collections?: unknown[];
  [key: string]: unknown;
}

let db: DatabaseType | null = null;

const SCHEMA = `
CREATE TABLE IF NOT EXISTS posts (
  id TEXT PRIMARY KEY,
  platform TEXT NOT NULL,
  shortcode TEXT,
  post_url TEXT,
  profile_url TEXT,
  author_username TEXT,
  author_name TEXT,
  text TEXT,
  thumbnail_url TEXT,
  media_type TEXT,
  timestamp TEXT,
  thumbnail_path TEXT,
  image_path TEXT,
  video_path TEXT,
  media_count INTEGER DEFAULT 1,
  imported_at INTEGER DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_posts_platform ON posts(platform);
CREATE INDEX IF NOT EXISTS idx_posts_media_type ON posts(media_type);
CREATE INDEX IF NOT EXISTS idx_posts_timestamp ON posts(timestamp DESC);

-- One row per individual media item of a post (carousel slides, multi-image
-- tweets). position is the 0-based slide order; local_path is filled in by the
-- downloader and must survive re-imports.
CREATE TABLE IF NOT EXISTS post_media (
  post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  media_type TEXT NOT NULL,
  source_url TEXT,
  local_path TEXT,
  PRIMARY KEY (post_id, position)
);

CREATE INDEX IF NOT EXISTS idx_post_media_post ON post_media(post_id);

CREATE TABLE IF NOT EXISTS downloads (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  post_id TEXT REFERENCES posts(id) ON DELETE CASCADE,
  asset_type TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  progress REAL DEFAULT 0,
  error TEXT,
  started_at INTEGER,
  completed_at INTEGER
);

-- Persisted mirror of the in-memory background queues (downloads, AI tagging,
-- web scans). Each manager's setJob() upserts here on every state transition so
-- the queue survives an app restart; at boot recover() re-enqueues the rows that
-- weren't terminal. payload holds a compact JSON of the job record (enough to
-- rebuild/resume it); rich UI fields (event timelines, page lists) are stripped
-- before serialisation. Not tied to posts(id) by FK: web jobs are keyed by URL
-- and may outlive a placeholder, and a stale row simply finds no post on resume.
CREATE TABLE IF NOT EXISTS jobs (
  kind       TEXT NOT NULL,             -- 'download' | 'analyze' | 'web'
  key        TEXT NOT NULL,             -- the manager's jobKey
  post_id    TEXT,
  payload    TEXT,                      -- JSON: compact job record for resume
  status     TEXT NOT NULL,
  progress   REAL DEFAULT 0,
  error      TEXT,
  attempts   INTEGER DEFAULT 0,
  created_at INTEGER DEFAULT (unixepoch()),
  updated_at INTEGER DEFAULT (unixepoch()),
  PRIMARY KEY (kind, key)
);

CREATE INDEX IF NOT EXISTS idx_jobs_kind_status ON jobs(kind, status);

-- User-defined custom sources ("collections"): named, colored buckets a post
-- can be filed into. A post can belong to many collections (many-to-many via
-- post_collections).
CREATE TABLE IF NOT EXISTS collections (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  color TEXT NOT NULL DEFAULT '#3d5afe',
  -- platform: 'instagram' for tags auto-derived from an Instagram saved folder
  -- (these nest under Instagram in the sidebar); NULL for manual sources.
  -- external_id: the source folder id (the numeric segment in the IG saved-collection
  -- URL), so re-importing the same folder reuses the tag instead of duplicating it,
  -- even after the user renames it. ig_name: the folder's ORIGINAL Instagram name,
  -- kept so we can tell whether the user renamed the tag and propose the right default.
  platform TEXT,
  external_id TEXT,
  ig_name TEXT,
  created_at INTEGER DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS post_collections (
  post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
  added_at INTEGER DEFAULT (unixepoch()),
  PRIMARY KEY (post_id, collection_id)
);

CREATE INDEX IF NOT EXISTS idx_post_collections_collection ON post_collections(collection_id);

-- Derived index over the JSON ai_tags array: one row per (post, normalized tag).
-- ai_tags on posts stays the SOURCE OF TRUTH; these rows are rebuilt from it in
-- updateAiAnalysis and backfilled in migrate, enabling indexed tag aggregation
-- and filtering instead of scanning + JSON.parse-ing every post.
CREATE TABLE IF NOT EXISTS post_tags (
  post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  tag_norm TEXT NOT NULL,
  tag_form TEXT NOT NULL,
  PRIMARY KEY (post_id, tag_norm)
);

CREATE INDEX IF NOT EXISTS idx_post_tags_norm ON post_tags(tag_norm);

-- Canonicalizzazione sinonimi: mappa una forma sinonima (alias_norm) sulla forma
-- canonica scelta. Invariante: canonical_norm NON deve a sua volta essere un alias
-- (niente catene) — saveTagAliases risolve sempre alla radice prima di inserire.
-- post_tags resta una derivazione idempotente di ai_tags mappata su questi alias;
-- ai_tags (source of truth) non viene mai toccato dalla canonicalizzazione.
-- status guida la review (come i cluster): 'proposed' (generato, NON ancora applicato
-- a post_tags) / 'accepted' (applicato). SOLO gli 'accepted' canonicalizzano i tag
-- (resolveAlias/getAliasMap li filtrano). Dismiss = riga eliminata.
CREATE TABLE IF NOT EXISTS tag_alias (
  alias_norm     TEXT PRIMARY KEY,   -- forma sinonima (lowercase)
  canonical_norm TEXT NOT NULL,      -- forma canonica scelta (lowercase)
  canonical_form TEXT NOT NULL,      -- display form della canonica
  status         TEXT NOT NULL DEFAULT 'accepted'  -- 'proposed' | 'accepted'
);

-- Derived index over the JSON ai_entities array, mirroring post_tags.
CREATE TABLE IF NOT EXISTS post_entities (
  post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  ent_norm TEXT NOT NULL,
  ent_form TEXT NOT NULL,
  PRIMARY KEY (post_id, ent_norm)
);

CREATE INDEX IF NOT EXISTS idx_post_entities_norm ON post_entities(ent_norm);

-- Persisted semantic tag clusters suggested by the local model (hybrid pipeline:
-- co-occurrence candidates → LLM naming/splitting). label is the model-given name.
-- status drives the review UX: 'proposed' (just generated) / 'accepted' (kept by
-- the user). Dismissed clusters are deleted outright. A regenerate run clears the
-- previous 'proposed' rows; 'accepted' rows and their tags are preserved.
CREATE TABLE IF NOT EXISTS tag_cluster (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  label TEXT NOT NULL,
  label_norm TEXT,
  status TEXT NOT NULL DEFAULT 'proposed',
  run_id INTEGER,
  created_at INTEGER DEFAULT (unixepoch()),
  updated_at INTEGER DEFAULT (unixepoch())
);

-- Membership is keyed by tag_norm: a tag belongs to at most one cluster (mirrors
-- the old union-find's one-tag-one-component invariant).
CREATE TABLE IF NOT EXISTS tag_cluster_membership (
  tag_norm TEXT PRIMARY KEY,
  cluster_id INTEGER NOT NULL REFERENCES tag_cluster(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_tag_cluster_membership_cluster ON tag_cluster_membership(cluster_id);
`;

function initialize(): DatabaseType {
  if (db) return db;

  const userDataPath = app.getPath('userData');
  const dbPath = path.join(userDataPath, 'shelfy.sqlite');

  const handle = new Database(dbPath);
  try {
    handle.pragma('journal_mode = WAL');
    handle.pragma('foreign_keys = ON');
    handle.exec(SCHEMA);
    migrate(handle);
    registerSqlFunctions(handle);
  } catch (err) {
    // Don't leave a half-initialized handle in `db`: a later initialize() would
    // return it (because `if (db) return db`) and operate on a broken schema.
    try {
      handle.close();
    } catch {}
    throw err;
  }

  db = handle;
  return db;
}

// Clean shutdown: checkpoint the WAL into the main db file and close the handle
// so the next initialize() opens fresh. Safe to call when not initialized.
function close(): void {
  if (!db) return;
  try {
    db.pragma('wal_checkpoint(TRUNCATE)');
  } catch {}
  try {
    db.close();
  } catch {}
  db = null;
  invalidateGlobalCaches();
}

// Whole-word matching for relevance scoring: returns 1 when `needle` appears in
// `haystack` bounded by non-alphanumeric characters (so "design" matches but
// "designer" does not). Used to rank exact-word hits above mere substring hits.
// Regexes are cached by needle — query terms repeat across every scanned row, so
// the cache is hit ~100% of the time and the per-row cost stays tiny.
const _wordReCache = new Map<string, RegExp | null>();
function wordMatchFn(haystack: unknown, needle: unknown): number {
  if (haystack == null || needle == null || needle === '') return 0;
  const key = String(needle);
  let re = _wordReCache.get(key);
  if (re === undefined) {
    const esc = key.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    try {
      re = new RegExp(`(?<![\\p{L}\\p{N}])${esc}(?![\\p{L}\\p{N}])`, 'iu');
    } catch {
      re = null;
    }
    if (_wordReCache.size > 500) _wordReCache.clear();
    _wordReCache.set(key, re);
  }
  return re && re.test(String(haystack)) ? 1 : 0;
}

function registerSqlFunctions(database: DatabaseType): void {
  database.function(
    'word_match',
    { deterministic: true },
    wordMatchFn as (...args: unknown[]) => number,
  );
}

// Schema/data migration version. Bump when adding a new gated migration step
// below. PRAGMA user_version persists in the db file, so the expensive
// full-table repairs run ONCE (on first boot after upgrade) instead of on
// every launch. The cheap ADD COLUMN checks stay unconditional (they're guarded
// by their own PRAGMA table_info check and are effectively free).
const SCHEMA_VERSION = 3;

function migrate(db: DatabaseType): void {
  const userVersion = db.pragma('user_version', { simple: true }) as number;

  // Add media_count to databases created before multi-media support.
  const cols = db.prepare('PRAGMA table_info(posts)').all() as { name: string }[];
  if (!cols.some((c) => c.name === 'media_count')) {
    db.exec('ALTER TABLE posts ADD COLUMN media_count INTEGER DEFAULT 1');
  }

  // Add AI analysis columns (local VLM categorization: description + tags).
  const aiCols: Record<string, string> = {
    ai_description: 'TEXT',
    ai_tags: 'TEXT', // JSON array of strings
    ai_status: 'TEXT',
    ai_model: 'TEXT',
    ai_analyzed_at: 'INTEGER',
    ai_category: 'TEXT',
    ai_content_type: 'TEXT',
    ai_entities: 'TEXT', // JSON array of strings
    ai_keywords: 'TEXT', // JSON array of strings
    ai_language: 'TEXT',
    ai_save_reason: 'TEXT',
  };
  for (const [name, type] of Object.entries(aiCols)) {
    if (!cols.some((c) => c.name === name)) {
      db.exec(`ALTER TABLE posts ADD COLUMN ${name} ${type}`);
    }
  }

  // Index ai_status: used by analysis-queue filters and the "unanalyzed" count.
  // Created here (not in SCHEMA) because ai_status is itself an ADD COLUMN, so it
  // doesn't exist on a fresh DB until the loop above runs. Idempotent.
  db.exec('CREATE INDEX IF NOT EXISTS idx_posts_ai_status ON posts(ai_status)');

  // User-authored layer, kept distinct from the AI-generated fields so it survives
  // an analysis regeneration (which only touches the ai_* columns / non-manual
  // post_tags rows): a free-text personal note and the user's own manual tags.
  // user_tags is the display source of truth (JSON array); it is also mirrored
  // into post_tags with tier='manual' so manual tags are first-class in tag
  // filtering and chat-search vocabulary. Same guarded ADD COLUMN pattern as
  // aiCols — NULLABLE, retro-compatible, effectively free on every boot.
  const userCols: Record<string, string> = {
    user_note: 'TEXT', // free-text personal note authored by the user
    user_tags: 'TEXT', // JSON array of manual tags (mirrored to post_tags tier='manual')
  };
  for (const [name, type] of Object.entries(userCols)) {
    if (!cols.some((c) => c.name === name)) {
      db.exec(`ALTER TABLE posts ADD COLUMN ${name} ${type}`);
    }
  }

  // Add web-reference columns for the "websites as references" feature: a site is
  // a posts row with platform='web', media_type='website'. These hold the raw/
  // canonical URLs and the JSON-serialized design signals (palette/fonts/tech/
  // awards), per-page metadata, and the capture epoch. All NULLABLE — social posts
  // leave them NULL, so this is fully retro-compatible. Same guarded ADD COLUMN
  // pattern as aiCols above (reuses the same `cols` PRAGMA read); no full-repair,
  // no SCHEMA_VERSION bump.
  const webCols: Record<string, string> = {
    web_url: 'TEXT', // raw URL as pasted by the user
    web_domain: 'TEXT', // normalized hostname without www. (== author_username)
    web_final_url: 'TEXT', // canonical URL after redirects (basis of the id)
    web_palette_json: 'TEXT', // JSON array of HEX colors
    web_fonts_json: 'TEXT', // JSON array of { family, usage? }
    web_tech_json: 'TEXT', // JSON array of tech-stack strings
    web_awards_json: 'TEXT', // JSON array of { platform, level?, date?, profileUrl? }
    web_pages_json: 'TEXT', // JSON array of per-page metadata { url, pageType?, title?, meta?, jsonld? }
    web_meta_json: 'TEXT', // JSON object of hero/site-level meta (description, ogImage, lang, …)
    web_captured_at: 'INTEGER', // epoch SECONDS of the capture (source of timestamp)
  };
  for (const [name, type] of Object.entries(webCols)) {
    if (!cols.some((c) => c.name === name)) {
      db.exec(`ALTER TABLE posts ADD COLUMN ${name} ${type}`);
    }
  }
  // Index the domain for fast per-site lookups / dedup (F8/F9). Idempotent.
  db.exec('CREATE INDEX IF NOT EXISTS idx_posts_web_domain ON posts(web_domain)');

  // Blur-up placeholder for the gallery tile: a ~24px JPEG data URI generated
  // from the post's local cover (thumbs.js), shipped inside getPosts so a card
  // paints a recognizable preview the frame it mounts. '' is the "tried,
  // ineligible" sentinel (undecodable cover) so failures aren't rescanned on
  // every boot; NULL means "not generated yet". Guarded ADD COLUMN — free.
  if (!cols.some((c) => c.name === 'thumb_blur')) {
    db.exec('ALTER TABLE posts ADD COLUMN thumb_blur TEXT');
  }

  // Web snapshots: each re-capture of a site archives the PREVIOUS state of its
  // posts row here (one row per past version, dated by captured_at), so the
  // panel can offer a version history. The posts row always holds the CURRENT
  // (latest) snapshot; web_snapshots holds only the older ones. ON DELETE CASCADE
  // removes a site's archived versions when the site itself is deleted (the files
  // on disk are cleaned separately — see getWebSiteFilePaths). Idempotent.
  db.exec(`
    CREATE TABLE IF NOT EXISTS web_snapshots (
      id              INTEGER PRIMARY KEY AUTOINCREMENT,
      post_id         TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
      captured_at     INTEGER NOT NULL,
      title           TEXT,
      web_pages_json  TEXT,
      web_palette_json TEXT,
      web_fonts_json  TEXT,
      web_tech_json   TEXT,
      web_awards_json TEXT,
      web_meta_json   TEXT,
      ai_description  TEXT,
      ai_tags_json    TEXT,
      ai_model        TEXT,
      ai_status       TEXT,
      ai_analyzed_at  INTEGER,
      ai_category     TEXT,
      ai_content_type TEXT,
      ai_entities_json TEXT,
      ai_keywords_json TEXT,
      ai_language     TEXT,
      ai_save_reason  TEXT,
      created_at      INTEGER NOT NULL
    )
  `);
  db.exec(
    'CREATE INDEX IF NOT EXISTS idx_web_snapshots_post ON web_snapshots(post_id, captured_at DESC)',
  );

  // Add folder-tag columns to collections created before Instagram-folder import:
  // platform groups them under their platform in the sidebar, external_id links a
  // tag back to its source IG folder (rename-safe), ig_name keeps the folder's
  // original name. Cheap, guarded ADD COLUMN — effectively free on every boot.
  const colCols = db.prepare('PRAGMA table_info(collections)').all() as { name: string }[];
  const collectionCols: Record<string, string> = {
    platform: 'TEXT',
    external_id: 'TEXT',
    ig_name: 'TEXT',
  };
  for (const [name, type] of Object.entries(collectionCols)) {
    if (!colCols.some((c) => c.name === name)) {
      db.exec(`ALTER TABLE collections ADD COLUMN ${name} ${type}`);
    }
  }

  // Add post_tags.tier: 'general'|'specific'|NULL. Popolato da applyAiAnalysis
  // quando l'analisi fornisce generalTags/specificTags; i tag legacy (e gli edit
  // manuali senza tier) restano NULL. Nessun full-repair: NULL = comportamento
  // pre-tier. Guarded ADD COLUMN — di fatto gratis ad ogni boot.
  const ptCols = db.prepare('PRAGMA table_info(post_tags)').all() as { name: string }[];
  if (!ptCols.some((c) => c.name === 'tier')) {
    db.exec('ALTER TABLE post_tags ADD COLUMN tier TEXT');
  }

  // Add tag_alias.status: 'proposed' (da rivedere, NON applicato a post_tags) /
  // 'accepted' (applicato). Le righe esistenti restano 'accepted' (DEFAULT) per
  // retro-compat: prima del review-flow ogni alias salvato era già applicato.
  // Guarded ADD COLUMN — di fatto gratis ad ogni boot.
  const aliasCols = db.prepare('PRAGMA table_info(tag_alias)').all() as { name: string }[];
  if (!aliasCols.some((c) => c.name === 'status')) {
    db.exec("ALTER TABLE tag_alias ADD COLUMN status TEXT NOT NULL DEFAULT 'accepted'");
  }

  // ── Full-table repairs: gated behind user_version so they run ONCE, not on
  // every boot. All steps below are idempotent; the gate is purely an
  // optimization to avoid full scans on every launch of a large archive. ──
  if (userVersion < 1) {
    // Repair Twitter post URLs saved before the empty-author fix: an absent
    // author produced `https://x.com//status/<id>`, which yt-dlp rejects. The
    // `i` placeholder is a valid username for yt-dlp's extractor and the
    // canonical URL.
    db.prepare(
      "UPDATE posts SET post_url = 'https://x.com/i/status/' || id " +
        "WHERE platform = 'twitter' AND post_url LIKE 'https://x.com//status/%'",
    ).run();

    // Backfill post_media for posts that predate the table. Each gets a single
    // position-0 entry derived from its existing columns; text-only posts (no
    // thumbnail and no local file) are left without media. Idempotent: only
    // touches posts that have no post_media rows yet.
    db.prepare(
      `
      INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
      SELECT
        p.id,
        0,
        CASE WHEN p.media_type = 'video' THEN 'video' ELSE 'image' END,
        p.thumbnail_url,
        COALESCE(p.image_path, p.thumbnail_path, p.video_path)
      FROM posts p
      WHERE NOT EXISTS (SELECT 1 FROM post_media pm WHERE pm.post_id = p.id)
        AND (p.thumbnail_url IS NOT NULL
             OR p.image_path IS NOT NULL
             OR p.thumbnail_path IS NOT NULL
             OR p.video_path IS NOT NULL)
    `,
    ).run();

    // Backfill the derived post_tags / post_entities indexes from the existing
    // JSON columns. Idempotent: only touches posts that have ai_tags/ai_entities
    // but no derived rows yet, and uses INSERT OR IGNORE so a re-run is a no-op.
    backfillDerivedTags(db);
  }

  if (userVersion < 2) {
    // Backfill the post date for Instagram posts that landed without one — e.g.
    // posts manually selected from the lightweight grid GraphQL nodes, which carry
    // a shortcode but no taken_at. Derived from the shortcode (see igDateFromShortcode),
    // so date display and date-sorting stop treating them as the oldest posts.
    backfillInstagramTimestamps(db);
  }

  if (userVersion < 3) {
    // Backfill the post date for any remaining undated posts ('' or NULL
    // timestamp) — in practice all Pinterest pins (their saved-pins RPC carries
    // no usable created_at) plus any IG post whose shortcode can't be decoded.
    // Undated posts sorted below 2016-era content in 'newest' and ABOVE
    // everything in 'oldest' ('' < any ISO string). IG rows get the better
    // shortcode-derived date first; the rest fall back to their import time
    // (same policy bulkUpsert now applies at insert). Format matches
    // Date#toISOString so the TEXT column keeps sorting lexicographically.
    backfillInstagramTimestamps(db);
    db.prepare(
      "UPDATE posts SET timestamp = strftime('%Y-%m-%dT%H:%M:%fZ', imported_at, 'unixepoch') " +
        "WHERE (timestamp IS NULL OR timestamp = '') AND imported_at IS NOT NULL",
    ).run();
  }

  if (userVersion < SCHEMA_VERSION) {
    db.pragma(`user_version = ${SCHEMA_VERSION}`);
  }
}

// One-time repair: fill `timestamp` for Instagram posts that have a usable
// shortcode but no date. Idempotent (only touches empty timestamps).
function backfillInstagramTimestamps(db: DatabaseType): void {
  const rows = db
    .prepare(
      "SELECT id, shortcode FROM posts WHERE platform = 'instagram' " +
        "AND (timestamp IS NULL OR timestamp = '') AND shortcode IS NOT NULL AND shortcode != ''",
    )
    .all() as { id: string; shortcode: string }[];
  if (!rows.length) return;
  const upd = db.prepare('UPDATE posts SET timestamp = ? WHERE id = ?');
  const tx = db.transaction(() => {
    for (const r of rows) {
      const iso = igDateFromShortcode(r.shortcode);
      if (iso) upd.run(iso, r.id);
    }
  });
  tx();
}

// Rebuild post_tags / post_entities from posts.ai_tags / posts.ai_entities for
// any post that has the JSON populated but no derived rows. Safe to re-run.
function backfillDerivedTags(db: DatabaseType): void {
  const tagPosts = db
    .prepare(
      `
      SELECT p.id, p.ai_tags FROM posts p
      WHERE p.ai_tags IS NOT NULL
        AND NOT EXISTS (SELECT 1 FROM post_tags t WHERE t.post_id = p.id)
    `,
    )
    .all() as { id: string; ai_tags: string | null }[];
  const entPosts = db
    .prepare(
      `
      SELECT p.id, p.ai_entities FROM posts p
      WHERE p.ai_entities IS NOT NULL
        AND NOT EXISTS (SELECT 1 FROM post_entities e WHERE e.post_id = p.id)
    `,
    )
    .all() as { id: string; ai_entities: string | null }[];

  if (!tagPosts.length && !entPosts.length) return;

  const insTag = db.prepare(
    'INSERT OR IGNORE INTO post_tags (post_id, tag_norm, tag_form) VALUES (?, ?, ?)',
  );
  const insEnt = db.prepare(
    'INSERT OR IGNORE INTO post_entities (post_id, ent_norm, ent_form) VALUES (?, ?, ?)',
  );

  const tx = db.transaction(() => {
    for (const row of tagPosts) {
      for (const { norm, form } of normalizeTagRows(row.ai_tags)) {
        insTag.run(row.id, norm, form);
      }
    }
    for (const row of entPosts) {
      for (const { norm, form } of normalizeTagRows(row.ai_entities)) {
        insEnt.run(row.id, norm, form);
      }
    }
  });
  tx();
}

// A raw `posts` table row (snake_case), as SELECT * returns it. Columns added by
// migrations are nullable; counts/epochs are numbers.
interface PostRow {
  id: string;
  platform: Shelfy.Platform;
  shortcode: string | null;
  post_url: string | null;
  profile_url: string | null;
  author_username: string | null;
  author_name: string | null;
  text: string | null;
  thumbnail_url: string | null;
  media_type: Shelfy.MediaType | null;
  timestamp: string | null;
  thumbnail_path: string | null;
  image_path: string | null;
  video_path: string | null;
  media_count: number | null;
  imported_at: number;
  thumb_blur: string | null;
  ai_description: string | null;
  ai_tags: string | null;
  ai_status: Shelfy.AiStatus | null;
  ai_model: string | null;
  ai_analyzed_at: number | null;
  ai_category: string | null;
  ai_content_type: string | null;
  ai_entities: string | null;
  ai_keywords: string | null;
  ai_language: string | null;
  ai_save_reason: string | null;
  user_note: string | null;
  user_tags: string | null;
  web_url: string | null;
  web_domain: string | null;
  web_final_url: string | null;
  web_palette_json: string | null;
  web_fonts_json: string | null;
  web_tech_json: string | null;
  web_awards_json: string | null;
  web_pages_json: string | null;
  web_meta_json: string | null;
  web_captured_at: number | null;
}

function rowToPost(row: PostRow | undefined): Shelfy.Post | null {
  if (!row) return null;
  const webMeta = parseJson<Shelfy.WebMeta>(row.web_meta_json, null);
  return {
    id: row.id,
    platform: row.platform,
    shortcode: row.shortcode,
    postUrl: row.post_url,
    profileUrl: row.profile_url,
    authorUsername: row.author_username,
    authorName: row.author_name,
    text: row.text,
    thumbnailUrl: row.thumbnail_url,
    mediaType: row.media_type,
    timestamp: row.timestamp,
    thumbnailPath: row.thumbnail_path,
    imagePath: row.image_path,
    videoPath: row.video_path,
    // '' is the backfill's "ineligible" sentinel — the renderer wants null.
    thumbBlur: row.thumb_blur || null,
    mediaCount: row.media_count ?? 1,
    importedAt: row.imported_at,
    aiDescription: row.ai_description ?? null,
    aiTags: parseTags(row.ai_tags),
    aiStatus: row.ai_status ?? null,
    aiModel: row.ai_model ?? null,
    aiAnalyzedAt: row.ai_analyzed_at ?? null,
    aiCategory: row.ai_category ?? null,
    aiContentType: row.ai_content_type ?? null,
    aiEntities: parseTags(row.ai_entities),
    aiKeywords: parseTags(row.ai_keywords),
    aiLanguage: row.ai_language ?? null,
    aiSaveReason: row.ai_save_reason ?? null,
    // User-authored layer (independent of the AI fields, survives regeneration).
    userNote: row.user_note ?? null,
    userTags: parseTags(row.user_tags),
    // Web-reference fields, parsed back to camelCase for the UI (F8). All NULL
    // for social posts → these are null/[]. Defensive parse: bad JSON → null.
    webUrl: row.web_url ?? null,
    webDomain: row.web_domain ?? null,
    webFinalUrl: row.web_final_url ?? null,
    webPalette: parseJson<string[]>(row.web_palette_json, []) ?? [],
    webFonts: parseJson<Shelfy.WebFont[]>(row.web_fonts_json, []) ?? [],
    webTech: parseJson<string[]>(row.web_tech_json, []) ?? [],
    webAwards: parseJson<Shelfy.WebAward[]>(row.web_awards_json, []) ?? [],
    webPages: parseJson<Shelfy.WebPage[]>(row.web_pages_json, []) ?? [],
    webMeta,
    // Single-page captures persist the flag inside web_meta_json (see the
    // orchestrator's ref.meta) so a "reanalyze" can replay the same mode.
    webSinglePage: !!(webMeta && webMeta.singlePage),
    webCapturedAt: row.web_captured_at ?? null,
  };
}

// ai_tags is stored as a JSON array string; tolerate null/legacy/garbage.
function parseTags(raw: string | null | undefined): string[] {
  if (!raw) return [];
  try {
    const v: unknown = JSON.parse(raw);
    return Array.isArray(v) ? (v as string[]) : [];
  } catch {
    return [];
  }
}

// Defensive JSON parse for web_*_json TEXT columns. null/empty → fallback;
// invalid JSON → fallback (never throws). `fallback` lets callers pick [] for
// array columns or null for the meta object.
function parseJson<T>(raw: string | null | undefined, fallback: T | null = null): T | null {
  if (raw == null || raw === '') return fallback;
  try {
    const v: unknown = JSON.parse(raw);
    return v == null ? fallback : (v as T);
  } catch {
    return fallback;
  }
}

// Turn a JSON array (raw string or array) of tags/entities into deduped
// { norm, form } rows for the derived index. norm = trim().toLowerCase()
// (case-insensitive, diacritics PRESERVED to match getTagStats grouping);
// form = the original trimmed string. Empty entries are skipped, and only the
// first form for a given norm is kept (one row per (post, norm)).
function normalizeTagRows(raw: unknown): NormForm[] {
  const arr = Array.isArray(raw) ? raw : parseTags(raw as string | null | undefined);
  const out: NormForm[] = [];
  const seen = new Set<string>();
  for (const t of arr) {
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

// Derive the ordered media list to persist for a post. Prefers an explicit
// `media` array (from the parsers); otherwise synthesizes a single entry from
// the thumbnail so single-media posts still get a post_media row.
function deriveMedia(post: PostInput): DerivedMedia[] {
  if (Array.isArray(post.media) && post.media.length > 0) {
    // localPath is propagated when the source already has the file on disk (web
    // screenshots: F6). Social posts don't set it → stays undefined → persisted
    // as NULL by replace/mergePostMedia, i.e. unchanged behavior for them.
    return post.media
      .map((m) => ({
        type: (m.type === 'video' ? 'video' : 'image') as Shelfy.PostMediaType,
        url: m.url || '',
        localPath: m.localPath ?? m.local_path ?? undefined,
      }))
      .filter((m) => m.url);
  }
  const url = post.thumbnailUrl ?? post.thumbnail_url;
  if (url && (post.mediaType ?? post.media_type) !== 'text') {
    return [
      {
        type: ((post.mediaType ?? post.media_type) === 'video'
          ? 'video'
          : 'image') as Shelfy.PostMediaType,
        url,
      },
    ];
  }
  return [];
}

// Run `SELECT ... WHERE col IN (<ids>)` in chunks of <=500 so we never blow past
// SQLite's ~999-variable limit, concatenating the rows from every chunk. `build`
// gets the comma-separated placeholder string for the chunk and returns the SQL.
function chunkedInQuery<R>(
  ids: (string | number)[],
  build: (placeholders: string) => string,
  batch = 500,
): R[] {
  const out: R[] = [];
  for (let i = 0; i < ids.length; i += batch) {
    const chunk = ids.slice(i, i + batch);
    const placeholders = chunk.map(() => '?').join(',');
    out.push(...(db!.prepare(build(placeholders)).all(...chunk) as R[]));
  }
  return out;
}

// Attach the ordered media array to a list of posts in one batched query.
function attachMedia<P extends { id: string; media?: Shelfy.PostMedia[] }>(posts: P[]): P[] {
  if (!posts.length) return posts;
  const ids = posts.map((p) => p.id);
  const rows = chunkedInQuery<{
    post_id: string;
    position: number;
    media_type: Shelfy.PostMediaType;
    source_url: string | null;
    local_path: string | null;
  }>(
    ids,
    (ph) => `SELECT post_id, position, media_type, source_url, local_path
       FROM post_media WHERE post_id IN (${ph}) ORDER BY post_id, position`,
  );

  const byPost = new Map<string, Shelfy.PostMedia[]>();
  for (const r of rows) {
    if (!byPost.has(r.post_id)) byPost.set(r.post_id, []);
    byPost.get(r.post_id)!.push({
      position: r.position,
      type: r.media_type,
      url: r.source_url,
      localPath: r.local_path,
    });
  }
  for (const p of posts) p.media = byPost.get(p.id) || [];
  return posts;
}

// Attach the list of collection ids each post belongs to, in one batched query.
function attachCollections<P extends { id: string; collectionIds?: number[] }>(posts: P[]): P[] {
  if (!posts.length) return posts;
  const ids = posts.map((p) => p.id);
  const rows = chunkedInQuery<{ post_id: string; collection_id: number }>(
    ids,
    (ph) => `SELECT post_id, collection_id FROM post_collections WHERE post_id IN (${ph})`,
  );

  const byPost = new Map<string, number[]>();
  for (const r of rows) {
    if (!byPost.has(r.post_id)) byPost.set(r.post_id, []);
    byPost.get(r.post_id)!.push(r.collection_id);
  }
  for (const p of posts) p.collectionIds = byPost.get(p.id) || [];
  return posts;
}

function postToRow(post: PostInput): PostRowParams {
  return {
    id: post.id,
    platform: post.platform,
    shortcode: post.shortcode ?? null,
    post_url: post.postUrl ?? post.post_url ?? null,
    profile_url: post.profileUrl ?? post.profile_url ?? null,
    author_username: post.authorUsername ?? post.author_username ?? null,
    author_name: post.authorName ?? post.author_name ?? null,
    text: post.text ?? null,
    thumbnail_url: post.thumbnailUrl ?? post.thumbnail_url ?? null,
    media_type: post.mediaType ?? post.media_type ?? null,
    timestamp: post.timestamp ?? null,
    thumbnail_path: post.thumbnailPath ?? post.thumbnail_path ?? null,
    image_path: post.imagePath ?? post.image_path ?? null,
    video_path: post.videoPath ?? post.video_path ?? null,
    media_count: deriveMedia(post).length || 1,
    // Web-reference columns (NULL for social posts). Accept camelCase or snake.
    // Arrays/objects are JSON-serialized; scalars pass through. Param presence in
    // the INSERT/updateMeta with all-NULL values is a no-op for non-web rows.
    web_url: post.webUrl ?? post.web_url ?? null,
    web_domain: post.webDomain ?? post.web_domain ?? null,
    web_final_url: post.webFinalUrl ?? post.web_final_url ?? null,
    web_palette_json: serializeJson(post.webPalette ?? post.web_palette_json),
    web_fonts_json: serializeJson(post.webFonts ?? post.web_fonts_json),
    web_tech_json: serializeJson(post.webTech ?? post.web_tech_json),
    web_awards_json: serializeJson(post.webAwards ?? post.web_awards_json),
    web_pages_json: serializeJson(post.webPages ?? post.web_pages_json),
    web_meta_json: serializeJson(post.webMeta ?? post.web_meta_json),
    web_captured_at: post.webCapturedAt ?? post.web_captured_at ?? null,
  };
}

// Serialize a value destined for a *_json TEXT column. null/undefined → null;
// strings pass through verbatim (already-serialized JSON or snake_case input);
// arrays/objects are JSON.stringify'd. Keeps postToRow tolerant of both the
// camelCase (array/object) and snake_case (string) shapes, like the ?? chains.
function serializeJson(v: unknown): string | null {
  if (v == null) return null;
  if (typeof v === 'string') return v;
  try {
    return JSON.stringify(v);
  } catch {
    return null;
  }
}

// Escape LIKE wildcards (`%`, `_`) and the escape char itself so user-supplied
// values match literally instead of acting as wildcards. Pair with ESCAPE '\'.
function likeEscape(s: unknown): string {
  return String(s ?? '').replace(/[\\%_]/g, (c) => '\\' + c);
}

// Curated IT + EN stopword / query-boilerplate set. The goal is to strip function
// words and conversational filler ("vorrei trovare delle reference di …") so only
// CONTENT terms survive tokenization. Without this, substring LIKE on words like
// "per"/"come"/"di" matches imPERmanenza, suPERsplat, etc. and drowns out the real
// content terms. Kept deliberately tight: we drop only clear noise, never plausible
// content words (e.g. "design", "tipografia", "cuffie" are NOT here).
const SEARCH_STOPWORDS = new Set<string>([
  // ── Italian articles / prepositions / conjunctions / pronouns ──
  'di',
  'a',
  'da',
  'in',
  'con',
  'su',
  'per',
  'tra',
  'fra',
  'e',
  'ed',
  'o',
  'od',
  'ma',
  'se',
  'né',
  'ne',
  'il',
  'lo',
  'la',
  'i',
  'gli',
  'le',
  'un',
  'uno',
  'una',
  'del',
  'dello',
  'della',
  'dei',
  'degli',
  'delle',
  'dal',
  'dallo',
  'dalla',
  'dai',
  'dagli',
  'dalle',
  'al',
  'allo',
  'alla',
  'ai',
  'agli',
  'alle',
  'nel',
  'nello',
  'nella',
  'nei',
  'negli',
  'nelle',
  'sul',
  'sullo',
  'sulla',
  'sui',
  'sugli',
  'sulle',
  'col',
  'coi',
  'che',
  'chi',
  'cui',
  'come',
  'dove',
  'quando',
  'quale',
  'quali',
  'quanto',
  'più',
  'meno',
  'molto',
  'poco',
  'tanto',
  'tutto',
  'tutti',
  'tutte',
  'ad',
  'è',
  'sono',
  'sia',
  'essere',
  'avere',
  'fare',
  'questo',
  'questa',
  'questi',
  'queste',
  'quello',
  'quella',
  'quelli',
  'quelle',
  'mio',
  'mia',
  'miei',
  'mie',
  'non',
  'anche',
  'già',
  'ancora',
  'poi',
  'qui',
  'qua',
  'lì',
  'là',
  // ── Italian query-boilerplate verbs / nouns ──
  'devo',
  'voglio',
  'vorrei',
  'cerco',
  'cerca',
  'cercare',
  'cercando',
  'trovo',
  'trova',
  'trovare',
  'trovando',
  'mostra',
  'mostrami',
  'dammi',
  'vedere',
  'avere',
  'reference',
  'references',
  'esempio',
  'esempi',
  'tipo',
  'tipi',
  'qualche',
  'alcuni',
  'alcune',
  'relativo',
  'relativa',
  'relativi',
  'relative',
  'riguardo',
  'riguardante',
  'circa',
  'simile',
  'simili',
  'qualcosa',
  'cosa',
  'cose',
  'roba',
  // ── English articles / prepositions / conjunctions ──
  'the',
  'an',
  'of',
  'for',
  'to',
  'with',
  'and',
  'or',
  'but',
  'on',
  'at',
  'by',
  'as',
  'is',
  'are',
  'be',
  'this',
  'that',
  'these',
  'those',
  'it',
  'its',
  // ── English query-boilerplate ──
  'find',
  'search',
  'show',
  'give',
  'me',
  'my',
  'want',
  'wants',
  'need',
  'needs',
  'like',
  'some',
  'any',
  'few',
  'example',
  'examples',
  'sample',
  'samples',
  'about',
  'related',
  'please',
  'looking',
  'look',
  'get',
  'something',
  'thing',
  'things',
  'stuff',
  'kind',
  'sort',
]);

// Short tokens are usually noise ("di", "ad") but a few are genuine content
// ("3d", "ai", "ux", "ui"). Whitelist those so the minLen filter keeps them.
const SHORT_CONTENT_TERMS = new Set<string>([
  '3d',
  '2d',
  'ai',
  'ar',
  'vr',
  'xr',
  'ux',
  'ui',
  'cg',
  'r',
  'go',
]);

// Extract only meaningful CONTENT terms from a free-text query: lowercase, split
// on non-alphanumerics, drop stopwords/boilerplate, drop too-short tokens (unless
// whitelisted), de-dupe. Shared by every query tokenizer so the same notion of
// "content word" applies to tag search, tag-from-text discovery, and post search.
// When the query is ALL stopwords, callers should fall back to the raw query so
// search still returns something.
// extractContentTerms with the universal fallback: when the query is ALL
// stopwords/too short (so no content terms survive), fall back to the raw
// trimmed/lowercased query as a single term, so search still returns something
// instead of nothing. Returns [] only for an empty/blank query.
function contentTermsOrRaw(query: unknown, opts: { minLen?: number } = {}): string[] {
  const terms = extractContentTerms(query, opts);
  if (terms.length) return terms;
  const raw = String(query ?? '')
    .trim()
    .toLowerCase();
  return raw ? [raw] : [];
}

function extractContentTerms(query: unknown, { minLen = 3 }: { minLen?: number } = {}): string[] {
  return [
    ...new Set(
      String(query ?? '')
        .toLowerCase()
        .split(/[^\p{L}\p{N}]+/u)
        .map((t) => t.trim())
        .filter((t) => {
          if (!t) return false;
          if (SEARCH_STOPWORDS.has(t)) return false;
          if (t.length >= minLen) return true;
          return SHORT_CONTENT_TERMS.has(t);
        }),
    ),
  ];
}

// Builds the shared WHERE clause + bound parameters for post listing queries.
// Returns { where, filterParams } so getPosts and getPostIds stay in sync.
function buildPostFilter(filters: PostFilters = {}): PostFilterResult {
  const {
    platform,
    source,
    mediaType,
    search,
    missingOnly,
    downloadStatus,
    collectionId,
    category,
    contentType,
    tag,
    tags,
    tagMode,
    entity,
    analyzedStatus,
    aiTagged,
    concepts,
    conceptMode,
  } = filters;

  const conditions: string[] = [];
  const filterParams: (string | number)[] = [];

  if (platform) {
    conditions.push('platform = ?');
    filterParams.push(platform);
  }

  // Source bucket (web-references unified search, F9 GAP-A): restrict to web
  // sites or to social posts without naming each social platform. 'all' (or
  // absent) imposes no constraint — the default unified behavior is unchanged.
  // 'social' is `platform != 'web'` so new social platforms need no change here.
  if (source === 'web') {
    conditions.push("platform = 'web'");
  } else if (source === 'social') {
    conditions.push("platform != 'web'");
  }

  if (collectionId) {
    conditions.push('id IN (SELECT post_id FROM post_collections WHERE collection_id = ?)');
    filterParams.push(collectionId);
  }

  if (mediaType) {
    conditions.push('media_type = ?');
    filterParams.push(mediaType);
  }

  if (category) {
    conditions.push('ai_category = ?');
    filterParams.push(category);
  }

  if (contentType) {
    conditions.push('ai_content_type = ?');
    filterParams.push(contentType);
  }

  // Tag filters hit the indexed post_tags table via EXISTS (case-insensitive:
  // tag_norm is the lowercased form). This replaces the old non-indexable
  // `ai_tags LIKE '%"…"%'` scan and is now case-insensitive (was case-sensitive).
  if (tag) {
    conditions.push(
      'EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = posts.id AND pt.tag_norm = ?)',
    );
    filterParams.push(String(tag).trim().toLowerCase());
  }

  // Multiple tags. Cleaned, deduped norms shared by the filter AND (in hybrid
  // search) the scoring path below.
  const cleanedTags = Array.isArray(tags)
    ? [
        ...new Set(
          tags
            .map((t) =>
              String(t ?? '')
                .trim()
                .toLowerCase(),
            )
            .filter(Boolean),
        ),
      ]
    : [];
  // Hybrid = tags + free-text together: in that case the tags must ALSO fold into
  // the relevance score (P1), and in OR mode they join the text OR block instead
  // of acting as a hard filter. The actual scoring/OR-join happens further down,
  // next to the text match units (reusing buildMatchUnit). Here we only emit the
  // HARD filter conditions that survive in each case:
  //   - AND mode: every tag is a hard EXISTS (post must carry all tags), hybrid or not.
  //   - OR mode, NON-hybrid (no text): a single EXISTS IN (any tag), as before.
  //   - OR mode, hybrid: NO hard tag filter here — tags enter the OR block below.
  const hybridTags = cleanedTags.length > 0 && !!search;
  if (cleanedTags.length) {
    if (tagMode === 'and') {
      for (const t of cleanedTags) {
        conditions.push(
          'EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = posts.id AND pt.tag_norm = ?)',
        );
        filterParams.push(t);
      }
    } else if (!hybridTags) {
      const placeholders = cleanedTags.map(() => '?').join(',');
      conditions.push(
        `EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = posts.id AND pt.tag_norm IN (${placeholders}))`,
      );
      for (const t of cleanedTags) filterParams.push(t);
    }
  }

  if (entity) {
    conditions.push(
      'EXISTS (SELECT 1 FROM post_entities pe WHERE pe.post_id = posts.id AND pe.ent_norm = ?)',
    );
    filterParams.push(String(entity).trim().toLowerCase());
  }

  if (analyzedStatus === 'analyzed') {
    conditions.push("ai_status = 'done'");
  } else if (analyzedStatus === 'unanalyzed') {
    conditions.push("(ai_status IS NULL OR ai_status != 'done')");
  }

  // Presence of AI-generated tags, via the indexed post_tags table. 'tagged'
  // keeps only posts with at least one tag; 'untagged' keeps those with none.
  // Scoped to AI-owned rows (tier general/specific or legacy-NULL): the user's
  // manual tags (tier='manual') don't make a post count as AI-tagged, so a
  // manually-tagged-but-unanalyzed post still shows up under "untagged".
  if (aiTagged === 'tagged') {
    conditions.push(
      "EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = posts.id AND (pt.tier IS NULL OR pt.tier IN ('general','specific')))",
    );
  } else if (aiTagged === 'untagged') {
    conditions.push(
      "NOT EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = posts.id AND (pt.tier IS NULL OR pt.tier IN ('general','specific')))",
    );
  }

  // Optional relevance ordering (only populated for free-text search). When set,
  // getPosts orders by this score DESC before timestamp DESC so posts matching
  // MORE distinct query terms (and matching them in structured fields) rank first.
  let relevanceExpr: string | null = null;
  const relevanceParams: string[] = [];

  // Columns a free-text term/concept is matched against (besides the post_tags table).
  // Includes the user-authored layer (note + manual tags) so personal annotations
  // are first-class in keyword search and contribute to IDF weighting.
  const termCols = ['text', 'ai_description', 'ai_tags', 'ai_keywords', 'user_note', 'user_tags'];

  // Builds the membership clause + relevance CASE for one match unit (a query
  // term or a suggested concept). Membership stays substring-based for recall;
  // the CASE encodes the HIERARCHY the gallery wants — an exact tag or a
  // whole-word hit ranks above a mere substring, and structured fields
  // (tag/keyword) rank above the description, which ranks above the raw caption.
  // `w` is the IDF weight so rarer terms still discriminate. Returns the clause
  // and CASE with their bound params in the order the `?` placeholders appear.
  const buildMatchUnit = (raw: string, w: number): MatchUnit => {
    const needle = String(raw).trim();
    const norm = needle.toLowerCase();
    const sub = `%${likeEscape(needle)}%`;
    const clause =
      `(text LIKE ? ESCAPE '\\' OR ai_description LIKE ? ESCAPE '\\' OR ai_tags LIKE ? ESCAPE '\\' OR ai_keywords LIKE ? ESCAPE '\\'` +
      ` OR user_note LIKE ? ESCAPE '\\' OR user_tags LIKE ? ESCAPE '\\'` +
      ` OR EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = posts.id AND pt.tag_norm LIKE ? ESCAPE '\\'))`;
    const clauseParams = [sub, sub, sub, sub, sub, sub, sub];
    // Manual tags are mirrored into post_tags, so the exact/substring post_tags
    // branches already score them. user_tags rides with ai_tags (whole-word 5w,
    // substring 3w) and the personal note with ai_description (4w / 2w).
    const score = `CASE
         WHEN EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = posts.id AND pt.tag_norm = ?) THEN ${(6 * w).toFixed(4)}
         WHEN word_match(ai_tags, ?) = 1 OR word_match(ai_keywords, ?) = 1 OR word_match(user_tags, ?) = 1 THEN ${(5 * w).toFixed(4)}
         WHEN word_match(ai_description, ?) = 1 OR word_match(user_note, ?) = 1 THEN ${(4 * w).toFixed(4)}
         WHEN word_match(text, ?) = 1 THEN ${(3.5 * w).toFixed(4)}
         WHEN ai_tags LIKE ? ESCAPE '\\' OR ai_keywords LIKE ? ESCAPE '\\' OR user_tags LIKE ? ESCAPE '\\'
              OR EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = posts.id AND pt.tag_norm LIKE ? ESCAPE '\\')
           THEN ${(3 * w).toFixed(4)}
         WHEN ai_description LIKE ? ESCAPE '\\' OR user_note LIKE ? ESCAPE '\\' THEN ${(2 * w).toFixed(4)}
         WHEN text LIKE ? ESCAPE '\\' THEN ${(1 * w).toFixed(4)}
         ELSE 0
       END`;
    const scoreParams = [
      norm, // post_tags exact
      norm, // word_match ai_tags
      norm, // word_match ai_keywords
      norm, // word_match user_tags
      norm, // word_match ai_description
      norm, // word_match user_note
      norm, // word_match text
      sub, // ai_tags LIKE
      sub, // ai_keywords LIKE
      sub, // user_tags LIKE
      sub, // post_tags LIKE
      sub, // ai_description LIKE
      sub, // user_note LIKE
      sub, // text LIKE
    ];
    return { clause, clauseParams, score, scoreParams };
  };

  // Scoring unit for a TAG in hybrid search (P1): exact-tag membership via the
  // indexed post_tags table, weighted by the tag's idf so a rare, discriminating
  // tag outranks a ubiquitous one — same weighting basis as searchPostsByTags.
  // The exact-tag weight (6*w) mirrors the top branch of buildMatchUnit so a tag
  // hit and a tag-equality text hit are on the same scale. Returns the membership
  // clause (used to OR tags into the text block in OR mode) and the score CASE.
  const buildTagScoreUnit = (norm: string, w: number): MatchUnit => {
    const clause = `EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = posts.id AND pt.tag_norm = ?)`;
    const clauseParams = [norm];
    const score = `CASE WHEN EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = posts.id AND pt.tag_norm = ?) THEN ${(6 * w).toFixed(4)} ELSE 0 END`;
    const scoreParams = [norm];
    return { clause, clauseParams, score, scoreParams };
  };

  // Each independent match unit becomes one OR/AND block; their CASEs are summed
  // into the relevance score. The base free-text query is one block (an OR over
  // its content terms); every active suggested concept adds another block.
  const blocks: { clause: string; params: string[] }[] = [];
  const scoreCases: string[] = [];
  const scoreParams: string[] = [];

  if (search) {
    const like = `%${likeEscape(search)}%`;

    // Split the query into individual CONTENT tokens (stopwords stripped) so
    // "AirPods Max" finds posts tagged "cuffie" (via tag_norm) without requiring
    // the full phrase in one column. Falls back to the raw query when the query
    // is all stopwords so the full-phrase LIKE still returns something.
    const terms = contentTermsOrRaw(search);

    const idf = termIdfWeights(terms, termCols);
    const termClauses: string[] = [];
    const termParams: string[] = [];
    for (const t of terms) {
      const u = buildMatchUnit(t, idf[t] || 1);
      termClauses.push(u.clause);
      termParams.push(...u.clauseParams);
      scoreCases.push(u.score);
      scoreParams.push(...u.scoreParams);
    }

    // Keep the full-phrase LIKE as an OR branch too: it aids recall on short,
    // verbatim queries (e.g. usernames/shortcodes) without re-admitting stopword
    // noise, since the phrase must appear literally.
    const phraseClause = `(text LIKE ? ESCAPE '\\' OR author_username LIKE ? ESCAPE '\\' OR shortcode LIKE ? ESCAPE '\\' OR ai_description LIKE ? ESCAPE '\\' OR ai_tags LIKE ? ESCAPE '\\' OR ai_keywords LIKE ? ESCAPE '\\' OR user_note LIKE ? ESCAPE '\\' OR user_tags LIKE ? ESCAPE '\\')`;

    blocks.push({
      clause: `(${[phraseClause, ...termClauses].join(' OR ')})`,
      params: [like, like, like, like, like, like, like, like, ...termParams],
    });
  }

  // Suggested-concept filters (the AI tag chips below the search bar). Each is a
  // free concept that may match a post tag OR a word in the caption/description,
  // so it reuses the same match-unit machinery as a query term. They broaden the
  // query in OR by default; the AND/OR toggle narrows to posts matching them all.
  const conceptList = Array.isArray(concepts)
    ? [...new Set(concepts.map((c) => String(c ?? '').trim()).filter(Boolean))]
    : [];
  for (const c of conceptList) {
    const w = termIdfWeights([c.toLowerCase()], termCols)[c.toLowerCase()] || 1;
    const u = buildMatchUnit(c, w);
    blocks.push({ clause: u.clause, params: u.clauseParams });
    scoreCases.push(u.score);
    scoreParams.push(...u.scoreParams);
  }

  // Hybrid tags (P1): fold each wanted tag into the relevance score, idf-weighted,
  // so the final ranking FUSES tag matches with the text relevance instead of
  // ranking by tag-count alone. In OR mode the tag membership ALSO joins the OR
  // block (a post matching only a tag, or only the text, is still admitted). In
  // AND mode the per-tag hard EXISTS filters above already gate membership, so we
  // only add the score here (no extra OR clause).
  if (hybridTags) {
    const tagIdf = tagIdfWeights(cleanedTags);
    for (const t of cleanedTags) {
      const u = buildTagScoreUnit(t, tagIdf.get(t) ?? 1);
      if (tagMode !== 'and') {
        blocks.push({ clause: u.clause, params: u.clauseParams });
      }
      scoreCases.push(u.score);
      scoreParams.push(...u.scoreParams);
    }
  }

  if (blocks.length) {
    const joiner = conceptMode === 'and' ? ' AND ' : ' OR ';
    conditions.push(`(${blocks.map((b) => b.clause).join(joiner)})`);
    for (const b of blocks) filterParams.push(...b.params);
    if (scoreCases.length) {
      relevanceExpr = scoreCases.join(' + ');
      relevanceParams.push(...scoreParams);
    }
  }

  const hasLocalAsset =
    '(thumbnail_path IS NOT NULL OR image_path IS NOT NULL OR video_path IS NOT NULL)';
  const noLocalAsset = '(thumbnail_path IS NULL AND image_path IS NULL AND video_path IS NULL)';

  if (missingOnly || downloadStatus === 'missing') {
    conditions.push(noLocalAsset);
  } else if (downloadStatus === 'downloaded') {
    conditions.push(hasLocalAsset);
  }

  const where = conditions.length ? `WHERE ${conditions.join(' AND ')}` : '';

  return { where, filterParams, relevanceExpr, relevanceParams };
}

// IDF-style weight per query term, computed against live post counts so rarer
// terms (which discriminate the relevant set) outweigh ubiquitous ones. df is the
// number of posts where the term appears in ANY of `cols`; weight = log(N/df),
// clamped to [1, 3] so the distinct-term-count signal still dominates and a single
// hyper-rare token can't swamp it. Falls back to 1 on any error/empty DB.
// Each df probe is a multi-LIKE full scan of posts, so weights are memoized per
// (cols, term) for the process lifetime; invalidateGlobalCaches drops the cache
// whenever post content changes. Size-capped so arbitrary queries can't grow it
// without bound.
const TERM_IDF_CACHE_MAX = 1000;
const _termIdfCache = new Map<string, number>();
function termIdfWeights(terms: string[], cols: string[]): Record<string, number> {
  const out: Record<string, number> = {};
  if (!Array.isArray(terms) || !terms.length) return out;
  try {
    const colsKey = cols.join(',');
    const misses: string[] = [];
    for (const t of terms) {
      const hit = _termIdfCache.get(`${colsKey} ${t}`);
      if (hit !== undefined) out[t] = hit;
      else misses.push(t);
    }
    if (!misses.length) return out;
    // Single full scan: COUNT(*) and every missed term's df come out of ONE
    // pass over posts (SUM(CASE WHEN <cols LIKE term> …) per term) instead of
    // 1 + N separate LIKE scans — each row is fetched once, not N+1 times.
    const orCols = cols.map((c) => `${c} LIKE ? ESCAPE '\\'`).join(' OR ');
    const dfExprs = misses.map((_, i) => `SUM(CASE WHEN ${orCols} THEN 1 ELSE 0 END) AS df${i}`);
    const row = db!
      .prepare(`SELECT COUNT(*) AS n, ${dfExprs.join(', ')} FROM posts`)
      .get(...misses.flatMap((t) => cols.map(() => `%${likeEscape(t)}%`))) as Record<
      string,
      number
    >;
    const N = row.n || 0;
    if (!N) {
      for (const t of misses) out[t] = 1;
      return out;
    }
    misses.forEach((t, i) => {
      const df = row[`df${i}`] || 0;
      const raw = df > 0 ? Math.log(N / df) : Math.log(N);
      const w = Math.max(1, Math.min(3, raw));
      out[t] = w;
      if (_termIdfCache.size >= TERM_IDF_CACHE_MAX) _termIdfCache.clear();
      _termIdfCache.set(`${colsKey} ${t}`, w);
    });
  } catch {
    for (const t of terms) {
      if (out[t] === undefined) out[t] = 1;
    }
  }
  return out;
}

// IDF-style weight per TAG (vs. termIdfWeights which works on free-text columns).
// df = numero di post che portano quel tag (dall'indice post_tags); weight =
// log(N / df), clampato in [1, 3] esattamente come termIdfWeights così che il
// segnale "quanti tag matchano" resti dominante e un tag iper-raro non sbilanci.
// Restituisce una Map(norm → weight). Fallback a 1 su errore/DB vuoto.
function tagIdfWeights(norms: unknown[]): Map<string, number> {
  const out = new Map<string, number>();
  const list = [
    ...new Set(
      (norms || [])
        .map((t) =>
          String(t ?? '')
            .trim()
            .toLowerCase(),
        )
        .filter(Boolean),
    ),
  ];
  if (!list.length) return out;
  try {
    const N = (db!.prepare('SELECT COUNT(*) AS c FROM posts').get() as { c: number }).c || 0;
    if (!N) {
      for (const t of list) out.set(t, 1);
      return out;
    }
    const dfStmt = db!.prepare(
      'SELECT COUNT(DISTINCT post_id) AS c FROM post_tags WHERE tag_norm = ?',
    );
    for (const t of list) {
      const df = (dfStmt.get(t) as { c: number }).c || 0;
      const raw = df > 0 ? Math.log(N / df) : Math.log(N);
      out.set(t, Math.max(1, Math.min(3, raw)));
    }
  } catch {
    for (const t of list) out.set(t, 1);
  }
  return out;
}

// filters: { platform?, mediaType?, search?, missingOnly?, limit = 50, offset = 0 }
// Returns { posts: Post[], total: number } where total is the count of ALL matching rows.
// Builds the recency ORDER BY clause for the given sort order. Undated posts
// (NULL *or* '' — importers emit '' when the source carries no usable date) go
// LAST in both directions; without the explicit check, '' sorts lexicographically
// BEFORE every ISO date and undated posts would top the 'oldest' listing. The id
// tiebreaker makes equal-timestamp order stable across refetches (SQLite gives
// no guarantee otherwise, and the gallery re-runs this query on every
// infinite-scroll growth — unstable ties would visibly reshuffle).
function recencyOrder(sortOrder: string | undefined): string {
  const undated = "(timestamp IS NULL OR timestamp = '')";
  return sortOrder === 'oldest'
    ? `${undated}, timestamp ASC, id ASC`
    : `${undated}, timestamp DESC, id DESC`;
}

function getPosts(filters: PostFilters = {}): { posts: Shelfy.Post[]; total: number } {
  if (!db) throw new Error('Database not initialized');
  const { limit = 50, offset = 0, sortOrder } = filters;

  const { where, filterParams, relevanceExpr, relevanceParams } = buildPostFilter(filters);

  const total = (
    db.prepare(`SELECT COUNT(*) as count FROM posts ${where}`).get(...filterParams) as {
      count: number;
    }
  ).count;

  // For free-text search, rank by relevance (distinct query terms matched, IDF-
  // and field-weighted) then recency, so multi-term-matching posts surface on the
  // first page instead of being buried under recent single-term matches. For all
  // other listings, order by recency in the chosen direction. The relevance
  // expression lives in the SELECT list, so its bound params precede the WHERE params.
  const order = recencyOrder(sortOrder);
  let sql: string;
  let params: (string | number)[];
  if (relevanceExpr) {
    sql = `SELECT * FROM posts ${where} ORDER BY (${relevanceExpr}) DESC, ${order} LIMIT ? OFFSET ?`;
    params = [...filterParams, ...relevanceParams, limit, offset];
  } else {
    sql = `SELECT * FROM posts ${where} ORDER BY ${order} LIMIT ? OFFSET ?`;
    params = [...filterParams, limit, offset];
  }
  const rows = db.prepare(sql).all(...params) as PostRow[];

  return {
    posts: attachCollections(
      attachMedia(rows.map(rowToPost).filter((p): p is Shelfy.Post => p !== null)),
    ),
    total,
  };
}

// Returns the ids of ALL rows matching the same filters as getPosts (no
// pagination), ordered by timestamp DESC. Used for bulk multi-post operations.
function getPostIds(filters: PostFilters = {}): string[] {
  if (!db) throw new Error('Database not initialized');
  const { where, filterParams } = buildPostFilter(filters);
  const rows = db
    .prepare(`SELECT id FROM posts ${where} ORDER BY ${recencyOrder(filters.sortOrder)}`)
    .all(...filterParams) as { id: string }[];
  return rows.map((r) => r.id);
}

// Fetches full posts for the given ids, batched to respect SQLite's variable
// limit, with media + collections attached. Result follows the input id order.
function getPostsByIds(ids: string[] = []): Shelfy.Post[] {
  if (!db) throw new Error('Database not initialized');
  if (!Array.isArray(ids) || ids.length === 0) return [];

  const BATCH = 500;
  const byId = new Map<string, Shelfy.Post>();
  for (let i = 0; i < ids.length; i += BATCH) {
    const chunk = ids.slice(i, i + BATCH);
    const placeholders = chunk.map(() => '?').join(',');
    const rows = db
      .prepare(`SELECT * FROM posts WHERE id IN (${placeholders})`)
      .all(...chunk) as PostRow[];
    for (const row of rows) {
      const post = rowToPost(row);
      if (post) byId.set(post.id, post);
    }
  }

  const posts: Shelfy.Post[] = [];
  for (const id of ids) {
    const post = byId.get(id);
    if (post) posts.push(post);
  }

  attachMedia(posts);
  attachCollections(posts);
  return posts;
}

// Of the given ids, returns the subset that already exist in the posts table.
// Lightweight (id-only, no media/collections) — used by the scraper's selection
// overlay to flag posts that are already in the local library.
function existingIds(ids: (string | number)[] = []): string[] {
  if (!db) throw new Error('Database not initialized');
  if (!Array.isArray(ids) || ids.length === 0) return [];
  const BATCH = 500;
  const out: string[] = [];
  for (let i = 0; i < ids.length; i += BATCH) {
    const chunk = ids.slice(i, i + BATCH).map((x) => String(x));
    const placeholders = chunk.map(() => '?').join(',');
    const rows = db.prepare(`SELECT id FROM posts WHERE id IN (${placeholders})`).all(...chunk) as {
      id: string;
    }[];
    for (const r of rows) out.push(r.id);
  }
  return out;
}

// Given DOM-derived post keys (Instagram shortcode / Twitter tweet id), returns
// the ones already in the DB as [{ key, id }] — matching each key against BOTH the
// primary key AND the shortcode column. This is what makes "Già in database" work
// for posts saved in earlier sessions: the scraper only reliably knows a post's
// shortcode from the DOM, while the stored id may be Instagram's numeric pk.
function savedByKeys(keys: (string | number)[] = []): { key: string; id: string }[] {
  if (!db) throw new Error('Database not initialized');
  if (!Array.isArray(keys) || keys.length === 0) return [];
  const want = new Set(keys.map((k) => String(k)));
  const list = Array.from(want);
  const BATCH = 500;
  const seen = new Set<string>();
  const out: { key: string; id: string }[] = [];
  for (let i = 0; i < list.length; i += BATCH) {
    const chunk = list.slice(i, i + BATCH);
    const ph = chunk.map(() => '?').join(',');
    const rows = db
      .prepare(`SELECT id, shortcode FROM posts WHERE id IN (${ph}) OR shortcode IN (${ph})`)
      .all(...chunk, ...chunk) as { id: string; shortcode: string | null }[];
    for (const r of rows) {
      const sc = r.shortcode != null ? String(r.shortcode) : '';
      const rid = String(r.id);
      // Prefer the key the scraper actually used (shortcode on IG, id on TW).
      const key = want.has(sc) ? sc : want.has(rid) ? rid : null;
      if (key && !seen.has(key)) {
        seen.add(key);
        out.push({ key, id: rid });
      }
    }
  }
  return out;
}

function getPost(id: string): Shelfy.Post | null {
  if (!db) throw new Error('Database not initialized');
  const row = db.prepare('SELECT * FROM posts WHERE id = ?').get(id) as PostRow | undefined;
  const post = rowToPost(row);
  if (!post) return null;
  attachMedia([post]);
  attachCollections([post]);
  return post;
}

// Lightweight posts for the AI analyzer: only the columns the analyzer needs,
// with ordered media attached (NO collections). Selection:
//   - ids: an array → those posts, in the given input order;
//   - missingOnly: true → posts not yet analyzed (ai_status IS NULL or != 'done');
//   - otherwise → all posts.
// No forced limit.
function getPostsForAnalysis({
  ids = null,
  missingOnly = false,
}: { ids?: string[] | null; missingOnly?: boolean } = {}): Shelfy.AnalysisPost[] {
  if (!db) throw new Error('Database not initialized');
  const COLS = `id, shortcode, media_type, video_path, image_path, thumbnail_path,
                thumbnail_url, author_username, text`;
  type LightRow = {
    id: string;
    shortcode: string | null;
    media_type: Shelfy.MediaType | null;
    video_path: string | null;
    image_path: string | null;
    thumbnail_path: string | null;
    thumbnail_url: string | null;
    author_username: string | null;
    text: string | null;
  };
  const toLight = (row: LightRow): Shelfy.AnalysisPost => ({
    id: row.id,
    shortcode: row.shortcode,
    mediaType: row.media_type,
    videoPath: row.video_path,
    imagePath: row.image_path,
    thumbnailPath: row.thumbnail_path,
    thumbnailUrl: row.thumbnail_url,
    authorUsername: row.author_username,
    text: row.text,
  });

  if (Array.isArray(ids)) {
    if (ids.length === 0) return [];
    const BATCH = 500;
    const byId = new Map<string, Shelfy.AnalysisPost>();
    for (let i = 0; i < ids.length; i += BATCH) {
      const chunk = ids.slice(i, i + BATCH);
      const placeholders = chunk.map(() => '?').join(',');
      const rows = db
        .prepare(`SELECT ${COLS} FROM posts WHERE id IN (${placeholders})`)
        .all(...chunk) as LightRow[];
      for (const row of rows) byId.set(row.id, toLight(row));
    }
    const posts: Shelfy.AnalysisPost[] = [];
    for (const id of ids) {
      const p = byId.get(id);
      if (p) posts.push(p);
    }
    attachMedia(posts);
    return posts;
  }

  const where = missingOnly ? "WHERE ai_status IS NULL OR ai_status != 'done'" : '';
  const rows = db.prepare(`SELECT ${COLS} FROM posts ${where}`).all() as LightRow[];
  const posts = rows.map(toLight);
  attachMedia(posts);
  return posts;
}

// Replace all media rows for a post (used when the post itself is being
// (re)inserted wholesale, so prior rows are gone or stale).
function replacePostMedia(postId: string, media: DerivedMedia[]): void {
  db!.prepare('DELETE FROM post_media WHERE post_id = ?').run(postId);
  const ins = db!.prepare(
    'INSERT OR IGNORE INTO post_media (post_id, position, media_type, source_url, local_path) VALUES (?, ?, ?, ?, ?)',
  );
  // m.localPath is set when the file is already on disk (web screenshots); for
  // social posts it's undefined → NULL → the downloader fills it later, unchanged.
  media.forEach((m, i) => ins.run(postId, i, m.type, m.url, m.localPath ?? null));
}

// Merge media for an already-existing post: add any missing slides and refresh
// source_url only where nothing has been downloaded yet, so local_path (and
// thus downloaded files) is never clobbered by a re-import.
function mergePostMedia(postId: string, media: DerivedMedia[]): void {
  const ins = db!.prepare(
    'INSERT OR IGNORE INTO post_media (post_id, position, media_type, source_url, local_path) VALUES (?, ?, ?, ?, ?)',
  );
  const upd = db!.prepare(
    'UPDATE post_media SET media_type = ?, source_url = ? WHERE post_id = ? AND position = ? AND local_path IS NULL',
  );
  media.forEach((m, i) => {
    // Insert carries an already-known local_path (web); on a row that already
    // exists this is a no-op (INSERT OR IGNORE), and the source_url refresh below
    // still only touches slides with no local_path — so a downloaded/local file
    // is never clobbered, exactly as before.
    ins.run(postId, i, m.type, m.url, m.localPath ?? null);
    upd.run(m.type, m.url, postId, i);
  });
}

// Single-post upsert. Delegates to bulkUpsert so it shares the SAME merge
// semantics: INSERT OR IGNORE + metadata update (only when no local path is
// set) + mergePostMedia, all inside a transaction. The previous implementation
// used INSERT OR REPLACE + replacePostMedia, which wiped the ai_* columns and
// already-downloaded local_path on every re-import — a data-loss bug.
function upsertPost(post: PostInput): Shelfy.UpsertResult {
  return bulkUpsert([post]);
}

// Bulk upsert using INSERT OR IGNORE to preserve existing downloaded paths
// `overwriteAi`: quando true, l'AI importata SOVRASCRIVE quella locale anche se il
// post ha già un'analisi (ai_status='done'). Default false → comportamento storico
// (lo scraper non clobbera mai un'analisi locale). L'import JSON deliberato lo passa
// true, così reimportare una propria ri-analisi aggiorna davvero i post esistenti.
function bulkUpsert(
  posts: PostInput[],
  { overwriteAi = false }: { overwriteAi?: boolean } = {},
): Shelfy.UpsertResult {
  if (!db) throw new Error('Database not initialized');
  const insert = db.prepare(`
    INSERT OR IGNORE INTO posts
      (id, platform, shortcode, post_url, profile_url, author_username, author_name,
       text, thumbnail_url, media_type, timestamp, thumbnail_path, image_path, video_path, media_count,
       web_url, web_domain, web_final_url, web_palette_json, web_fonts_json, web_tech_json,
       web_awards_json, web_pages_json, web_meta_json, web_captured_at)
    VALUES
      (@id, @platform, @shortcode, @post_url, @profile_url, @author_username, @author_name,
       @text, @thumbnail_url, @media_type,
       -- No usable date from the source (e.g. Pinterest's saved-pins RPC has no
       -- created_at) → fall back to the import time, so the post sorts where the
       -- user expects instead of below 2016-era posts. Insert-only: updateMeta
       -- below never replaces an existing date with a fallback.
       COALESCE(NULLIF(@timestamp, ''), strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
       @thumbnail_path, @image_path, @video_path, @media_count,
       @web_url, @web_domain, @web_final_url, @web_palette_json, @web_fonts_json, @web_tech_json,
       @web_awards_json, @web_pages_json, @web_meta_json, @web_captured_at)
  `);

  // For fields that don't involve local paths, update metadata if row already exists
  const updateMeta = db.prepare(`
    UPDATE posts SET
      platform = @platform,
      shortcode = @shortcode,
      post_url = @post_url,
      profile_url = @profile_url,
      author_username = @author_username,
      author_name = @author_name,
      text = @text,
      thumbnail_url = @thumbnail_url,
      media_type = @media_type,
      media_count = @media_count,
      -- A re-import whose parser found no date ('' / NULL) must never clobber a
      -- date we already have (from a previous sync, a shortcode decode, or the
      -- insert-time fallback above).
      timestamp = COALESCE(NULLIF(@timestamp, ''), timestamp),
      web_url = @web_url,
      web_domain = @web_domain,
      web_final_url = @web_final_url,
      web_palette_json = @web_palette_json,
      web_fonts_json = @web_fonts_json,
      web_tech_json = @web_tech_json,
      web_awards_json = @web_awards_json,
      web_pages_json = @web_pages_json,
      web_meta_json = @web_meta_json,
      web_captured_at = @web_captured_at
    WHERE id = @id
      AND thumbnail_path IS NULL
      AND image_path IS NULL
      AND video_path IS NULL
  `);

  // Reads the existing ai_status so we know whether to overwrite AI data on a
  // re-import: present locally → keep it, never clobber a local analysis.
  const existingStatus = db.prepare('SELECT ai_status FROM posts WHERE id = ?');

  let inserted = 0;
  let skipped = 0;
  let aiUpdated = 0;

  // L'AI importata "porta davvero un'analisi" solo se ha tag/descrizione (non il
  // solo ai_status): così overwriteAi non azzera un'analisi locale buona con un
  // import che di AI non ha nulla.
  const carriesAnalysis = (f: AiFields | null): boolean =>
    !!(
      f &&
      (f.tags || f.description || f.generalTags || f.specificTags || f.keywords || f.entities)
    );

  const insertMany = db.transaction((postsArr: PostInput[]) => {
    for (const post of postsArr) {
      const row = postToRow(post);
      const media = deriveMedia(post);
      const aiFields = extractAiFields(post);
      const result = insert.run(row);
      if (result.changes === 0) {
        updateMeta.run(row);
        mergePostMedia(post.id, media);
        // Existing post: apply imported AI when the row has no local analysis yet
        // (ai_status NULL) OR when overwriteAi is set AND the import actually
        // carries an analysis (so a deliberate re-import refreshes the tags).
        if (aiFields) {
          const cur = existingStatus.get(post.id) as { ai_status: string | null } | undefined;
          const noLocal = cur && (cur.ai_status === null || cur.ai_status === undefined);
          if (noLocal || (overwriteAi && carriesAnalysis(aiFields))) {
            applyAiAnalysis(post.id, aiFields);
            aiUpdated++;
          }
        }
        skipped++;
      } else {
        replacePostMedia(post.id, media);
        // New post: always apply imported AI data if present.
        if (aiFields) {
          applyAiAnalysis(post.id, aiFields);
          aiUpdated++;
        }
        inserted++;
      }
    }
  });

  insertMany(posts);
  invalidateGlobalCaches();
  return { inserted, skipped, aiUpdated };
}

// ─── Web references (sites as platform='web' posts) ─────────────────────────
//
// A website is a posts row with platform='web', media_type='website', and a
// deterministic id derived from its normalized final URL (dedup per-URL via
// INSERT OR IGNORE). These helpers map the WebReference contract (see
// notes/specs/06-persistence-schema.md) onto the SAME shape bulkUpsert →
// postToRow/deriveMedia/extractAiFields/applyAiAnalysis already consume, so web
// posts inherit transaction/dedup/merge/AI-resync for free.

// hostname without leading 'www.', lowercased. Returns '' when unparseable.
function webHostname(rawUrl: string): string {
  try {
    return new URL(rawUrl).hostname.toLowerCase().replace(/^www\./, '');
  } catch {
    return '';
  }
}

// Canonical URL string used as the dedup key: lowercased scheme+host, no 'www.',
// no trailing slash, no fragment, tracking params (utm_*, gclid, fbclid, ref)
// stripped. Falls back to the raw string when it can't be parsed.
function normalizeWebUrl(rawUrl: string): string {
  let u: URL;
  try {
    u = new URL(rawUrl);
  } catch {
    // A bare hostname ("example.com") has no scheme → `new URL` throws. Retry with
    // https:// prepended so the id matches the post-redirect finalUrl
    // ("https://example.com") the orchestrator later persists. Without this the
    // placeholder row (built from the raw paste) and the enriched row (built from
    // finalUrl) get DIFFERENT deterministic ids → a duplicate post in the gallery
    // AND a duplicate entry in the Websites panel.
    try {
      u = new URL('https://' + String(rawUrl ?? '').trim());
    } catch {
      return String(rawUrl ?? '').trim();
    }
  }
  u.protocol = u.protocol.toLowerCase();
  u.hostname = u.hostname.toLowerCase().replace(/^www\./, '');
  u.hash = '';
  const drop = ['gclid', 'fbclid', 'ref'];
  for (const [k] of [...u.searchParams]) {
    if (k.toLowerCase().startsWith('utm_') || drop.includes(k.toLowerCase())) {
      u.searchParams.delete(k);
    }
  }
  let s = u.toString();
  // Strip a trailing slash on the path (but keep "https://host/").
  s = s.replace(/\/(?=$|\?)/, (m, off: number) => (s[off - 1] === '/' ? m : ''));
  if (s.endsWith('/') && !/^https?:\/\/[^/]+\/$/.test(s)) s = s.slice(0, -1);
  return s;
}

// Deterministic post id for a site: 'web:' + sha1(normalizeWebUrl(url)).
function webPostId(finalUrl: string): string {
  return 'web:' + crypto.createHash('sha1').update(normalizeWebUrl(finalUrl)).digest('hex');
}

// Case-insensitive union of string arrays, preserving the first-seen form.
function unionStrings(...lists: (unknown[] | undefined)[]): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const list of lists) {
    if (!Array.isArray(list)) continue;
    for (const v of list) {
      if (typeof v !== 'string') continue;
      const form = v.trim();
      if (!form) continue;
      const norm = form.toLowerCase();
      if (seen.has(norm)) continue;
      seen.add(norm);
      out.push(form);
    }
  }
  return out;
}

// Map a WebReference (§1.1 of spec 06) to the camelCase post shape bulkUpsert
// consumes. Tolerant of missing optional fields. Internal: exported too so the
// orchestrator (F10) / tests can reuse it, but the public entrypoint is
// upsertWebReference.
function webRefToPost(ref: WebReference = {}): PostInput {
  const finalUrl = ref.finalUrl || ref.url || '';
  const domain = ref.domain || webHostname(finalUrl);
  // The identity is the id assigned when the placeholder was created (from the
  // pasted URL) — passed explicitly as ref.id. Deriving it from finalUrl instead
  // breaks when the home redirects (e.g. "/" → "/it" on a multilingual site or
  // http→https): the enriched row would land on a DIFFERENT id than the
  // placeholder + the live job, splitting one site into two records.
  const id = ref.id || webPostId(finalUrl);
  const pages = Array.isArray(ref.pages) ? ref.pages : [];
  const hero = pages[0] || ({} as Shelfy.WebPage);
  const capturedAt = ref.capturedAt || Math.floor(Date.now() / 1000);

  // Searchable text: title + meta description + hero content. The other pages'
  // contentText stays in web_pages_json; text search runs on posts.text.
  const text =
    [ref.title, ref.description, hero.contentText].filter(Boolean).join('\n\n').slice(0, 20000) ||
    null;

  const ai = ref.ai;
  const flatTags = ai
    ? Array.isArray(ai.tags)
      ? ai.tags
      : unionStrings(ai.general_tags, ai.specific_tags)
    : undefined;

  return {
    // ── standard posts columns ──
    id,
    platform: 'web',
    shortcode: null,
    postUrl: finalUrl,
    profileUrl: domain ? `https://${domain}` : null,
    authorUsername: domain || null,
    authorName: ref.title || domain || null,
    text,
    thumbnailUrl: hero.meta?.ogImage || null,
    mediaType: 'website',
    timestamp: new Date(capturedAt * 1000).toISOString(),
    // screenshots are ALREADY local files → populate paths directly
    thumbnailPath: hero.screenshotPath || null,
    imagePath: hero.screenshotPath || null,
    videoPath: null,

    // ── one slide per captured page (deriveMedia reads this; localPath set) ──
    media: pages
      .filter((p) => p && p.screenshotPath)
      .map((p) => ({ type: 'image', url: p.url || finalUrl, localPath: p.screenshotPath })),

    // ── AI fields (extractAiFields maps these; applyAiAnalysis writes them) ──
    aiDescription: ai?.description,
    aiTags: flatTags,
    aiGeneralTags: ai?.general_tags,
    aiSpecificTags: ai?.specific_tags,
    aiEntities: ai ? unionStrings(ai.entities, ref.techStack) : undefined,
    aiKeywords: ai?.search_keywords,
    aiCategory: ai?.industry, // industry → ai_category (raw enum slug)
    aiContentType: ai?.purpose, // purpose  → ai_content_type (raw enum slug)
    aiLanguage: ai?.language,
    aiSaveReason: ai?.save_reason,
    aiStatus: ai ? 'done' : undefined, // 'done' only when the AI actually ran
    aiModel: ai?.model,

    // ── web_* columns (objects/arrays → JSON via postToRow.serializeJson) ──
    webUrl: ref.url || finalUrl,
    webDomain: domain || null,
    webFinalUrl: finalUrl,
    webPalette: ref.palette,
    webFonts: ref.fonts,
    webTech: ref.techStack,
    webAwards: ref.awards,
    webPages: pages,
    webMeta:
      ref.meta ??
      (ref.description || ref.lang ? { description: ref.description, lang: ref.lang } : undefined),
    webCapturedAt: capturedAt,
  };
}

// Public entrypoint: persist a fully-enriched WebReference. Thin wrapper over
// bulkUpsert so it inherits the transaction, per-URL dedup, media merge and
// applyAiAnalysis. overwriteAi defaults true (a deliberate re-capture refreshes
// the analysis); set false to preserve a prior local analysis. Returns
// { id, inserted, skipped, aiUpdated }.
function upsertWebReference(
  ref: WebReference,
  { overwriteAi = true }: { overwriteAi?: boolean } = {},
): Shelfy.UpsertResult & { id: string } {
  if (!db) throw new Error('Database not initialized');
  const post = webRefToPost(ref);
  // Archive the PREVIOUS capture (if any) before this one overwrites the row, so
  // the panel keeps a dated version history. No-op for a first capture or a bare
  // placeholder (no prior pages). Screenshots don't collide on disk because each
  // capture writes date-stamped files (see weborchestrator).
  archiveCurrentWebSnapshot(post.id);
  const res = bulkUpsert([post], { overwriteAi });
  // A web re-capture must REPLACE the current media with the latest pages (the
  // bulkUpsert update path only merges/adds). The previous screenshots survive on
  // disk and stay referenced by the snapshot we just archived above.
  replacePostMedia(post.id, deriveMedia(post));
  // bulkUpsert's updateMeta path is gated on `thumbnail_path/image_path IS NULL`,
  // so for an EXISTING row that already has a capture it refreshes nothing —
  // neither the screenshot nor the web_* metadata. For a deliberate re-capture we
  // WANT the posts row to become the new (current) version wholesale, so write the
  // web columns + hero paths + title/text explicitly here. (The first capture's
  // row was a path-less placeholder, so this also covers it.)
  const row = postToRow(post);
  db.prepare(
    `UPDATE posts SET
       author_name = @author_name,
       text = @text,
       timestamp = @timestamp,
       thumbnail_url = @thumbnail_url,
       thumbnail_path = COALESCE(@thumbnail_path, thumbnail_path),
       image_path = COALESCE(@image_path, image_path),
       web_url = @web_url,
       web_domain = @web_domain,
       web_final_url = @web_final_url,
       web_palette_json = @web_palette_json,
       web_fonts_json = @web_fonts_json,
       web_tech_json = @web_tech_json,
       web_awards_json = @web_awards_json,
       web_pages_json = @web_pages_json,
       web_meta_json = @web_meta_json,
       web_captured_at = @web_captured_at
     WHERE id = @id`,
  ).run(row);
  invalidateGlobalCaches();
  return { id: post.id, ...res };
}

// Batch variant; same semantics as upsertWebReference.
function upsertWebReferences(
  refs: WebReference[] = [],
  { overwriteAi = true }: { overwriteAi?: boolean } = {},
): Shelfy.UpsertResult {
  if (!db) throw new Error('Database not initialized');
  return bulkUpsert((refs || []).map(webRefToPost), { overwriteAi });
}

// Placeholder-first (F10): create the raw posts row IMMEDIATELY from just the
// URL — deterministic id, platform='web', media_type='website', web_url/web_domain
// set, ai_status NULL, no screenshots yet — so the gallery shows a card right
// away, then the orchestrator enriches the same row in the background. Idempotent
// on the deterministic id. Returns the post id.
function createWebPlaceholder(url: string): string {
  if (!db) throw new Error('Database not initialized');
  const finalUrl = url;
  const domain = webHostname(finalUrl);
  const id = webPostId(finalUrl);
  // No ai key → aiStatus undefined → row stays unanalyzed; no pages → no media.
  bulkUpsert([webRefToPost({ url, finalUrl, domain, title: domain, pages: [] })]);
  return id;
}

// getStats full-scans posts and is polled every ~5 s plus once per completed
// download job, so the result is memoized briefly. Explicit invalidation covers
// the hot write paths (invalidateGlobalCaches for upserts/deletes/AI writes,
// updatePaths/updateMediaPath for download completions); the TTL caps staleness
// from any residual write path that skips both.
const STATS_CACHE_TTL_MS = 5000;
let _statsCache: { value: Shelfy.Stats; at: number } | null = null;
function invalidateStatsCache(): void {
  _statsCache = null;
}

function getStats(): Shelfy.Stats {
  if (!db) throw new Error('Database not initialized');
  if (_statsCache && Date.now() - _statsCache.at < STATS_CACHE_TTL_MS) {
    return _statsCache.value;
  }

  const agg = db
    .prepare(
      `SELECT COUNT(*) AS total,
        COALESCE(SUM(thumbnail_path IS NOT NULL OR image_path IS NOT NULL OR video_path IS NOT NULL), 0) AS downloaded,
        COALESCE(SUM(thumbnail_path IS NOT NULL), 0) AS thumbnails,
        COALESCE(SUM(image_path IS NOT NULL), 0) AS images,
        COALESCE(SUM(video_path IS NOT NULL), 0) AS videos
       FROM posts`,
    )
    .get() as {
    total: number;
    downloaded: number;
    thumbnails: number;
    images: number;
    videos: number;
  };

  const platformRows = db
    .prepare(
      "SELECT platform, COUNT(*) as count FROM posts WHERE platform IN ('instagram', 'twitter', 'pinterest', 'web') GROUP BY platform",
    )
    .all() as { platform: 'instagram' | 'twitter' | 'pinterest' | 'web'; count: number }[];
  const byPlatform: Record<'instagram' | 'twitter' | 'pinterest' | 'web', number> = {
    instagram: 0,
    twitter: 0,
    pinterest: 0,
    web: 0,
  };
  for (const r of platformRows) {
    byPlatform[r.platform] = r.count;
  }

  const mediaTypeRows = db
    .prepare('SELECT media_type, COUNT(*) as count FROM posts GROUP BY media_type')
    .all() as { media_type: string | null; count: number }[];
  const byMediaType: Partial<Record<string, number>> = {};
  for (const r of mediaTypeRows) {
    if (r.media_type) byMediaType[r.media_type] = r.count;
  }

  const value: Shelfy.Stats = {
    total: agg.total,
    byPlatform,
    byMediaType,
    downloaded: agg.downloaded,
    downloadedByType: { thumbnails: agg.thumbnails, images: agg.images, videos: agg.videos },
  };
  _statsCache = { value, at: Date.now() };
  return value;
}

function updatePaths(
  id: string,
  {
    thumbnailPath,
    imagePath,
    videoPath,
    thumbBlur,
  }: {
    thumbnailPath?: string | null;
    imagePath?: string | null;
    videoPath?: string | null;
    thumbBlur?: string | null;
  },
): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare(
    `
    UPDATE posts SET
      thumbnail_path = COALESCE(?, thumbnail_path),
      image_path = COALESCE(?, image_path),
      video_path = COALESCE(?, video_path),
      thumb_blur = COALESCE(?, thumb_blur)
    WHERE id = ?
  `,
  ).run(thumbnailPath ?? null, imagePath ?? null, videoPath ?? null, thumbBlur ?? null, id);
  invalidateStatsCache();
}

// ── Blur-up placeholders (see thumbs.js) ────────────────────────────────────
// Posts that have a local cover image but no placeholder yet — the startup
// backfill's work list. src mirrors PostCard's cover choice (thumbnail first).
function listPostsMissingThumbBlur(): { id: string; src: string }[] {
  if (!db) throw new Error('Database not initialized');
  return db
    .prepare(
      `
    SELECT id, COALESCE(thumbnail_path, image_path) AS src FROM posts
    WHERE thumb_blur IS NULL
      AND (thumbnail_path IS NOT NULL OR image_path IS NOT NULL)
  `,
    )
    .all() as { id: string; src: string }[];
}

function setThumbBlur(id: string, thumbBlur: string): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare('UPDATE posts SET thumb_blur = ? WHERE id = ?').run(thumbBlur, id);
}

// Record the downloaded file for one media slide. Inserts the row if the
// post predates having per-slide media (defensive — backfill usually covers it).
function updateMediaPath(
  postId: string,
  position: number,
  localPath: string,
  mediaType: Shelfy.PostMediaType = 'image',
): void {
  if (!db) throw new Error('Database not initialized');
  const res = db
    .prepare('UPDATE post_media SET local_path = ? WHERE post_id = ? AND position = ?')
    .run(localPath, postId, position);
  if (res.changes === 0) {
    db.prepare(
      'INSERT OR IGNORE INTO post_media (post_id, position, media_type, source_url, local_path) VALUES (?, ?, ?, NULL, ?)',
    ).run(postId, position, mediaType, localPath);
  }
  invalidateStatsCache();
}

// Serialize an array field for storage: undefined → skip (handled by caller),
// null → explicit NULL, otherwise a JSON array string (coercing non-arrays to []).
function serializeArrayField(v: unknown[] | null): string | null {
  if (v === null) return null;
  return JSON.stringify(Array.isArray(v) ? v : []);
}

// Persist the result of a local VLM analysis. Distinguishes "field not provided"
// (undefined → column left untouched) from "field explicitly null" (→ writes
// NULL, e.g. to clear ai_status when a job is cancelled). Only the keys present
// in `fields` are written; passing status alone (e.g. 'analyzing'/'error')
// updates just the status, and {status: null} actually clears it.
function updateAiAnalysis(id: string, fields: AiFields = {}): void {
  if (!db) throw new Error('Database not initialized');
  const tx = db.transaction(() => applyAiAnalysis(id, fields));
  tx();
  invalidateGlobalCaches();
}

// Writes the user-authored layer (personal note + manual tags), independent of
// the AI fields. Only the keys present in `fields` are touched:
//   - note: string|null → user_note
//   - manualTags: string[]|null → user_tags (JSON) + resynced post_tags tier='manual'
// Manual tags are canonicalized via resolveAlias (same as AI tags) so they follow
// the archive's tag aliasing, and deduped on the canonical norm. The user_tags
// JSON column is the display source of truth; the post_tags mirror exists for tag
// filtering and chat-search vocabulary. Single transaction; invalidates caches.
function updateUserContent(
  id: string,
  fields: { note?: string | null; manualTags?: string[] | null } = {},
): void {
  if (!db) throw new Error('Database not initialized');
  const { note, manualTags } = fields;
  const tx = db.transaction(() => {
    const sets: string[] = [];
    const params: (string | null)[] = [];
    if (note !== undefined) {
      sets.push('user_note = ?');
      params.push(note);
    }
    if (manualTags !== undefined) {
      sets.push('user_tags = ?');
      params.push(serializeArrayField(manualTags));
    }
    if (sets.length) {
      db!.prepare(`UPDATE posts SET ${sets.join(', ')} WHERE id = ?`).run(...params, id);
    }
    if (manualTags !== undefined) {
      db!.prepare("DELETE FROM post_tags WHERE post_id = ? AND tier = 'manual'").run(id);
      const ins = db!.prepare(
        "INSERT OR IGNORE INTO post_tags (post_id, tag_norm, tag_form, tier) VALUES (?, ?, ?, 'manual')",
      );
      const seen = new Set<string>();
      for (const { norm, form } of normalizeTagRows(manualTags)) {
        const c = resolveAlias(norm);
        if (!c.norm || seen.has(c.norm)) continue;
        seen.add(c.norm);
        const outForm = c.norm === norm ? form : c.form || form;
        ins.run(id, c.norm, outForm);
      }
    }
  });
  tx();
  invalidateGlobalCaches();
}

// Insert a manual bookmark (platform='manual'): a user-added post built from local
// files, with a personal note + manual tags. `media` is the ordered slide list
// from electron/bookmarks.js ([{ type, localPath, sourcePath, kind }]); local_path
// is the renderable preview/original and source_url carries the original file path
// (so the modal can open it). The note/tags are written via updateUserContent so
// they share the exact manual-layer semantics (user_tags JSON + post_tags
// tier='manual', alias-canonicalized). Returns { id }.
function addManualBookmark({
  id,
  note = '',
  tags = [],
  mediaType = 'image',
  thumbnailPath = null,
  imagePath = null,
  videoPath = null,
  media = [],
}: {
  id?: string;
  note?: string;
  tags?: string[];
  mediaType?: string;
  thumbnailPath?: string | null;
  imagePath?: string | null;
  videoPath?: string | null;
  media?: { type?: string; kind?: string; sourcePath?: string | null; localPath?: string | null }[];
} = {}): { id: string } {
  if (!db) throw new Error('Database not initialized');
  if (!id) throw new Error('addManualBookmark: missing id');
  const nowIso = new Date().toISOString();
  const tx = db.transaction(() => {
    db!
      .prepare(
        `INSERT INTO posts
         (id, platform, media_type, timestamp, thumbnail_path, image_path, video_path,
          media_count, imported_at)
       VALUES (?, 'manual', ?, ?, ?, ?, ?, ?, unixepoch())`,
      )
      .run(id, mediaType, nowIso, thumbnailPath, imagePath, videoPath, media.length || 1);
    const ins = db!.prepare(
      'INSERT OR IGNORE INTO post_media (post_id, position, media_type, source_url, local_path) VALUES (?, ?, ?, ?, ?)',
    );
    media.forEach((m, i) => {
      const type = m.type === 'video' ? 'video' : m.kind === 'file' ? 'file' : 'image';
      ins.run(id, i, type, m.sourcePath ?? null, m.localPath ?? null);
    });
  });
  tx();
  // Personal note + manual tags (own transaction + cache invalidation).
  updateUserContent(id, {
    note: note ? note : null,
    manualTags: Array.isArray(tags) ? tags : [],
  });
  invalidateGlobalCaches();
  return { id };
}

// Deletes the AI-generated description (ai_description → NULL) for one or more
// posts, leaving tags/entities/keywords intact. Also resets ai_status so the post
// counts as "not analyzed" again — that's what makes it reappear in the overview's
// unanalyzed count and in "Analizza mancanti", so the description can be
// regenerated. Runs all ids in a single transaction. Returns posts affected.
function clearAiDescriptions(ids: string | string[] = []): number {
  if (!db) throw new Error('Database not initialized');
  const list = (Array.isArray(ids) ? ids : [ids]).filter(Boolean);
  if (list.length === 0) return 0;
  const tx = db.transaction(() => {
    for (const id of list) applyAiAnalysis(id, { description: null, status: null });
  });
  tx();
  invalidateGlobalCaches();
  return list.length;
}

// Deletes the AI-generated tags (ai_tags → NULL, derived post_tags rows dropped)
// for one or more posts, leaving description/entities/keywords intact. Mirrors
// clearAiDescriptions: also resets ai_status so the post counts as "not analyzed"
// again and can be re-tagged via "Analizza". Single transaction. Returns posts
// affected.
function clearAiTags(ids: string | string[] = []): number {
  if (!db) throw new Error('Database not initialized');
  const list = (Array.isArray(ids) ? ids : [ids]).filter(Boolean);
  if (list.length === 0) return 0;
  const tx = db.transaction(() => {
    for (const id of list) applyAiAnalysis(id, { tags: null, status: null });
  });
  tx();
  invalidateGlobalCaches();
  return list.length;
}

// Core of updateAiAnalysis WITHOUT its own transaction wrapper, so it can be
// reused inside another open transaction (e.g. bulkUpsert's insertMany). Writes
// the ai_* columns provided in `fields` and resyncs the derived tag/entity
// indexes for the fields actually present. Same undefined/null semantics as
// updateAiAnalysis. Must be called from within a db.transaction.
function applyAiAnalysis(id: string, fields: AiFields = {}): void {
  const {
    description,
    tags,
    status,
    model,
    category,
    contentType,
    entities,
    keywords,
    language,
    saveReason,
    analyzedAt,
    generalTags,
    specificTags,
  } = fields;

  const sets: string[] = [];
  const params: (string | number | null)[] = [];
  const add = (col: string, value: string | number | null): void => {
    sets.push(`${col} = ?`);
    params.push(value);
  };

  if (description !== undefined) add('ai_description', description);
  if (tags !== undefined) add('ai_tags', serializeArrayField(tags));
  if (status !== undefined) add('ai_status', status);
  if (model !== undefined) add('ai_model', model);
  if (category !== undefined) add('ai_category', category);
  if (contentType !== undefined) add('ai_content_type', contentType);
  if (entities !== undefined) add('ai_entities', serializeArrayField(entities));
  if (keywords !== undefined) add('ai_keywords', serializeArrayField(keywords));
  if (language !== undefined) add('ai_language', language);
  if (saveReason !== undefined) add('ai_save_reason', saveReason);

  // analyzed_at: an explicit value (e.g. carried by an import) wins; otherwise
  // stamp it only when the analysis actually completes.
  if (analyzedAt !== undefined) add('ai_analyzed_at', analyzedAt);
  else if (status === 'done') add('ai_analyzed_at', Math.floor(Date.now() / 1000));

  // Resync the derived indexes only for the fields actually provided: tags
  // present (incl. null/[]) → rebuild post_tags; entities present → post_entities.
  const syncTags = tags !== undefined;
  const syncEntities = entities !== undefined;

  if (!sets.length && !syncTags && !syncEntities) return; // nothing provided → no-op

  if (sets.length) {
    db!.prepare(`UPDATE posts SET ${sets.join(', ')} WHERE id = ?`).run(...params, id);
  }
  if (syncTags) {
    // Drop only the AI-owned tag rows (tier general/specific/legacy-NULL); the
    // user's manual tags (tier='manual') are a separate layer and must survive an
    // analysis regeneration or an AI-tags clear.
    db!
      .prepare(
        "DELETE FROM post_tags WHERE post_id = ? AND (tier IS NULL OR tier IN ('general','specific'))",
      )
      .run(id);
    const ins = db!.prepare(
      'INSERT OR IGNORE INTO post_tags (post_id, tag_norm, tag_form, tier) VALUES (?, ?, ?, ?)',
    );

    // Tier per-norm dai campi opzionali generalTags/specificTags: 'specific' vince
    // su 'general'; NULL quando i due campi non sono forniti (legacy/edit manuale).
    // I tier sono indicizzati sul norm POST-alias così seguono la canonicalizzazione.
    const tierByNorm = new Map<string, Shelfy.TagTier>();
    const haveTiers = Array.isArray(generalTags) || Array.isArray(specificTags);
    if (haveTiers) {
      for (const t of Array.isArray(generalTags) ? generalTags : []) {
        const n = resolveAlias(
          String(t ?? '')
            .trim()
            .toLowerCase(),
        ).norm;
        if (n) tierByNorm.set(n, 'general');
      }
      for (const t of Array.isArray(specificTags) ? specificTags : []) {
        const n = resolveAlias(
          String(t ?? '')
            .trim()
            .toLowerCase(),
        ).norm;
        if (n) tierByNorm.set(n, 'specific'); // specific vince
      }
    }

    // Canonicalizza ogni norm via resolveAlias prima dell'inserimento, deduplicando
    // sui norm canonici (più sinonimi possono collassare sullo stesso canonico).
    const seen = new Set<string>();
    for (const { norm, form } of normalizeTagRows(tags)) {
      const c = resolveAlias(norm);
      if (!c.norm || seen.has(c.norm)) continue;
      seen.add(c.norm);
      // form: preferisci la display form canonica quando l'alias ha rimappato il norm,
      // altrimenti la form originale del post.
      const outForm = c.norm === norm ? form : c.form || form;
      ins.run(id, c.norm, outForm, haveTiers ? (tierByNorm.get(c.norm) ?? null) : null);
    }
  }
  if (syncEntities) {
    db!.prepare('DELETE FROM post_entities WHERE post_id = ?').run(id);
    const ins = db!.prepare(
      'INSERT OR IGNORE INTO post_entities (post_id, ent_norm, ent_form) VALUES (?, ?, ?)',
    );
    for (const { norm, form } of normalizeTagRows(entities)) ins.run(id, norm, form);
  }
}

// Extract the AI analysis fields from a normalized imported post into the shape
// applyAiAnalysis expects, keeping only the keys actually provided (undefined
// means "not in the export" → leave the column untouched). Returns null when
// the record carries no AI data at all.
function extractAiFields(post: PostInput): AiFields | null {
  const out: AiFields = {};
  let any = false;
  const map: Record<string, keyof AiFields> = {
    aiDescription: 'description',
    aiTags: 'tags',
    aiStatus: 'status',
    aiModel: 'model',
    aiCategory: 'category',
    aiContentType: 'contentType',
    aiEntities: 'entities',
    aiKeywords: 'keywords',
    aiLanguage: 'language',
    aiSaveReason: 'saveReason',
    aiAnalyzedAt: 'analyzedAt',
    aiGeneralTags: 'generalTags',
    aiSpecificTags: 'specificTags',
  };
  const src = post as unknown as Record<string, unknown>;
  for (const [from, dst] of Object.entries(map)) {
    if (src[from] !== undefined) {
      (out as Record<string, unknown>)[dst] = src[from];
      any = true;
    }
  }
  return any ? out : null;
}

// Returns the `limit` most frequently used AI tags across all analyzed posts.
// Counting is case-insensitive (so "Cucina" and "cucina" are the same tag), but
// the returned form is the most common casing seen for that tag.
function getFrequentTags(limit = 30): string[] {
  if (!db) throw new Error('Database not initialized');
  // Count over the indexed post_tags table (tag_norm = trim().toLowerCase()),
  // like getTagStats, instead of full-scanning + JSON.parsing every post's
  // ai_tags (this is called once per post in the analysis loop). Returns the
  // same shape as before: an array of display-form strings, most frequent first.
  const top = db
    .prepare(
      `
      SELECT tag_norm AS norm, COUNT(*) AS count
      FROM post_tags GROUP BY tag_norm ORDER BY count DESC LIMIT ?
    `,
    )
    .all(limit) as { norm: string; count: number }[];
  if (!top.length) return [];

  const norms = top.map((r) => r.norm);
  const placeholders = norms.map(() => '?').join(',');
  const formRows = db
    .prepare(
      `
      SELECT tag_norm AS norm, tag_form AS form, COUNT(*) AS c
      FROM post_tags WHERE tag_norm IN (${placeholders})
      GROUP BY tag_norm, tag_form
    `,
    )
    .all(...norms) as { norm: string; form: string; c: number }[];
  const forms = new Map<string, Map<string, number>>(); // norm → Map(form → count)
  for (const r of formRows) {
    if (!forms.has(r.norm)) forms.set(r.norm, new Map());
    forms.get(r.norm)!.set(r.form, r.c);
  }

  return top.map((r) => bestForm(forms.get(r.norm), r.norm));
}

// ─── AI Tags (analytics + tag maintenance) ────────────────────────────────────

// Strip diacritics, lowercase and trim — the canonical key for grouping
// near-duplicate tags ("Città" / "citta" / " CITTA ").
function normalizeTag(s: unknown): string {
  return String(s).normalize('NFD').replace(/[̀-ͯ]/g, '').trim().toLowerCase();
}

// Bounded Levenshtein edit distance between two short strings.
function levenshtein(a: string, b: string): number {
  if (a === b) return 0;
  const m = a.length;
  const n = b.length;
  if (m === 0) return n;
  if (n === 0) return m;
  let prev = new Array<number>(n + 1);
  let cur = new Array<number>(n + 1);
  for (let j = 0; j <= n; j++) prev[j] = j;
  for (let i = 1; i <= m; i++) {
    cur[0] = i;
    const ca = a.charCodeAt(i - 1);
    for (let j = 1; j <= n; j++) {
      const cost = ca === b.charCodeAt(j - 1) ? 0 : 1;
      cur[j] = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost);
    }
    const tmp = prev;
    prev = cur;
    cur = tmp;
  }
  return prev[n];
}

// High-level counters for the AI Tags dashboard header.
function getAiOverview(): Shelfy.AiOverview {
  if (!db) throw new Error('Database not initialized');
  const total = (db.prepare('SELECT COUNT(*) as count FROM posts').get() as { count: number })
    .count;
  const analyzed = (
    db.prepare("SELECT COUNT(*) as count FROM posts WHERE ai_status = 'done'").get() as {
      count: number;
    }
  ).count;
  const unanalyzed = total - analyzed;

  const byCategory = db
    .prepare(
      "SELECT ai_category AS category, COUNT(*) AS count FROM posts WHERE ai_category IS NOT NULL AND ai_category != '' GROUP BY ai_category ORDER BY count DESC",
    )
    .all() as Shelfy.NamedCount<'category'>[];
  const byContentType = db
    .prepare(
      "SELECT ai_content_type AS contentType, COUNT(*) AS count FROM posts WHERE ai_content_type IS NOT NULL AND ai_content_type != '' GROUP BY ai_content_type ORDER BY count DESC",
    )
    .all() as Shelfy.NamedCount<'contentType'>[];
  const languages = db
    .prepare(
      "SELECT ai_language AS language, COUNT(*) AS count FROM posts WHERE ai_language IS NOT NULL AND ai_language != '' GROUP BY ai_language ORDER BY count DESC",
    )
    .all() as Shelfy.NamedCount<'language'>[];

  const uniqueTags = (
    db.prepare('SELECT COUNT(DISTINCT tag_norm) AS count FROM post_tags').get() as { count: number }
  ).count;
  const taggedPosts = (
    db.prepare('SELECT COUNT(DISTINCT post_id) AS count FROM post_tags').get() as { count: number }
  ).count;

  return {
    total,
    analyzed,
    unanalyzed,
    byCategory,
    byContentType,
    languages,
    uniqueTags,
    taggedPosts,
  };
}

// Per-tag stats: count, most-recent use, and category distribution. Tags are
// counted case-insensitively; the most-common casing is used as the display form.
function getTagStats({
  limit = 300,
  tier = null,
}: { limit?: number; tier?: 'general' | 'specific' | null } = {}): Shelfy.Tag[] {
  if (!db) throw new Error('Database not initialized');

  // Optional tier filter ('general'|'specific'): conta solo le righe di quel tier
  // (i tag legacy/edit manuali hanno tier NULL e restano fuori dal filtro). tier
  // = null → comportamento attuale (tutte le righe).
  const tierWhere = tier === 'general' || tier === 'specific' ? 'WHERE pt.tier = ?' : '';
  const tierParam: string[] = tierWhere ? [tier as string] : [];

  // Top tags by post count, straight off the indexed post_tags table.
  const top = db
    .prepare(
      `
      SELECT tag_norm AS norm, COUNT(*) AS count, MAX(p.timestamp) AS lastUsed
      FROM post_tags pt JOIN posts p ON p.id = pt.post_id
      ${tierWhere}
      GROUP BY tag_norm
      ORDER BY count DESC
      LIMIT ?
    `,
    )
    .all(...tierParam, limit) as { norm: string; count: number; lastUsed: string | null }[];
  if (!top.length) return [];

  const norms = top.map((r) => r.norm);
  const placeholders = norms.map(() => '?').join(',');

  // Display form = most frequent original casing per norm.
  const formRows = db
    .prepare(
      `
      SELECT tag_norm AS norm, tag_form AS form, COUNT(*) AS c
      FROM post_tags WHERE tag_norm IN (${placeholders})
      GROUP BY tag_norm, tag_form
    `,
    )
    .all(...norms) as { norm: string; form: string; c: number }[];
  const forms = new Map<string, Map<string, number>>(); // norm → Map(form → count)
  for (const r of formRows) {
    if (!forms.has(r.norm)) forms.set(r.norm, new Map());
    forms.get(r.norm)!.set(r.form, r.c);
  }

  // Category distribution per norm via JOIN on posts.ai_category.
  const catRows = db
    .prepare(
      `
      SELECT pt.tag_norm AS norm, p.ai_category AS category, COUNT(*) AS count
      FROM post_tags pt JOIN posts p ON p.id = pt.post_id
      WHERE pt.tag_norm IN (${placeholders})
        AND p.ai_category IS NOT NULL AND p.ai_category != ''
      GROUP BY pt.tag_norm, p.ai_category
    `,
    )
    .all(...norms) as { norm: string; category: string; count: number }[];
  const cats = new Map<string, Shelfy.TagCategoryCount[]>(); // norm → [{category, count}]
  for (const r of catRows) {
    if (!cats.has(r.norm)) cats.set(r.norm, []);
    cats.get(r.norm)!.push({ category: r.category, count: r.count });
  }

  return top.map((r) => ({
    tag: bestForm(forms.get(r.norm), r.norm),
    count: r.count,
    lastUsed: r.lastUsed || null,
    categories: (cats.get(r.norm) || []).slice().sort((a, b) => b.count - a.count),
  }));
}

// Pick the most frequent original casing for a normalized key.
function bestForm(fm: Map<string, number> | undefined, fallback: string): string {
  if (!fm) return fallback;
  let best = fallback;
  let bestCount = -1;
  for (const [form, c] of fm) {
    if (c > bestCount) {
      best = form;
      bestCount = c;
    }
  }
  return best;
}

// Per-entity counts from ai_entities. Dedup is case-insensitive but the
// original casing is preserved (entities are proper nouns).
function getEntityStats({ limit = 300 }: { limit?: number } = {}): Shelfy.Entity[] {
  if (!db) throw new Error('Database not initialized');
  const top = db
    .prepare(
      `
      SELECT ent_norm AS norm, COUNT(*) AS count
      FROM post_entities GROUP BY ent_norm ORDER BY count DESC LIMIT ?
    `,
    )
    .all(limit) as { norm: string; count: number }[];
  if (!top.length) return [];

  const norms = top.map((r) => r.norm);
  const placeholders = norms.map(() => '?').join(',');
  const formRows = db
    .prepare(
      `
      SELECT ent_norm AS norm, ent_form AS form, COUNT(*) AS c
      FROM post_entities WHERE ent_norm IN (${placeholders})
      GROUP BY ent_norm, ent_form
    `,
    )
    .all(...norms) as { norm: string; form: string; c: number }[];
  const forms = new Map<string, Map<string, number>>(); // norm → Map(form → count)
  for (const r of formRows) {
    if (!forms.has(r.norm)) forms.set(r.norm, new Map());
    forms.get(r.norm)!.set(r.form, r.c);
  }

  return top.map((r) => ({ entity: bestForm(forms.get(r.norm), r.norm), count: r.count }));
}

// Tags that most often co-occur with `tag` in the same post.
function getTagCooccurrence(
  tag: string,
  { limit = 8 }: { limit?: number } = {},
): Shelfy.TagCount[] {
  if (!db) throw new Error('Database not initialized');
  // normalizeTag strips diacritics, but post_tags.tag_norm only lowercases; use
  // a plain lowercase/trim here so the target matches the indexed norm.
  const target = String(tag ?? '')
    .trim()
    .toLowerCase();
  if (!target) return [];

  // Co-occurring tags: self-join post_tags on the same post, excluding the target.
  const rows = db
    .prepare(
      `
      SELECT b.tag_norm AS norm, COUNT(*) AS count
      FROM post_tags a JOIN post_tags b ON b.post_id = a.post_id
      WHERE a.tag_norm = ? AND b.tag_norm != a.tag_norm
      GROUP BY b.tag_norm
      ORDER BY count DESC
      LIMIT ?
    `,
    )
    .all(target, limit) as { norm: string; count: number }[];
  if (!rows.length) return [];

  const norms = rows.map((r) => r.norm);
  const placeholders = norms.map(() => '?').join(',');
  const formRows = db
    .prepare(
      `
      SELECT tag_norm AS norm, tag_form AS form, COUNT(*) AS c
      FROM post_tags WHERE tag_norm IN (${placeholders})
      GROUP BY tag_norm, tag_form
    `,
    )
    .all(...norms) as { norm: string; form: string; c: number }[];
  const forms = new Map<string, Map<string, number>>();
  for (const r of formRows) {
    if (!forms.has(r.norm)) forms.set(r.norm, new Map());
    forms.get(r.norm)!.set(r.form, r.c);
  }

  return rows.map((r) => ({ tag: bestForm(forms.get(r.norm), r.norm), count: r.count }));
}

// Lexical retrieval over the FULL tag vocabulary (not just the top-frequency
// slice getTagStats returns): given a free-text query, return archive tags whose
// normalized form contains any query token, most-used first. This is what lets
// search surface long-tail "specific" tags (e.g. 'cuffie') that sit far below
// the frequency cutoff and are otherwise invisible to the model.
// Tokens are split on non-alphanumerics, so LIKE wildcards can't leak in.
function searchTagsByText(
  query: string,
  { limit = 40, minTermLen = 3 }: { limit?: number; minTermLen?: number } = {},
): Shelfy.TagCount[] {
  if (!db) throw new Error('Database not initialized');
  const terms = contentTermsOrRaw(query, { minLen: minTermLen });
  if (!terms.length) return [];

  const where = terms.map(() => "tag_norm LIKE ? ESCAPE '\\'").join(' OR ');
  const params = terms.map((t) => `%${likeEscape(t)}%`);
  const rows = db
    .prepare(
      `
      SELECT tag_norm AS norm, COUNT(*) AS count
      FROM post_tags
      WHERE ${where}
      GROUP BY tag_norm
      ORDER BY count DESC
      LIMIT ?
    `,
    )
    .all(...params, limit) as { norm: string; count: number }[];
  if (!rows.length) return [];

  const norms = rows.map((r) => r.norm);
  const placeholders = norms.map(() => '?').join(',');
  const formRows = db
    .prepare(
      `
      SELECT tag_norm AS norm, tag_form AS form, COUNT(*) AS c
      FROM post_tags WHERE tag_norm IN (${placeholders})
      GROUP BY tag_norm, tag_form
    `,
    )
    .all(...norms) as { norm: string; form: string; c: number }[];
  const forms = new Map<string, Map<string, number>>();
  for (const r of formRows) {
    if (!forms.has(r.norm)) forms.set(r.norm, new Map());
    forms.get(r.norm)!.set(r.form, r.c);
  }

  return rows.map((r) => ({ tag: bestForm(forms.get(r.norm), r.norm), count: r.count }));
}

// Semantic fallback for tag discovery: searches post text fields (caption, ai_description,
// ai_tags) for query terms, then returns the most-frequent tags associated with those
// matching posts. Used when lexical tag-name search yields too few results — e.g. a
// query like "airpods" finds no tag named "airpods" but finds posts whose caption
// mentions it, and those posts carry tags like "cuffie", "prodotto tecnologico", etc.
function getTopTagsForTextQuery(
  query: string,
  {
    limit = 40,
    postLimit = 80,
    minTermLen = 3,
  }: { limit?: number; postLimit?: number; minTermLen?: number } = {},
): Shelfy.TagCount[] {
  if (!db) throw new Error('Database not initialized');
  const terms = contentTermsOrRaw(query, { minLen: minTermLen });
  if (!terms.length) return [];

  const textCols = ['text', 'ai_description', 'ai_tags'];
  const whereClauses = terms.flatMap(() => textCols.map((col) => `${col} LIKE ? ESCAPE '\\'`));
  const params = terms.flatMap((t) => textCols.map(() => `%${likeEscape(t)}%`));

  const postIds = (
    db
      .prepare(
        `SELECT id FROM posts WHERE ${whereClauses.join(' OR ')} ORDER BY timestamp DESC, id LIMIT ?`,
      )
      .all(...params, postLimit) as { id: string }[]
  ).map((r) => r.id);

  if (!postIds.length) return [];

  const placeholders = postIds.map(() => '?').join(',');
  const rows = db
    .prepare(
      `
      SELECT tag_norm AS norm, COUNT(*) AS count
      FROM post_tags
      WHERE post_id IN (${placeholders})
      GROUP BY tag_norm
      ORDER BY count DESC
      LIMIT ?
    `,
    )
    .all(...postIds, limit) as { norm: string; count: number }[];

  if (!rows.length) return [];

  const norms = rows.map((r) => r.norm);
  const ph2 = norms.map(() => '?').join(',');
  const formRows = db
    .prepare(
      `SELECT tag_norm AS norm, tag_form AS form, COUNT(*) AS c FROM post_tags WHERE tag_norm IN (${ph2}) GROUP BY tag_norm, tag_form`,
    )
    .all(...norms) as { norm: string; form: string; c: number }[];
  const forms = new Map<string, Map<string, number>>();
  for (const r of formRows) {
    if (!forms.has(r.norm)) forms.set(r.norm, new Map());
    forms.get(r.norm)!.set(r.form, r.c);
  }

  return rows.map((r) => ({ tag: bestForm(forms.get(r.norm), r.norm), count: r.count }));
}

// Per-norm post count across the WHOLE archive — the denominator for tag lift.
// Memoized: the archive is static within a process, so we scan post_tags once.
let _globalTagCounts: Map<string, number> | null = null;
function getGlobalTagCounts(): Map<string, number> {
  if (_globalTagCounts) return _globalTagCounts;
  const m = new Map<string, number>();
  const rows = db!
    .prepare('SELECT tag_norm AS t, COUNT(*) c FROM post_tags GROUP BY tag_norm')
    .all() as { t: string; c: number }[];
  for (const r of rows) m.set(r.t, r.c);
  _globalTagCounts = m;
  return m;
}

// DISTINCTIVENESS-ranked tag discovery from post CONTENT (the noise-killer).
//
// Like getTopTagsForTextQuery, this finds posts whose content mentions the query
// terms — but it scans ai_keywords too (omitted by the old fallback) and ranks the
// harvested tags by how DISTINCTIVE they are of the matched set instead of by raw
// frequency. Raw frequency surfaces archive-dominant tags (e.g. "design", "3d")
// that ride along on every query; distinctiveness surfaces tags concentrated in the
// matched posts and rare elsewhere.
//
// IDF-WEIGHTED MATCHED SET — the key to killing noise on hard queries. A query like
// "accessori di cuffie … Airpods" mixes a rare, topic-defining term ("cuffie" df=8,
// "airpods" df=5) with vague ones ("accessori" df=26, "reference" df=109). Matching
// on ANY term (the old behaviour) floods the set with off-topic posts, so the tags
// harvested off them are noise. Instead each matched post is weighted by the SUM of
// its matched terms' IDF, and we also remember its single most-distinctive matched
// term (pBestIdf). Posts riding only on common query words barely contribute.
//
// For each tag carried by ≥1 matched post:
//   inSet   = matched posts carrying it (raw count)
//   wInSet  = Σ post weight over those posts (IDF-weighted prevalence)
//   lift    = inSet / globalCount        (0..1; ~1 if nearly exclusive to the topic)
//   score   = wInSet · lift^liftExp      (lift exponent crushes low-lift filler)
// Gating (drops the noise the harness penalises):
//   • lift < minLift                         → weak topic association, drop.
//   • inSet ≥ minInSet: require lift ≥ massMinLift (a frequent tag must still be
//                       characteristic of the topic, not archive-wide boilerplate).
//   • inSet  < minInSet (a 1-off): keep ONLY if its post matched a near-top-IDF
//                       query term (pBestIdf ≥ soloIdfFrac · maxIdf). This admits a
//                       solo tag sitting on a genuinely on-topic "cuffie"/"airpods"
//                       post while rejecting one sitting on an "accessori"-only post.
// Returns [{ tag, inSet, count, lift, score }] sorted by score desc.
function getTagDistinctivenessForTextQuery(
  query: string,
  {
    limit = 60,
    postLimit = 4000,
    minTermLen = 3,
    minLift = 0.12,
    minInSet = 2,
    massMinLift = 0.2,
    soloIdfFrac = 0.9,
    liftExp = 3,
  }: {
    limit?: number;
    postLimit?: number;
    minTermLen?: number;
    minLift?: number;
    minInSet?: number;
    massMinLift?: number;
    soloIdfFrac?: number;
    liftExp?: number;
  } = {},
): Shelfy.TagDistinctiveness[] {
  if (!db) throw new Error('Database not initialized');
  const terms = contentTermsOrRaw(query, { minLen: minTermLen });
  if (!terms.length) return [];

  // 1. Per-term document frequency → IDF. Total posts is the IDF denominator base.
  // user_note/user_tags ride along so the user's own note/tags steer retrieval
  // exactly like the AI fields (same column set as buildPostFilter's term search).
  // ONE scan computes COUNT(*) and every term's df (SUM(CASE …) per term) instead
  // of the old 1 + N separate full LIKE scans.
  const cols = ['text', 'ai_description', 'ai_tags', 'ai_keywords', 'user_note', 'user_tags'];
  const termWhere = cols.map((c) => `${c} LIKE ? ESCAPE '\\'`).join(' OR ');
  const termLikes = terms.flatMap((t) => cols.map(() => `%${likeEscape(t)}%`));
  const dfExprs = terms.map((_, i) => `SUM(CASE WHEN ${termWhere} THEN 1 ELSE 0 END) AS df${i}`);
  const dfRow = db
    .prepare(`SELECT COUNT(*) AS n, ${dfExprs.join(', ')} FROM posts`)
    .get(...termLikes) as Record<string, number>;
  const total = dfRow.n || 0;
  const idf = new Map<string, number>();
  let maxIdf = 0;
  terms.forEach((t, i) => {
    const df = dfRow[`df${i}`] || 0;
    const v = Math.log((total + 1) / (df + 1));
    idf.set(t, v);
    if (v > maxIdf) maxIdf = v;
  });

  // 2. Matched posts, each weighted by its matched terms' IDF (pw) and tagged with
  //    its single most-distinctive matched term's IDF (pBest), for the solo gate.
  //    ONE scan with a per-term match flag per row replaces the old per-term LIKE
  //    scans. Rows stream in the same (timestamp DESC, id) order the per-term
  //    queries used and each term's postLimit cap is applied here, so the
  //    harvested set is IDENTICAL; iteration stops as soon as every cap is full.
  const pw = new Map<string, number>(); // postId → Σ idf of matched terms
  const pBest = new Map<string, number>(); // postId → max idf of any matched term
  const flagExprs = terms.map((_, i) => `(CASE WHEN ${termWhere} THEN 1 ELSE 0 END) AS m${i}`);
  const anyTermWhere = terms.map(() => `(${termWhere})`).join(' OR ');
  const matchStmt = db.prepare(
    `SELECT id, ${flagExprs.join(', ')} FROM posts WHERE ${anyTermWhere} ORDER BY timestamp DESC, id`,
  );
  const remaining = terms.map(() => postLimit);
  let openCaps = terms.length;
  for (const r of matchStmt.iterate(...termLikes, ...termLikes) as IterableIterator<
    Record<string, number | string>
  >) {
    for (let i = 0; i < terms.length; i++) {
      if (remaining[i] <= 0 || !r[`m${i}`]) continue;
      remaining[i] -= 1;
      if (remaining[i] === 0) openCaps -= 1;
      const w = idf.get(terms[i])!;
      const rid = r.id as string;
      pw.set(rid, (pw.get(rid) || 0) + w);
      if (w > (pBest.get(rid) || 0)) pBest.set(rid, w);
    }
    if (!openCaps) break;
  }
  if (!pw.size) return [];

  // 3. Per-tag association over the matched posts: raw count, IDF-weighted sum, and
  //    the best single-term IDF among the posts carrying it (solo-gate input).
  const postIds = [...pw.keys()];
  const tagRows = chunkedInQuery<{ norm: string; pid: string }>(
    postIds,
    (ph) => `SELECT tag_norm AS norm, post_id AS pid FROM post_tags WHERE post_id IN (${ph})`,
  );
  if (!tagRows.length) return [];
  const agg = new Map<string, { inSet: number; wInSet: number; bestIdf: number }>(); // norm → { inSet, wInSet, bestIdf }
  for (const r of tagRows) {
    let a = agg.get(r.norm);
    if (!a) {
      a = { inSet: 0, wInSet: 0, bestIdf: 0 };
      agg.set(r.norm, a);
    }
    a.inSet += 1;
    a.wInSet += pw.get(r.pid) || 0;
    const b = pBest.get(r.pid) || 0;
    if (b > a.bestIdf) a.bestIdf = b;
  }

  // 4. Score by distinctiveness, gate out near-zero-association noise.
  const globalCounts = getGlobalTagCounts();
  const soloIdfMin = soloIdfFrac * maxIdf;
  const scored: { norm: string; inSet: number; global: number; lift: number; score: number }[] = [];
  for (const [norm, a] of agg) {
    const global = globalCounts.get(norm) || a.inSet;
    const lift = a.inSet / global;
    if (lift < minLift) continue;
    if (a.inSet >= minInSet) {
      if (lift < massMinLift) continue; // frequent but not characteristic → drop
    } else if (a.bestIdf < soloIdfMin) {
      continue; // 1-off riding only a common query word → drop
    }
    scored.push({ norm, inSet: a.inSet, global, lift, score: a.wInSet * Math.pow(lift, liftExp) });
  }
  if (!scored.length) return [];
  scored.sort((a, b) => b.score - a.score || b.lift - a.lift || (a.norm < b.norm ? -1 : 1));
  const top = scored.slice(0, limit);

  // 5. Resolve display forms.
  const norms = top.map((r) => r.norm);
  const ph2 = norms.map(() => '?').join(',');
  const formRows = db
    .prepare(
      `SELECT tag_norm AS norm, tag_form AS form, COUNT(*) AS c FROM post_tags WHERE tag_norm IN (${ph2}) GROUP BY tag_norm, tag_form`,
    )
    .all(...norms) as { norm: string; form: string; c: number }[];
  const forms = new Map<string, Map<string, number>>();
  for (const r of formRows) {
    if (!forms.has(r.norm)) forms.set(r.norm, new Map());
    forms.get(r.norm)!.set(r.form, r.c);
  }
  return top.map((r) => ({
    tag: bestForm(forms.get(r.norm), r.norm),
    inSet: r.inSet,
    count: r.global,
    lift: r.lift,
    score: r.score,
  }));
}

// ── KEYWORD retrieval (suggested search words, from ai_keywords) ────────────────
//
// "Parole suggerite": short words/phrases the user might want to search for in the
// static + AI-generated descriptions. The signal lives in posts.ai_keywords (a
// JSON array of short IT strings, e.g. ["custodia airpods","prototipo airpods"]).
//
// getKeywordsForTextQuery(query):
//   1. Find posts whose CONTENT terms (extractContentTerms, parameterized LIKE,
//      likeEscape) appear in text/ai_description/ai_tags/ai_keywords.
//   2. Harvest those posts' ai_keywords (try/catch per row — tolerate garbage),
//      tallying per-phrase counts in the matched set.
//   3. Rank by DISTINCTIVENESS, not raw frequency: most ai_keywords phrases are
//      archive-wide singletons, so phrase frequency is useless. Instead we score
//      each phrase by its TOKENS' distinctiveness to the matched set — a token
//      that is common among the matched posts but rare globally (high lift) marks
//      a phrase as on-topic, while boilerplate tokens ("design", "motion",
//      "creative", "generative") that dominate the whole archive are down-weighted.
//      Phrases that literally contain a query content term get a strong boost.
//   4. De-duplicate (case/whitespace) and collapse phrases that are substrings of
//      a higher-ranked, already-kept phrase. Returns clean short words/phrases.

// Splits a keyword phrase into lowercased content tokens (>=3 chars, or the short
// whitelist), reusing the same notion of "content word" as the query tokenizer.
function keywordTokens(phrase: unknown): string[] {
  return String(phrase ?? '')
    .toLowerCase()
    .split(/[^\p{L}\p{N}]+/u)
    .map((t) => t.trim())
    .filter((t) => t && (t.length >= 3 || SHORT_CONTENT_TERMS.has(t)));
}

function getKeywordsForTextQuery(
  query: string,
  {
    limit = 12,
    postLimit = 600,
    minTermLen = 3,
  }: { limit?: number; postLimit?: number; minTermLen?: number } = {},
): string[] {
  if (!db) throw new Error('Database not initialized');
  const terms = contentTermsOrRaw(query, { minLen: minTermLen });
  if (!terms.length) return [];

  // 1. Posts matching ANY query content term across the text columns + ai_keywords.
  // user_note/user_tags included so personal notes/tags pull a post into the
  // matched set (same column set as buildPostFilter's term search).
  const cols = ['text', 'ai_description', 'ai_tags', 'ai_keywords', 'user_note', 'user_tags'];
  const whereClauses = terms.flatMap(() => cols.map((c) => `${c} LIKE ? ESCAPE '\\'`));
  const params = terms.flatMap((t) => cols.map(() => `%${likeEscape(t)}%`));
  const rows = db
    .prepare(
      `SELECT ai_keywords FROM posts WHERE (${whereClauses.join(' OR ')}) AND ai_keywords IS NOT NULL LIMIT ?`,
    )
    .all(...params, postLimit) as { ai_keywords: string }[];
  if (!rows.length) return [];

  // 2. Harvest keyword phrases from the matched posts (garbage-tolerant), tallying
  //    per-phrase matched-set count and per-token matched-set count.
  const phraseCount = new Map<string, number>(); // norm phrase → count in matched set
  const phraseForm = new Map<string, string>(); // norm phrase → display form (first seen, trimmed)
  const setTokenCount = new Map<string, number>(); // token → count in matched set
  for (const row of rows) {
    let arr: unknown;
    try {
      arr = JSON.parse(row.ai_keywords);
    } catch {
      continue;
    }
    if (!Array.isArray(arr)) continue;
    for (const k of arr) {
      if (typeof k !== 'string') continue;
      const form = k.trim();
      if (!form) continue;
      const norm = form.toLowerCase();
      if (!phraseCount.has(norm)) phraseForm.set(norm, form);
      phraseCount.set(norm, (phraseCount.get(norm) || 0) + 1);
      for (const tok of new Set(keywordTokens(norm))) {
        setTokenCount.set(tok, (setTokenCount.get(tok) || 0) + 1);
      }
    }
  }
  if (!phraseCount.size) return [];

  // Global token frequency across ALL ai_keywords, for the distinctiveness ratio.
  // Cached so repeated queries don't rescan the archive every call.
  const globalTokenCount = getGlobalKeywordTokenCounts();
  // Total keyword occurrences — the denominator for an IDF-like rarity factor.
  let globalTotal = 0;
  for (const c of globalTokenCount.values()) globalTotal += c;

  // Query content terms as a lowercased set, for the on-topic boost.
  const querySet = new Set(terms.map((t) => t.toLowerCase()));
  const queryHit = (tok: string): boolean =>
    querySet.has(tok) || [...querySet].some((q) => tok.includes(q) || q.includes(tok));

  // 3. Score each phrase by two complementary, rarity-aware on-topic signals:
  //    (a) coverage — for each DISTINCT query content term the phrase contains, add
  //        its IDF rarity. This is the strongest "parole suggerite" signal: the user
  //        literally typed the word. IDF weighting means a rare on-topic query token
  //        ("prodotto", "accessori", "cuffie", "tipografia") contributes a lot, while
  //        an archive-wide query word that is also boilerplate ("design": ~3000
  //        occurrences) contributes almost nothing — so generic "design …" filler
  //        can't win on the strength of the word "design" alone. This is what surfaces
  //        the rare gold keywords for broad queries (e.g. "product design") whose
  //        matched set is otherwise dominated by design/3d filler.
  //    (b) distinctiveness — prevalence × lift^DIST_LIFT_EXP over the phrase's tokens.
  //        A token must be present in the matched set AND characteristic of it (high
  //        lift) to count; the lift EXPONENT crushes high-count-but-low-lift filler
  //        ("design"/"3d", lift ≈ 0.2) while preserving tokens nearly exclusive to the
  //        matched set ("airpods", "fluid", "shader", lift ≈ 1). This rewards phrases
  //        built from words genuinely typical of the topic, even when they aren't in
  //        the query verbatim. Requiring inSet ≥ 2 drops 1-off proper-noun noise.
  //    distinctiveness is down-weighted (DIST_WEIGHT) so coverage leads but a tight,
  //    on-topic phrase still ranks above a coverage-tied one. Constants picked at the
  //    centre of a stable 5/5 plateau on the eval harness.
  const DIST_LIFT_EXP = 3.2;
  const DIST_WEIGHT = 0.2;
  const idf = (tok: string): number =>
    Math.log((globalTotal + 1) / ((globalTokenCount.get(tok) || 0) + 1));
  const scorePhrase = (norm: string): number => {
    const toks = [...new Set(keywordTokens(norm))];
    if (!toks.length) return 0;
    let coverage = 0;
    let distinct = 0;
    for (const tok of toks) {
      const inSet = setTokenCount.get(tok) || 0;
      const global = globalTokenCount.get(tok) || inSet || 1;
      const lift = inSet / global; // 0..1; ~0 for boilerplate, ~1 if exclusive
      if (inSet >= 2) distinct += inSet * Math.pow(lift, DIST_LIFT_EXP);
      if (queryHit(tok)) coverage += idf(tok);
    }
    // Length-normalize so a long filler phrase can't accumulate its way past a
    // tight on-topic one.
    return (coverage + DIST_WEIGHT * distinct) / Math.sqrt(toks.length);
  };

  const ranked = [...phraseCount.keys()]
    .map((norm) => ({ norm, form: phraseForm.get(norm)!, score: scorePhrase(norm) }))
    .filter((r) => r.score > 0)
    .sort((a, b) => b.score - a.score || (a.norm < b.norm ? -1 : 1));

  // 4. Emit, de-duping case/whitespace and dropping phrases that are a substring
  //    of an already-kept (higher-ranked) phrase — keep the richer phrase.
  const out: string[] = [];
  const kept: string[] = [];
  for (const r of ranked) {
    if (out.length >= limit) break;
    const norm = r.norm.replace(/\s+/g, ' ').trim();
    if (!norm) continue;
    // Drop only when the current phrase is a SUBSTRING of an already-kept (higher
    // ranked) phrase — keep the richer phrase. We must NOT drop a longer phrase
    // just because it CONTAINS a kept shorter one (that discards the richer one).
    if (kept.some((k) => k.includes(norm))) continue;
    kept.push(norm);
    out.push(r.form);
  }
  return out;
}

// Total occurrences of each keyword TOKEN across the whole ai_keywords corpus.
// Memoized: the corpus is static within a process, and getKeywordsForTextQuery
// would otherwise rescan every keyword on every query.
let _globalKwTokenCounts: Map<string, number> | null = null;
function getGlobalKeywordTokenCounts(): Map<string, number> {
  if (_globalKwTokenCounts) return _globalKwTokenCounts;
  const m = new Map<string, number>();
  const rows = db!.prepare('SELECT ai_keywords FROM posts WHERE ai_keywords IS NOT NULL').all() as {
    ai_keywords: string;
  }[];
  for (const row of rows) {
    let arr: unknown;
    try {
      arr = JSON.parse(row.ai_keywords);
    } catch {
      continue;
    }
    if (!Array.isArray(arr)) continue;
    for (const k of arr) {
      if (typeof k !== 'string') continue;
      for (const tok of new Set(keywordTokens(k))) {
        m.set(tok, (m.get(tok) || 0) + 1);
      }
    }
  }
  _globalKwTokenCounts = m;
  return m;
}

// Drop all process-lifetime memoized aggregates derived from post content
// (tag/keyword counts, per-term IDF weights, archive stats). MUST be called by
// every mutation that changes posts content or the post_tags index, otherwise
// tag-lift / keyword / relevance ranking goes stale.
// _wordReCache is keyed by query needle (not by archive content) so it is NOT
// invalidated here — it stays correct across mutations.
function invalidateGlobalCaches(): void {
  _globalTagCounts = null;
  _globalKwTokenCounts = null;
  _aliasMap = null;
  _termIdfCache.clear();
  invalidateStatsCache();
}

// ─── Canonicalizzazione sinonimi (tag_alias) ──────────────────────────────────

// Mappa alias_norm → { norm, form } canonici, memoizzata per processo. La tabella
// tag_alias è statica entro un processo finché non si chiama saveTagAliases, che
// invalida questa cache via invalidateGlobalCaches. SOLO gli alias 'accepted'
// canonicalizzano: le proposte ('proposed') NON entrano nella mappa.
let _aliasMap: Map<string, ResolvedAliasEntry> | null = null;
function getAliasMap(): Map<string, ResolvedAliasEntry> {
  if (_aliasMap) return _aliasMap;
  const m = new Map<string, ResolvedAliasEntry>();
  try {
    for (const r of db!
      .prepare(
        "SELECT alias_norm AS a, canonical_norm AS n, canonical_form AS f FROM tag_alias WHERE status = 'accepted'",
      )
      .all() as { a: string; n: string; f: string }[]) {
      m.set(r.a, { norm: r.n, form: r.f });
    }
  } catch {}
  _aliasMap = m;
  return m;
}

// Risolve un tag_norm alla sua forma canonica. Identità ({ norm, form: norm })
// se non è un alias. La tabella mantiene l'invariante "no catene" (canonical_norm
// non è mai a sua volta un alias), quindi un singolo lookup basta; per robustezza
// seguiamo comunque eventuali catene residue con un cap, evitando loop infiniti.
function resolveAlias(norm: unknown): Shelfy.ResolvedAlias {
  const key = String(norm ?? '')
    .trim()
    .toLowerCase();
  if (!key) return { norm: '', form: '' };
  const map = getAliasMap();
  let hit = map.get(key);
  if (!hit) return { norm: key, form: key };
  // Segui la catena alla radice (difensivo: l'invariante "no catene" dovrebbe già
  // garantire un solo salto, ma seguiamo comunque, con guardia anti-loop).
  const seen = new Set([key]);
  while (true) {
    const next = map.get(hit.norm);
    if (!next || seen.has(hit.norm)) return { norm: hit.norm, form: hit.form };
    seen.add(hit.norm);
    hit = next;
  }
}

// Tag che non sono ancora alias e non sono la canonica di nessun alias, ordinati
// per frequenza — i candidati che buildTagAliases (analyzer) può mappare su una
// canonica esistente. Restituisce [{ norm, form, count }].
function getUnaliasedTags({ limit = 300 }: { limit?: number } = {}): Shelfy.VocabTag[] {
  if (!db) throw new Error('Database not initialized');
  const rows = db
    .prepare(
      `
      SELECT tag_norm AS norm, COUNT(*) AS count
      FROM post_tags
      WHERE tag_norm NOT IN (SELECT alias_norm FROM tag_alias)
        AND tag_norm NOT IN (SELECT canonical_norm FROM tag_alias)
      GROUP BY tag_norm
      ORDER BY count DESC
      LIMIT ?
    `,
    )
    .all(limit) as { norm: string; count: number }[];
  if (!rows.length) return [];

  const norms = rows.map((r) => r.norm);
  const placeholders = norms.map(() => '?').join(',');
  const formRows = db
    .prepare(
      `
      SELECT tag_norm AS norm, tag_form AS form, COUNT(*) AS c
      FROM post_tags WHERE tag_norm IN (${placeholders})
      GROUP BY tag_norm, tag_form
    `,
    )
    .all(...norms) as { norm: string; form: string; c: number }[];
  const forms = new Map<string, Map<string, number>>();
  for (const r of formRows) {
    if (!forms.has(r.norm)) forms.set(r.norm, new Map());
    forms.get(r.norm)!.set(r.form, r.c);
  }
  return rows.map((r) => ({
    norm: r.norm,
    form: bestForm(forms.get(r.norm), r.norm),
    count: r.count,
  }));
}

// Vocabolario canonico corrente per l'allowlist del prompt di buildTagAliases:
// tutti i tag_norm in uso che NON sono alias (cioè le forme canoniche valide su
// cui un alias può puntare), ordinati per frequenza. [{ norm, form, count }].
function getCanonicalVocab({ limit = 300 }: { limit?: number } = {}): Shelfy.VocabTag[] {
  if (!db) throw new Error('Database not initialized');
  const rows = db
    .prepare(
      `
      SELECT tag_norm AS norm, COUNT(*) AS count
      FROM post_tags
      WHERE tag_norm NOT IN (SELECT alias_norm FROM tag_alias)
      GROUP BY tag_norm
      ORDER BY count DESC
      LIMIT ?
    `,
    )
    .all(limit) as { norm: string; count: number }[];
  if (!rows.length) return [];

  const norms = rows.map((r) => r.norm);
  const placeholders = norms.map(() => '?').join(',');
  const formRows = db
    .prepare(
      `
      SELECT tag_norm AS norm, tag_form AS form, COUNT(*) AS c
      FROM post_tags WHERE tag_norm IN (${placeholders})
      GROUP BY tag_norm, tag_form
    `,
    )
    .all(...norms) as { norm: string; form: string; c: number }[];
  const forms = new Map<string, Map<string, number>>();
  for (const r of formRows) {
    if (!forms.has(r.norm)) forms.set(r.norm, new Map());
    forms.get(r.norm)!.set(r.form, r.c);
  }
  return rows.map((r) => ({
    norm: r.norm,
    form: bestForm(forms.get(r.norm), r.norm),
    count: r.count,
  }));
}

// Salva nuovi alias. pairs: [{ aliasNorm, aliasForm, canonicalNorm, canonicalForm }].
// L'opzione status determina il comportamento (firma retro-compatibile, 2° arg opzionale):
//   - status 'accepted' (review confermata, oppure flusso legacy): inserisce gli alias E
//     ri-canonicalizza in transazione le righe post_tags che li usano (comportamento storico).
//   - status 'proposed' (default): inserisce gli alias come PROPOSTE da rivedere, SENZA
//     toccare post_tags (rewritten=0). Le proposte non canonicalizzano nulla finché non
//     vengono accettate via setAliasStatus.
// Invariante "no catene": se la canonica scelta è essa stessa un alias ACCETTATO (esistente
// o appena introdotto come accepted nel batch), la si risolve alla RADICE prima di inserire.
// Solo nel ramo 'accepted' gli alias preesistenti che puntavano all'alias appena promosso
// vengono ripuntati alla radice e le righe post_tags con tag_norm = aliasNorm vengono
// spostate sulla canonica (UPDATE, fondendo con righe già presenti). ai_tags intatto.
function saveTagAliases(
  pairs: AliasPair[] = [],
  { status = 'proposed' }: { status?: Shelfy.AliasStatus } = {},
): { added: number; rewritten: number } {
  if (!db) throw new Error('Database not initialized');
  const accept = status === 'accepted';
  const cleaned = (Array.isArray(pairs) ? pairs : [])
    .map((p) => ({
      aliasNorm: String(p?.aliasNorm ?? '')
        .trim()
        .toLowerCase(),
      aliasForm: String(p?.aliasForm ?? p?.aliasNorm ?? '').trim(),
      canonicalNorm: String(p?.canonicalNorm ?? '')
        .trim()
        .toLowerCase(),
      canonicalForm: String(p?.canonicalForm ?? p?.canonicalNorm ?? '').trim(),
    }))
    // Scarta righe vuote o auto-mappanti (un tag non è alias di se stesso).
    .filter((p) => p.aliasNorm && p.canonicalNorm && p.aliasNorm !== p.canonicalNorm);
  if (!cleaned.length) return { added: 0, rewritten: 0 };

  const insAlias = db.prepare(
    'INSERT OR REPLACE INTO tag_alias (alias_norm, canonical_norm, canonical_form, status) VALUES (?, ?, ?, ?)',
  );
  const updMembership = db.prepare(
    'UPDATE OR IGNORE tag_alias SET canonical_norm = ?, canonical_form = ? WHERE canonical_norm = ? AND status = ?',
  );

  let added = 0;
  let rewritten = 0;

  const tx = db.transaction(() => {
    // Mappa alias→{norm,form} degli alias ACCETTATI (DB) per risolvere catene alla
    // radice: solo gli accepted canonicalizzano, quindi solo loro definiscono catene.
    const liveAlias = new Map<string, ResolvedAliasEntry>();
    for (const r of db!
      .prepare(
        "SELECT alias_norm AS a, canonical_norm AS n, canonical_form AS f FROM tag_alias WHERE status = 'accepted'",
      )
      .all() as { a: string; n: string; f: string }[]) {
      liveAlias.set(r.a, { norm: r.n, form: r.f });
    }
    // In modalità accepted, includi anche i nuovi pair nella risoluzione delle catene
    // (last-write-wins per alias). In modalità proposed le proposte non formano catene.
    if (accept) {
      for (const p of cleaned)
        liveAlias.set(p.aliasNorm, { norm: p.canonicalNorm, form: p.canonicalForm });
    }

    // Risolve `norm` alla radice della catena di alias. `fallbackForm` è la display
    // form da usare quando la radice NON è essa stessa un alias (cioè la canonica
    // scelta dal chiamante), così non si perde il casing fornito.
    const rootOf = (norm: string, fallbackForm: string): ResolvedAliasEntry => {
      let cur = norm;
      let form = fallbackForm || norm;
      const seen = new Set([cur]);
      let hit = liveAlias.get(cur);
      while (hit) {
        cur = hit.norm;
        form = hit.form;
        if (seen.has(cur)) break;
        seen.add(cur);
        hit = liveAlias.get(cur);
      }
      return { norm: cur, form: form || cur };
    };

    for (const p of cleaned) {
      const root = rootOf(p.canonicalNorm, p.canonicalForm);
      // Non creare un alias che punti a se stesso dopo la risoluzione.
      if (root.norm === p.aliasNorm) continue;
      insAlias.run(p.aliasNorm, root.norm, root.form, accept ? 'accepted' : 'proposed');
      added++;

      // Le proposte si limitano a registrare la riga: niente rewrite di membership
      // o post_tags finché non vengono accettate.
      if (!accept) continue;

      // Se questo alias era a sua volta canonica di altri alias ACCETTATI, ripuntali alla radice.
      updMembership.run(root.norm, root.form, p.aliasNorm, 'accepted');

      // Ri-canonicalizza le righe post_tags che usano aliasNorm: sposta sulla
      // canonica radice. UPDATE OR IGNORE evita di violare la PK (post_id,tag_norm)
      // quando il post ha già la canonica; in quel caso la riga alias resta e va
      // ripulita subito dopo.
      rewritten += db!
        .prepare('UPDATE OR IGNORE post_tags SET tag_norm = ?, tag_form = ? WHERE tag_norm = ?')
        .run(root.norm, root.form, p.aliasNorm).changes;
      db!.prepare('DELETE FROM post_tags WHERE tag_norm = ?').run(p.aliasNorm);
    }
  });
  tx();
  invalidateGlobalCaches();
  return { added, rewritten };
}

// Elenco degli alias per la review UX. Restituisce
// [{ aliasNorm, aliasForm, canonicalNorm, canonicalForm, status, count }] dove
// count = numero di post che portano aliasNorm in post_tags (quanti tag verrebbero
// canonicalizzati accettando l'alias; per gli 'accepted' già applicati è 0, perché
// post_tags è già stato spostato). Filtrabile per status; ordinato per count desc.
// aliasForm è la display form più frequente del tag in post_tags (fallback: aliasNorm).
function getTagAliases({
  status = null,
}: { status?: Shelfy.AliasStatus | null } = {}): Shelfy.TagAlias[] {
  if (!db) throw new Error('Database not initialized');
  const where = status ? 'WHERE ta.status = ?' : '';
  const args: string[] = status ? [status] : [];
  const rows = db
    .prepare(
      `
      SELECT ta.alias_norm AS aliasNorm, ta.canonical_norm AS canonicalNorm,
             ta.canonical_form AS canonicalForm, ta.status AS status,
             (SELECT COUNT(*) FROM post_tags pt WHERE pt.tag_norm = ta.alias_norm) AS count
      FROM tag_alias ta
      ${where}
      ORDER BY count DESC, ta.alias_norm ASC
    `,
    )
    .all(...args) as {
    aliasNorm: string;
    canonicalNorm: string;
    canonicalForm: string;
    status: Shelfy.AliasStatus;
    count: number;
  }[];
  if (!rows.length) return [];

  // Ricava la display form migliore per ogni aliasNorm dalle righe post_tags (la
  // forma più usata); fallback alla aliasNorm quando il tag non è (più) in post_tags.
  const norms = [...new Set(rows.map((r) => r.aliasNorm))];
  const placeholders = norms.map(() => '?').join(',');
  const formRows = db
    .prepare(
      `
      SELECT tag_norm AS norm, tag_form AS form, COUNT(*) AS c
      FROM post_tags WHERE tag_norm IN (${placeholders})
      GROUP BY tag_norm, tag_form
    `,
    )
    .all(...norms) as { norm: string; form: string; c: number }[];
  const forms = new Map<string, Map<string, number>>();
  for (const r of formRows) {
    if (!forms.has(r.norm)) forms.set(r.norm, new Map());
    forms.get(r.norm)!.set(r.form, r.c);
  }
  return rows.map((r) => ({
    aliasNorm: r.aliasNorm,
    aliasForm: bestForm(forms.get(r.aliasNorm), r.aliasNorm),
    canonicalNorm: r.canonicalNorm,
    canonicalForm: r.canonicalForm,
    status: r.status,
    count: r.count,
  }));
}

// Transizione di stato di un alias proposto (review). Restituisce { ok, rewritten }.
//   - 'accepted': setta lo status ad 'accepted' E, nella stessa transazione,
//     ri-canonicalizza le righe post_tags che usano aliasNorm spostandole sulla
//     canonica risolta ALLA RADICE (no catene), restituendo quante ne ha riscritte.
//   - 'dismissed': ELIMINA la riga tag_alias (era solo una proposta, post_tags non
//     è mai stato toccato), rewritten=0.
// In entrambi i casi invalida le cache. ok=false se l'alias non esiste o lo status
// richiesto non è gestito.
function setAliasStatus(
  aliasNorm: unknown,
  status: Shelfy.AliasStatus | 'dismissed' | string,
): { ok: boolean; rewritten: number } {
  if (!db) throw new Error('Database not initialized');
  const key = String(aliasNorm ?? '')
    .trim()
    .toLowerCase();
  if (!key) return { ok: false, rewritten: 0 };

  if (status === 'dismissed') {
    // Era solo una proposta: rimuovi la riga, post_tags resta intatto.
    const res = db.prepare('DELETE FROM tag_alias WHERE alias_norm = ?').run(key);
    invalidateGlobalCaches();
    return { ok: res.changes > 0, rewritten: 0 };
  }

  if (status !== 'accepted') return { ok: false, rewritten: 0 };

  let ok = false;
  let rewritten = 0;
  const tx = db.transaction(() => {
    const row = db!
      .prepare(
        'SELECT canonical_norm AS n, canonical_form AS f FROM tag_alias WHERE alias_norm = ?',
      )
      .get(key) as { n: string; f: string | null } | undefined;
    if (!row) return;
    ok = true;

    // Risolvi la canonica alla radice tra gli alias GIÀ accettati (no catene), così
    // se la canonica scelta è a sua volta un alias accettato si punta alla radice.
    const liveAlias = new Map<string, ResolvedAliasEntry>();
    for (const r of db!
      .prepare(
        "SELECT alias_norm AS a, canonical_norm AS n, canonical_form AS f FROM tag_alias WHERE status = 'accepted' AND alias_norm != ?",
      )
      .all(key) as { a: string; n: string; f: string }[]) {
      liveAlias.set(r.a, { norm: r.n, form: r.f });
    }
    let curNorm = row.n;
    let curForm = row.f || row.n;
    const seen = new Set([curNorm]);
    let hit = liveAlias.get(curNorm);
    while (hit) {
      curNorm = hit.norm;
      curForm = hit.form;
      if (seen.has(curNorm)) break;
      seen.add(curNorm);
      hit = liveAlias.get(curNorm);
    }

    // Promuovi lo stato ad accepted (con la canonica risolta alla radice).
    db!
      .prepare(
        "UPDATE tag_alias SET canonical_norm = ?, canonical_form = ?, status = 'accepted' WHERE alias_norm = ?",
      )
      .run(curNorm, curForm, key);

    // Ri-canonicalizza post_tags: sposta le righe alias sulla radice; UPDATE OR IGNORE
    // evita di violare la PK quando il post ha già la canonica, poi ripuliamo i residui.
    rewritten += db!
      .prepare('UPDATE OR IGNORE post_tags SET tag_norm = ?, tag_form = ? WHERE tag_norm = ?')
      .run(curNorm, curForm, key).changes;
    db!.prepare('DELETE FROM post_tags WHERE tag_norm = ?').run(key);
  });
  tx();
  invalidateGlobalCaches();
  return { ok, rewritten };
}

// buildTagCommunities + cosineSim vivono in ./cluster-core (vedi require in testa):
// lo stesso codice puro è usato dal worker_threads (cluster-worker.js) e dal
// fallback in-process qui sotto, così l'output dell'offload è identico al locale.

// Timeout difensivo per l'offload del clustering: oltre questo il worker viene
// annullato e si ricade sul calcolo in-process. Il clustering pesante è raro.
const CLUSTER_WORKER_TIMEOUT_MS = 120_000;

// Esegue buildTagCommunities in un worker_threads (CPU puro fuori dal main thread).
// Risolve coi gruppi; rigetta su errore/timeout così il chiamante può ricadere sul
// calcolo sincrono in-process.
function runClusterWorker(payload: ClusterWorkerPayload): Promise<string[][]> {
  return new Promise<string[][]>((resolve, reject) => {
    let worker: Worker;
    try {
      worker = new Worker(path.join(__dirname, 'cluster-worker.js'), { workerData: payload });
    } catch (err) {
      reject(err as Error);
      return;
    }
    let settled = false;
    let timer: NodeJS.Timeout | null = null;
    const finish = <T>(fn: (arg: T) => void, arg: T): void => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      worker.terminate();
      fn(arg);
    };
    timer = setTimeout(
      () => finish(reject, new Error('cluster worker timeout')),
      CLUSTER_WORKER_TIMEOUT_MS,
    );
    worker.once('message', (msg: { ok?: boolean; groups?: string[][]; error?: string }) =>
      msg && msg.ok
        ? finish(resolve, msg.groups!)
        : finish(reject, new Error((msg && msg.error) || 'cluster worker failed')),
    );
    worker.once('error', (err) => finish(reject, err));
    worker.once('exit', (code) => {
      if (code !== 0) finish(reject, new Error(`cluster worker exited with code ${code}`));
    });
  });
}

// Clustering dei tag con offload su worker_threads e fallback in-process identico.
// `vecByNorm` (Map norm→vettore) è opzionale: se presente, il peso d'arco fonde
// jaccard+coseno (P4). Map ed edge sono serializzabili → passano per workerData.
async function clusterTagCommunities(
  freq: Map<string, number>,
  edges: CoEdge[],
  vecByNorm: Map<string, number[]> | null,
  opts: ClusterOpts,
): Promise<string[][]> {
  try {
    return await runClusterWorker({
      freq: [...freq],
      edges,
      vecByNorm: vecByNorm ? [...vecByNorm] : null,
      opts,
    });
  } catch (err) {
    console.log(
      `[db] cluster worker non disponibile (${(err as Error)?.message || err}): calcolo in-process`,
    );
    const cosSim = vecByNorm
      ? (a: string, b: string): number => {
          const va = vecByNorm.get(a);
          const vb = vecByNorm.get(b);
          return va && vb ? cosineSim(va, vb) : 0;
        }
      : null;
    return buildTagCommunities(freq, edges, { ...opts, cosSim });
  }
}

// Build the RAW candidate groups handed to the LLM for naming/refinement. Tags
// are normalized keys (tag_norm); each group carries its members' top co-occurring
// neighbors as compact context. No LLM here — pure co-occurrence structure (+ una
// componente di similarità via embeddings, P4).
//
// P5: minTagCount basso (default 1) per NON escludere a priori i tag rari; ciò che
// resta comunque fuori da ogni gruppo (rari isolati) finisce in un passo di
// COPERTURA dedicato invece di sparire silenziosamente — e viene loggato.
// P4: edge score = alpha·jaccard + (1-alpha)·cos, con cos dal modulo embeddings su
// vettori delle FORME dei tag; fallback solo-jaccard + log se embeddings non pronti.
async function getTagCandidateGroups({
  minTagCount = 1,
  minJaccard = 0.15,
  maxGroupSize = 14,
  neighborLimit = 4,
  alpha = 0.5,
}: {
  minTagCount?: number;
  minJaccard?: number;
  maxGroupSize?: number;
  neighborLimit?: number;
  alpha?: number;
} = {}): Promise<Shelfy.TagCandidateGroup[]> {
  if (!db) throw new Error('Database not initialized');

  const freq = new Map<string, number>();
  for (const r of db
    .prepare('SELECT tag_norm AS norm, COUNT(*) AS c FROM post_tags GROUP BY tag_norm')
    .all() as { norm: string; c: number }[]) {
    if (r.c >= minTagCount) freq.set(r.norm, r.c);
  }
  if (freq.size < 2) return [];

  const edges = (
    db
      .prepare(
        `
      SELECT a.tag_norm AS a, b.tag_norm AS b, COUNT(*) AS c
      FROM post_tags a JOIN post_tags b ON b.post_id = a.post_id AND a.tag_norm < b.tag_norm
      GROUP BY a.tag_norm, b.tag_norm
      HAVING c >= 2
    `,
      )
      .all() as CoEdge[]
  ).filter((e) => freq.has(e.a) && freq.has(e.b));

  // Forme display per ogni tag (per gli embeddings e l'output di copertura).
  const formByNorm = new Map<string, string>();
  {
    const norms = [...freq.keys()];
    const formRows = chunkedInQuery<{ norm: string; form: string; c: number }>(
      norms,
      (ph) =>
        `SELECT tag_norm AS norm, tag_form AS form, COUNT(*) AS c FROM post_tags WHERE tag_norm IN (${ph}) GROUP BY tag_norm, tag_form`,
    );
    const fm = new Map<string, Map<string, number>>();
    for (const r of formRows) {
      if (!fm.has(r.norm)) fm.set(r.norm, new Map());
      fm.get(r.norm)!.set(r.form, r.c);
    }
    for (const n of norms) formByNorm.set(n, bestForm(fm.get(n), n));
  }

  // P4: costruisce cosSim(a,b) dagli embeddings delle FORME dei tag. embedTexts è
  // async (interroga il server di embedding), perciò questa funzione è async; il
  // require è in try/catch così node --check e il fallback solo-jaccard reggono
  // anche senza il file embeddings.js o senza modello scaricato.
  let vecByNorm: Map<string, number[]> | null = null;
  try {
    // Lazy require: embeddings.js queries a heavy local model server and may be
    // absent / unbuilt — kept as a synchronous require so a missing module simply
    // falls back to jaccard-only clustering instead of failing the whole module.

    const emb = require('./embeddings') as EmbeddingsModule;
    if (
      emb &&
      typeof emb.isEmbeddingReady === 'function' &&
      emb.isEmbeddingReady() &&
      typeof emb.embedTexts === 'function' &&
      typeof emb.cosineSim === 'function'
    ) {
      const norms = [...freq.keys()];
      const forms = norms.map((n) => formByNorm.get(n) || n);
      const vecs = await emb.embedTexts(forms);
      const vmap = new Map<string, number[]>();
      norms.forEach((n, i) => {
        if (vecs && vecs[i]) vmap.set(n, vecs[i]!);
      });
      if (vmap.size) {
        vecByNorm = vmap;
        console.log(
          `[db] embeddings: ${vmap.size} vettori, clustering fuso jaccard+cos (alpha=${alpha})`,
        );
      } else {
        console.log('[db] embeddings: nessun vettore disponibile, fallback solo-jaccard');
      }
    } else {
      console.log('[db] embeddings non pronti: clustering con solo-jaccard');
    }
  } catch (err) {
    console.log(
      `[db] embeddings non disponibili (${(err as Error)?.message || err}): clustering con solo-jaccard`,
    );
  }

  // Offload del clustering (label propagation + coseno per arco) su worker_threads;
  // il coseno è ricostruito dai vettori nel worker. Fallback in-process identico.
  const communities = await clusterTagCommunities(freq, edges, vecByNorm, {
    minJaccard,
    maxGroupSize,
    alpha,
  });

  // Neighbor lookup (raw co-occurrence counts) for LLM context.
  const nbr = new Map<string, [string, number][]>();
  const push = (a: string, b: string, c: number): void => {
    if (!nbr.has(a)) nbr.set(a, []);
    nbr.get(a)!.push([b, c]);
  };
  for (const { a, b, c } of edges) {
    push(a, b, c);
    push(b, a, c);
  }
  const topNeighbors = (n: string): string[] =>
    (nbr.get(n) || [])
      .sort((x, y) => y[1] - x[1])
      .slice(0, neighborLimit)
      .map(([t]) => t);

  const groups: Shelfy.TagCandidateGroup[] = communities.map((tags) => {
    const neighbors: Record<string, string[]> = {};
    for (const t of tags) neighbors[t] = topNeighbors(t).filter((x) => !tags.includes(x));
    return { tags, neighbors };
  });

  // ── P5 COPERTURA: i tag rari/isolati che non sono finiti in nessun gruppo non
  // vanno persi silenziosamente. Li raccogliamo e li raggruppiamo per vicino di
  // co-occorrenza più forte (se ne hanno uno), o li logghiamo come esclusi.
  const placed = new Set<string>();
  for (const g of groups) for (const t of g.tags) placed.add(t);
  const leftover = [...freq.keys()].filter((n) => !placed.has(n));
  if (leftover.length) {
    // Raggruppa i leftover attorno al loro vicino di co-occorrenza più forte che è
    // già piazzato in un gruppo: li annettiamo come "coda" del gruppo del vicino.
    const groupOf = new Map<string, number>(); // norm → indice gruppo
    groups.forEach((g, i) => g.tags.forEach((t) => groupOf.set(t, i)));
    const stillOut: string[] = [];
    for (const t of leftover) {
      const neigh = (nbr.get(t) || []).sort((x, y) => y[1] - x[1]);
      let attached = false;
      for (const [m] of neigh) {
        const gi = groupOf.get(m);
        if (gi != null && groups[gi].tags.length < maxGroupSize) {
          groups[gi].tags.push(t);
          groups[gi].neighbors[t] = topNeighbors(t).filter((x) => !groups[gi].tags.includes(x));
          groupOf.set(t, gi);
          attached = true;
          break;
        }
      }
      if (!attached) stillOut.push(t);
    }
    if (stillOut.length) {
      console.log(
        `[db] clustering: ${stillOut.length} tag rari senza gruppo (esclusi): ${stillOut.slice(0, 50).join(', ')}${stillOut.length > 50 ? ' …' : ''}`,
      );
    }
  }

  return groups;
}

// Persist a fresh clustering run as 'proposed'. clusters: [{ label, tags:[norm] }].
// Clears the previous 'proposed' rows (and their memberships via cascade) but
// leaves 'accepted' clusters untouched; tags already owned by an accepted cluster
// are skipped so a regenerate never steals them. Returns { runId, count }.
function saveClusterRun(clusters: { label?: string; tags?: unknown[] }[] = []): {
  runId: number;
  count: number;
} {
  if (!db) throw new Error('Database not initialized');
  const runId = Date.now();
  const insCluster = db.prepare(
    "INSERT INTO tag_cluster (label, label_norm, status, run_id) VALUES (?, ?, 'proposed', ?)",
  );
  const insMember = db.prepare(
    'INSERT OR IGNORE INTO tag_cluster_membership (tag_norm, cluster_id) VALUES (?, ?)',
  );

  const tx = db.transaction(() => {
    db!.prepare("DELETE FROM tag_cluster WHERE status = 'proposed'").run();
    const accepted = new Set(
      (
        db!
          .prepare(
            "SELECT m.tag_norm AS norm FROM tag_cluster_membership m JOIN tag_cluster c ON c.id = m.cluster_id WHERE c.status = 'accepted'",
          )
          .all() as { norm: string }[]
      ).map((r) => r.norm),
    );
    let count = 0;
    for (const cl of clusters) {
      const label = String(cl?.label ?? '').trim();
      const tags = Array.isArray(cl?.tags)
        ? [
            ...new Set(
              cl.tags
                .map((t) =>
                  String(t ?? '')
                    .trim()
                    .toLowerCase(),
                )
                .filter(Boolean),
            ),
          ].filter((t) => !accepted.has(t))
        : [];
      if (!label || tags.length < 2) continue;
      const cid = insCluster.run(label, label.toLowerCase(), runId).lastInsertRowid;
      for (const t of tags) insMember.run(t, cid);
      count++;
    }
    return count;
  });

  return { runId, count: tx() };
}

// Read persisted clusters (proposed + accepted) for the AI Tags view. Resolves
// display forms and post counts off the index. Same shape as the old function
// (id, tags, topTag, postCount) plus label/status; topTag is the model label.
// `maxClusters` (optional) caps the returned list, keeping the postCount DESC
// ordering — callers without arguments get the full list as before.
function getTagClusters({ maxClusters }: { maxClusters?: number } = {}): Shelfy.TagCluster[] {
  if (!db) throw new Error('Database not initialized');

  const clusters = db
    .prepare("SELECT id, label, status FROM tag_cluster WHERE status IN ('proposed','accepted')")
    .all() as { id: number; label: string; status: 'proposed' | 'accepted' }[];
  if (!clusters.length) return [];

  const byCluster = new Map<number, string[]>(); // id → [norm]
  for (const r of db
    .prepare('SELECT cluster_id AS cid, tag_norm AS norm FROM tag_cluster_membership')
    .all() as { cid: number; norm: string }[]) {
    if (!byCluster.has(r.cid)) byCluster.set(r.cid, []);
    byCluster.get(r.cid)!.push(r.norm);
  }

  const freq = new Map<string, number>();
  for (const r of db
    .prepare('SELECT tag_norm AS norm, COUNT(*) AS c FROM post_tags GROUP BY tag_norm')
    .all() as { norm: string; c: number }[]) {
    freq.set(r.norm, r.c);
  }

  const formCache = new Map<string, string>();
  const formStmt = db.prepare(
    'SELECT tag_form AS form, COUNT(*) AS c FROM post_tags WHERE tag_norm = ? GROUP BY tag_form',
  );
  const displayForm = (norm: string): string => {
    if (formCache.has(norm)) return formCache.get(norm)!;
    const fm = new Map<string, number>();
    for (const r of formStmt.all(norm) as { form: string; c: number }[]) fm.set(r.form, r.c);
    const f = bestForm(fm, norm);
    formCache.set(norm, f);
    return f;
  };

  const out: Shelfy.TagCluster[] = [];
  for (const c of clusters) {
    const norms = (byCluster.get(c.id) || [])
      .slice()
      .sort((a, b) => (freq.get(b) || 0) - (freq.get(a) || 0));
    if (!norms.length) continue;
    const ph = norms.map(() => '?').join(',');
    const postCount = (
      db
        .prepare(`SELECT COUNT(DISTINCT post_id) AS count FROM post_tags WHERE tag_norm IN (${ph})`)
        .get(...norms) as { count: number }
    ).count;
    out.push({
      id: c.id,
      label: c.label,
      status: c.status,
      topTag: c.label,
      tags: norms.map(displayForm),
      postCount,
    });
  }

  out.sort((a, b) => b.postCount - a.postCount);
  return Number.isFinite(maxClusters) && (maxClusters as number) > 0
    ? out.slice(0, maxClusters)
    : out;
}

// Accept a proposed cluster, or dismiss one (dismiss deletes it outright so its
// tags become free for a future run). status ∈ {'proposed','accepted','dismissed'}.
function setClusterStatus(id: number, status: Shelfy.ClusterStatus | string): { updated: number } {
  if (!db) throw new Error('Database not initialized');
  if (!['proposed', 'accepted', 'dismissed'].includes(status))
    throw new Error('Invalid cluster status');
  if (status === 'dismissed') {
    return { updated: db.prepare('DELETE FROM tag_cluster WHERE id = ?').run(id).changes };
  }
  return {
    updated: db
      .prepare('UPDATE tag_cluster SET status = ?, updated_at = unixepoch() WHERE id = ?')
      .run(status, id).changes,
  };
}

// Rename a cluster's label (the user-visible theme name).
function renameCluster(id: number, label: unknown): { updated: number } {
  if (!db) throw new Error('Database not initialized');
  const l = String(label ?? '').trim();
  if (!l) throw new Error('Cluster label is required');
  return {
    updated: db
      .prepare(
        'UPDATE tag_cluster SET label = ?, label_norm = ?, updated_at = unixepoch() WHERE id = ?',
      )
      .run(l, l.toLowerCase(), id).changes,
  };
}

// Remove one tag from a cluster (the user judged it out of place).
function removeTagFromCluster(tagNorm: unknown, clusterId: number): { removed: number } {
  if (!db) throw new Error('Database not initialized');
  const t = String(tagNorm ?? '')
    .trim()
    .toLowerCase();
  if (!t) return { removed: 0 };
  return {
    removed: db
      .prepare('DELETE FROM tag_cluster_membership WHERE tag_norm = ? AND cluster_id = ?')
      .run(t, clusterId).changes,
  };
}

// Suggest groups of near-duplicate tags (accents/case/typos/short plurals) the
// user might want to merge. canonical = most frequent variant.
function getTagMergeSuggestions({
  limit = 30,
}: { limit?: number } = {}): Shelfy.TagMergeSuggestion[] {
  if (!db) throw new Error('Database not initialized');

  // Distinct tags (lowercase form = tag_norm) with their post counts, straight
  // off the index, plus the accent-stripped normalized key used for grouping.
  const tags = (
    db
      .prepare('SELECT tag_norm AS form, COUNT(*) AS count FROM post_tags GROUP BY tag_norm')
      .all() as { form: string; count: number }[]
  ).map(({ form, count }) => ({ form, count, norm: normalizeTag(form) }));

  // Union-find over tag indices: same normalized key, or Levenshtein <= 2.
  const parent = tags.map((_, i) => i);
  const find = (x: number): number => {
    while (parent[x] !== x) {
      parent[x] = parent[parent[x]];
      x = parent[x];
    }
    return x;
  };
  const union = (a: number, b: number): void => {
    const ra = find(a),
      rb = find(b);
    if (ra !== rb) parent[ra] = rb;
  };

  // Group by exact normalized key first.
  const byNorm = new Map<string, number[]>();
  tags.forEach((t, i) => {
    if (!byNorm.has(t.norm)) byNorm.set(t.norm, []);
    byNorm.get(t.norm)!.push(i);
  });
  for (const idxs of byNorm.values()) {
    for (let i = 1; i < idxs.length; i++) union(idxs[0], idxs[i]);
  }

  // Near-duplicate normalized forms (typo/plural). BUCKETING kills the O(n²):
  // a Levenshtein <= 2 match implies a length within 2, so we only compare tags
  // whose length band (and its two neighbors) overlap. We do NOT key on the first
  // character: a single substitution at position 0 ('typo'/'tipo', 'casa'/'rasa')
  // is edit distance 1 with a different first char, so first-char bucketing would
  // silently drop those genuine near-duplicates. Bucketing on the length band
  // alone keeps the comparison set bounded while catching first-char edits.
  const BAND = 2; // length-band width; lengths L and L' with |L-L'|<=2 must share or neighbor a band
  const bucketKey = (band: number): string => `${band}`;
  const buckets = new Map<string, number[]>(); // key → [index]
  tags.forEach((t, i) => {
    const band = Math.floor(t.norm.length / BAND);
    const key = bucketKey(band);
    if (!buckets.has(key)) buckets.set(key, []);
    buckets.get(key)!.push(i);
  });

  const compared = new Set<string>(); // "i:j" dedupe across overlapping neighbor scans
  tags.forEach((t, i) => {
    if (!t.norm) return;
    const band = Math.floor(t.norm.length / BAND);
    for (let b = band - 1; b <= band + 1; b++) {
      const cand = buckets.get(bucketKey(b));
      if (!cand) continue;
      for (const j of cand) {
        if (j <= i) continue;
        if (find(i) === find(j)) continue;
        const pk = i + ':' + j;
        if (compared.has(pk)) continue;
        compared.add(pk);
        if (levenshtein(t.norm, tags[j].norm) <= 2) union(i, j);
      }
    }
  });

  const groups = new Map<number, { form: string; count: number; norm: string }[]>(); // root → [tag]
  tags.forEach((t, i) => {
    const r = find(i);
    if (!groups.has(r)) groups.set(r, []);
    groups.get(r)!.push(t);
  });

  const result: Shelfy.TagMergeSuggestion[] = [];
  for (const members of groups.values()) {
    if (members.length < 2) continue;
    const sorted = members.slice().sort((a, b) => b.count - a.count);
    const totalCount = sorted.reduce((s, m) => s + m.count, 0);
    result.push({
      canonical: sorted[0].form,
      variants: sorted.slice(1).map((m) => m.form),
      totalCount,
    });
  }

  return result.sort((a, b) => b.totalCount - a.totalCount).slice(0, limit);
}

// Tag-hygiene snapshot for the dashboard.
function getTagHealth(): Shelfy.TagHealth {
  if (!db) throw new Error('Database not initialized');

  // Per-tag post counts off the index. orphan = used in exactly one post,
  // rare = used in <= 2 posts.
  const rows = db
    .prepare('SELECT tag_norm AS norm, COUNT(*) AS count FROM post_tags GROUP BY tag_norm')
    .all() as { norm: string; count: number }[];

  const orphanNorms = rows.filter((r) => r.count === 1).map((r) => r.norm);
  const rareTags = rows.filter((r) => r.count <= 2).length;

  // Resolve display forms only for orphan tags.
  const formStmt = db.prepare(
    'SELECT tag_form AS form, COUNT(*) AS c FROM post_tags WHERE tag_norm = ? GROUP BY tag_form',
  );
  const orphanTags: Shelfy.TagCount[] = orphanNorms.map((norm) => {
    const fm = new Map<string, number>();
    for (const r of formStmt.all(norm) as { form: string; c: number }[]) fm.set(r.form, r.c);
    return { tag: bestForm(fm, norm), count: 1 };
  });
  orphanTags.sort((a, b) => a.tag.localeCompare(b.tag));

  const unanalyzedPosts = (
    db
      .prepare("SELECT COUNT(*) as count FROM posts WHERE ai_status IS NULL OR ai_status != 'done'")
      .get() as { count: number }
  ).count;

  // Analyzed but with no derived tag rows → untagged (empty/[]/null ai_tags).
  const untaggedPosts = (
    db
      .prepare(
        "SELECT COUNT(*) as count FROM posts WHERE ai_status = 'done' AND NOT EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = posts.id)",
      )
      .get() as { count: number }
  ).count;

  return { orphanTags, rareTags, unanalyzedPosts, untaggedPosts };
}

// Rewrite a single post's ai_tags: replace any occurrence whose lowercase form
// matches `fromKey` with `to`, preserving order and de-duplicating (case-insensitive).
function rewritePostTags(
  rawTags: string | null | undefined,
  fromKeys: Set<string>,
  to: string,
): string[] | null {
  const tags = parseTags(rawTags);
  const out: string[] = [];
  const seen = new Set<string>();
  let changed = false;
  for (const t of tags) {
    if (typeof t !== 'string') {
      out.push(t);
      continue;
    }
    const key = t.trim().toLowerCase();
    let val = t;
    if (fromKeys.has(key)) {
      val = to;
      changed = true;
    }
    const vk = val.trim().toLowerCase();
    if (seen.has(vk)) {
      changed = true;
      continue;
    }
    seen.add(vk);
    out.push(val);
  }
  return changed ? out : null;
}

// Rename a tag everywhere (case-insensitive match), de-duplicating. Returns the
// number of posts whose ai_tags were rewritten.
function renameTag(from: string, to: string): { updated: number } {
  if (!db) throw new Error('Database not initialized');
  return mergeTags([from], to);
}

// Merge several source tags into a single target across all posts. Returns the
// number of posts updated.
function mergeTags(sources: string[] = [], target: string): { updated: number } {
  if (!db) throw new Error('Database not initialized');
  const to = String(target ?? '').trim();
  if (!to) throw new Error('Target tag is required');
  const fromKeys = new Set(
    sources
      .map((s) =>
        String(s ?? '')
          .trim()
          .toLowerCase(),
      )
      .filter(Boolean),
  );
  if (!fromKeys.size) return { updated: 0 };

  // Only posts that actually carry one of the source tags can change. fromKeys
  // are already trim().toLowerCase(), exactly matching post_tags.tag_norm, so we
  // can pull the affected post ids straight from the index instead of scanning
  // and JSON-parsing every post's ai_tags.
  const keyList = [...fromKeys];
  const affectedIds = (
    db
      .prepare(
        `SELECT DISTINCT post_id AS id FROM post_tags WHERE tag_norm IN (${keyList.map(() => '?').join(',')})`,
      )
      .all(...keyList) as { id: string }[]
  ).map((r) => r.id);
  const rows = affectedIds.length
    ? chunkedInQuery<{ id: string; ai_tags: string | null; user_tags: string | null }>(
        affectedIds,
        (ph) => `SELECT id, ai_tags, user_tags FROM posts WHERE id IN (${ph})`,
      )
    : [];
  const upd = db.prepare('UPDATE posts SET ai_tags = ? WHERE id = ?');
  const updUser = db.prepare('UPDATE posts SET user_tags = ? WHERE id = ?');
  // Keep the derived index in sync: any post whose JSON we rewrite gets its
  // post_tags rebuilt from the new tag list. Only the AI-owned rows are dropped —
  // the user's manual tags (tier='manual') live in a separate layer and survive.
  const delTags = db.prepare(
    "DELETE FROM post_tags WHERE post_id = ? AND (tier IS NULL OR tier IN ('general','specific'))",
  );
  // Carry the AI tier (general/specific/NULL) through the rebuild, mirroring
  // applyAiAnalysis. Reading the existing per-norm tier BEFORE delTags lets us
  // re-apply it on the rewritten rows so a rename/merge never flattens the P2
  // tier split (the only other source of tiers is analysis time).
  const selTier = db.prepare(
    "SELECT tag_norm AS norm, tier FROM post_tags WHERE post_id = ? AND (tier IS NULL OR tier IN ('general','specific'))",
  );
  const insTag = db.prepare(
    'INSERT OR IGNORE INTO post_tags (post_id, tag_norm, tag_form, tier) VALUES (?, ?, ?, ?)',
  );
  // Manual tags follow a rename/merge too: rewrite the user_tags JSON (the display
  // source of truth) and resync their tier='manual' index rows, mirroring what we
  // do for ai_tags. Without this a renamed tag would keep its old name in the
  // user's own tag list.
  const delManual = db.prepare("DELETE FROM post_tags WHERE post_id = ? AND tier = 'manual'");
  const insManual = db.prepare(
    "INSERT OR IGNORE INTO post_tags (post_id, tag_norm, tag_form, tier) VALUES (?, ?, ?, 'manual')",
  );
  // The target's CANONICAL norm: the rewritten target tag lands here after
  // resolveAlias, so the tier-inheritance check below must compare against it.
  const toNormKey = resolveAlias(to.toLowerCase()).norm;
  let updated = 0;
  const tx = db.transaction(() => {
    for (const row of rows) {
      let touched = false;
      const next = rewritePostTags(row.ai_tags, fromKeys, to);
      if (next) {
        upd.run(JSON.stringify(next), row.id);
        // Snapshot the current tier per (pre-rename) norm so the rewritten rows
        // keep it. The renamed target inherits the tier of the first source that
        // had one (or its own prior tier), like the rest of the AI write path.
        const tierByNorm = new Map<string, Shelfy.TagTier | null>();
        for (const r of selTier.all(row.id) as { norm: string; tier: Shelfy.TagTier | null }[])
          tierByNorm.set(r.norm, r.tier ?? null);
        let mergedTier: Shelfy.TagTier | null | undefined;
        for (const k of fromKeys) {
          const ck = resolveAlias(k).norm; // stored tiers are keyed by canonical norm
          if (tierByNorm.has(ck) && tierByNorm.get(ck) != null) {
            mergedTier = tierByNorm.get(ck);
            break;
          }
        }
        delTags.run(row.id);
        const seen = new Set<string>();
        for (const { norm, form } of normalizeTagRows(next)) {
          const c = resolveAlias(norm);
          if (!c.norm || seen.has(c.norm)) continue;
          seen.add(c.norm);
          // form: prefer the canonical display form when the alias remapped the norm.
          const outForm = c.norm === norm ? form : c.form || form;
          // tier: the target norm inherits the merged source tier (falling back to
          // its own prior tier); every other surviving tag keeps its own tier.
          // Stored tiers are keyed by the canonical norm, so look up against c.norm.
          const priorTier = tierByNorm.has(c.norm) ? tierByNorm.get(c.norm)! : null;
          const tier = c.norm === toNormKey ? (mergedTier ?? priorTier ?? null) : priorTier;
          insTag.run(row.id, c.norm, outForm, tier);
        }
        touched = true;
      }
      const nextUser = rewritePostTags(row.user_tags, fromKeys, to);
      if (nextUser) {
        updUser.run(JSON.stringify(nextUser), row.id);
        delManual.run(row.id);
        // Canonicalize through resolveAlias like the AI rebuild above (and like
        // updateUserContent): a renamed/merged tag must land on its canonical norm,
        // not the raw one, or the manual index diverges from the maintained alias map.
        const seenManual = new Set<string>();
        for (const { norm, form } of normalizeTagRows(nextUser)) {
          const c = resolveAlias(norm);
          if (!c.norm || seenManual.has(c.norm)) continue;
          seenManual.add(c.norm);
          const outForm = c.norm === norm ? form : c.form || form;
          insManual.run(row.id, c.norm, outForm);
        }
        touched = true;
      }
      if (touched) updated++;
    }

    // Keep cluster membership coherent: the source tags vanish from post_tags, so
    // the target inherits the cluster of the first source that had one, and the
    // now-defunct source memberships are dropped.
    const toNorm = to.toLowerCase();
    const keys = [...fromKeys];
    const memSel = db!.prepare(
      'SELECT cluster_id AS cid FROM tag_cluster_membership WHERE tag_norm = ?',
    );
    let inheritCid: number | null = null;
    for (const key of keys) {
      const m = memSel.get(key) as { cid: number } | undefined;
      if (m) {
        inheritCid = m.cid;
        break;
      }
    }
    db!
      .prepare(
        `DELETE FROM tag_cluster_membership WHERE tag_norm IN (${keys.map(() => '?').join(',')})`,
      )
      .run(...keys);
    if (inheritCid !== null) {
      db!
        .prepare(
          'INSERT OR IGNORE INTO tag_cluster_membership (tag_norm, cluster_id) VALUES (?, ?)',
        )
        .run(toNorm, inheritCid);
    }
  });
  tx();
  invalidateGlobalCaches();
  return { updated };
}

// Ids of posts whose ai_tags contain at least one (or all) of `tags`,
// matched case-insensitively.
function getTagGraph({
  maxNodes = 60,
  minEdgeWeight = 2,
}: { maxNodes?: number; minEdgeWeight?: number } = {}): Shelfy.TagGraph {
  const topNodes = db!
    .prepare(
      `SELECT tag_norm AS id, tag_norm AS label, COUNT(*) AS weight
       FROM post_tags
       GROUP BY tag_norm
       ORDER BY weight DESC
       LIMIT ?`,
    )
    .all(maxNodes) as { id: string; label: string; weight: number }[];

  if (!topNodes.length) return { nodes: [], edges: [] };

  const nodeIds = topNodes.map((n) => n.id);
  const ph = nodeIds.map(() => '?').join(',');

  const edges = db!
    .prepare(
      `SELECT a.tag_norm AS source, b.tag_norm AS target, COUNT(*) AS weight
       FROM post_tags a
       JOIN post_tags b ON b.post_id = a.post_id AND a.tag_norm < b.tag_norm
       WHERE a.tag_norm IN (${ph}) AND b.tag_norm IN (${ph})
       GROUP BY a.tag_norm, b.tag_norm
       HAVING weight >= ?
       ORDER BY weight DESC`,
    )
    .all(...nodeIds, ...nodeIds, minEdgeWeight) as Shelfy.TagGraphEdge[];

  const clusterMap = new Map<string, number>();
  try {
    const members = db!
      .prepare(
        `SELECT m.tag_norm, m.cluster_id AS clusterId
         FROM tag_cluster_membership m
         JOIN tag_cluster c ON c.id = m.cluster_id
         WHERE c.status = 'accepted' AND m.tag_norm IN (${ph})`,
      )
      .all(...nodeIds) as { tag_norm: string; clusterId: number }[];
    for (const r of members) clusterMap.set(r.tag_norm, r.clusterId);
  } catch {}

  const nodes: Shelfy.TagGraphNode[] = topNodes.map((n) => ({
    id: n.id,
    label: n.label,
    weight: n.weight,
    clusterId: clusterMap.get(n.id) ?? null,
  }));

  return { nodes, edges };
}

function getPostIdsByTags(tags: string[] = [], mode: 'and' | 'or' | string = 'or'): string[] {
  if (!db) throw new Error('Database not initialized');
  const wanted = tags
    .map((t) =>
      String(t ?? '')
        .trim()
        .toLowerCase(),
    )
    .filter(Boolean);
  if (!wanted.length) return [];
  if (mode === 'and') {
    // Post must carry every wanted tag. A single GROUP BY ... HAVING does it in
    // one query when wanted fits in one parameter batch (wanted.length + 1 must
    // stay under SQLite's ~999-var limit — always true in practice). For the
    // pathological case of more tags than the limit, fall back to the per-tag
    // intersection in JS, whose COUNT-DISTINCT semantics survive chunking.
    if (wanted.length + 1 <= 900) {
      const rows = db
        .prepare(
          `SELECT post_id FROM post_tags WHERE tag_norm IN (${wanted.map(() => '?').join(',')})
           GROUP BY post_id HAVING COUNT(DISTINCT tag_norm) = ?`,
        )
        .all(...wanted, wanted.length) as { post_id: string }[];
      return rows.map((r) => r.post_id);
    }
    let acc: Set<string> | null = null;
    for (const tag of wanted) {
      const set = new Set(
        (
          db.prepare('SELECT post_id FROM post_tags WHERE tag_norm = ?').all(tag) as {
            post_id: string;
          }[]
        ).map((r) => r.post_id),
      );
      acc = acc === null ? set : new Set([...acc].filter((id: string) => set.has(id)));
      if (!acc.size) return [];
    }
    return [...(acc ?? [])];
  }
  const rows = chunkedInQuery<{ post_id: string }>(
    wanted,
    (ph) => `SELECT DISTINCT post_id FROM post_tags WHERE tag_norm IN (${ph})`,
  );
  // Chunking can yield the same post from multiple chunks → dedupe.
  return [...new Set(rows.map((r) => r.post_id))];
}

// Searches posts by tags with IDF-weighted relevance ranking. Posts are ordered
// by Σ idf(tag) over the MATCHED wanted tags DESC (rarer/more discriminating tags
// weigh more than ubiquitous ones), then by posts.timestamp DESC as a tie-break.
// Tags are normalized (lowercase/trim) exactly like getPostIdsByTags so they
// match the indexed post_tags.tag_norm. `mode`:
//   - 'or' (default): a post matches if it carries ANY of the wanted tags;
//   - 'and': a post matches only if it carries ALL of the wanted tags.
// `total` is the count of distinct matching posts BEFORE limit/offset.
// limit/offset are applied to the ORDERED id list, then hydrated with
// getPostsByIds (which preserves the input order). Empty tags → { posts:[], total:0 }.
function searchPostsByTags(
  tags: string[] = [],
  {
    mode = 'or',
    limit = 60,
    offset = 0,
    source = 'all',
  }: { mode?: 'and' | 'or' | string; limit?: number; offset?: number; source?: string } = {},
): { posts: Shelfy.Post[]; total: number } {
  if (!db) throw new Error('Database not initialized');
  // Canonicalize via resolveAlias (accepted aliases only) so a query for an alias
  // matches the canonical tag_norm that applyAiAnalysis actually indexes. Dedup:
  // two input aliases may resolve to the same canonical, which would otherwise
  // double-count its idf weight.
  const wanted = [...new Set(tags.map((t) => resolveAlias(t).norm).filter(Boolean))];
  if (!wanted.length) return { posts: [], total: 0 };

  const placeholders = wanted.map(() => '?').join(',');
  // idf per tag (clampato [1,3] come termIdfWeights), calcolato una volta. Lo score
  // del post è la SOMMA degli idf dei suoi tag matchati: una CASE per ciascun tag
  // wanted, col peso idf bound come parametro, sommata nell'aggregazione SQL.
  const idf = tagIdfWeights(wanted);
  const weightCases = wanted
    .map(() => `MAX(CASE WHEN pt.tag_norm = ? THEN ? ELSE 0 END)`)
    .join(' + ');
  const weightParams: (string | number)[] = [];
  for (const t of wanted) weightParams.push(t, idf.get(t) ?? 1);

  // Source bucket (F9 GAP-A): restrict the JOINed posts to web / social. 'all'
  // (default) adds no constraint, so existing tag search behaves exactly as before.
  let sourceClause = '';
  if (source === 'web') sourceClause = "AND p.platform = 'web'";
  else if (source === 'social') sourceClause = "AND p.platform != 'web'";

  // For 'and', keep only posts that carry every wanted tag (same HAVING as
  // getPostIdsByTags 'and'). total resta il conteggio dei post distinti.
  const having = mode === 'and' ? 'HAVING COUNT(DISTINCT pt.tag_norm) = ?' : '';
  const sql = `
    SELECT pt.post_id AS id, (${weightCases}) AS score, p.timestamp AS ts
    FROM post_tags pt JOIN posts p ON p.id = pt.post_id
    WHERE pt.tag_norm IN (${placeholders}) ${sourceClause}
    GROUP BY pt.post_id
    ${having}
    ORDER BY score DESC, p.timestamp DESC
  `;
  const params =
    mode === 'and' ? [...weightParams, ...wanted, wanted.length] : [...weightParams, ...wanted];
  const rows = db.prepare(sql).all(...params) as { id: string; score: number; ts: string | null }[];

  const total = rows.length;
  const pageIds = rows.slice(offset, offset + limit).map((r) => r.id);
  const posts = getPostsByIds(pageIds); // preserves the ordered id list
  return { posts, total };
}

// Hybrid search: combines tag-based filtering with full-text search on post
// content. NON reimplementa LIKE+ordinamento: instrada su buildPostFilter/getPosts
// così che lo score finale FONDA il match dei tag con la relevanceExpr testuale
// (vedi buildPostFilter, ramo hybridTags). Semantica:
//   - mode 'and': il post deve avere TUTTI i tag E matchare il testo;
//   - mode 'or':  match per tag O per testo, ranking per score fuso (idf tag +
//                 relevance testo).
// I path puri (solo-tag, solo-testo) delegano alle funzioni dedicate.
function searchPostsHybrid(
  tags: string[] = [],
  textQuery = '',
  {
    mode = 'or',
    limit = 60,
    offset = 0,
    source = 'all',
  }: { mode?: 'and' | 'or' | string; limit?: number; offset?: number; source?: string } = {},
): { posts: Shelfy.Post[]; total: number } {
  if (!db) throw new Error('Database not initialized');

  // Same alias canonicalization (+ dedup) as searchPostsByTags, so the hybrid
  // path matches the indexed canonical tag_norms too.
  const wanted = [...new Set(tags.map((t) => resolveAlias(t).norm).filter(Boolean))];
  const terms = contentTermsOrRaw(textQuery);

  const hasTags = wanted.length > 0;
  const hasText = terms.length > 0;

  // Source bucket (F9 GAP-A): forwarded to whichever delegate path runs, so the
  // restriction holds across all three branches. Default 'all' is unconstrained.
  if (!hasTags && !hasText) return { posts: [], total: 0 };
  if (!hasTags) return getPosts({ search: textQuery, limit, offset, source });
  if (!hasText) return searchPostsByTags(tags, { mode, limit, offset, source });

  // Both dimensions active → un'unica query via getPosts: tags + search nel filtro,
  // tagMode che pilota AND/OR. buildPostFilter fonde gli idf dei tag nella
  // relevanceExpr testuale e (in OR) li OR-a nel blocco testo.
  return getPosts({ search: textQuery, tags: wanted, tagMode: mode, limit, offset, source });
}

function normalizeImportedPost(post: RawImportPost): PostInput {
  // The parsers accept their own ExportedPost shape (id?: string); a raw export
  // record is a structural superset (id may be number), so route it through the
  // parsers via unknown — runtime behavior is unchanged (they read id||shortcode).
  const raw = post as unknown as Parameters<typeof normalizeIg>[0];
  if (post.platform === 'instagram' || post.shortcode) return normalizeIg(raw) as PostInput;
  return normalizeTw(raw as Parameters<typeof normalizeTw>[0]) as PostInput;
}

// Returns { imported: N }
async function importFromJSON(filePath: string): Promise<Shelfy.ImportResult> {
  if (!db) throw new Error('Database not initialized');

  let parsed: unknown;
  try {
    // Async read: a multi-hundred-MB export must not freeze the main process for
    // the disk I/O share of the stall. JSON.parse below is still synchronous (no
    // streaming parser without a new dependency) — documented residual limit.
    const raw = await fs.promises.readFile(filePath, 'utf8');
    parsed = JSON.parse(raw);
  } catch (err) {
    throw new Error(`Failed to read or parse "${filePath}": ${(err as Error).message}`);
  }
  const parsedObj = parsed as { posts?: unknown[]; collections?: unknown[] } | null;
  const posts = (Array.isArray(parsed) ? parsed : (parsedObj?.posts ?? [])) as RawImportPost[];
  const collectionDefs = (
    parsed && !Array.isArray(parsed) && Array.isArray(parsedObj?.collections)
      ? parsedObj!.collections
      : []
  ) as CollectionDef[];

  let normalized: PostInput[];
  try {
    normalized = posts.map(normalizeImportedPost);
  } catch (err) {
    throw new Error(`Failed to normalize posts: ${(err as Error).message}`);
  }

  // Import deliberato: aggiorna anche l'AI dei post già esistenti (overwriteAi),
  // così reimportare una propria ri-analisi rinfresca i tag invece di saltarli.
  // `imported` = righe nuove; `updated` = post esistenti la cui AI è stata rinfrescata.
  const { inserted, aiUpdated } = bulkUpsert(normalized, { overwriteAi: true });

  // Ricrea le collezioni (cartelle, incl. Instagram) e ri-collega i post. Usa i post
  // RAW (parsed), che portano la chiave `collections`, non quelli normalizzati.
  // bulkUpsert ha già committato la propria transazione: se le collezioni falliscono
  // (export corrotto) degradiamo con grazia — i post restano importati, i legami
  // sono best-effort — invece di rigettare l'intera operazione.
  let col = { collectionsCreated: 0, linksAdded: 0 };
  try {
    col = importCollections(collectionDefs, posts);
  } catch (err) {
    console.error('importFromJSON: importCollections failed, posts kept:', err);
  }

  return {
    imported: inserted,
    updated: aiUpdated,
    collections: col.collectionsCreated,
    links: col.linksAdded,
  };
}

// Attach the persisted tag TIER split (P2) to exported posts so it round-trips:
// aiGeneralTags / aiSpecificTags are read from post_tags.tier (display form). Posts
// with no tier (legacy/NULL) get nothing → on re-import they fall back to flat
// aiTags, exactly as before. ai_tags (flat) stays the source of truth alongside.
function attachTagTiers(posts: Shelfy.Post[]): Shelfy.Post[] {
  if (!db || !posts.length) return posts;
  const ids = posts.map((p) => p.id);
  const rows = chunkedInQuery<{ pid: string; form: string; tier: Shelfy.TagTier | null }>(
    ids,
    (ph) =>
      `SELECT post_id AS pid, tag_form AS form, tier FROM post_tags WHERE post_id IN (${ph}) AND tier IS NOT NULL`,
  );
  const gen = new Map<string, string[]>();
  const spec = new Map<string, string[]>();
  for (const r of rows) {
    const m = r.tier === 'general' ? gen : r.tier === 'specific' ? spec : null;
    if (!m) continue;
    if (!m.has(r.pid)) m.set(r.pid, []);
    m.get(r.pid)!.push(r.form);
  }
  for (const p of posts) {
    const g = gen.get(p.id);
    const s = spec.get(p.id);
    if (g && g.length) p.aiGeneralTags = g;
    if (s && s.length) p.aiSpecificTags = s;
  }
  return posts;
}

// ─── Round-trip delle collezioni (cartelle, incluse quelle Instagram) ──────────
//
// Le collezioni (sidebar "sorgenti"/cartelle) hanno un id autoincrement NON
// portabile tra installazioni. Per esportarle/importarle usiamo una CHIAVE stabile:
// l'external_id per le cartelle Instagram (rename-safe), il nome per quelle manuali.
function collectionKey(c: CollectionDef): string {
  const ext = c.externalId ?? c.external_id;
  return ext != null && String(ext) !== '' ? `x:${ext}` : `n:${String(c.name ?? '').trim()}`;
}

// Definizioni collezioni per l'export JSON (senza id/count volatili).
function getCollectionsForExport(): Shelfy.CollectionExport[] {
  if (!db) throw new Error('Database not initialized');
  return db
    .prepare(
      'SELECT name, color, platform, external_id AS externalId, ig_name AS igName FROM collections ORDER BY created_at ASC, id ASC',
    )
    .all() as Shelfy.CollectionExport[];
}

// Allega a ogni post le CHIAVI delle collezioni a cui appartiene (non gli id), così
// l'appartenenza sopravvive a export→import su un'altra installazione.
function attachCollectionKeys(posts: Shelfy.Post[]): Shelfy.Post[] {
  if (!db || !posts.length) return posts;
  const ids = posts.map((p) => p.id);
  const rows = chunkedInQuery<{ pid: string; name: string; externalId: string | null }>(
    ids,
    (ph) => `SELECT pc.post_id AS pid, c.name AS name, c.external_id AS externalId
             FROM post_collections pc JOIN collections c ON c.id = pc.collection_id
             WHERE pc.post_id IN (${ph})`,
  );
  const byPost = new Map<string, string[]>();
  for (const r of rows) {
    const key = collectionKey(r);
    if (!byPost.has(r.pid)) byPost.set(r.pid, []);
    byPost.get(r.pid)!.push(key);
  }
  for (const p of posts) {
    const k = byPost.get(p.id);
    if (k && k.length) p.collections = k;
  }
  return posts;
}

// Returns saved posts (with ordered media + tag tier + collection keys) for a JSON
// export. When `platforms` is a non-empty array, only posts from those sources are
// included. Collection DEFINITIONS go alongside (see getCollectionsForExport).
function exportAllPosts(platforms?: string[]): Shelfy.Post[] {
  if (!db) throw new Error('Database not initialized');
  let sql = 'SELECT * FROM posts';
  const params: string[] = [];
  if (Array.isArray(platforms) && platforms.length > 0) {
    sql += ` WHERE platform IN (${platforms.map(() => '?').join(',')})`;
    params.push(...platforms);
  }
  sql += ' ORDER BY timestamp DESC';
  const rows = db.prepare(sql).all(...params) as PostRow[];
  return attachCollectionKeys(
    attachTagTiers(attachMedia(rows.map(rowToPost).filter((p): p is Shelfy.Post => p !== null))),
  );
}

// Find-or-create di una collezione importata. Abbina per external_id (cartelle IG,
// rename-safe) o, in assenza, per nome tra le manuali (external_id NULL) — così non
// si duplicano le cartelle a ogni reimport. Ritorna { id, created }.
function findOrCreateCollection(def: CollectionDef): { id: number; created: boolean } {
  const ext =
    def.externalId != null && String(def.externalId) !== '' ? String(def.externalId) : null;
  let row: { id: number } | undefined;
  if (ext != null)
    row = db!.prepare('SELECT id FROM collections WHERE external_id = ?').get(ext) as
      | { id: number }
      | undefined;
  if (!row && def.name)
    row = db!
      .prepare('SELECT id FROM collections WHERE name = ? AND external_id IS NULL')
      .get(String(def.name).trim()) as { id: number } | undefined;
  if (row) return { id: row.id, created: false };
  return { id: createCollection(def).id, created: true };
}

// Ricrea/abbina le collezioni di un export e ri-collega i post. `defs` sono le
// definizioni top-level; `rawPosts` sono i post NON normalizzati (portano la chiave
// `collections`). Idempotente: reimportare non duplica né le cartelle né i legami.
function importCollections(
  defs: CollectionDef[] = [],
  rawPosts: RawImportPost[] = [],
): { collectionsCreated: number; linksAdded: number } {
  if (
    !db ||
    (!defs.length &&
      !rawPosts.some((p) => Array.isArray(p && p.collections) && p.collections!.length))
  ) {
    return { collectionsCreated: 0, linksAdded: 0 };
  }
  const keyToId = new Map<string, number>();
  let created = 0;
  const ensure = (def: CollectionDef): number => {
    const key = collectionKey(def);
    if (keyToId.has(key)) return keyToId.get(key)!;
    const { id, created: isNew } = findOrCreateCollection(def);
    if (isNew) created++;
    keyToId.set(key, id);
    return id;
  };
  let lastLinks = 0;
  const tx = db.transaction(() => {
    for (const def of defs) if (def && (def.name || def.externalId != null)) ensure(def);
    const byCol = new Map<number, Set<string>>(); // localId → Set(postId)
    for (const p of rawPosts) {
      const pid = p && p.id != null ? String(p.id) : null;
      if (!pid || !Array.isArray(p.collections)) continue;
      for (const rawKey of p.collections) {
        // L'export produce sempre chiavi stringa (collectionKey), ma un file
        // editato/corrotto a mano può portare un elemento non-stringa: scartalo
        // invece di far crollare l'intero import (i post sono già committati).
        const key = typeof rawKey === 'string' ? rawKey : null;
        if (!key) continue;
        let id = keyToId.get(key);
        if (id == null) {
          // Chiave non presente tra le def → ricostruisci una def minimale dalla chiave.
          const def: CollectionDef = key.startsWith('x:')
            ? { externalId: key.slice(2), name: key.slice(2), platform: 'instagram' }
            : { name: key.slice(2) };
          id = ensure(def);
        }
        if (id == null) continue;
        if (!byCol.has(id)) byCol.set(id, new Set());
        byCol.get(id)!.add(pid);
      }
    }
    let links = 0;
    for (const [id, pids] of byCol) links += addPostsToCollections([...pids], [id]).added;
    lastLinks = links;
  });
  tx();
  return { collectionsCreated: created, linksAdded: lastLinks };
}

// ─── Custom sources (collections) ─────────────────────────────────────────────

function getCollections(): Shelfy.Collection[] {
  if (!db) throw new Error('Database not initialized');
  const rows = db
    .prepare(
      `
    SELECT c.id, c.name, c.color, c.platform, c.external_id, c.ig_name, c.created_at,
      (SELECT COUNT(*) FROM post_collections pc WHERE pc.collection_id = c.id) AS count
    FROM collections c
    ORDER BY c.created_at ASC, c.id ASC
  `,
    )
    .all() as {
    id: number;
    name: string;
    color: string;
    platform: Shelfy.Platform | null;
    external_id: string | null;
    ig_name: string | null;
    created_at: number;
    count: number;
  }[];
  return rows.map((r) => ({
    id: r.id,
    name: r.name,
    color: r.color,
    platform: r.platform ?? null,
    externalId: r.external_id ?? null,
    igName: r.ig_name ?? null,
    count: r.count,
    createdAt: r.created_at,
  }));
}

function createCollection({
  name,
  color,
  platform,
  externalId,
  igName,
}: CollectionDef = {}): Shelfy.Collection {
  if (!db) throw new Error('Database not initialized');
  const trimmed = (name ?? '').trim();
  if (!trimmed) throw new Error('Collection name is required');
  const col = color || '#3d5afe';
  const info = db
    .prepare(
      'INSERT INTO collections (name, color, platform, external_id, ig_name) VALUES (?, ?, ?, ?, ?)',
    )
    .run(
      trimmed,
      col,
      platform || null,
      externalId != null ? String(externalId) : null,
      igName || null,
    );
  return {
    id: info.lastInsertRowid as number,
    name: trimmed,
    color: col,
    platform: (platform as Shelfy.Platform | null) || null,
    externalId: externalId != null ? String(externalId) : null,
    igName: igName || null,
    count: 0,
  };
}

function updateCollection(
  id: number,
  { name, color }: { name?: string; color?: string } = {},
): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare(
    `
    UPDATE collections SET
      name = COALESCE(?, name),
      color = COALESCE(?, color)
    WHERE id = ?
  `,
  ).run(name?.trim() || null, color || null, id);
}

function deleteCollection(id: number): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare('DELETE FROM collections WHERE id = ?').run(id);
}

// Add many posts to many collections at once (INSERT OR IGNORE so re-adding is
// a no-op). Returns the number of (post, collection) links now present.
function addPostsToCollections(
  postIds: string[] = [],
  collectionIds: number[] = [],
): { added: number } {
  if (!db) throw new Error('Database not initialized');
  const ins = db.prepare(
    'INSERT OR IGNORE INTO post_collections (post_id, collection_id) VALUES (?, ?)',
  );
  let added = 0;
  const tx = db.transaction(() => {
    for (const pid of postIds) {
      for (const cid of collectionIds) {
        added += ins.run(pid, cid).changes;
      }
    }
  });
  tx();
  return { added };
}

function removePostFromCollection(postId: string, collectionId: number): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare('DELETE FROM post_collections WHERE post_id = ? AND collection_id = ?').run(
    postId,
    collectionId,
  );
}

// Permanently removes posts (and everything that cascades from them via the
// ON DELETE CASCADE foreign keys: post_media, downloads, post_collections,
// post_tags, post_entities). Tag clusters aren't tied to posts so they're left
// alone. On-disk files are deleted separately by the IPC layer. Returns the
// number of post rows removed.
function deletePosts(ids: string[] = []): { deleted: number } {
  if (!db) throw new Error('Database not initialized');
  const list = Array.isArray(ids) ? ids.filter(Boolean) : [];
  if (list.length === 0) return { deleted: 0 };
  const del = db.prepare('DELETE FROM posts WHERE id = ?');
  let deleted = 0;
  const tx = db.transaction(() => {
    for (const id of list) deleted += del.run(id).changes;
  });
  tx();
  invalidateGlobalCaches();
  return { deleted };
}

// ─── Background job persistence (jobs table) ──────────────────────────────────
//
// Generic mirror of the three in-memory queues (downloader/analyzer/web). The
// managers own the semantics; here we only do plain CRUD keyed by (kind, key).

const JOB_UPSERT_SQL = `INSERT INTO jobs (kind, key, post_id, payload, status, progress, error, attempts, updated_at)
     VALUES (@kind, @key, @post_id, @payload, @status, @progress, @error, @attempts, unixepoch())
     ON CONFLICT(kind, key) DO UPDATE SET
       post_id  = excluded.post_id,
       payload  = excluded.payload,
       status   = excluded.status,
       progress = excluded.progress,
       error    = excluded.error,
       attempts = excluded.attempts,
       updated_at = unixepoch()`;

function jobRowParams({
  kind,
  key,
  post_id = null,
  payload = null,
  status,
  progress = 0,
  error = null,
  attempts = 0,
}: JobUpsertRow): {
  kind: string;
  key: string;
  post_id: string | null;
  payload: string | null;
  status: string;
  progress: number;
  error: string | null;
  attempts: number;
} {
  return { kind, key, post_id, payload, status, progress, error, attempts };
}

function jobUpsert(row: JobUpsertRow): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare(JOB_UPSERT_SQL).run(jobRowParams(row));
}

// Batch variant for the queues' bulk paths (enqueueMany/cancelAll/recover): one
// transaction instead of one implicit WAL commit per job.
function jobsUpsertMany(rows: JobUpsertRow[] = []): void {
  if (!db) throw new Error('Database not initialized');
  const list = Array.isArray(rows) ? rows : [];
  if (list.length === 0) return;
  const stmt = db.prepare(JOB_UPSERT_SQL);
  db.transaction((items: JobUpsertRow[]) => {
    for (const row of items) stmt.run(jobRowParams(row));
  })(list);
}

function jobDelete(kind: string, key: string): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare('DELETE FROM jobs WHERE kind = ? AND key = ?').run(kind, key);
}

function jobsDeleteMany(kind: string, keys: string[] = []): void {
  if (!db) throw new Error('Database not initialized');
  const list = Array.isArray(keys) ? keys : [];
  if (list.length === 0) return;
  const del = db.prepare('DELETE FROM jobs WHERE kind = ? AND key = ?');
  db.transaction((ks: string[]) => {
    for (const key of ks) del.run(kind, key);
  })(list);
}

function jobDeleteAll(kind: string): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare('DELETE FROM jobs WHERE kind = ?').run(kind);
}

function jobsByKind(kind: string): Shelfy.Job[] {
  if (!db) throw new Error('Database not initialized');
  return db
    .prepare('SELECT * FROM jobs WHERE kind = ? ORDER BY created_at, rowid')
    .all(kind) as Shelfy.Job[];
}

// Boot cleanup: a crash mid-analysis leaves posts pinned at ai_status='analyzing'
// forever. Reset them so they count as unanalyzed again — the analyzer's recover()
// re-enqueues the ones that still have a job row; the rest become re-analyzable.
function clearStuckAnalyzing(): number {
  if (!db) throw new Error('Database not initialized');
  const r = db.prepare("UPDATE posts SET ai_status = NULL WHERE ai_status = 'analyzing'").run();
  if (r.changes > 0) invalidateGlobalCaches();
  return r.changes;
}

function clearAllData(): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare('DELETE FROM posts').run();
  // tag clusters and aliases don't cascade from posts (tag_alias is keyed by
  // alias_norm with no FK to posts); clear them explicitly and drop the memoized
  // global tag/keyword caches so they don't go stale.
  db.prepare('DELETE FROM tag_cluster_membership').run();
  db.prepare('DELETE FROM tag_cluster').run();
  db.prepare('DELETE FROM tag_alias').run();
  invalidateGlobalCaches();
}

// Wipes every AI-generated field across all posts (description, tags, entities,
// keywords, category, content type, language, save reason) and resets ai_status
// so the posts count as unanalyzed and can be re-analyzed. Also clears the
// derived tag/entity indexes and the tag clusters built from them. Post
// metadata and downloaded files are left untouched.
function clearAllAiAnalysis(): { ok: boolean } {
  if (!db) throw new Error('Database not initialized');
  const tx = db.transaction(() => {
    db!
      .prepare(
        `
      UPDATE posts SET
        ai_description = NULL,
        ai_tags = NULL,
        ai_status = NULL,
        ai_model = NULL,
        ai_analyzed_at = NULL,
        ai_category = NULL,
        ai_content_type = NULL,
        ai_entities = NULL,
        ai_keywords = NULL,
        ai_language = NULL,
        ai_save_reason = NULL
    `,
      )
      .run();
    // Preserve the user's manual tags (tier='manual') — they are user content,
    // not AI analysis, so a "clear all AI tags" must not delete them.
    db!.prepare("DELETE FROM post_tags WHERE tier IS NULL OR tier IN ('general','specific')").run();
    db!.prepare('DELETE FROM post_entities').run();
    db!.prepare('DELETE FROM tag_cluster_membership').run();
    db!.prepare('DELETE FROM tag_cluster').run();
    // Aliases are AI-derived (proposed/accepted tag merges) and don't cascade
    // from post_tags, so a "clear all AI analysis" must drop them too — otherwise
    // the AI Tags review screen keeps listing aliases for a wiped library.
    db!.prepare('DELETE FROM tag_alias').run();
  });
  tx();
  invalidateGlobalCaches();
  return { ok: true };
}

// Forgets every local file path without touching post metadata, so posts show
// as not-downloaded again. Pairs with downloader.clearAllAssets() deleting the
// actual files on disk.
function clearAllAssetPaths(): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare('UPDATE posts SET thumbnail_path = NULL, image_path = NULL, video_path = NULL').run();
  db.prepare('UPDATE post_media SET local_path = NULL').run();
}

// Returns all local file paths for a post (main row + per-slide media).
// Files of the CURRENT capture only (posts row paths + post_media). Does NOT
// include archived snapshot screenshots — that's getLocalFilePaths' job (which
// unions snapshots in) and getWebSiteFilePaths'. deleteLatestReport needs the
// current-only set so it never unlinks a screenshot it is about to promote.
function getCurrentCaptureFilePaths(postId: string): string[] {
  if (!db) throw new Error('Database not initialized');
  const row = db
    .prepare('SELECT thumbnail_path, image_path, video_path FROM posts WHERE id = ?')
    .get(postId) as
    | { thumbnail_path: string | null; image_path: string | null; video_path: string | null }
    | undefined;
  const slides = db
    .prepare('SELECT local_path, source_url FROM post_media WHERE post_id = ?')
    .all(postId) as { local_path: string | null; source_url: string | null }[];
  const paths: (string | null | undefined)[] = [
    row?.thumbnail_path,
    row?.image_path,
    row?.video_path,
  ];
  // Manual bookmarks (electron/bookmarks.js) store the copied original file's
  // FILESYSTEM path in post_media.source_url (for social posts it is a remote
  // URL). Include it only when it points inside userData/assets, so deleting a
  // manual pdf/file post also removes the copied original — never a remote URL
  // or an arbitrary path outside our asset dirs.
  const assetsRoot = path.join(app.getPath('userData'), 'assets') + path.sep;
  for (const s of slides) {
    if (s.local_path) paths.push(s.local_path);
    if (s.source_url && s.source_url.startsWith(assetsRoot)) paths.push(s.source_url);
  }
  // Dedup: for manual image/video slides local_path === source_url (same file).
  return [...new Set(paths.filter((p): p is string => Boolean(p)))];
}

// Every on-disk file owned by a post when it is FULLY deleted: the current
// capture plus, for web posts, every archived snapshot's screenshots. The
// generic delete path (Gallery multi-select, collections:delete → deletePosts)
// goes through here, so it must union in the web_snapshots files — otherwise the
// snapshot rows cascade away while their screenshots leak on disk forever with
// no row left to reclaim them.
function getLocalFilePaths(postId: string): string[] {
  if (!db) throw new Error('Database not initialized');
  const current = getCurrentCaptureFilePaths(postId);
  const snaps = db
    .prepare('SELECT web_pages_json FROM web_snapshots WHERE post_id = ?')
    .all(postId) as { web_pages_json: string | null }[];
  if (!snaps.length) return current;
  const snapPaths = snaps.flatMap((s) => snapshotPagePaths(s.web_pages_json));
  return [...new Set([...current, ...snapPaths])];
}

// ─── Web snapshots (dated version history of a site) ────────────────────────
//
// The posts row holds the CURRENT capture; web_snapshots holds the older ones.
// A re-capture archives the prior posts state here before overwriting it.

// The on-disk screenshot paths referenced by a snapshot's web_pages_json — the
// hero screenshotPath plus every vertical chunk band, so cleanup never orphans the
// sliced files of a tall capture.
function snapshotPagePaths(webPagesJson: string | null): string[] {
  const pages = parseJson<Shelfy.WebPage[]>(webPagesJson, []);
  if (!Array.isArray(pages)) return [];
  const out: string[] = [];
  for (const p of pages) {
    if (!p) continue;
    if (p.screenshotPath) out.push(p.screenshotPath);
    if (Array.isArray(p.chunks)) {
      for (const c of p.chunks) if (c && c.screenshotPath) out.push(c.screenshotPath);
    }
  }
  // Dedup: chunk[0] usually equals screenshotPath on short (single-chunk) pages.
  return [...new Set(out)];
}

// A raw `web_snapshots` row (snake_case + *_json columns).
interface WebSnapshotRow {
  id: number;
  post_id: string;
  captured_at: number;
  title: string | null;
  web_pages_json: string | null;
  web_palette_json: string | null;
  web_fonts_json: string | null;
  web_tech_json: string | null;
  web_awards_json: string | null;
  web_meta_json: string | null;
  ai_description: string | null;
  ai_tags_json: string | null;
  ai_model: string | null;
  ai_status: Shelfy.AiStatus | null;
  ai_analyzed_at: number | null;
  ai_category: string | null;
  ai_content_type: string | null;
  ai_entities_json: string | null;
  ai_keywords_json: string | null;
  ai_language: string | null;
  ai_save_reason: string | null;
  created_at: number;
}

// Copy the current posts row's web/AI state into web_snapshots, IF it carries a
// real prior capture (has captured_at + at least one page). Returns the new
// snapshot id, or null when there's nothing worth archiving (first capture or a
// bare placeholder). Safe to call inside upsertWebReference before the overwrite.
function archiveCurrentWebSnapshot(postId: string): number | null {
  if (!db) throw new Error('Database not initialized');
  const row = db
    .prepare(
      `SELECT author_name, web_palette_json, web_fonts_json, web_tech_json,
              web_awards_json, web_pages_json, web_meta_json, web_captured_at,
              ai_description, ai_tags, ai_model, ai_status, ai_analyzed_at,
              ai_category, ai_content_type, ai_entities, ai_keywords,
              ai_language, ai_save_reason
       FROM posts WHERE id = ?`,
    )
    .get(postId) as
    | Pick<
        PostRow,
        | 'author_name'
        | 'web_palette_json'
        | 'web_fonts_json'
        | 'web_tech_json'
        | 'web_awards_json'
        | 'web_pages_json'
        | 'web_meta_json'
        | 'web_captured_at'
        | 'ai_description'
        | 'ai_tags'
        | 'ai_model'
        | 'ai_status'
        | 'ai_analyzed_at'
        | 'ai_category'
        | 'ai_content_type'
        | 'ai_entities'
        | 'ai_keywords'
        | 'ai_language'
        | 'ai_save_reason'
      >
    | undefined;
  if (!row || !row.web_captured_at || !row.web_pages_json) return null;
  const pages = parseJson<Shelfy.WebPage[]>(row.web_pages_json, []);
  if (!Array.isArray(pages) || pages.length === 0) return null;
  const info = db
    .prepare(
      `INSERT INTO web_snapshots
         (post_id, captured_at, title, web_pages_json, web_palette_json, web_fonts_json,
          web_tech_json, web_awards_json, web_meta_json, ai_description, ai_tags_json,
          ai_model, ai_status, ai_analyzed_at, ai_category, ai_content_type,
          ai_entities_json, ai_keywords_json, ai_language, ai_save_reason, created_at)
       VALUES
         (@post_id, @captured_at, @title, @web_pages_json, @web_palette_json, @web_fonts_json,
          @web_tech_json, @web_awards_json, @web_meta_json, @ai_description, @ai_tags_json,
          @ai_model, @ai_status, @ai_analyzed_at, @ai_category, @ai_content_type,
          @ai_entities_json, @ai_keywords_json, @ai_language, @ai_save_reason, @created_at)`,
    )
    .run({
      post_id: postId,
      captured_at: row.web_captured_at,
      title: row.author_name ?? null,
      web_pages_json: row.web_pages_json ?? null,
      web_palette_json: row.web_palette_json ?? null,
      web_fonts_json: row.web_fonts_json ?? null,
      web_tech_json: row.web_tech_json ?? null,
      web_awards_json: row.web_awards_json ?? null,
      web_meta_json: row.web_meta_json ?? null,
      ai_description: row.ai_description ?? null,
      ai_tags_json: row.ai_tags ?? null,
      ai_model: row.ai_model ?? null,
      ai_status: row.ai_status ?? null,
      ai_analyzed_at: row.ai_analyzed_at ?? null,
      ai_category: row.ai_category ?? null,
      ai_content_type: row.ai_content_type ?? null,
      ai_entities_json: row.ai_entities ?? null,
      ai_keywords_json: row.ai_keywords ?? null,
      ai_language: row.ai_language ?? null,
      ai_save_reason: row.ai_save_reason ?? null,
      created_at: Math.floor(Date.now() / 1000),
    });
  return info.lastInsertRowid as number;
}

// Archived versions of a site, newest first, JSON parsed back to camelCase for
// the UI (mirrors toLight's web/AI shape).
function getWebSnapshots(postId: string): Shelfy.WebSnapshot[] {
  if (!db) throw new Error('Database not initialized');
  const rows = db
    .prepare('SELECT * FROM web_snapshots WHERE post_id = ? ORDER BY captured_at DESC, id DESC')
    .all(postId) as WebSnapshotRow[];
  return rows.map((r) => ({
    id: r.id,
    postId: r.post_id,
    capturedAt: r.captured_at,
    title: r.title ?? null,
    webPages: parseJson<Shelfy.WebPage[]>(r.web_pages_json, []) ?? [],
    webPalette: parseJson<string[]>(r.web_palette_json, []) ?? [],
    webFonts: parseJson<Shelfy.WebFont[]>(r.web_fonts_json, []) ?? [],
    webTech: parseJson<string[]>(r.web_tech_json, []) ?? [],
    webAwards: parseJson<Shelfy.WebAward[]>(r.web_awards_json, []) ?? [],
    webMeta: parseJson<Shelfy.WebMeta>(r.web_meta_json, null),
    aiDescription: r.ai_description ?? null,
    aiTags: parseTags(r.ai_tags_json),
    aiModel: r.ai_model ?? null,
    aiStatus: r.ai_status ?? null,
    aiCategory: r.ai_category ?? null,
    aiContentType: r.ai_content_type ?? null,
    aiEntities: parseTags(r.ai_entities_json),
    aiKeywords: parseTags(r.ai_keywords_json),
    aiLanguage: r.ai_language ?? null,
    aiSaveReason: r.ai_save_reason ?? null,
  }));
}

// Per-post count of ARCHIVED snapshots (the current capture isn't counted). The
// UI adds +1 for the current version to render the "⧉N" badge.
function getWebSnapshotCounts(): Record<string, number> {
  if (!db) throw new Error('Database not initialized');
  const rows = db
    .prepare('SELECT post_id, COUNT(*) AS n FROM web_snapshots GROUP BY post_id')
    .all() as { post_id: string; n: number }[];
  const out: Record<string, number> = {};
  for (const r of rows) out[r.post_id] = r.n;
  return out;
}

// Delete one archived snapshot. Returns its on-disk screenshot paths so the
// caller can unlink them (the row's foreign data is gone after this).
function deleteWebSnapshot(id: number): { pagePaths: string[] } {
  if (!db) throw new Error('Database not initialized');
  const row = db.prepare('SELECT web_pages_json FROM web_snapshots WHERE id = ?').get(id) as
    | { web_pages_json: string | null }
    | undefined;
  if (!row) return { pagePaths: [] };
  const pagePaths = snapshotPagePaths(row.web_pages_json);
  db.prepare('DELETE FROM web_snapshots WHERE id = ?').run(id);
  return { pagePaths };
}

// Every on-disk file owned by a site: the current capture (posts row + media)
// plus every archived snapshot's screenshots. Used to clean disk on a full
// "delete site". Deduped.
function getWebSiteFilePaths(postId: string): string[] {
  if (!db) throw new Error('Database not initialized');
  const current = getCurrentCaptureFilePaths(postId);
  const snaps = db
    .prepare('SELECT web_pages_json FROM web_snapshots WHERE post_id = ?')
    .all(postId) as { web_pages_json: string | null }[];
  const snapPaths = snaps.flatMap((s) => snapshotPagePaths(s.web_pages_json));
  return [...new Set([...current, ...snapPaths])];
}

// Restore an archived snapshot into the posts row (it becomes the current
// version), rebuild media + derived tag/entity indexes, then drop the snapshot
// row. Returns false when the snapshot doesn't belong to the post.
function promoteSnapshotToPost(postId: string, snapshotId: number): boolean {
  if (!db) throw new Error('Database not initialized');
  const snap = db
    .prepare('SELECT * FROM web_snapshots WHERE id = ? AND post_id = ?')
    .get(snapshotId, postId) as WebSnapshotRow | undefined;
  if (!snap) return false;
  const pages = parseJson<Shelfy.WebPage[]>(snap.web_pages_json, []);
  const hero =
    (Array.isArray(pages) ? pages : []).find((p) => p && p.screenshotPath) ||
    ({} as Shelfy.WebPage);
  db.prepare(
    `UPDATE posts SET
       author_name = @title,
       timestamp = @timestamp,
       thumbnail_path = @hero, image_path = @hero,
       web_palette_json = @web_palette_json,
       web_fonts_json = @web_fonts_json,
       web_tech_json = @web_tech_json,
       web_awards_json = @web_awards_json,
       web_pages_json = @web_pages_json,
       web_meta_json = @web_meta_json,
       web_captured_at = @captured_at,
       ai_description = @ai_description,
       ai_tags = @ai_tags_json,
       ai_model = @ai_model,
       ai_status = @ai_status,
       ai_analyzed_at = @ai_analyzed_at,
       ai_category = @ai_category,
       ai_content_type = @ai_content_type,
       ai_entities = @ai_entities_json,
       ai_keywords = @ai_keywords_json,
       ai_language = @ai_language,
       ai_save_reason = @ai_save_reason
     WHERE id = @id`,
  ).run({
    id: postId,
    title: snap.title ?? null,
    timestamp: new Date((snap.captured_at || 0) * 1000).toISOString(),
    hero: hero.screenshotPath || null,
    web_palette_json: snap.web_palette_json ?? null,
    web_fonts_json: snap.web_fonts_json ?? null,
    web_tech_json: snap.web_tech_json ?? null,
    web_awards_json: snap.web_awards_json ?? null,
    web_pages_json: snap.web_pages_json ?? null,
    web_meta_json: snap.web_meta_json ?? null,
    captured_at: snap.captured_at,
    ai_description: snap.ai_description ?? null,
    ai_tags_json: snap.ai_tags_json ?? null,
    ai_model: snap.ai_model ?? null,
    ai_status: snap.ai_status ?? null,
    ai_analyzed_at: snap.ai_analyzed_at ?? null,
    ai_category: snap.ai_category ?? null,
    ai_content_type: snap.ai_content_type ?? null,
    ai_entities_json: snap.ai_entities_json ?? null,
    ai_keywords_json: snap.ai_keywords_json ?? null,
    ai_language: snap.ai_language ?? null,
    ai_save_reason: snap.ai_save_reason ?? null,
  });
  const media: DerivedMedia[] = (Array.isArray(pages) ? pages : [])
    .filter((p) => p && p.screenshotPath)
    .map((p) => ({ type: 'image', url: p.url || '', localPath: p.screenshotPath }));
  replacePostMedia(postId, media);
  // Resync the derived post_tags / post_entities indexes to the promoted AI.
  applyAiAnalysis(postId, {
    tags: parseTags(snap.ai_tags_json),
    entities: parseTags(snap.ai_entities_json),
  });
  db.prepare('DELETE FROM web_snapshots WHERE id = ?').run(snapshotId);
  return true;
}

// Strip a web post back to a re-analyzable placeholder: keep its identity
// (url/domain/title) but clear every capture artefact + AI. Used when the LAST
// report of a site is deleted and there's no older snapshot to promote.
function clearWebPostToPlaceholder(postId: string): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare(
    `UPDATE posts SET
       thumbnail_path = NULL, image_path = NULL, video_path = NULL,
       thumbnail_url = NULL, text = NULL,
       web_palette_json = NULL, web_fonts_json = NULL, web_tech_json = NULL,
       web_awards_json = NULL, web_pages_json = NULL, web_meta_json = NULL,
       web_captured_at = NULL,
       ai_description = NULL, ai_tags = NULL, ai_status = NULL, ai_model = NULL,
       ai_analyzed_at = NULL, ai_category = NULL, ai_content_type = NULL,
       ai_entities = NULL, ai_keywords = NULL, ai_language = NULL, ai_save_reason = NULL
     WHERE id = ?`,
  ).run(postId);
  db.prepare('DELETE FROM post_media WHERE post_id = ?').run(postId);
  // Strip only the AI-owned tag rows; the user's manual tags (tier='manual')
  // are a separate layer (user_tags JSON is left untouched here) and must
  // survive the reset to a re-analyzable placeholder — mirror applyAiAnalysis.
  db.prepare(
    "DELETE FROM post_tags WHERE post_id = ? AND (tier IS NULL OR tier IN ('general','specific'))",
  ).run(postId);
  db.prepare('DELETE FROM post_entities WHERE post_id = ?').run(postId);
}

// "Delete only the report" for a site: drop the CURRENT capture and promote the
// most recent archived snapshot to current; if none exists, clear the site back
// to a placeholder. Returns the removed current capture's file paths (to unlink)
// and whether an older version was promoted.
function deleteLatestReport(postId: string): { removedPaths: string[]; promoted: boolean } {
  if (!db) throw new Error('Database not initialized');
  // Current capture only: the older snapshots are promoted/kept here, so we must
  // NOT unlink their screenshots (getLocalFilePaths would now include them).
  const removedPaths = getCurrentCaptureFilePaths(postId);
  const prev = db
    .prepare(
      'SELECT id FROM web_snapshots WHERE post_id = ? ORDER BY captured_at DESC, id DESC LIMIT 1',
    )
    .get(postId) as { id: number } | undefined;
  const tx = db.transaction(() => {
    if (prev) promoteSnapshotToPost(postId, prev.id);
    else clearWebPostToPlaceholder(postId);
  });
  tx();
  invalidateGlobalCaches();
  return { removedPaths, promoted: !!prev };
}

// Clears all local-file path columns for a post (does not delete files from disk).
function clearPostLocalFiles(postId: string): void {
  if (!db) throw new Error('Database not initialized');
  db.prepare(
    'UPDATE posts SET thumbnail_path = NULL, image_path = NULL, video_path = NULL WHERE id = ?',
  ).run(postId);
  db.prepare('UPDATE post_media SET local_path = NULL WHERE post_id = ?').run(postId);
}

export {
  initialize,
  close,
  getPosts,
  getPostIds,
  getPostsByIds,
  getPost,
  existingIds,
  savedByKeys,
  getPostsForAnalysis,
  upsertPost,
  bulkUpsert,
  getStats,
  updatePaths,
  updateMediaPath,
  listPostsMissingThumbBlur,
  setThumbBlur,
  extractContentTerms,
  updateAiAnalysis,
  updateUserContent,
  addManualBookmark,
  clearAiDescriptions,
  clearAiTags,
  getFrequentTags,
  importFromJSON,
  exportAllPosts,
  getCollectionsForExport,
  clearAllData,
  clearAllAiAnalysis,
  clearAllAssetPaths,
  getLocalFilePaths,
  getCurrentCaptureFilePaths,
  clearPostLocalFiles,
  getCollections,
  createCollection,
  updateCollection,
  deleteCollection,
  addPostsToCollections,
  removePostFromCollection,
  deletePosts,
  jobUpsert,
  jobsUpsertMany,
  jobDelete,
  jobsDeleteMany,
  jobDeleteAll,
  jobsByKind,
  clearStuckAnalyzing,
  getAiOverview,
  getTagStats,
  getEntityStats,
  getTagCooccurrence,
  searchTagsByText,
  getTopTagsForTextQuery,
  getTagDistinctivenessForTextQuery,
  getKeywordsForTextQuery,
  resolveAlias,
  getUnaliasedTags,
  getCanonicalVocab,
  saveTagAliases,
  getTagAliases,
  setAliasStatus,
  getTagClusters,
  getTagCandidateGroups,
  getTagMergeSuggestions,
  getTagHealth,
  saveClusterRun,
  setClusterStatus,
  renameCluster,
  removeTagFromCluster,
  buildTagCommunities,
  getTagGraph,
  renameTag,
  mergeTags,
  getPostIdsByTags,
  searchPostsByTags,
  searchPostsHybrid,
  // Web references (sites as platform='web').
  upsertWebReference,
  upsertWebReferences,
  createWebPlaceholder,
  webRefToPost,
  webPostId,
  normalizeWebUrl,
  // Web snapshots (dated version history).
  getWebSnapshots,
  getWebSnapshotCounts,
  deleteWebSnapshot,
  deleteLatestReport,
  getWebSiteFilePaths,
  invalidateGlobalCaches,
};

// Test-only: lets the suite verify migrate()'s idempotent backfill of the
// derived tables without re-opening the singleton DB.
export const __backfillDerivedTags = (): void => backfillDerivedTags(db!);
export const __termIdfWeights = termIdfWeights;
