//! Reader tests on synthetic desktop files.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::catalog::{self, Disposition};
use super::fixture::{DESKTOP_SCHEMA_CURRENT, DESKTOP_SCHEMA_EARLY};
use super::*;

/// Creates a desktop file at `dir/shelfy.sqlite` in WAL mode, as the app
/// does, runs `sql` on it and closes it cleanly.
fn create_library(dir: &Path, schema: &str, sql: &str) -> PathBuf {
    let path = dir.join("shelfy.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch(schema).unwrap();
    conn.execute_batch(sql).unwrap();
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
        .unwrap();
    drop(conn);
    path
}

/// One row per table, every column set to a distinct value.
const FULL_ROWS: &str = r#"
INSERT INTO posts (id, platform, shortcode, post_url, profile_url, author_username, author_name,
  text, thumbnail_url, media_type, timestamp, thumbnail_path, preview_path, image_path, video_path,
  media_count, imported_at, ai_description, ai_tags, ai_status, ai_model, ai_analyzed_at,
  ai_category, ai_content_type, ai_entities, ai_keywords, ai_language, ai_save_reason, ai_web_json,
  user_note, user_tags, web_url, web_domain, web_final_url, web_palette_json, web_fonts_json,
  web_tech_json, web_awards_json, web_pages_json, web_meta_json, web_captured_at, thumb_blur)
VALUES ('p1', 'instagram', 'sc', 'purl', 'prof', 'user', 'name', 'text', 'turl', 'image',
  '2024-01-01T00:00:00.000Z', '/t', '/pv', '/i', '/v', 3, 1700000000, 'desc', '["a"]', 'done',
  'model', 1700000001, 'cat', 'ctype', '["e"]', '["k"]', 'en', 'why', '{"schema":2}', 'note',
  '["u"]', 'wurl', 'wdom', 'wfinal', '[1]', '[2]', '[3]', '[4]', '[5]', '{"m":1}', 1700000002,
  'data:blur');
INSERT INTO post_media VALUES ('p1', 0, 'image', 'src', '/local');
INSERT INTO collections (id, name, color, platform, external_id, ig_name, created_at)
  VALUES (7, 'Saved', '#fff', 'instagram', '123', 'IG name', 1700000003);
INSERT INTO post_collections VALUES ('p1', 7, 1700000004);
INSERT INTO post_tags (post_id, tag_norm, tag_form, tier) VALUES ('p1', 'tag', 'Tag', 'general');
INSERT INTO post_entities VALUES ('p1', 'ent', 'Ent');
INSERT INTO post_facets VALUES ('p1', 'style', 'Minimal');
INSERT INTO tag_alias VALUES ('alias', 'canon', 'Canon', 'proposed');
INSERT INTO tag_cluster VALUES (9, 'Label', 'label', 'accepted', 1700000000000, 1700000005, 1700000006);
INSERT INTO tag_cluster_membership VALUES ('tag', 9);
INSERT INTO web_snapshots (id, post_id, captured_at, title, web_pages_json, web_palette_json,
  web_fonts_json, web_tech_json, web_awards_json, web_meta_json, ai_description, ai_tags_json,
  ai_model, ai_status, ai_analyzed_at, ai_category, ai_content_type, ai_entities_json,
  ai_keywords_json, ai_language, ai_save_reason, created_at, ai_web_json)
VALUES (4, 'p1', 1600000000, 'T', 'pg', 'pa', 'fo', 'te', 'aw', 'me', 'ad', 'at', 'am', 'as',
  1600000001, 'ac', 'act', 'ae', 'ak', 'al', 'asr', 1600000002, 'aweb');
INSERT INTO jobs VALUES ('download', 'p1:image', 'p1', '{}', 'done', 1.0, NULL, 2, 1700000007, 1700000008);
INSERT INTO downloads VALUES (1, 'p1', 'image', 'pending', 0.5, 'err', 1, 2);
"#;

#[test]
fn reads_every_column_of_a_current_library() {
    let dir = tempfile::tempdir().unwrap();
    let path = create_library(dir.path(), DESKTOP_SCHEMA_CURRENT, FULL_ROWS);
    let db = LegacyDb::open(&path).unwrap();
    assert!(db.is_read_only());
    assert_eq!(db.schema().user_version, 3);
    assert_eq!(db.schema().journal_mode, "wal");

    let posts: Vec<PostRow> = db.read_all().unwrap();
    assert_eq!(
        posts,
        [PostRow {
            id: "p1".into(),
            platform: "instagram".into(),
            shortcode: Some("sc".into()),
            post_url: Some("purl".into()),
            profile_url: Some("prof".into()),
            author_username: Some("user".into()),
            author_name: Some("name".into()),
            text: Some("text".into()),
            thumbnail_url: Some("turl".into()),
            media_type: Some("image".into()),
            timestamp: Some("2024-01-01T00:00:00.000Z".into()),
            thumbnail_path: Some("/t".into()),
            preview_path: Some("/pv".into()),
            image_path: Some("/i".into()),
            video_path: Some("/v".into()),
            media_count: Some(3),
            imported_at: Some(1_700_000_000),
            ai_description: Some("desc".into()),
            ai_tags: Some(r#"["a"]"#.into()),
            ai_status: Some("done".into()),
            ai_model: Some("model".into()),
            ai_analyzed_at: Some(1_700_000_001),
            ai_category: Some("cat".into()),
            ai_content_type: Some("ctype".into()),
            ai_entities: Some(r#"["e"]"#.into()),
            ai_keywords: Some(r#"["k"]"#.into()),
            ai_language: Some("en".into()),
            ai_save_reason: Some("why".into()),
            ai_web_json: Some(r#"{"schema":2}"#.into()),
            user_note: Some("note".into()),
            user_tags: Some(r#"["u"]"#.into()),
            web_url: Some("wurl".into()),
            web_domain: Some("wdom".into()),
            web_final_url: Some("wfinal".into()),
            web_palette_json: Some("[1]".into()),
            web_fonts_json: Some("[2]".into()),
            web_tech_json: Some("[3]".into()),
            web_awards_json: Some("[4]".into()),
            web_pages_json: Some("[5]".into()),
            web_meta_json: Some(r#"{"m":1}"#.into()),
            web_captured_at: Some(1_700_000_002),
            thumb_blur: Some("data:blur".into()),
        }]
    );
    assert_eq!(posts[0].platform(), Some(crate::ids::Platform::Instagram));

    assert_eq!(
        db.read_all::<PostMediaRow>().unwrap(),
        [PostMediaRow {
            post_id: "p1".into(),
            position: 0,
            media_type: "image".into(),
            source_url: Some("src".into()),
            local_path: Some("/local".into()),
        }]
    );
    assert_eq!(
        db.read_all::<CollectionRow>().unwrap(),
        [CollectionRow {
            id: 7,
            name: "Saved".into(),
            color: "#fff".into(),
            created_at: Some(1_700_000_003),
            platform: Some("instagram".into()),
            external_id: Some("123".into()),
            ig_name: Some("IG name".into()),
        }]
    );
    assert_eq!(
        db.read_all::<PostCollectionRow>().unwrap(),
        [PostCollectionRow {
            post_id: "p1".into(),
            collection_id: 7,
            added_at: Some(1_700_000_004),
        }]
    );
    assert_eq!(
        db.read_all::<PostTagRow>().unwrap(),
        [PostTagRow {
            post_id: "p1".into(),
            tag_norm: "tag".into(),
            tag_form: "Tag".into(),
            tier: Some("general".into()),
        }]
    );
    assert_eq!(
        db.read_all::<PostEntityRow>().unwrap(),
        [PostEntityRow {
            post_id: "p1".into(),
            ent_norm: "ent".into(),
            ent_form: "Ent".into(),
        }]
    );
    assert_eq!(
        db.read_all::<PostFacetRow>().unwrap(),
        [PostFacetRow {
            post_id: "p1".into(),
            facet: "style".into(),
            value: "Minimal".into(),
        }]
    );
    assert_eq!(
        db.read_all::<TagAliasRow>().unwrap(),
        [TagAliasRow {
            alias_norm: "alias".into(),
            canonical_norm: "canon".into(),
            canonical_form: "Canon".into(),
            status: "proposed".into(),
        }]
    );
    assert_eq!(
        db.read_all::<TagClusterRow>().unwrap(),
        [TagClusterRow {
            id: 9,
            label: "Label".into(),
            label_norm: Some("label".into()),
            status: "accepted".into(),
            run_id: Some(1_700_000_000_000),
            created_at: Some(1_700_000_005),
            updated_at: Some(1_700_000_006),
        }]
    );
    assert_eq!(
        db.read_all::<TagClusterMembershipRow>().unwrap(),
        [TagClusterMembershipRow {
            tag_norm: "tag".into(),
            cluster_id: 9,
        }]
    );
    assert_eq!(
        db.read_all::<WebSnapshotRow>().unwrap(),
        [WebSnapshotRow {
            id: 4,
            post_id: "p1".into(),
            captured_at: Some(1_600_000_000),
            title: Some("T".into()),
            web_pages_json: Some("pg".into()),
            web_palette_json: Some("pa".into()),
            web_fonts_json: Some("fo".into()),
            web_tech_json: Some("te".into()),
            web_awards_json: Some("aw".into()),
            web_meta_json: Some("me".into()),
            ai_description: Some("ad".into()),
            ai_tags_json: Some("at".into()),
            ai_model: Some("am".into()),
            ai_status: Some("as".into()),
            ai_analyzed_at: Some(1_600_000_001),
            ai_category: Some("ac".into()),
            ai_content_type: Some("act".into()),
            ai_entities_json: Some("ae".into()),
            ai_keywords_json: Some("ak".into()),
            ai_language: Some("al".into()),
            ai_save_reason: Some("asr".into()),
            created_at: Some(1_600_000_002),
            ai_web_json: Some("aweb".into()),
        }]
    );
    assert_eq!(
        db.read_all::<JobRow>().unwrap(),
        [JobRow {
            kind: "download".into(),
            key: "p1:image".into(),
            post_id: Some("p1".into()),
            payload: Some("{}".into()),
            status: "done".into(),
            progress: Some(1.0),
            error: None,
            attempts: Some(2),
            created_at: Some(1_700_000_007),
            updated_at: Some(1_700_000_008),
        }]
    );
    assert_eq!(
        db.read_all::<DownloadRow>().unwrap(),
        [DownloadRow {
            id: 1,
            post_id: Some("p1".into()),
            asset_type: "image".into(),
            status: "pending".into(),
            progress: Some(0.5),
            error: Some("err".into()),
            started_at: Some(1),
            completed_at: Some(2),
        }]
    );
}

#[test]
fn a_current_library_is_fully_covered() {
    let dir = tempfile::tempdir().unwrap();
    let path = create_library(dir.path(), DESKTOP_SCHEMA_CURRENT, "");
    let db = LegacyDb::open(&path).unwrap();
    let coverage = db.schema().coverage();
    assert_eq!(coverage.unmapped_columns().count(), 0);
    assert_eq!(coverage.unmapped_tables().count(), 0);
    assert!(coverage.missing_required().is_empty());
    assert_eq!(coverage.count(ColumnStatus::Present), 121);
    assert_eq!(coverage.count(ColumnStatus::AbsentOptional), 0);
    assert!(coverage.other_objects.is_empty());
    // The fresh file has no AUTOINCREMENT rows yet, so no sqlite_sequence.
    assert!(
        coverage
            .tables
            .iter()
            .all(|t| t.status == TableStatus::Known || t.status == TableStatus::SqliteInternal)
    );
}

#[test]
fn an_early_library_reads_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = create_library(
        dir.path(),
        DESKTOP_SCHEMA_EARLY,
        "INSERT INTO posts (id, platform, timestamp, imported_at) VALUES ('1_2', 'instagram', '', 1600000000);
         INSERT INTO tag_alias VALUES ('a', 'b', 'B');
         INSERT INTO collections (name) VALUES ('Mine');",
    );
    let db = LegacyDb::open(&path).unwrap();
    assert_eq!(db.schema().user_version, 0);

    let posts: Vec<PostRow> = db.read_all().unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].media_count, Some(1), "the ALTER default");
    assert_eq!(posts[0].ai_status, None);
    assert_eq!(posts[0].preview_path, None);
    assert_eq!(posts[0].timestamp.as_deref(), Some(""));

    let aliases: Vec<TagAliasRow> = db.read_all().unwrap();
    assert_eq!(aliases[0].status, "accepted", "the ALTER default");
    let collections: Vec<CollectionRow> = db.read_all().unwrap();
    assert_eq!(collections[0].platform, None);
    assert_eq!(collections[0].color, "#3d5afe");

    // Tables the early file lacks read as empty.
    assert!(db.read_all::<PostMediaRow>().unwrap().is_empty());
    assert!(db.read_all::<WebSnapshotRow>().unwrap().is_empty());
    assert_eq!(db.row_count("web_snapshots").unwrap(), 0);

    let coverage = db.schema().coverage();
    assert_eq!(coverage.unmapped_columns().count(), 0);
    assert!(coverage.missing_required().is_empty());
    let absent: Vec<_> = coverage
        .tables
        .iter()
        .filter(|t| t.status == TableStatus::AbsentOptional)
        .map(|t| t.table.as_str())
        .collect();
    assert_eq!(
        absent,
        [
            "post_media",
            "post_entities",
            "post_facets",
            "tag_cluster",
            "tag_cluster_membership",
            "web_snapshots",
            "jobs"
        ]
    );
    // posts: 42 catalog columns, 15 in the early file.
    let posts_absent = coverage
        .columns
        .iter()
        .filter(|c| c.table == "posts" && c.status == ColumnStatus::AbsentOptional)
        .count();
    assert_eq!(posts_absent, 42 - 15);
}

#[test]
fn unknown_columns_and_tables_are_unmapped() {
    let dir = tempfile::tempdir().unwrap();
    let path = create_library(
        dir.path(),
        DESKTOP_SCHEMA_CURRENT,
        "ALTER TABLE posts ADD COLUMN favorite INTEGER;
         CREATE TABLE extra (x TEXT);
         CREATE VIEW v AS SELECT id FROM posts;",
    );
    let db = LegacyDb::open(&path).unwrap();
    let coverage = db.schema().coverage();
    let unmapped: Vec<_> = coverage
        .unmapped_columns()
        .map(|c| format!("{}.{}", c.table, c.column))
        .collect();
    assert_eq!(unmapped, ["posts.favorite", "extra.x"]);
    assert_eq!(coverage.unmapped_tables().count(), 1);
    assert_eq!(coverage.other_objects.len(), 1);
    // The typed reader ignores the extra column.
    assert!(db.read_all::<PostRow>().unwrap().is_empty());
}

#[test]
fn values_are_read_leniently() {
    let dir = tempfile::tempdir().unwrap();
    let path = create_library(
        dir.path(),
        DESKTOP_SCHEMA_CURRENT,
        "INSERT INTO posts (id, platform, media_count, imported_at, text, web_captured_at)
           VALUES (42, 'twitter', 2.0, '1700000000', 12.5, 'soon');",
    );
    let db = LegacyDb::open(&path).unwrap();
    let posts: Vec<PostRow> = db.read_all().unwrap();
    assert_eq!(posts[0].id, "42");
    assert_eq!(posts[0].media_count, Some(2));
    assert_eq!(posts[0].imported_at, Some(1_700_000_000));
    assert_eq!(posts[0].text.as_deref(), Some("12.5"));
    assert_eq!(posts[0].web_captured_at, None);
    let classes = db.storage_classes("posts").unwrap();
    assert_eq!(classes["web_captured_at"].get("text"), Some(&1));
    assert_eq!(classes["id"].get("text"), Some(&1), "TEXT affinity");
    assert_eq!(classes["ai_status"].get("null"), Some(&1));
    assert!(db.storage_classes("nope").unwrap().is_empty());
}

#[test]
fn opening_never_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = create_library(dir.path(), DESKTOP_SCHEMA_CURRENT, FULL_ROWS);
    let before = fs::read(&path).unwrap();
    let files_before = dir_listing(dir.path());
    {
        let db = LegacyDb::open(&path).unwrap();
        assert_eq!(db.open_mode(), OpenMode::Immutable);
        assert!(db.is_read_only());
        for table in catalog::TABLES {
            db.row_count(table.name).unwrap();
        }
        db.read_all::<PostRow>().unwrap();
        db.read_all::<WebSnapshotRow>().unwrap();
    }
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(dir_listing(dir.path()), files_before);
}

#[test]
fn missing_files_and_non_libraries_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.sqlite");
    assert!(matches!(
        LegacyDb::open(&missing),
        Err(LegacyError::Open(_))
    ));
    assert!(!missing.exists(), "never created");

    let garbage = dir.path().join("garbage.sqlite");
    fs::write(
        &garbage,
        b"this is not a database at all, just some bytes......",
    )
    .unwrap();
    assert!(matches!(
        LegacyDb::open(&garbage),
        Err(LegacyError::NotALibrary(_))
    ));

    let other = dir.path().join("other.sqlite");
    Connection::open(&other)
        .unwrap()
        .execute_batch("CREATE TABLE t (x)")
        .unwrap();
    assert!(matches!(
        LegacyDb::open(&other),
        Err(LegacyError::NotALibrary(_))
    ));
}

#[test]
fn the_snapshot_is_stable_while_the_desktop_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = create_library(
        dir.path(),
        DESKTOP_SCHEMA_CURRENT,
        "INSERT INTO posts (id, platform) VALUES ('a', 'twitter');",
    );
    let writer = Connection::open(&path).unwrap();
    writer
        .query_row("SELECT COUNT(*) FROM posts", [], |r| r.get::<_, i64>(0))
        .unwrap();
    let db = LegacyDb::open(&path).unwrap();
    assert_eq!(
        db.open_mode(),
        OpenMode::SharedReadOnly,
        "the WAL is in use"
    );
    assert!(db.is_read_only());
    assert_eq!(db.row_count("posts").unwrap(), 1);
    writer
        .execute(
            "INSERT INTO posts (id, platform) VALUES ('b', 'twitter')",
            [],
        )
        .unwrap();
    assert_eq!(db.row_count("posts").unwrap(), 1, "same snapshot");
    assert_eq!(db.read_all::<PostRow>().unwrap().len(), 1);
    drop(db);
    assert_eq!(
        LegacyDb::open(&path).unwrap().row_count("posts").unwrap(),
        2
    );
}

#[test]
fn paths_with_uri_characters_open_immutable() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("Application Support #1 ?%20 é");
    fs::create_dir(&nested).unwrap();
    let path = create_library(
        &nested,
        DESKTOP_SCHEMA_CURRENT,
        "INSERT INTO posts (id, platform) VALUES ('a', 'twitter');",
    );
    let db = LegacyDb::open(&path).unwrap();
    assert_eq!(db.open_mode(), OpenMode::Immutable);
    assert_eq!(db.row_count("posts").unwrap(), 1);
    drop(db);
    assert_eq!(dir_listing(&nested).len(), 1, "no -wal/-shm created");
}

#[test]
fn every_catalog_table_streams_from_a_current_library() {
    // Guards the column order of each record against the catalog.
    let dir = tempfile::tempdir().unwrap();
    let path = create_library(dir.path(), DESKTOP_SCHEMA_CURRENT, FULL_ROWS);
    let db = LegacyDb::open(&path).unwrap();
    for spec in catalog::TABLES {
        let expected = db.row_count(spec.name).unwrap();
        assert_eq!(expected, 1, "{}", spec.name);
        if let Disposition::Dropped { reason } = spec.disposition {
            assert!(!reason.is_empty());
        }
    }
}

fn dir_listing(dir: &Path) -> Vec<(String, u64)> {
    let mut out: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                e.metadata().unwrap().len(),
            )
        })
        .collect();
    out.sort();
    out
}
