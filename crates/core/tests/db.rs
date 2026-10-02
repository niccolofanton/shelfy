//! `UserDb`, `ControlDb` and the handle cache (plan §2.3).

mod support;

use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode};
use shelfy_core::db::{
    ControlDb, ControlDbConfig, DbError, UserDb, UserDbCache, UserDbCacheConfig, UserDbConfig,
    is_valid_user_id, library_ids, lock_library, unlock_library,
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

#[test]
fn get_if_present_never_opens_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(&dir, 64, Duration::from_secs(600));
    assert!(cache.get_if_present("userA").is_none());
    assert!(
        !dir.path().join("users/userA").exists(),
        "a peek creates nothing"
    );
    let db = cache.get("userA").unwrap();
    let peeked = cache.get_if_present("userA").expect("cached");
    assert!(Arc::ptr_eq(&db, &peeked));
    let before = db.generation();
    cache.evict("userA");
    assert!(cache.get_if_present("userA").is_none());
    let reopened = cache.get("userA").unwrap();
    assert!(
        !Arc::ptr_eq(&db, &reopened),
        "a new handle after the eviction"
    );
    assert_ne!(db.generation().instance, reopened.generation().instance);
    assert_ne!(db.generation(), before, "the retired generation moved on");
}

/// The *From T11* note of P1-07, fixed in the cache: every handle on a
/// user's library shares one generation, so a write through a handle the
/// cache has evicted for capacity (or idleness) still moves what the new
/// handle reports.
#[test]
fn handles_of_one_library_share_its_generation() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(&dir, 1, Duration::from_millis(50));
    // A request still holds `old` while another user's library pushes it out.
    let old = cache.get("userA").unwrap();
    cache.run_maintenance();
    let other = cache.get("userB").unwrap();
    cache.run_maintenance();
    assert!(
        cache.get_if_present("userA").is_none(),
        "evicted for capacity"
    );
    let new = cache.get("userA").unwrap();
    assert!(!Arc::ptr_eq(&old, &new));
    assert_eq!(old.generation(), new.generation());
    assert_eq!(cache.generation("userA"), Some(new.generation()));

    let seen = new.generation();
    old.write(|tx| repo::posts::insert(tx, &bare_post("ig_1", Platform::Instagram, NOW), NOW))
        .unwrap();
    assert_ne!(
        new.generation(),
        seen,
        "the write through the old handle shows"
    );
    assert_eq!(old.generation(), new.generation());
    assert_ne!(other.generation().instance, new.generation().instance);

    // Once no handle holds the library, maintenance forgets its generation;
    // the next handle starts a new instance, so no old value comes back.
    let instance = new.generation().instance;
    drop((old, new, other));
    thread::sleep(Duration::from_millis(120));
    cache.run_maintenance();
    assert!(cache.is_empty(), "idle libraries are released");
    assert_eq!(cache.generation("userA"), None);
    let fresh = cache.get("userA").unwrap();
    assert_ne!(fresh.generation().instance, instance);
}

/// F6 (P1-03 review, L3): a library's generation cell stays registered after
/// its last handle closes, until a maintenance pass forgets it. A library
/// replaced in between (lock, restore, unlock, with no request of that user
/// and no maintenance pass while it is locked) must still come back with a
/// new generation, or the ETags and cached counts from before the restore
/// would match it.
#[test]
fn a_library_replaced_while_no_handle_held_it_gets_a_new_generation() {
    let dir = tempfile::tempdir().unwrap();
    let users = dir.path().join("users");
    let cache = cache(&dir, 1, Duration::from_secs(600));
    let a = cache.get("userA").unwrap();
    a.write(|tx| repo::posts::insert(tx, &bare_post("ig_1", Platform::Instagram, NOW), NOW))
        .unwrap();
    let before = a.generation();

    // Another user's library pushes A out while a request still holds A's
    // handle, so the maintenance pass keeps A's cell; then the request ends.
    let _b = cache.get("userB").unwrap();
    cache.run_maintenance();
    assert!(cache.get_if_present("userA").is_none(), "A was evicted");
    drop(a);

    // The operator restores A: nobody holds the file (an exclusive lock is
    // granted), its content changes, and it is unlocked again.
    assert!(lock_library(&users, "userA", "restore").unwrap());
    {
        let conn = Connection::open(users.join("userA/library.sqlite")).unwrap();
        conn.execute_batch("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;")
            .expect("nobody else has the library open");
        conn.execute("DELETE FROM posts", []).unwrap();
    }
    assert!(unlock_library(&users, "userA").unwrap());

    let reopened = cache.get("userA").unwrap();
    let posts: i64 = reopened
        .read(|c| {
            c.query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
                .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(posts, 0, "the restored content");
    assert_ne!(
        reopened.generation().instance,
        before.instance,
        "the replaced library starts a new generation"
    );
}

#[test]
fn handles_opened_outside_the_cache_have_their_own_generation() {
    let dir = tempfile::tempdir().unwrap();
    let a = open(&dir, &UserDbConfig::default());
    let b = open(&dir, &UserDbConfig::default());
    assert_ne!(a.generation().instance, b.generation().instance);
}

#[test]
fn a_locked_library_is_released_and_never_opened() {
    let dir = tempfile::tempdir().unwrap();
    let users = dir.path().join("users");
    let cache = cache(&dir, 64, Duration::from_secs(600));
    let db = cache.get("userA").unwrap();
    db.write(|tx| repo::posts::insert(tx, &bare_post("ig_1", Platform::Instagram, NOW), NOW))
        .unwrap();

    assert!(lock_library(&users, "userA", "restore").unwrap());
    assert!(cache.is_locked("userA").unwrap());
    // Maintenance releases the handle of a locked user, with no request.
    cache.run_maintenance();
    assert!(!cache.is_open("userA"));
    assert_eq!(db.open_connections(), (false, 0));
    // Requests are refused while it is locked.
    let err = cache.get("userA").err().unwrap();
    assert!(matches!(err, DbError::Locked) && err.is_locked(), "{err}");

    // A refusal releases a cached handle too, and the library of a user
    // locked before their first request is never created.
    let b = cache.get("userB").unwrap();
    lock_library(&users, "userB", "").unwrap();
    assert!(matches!(cache.get("userB"), Err(DbError::Locked)));
    cache.run_maintenance();
    assert_eq!(b.open_connections(), (false, 0));
    lock_library(&users, "userC", "").unwrap();
    assert!(matches!(cache.get("userC"), Err(DbError::Locked)));
    assert!(!users.join("userC").join("library.sqlite").exists());

    // Unlocked, the next request opens a new handle on the same data.
    assert!(unlock_library(&users, "userA").unwrap());
    let again = cache.get("userA").unwrap();
    assert!(!Arc::ptr_eq(&db, &again));
    assert_ne!(again.generation().instance, db.generation().instance);
    let n: i64 = again
        .read(|c| {
            c.query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
                .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(n, 1);
}

/// Takes `path` the way `admin user restore-db` checks a library is unused:
/// an exclusive lock, granted only while no other connection has it open.
fn exclusive_probe(path: &std::path::Path) -> rusqlite::Result<Connection> {
    let probe = Connection::open(path)?;
    probe.busy_timeout(Duration::ZERO)?;
    probe.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
    probe.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;
    probe.execute_batch("BEGIN EXCLUSIVE; COMMIT;")?;
    Ok(probe)
}

#[test]
fn a_released_handle_cannot_reopen_a_locked_library() {
    // Review of P1-12, H1: a request or a job chunk that took its handle
    // before `admin user lock` kept it after the server released it, and its
    // next call reopened the library by path, so it wrote into a library
    // being restored.
    let dir = tempfile::tempdir().unwrap();
    let users = dir.path().join("users");
    let cache = cache(&dir, 64, Duration::from_secs(600));
    let held = cache.get("userA").unwrap();
    held.write(|tx| repo::posts::insert(tx, &bare_post("ig_1", Platform::Instagram, NOW), NOW))
        .unwrap();
    held.read(|_| Ok::<_, DbError>(())).unwrap();

    lock_library(&users, "userA", "restore").unwrap();
    cache.run_maintenance();
    assert_eq!(held.open_connections(), (false, 0), "released");
    // Nothing holds the file: a restore may take it now.
    let library = users.join("userA").join("library.sqlite");
    drop(exclusive_probe(&library).unwrap());

    // The held handle stays closed.
    let err = held
        .write(|tx| repo::posts::insert(tx, &bare_post("ig_2", Platform::Instagram, NOW), NOW))
        .unwrap_err();
    assert!(matches!(err, RepoError::Db(DbError::Locked)), "{err}");
    let err = held
        .read(|c| {
            c.query_row("SELECT count(*) FROM posts", [], |r| r.get::<_, i64>(0))
                .map_err(DbError::from)
        })
        .unwrap_err();
    assert!(err.is_locked(), "{err}");
    assert!(held.checkpoint().unwrap_err().is_locked());
    assert_eq!(held.open_connections(), (false, 0), "nothing reopened");
    drop(exclusive_probe(&library).unwrap());

    // Opening the library anew is refused too, and a locked library that
    // does not exist yet is not created.
    let err = UserDb::open(&library, &UserDbConfig::default())
        .err()
        .unwrap();
    assert!(matches!(err, DbError::Locked), "{err}");
    lock_library(&users, "userB", "").unwrap();
    let missing = users.join("userB").join("library.sqlite");
    assert!(matches!(
        UserDb::open(&missing, &UserDbConfig::default()),
        Err(DbError::Locked)
    ));
    assert!(!missing.exists());

    // Unlocked, the same handle works again, on the same data.
    unlock_library(&users, "userA").unwrap();
    let n: i64 = held
        .read(|c| {
            c.query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
                .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_lock_taken_while_a_handle_is_open_takes_effect_at_its_release() {
    // A handle whose writer is still open keeps the file, so a restore waits
    // for it; once it is released, the lock holds.
    let dir = tempfile::tempdir().unwrap();
    let users = dir.path().join("users");
    let cache = cache(&dir, 64, Duration::from_secs(600));
    let held = cache.get("userA").unwrap();
    lock_library(&users, "userA", "restore").unwrap();
    let library = users.join("userA").join("library.sqlite");
    let busy = exclusive_probe(&library).unwrap_err();
    assert_eq!(
        busy.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy),
        "the open writer keeps a restore out: {busy}"
    );
    held.release();
    drop(exclusive_probe(&library).unwrap());
    assert!(
        held.write(|tx| repo::posts::insert(tx, &bare_post("ig_1", Platform::Instagram, NOW), NOW))
            .is_err()
    );
}

#[test]
fn library_ids_lists_the_users_with_a_library() {
    let dir = tempfile::tempdir().unwrap();
    let users = dir.path().join("users");
    assert!(library_ids(&users).unwrap().is_empty(), "no users dir yet");
    let cache = cache(&dir, 64, Duration::from_secs(600));
    cache.get("userB").unwrap();
    cache.get("userA").unwrap();
    std::fs::create_dir_all(users.join("noLibrary")).unwrap();
    std::fs::create_dir_all(users.join("not.a.user")).unwrap();
    std::fs::write(users.join("stray.sqlite"), b"").unwrap();
    assert_eq!(library_ids(&users).unwrap(), ["userA", "userB"]);
    assert!(is_valid_user_id("01J9Z3B8K4QW6TFX0V7G2N5RCA"));
    assert!(!is_valid_user_id("../etc"));
}
