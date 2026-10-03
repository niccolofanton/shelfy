#!/usr/bin/env node
// scripts/live/subset-library.fixture.mjs — a small, synthetic desktop
// library for testing subset-library.mjs without ever touching the owner's
// own library (the card's "test it on a synthetic fixture database, never
// on the owner's library").
//
// The schema is `crates/core/src/legacy/fixture.rs`'s `DESKTOP_SCHEMA_CURRENT`
// (the exact desktop `SCHEMA` + `migrate()` DDL, `SCHEMA_VERSION = 3`),
// copied here because this tool is plain Node and cannot `use` the Rust
// crate; the counts below are written by hand so a test can assert on them
// directly. Every string in it is made up: no real caption, URL, username
// or path appears anywhere in this file.
//
// Usage:
//   node subset-library.fixture.mjs <path>     write the fixture to <path>
//
// Or import `buildFixture(path)` to do the same from another script (used
// by this tool's own ad hoc verification runs, never by subset-library.mjs
// itself).

import { rmSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';

// Verbatim from crates/core/src/legacy/fixture.rs::DESKTOP_SCHEMA_CURRENT.
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
`;

const NOW = 1_790_899_200; // 2026-10-02T00:00:00Z, unix seconds (desktop timestamps)
const DAY = 86_400;

function isoDaysAgo(days) {
  return new Date((NOW - days * DAY) * 1000).toISOString();
}

/** Deterministic counts a test can assert on directly. */
export const FIXTURE_SUMMARY = {
  posts: 24,
  byPlatform: { instagram: 10, twitter: 6, pinterest: 3, web: 3, manual: 2 },
  collectionsTotal: 5,
  collectionsWithMembers: 4, // "Empty folder" starts with none
};

/** Writes the fixture to `dbPath` (replaced if it exists, `-wal`/`-shm`
 * siblings too) and returns [`FIXTURE_SUMMARY`]. */
export function buildFixture(dbPath) {
  for (const suffix of ['', '-wal', '-shm', '-journal']) {
    rmSync(dbPath + suffix, { force: true });
  }
  const db = new DatabaseSync(dbPath);
  try {
    db.exec(SCHEMA);
    db.exec('PRAGMA foreign_keys = ON');

    const insertPost = db.prepare(`
      INSERT INTO posts (
        id, platform, shortcode, post_url, profile_url, author_username, author_name, text,
        thumbnail_url, media_type, timestamp, thumbnail_path, image_path, video_path,
        media_count, imported_at, web_url, web_domain, web_final_url, ai_web_json
      ) VALUES (
        :id, :platform, :shortcode, :post_url, :profile_url, :author_username, :author_name,
        :text, :thumbnail_url, :media_type, :timestamp, :thumbnail_path, :image_path,
        :video_path, :media_count, :imported_at, :web_url, :web_domain, :web_final_url,
        :ai_web_json
      )
    `);
    let nextImportOffset = 0;
    const post = (fields) =>
      insertPost.run({
        shortcode: null,
        post_url: null,
        profile_url: null,
        author_username: null,
        author_name: null,
        text: null,
        thumbnail_url: null,
        thumbnail_path: null,
        image_path: null,
        video_path: null,
        media_count: 1,
        imported_at: NOW - (nextImportOffset += 1) * 3600, // an hour apart, oldest import first
        web_url: null,
        web_domain: null,
        web_final_url: null,
        ai_web_json: null,
        ...fields,
      });

    // Instagram: 10 posts, two carousels with slides, one in the folder
    // five times over (ig_1..ig_5), five outside it (ig_6..ig_10).
    for (let i = 1; i <= 10; i += 1) {
      post({
        id: `ig_${i}`,
        platform: 'instagram',
        shortcode: `FixtureShort${i}`,
        post_url: `https://www.instagram.com/p/FixtureShort${i}/`,
        profile_url: 'https://www.instagram.com/fixture.studio/',
        author_username: 'fixture.studio',
        author_name: 'Fixture Studio',
        text: `synthetic instagram fixture post ${i}`,
        media_type: i <= 3 ? 'carousel' : i <= 7 ? 'video' : 'image',
        timestamp: isoDaysAgo(i),
        thumbnail_path: `/fixture/assets/images/ig-${i}-cover.jpg`,
        image_path: `/fixture/assets/images/ig-${i}-0.jpg`,
        media_count: i <= 3 ? 3 : 1,
      });
      if (i <= 3) {
        for (let slide = 0; slide < 3; slide += 1) {
          db.prepare(
            'INSERT INTO post_media (post_id, position, media_type, source_url, local_path) ' +
              'VALUES (?, ?, ?, ?, ?)',
          ).run(
            `ig_${i}`,
            slide,
            'image',
            `https://cdn.fixture.test/ig-${i}-${slide}.jpg`,
            `/fixture/assets/images/ig-${i}-${slide}.jpg`,
          );
        }
      }
      if (i <= 5) {
        db.prepare(
          "INSERT INTO post_tags (post_id, tag_norm, tag_form, tier) VALUES (?, 'fixture', 'Fixture', 'manual')",
        ).run(`ig_${i}`);
      }
      if (i === 1) {
        db.prepare(
          "INSERT INTO post_entities (post_id, ent_norm, ent_form) VALUES ('ig_1', 'fixture studio', 'Fixture Studio')",
        ).run();
      }
    }

    // Twitter: 6 posts.
    for (let i = 1; i <= 6; i += 1) {
      post({
        id: `x_${i}`,
        platform: 'twitter',
        post_url: `https://x.com/fixture_user/status/${1000 + i}`,
        profile_url: 'https://x.com/fixture_user',
        author_username: 'fixture_user',
        text: `synthetic twitter fixture post ${i}`,
        media_type: i <= 2 ? 'video' : 'text',
        timestamp: isoDaysAgo(10 + i),
      });
    }

    // Pinterest: 3 posts, two in a board.
    for (let i = 1; i <= 3; i += 1) {
      post({
        id: `pin_${i}`,
        platform: 'pinterest',
        post_url: `https://www.pinterest.com/pin/${2000 + i}/`,
        text: `synthetic pinterest fixture pin ${i}`,
        media_type: 'image',
        timestamp: isoDaysAgo(20 + i),
        thumbnail_path: `/fixture/assets/images/pin-${i}.jpg`,
      });
      db.prepare('INSERT INTO downloads (post_id, asset_type, status) VALUES (?, ?, ?)').run(
        `pin_${i}`,
        'image',
        'completed',
      );
    }

    // Web: 3 posts, two with snapshot history and facets. post_facets is
    // rebuilt from ai_web_json.facets on migrate (not carried verbatim), so
    // the two must agree, or `shelfy-migrate plan` reports it as
    // "not derivable" — a fixture-authoring trap, not a subsetting one.
    for (let i = 1; i <= 3; i += 1) {
      const hasHistory = i <= 2;
      post({
        id: `web_${i}`,
        platform: 'web',
        media_type: 'website',
        text: `Fixture Studio ${i} — synthetic website reference`,
        timestamp: isoDaysAgo(30 + i),
        web_url: `https://fixture-studio-${i}.example.test/`,
        web_domain: `fixture-studio-${i}.example.test`,
        web_final_url: `https://fixture-studio-${i}.example.test/`,
        ai_web_json: hasHistory ? '{"facets":{"style":["minimal"]}}' : null,
      });
      if (hasHistory) {
        db.prepare(
          'INSERT INTO web_snapshots (post_id, captured_at, title, created_at) VALUES (?, ?, ?, ?)',
        ).run(`web_${i}`, NOW - (30 + i + 7) * DAY, `Fixture Studio ${i} (previous version)`, NOW);
        db.prepare(
          "INSERT INTO post_facets (post_id, facet, value) VALUES (?, 'style', 'minimal')",
        ).run(`web_${i}`);
      }
    }

    // Manual: 2 posts. The desktop's own manual id convention is
    // `manual:<uuid>` (crates/core/src/legacy/catalog.rs); anything else has
    // no canonical key and `shelfy-migrate plan` refuses it outright.
    for (let i = 1; i <= 2; i += 1) {
      post({
        id: `manual:fixture-upload-${i}`,
        platform: 'manual',
        media_type: 'file',
        text: `synthetic manual fixture upload ${i}`,
        timestamp: isoDaysAgo(40 + i),
      });
    }

    // Collections: one IG folder (5 members), one Pinterest board (2
    // members), one manual folder (1 member), one with members that will
    // all be dropped by a small --count (so it must disappear too), and one
    // that starts empty (so the "has no members" rule has a base case that
    // does not depend on the sample at all).
    const collection = (fields) =>
      db
        .prepare(
          'INSERT INTO collections (name, color, platform, external_id, ig_name) ' +
            'VALUES (:name, :color, :platform, :external_id, :ig_name)',
        )
        .run({ color: '#3d5afe', platform: null, external_id: null, ig_name: null, ...fields });
    const addMember = (postId, collectionId) =>
      db
        .prepare('INSERT INTO post_collections (post_id, collection_id) VALUES (?, ?)')
        .run(postId, collectionId);

    const igFolder = collection({
      name: 'Fixture IG Folder',
      platform: 'instagram',
      external_id: '170000000001',
      ig_name: 'fixture-folder',
    }).lastInsertRowid;
    for (let i = 1; i <= 5; i += 1) addMember(`ig_${i}`, igFolder);

    const board = collection({
      name: 'Fixture Pinterest Board',
      platform: 'pinterest',
    }).lastInsertRowid;
    addMember('pin_1', board);
    addMember('pin_2', board);

    const manualFolder = collection({ name: 'Fixture Manual Folder' }).lastInsertRowid;
    addMember('manual:fixture-upload-1', manualFolder);

    const edgeFolder = collection({ name: 'Fixture Edge Folder' }).lastInsertRowid;
    addMember('ig_10', edgeFolder);
    addMember('x_6', edgeFolder);

    collection({ name: 'Fixture Empty Folder' }); // no members, ever

    // Jobs: one tied to a post that stays however the sample falls (no FK:
    // must be matched by hand), one tied to no post (must survive any
    // sample), one tied to a post likely dropped by a small --count.
    db.prepare(
      "INSERT INTO jobs (kind, key, post_id, status) VALUES ('download', 'job-ig-1', 'ig_1', 'succeeded')",
    ).run();
    db.prepare(
      "INSERT INTO jobs (kind, key, post_id, status) VALUES ('web', 'job-no-post', NULL, 'queued')",
    ).run();
    db.prepare(
      "INSERT INTO jobs (kind, key, post_id, status) VALUES ('analyze', 'job-ig-10', 'ig_10', 'failed')",
    ).run();

    // Vocabulary: post-independent, kept wholesale regardless of the sample.
    db.prepare(
      "INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form) VALUES ('fixtures', 'fixture', 'Fixture')",
    ).run();
    const cluster = db
      .prepare(
        "INSERT INTO tag_cluster (label, label_norm, status) VALUES ('Fixture cluster', 'fixture cluster', 'accepted')",
      )
      .run().lastInsertRowid;
    db.prepare('INSERT INTO tag_cluster_membership (tag_norm, cluster_id) VALUES (?, ?)').run(
      'fixture',
      cluster,
    );

    db.exec('PRAGMA wal_checkpoint(TRUNCATE)');
  } finally {
    db.close();
  }
  return FIXTURE_SUMMARY;
}

// Runnable standalone: `node subset-library.fixture.mjs <path>`.
if (import.meta.url === `file://${process.argv[1]}`) {
  const target = process.argv[2];
  if (!target) {
    console.error('usage: subset-library.fixture.mjs <path>');
    process.exitCode = 2;
  } else {
    const summary = buildFixture(target);
    console.log(`wrote the synthetic fixture to ${target}: ${JSON.stringify(summary)}`);
  }
}
