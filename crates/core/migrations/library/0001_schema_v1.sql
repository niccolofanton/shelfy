-- library.sqlite schema v1: one database per user.
-- Spec: docs/web-port/IMPLEMENTATION-PLAN.md §2.7 (tables) and §2.14 (FTS5).
--
-- Migrations are append-only: once this file ships, a schema change is a new
-- numbered file. crates/core/tests/fixtures/schema/library-v1.sql freezes this
-- version and the schema tests fail if the two drift apart.
--
-- Timestamps are unix milliseconds. FTS rows are written by the core in the same
-- transaction as the change (crates/core/src/search/index.rs), never by triggers.

-- "SHLB": marks the file as a Shelfy library, so an installer can reject other files.
PRAGMA application_id = 1397247042;

CREATE TABLE posts (
  id INTEGER PRIMARY KEY,                    -- internal rowid: joins + FTS rowid
  key TEXT NOT NULL UNIQUE,                  -- public id, see §2.8
  platform TEXT NOT NULL CHECK (platform IN ('instagram','twitter','pinterest','web','manual')),
  native_id TEXT NOT NULL,
  shortcode TEXT, post_url TEXT, profile_url TEXT, author_username TEXT, author_name TEXT,
  caption TEXT,                              -- ≤ 20 000 chars
  media_type TEXT NOT NULL,                  -- image|images|carousel|video|text|website|file
  media_count INTEGER NOT NULL DEFAULT 1,
  posted_at INTEGER,                         -- ms; NULL when unknown
  imported_at INTEGER NOT NULL,
  sort_ts INTEGER NOT NULL,                  -- COALESCE(posted_at, imported_at)
  cover_object INTEGER REFERENCES media_objects(id),
  cover_url TEXT, cover_url_expires_at INTEGER,
  thumbhash BLOB,                            -- ≤ 25 bytes
  archive_state TEXT NOT NULL DEFAULT 'pending'
    CHECK (archive_state IN ('pending','partial','done','failed','client','link_only')),
  ai_status TEXT, ai_attempts INTEGER NOT NULL DEFAULT 0, ai_next_at INTEGER, ai_error TEXT,
  ai_provider TEXT, ai_model TEXT, ai_schema_version INTEGER,
  ai_description TEXT, ai_save_reason TEXT, ai_language TEXT, ai_category TEXT, ai_content_type TEXT,
  ai_tags_json TEXT, ai_entities_json TEXT, ai_keywords_json TEXT, ai_web_json TEXT, ai_analyzed_at INTEGER,
  user_note TEXT, user_tags_json TEXT,
  web_url TEXT, web_domain TEXT, web_final_url TEXT,
  current_capture_id INTEGER REFERENCES web_captures(id) ON DELETE SET NULL,
  updated_at INTEGER NOT NULL, deleted_at INTEGER,
  UNIQUE (platform, native_id)
);
CREATE INDEX posts_sort      ON posts(sort_ts DESC, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX posts_platform  ON posts(platform, sort_ts DESC, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX posts_shortcode ON posts(shortcode) WHERE shortcode IS NOT NULL;
CREATE INDEX posts_ai        ON posts(ai_status, ai_next_at) WHERE ai_status IS NOT NULL;
CREATE INDEX posts_domain    ON posts(web_domain) WHERE web_domain IS NOT NULL;
CREATE INDEX posts_trash     ON posts(deleted_at) WHERE deleted_at IS NOT NULL;
-- Not in §2.7: indexes on foreign-key child columns, so deleting a media object
-- (GC) or a web capture checks its references without scanning the parent table.
CREATE INDEX posts_cover_object    ON posts(cover_object) WHERE cover_object IS NOT NULL;
CREATE INDEX posts_current_capture ON posts(current_capture_id) WHERE current_capture_id IS NOT NULL;

CREATE TABLE post_media (
  post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('image','video','file','page')),
  source_url TEXT, source_url_expires_at INTEGER,
  video_url TEXT, video_url_expires_at INTEGER,     -- direct MP4 kept by the parser
  width INTEGER, height INTEGER, duration_ms INTEGER, label TEXT,
  object_id INTEGER REFERENCES media_objects(id),   -- archived image / poster
  video_object_id INTEGER REFERENCES media_objects(id), -- kept ("offline") video
  fetch_attempts INTEGER NOT NULL DEFAULT 0, fetch_next_at INTEGER, fetch_error TEXT,
  PRIMARY KEY (post_id, position)
) WITHOUT ROWID;
CREATE INDEX post_media_pending ON post_media(fetch_next_at) WHERE object_id IS NULL AND kind IN ('image','video');
CREATE INDEX post_media_object       ON post_media(object_id) WHERE object_id IS NOT NULL;
CREATE INDEX post_media_video_object ON post_media(video_object_id) WHERE video_object_id IS NOT NULL;

CREATE TABLE media_objects (
  id INTEGER PRIMARY KEY, sha256 BLOB NOT NULL UNIQUE,
  ext TEXT NOT NULL, mime TEXT NOT NULL, bytes INTEGER NOT NULL,
  width INTEGER, height INTEGER, duration_ms INTEGER,
  role TEXT NOT NULL,          -- image|poster|video|file|preview|screenshot|band|section|footer|filmstrip|favicon|og
  variants INTEGER NOT NULL DEFAULT 0,   -- bitmask: 1 = g480
  origin TEXT NOT NULL,        -- server|extension|upload|capture|migration
  created_at INTEGER NOT NULL, unreferenced_since INTEGER
);

CREATE TABLE collections (id INTEGER PRIMARY KEY, name TEXT NOT NULL, color TEXT NOT NULL DEFAULT '#3d5afe',
  platform TEXT, external_id TEXT, source_name TEXT, position INTEGER, created_at INTEGER NOT NULL,
  UNIQUE (platform, external_id));
CREATE TABLE post_collections (post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE, added_at INTEGER NOT NULL,
  PRIMARY KEY (post_id, collection_id)) WITHOUT ROWID;
CREATE INDEX post_collections_c ON post_collections(collection_id, post_id);

CREATE TABLE post_tags (post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  tag_norm TEXT NOT NULL, tag_form TEXT NOT NULL,
  source TEXT NOT NULL CHECK (source IN ('ai','manual')),
  tier TEXT CHECK (tier IN ('general','specific')),
  PRIMARY KEY (post_id, tag_norm, source)) WITHOUT ROWID;
CREATE INDEX post_tags_norm ON post_tags(tag_norm, post_id);
CREATE TABLE post_entities (post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  ent_norm TEXT NOT NULL, ent_form TEXT NOT NULL, PRIMARY KEY (post_id, ent_norm)) WITHOUT ROWID;
CREATE INDEX post_entities_norm ON post_entities(ent_norm, post_id);
CREATE TABLE tag_alias (alias_norm TEXT PRIMARY KEY, canonical_norm TEXT NOT NULL, canonical_form TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('proposed','accepted')), created_at INTEGER NOT NULL);
CREATE TABLE tag_cluster (id INTEGER PRIMARY KEY, label TEXT NOT NULL, label_norm TEXT,
  status TEXT NOT NULL DEFAULT 'proposed', run_id INTEGER, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE tag_cluster_membership (tag_norm TEXT PRIMARY KEY,
  cluster_id INTEGER NOT NULL REFERENCES tag_cluster(id) ON DELETE CASCADE);
-- Not in §2.7 (the desktop has it): deleting a cluster finds its members by index.
CREATE INDEX tag_cluster_membership_cluster ON tag_cluster_membership(cluster_id);
CREATE TABLE tag_embeddings (tag_norm TEXT NOT NULL, model TEXT NOT NULL, dim INTEGER NOT NULL, vec BLOB NOT NULL,
  PRIMARY KEY (tag_norm, model)) WITHOUT ROWID;

CREATE TABLE web_captures (id INTEGER PRIMARY KEY,
  post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  captured_at INTEGER NOT NULL, requested_url TEXT, final_url TEXT,
  status TEXT NOT NULL, partial INTEGER NOT NULL DEFAULT 0, engine TEXT, viewport TEXT,
  title TEXT, palette_json TEXT, fonts_json TEXT, tech_json TEXT, awards_json TEXT,
  meta_json TEXT, pages_json TEXT, traits_json TEXT,   -- pages_json holds text + probes, never file paths
  hero_object INTEGER REFERENCES media_objects(id), favicon_object INTEGER REFERENCES media_objects(id),
  ai_snapshot_json TEXT,                               -- frozen AI layer of this version
  created_at INTEGER NOT NULL);
CREATE INDEX web_captures_post ON web_captures(post_id, captured_at DESC);
CREATE TABLE web_capture_assets (capture_id INTEGER NOT NULL REFERENCES web_captures(id) ON DELETE CASCADE,
  page_index INTEGER NOT NULL, role TEXT NOT NULL, seq INTEGER NOT NULL,
  object_id INTEGER NOT NULL REFERENCES media_objects(id), css_top INTEGER, css_height INTEGER,
  PRIMARY KEY (capture_id, page_index, role, seq)) WITHOUT ROWID;
-- Not in §2.7: foreign-key child index, as for post_media above.
CREATE INDEX web_capture_assets_object ON web_capture_assets(object_id);

-- Column order is fixed: bm25() weights are positional (§2.14).
CREATE VIRTUAL TABLE posts_fts USING fts5(
  tags, keywords, entities, description, note, caption, author, web_text,
  content='', contentless_delete=1,
  tokenize="unicode61 remove_diacritics 2", prefix='2 3');

CREATE TABLE settings (key TEXT PRIMARY KEY, value_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE notifications (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, code TEXT NOT NULL,
  params_json TEXT, target TEXT, created_at INTEGER NOT NULL, read_at INTEGER);
CREATE TABLE sync_runs (id TEXT PRIMARY KEY, platform TEXT NOT NULL, source_kind TEXT NOT NULL, source_key TEXT,
  started_at INTEGER NOT NULL, finished_at INTEGER, status TEXT NOT NULL,
  scanned INTEGER NOT NULL DEFAULT 0, inserted INTEGER NOT NULL DEFAULT 0, updated INTEGER NOT NULL DEFAULT 0,
  known INTEGER NOT NULL DEFAULT 0, error_code TEXT, client_version TEXT);
CREATE TABLE sync_sources (platform TEXT NOT NULL, source_key TEXT NOT NULL,
  collection_id INTEGER REFERENCES collections(id) ON DELETE SET NULL,
  last_run_at INTEGER, newest_native_id TEXT, PRIMARY KEY (platform, source_key));
CREATE TABLE ai_cache (kind TEXT NOT NULL, key_hash BLOB NOT NULL, value_json TEXT NOT NULL, created_at INTEGER NOT NULL,
  PRIMARY KEY (kind, key_hash)) WITHOUT ROWID;
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
