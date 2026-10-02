//! Schema upgrades on the paths the server takes (plan §3.8): every committed
//! schema fixture, written to disk as an older release left it, is upgraded
//! lazily when a library is first opened, by the sweep after boot, and, for
//! the control database, at boot. A database ahead of this build opens as is
//! unless its compat floor refuses this build (rollbacks).
//!
//! New fixtures (`tests/fixtures/schema/<kind>-v<N>.sql`) join these tests on
//! their own.

mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use shelfy_core::db::{
    ControlDb, ControlDbConfig, DbError, LibraryUpgrade, UserDb, UserDbCache, UserDbCacheConfig,
    UserDbConfig, lock_library,
};
use shelfy_core::schema::{self, Kind, Upgrade};
use support::fixture_path;

const USER: &str = "01J9Z3B8K4QW6TFX0V7G2N5RCA";

/// Writes the committed fixture of `kind` at `version` to a database file at
/// `path`, as an older release left it (rollback journal, no WAL yet).
/// Returns `false` when the fixture does not exist.
fn fixture_file(path: &Path, kind: Kind, version: usize) -> bool {
    let Ok(sql) = std::fs::read_to_string(fixture_path(&format!("{}-v{version}.sql", kind.name())))
    else {
        return false;
    };
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    Connection::open(path).unwrap().execute_batch(&sql).unwrap();
    true
}

/// Row counts of the user tables (FTS and shadow tables left out).
fn counts(conn: &Connection) -> Vec<(String, i64)> {
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
               AND sql NOT LIKE 'CREATE VIRTUAL%'
               AND name NOT IN (SELECT name FROM pragma_table_list WHERE type = 'shadow')
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let n = conn
                .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
                    r.get(0)
                })
                .unwrap();
            (table, n)
        })
        .collect()
}

/// Integrity, foreign keys, version and data of an upgraded database.
fn check_upgraded(conn: &Connection, kind: Kind, before: &[(String, i64)]) {
    assert_eq!(schema::version(conn).unwrap(), kind.latest_version());
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let fk: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk, 0);
    let after = counts(conn);
    for (table, n) in before {
        let now = after.iter().find(|(t, _)| t == table).map(|(_, n)| *n);
        assert_eq!(now, Some(*n), "{table} lost rows in the upgrade");
    }
    if kind == Kind::Library {
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM posts_fts WHERE posts_fts MATCH 'lampada'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(hits > 0, "search still answers");
    }
}

fn expected(kind: Kind, version: usize) -> Upgrade {
    if version == kind.latest_version() {
        Upgrade::Current
    } else {
        Upgrade::Upgraded {
            from: version,
            to: kind.latest_version(),
        }
    }
}

fn cache(users: &Path) -> UserDbCache {
    UserDbCache::new(
        users,
        &UserDbCacheConfig::default(),
        UserDbConfig::default(),
    )
}

#[test]
fn every_library_fixture_upgrades_lazily_when_first_opened() {
    let latest = Kind::Library.latest_version();
    for version in 1..=latest {
        let dir = tempfile::tempdir().unwrap();
        let users = dir.path().join("users");
        let path = users.join(USER).join("library.sqlite");
        assert!(
            fixture_file(&path, Kind::Library, version),
            "fixture v{version}"
        );
        let before = counts(&Connection::open(&path).unwrap());

        let seen = Arc::new(Mutex::new(Vec::new()));
        let cache = {
            let seen = Arc::clone(&seen);
            cache(&users).with_upgrade_listener(move |user, upgrade| {
                seen.lock().unwrap().push((user.to_owned(), upgrade));
            })
        };
        let db = cache.get(USER).unwrap();
        let upgrade = expected(Kind::Library, version);
        assert_eq!(db.schema_upgrade(), upgrade, "v{version}");
        let reported = seen.lock().unwrap().clone();
        if upgrade == Upgrade::Current {
            assert!(reported.is_empty(), "nothing to report for v{version}");
        } else {
            assert_eq!(reported, [(USER.to_owned(), upgrade)]);
        }
        db.read(|conn| {
            check_upgraded(conn, Kind::Library, &before);
            Ok::<_, DbError>(())
        })
        .unwrap();
        // The same handle serves later requests without upgrading again.
        assert!(Arc::ptr_eq(&db, &cache.get(USER).unwrap()));
        assert_eq!(seen.lock().unwrap().len(), reported.len());
    }
}

#[test]
fn every_library_fixture_upgrades_in_the_sweep() {
    let latest = Kind::Library.latest_version();
    for version in 1..=latest {
        let dir = tempfile::tempdir().unwrap();
        let users = dir.path().join("users");
        let path = users.join(USER).join("library.sqlite");
        assert!(
            fixture_file(&path, Kind::Library, version),
            "fixture v{version}"
        );
        let before = counts(&Connection::open(&path).unwrap());

        let cache = cache(&users);
        let outcome = cache.upgrade(USER).unwrap();
        let wanted = match expected(Kind::Library, version) {
            Upgrade::Upgraded { from, to } => LibraryUpgrade::Upgraded { from, to },
            _ => LibraryUpgrade::Current,
        };
        assert_eq!(outcome, wanted, "v{version}");
        assert!(
            !cache.is_open(USER),
            "the sweep keeps its handles out of the cache"
        );
        check_upgraded(&Connection::open(&path).unwrap(), Kind::Library, &before);
        // A second sweep finds it current from the header alone.
        assert_eq!(cache.upgrade(USER).unwrap(), LibraryUpgrade::Current);
    }
}

#[test]
fn the_sweep_creates_an_empty_file_and_skips_what_it_must_not_touch() {
    let dir = tempfile::tempdir().unwrap();
    let users = dir.path().join("users");
    let cache = cache(&users);
    let latest = Kind::Library.latest_version();

    // An empty file (an open interrupted before its first migration).
    let empty = users.join(USER).join("library.sqlite");
    std::fs::create_dir_all(empty.parent().unwrap()).unwrap();
    std::fs::write(&empty, b"").unwrap();
    assert_eq!(
        cache.upgrade(USER).unwrap(),
        LibraryUpgrade::Upgraded {
            from: 0,
            to: latest
        }
    );

    // An open library is current: opening upgraded it.
    let open = "01J9Z3B8K4QW6TFX0V7G2N5RCB";
    cache.get(open).unwrap();
    assert_eq!(cache.upgrade(open).unwrap(), LibraryUpgrade::Current);

    // No library, a locked one, a bad id.
    assert_eq!(
        cache.upgrade("01J9Z3B8K4QW6TFX0V7G2N5RCC").unwrap(),
        LibraryUpgrade::Missing
    );
    let locked = "01J9Z3B8K4QW6TFX0V7G2N5RCD";
    std::fs::create_dir_all(users.join(locked)).unwrap();
    drop(
        UserDb::open(
            users.join(locked).join("library.sqlite"),
            &UserDbConfig::default(),
        )
        .unwrap(),
    );
    lock_library(&users, locked, "test").unwrap();
    assert_eq!(cache.upgrade(locked).unwrap(), LibraryUpgrade::Locked);
    assert!(matches!(cache.upgrade("../x"), Err(DbError::InvalidUserId)));
}

#[test]
fn every_control_fixture_upgrades_at_boot() {
    let latest = Kind::Control.latest_version();
    let mut upgraded = 0;
    for version in 1..=latest {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control/control.sqlite");
        assert!(
            fixture_file(&path, Kind::Control, version),
            "fixture v{version}"
        );
        let before = counts(&Connection::open(&path).unwrap());
        let db = ControlDb::open(&path, &ControlDbConfig::default()).unwrap();
        let upgrade = db.schema_upgrade();
        assert_eq!(upgrade, expected(Kind::Control, version));
        if upgrade != Upgrade::Current {
            upgraded += 1;
        }
        db.read(|conn| {
            check_upgraded(conn, Kind::Control, &before);
            Ok::<_, DbError>(())
        })
        .unwrap();
    }
    // Every fixture but the latest took a real upgrade (v1 → v2 since T9).
    assert_eq!(upgraded, latest - 1);
}

/// Moves the file at `path` one version past this build, as the next release
/// would, optionally with a compat floor.
fn make_newer(path: &Path, kind: Kind, floor: Option<usize>) -> usize {
    let newer = kind.latest_version() + 1;
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS added_by_next_release (id INTEGER PRIMARY KEY);
         DROP TABLE IF EXISTS schema_compat;",
    )
    .unwrap();
    if let Some(floor) = floor {
        conn.execute_batch(&format!(
            "CREATE TABLE schema_compat (min_reader_version INTEGER NOT NULL);
             INSERT INTO schema_compat (min_reader_version) VALUES ({floor});"
        ))
        .unwrap();
    }
    conn.pragma_update(None, "user_version", i64::try_from(newer).unwrap())
        .unwrap();
    newer
}

#[test]
fn a_newer_library_opens_unless_its_compat_floor_refuses_this_build() {
    let dir = tempfile::tempdir().unwrap();
    let users = dir.path().join("users");
    let path = users.join(USER).join("library.sqlite");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    drop(UserDb::open(&path, &UserDbConfig::default()).unwrap());
    let latest = Kind::Library.latest_version();

    // Expand only: the previous build runs on it (a rollback).
    let newer = make_newer(&path, Kind::Library, None);
    let db = UserDb::open(&path, &UserDbConfig::default()).unwrap();
    assert_eq!(db.schema_upgrade(), Upgrade::Ahead { found: newer });
    let posts: i64 = db
        .read(|c| {
            c.query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
                .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(posts, 0);
    drop(db);
    assert_eq!(
        cache(&users).upgrade(USER).unwrap(),
        LibraryUpgrade::Ahead { found: newer }
    );
    // The file is left at its version: nothing was migrated down or up.
    let on_disk = schema::version(&Connection::open(&path).unwrap()).unwrap();
    assert_eq!(on_disk, newer);

    // A floor at this build's version still admits it.
    make_newer(&path, Kind::Library, Some(latest));
    assert!(UserDb::open(&path, &UserDbConfig::default()).is_ok());

    // A contract step that needs a newer build refuses this one, everywhere.
    make_newer(&path, Kind::Library, Some(newer));
    let err = UserDb::open(&path, &UserDbConfig::default()).err().unwrap();
    assert!(
        matches!(
            err,
            DbError::SchemaTooNew {
                kind: "library",
                found,
                needs,
                supported,
            } if found == newer && needs == newer && supported == latest
        ),
        "{err}"
    );
    assert!(matches!(
        cache(&users).upgrade(USER),
        Err(DbError::SchemaTooNew { .. })
    ));
    let err = cache(&users).get(USER).err().unwrap();
    assert!(
        matches!(&err, DbError::Open(inner) if matches!(**inner, DbError::SchemaTooNew { .. })),
        "{err}"
    );
}

#[test]
fn a_newer_control_database_boots_unless_its_compat_floor_refuses_this_build() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sqlite");
    drop(ControlDb::open(&path, &ControlDbConfig::default()).unwrap());

    let newer = make_newer(&path, Kind::Control, None);
    let db = ControlDb::open(&path, &ControlDbConfig::default()).unwrap();
    assert_eq!(db.schema_upgrade(), Upgrade::Ahead { found: newer });
    drop(db);

    make_newer(&path, Kind::Control, Some(newer));
    let err = ControlDb::open(&path, &ControlDbConfig::default())
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            DbError::SchemaTooNew {
                kind: "control",
                ..
            }
        ),
        "{err}"
    );
}
