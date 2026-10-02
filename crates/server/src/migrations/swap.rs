//! The atomic install of a migrated library (plan §4.3: "new DB file +
//! rename", the previous web library kept).
//!
//! The new library is built and checked in its own file. Putting it in place
//! uses SQLite's online backup API rather than a file rename: the live
//! library is in WAL mode and may be open (the API's handle cache, a request
//! in flight), and renaming a WAL database separates it from its `-wal` and
//! `-shm` files, which SQLite would then apply to the wrong database. The
//! backup copies every page of the new file into the live one in a single
//! write transaction: readers see the old library or the new one, never a
//! mix, and a failure leaves the old one untouched.
//!
//! Before that, the live library is copied the same way to
//! `library.prev-<job id>.sqlite` next to it ([`keep_previous`]; a retry of
//! the same job keeps the first copy). Its settings and notifications are
//! carried into the new library, since an empty library (no posts, no
//! collections) may still have them: a setting the web library has wins over
//! the desktop's.
//!
//! A library locked for maintenance (`admin user lock`, a restore) is never
//! touched: the lock is checked before the live library is opened and again
//! once the connection holds SQLite's shared lock, which a restore waits for.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use shelfy_core::db::is_library_file_locked;

/// How long the swap waits for other connections to release a lock.
const BUSY_RETRIES: u32 = 100;
const BUSY_PAUSE: Duration = Duration::from_millis(100);

/// Why the swap did not happen.
#[derive(Debug, thiserror::Error)]
pub enum SwapError {
    /// The live library has posts or collections.
    #[error("the web library is not empty")]
    NotEmpty,
    /// The live library stayed locked.
    #[error("the web library stayed locked")]
    Busy,
    /// The live library is locked for maintenance (`admin user lock`).
    #[error("the web library is locked for maintenance")]
    Locked,
    /// The two databases have different page sizes.
    #[error("the bundle's page size differs from the web library's")]
    PageSize,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Whether the library at `path` has no posts and no collections.
///
/// # Errors
///
/// The library cannot be read.
pub fn is_empty(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM posts) AND NOT EXISTS (SELECT 1 FROM collections)",
        [],
        |r| r.get(0),
    )
}

/// Replaces the content of the empty library at `live` with the library at
/// `new`, after copying `live` to `previous`. Blocking.
///
/// # Errors
///
/// [`SwapError::Locked`] when `live` is locked for maintenance;
/// [`SwapError::NotEmpty`] when it has posts or collections;
/// [`SwapError::Busy`] when it stays locked; otherwise SQLite or I/O errors.
/// On error `live` is unchanged.
pub fn replace_library(new: &Path, live: &Path, previous: &Path) -> Result<(), SwapError> {
    refuse_locked(live)?;
    // Read-write without CREATE: the live library exists (the caller opened it).
    let mut live_conn = Connection::open_with_flags(
        live,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    live_conn.busy_timeout(Duration::from_secs(5))?;
    let empty = is_empty(&live_conn).map_err(|err| match err.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => SwapError::Busy,
        _ => err.into(),
    })?;
    // The read holds SQLite's shared lock until this connection closes, and
    // a restore takes the library only with an exclusive one: a lock taken
    // since the check above is seen now, and a later one waits for the swap.
    refuse_locked(live)?;
    if !empty {
        return Err(SwapError::NotEmpty);
    }

    // The new library takes the live one's settings (they win over the
    // desktop's) and notifications.
    {
        let new_conn = Connection::open(new)?;
        new_conn.execute("ATTACH DATABASE ?1 AS live", [live.to_string_lossy()])?;
        new_conn.execute_batch(
            "INSERT INTO main.settings (key, value_json, updated_at)
               SELECT key, value_json, updated_at FROM live.settings WHERE true
               ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json,
                                               updated_at = excluded.updated_at;
             INSERT INTO main.notifications (kind, code, params_json, target, created_at, read_at)
               SELECT kind, code, params_json, target, created_at, read_at FROM live.notifications
               ORDER BY id;
             DETACH DATABASE live;",
        )?;
    }

    keep_previous(&live_conn, previous)?;

    // Install: every page of the new library, in one write transaction.
    let new_conn = Connection::open_with_flags(
        new,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let page_size = |c: &Connection| c.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0));
    if page_size(&new_conn)? != page_size(&live_conn)? {
        // A WAL database cannot change its page size through a backup.
        return Err(SwapError::PageSize);
    }
    copy_pages(&new_conn, &mut live_conn)?;
    live_conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    Ok(())
}

/// [`SwapError::Locked`] when the library at `live` is locked for
/// maintenance.
pub(crate) fn refuse_locked(live: &Path) -> Result<(), SwapError> {
    if is_library_file_locked(live)? {
        Err(SwapError::Locked)
    } else {
        Ok(())
    }
}

/// Keeps the library of `live_conn` as `previous`: one consistent copy, as a
/// single file. A copy that exists already (an earlier try of the same job)
/// is kept: it holds the library as it was before the job. Blocking.
///
/// # Errors
///
/// [`SwapError::Busy`] when the library stays locked; SQLite or I/O errors.
pub fn keep_previous(live_conn: &Connection, previous: &Path) -> Result<(), SwapError> {
    if previous.exists() {
        return Ok(());
    }
    let partial = previous.with_extension("sqlite.partial");
    let _ = fs::remove_file(&partial);
    {
        let mut copy = Connection::open(&partial)?;
        copy_pages(live_conn, &mut copy)?;
        copy.pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get::<_, String>(0))?;
    }
    fs::File::open(&partial)?.sync_all()?;
    fs::rename(&partial, previous)?;
    Ok(())
}

/// Copies every page of `source` into `target` in one step, retrying while a
/// lock is held.
fn copy_pages(source: &Connection, target: &mut Connection) -> Result<(), SwapError> {
    let backup = Backup::new(source, target)?;
    for _ in 0..BUSY_RETRIES {
        if backup.step(-1)? == StepResult::Done {
            return Ok(());
        }
        thread::sleep(BUSY_PAUSE);
    }
    Err(SwapError::Busy)
}

#[cfg(test)]
mod tests {
    use shelfy_core::db::{DbError, LOCK_FILE_NAME, UserDb, UserDbConfig};
    use shelfy_core::repo::Platform;
    use shelfy_core::repo::posts::{self, NewPost};

    use super::*;

    fn post(key: &str) -> NewPost {
        NewPost::new(key, Platform::Instagram, &key[3..], "image", 1)
    }

    #[test]
    fn the_new_library_replaces_an_empty_one_while_it_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let live_path = dir.path().join("library.sqlite");
        let new_path = dir.path().join("new.sqlite");
        let previous = dir.path().join("library.prev-X.sqlite");

        // The live library, open as the server keeps it, with a setting.
        let live = UserDb::open(&live_path, &UserDbConfig::default()).unwrap();
        live.write(|tx| {
            tx.execute(
                "INSERT INTO settings (key, value_json, updated_at) VALUES ('language', '\"it\"', 1)",
                [],
            )
            .map_err(DbError::from)
        })
        .unwrap();
        let new = UserDb::open(&new_path, &UserDbConfig::default()).unwrap();
        new.write(|tx| {
            posts::insert(tx, &post("ig_1"), 1)?;
            posts::insert(tx, &post("ig_2"), 1)
        })
        .unwrap();
        new.checkpoint().unwrap();
        drop(new);

        replace_library(&new_path, &live_path, &previous).unwrap();

        // The open handle sees the new library, with the setting carried over.
        let (posts, language): (i64, String) = live
            .read(|c| {
                c.query_row(
                    "SELECT (SELECT count(*) FROM posts),
                            (SELECT value_json FROM settings WHERE key = 'language')",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(DbError::from)
            })
            .unwrap();
        assert_eq!((posts, language.as_str()), (2, "\"it\""));
        let old = Connection::open(&previous).unwrap();
        let old_posts: i64 = old
            .query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(old_posts, 0, "the previous library is kept as it was");

        // A library with posts is never replaced.
        let err = replace_library(&new_path, &live_path, &previous).unwrap_err();
        assert!(matches!(err, SwapError::NotEmpty), "{err}");
    }

    #[test]
    fn a_library_locked_for_maintenance_is_never_replaced() {
        // Review of P1-12, M3: the install of a migration wrote into a
        // library that an operator had locked to restore it.
        let dir = tempfile::tempdir().unwrap();
        let live_path = dir.path().join("library.sqlite");
        let new_path = dir.path().join("new.sqlite");
        let previous = dir.path().join("library.prev-X.sqlite");
        drop(UserDb::open(&live_path, &UserDbConfig::default()).unwrap());
        let new = UserDb::open(&new_path, &UserDbConfig::default()).unwrap();
        new.write(|tx| posts::insert(tx, &post("ig_1"), 1)).unwrap();
        new.checkpoint().unwrap();
        drop(new);

        std::fs::write(dir.path().join(LOCK_FILE_NAME), "restore").unwrap();
        let err = replace_library(&new_path, &live_path, &previous).unwrap_err();
        assert!(matches!(err, SwapError::Locked), "{err}");
        assert!(!previous.exists(), "nothing was copied");
        let posts: i64 = Connection::open(&live_path)
            .unwrap()
            .query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(posts, 0, "the live library is untouched");

        std::fs::remove_file(dir.path().join(LOCK_FILE_NAME)).unwrap();
        replace_library(&new_path, &live_path, &previous).unwrap();
    }

    #[test]
    fn the_web_librarys_settings_win_and_the_first_copy_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let live_path = dir.path().join("library.sqlite");
        let new_path = dir.path().join("new.sqlite");
        let previous = dir.path().join("library.prev-7.sqlite");
        let setting = |db: &UserDb, key: &str, value: &str| {
            db.write(|tx| {
                tx.execute(
                    "INSERT INTO settings (key, value_json, updated_at) VALUES (?1, ?2, 1)",
                    [key, value],
                )
                .map_err(DbError::from)
            })
            .unwrap();
        };
        let live = UserDb::open(&live_path, &UserDbConfig::default()).unwrap();
        setting(&live, "language", "\"en\"");
        // The desktop's settings, in the bundle.
        let new = UserDb::open(&new_path, &UserDbConfig::default()).unwrap();
        setting(&new, "language", "\"it\"");
        setting(
            &new,
            "archiveAssetTypes",
            r#"{"thumbnail":true,"image":true,"video":false}"#,
        );
        new.checkpoint().unwrap();
        drop(new);

        // A copy from an earlier try of the same job holds the library as it
        // was before the job: it is kept.
        std::fs::write(&previous, b"the first copy").unwrap();
        replace_library(&new_path, &live_path, &previous).unwrap();
        assert_eq!(std::fs::read(&previous).unwrap(), b"the first copy");

        let settings: Vec<String> = live
            .read(|c| {
                c.prepare("SELECT key || '=' || value_json FROM settings ORDER BY key")
                    .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
                    .map_err(DbError::from)
            })
            .unwrap();
        assert_eq!(
            settings,
            [
                r#"archiveAssetTypes={"thumbnail":true,"image":true,"video":false}"#,
                r#"language="en""#
            ]
        );
    }
}
