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
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
use shelfy_core::db::{
    ControlDb, ControlDbConfig, DbError, LibraryUpgrade, UserDb, UserDbCache, UserDbCacheConfig,
    UserDbConfig, lock_library,
};
use shelfy_core::schema::{self, Kind, OLDER_BUILD_META_KEY, OlderBuild, Upgrade};
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

/// Runs `openers` at once on the database at `path` while another connection
/// holds its write lock, so every opener reads the old version before any of
/// them can migrate; then lets them go. Returns what each opener reported.
fn race<T: Send + 'static>(path: &Path, openers: Vec<Box<dyn FnOnce() -> T + Send>>) -> Vec<T> {
    let holder = Connection::open(path).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let start = Arc::new(Barrier::new(openers.len() + 1));
    let threads: Vec<_> = openers
        .into_iter()
        .map(|opener| {
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                opener()
            })
        })
        .collect();
    start.wait();
    thread::sleep(Duration::from_millis(500));
    holder.execute_batch("COMMIT").unwrap();
    threads.into_iter().map(|t| t.join().unwrap()).collect()
}

#[test]
fn concurrent_openers_of_an_old_control_database_migrate_it_once() {
    // Review of P1-12, M1: both openers read `user_version` before taking
    // the write lock, so the second applied migration v2 again and failed
    // with "duplicate column name: expires_at".
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sqlite");
    assert!(fixture_file(&path, Kind::Control, 1));
    let wal: String = Connection::open(&path)
        .unwrap()
        .pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))
        .unwrap();
    assert_eq!(wal, "wal");

    let opener = |path: std::path::PathBuf| -> Box<dyn FnOnce() -> Result<Upgrade, String> + Send> {
        Box::new(move || {
            ControlDb::open(&path, &ControlDbConfig::default())
                .map(|db| db.schema_upgrade())
                .map_err(|err| err.to_string())
        })
    };
    let mut outcomes = race(&path, vec![opener(path.clone()), opener(path.clone())]);
    outcomes.sort_by_key(|o| format!("{o:?}"));
    let latest = Kind::Control.latest_version();
    assert_eq!(
        outcomes,
        [
            Ok(Upgrade::Current),
            Ok(Upgrade::Upgraded {
                from: 1,
                to: latest
            })
        ],
        "one opener migrates, the other finds nothing left to do"
    );
    let conn = Connection::open(&path).unwrap();
    assert_eq!(schema::version(&conn).unwrap(), latest);
    let columns: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('api_tokens') WHERE name = 'expires_at'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(columns, 1);
}

#[test]
fn the_sweep_and_a_first_request_migrate_a_library_once() {
    // Review of P1-12, M1: the sweep (outside the cache) and a first request
    // (through it) on a library that was never migrated: the second one
    // failed with "table posts already exists".
    let dir = tempfile::tempdir().unwrap();
    let users = dir.path().join("users");
    let path = users.join(USER).join("library.sqlite");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // A WAL-mode file at v0: an open interrupted before its first migration.
    let wal: String = Connection::open(&path)
        .unwrap()
        .pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))
        .unwrap();
    assert_eq!(wal, "wal");

    let cache = Arc::new(cache(&users));
    let sweep: Box<dyn FnOnce() -> Result<String, String> + Send> = {
        let cache = Arc::clone(&cache);
        Box::new(move || {
            cache
                .upgrade(USER)
                .map(|u| format!("{u:?}"))
                .map_err(|e| e.to_string())
        })
    };
    let request: Box<dyn FnOnce() -> Result<String, String> + Send> = {
        let cache = Arc::clone(&cache);
        Box::new(move || {
            cache
                .get(USER)
                .map(|db| format!("{:?}", db.schema_upgrade()))
                .map_err(|e| e.to_string())
        })
    };
    let outcomes = race(&path, vec![sweep, request]);
    let latest = Kind::Library.latest_version();
    let upgraded = format!("Upgraded {{ from: 0, to: {latest} }}");
    assert!(
        outcomes.iter().all(Result::is_ok),
        "neither opener fails: {outcomes:?}"
    );
    let migrated = outcomes
        .iter()
        .filter(|o| o.as_deref() == Ok(upgraded.as_str()))
        .count();
    assert_eq!(migrated, 1, "exactly one opener migrates: {outcomes:?}");
    assert!(
        outcomes.iter().any(|o| o.as_deref() == Ok("Current")),
        "{outcomes:?}"
    );

    // The request's handle serves the migrated library.
    let n: i64 = cache
        .get(USER)
        .unwrap()
        .read(|c| {
            c.query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
                .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(n, 0);
}

fn older_build_record(path: &Path) -> Option<OlderBuild> {
    schema::older_build(&Connection::open(path).unwrap()).unwrap()
}

#[test]
fn opening_a_newer_library_records_that_an_older_build_wrote_to_it() {
    // Review of P1-12, M5: a rolled-back build writes rows that the newer
    // release's derived data does not cover, and nothing recorded it.
    let dir = tempfile::tempdir().unwrap();
    let users = dir.path().join("users");
    let path = users.join(USER).join("library.sqlite");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    drop(UserDb::open(&path, &UserDbConfig::default()).unwrap());
    let latest = Kind::Library.latest_version();
    assert_eq!(
        older_build_record(&path),
        None,
        "a current library records nothing"
    );

    let newer = make_newer(&path, Kind::Library, None);
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let db = UserDb::open(&path, &UserDbConfig::default()).unwrap();
    assert_eq!(db.schema_upgrade(), Upgrade::Ahead { found: newer });
    drop(db);
    let record = older_build_record(&path).expect("recorded at open");
    assert_eq!(record.build_version, latest);
    assert!(i128::from(record.since) >= i128::try_from(before).unwrap());

    // Opening it again keeps the first record; so does the sweep.
    drop(UserDb::open(&path, &UserDbConfig::default()).unwrap());
    assert_eq!(
        cache(&users).upgrade(USER).unwrap(),
        LibraryUpgrade::Ahead { found: newer }
    );
    assert_eq!(older_build_record(&path), Some(record));

    // A build between this one and the file opened it first: the record
    // goes down to this build's version and keeps the first time.
    let set = |value: &str| {
        Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                [OLDER_BUILD_META_KEY, value],
            )
            .unwrap();
    };
    set(&format!(
        r#"{{"buildVersion":{},"since":1000}}"#,
        latest + 1
    ));
    drop(UserDb::open(&path, &UserDbConfig::default()).unwrap());
    assert_eq!(
        older_build_record(&path),
        Some(OlderBuild {
            build_version: latest,
            since: 1000
        })
    );
    // An older build on record stays.
    set(r#"{"buildVersion":0,"since":5}"#);
    drop(UserDb::open(&path, &UserDbConfig::default()).unwrap());
    assert_eq!(
        older_build_record(&path),
        Some(OlderBuild {
            build_version: 0,
            since: 5
        })
    );

    // The sweep records it for a library nobody opened since the rollback.
    let other = "01J9Z3B8K4QW6TFX0V7G2N5RCB";
    let other_path = users.join(other).join("library.sqlite");
    std::fs::create_dir_all(other_path.parent().unwrap()).unwrap();
    drop(UserDb::open(&other_path, &UserDbConfig::default()).unwrap());
    make_newer(&other_path, Kind::Library, None);
    assert!(matches!(
        cache(&users).upgrade(other).unwrap(),
        LibraryUpgrade::Ahead { .. }
    ));
    assert_eq!(
        older_build_record(&other_path).map(|r| r.build_version),
        Some(latest)
    );
}
