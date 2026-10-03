//! Schema v1 (plan §2.6, §2.7) and the committed schema fixtures.
//!
//! `tests/fixtures/schema/<kind>-v<N>.sql` freezes schema version N with a small
//! synthetic dataset in every table. The tests check that the migrations still
//! produce exactly that schema and that every fixture upgrades cleanly to the
//! latest version.
//!
//! When a migration is added, write the fixture of the new version with
//! `SHELFY_BLESS=1 cargo test -p shelfy-core --test schema` and commit it; the
//! older fixtures never change.

mod support;

use rusqlite::Connection;
use shelfy_core::db::{ControlDb, ControlDbConfig, DbError, UserDb, UserDbConfig};
use shelfy_core::schema::{self, Kind};
use shelfy_core::search::index;
use support::{blessing, dump, fixture_control, fixture_library, fixture_path, load};

const LIBRARY_TABLES: &[&str] = &[
    "ai_cache",
    "collections",
    "media_objects",
    "meta",
    "notifications",
    "post_collections",
    "post_entities",
    "post_media",
    "post_tags",
    "posts",
    "posts_fts",
    "settings",
    "sync_runs",
    "sync_sources",
    "tag_alias",
    "tag_cluster",
    "tag_cluster_membership",
    "tag_embeddings",
    "web_capture_assets",
    "web_captures",
];

const LIBRARY_INDEXES: &[&str] = &[
    "post_collections_c",
    "post_entities_norm",
    "post_media_object",
    "post_media_pending",
    "post_media_video_object",
    "post_tags_norm",
    "posts_ai",
    "posts_cover_object",
    "posts_current_capture",
    "posts_domain",
    "posts_platform",
    "posts_shortcode",
    "posts_sort",
    "posts_trash",
    "tag_cluster_membership_cluster",
    "web_capture_assets_object",
    "web_captures_post",
];

const CONTROL_TABLES: &[&str] = &[
    "api_tokens",
    "audit_log",
    "feature_flags",
    "idempotency",
    "invites",
    "jobs",
    "magic_links",
    "pairing_codes",
    "passkeys",
    "provider_keys",
    "queue_state",
    "sessions",
    "uploads",
    "usage_daily",
    "users",
];

const CONTROL_INDEXES: &[&str] = &["jobs_active_dedupe", "jobs_ready", "jobs_user"];

fn migrated(kind: Kind, version: usize) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    schema::migrate_to(&mut conn, kind, version).unwrap();
    conn
}

/// User tables and explicit indexes (no FTS shadow tables, no auto-indexes).
fn names(conn: &Connection, ty: &str) -> Vec<String> {
    conn.prepare(
        "SELECT name FROM sqlite_schema WHERE type = ?1 AND name NOT LIKE 'sqlite_%'
           AND sql IS NOT NULL
           AND name NOT IN (SELECT name FROM pragma_table_list WHERE type = 'shadow')
         ORDER BY name",
    )
    .unwrap()
    .query_map([ty], |r| r.get(0))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

/// The full schema, for comparing two databases.
fn schema_rows(conn: &Connection) -> Vec<(String, String, String, Option<String>)> {
    conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn pragma_i64(conn: &Connection, name: &str) -> i64 {
    conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
        .unwrap()
}

fn fixture_name(kind: Kind, version: usize) -> String {
    format!("{}-v{version}.sql", kind.name())
}

#[test]
fn library_schema_has_every_table_and_index() {
    let conn = migrated(Kind::Library, 1);
    assert_eq!(names(&conn, "table"), LIBRARY_TABLES);
    assert_eq!(names(&conn, "index"), LIBRARY_INDEXES);
    assert_eq!(
        pragma_i64(&conn, "application_id"),
        i64::from(schema::LIBRARY_APPLICATION_ID)
    );
    assert_eq!(schema::version(&conn).unwrap(), 1);
}

#[test]
fn control_schema_has_every_table_and_index() {
    let conn = migrated(Kind::Control, 1);
    assert_eq!(names(&conn, "table"), CONTROL_TABLES);
    assert_eq!(names(&conn, "index"), CONTROL_INDEXES);
    assert_eq!(
        pragma_i64(&conn, "application_id"),
        i64::from(schema::CONTROL_APPLICATION_ID)
    );
}

#[test]
fn library_token_kind_upgrade_preserves_existing_credentials() {
    for version in [4, 5] {
        let mut conn = migrated(Kind::Control, version);
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        // Historical rows must not use the latest-version fixture builder:
        // both pre-export v4 and export v5 must upgrade losslessly.
        conn.execute_batch(
            "INSERT INTO users (id, email, role, quota_bytes, created_at) \
         VALUES ('existing-user', 'existing@example.test', 'member', 0, 0); \
         INSERT INTO api_tokens \
           (id, user_id, kind, token_hash, label, scopes, created_at, last_used_at, \
            revoked_at, expires_at, install_hash) \
         VALUES \
           ('extension-token', 'existing-user', 'extension', X'aa01', 'Chrome', \
            'ingest tasks uploads lookup', 100, 110, 120, NULL, X'bb01'), \
           ('migrate-token', 'existing-user', 'migrate', X'aa02', 'Migration', \
            'migrate', 100, 110, NULL, 1000, NULL), \
           ('shortcut-token', 'existing-user', 'shortcut', X'aa03', 'Phone', \
            'links:create', 100, NULL, NULL, NULL, NULL);",
        )
        .unwrap();
        let tokens = |conn: &Connection| {
            conn.prepare(
                "SELECT id, user_id, kind, token_hash, label, scopes, created_at, last_used_at, \
             revoked_at, expires_at, install_hash FROM api_tokens ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                (0..row.as_ref().column_count())
                    .map(|i| row.get::<_, rusqlite::types::Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
        };
        if version == 5 {
            conn.execute_batch("INSERT INTO exports (id,user_id,job_id,created_at,expires_at,estimated_bytes,bytes,deleted_at) VALUES ('existing-export','existing-user',99,100,1000,4096,2048,NULL)").unwrap();
        }
        let exports = |conn: &Connection| {
            conn.prepare("SELECT id,user_id,job_id,created_at,expires_at,estimated_bytes,bytes,deleted_at FROM exports ORDER BY id")
            .unwrap().query_map([], |row| {
                (0..row.as_ref().column_count()).map(|i| row.get::<_, rusqlite::types::Value>(i)).collect::<rusqlite::Result<Vec<_>>>()
            }).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        let exports_before = (version == 5).then(|| exports(&conn));
        let before = tokens(&conn);
        schema::migrate(&mut conn, Kind::Control).unwrap();
        assert_eq!(
            tokens(&conn),
            before,
            "hashes, scopes, expiry and install kept"
        );
        assert_eq!(schema::version(&conn).unwrap(), 6);
        if let Some(before) = exports_before {
            assert_eq!(exports(&conn), before, "export metadata kept from v5 to v6");
        }
        assert!(names(&conn, "index").contains(&"api_tokens_install".to_owned()));
        conn.execute(
            "INSERT INTO users (id, email, role, quota_bytes, created_at) \
         VALUES ('library-user', 'library@example.test', 'member', 0, 0)",
            [],
        )
        .unwrap();
        let insert = "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
                  VALUES ('library-token', 'library-user', ?1, X'ffff', 'library:read', 0)";
        assert!(
            conn.execute(insert, ["unknown"]).is_err(),
            "kind CHECK remains"
        );
        conn.execute(insert, ["library"]).unwrap();
        conn.execute("DELETE FROM users WHERE id = 'library-user'", [])
            .unwrap();
        assert_eq!(
            tokens(&conn),
            before,
            "user deletion still cascades to tokens"
        );
    }
}

#[test]
fn post_tags_key_includes_the_source() {
    let conn = migrated(Kind::Library, 1);
    let pk: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('post_tags') WHERE pk > 0 ORDER BY pk")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(pk, ["post_id", "tag_norm", "source"]);
}

#[test]
fn fts_table_is_contentless_with_the_plan_options() {
    let conn = migrated(Kind::Library, 1);
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name = 'posts_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    for option in [
        "content=''",
        "contentless_delete=1",
        "tokenize=\"unicode61 remove_diacritics 2\"",
        "prefix='2 3'",
    ] {
        assert!(sql.contains(option), "{option} missing from {sql}");
    }
    // Rows can be deleted by rowid (contentless_delete), and deleting a missing
    // rowid is a no-op.
    conn.execute(
        "INSERT INTO posts_fts (rowid, caption) VALUES (7, 'Città di vetro')",
        [],
    )
    .unwrap();
    let hit: i64 = conn
        .query_row(
            "SELECT rowid FROM posts_fts WHERE posts_fts MATCH 'citta'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hit, 7);
    conn.execute("DELETE FROM posts_fts WHERE rowid = 7", [])
        .unwrap();
    conn.execute("DELETE FROM posts_fts WHERE rowid = 8", [])
        .unwrap();
    let left: i64 = conn
        .query_row(
            "SELECT count(*) FROM posts_fts WHERE posts_fts MATCH 'citta'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn fixtures_exist_for_every_version() {
    for kind in [Kind::Library, Kind::Control] {
        for version in 1..=kind.latest_version() {
            let path = fixture_path(&fixture_name(kind, version));
            assert!(
                path.exists() || blessing(),
                "missing {}: run SHELFY_BLESS=1 cargo test -p shelfy-core --test schema",
                path.display()
            );
        }
    }
}

#[test]
fn bless_latest_fixtures() {
    // Writes the fixtures of the latest versions when SHELFY_BLESS=1; otherwise
    // only checks the dataset builders still work on the latest schema.
    for kind in [Kind::Library, Kind::Control] {
        let version = kind.latest_version();
        let conn = migrated(kind, version);
        match kind {
            Kind::Library => fixture_library(&conn),
            Kind::Control => fixture_control(&conn),
        }
        let title = format!(
            "Shelfy {}.sqlite at schema v{version}, with synthetic data in every table.",
            kind.name()
        );
        let sql = dump(&conn, kind, &title);
        if blessing() {
            let path = fixture_path(&fixture_name(kind, version));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, sql).unwrap();
        }
    }
}

#[test]
fn every_fixture_matches_its_migration() {
    for kind in [Kind::Library, Kind::Control] {
        for version in 1..=kind.latest_version() {
            let Ok(sql) = std::fs::read_to_string(fixture_path(&fixture_name(kind, version)))
            else {
                continue; // reported by fixtures_exist_for_every_version
            };
            let fixture = load(&sql);
            let fresh = migrated(kind, version);
            assert_eq!(
                schema_rows(&fixture),
                schema_rows(&fresh),
                "{} v{version}: the migrations no longer produce the committed schema \
                 (released migrations must not change)",
                kind.name()
            );
            assert_eq!(schema::version(&fixture).unwrap(), version);
            assert_eq!(
                pragma_i64(&fixture, "application_id"),
                i64::from(kind.application_id())
            );
        }
    }
}

#[test]
fn every_fixture_upgrades_to_the_latest_version() {
    for kind in [Kind::Library, Kind::Control] {
        for version in 1..=kind.latest_version() {
            let Ok(sql) = std::fs::read_to_string(fixture_path(&fixture_name(kind, version)))
            else {
                continue;
            };
            let mut conn = load(&sql);
            let tables = names(&conn, "table");
            let counts_before: Vec<i64> = tables.iter().map(|t| count(&conn, t)).collect();
            schema::migrate(&mut conn, kind).unwrap();
            assert_eq!(schema::version(&conn).unwrap(), kind.latest_version());
            let integrity: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(integrity, "ok");
            let fk_violations: i64 = conn
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(fk_violations, 0);
            for (table, before) in tables.iter().zip(counts_before) {
                if !["posts_fts", "posts_infix"].contains(&table.as_str()) {
                    assert_eq!(count(&conn, table), before, "{table} lost rows");
                }
            }
            if kind == Kind::Library {
                for (table, term) in [("posts_fts", "lampada"), ("posts_infix", "\"soffiat\"")] {
                    let hits: i64 = conn
                        .query_row(
                            &format!("SELECT count(*) FROM {table} WHERE {table} MATCH ?1"),
                            [term],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert!(hits > 0, "the fixture's {table} answers queries");
                }
                // The migrations fill the indexes exactly as the code
                // maintains them.
                assert_eq!(index::verify(&conn).unwrap(), Vec::<i64>::new());
            }
        }
    }
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
        r.get(0)
    })
    .unwrap()
}

#[test]
fn opening_refuses_foreign_files() {
    let dir = tempfile::tempdir().unwrap();

    let other = dir.path().join("other.sqlite");
    Connection::open(&other)
        .unwrap()
        .execute_batch("CREATE TABLE notes (body TEXT);")
        .unwrap();
    let err = UserDb::open(&other, &UserDbConfig::default())
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            DbError::WrongApplication {
                expected: "library",
                found: 0
            }
        ),
        "{err}"
    );

    let control = dir.path().join("control.sqlite");
    drop(ControlDb::open(&control, &ControlDbConfig::default()).unwrap());
    let err = UserDb::open(&control, &UserDbConfig::default())
        .err()
        .unwrap();
    assert!(matches!(err, DbError::WrongApplication { .. }), "{err}");
}

/// Inserts a passkey of the fixture owner; returns the id it got.
fn insert_passkey(conn: &Connection, cred: &[u8]) -> i64 {
    conn.execute(
        "INSERT INTO passkeys (user_id, cred_id, passkey_json, created_at) \
         VALUES ('01J9Z3B8K4QW6TFX0V7G2N5RCA', ?1, '{}', 1)",
        [cred],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn add_owner(conn: &Connection) {
    conn.execute(
        "INSERT INTO users (id, email, role, quota_bytes, created_at) \
         VALUES ('01J9Z3B8K4QW6TFX0V7G2N5RCA', 'owner@example.test', 'owner', 0, 1)",
        [],
    )
    .unwrap();
}

#[test]
fn passkey_ids_are_never_reused() {
    // F5: before v3, deleting the newest passkey gave its id to the next one,
    // so audit rows that name a passkey by id became ambiguous.
    let mut conn = migrated(Kind::Control, 2);
    add_owner(&conn);
    conn.execute_batch(
        "INSERT INTO audit_log (at, action, meta_json) VALUES (1, 'passkey.create', '{\"id\":4}');
         INSERT INTO audit_log (at, action, meta_json) VALUES (1, 'passkey.delete', '{\"id\":4}');
         INSERT INTO audit_log (at, action, meta_json) VALUES (1, 'passkey.delete', 'not json');
         INSERT INTO audit_log (at, action, meta_json) VALUES (1, 'invite.create', '{\"id\":99}');",
    )
    .unwrap();
    for cred in [b"c1", b"c2", b"c3"] {
        insert_passkey(&conn, cred);
    }
    let delete = |conn: &Connection, id: i64| {
        conn.execute("DELETE FROM passkeys WHERE id = ?1", [id])
            .unwrap();
    };
    delete(&conn, 3);
    assert_eq!(
        insert_passkey(&conn, b"c9"),
        3,
        "v2 reuses the freed id: the bug"
    );
    delete(&conn, 3);

    schema::migrate(&mut conn, Kind::Control).unwrap();
    let kept: Vec<(i64, Vec<u8>)> = conn
        .prepare("SELECT id, cred_id FROM passkeys ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(kept, [(1, b"c1".to_vec()), (2, b"c2".to_vec())]);
    // Above every id the table or a passkey audit row has used, freed or not.
    assert_eq!(insert_passkey(&conn, b"c4"), 5);
    delete(&conn, 5);
    assert_eq!(insert_passkey(&conn, b"c5"), 6, "a freed id stays freed");

    // A fresh database starts at 1.
    let fresh = migrated(Kind::Control, Kind::Control.latest_version());
    add_owner(&fresh);
    assert_eq!(insert_passkey(&fresh, b"c1"), 1);
}
