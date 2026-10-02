//! DDL of desktop libraries, for tests that build synthetic legacy files.
//!
//! Not used by the reader itself. Execute the SQL on a writable connection of
//! your own; the reader only ever opens files read-only.

/// A library created by the current desktop app: the statements of `SCHEMA`
/// in `electron/db.ts` (comments removed), then the DDL `migrate()` runs on a
/// fresh file (`SCHEMA_VERSION = 3`).
pub const DESKTOP_SCHEMA_CURRENT: &str = r#"
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
  preview_path TEXT,
  image_path TEXT,
  video_path TEXT,
  media_count INTEGER DEFAULT 1,
  imported_at INTEGER DEFAULT (unixepoch())
);
CREATE INDEX IF NOT EXISTS idx_posts_platform ON posts(platform);
CREATE INDEX IF NOT EXISTS idx_posts_media_type ON posts(media_type);
CREATE INDEX IF NOT EXISTS idx_posts_timestamp ON posts(timestamp DESC);
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
CREATE TABLE IF NOT EXISTS jobs (
  kind       TEXT NOT NULL,
  key        TEXT NOT NULL,
  post_id    TEXT,
  payload    TEXT,
  status     TEXT NOT NULL,
  progress   REAL DEFAULT 0,
  error      TEXT,
  attempts   INTEGER DEFAULT 0,
  created_at INTEGER DEFAULT (unixepoch()),
  updated_at INTEGER DEFAULT (unixepoch()),
  PRIMARY KEY (kind, key)
);
CREATE INDEX IF NOT EXISTS idx_jobs_kind_status ON jobs(kind, status);
CREATE TABLE IF NOT EXISTS collections (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  color TEXT NOT NULL DEFAULT '#3d5afe',
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
CREATE TABLE IF NOT EXISTS post_tags (
  post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  tag_norm TEXT NOT NULL,
  tag_form TEXT NOT NULL,
  PRIMARY KEY (post_id, tag_norm)
);
CREATE INDEX IF NOT EXISTS idx_post_tags_norm ON post_tags(tag_norm);
CREATE TABLE IF NOT EXISTS tag_alias (
  alias_norm     TEXT PRIMARY KEY,
  canonical_norm TEXT NOT NULL,
  canonical_form TEXT NOT NULL,
  status         TEXT NOT NULL DEFAULT 'accepted'
);
CREATE TABLE IF NOT EXISTS post_entities (
  post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  ent_norm TEXT NOT NULL,
  ent_form TEXT NOT NULL,
  PRIMARY KEY (post_id, ent_norm)
);
CREATE INDEX IF NOT EXISTS idx_post_entities_norm ON post_entities(ent_norm);
CREATE TABLE IF NOT EXISTS tag_cluster (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  label TEXT NOT NULL,
  label_norm TEXT,
  status TEXT NOT NULL DEFAULT 'proposed',
  run_id INTEGER,
  created_at INTEGER DEFAULT (unixepoch()),
  updated_at INTEGER DEFAULT (unixepoch())
);
CREATE TABLE IF NOT EXISTS tag_cluster_membership (
  tag_norm TEXT PRIMARY KEY,
  cluster_id INTEGER NOT NULL REFERENCES tag_cluster(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_tag_cluster_membership_cluster ON tag_cluster_membership(cluster_id);

ALTER TABLE posts ADD COLUMN ai_description TEXT;
ALTER TABLE posts ADD COLUMN ai_tags TEXT;
ALTER TABLE posts ADD COLUMN ai_status TEXT;
ALTER TABLE posts ADD COLUMN ai_model TEXT;
ALTER TABLE posts ADD COLUMN ai_analyzed_at INTEGER;
ALTER TABLE posts ADD COLUMN ai_category TEXT;
ALTER TABLE posts ADD COLUMN ai_content_type TEXT;
ALTER TABLE posts ADD COLUMN ai_entities TEXT;
ALTER TABLE posts ADD COLUMN ai_keywords TEXT;
ALTER TABLE posts ADD COLUMN ai_language TEXT;
ALTER TABLE posts ADD COLUMN ai_save_reason TEXT;
CREATE INDEX IF NOT EXISTS idx_posts_ai_status ON posts(ai_status);
ALTER TABLE posts ADD COLUMN user_note TEXT;
ALTER TABLE posts ADD COLUMN user_tags TEXT;
ALTER TABLE posts ADD COLUMN web_url TEXT;
ALTER TABLE posts ADD COLUMN web_domain TEXT;
ALTER TABLE posts ADD COLUMN web_final_url TEXT;
ALTER TABLE posts ADD COLUMN web_palette_json TEXT;
ALTER TABLE posts ADD COLUMN web_fonts_json TEXT;
ALTER TABLE posts ADD COLUMN web_tech_json TEXT;
ALTER TABLE posts ADD COLUMN web_awards_json TEXT;
ALTER TABLE posts ADD COLUMN web_pages_json TEXT;
ALTER TABLE posts ADD COLUMN web_meta_json TEXT;
ALTER TABLE posts ADD COLUMN web_captured_at INTEGER;
ALTER TABLE posts ADD COLUMN ai_web_json TEXT;
CREATE INDEX IF NOT EXISTS idx_posts_web_domain ON posts(web_domain);
CREATE TABLE IF NOT EXISTS post_facets (
  post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  facet   TEXT NOT NULL,
  value   TEXT NOT NULL,
  PRIMARY KEY (post_id, facet, value)
);
CREATE INDEX IF NOT EXISTS idx_post_facets_value ON post_facets(facet, value);
ALTER TABLE posts ADD COLUMN thumb_blur TEXT;
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
);
CREATE INDEX IF NOT EXISTS idx_web_snapshots_post ON web_snapshots(post_id, captured_at DESC);
ALTER TABLE web_snapshots ADD COLUMN ai_web_json TEXT;
ALTER TABLE post_tags ADD COLUMN tier TEXT;
PRAGMA user_version = 3;
"#;

/// An early library, before every `migrate()` addition: `posts` with its
/// base columns only, `post_media` without later tables, no data repairs
/// (`user_version` 0).
pub const DESKTOP_SCHEMA_EARLY: &str = r#"
CREATE TABLE posts (
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
  imported_at INTEGER DEFAULT (unixepoch())
);
CREATE TABLE collections (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  color TEXT NOT NULL DEFAULT '#3d5afe',
  created_at INTEGER DEFAULT (unixepoch())
);
CREATE TABLE post_collections (
  post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
  added_at INTEGER DEFAULT (unixepoch()),
  PRIMARY KEY (post_id, collection_id)
);
CREATE TABLE post_tags (
  post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  tag_norm TEXT NOT NULL,
  tag_form TEXT NOT NULL,
  PRIMARY KEY (post_id, tag_norm)
);
CREATE TABLE tag_alias (
  alias_norm     TEXT PRIMARY KEY,
  canonical_norm TEXT NOT NULL,
  canonical_form TEXT NOT NULL
);
CREATE TABLE downloads (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  post_id TEXT REFERENCES posts(id) ON DELETE CASCADE,
  asset_type TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  progress REAL DEFAULT 0,
  error TEXT,
  started_at INTEGER,
  completed_at INTEGER
);
"#;
