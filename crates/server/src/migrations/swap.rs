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
//! `library.prev-<install id>.sqlite` next to it. Its settings and
//! notifications are carried into the new library, since an empty library
//! (no posts, no collections) may still have them.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

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
/// [`SwapError::NotEmpty`] when `live` has posts or collections;
/// [`SwapError::Busy`] when it stays locked; otherwise SQLite or I/O errors.
/// On error `live` is unchanged.
pub fn replace_library(new: &Path, live: &Path, previous: &Path) -> Result<(), SwapError> {
    // Read-write without CREATE: the live library exists (the caller opened it).
    let mut live_conn = Connection::open_with_flags(
        live,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    live_conn.busy_timeout(Duration::from_secs(5))?;
    if !is_empty(&live_conn)? {
        return Err(SwapError::NotEmpty);
    }

    // The new library takes the live one's settings and notifications.
    {
        let new_conn = Connection::open(new)?;
        new_conn.execute("ATTACH DATABASE ?1 AS live", [live.to_string_lossy()])?;
        new_conn.execute_batch(
            "INSERT OR IGNORE INTO main.settings (key, value_json, updated_at)
               SELECT key, value_json, updated_at FROM live.settings;
             INSERT INTO main.notifications (kind, code, params_json, target, created_at, read_at)
               SELECT kind, code, params_json, target, created_at, read_at FROM live.notifications
               ORDER BY id;
             DETACH DATABASE live;",
        )?;
    }

    // Keep the previous library: one consistent copy, as a single file.
    let partial = previous.with_extension("sqlite.partial");
    let _ = fs::remove_file(&partial);
    {
        let mut copy = Connection::open(&partial)?;
        copy_pages(&live_conn, &mut copy)?;
        copy.pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get::<_, String>(0))?;
    }
    fs::File::open(&partial)?.sync_all()?;
    fs::rename(&partial, previous)?;

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
    use shelfy_core::db::{DbError, UserDb, UserDbConfig};
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
}
