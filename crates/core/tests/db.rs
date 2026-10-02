//! `UserDb`, `ControlDb` and the handle cache (plan §2.3).

mod support;

use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode};
use shelfy_core::db::{
    ControlDb, ControlDbConfig, DbError, UserDb, UserDbCache, UserDbCacheConfig, UserDbConfig,
};
use shelfy_core::repo::{self, Platform, RepoError};
use support::{NOW, bare_post};

fn open(dir: &tempfile::TempDir, config: &UserDbConfig) -> UserDb {
    UserDb::open(dir.path().join("library.sqlite"), config).unwrap()
}

fn pragma(conn: &Connection, name: &str) -> String {
    conn.query_row(&format!("PRAGMA {name}"), [], |r| {
        r.get::<_, rusqlite::types::Value>(0)
    })
    .map(|v| match v {
        rusqlite::types::Value::Integer(i) => i.to_string(),
        rusqlite::types::Value::Text(s) => s,
        other => format!("{other:?}"),
    })
    .unwrap()
}

#[test]
fn connections_use_the_plan_pragmas() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir, &UserDbConfig::default());
    let check = |conn: &Connection, cache: &str| {
        assert_eq!(pragma(conn, "journal_mode"), "wal");
        assert_eq!(pragma(conn, "synchronous"), "1"); // NORMAL
        assert_eq!(pragma(conn, "foreign_keys"), "1");
        assert_eq!(pragma(conn, "busy_timeout"), "5000");
        assert_eq!(pragma(conn, "temp_store"), "2"); // MEMORY
        assert_eq!(pragma(conn, "cache_size"), cache);
        assert_eq!(pragma(conn, "mmap_size"), "268435456");
    };
    db.write(|tx| {
        check(tx, "-2000");
        Ok::<_, DbError>(())
    })
    .unwrap();
    db.read(|conn| {
        check(conn, "-1000");
        Ok::<_, DbError>(())
    })
    .unwrap();
}

#[test]
fn readers_are_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir, &UserDbConfig::default());
    let err = db
        .read(|conn| {
            conn.execute("INSERT INTO meta (key, value) VALUES ('k', 'v')", [])
                .map_err(DbError::from)
        })
        .unwrap_err();
    let DbError::Sqlite(e) = err else {
        panic!("{err}")
    };
    assert_eq!(e.sqlite_error_code(), Some(ErrorCode::ReadOnly));
}

#[test]
fn readers_open_lazily_up_to_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(&dir, &UserDbConfig::default()));
    assert_eq!(db.open_connections(), (true, 0));

    // Two readers busy at once, a third read waits for one of them.
    let inside = Arc::new(Barrier::new(3));
    let release = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let (db, inside, release) = (db.clone(), inside.clone(), release.clone());
            thread::spawn(move || {
                db.read(|_| {
                    inside.wait();
                    release.wait();
                    Ok::<_, DbError>(())
                })
                .unwrap();
            })
        })
        .collect();
    inside.wait();
    assert_eq!(db.open_connections(), (true, 2));
    let waiter = {
        let db = db.clone();
        thread::spawn(move || db.read(|_| Ok::<_, DbError>(())))
    };
    thread::sleep(Duration::from_millis(50));
    release.wait();
    for h in handles {
        h.join().unwrap();
    }
    waiter.join().unwrap().unwrap();
    assert_eq!(db.open_connections(), (true, 2));
}

#[test]
fn a_read_times_out_when_every_reader_stays_busy() {
    let dir = tempfile::tempdir().unwrap();
    let config = UserDbConfig {
        max_readers: 1,
        reader_wait_timeout: Duration::from_millis(50),
        ..UserDbConfig::default()
    };
    let db = Arc::new(open(&dir, &config));
    let inside = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let holder = {
        let (db, inside, release) = (db.clone(), inside.clone(), release.clone());
        thread::spawn(move || {
            db.read(|_| {
                inside.wait();
                release.wait();
                Ok::<_, DbError>(())
            })
        })
    };
    inside.wait();
    let err = db.read(|_| Ok::<_, DbError>(())).unwrap_err();
    assert!(matches!(err, DbError::ReaderTimeout), "{err}");
    release.wait();
    holder.join().unwrap().unwrap();
}

#[test]
fn idle_readers_are_closed() {
    let dir = tempfile::tempdir().unwrap();
    let config = UserDbConfig {
        reader_idle_timeout: Duration::from_millis(30),
        ..UserDbConfig::default()
    };
    let db = open(&dir, &config);
    db.read(|_| Ok::<_, DbError>(())).unwrap();
    assert_eq!(db.open_connections(), (true, 1));
    thread::sleep(Duration::from_millis(60));
    db.prune_idle_readers();
    assert_eq!(db.open_connections(), (true, 0));
}

#[test]
fn writes_commit_or_roll_back_and_bump_the_generation() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir, &UserDbConfig::default());
    let g0 = db.generation();

    db.write(|tx| repo::posts::insert(tx, &bare_post("ig_1", Platform::Instagram, NOW), NOW))
        .unwrap();
    let g1 = db.generation();
    assert_eq!(g1.instance, g0.instance);
    assert_eq!(g1.counter, g0.counter + 1);

    // A failing closure rolls back and leaves the generation alone.
    let err = db
        .write(|tx| {
            repo::posts::insert(tx, &bare_post("ig_2", Platform::Instagram, NOW), NOW)?;
            Err::<(), _>(RepoError::Conflict("test"))
        })
        .unwrap_err();
    assert!(matches!(err, RepoError::Conflict("test")));
    assert_eq!(db.generation(), g1);

    // A transaction that changes nothing does not invalidate caches.
    db.write(|tx| {
        tx.execute("DELETE FROM meta WHERE key = 'missing'", [])
            .map_err(DbError::from)
    })
    .unwrap();
    assert_eq!(db.generation(), g1);

    let keys: Vec<String> = db
        .read(|conn| {
            conn.prepare("SELECT key FROM posts")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()
                .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(keys, ["ig_1"]);
}

#[test]
fn a_read_snapshot_ignores_later_commits() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(&dir, &UserDbConfig::default()));
    let started = Arc::new(Barrier::new(2));
    let committed = Arc::new(Barrier::new(2));
    let reader = {
        let (db, started, committed) = (db.clone(), started.clone(), committed.clone());
        thread::spawn(move || {
            db.read(|conn| {
                let count = |c: &Connection| -> rusqlite::Result<i64> {
                    c.query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
                };
                let before = count(conn)?;
                started.wait();
                committed.wait();
                Ok::<_, DbError>((before, count(conn)?))
            })
        })
    };
    started.wait();
    // The writer is not blocked by the open read transaction (WAL).
    db.write(|tx| repo::posts::insert(tx, &bare_post("ig_1", Platform::Instagram, NOW), NOW))
        .unwrap();
    committed.wait();
    assert_eq!(reader.join().unwrap().unwrap(), (0, 0));
    let after: i64 = db
        .read(|c| {
            c.query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
                .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(after, 1);
}

#[test]
fn release_checkpoints_closes_and_reopens_on_demand() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir, &UserDbConfig::default());
    db.write(|tx| repo::posts::insert(tx, &bare_post("ig_1", Platform::Instagram, NOW), NOW))
        .unwrap();
    db.read(|_| Ok::<_, DbError>(())).unwrap();
    let wal = dir.path().join("library.sqlite-wal");
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);

    db.release();
    assert_eq!(db.open_connections(), (false, 0));
    // The checkpoint emptied the WAL (SQLite also deletes it on the last close).
    assert!(std::fs::metadata(&wal).map_or(true, |m| m.len() == 0));

    // The handle stays usable.
    let n: i64 = db
        .read(|c| {
            c.query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
                .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(db.open_connections(), (true, 1));
    db.write(|tx| repo::posts::insert(tx, &bare_post("ig_2", Platform::Instagram, NOW), NOW))
        .unwrap();
}

#[test]
fn control_db_keeps_its_readers_open() {
    let dir = tempfile::tempdir().unwrap();
    let db = ControlDb::open(
        dir.path().join("control.sqlite"),
        &ControlDbConfig::default(),
    )
    .unwrap();
    assert_eq!(db.open_connections(), (true, 4));
    db.write(|tx| {
        tx.execute(
            "INSERT INTO feature_flags (key, value_json, updated_at) VALUES ('x', 'true', 1)",
            [],
        )
        .map_err(DbError::from)
    })
    .unwrap();
    let n: i64 = db
        .read(|c| {
            c.query_row("SELECT count(*) FROM feature_flags", [], |r| r.get(0))
                .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(db.open_connections(), (true, 4));
}

fn cache(dir: &tempfile::TempDir, max_open: u64, time_to_idle: Duration) -> UserDbCache {
    UserDbCache::new(
        dir.path().join("users"),
        &UserDbCacheConfig {
            max_open,
            time_to_idle,
        },
        UserDbConfig::default(),
    )
}

#[test]
fn cache_opens_each_user_once_under_their_directory() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(&dir, 64, Duration::from_secs(600));
    let a = cache.get("01J9Z3B8K4QW6TFX0V7G2N5RCA").unwrap();
    let again = cache.get("01J9Z3B8K4QW6TFX0V7G2N5RCA").unwrap();
    assert!(Arc::ptr_eq(&a, &again));
    assert_eq!(
        a.path(),
        dir.path()
            .join("users/01J9Z3B8K4QW6TFX0V7G2N5RCA/library.sqlite")
    );
    assert!(a.path().exists());

    // Concurrent first requests for one user share one open.
    let cache = Arc::new(cache);
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let cache = cache.clone();
            thread::spawn(move || cache.get("01J9Z3B8K4QW6TFX0V7G2N5RCB").unwrap())
        })
        .collect();
    let dbs: Vec<Arc<UserDb>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(dbs.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])));
}

#[test]
fn cache_rejects_unsafe_user_ids() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(&dir, 64, Duration::from_secs(600));
    for bad in [
        "",
        "..",
        "../etc",
        "a/b",
        "a.b",
        "a b",
        "ü",
        &"a".repeat(65),
    ] {
        assert!(
            matches!(cache.get(bad), Err(DbError::InvalidUserId)),
            "{bad:?}"
        );
    }
}

#[test]
fn cache_evicts_the_least_recently_used_beyond_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(&dir, 2, Duration::from_secs(600));
    // moka records accesses lazily; maintenance after each step makes the
    // recency order deterministic for the test.
    let a = cache.get("userA").unwrap();
    cache.run_maintenance();
    let b = cache.get("userB").unwrap();
    cache.run_maintenance();
    cache.get("userA").unwrap(); // A is now more recent than B
    cache.run_maintenance();
    cache.get("userC").unwrap();
    cache.run_maintenance();
    assert_eq!(cache.len(), 2);
    // B was evicted and released; A is still open.
    assert_eq!(b.open_connections(), (false, 0));
    assert!(a.open_connections().0);
    let b_again = cache.get("userB").unwrap();
    assert!(!Arc::ptr_eq(&b, &b_again));
    assert!(b_again.open_connections().0);
}

#[test]
fn cache_releases_databases_after_the_idle_time() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(&dir, 64, Duration::from_millis(50));
    let db = cache.get("userA").unwrap();
    db.write(|tx| repo::posts::insert(tx, &bare_post("ig_1", Platform::Instagram, NOW), NOW))
        .unwrap();
    thread::sleep(Duration::from_millis(120));
    cache.run_maintenance();
    assert!(cache.is_empty());
    assert_eq!(db.open_connections(), (false, 0));
}

#[test]
fn evict_releases_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(&dir, 64, Duration::from_secs(600));
    let db = cache.get("userA").unwrap();
    cache.evict("userA");
    cache.run_maintenance();
    assert_eq!(db.open_connections(), (false, 0));
    assert!(cache.is_empty());
}
