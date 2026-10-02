//! Opening connections and applying the plan's pragmas (§2.3).

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use super::DbError;

/// Per-connection SQLite settings (plan §2.3, "Pragmas").
///
/// `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON` and
/// `temp_store=MEMORY` are fixed; the sizes below are tunable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pragmas {
    /// `busy_timeout`: how long a statement waits on a lock held by another
    /// connection before failing with `SQLITE_BUSY`.
    pub busy_timeout: Duration,
    /// Page cache of the writer connection, in KiB (`cache_size = -N`).
    pub writer_cache_kib: u32,
    /// Page cache of each reader connection, in KiB.
    pub reader_cache_kib: u32,
    /// `mmap_size` in bytes. Mapped pages live in the kernel page cache, which
    /// is reclaimable under the container's memory limit.
    pub mmap_size: u64,
}

impl Default for Pragmas {
    fn default() -> Self {
        Self {
            busy_timeout: Duration::from_millis(5000),
            writer_cache_kib: 2000,
            reader_cache_kib: 1000,
            mmap_size: 268_435_456,
        }
    }
}

/// Opens (creating it if needed) the read-write connection of a database.
///
/// Write transactions on it default to `BEGIN IMMEDIATE`, so a writer takes the
/// lock up front instead of failing to upgrade a read lock later.
pub(crate) fn open_writer(path: &Path, pragmas: &Pragmas) -> Result<Connection, DbError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(pragmas.busy_timeout)?;
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(DbError::JournalMode(mode));
    }
    apply_common(&conn, pragmas, pragmas.writer_cache_kib)?;
    conn.set_transaction_behavior(TransactionBehavior::Immediate);
    Ok(conn)
}

/// Opens a read-only connection. The database must already exist in WAL mode,
/// which the writer guarantees by being opened first.
pub(crate) fn open_reader(path: &Path, pragmas: &Pragmas) -> Result<Connection, DbError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(pragmas.busy_timeout)?;
    apply_common(&conn, pragmas, pragmas.reader_cache_kib)?;
    Ok(conn)
}

fn apply_common(conn: &Connection, pragmas: &Pragmas, cache_kib: u32) -> Result<(), DbError> {
    // Per connection; it only matters for writes, but every connection gets the
    // same settings so a reader-turned-writer can never run with FULL.
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "cache_size", -i64::from(cache_kib))?;
    // `PRAGMA mmap_size` answers with the size it actually applied, so it is
    // read back rather than set with a plain update.
    let mmap = i64::try_from(pragmas.mmap_size).unwrap_or(i64::MAX);
    let _applied: i64 = conn.pragma_update_and_check(None, "mmap_size", mmap, |row| row.get(0))?;
    Ok(())
}
