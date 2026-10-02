//! `shelfy-migrate plan` on synthetic desktop libraries.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{Connection, params};
use shelfy_core::ids::web::legacy_post_id;
use shelfy_core::legacy::LegacyDb;
use shelfy_core::legacy::fixture::{DESKTOP_SCHEMA_CURRENT, DESKTOP_SCHEMA_EARLY};
use shelfy_migrate::plan::{PlanOptions, plan};
use shelfy_migrate::report::PlanReport;

/// pk 3191575067010950169 is shortcode `CxKwJ0fLmQZ`.
const PK: &str = "3191575067010950169";
const SHORTCODE: &str = "CxKwJ0fLmQZ";

struct Library {
    dir: tempfile::TempDir,
    db: PathBuf,
}

impl Library {
    fn new(schema: &str) -> Library {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("shelfy.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(schema).unwrap();
        Library { dir, db }
    }

    /// Runs `sql` with foreign keys off, so tests can plant orphan rows.
    fn exec(&self, sql: &str) -> &Self {
        let conn = Connection::open(&self.db).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute_batch(sql).unwrap();
        self
    }

    fn post(
        &self,
        id: &str,
        platform: &str,
        shortcode: Option<&str>,
        thumbnail_path: Option<&str>,
    ) -> &Self {
        Connection::open(&self.db)
            .unwrap()
            .execute(
                "INSERT INTO posts (id, platform, shortcode, timestamp, imported_at, thumbnail_path, thumbnail_url)
                 VALUES (?1, ?2, ?3, '2024-01-01T00:00:00.000Z', 1700000000, ?4, 'https://cdn.example/x.jpg?oe=5F5E1000')",
                params![id, platform, shortcode, thumbnail_path],
            )
            .unwrap();
        self
    }

    fn web(&self, url: &str, note: Option<&str>) -> &Self {
        Connection::open(&self.db)
            .unwrap()
            .execute(
                "INSERT INTO posts (id, platform, media_type, web_url, web_final_url, post_url, imported_at, user_note,
                   web_pages_json, web_captured_at)
                 VALUES (?1, 'web', 'website', ?2, ?2, ?2, 1700000000, ?3, '[]', 1700000000)",
                params![legacy_post_id(url), url, note],
            )
            .unwrap();
        self
    }

    /// Closes the WAL cleanly, as the desktop does on quit.
    fn close(&self) -> &Self {
        let conn = Connection::open(&self.db).unwrap();
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .unwrap();
        self
    }

    fn media_root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn plan(&self, media_root: Option<PathBuf>, redact: bool) -> PlanReport {
        self.close();
        let db = LegacyDb::open(&self.db).unwrap();
        plan(
            &db,
            &PlanOptions {
                media_root,
                redact,
                now_ms: 1_700_000_000_000,
            },
        )
        .unwrap()
    }
}

fn outcome(report: &PlanReport, table: &str, outcome: &str) -> u64 {
    let t = report.tables.iter().find(|t| t.table == table).unwrap();
    t.outcomes.get(outcome).copied().unwrap_or(0)
}

#[test]
fn an_empty_current_library_passes() {
    let lib = Library::new(DESKTOP_SCHEMA_CURRENT);
    let report = lib.plan(None, false);
    assert!(report.verdict.pass, "{:?}", report.errors);
    assert_eq!(report.coverage.columns, 121);
    assert_eq!(report.coverage.present, 121);
    assert!(report.coverage.unmapped.is_empty());
    assert!(report.tables.iter().all(|t| t.accounted));
    assert!(report.dry_run);
}

#[test]
fn instagram_forms_of_one_post_merge_into_one_key() {
    let lib = Library::new(DESKTOP_SCHEMA_CURRENT);
    lib.post(&format!("{PK}_25025320"), "instagram", Some(SHORTCODE), None)
        .post(PK, "instagram", Some(SHORTCODE), Some("/home/u/Shelfy/assets/thumbnails/instagram-x.jpg"))
        .post(SHORTCODE, "instagram", Some(SHORTCODE), None)
        .post("1700000000000000001", "twitter", None, None)
        .exec(&format!(
            "UPDATE posts SET user_note = 'first' WHERE id = '{PK}_25025320';
             UPDATE posts SET user_note = 'second' WHERE id = '{SHORTCODE}';
             INSERT INTO post_media VALUES ('{PK}_25025320', 0, 'image', 'https://cdn/a.jpg', NULL);
             INSERT INTO post_media VALUES ('{PK}', 0, 'image', 'https://cdn/a.jpg', NULL);
             INSERT INTO post_media VALUES ('missing', 0, 'image', NULL, NULL);
             INSERT INTO collections (id, name, platform, external_id) VALUES (1, 'A', 'instagram', '99'), (2, 'A again', 'instagram', '99'), (3, 'Mine', NULL, NULL);
             INSERT INTO post_collections VALUES ('{PK}_25025320', 1, 1), ('{PK}', 2, 1), ('{SHORTCODE}', 3, 1);
             INSERT INTO post_tags VALUES ('{PK}', 'cat', 'Cat', 'general'), ('{SHORTCODE}', 'cat', 'Cat', NULL), ('{SHORTCODE}', 'mine', 'Mine', 'manual');"
        ));
    let report = lib.plan(None, false);

    assert_eq!(report.duplicates.instagram.groups, 1);
    assert_eq!(report.duplicates.instagram.rows_merged, 2);
    assert_eq!(report.duplicates.instagram.notes_to_concatenate, 1);
    let group = &report.duplicates.instagram.listed[0];
    assert_eq!(group.key.as_deref(), Some(format!("ig_{PK}").as_str()));
    let kept: Vec<_> = group.members.iter().filter(|m| m.kept).collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0].legacy_id.as_deref(),
        Some(PK),
        "the row with archived files is kept"
    );

    assert_eq!(outcome(&report, "posts", "insert"), 2);
    assert_eq!(outcome(&report, "posts", "merge"), 2);
    assert_eq!(outcome(&report, "post_media", "insert"), 1);
    assert_eq!(outcome(&report, "post_media", "merge"), 1);
    assert_eq!(outcome(&report, "post_media", "orphan"), 1);
    assert_eq!(report.duplicates.collections.groups, 1);
    assert_eq!(outcome(&report, "collections", "merge"), 1);
    // Memberships of the merged posts and collections collapse.
    assert_eq!(outcome(&report, "post_collections", "insert"), 2);
    assert_eq!(outcome(&report, "post_collections", "merge"), 1);
    // AI and manual tags with the same name coexist; two AI rows collapse.
    assert_eq!(outcome(&report, "post_tags", "insert"), 2);
    assert_eq!(outcome(&report, "post_tags", "merge"), 1);

    let ig = &report.identity.by_platform["instagram"];
    assert_eq!(ig.rows, 3);
    assert_eq!(ig.distinct_keys, 1);
    assert_eq!(ig.sources["composite"], 1);
    assert_eq!(ig.sources["pk"], 1);
    assert_eq!(ig.sources["shortcode"], 1);
    assert_eq!(report.identity.ig_shortcode_check.consistent, 2);

    assert!(report.tables.iter().all(|t| t.accounted));
    assert!(report.verdict.pass, "{:?}", report.errors);
    assert!(report.warnings.iter().any(|w| w.contains("missing parent")));
}

#[test]
fn http_and_https_twins_merge() {
    let lib = Library::new(DESKTOP_SCHEMA_CURRENT);
    lib.web("https://www.example.com/work/", Some("a"))
        .web("http://example.com/work", Some("b"))
        .web("https://example.org", None);
    let report = lib.plan(None, false);
    assert_eq!(report.duplicates.web.groups, 1);
    assert_eq!(report.duplicates.web.rows_in_groups, 2);
    assert_eq!(report.duplicates.web.notes_to_concatenate, 1);
    assert_eq!(report.identity.web_legacy_id_check["web_url"], 3);
    assert_eq!(report.web.sites, 3);
    assert_eq!(report.web.placeholders, 3);
    assert_eq!(outcome(&report, "posts", "insert"), 2);
    assert!(report.verdict.pass);
}

#[test]
fn redaction_hides_ids_and_paths() {
    let lib = Library::new(DESKTOP_SCHEMA_CURRENT);
    lib.post(&format!("{PK}_1"), "instagram", Some(SHORTCODE), None)
        .post(PK, "instagram", Some(SHORTCODE), None);
    let report = lib.plan(Some(lib.media_root()), true);
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains(PK));
    assert!(!json.contains(&lib.media_root().display().to_string()));
    assert_eq!(report.duplicates.instagram.listed[0].key, None);
}

#[test]
fn files_are_checked_under_the_media_root() {
    let lib = Library::new(DESKTOP_SCHEMA_CURRENT);
    let root = lib.media_root();
    fs::create_dir_all(root.join("assets/thumbnails")).unwrap();
    fs::create_dir_all(root.join("assets/videos")).unwrap();
    fs::create_dir_all(root.join("assets/web")).unwrap();
    fs::write(root.join("assets/thumbnails/instagram-a.jpg"), b"cover").unwrap();
    fs::write(root.join("assets/videos/instagram-a.mp4"), b"video bytes").unwrap();
    fs::write(root.join("assets/web/hero.webp"), b"hero").unwrap();
    fs::write(root.join("assets/web/leftover.webp"), b"old").unwrap();
    let old = "/Users/someone/Library/Application Support/Shelfy/assets";
    lib.post("1_2", "instagram", None, Some(&format!("{old}/thumbnails/instagram-a.jpg")))
        .post("3_4", "instagram", None, Some(&format!("{old}/thumbnails/gone.jpg")))
        .exec(&format!(
            "UPDATE posts SET video_path = '{old}/videos/instagram-a.mp4' WHERE id = '1_2';
             INSERT INTO posts (id, platform, media_type, web_url, imported_at, thumbnail_path, web_pages_json, web_meta_json, ai_web_json)
               VALUES ('web:x', 'web', 'website', 'https://example.com', 1700000000, '{old}/web/hero.webp',
                 '[{{\"url\":\"https://example.com\",\"screenshotPath\":\"{old}/web/hero.webp\",\"hero\":{{\"path\":\"{old}/web/hero.webp\"}},\"chunks\":[{{\"screenshotPath\":\"{old}/web/band.webp\"}}]}}]',
                 '{{\"favicon\":\"{old}/web/fav.webp\",\"ogImage\":\"https://example.com/og.png\"}}',
                 '{{\"facets\":{{\"style\":[\"Minimal\"]}}}}');
             INSERT INTO post_media VALUES ('web:x', 0, 'image', 'https://example.com', '{old}/web/hero.webp');
             INSERT INTO post_facets VALUES ('web:x', 'style', 'Minimal');"
        ));
    let report = lib.plan(Some(root.clone()), false);
    let files = &report.files;
    assert!(files.checked && files.legacy_root_detected);
    assert_eq!(files.classes["cover"].present, 2);
    assert_eq!(files.classes["cover"].missing, 1);
    assert_eq!(files.classes["video"].present, 1);
    assert_eq!(files.classes["web_band"].missing, 1);
    assert_eq!(files.classes["web_favicon"].missing, 1);
    assert_eq!(files.totals.files, 6);
    assert_eq!(files.totals.present, 3);
    assert_eq!(files.upload.files_videos, 1);
    assert_eq!(files.upload.bytes_videos, 11);
    assert_eq!(files.upload.files_default, 2);
    assert_eq!(files.orphans.files, 1);
    assert_eq!(files.orphans.by_dir["web"], (1, 3));
    assert_eq!(files.covers.with_local_cover, 2);
    assert_eq!(files.covers.without_local_cover["instagram"], 1);
    // oe=5F5E1000 is 2020-09-13, before "now" (2023-11-14).
    assert_eq!(files.covers.ig_without_cover_url["expired"], 1);
    assert_eq!(report.web.captured, 1);
    assert_eq!(report.web.assets_by_role["hero"], 1);
    assert_eq!(report.web.facets.derivable_rows, 1);
    assert_eq!(outcome(&report, "post_facets", "rebuilt"), 1);
    assert_eq!(report.posts.slides_by_kind["page"], 1);
    assert!(report.verdict.pass, "{:?}", report.errors);
    assert!(report.warnings.iter().any(|w| w.contains("missing")));
}

#[test]
fn unmapped_columns_and_unknown_platforms_fail() {
    let lib = Library::new(DESKTOP_SCHEMA_CURRENT);
    lib.exec(
        "ALTER TABLE posts ADD COLUMN favorite INTEGER;
         CREATE TABLE extra (x TEXT); INSERT INTO extra VALUES ('a');
         INSERT INTO posts (id, platform) VALUES ('t1', 'tiktok');
         INSERT INTO post_facets VALUES ('t1', 'style', 'Not derivable');",
    );
    let report = lib.plan(None, false);
    assert!(!report.verdict.pass);
    assert!(!report.verdict.no_unmapped_column);
    assert!(!report.verdict.every_row_accounted);
    assert_eq!(
        report.coverage.unmapped,
        ["posts.favorite", "extra.x", "extra (table)"]
    );
    assert_eq!(outcome(&report, "posts", "unmappable"), 1);
    assert_eq!(outcome(&report, "extra", "unmapped"), 1);
    assert_eq!(report.web.facets.not_derivable_rows, 1);
    assert!(report.errors.iter().any(|e| e.contains("post_facets")));
}

#[test]
fn an_early_library_maps_with_defaults() {
    let lib = Library::new(DESKTOP_SCHEMA_EARLY);
    lib.exec(&format!(
        "INSERT INTO posts (id, platform, shortcode, timestamp, thumbnail_url) VALUES ('{SHORTCODE}', 'instagram', '{SHORTCODE}', '', 'https://cdn/x.jpg');
         INSERT INTO posts (id, platform, post_url, timestamp) VALUES ('42', 'twitter', 'https://x.com//status/42', NULL);
         INSERT INTO tag_alias VALUES ('a', 'b', 'B');"
    ));
    let report = lib.plan(None, false);
    assert!(report.verdict.pass, "{:?}", report.errors);
    assert_eq!(report.source.user_version, 0);
    assert_eq!(report.source.repairs_pending.len(), 3);
    assert_eq!(report.coverage.missing_required.len(), 0);
    assert_eq!(report.coverage.absent_optional.len(), 27 + 3 + 1 + 1);
    assert_eq!(report.posts.posted_at.empty, 1);
    assert_eq!(report.posts.posted_at.null, 1);
    assert_eq!(report.posts.posted_at.undated_ig_datable_from_shortcode, 1);
    assert_eq!(report.posts.x_status_urls_to_repair, 1);
    assert_eq!(report.posts.without_slides_backfillable, 1);
    assert_eq!(report.tags.alias_status["accepted"], 1);
    assert_eq!(
        report.identity.by_platform["instagram"].sources["shortcode"],
        1
    );
}

// ── the binary ─────────────────────────────────────────────────────────────

fn migrate(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_shelfy-migrate"))
        .args(args)
        .output()
        .expect("failed to run shelfy-migrate")
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_cli_prints_text_and_json_and_writes_nothing() {
    let lib = Library::new(DESKTOP_SCHEMA_CURRENT);
    lib.post(&format!("{PK}_1"), "instagram", Some(SHORTCODE), None)
        .close();
    let db = lib.db.to_str().unwrap();
    let before = fs::read(&lib.db).unwrap();
    let files_before = listing(lib.dir.path());

    let text = migrate(&["plan", "--db", db]);
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    let stdout = String::from_utf8(text.stdout).unwrap();
    assert!(stdout.contains("dry run"));
    assert!(stdout.contains("PASS"));

    let json = migrate(&["plan", "--db", db, "--json", "--redact"]);
    assert!(json.status.success());
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["verdict"]["pass"], true);
    assert_eq!(value["source"]["open_mode"], "immutable");
    assert_eq!(value["identity"]["by_platform"]["instagram"]["rows"], 1);

    assert_eq!(
        fs::read(&lib.db).unwrap(),
        before,
        "the library is untouched"
    );
    assert_eq!(listing(lib.dir.path()), files_before, "no side files");
}

#[test]
fn the_cli_exit_codes() {
    let lib = Library::new(DESKTOP_SCHEMA_CURRENT);
    lib.exec("ALTER TABLE posts ADD COLUMN favorite INTEGER;")
        .close();
    let failed = migrate(&["plan", "--db", lib.db.to_str().unwrap()]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stdout).contains("FAIL"));

    let missing = lib.dir.path().join("nope.sqlite");
    let unreadable = migrate(&["plan", "--db", missing.to_str().unwrap()]);
    assert_eq!(unreadable.status.code(), Some(3));
    assert!(!missing.exists());

    let usage = migrate(&["plan"]);
    assert_eq!(usage.status.code(), Some(2));
}

#[test]
fn the_cli_prints_the_mapping() {
    let output = migrate(&["mapping"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("| `posts.thumb_blur` | added | dropped |"));
    let json = migrate(&["mapping", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 13);
}
