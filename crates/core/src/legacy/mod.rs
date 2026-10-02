//! Read-only reader for desktop Shelfy libraries (`<userData>/shelfy.sqlite`).
//!
//! The desktop schema lives in `electron/db.ts` (`SCHEMA` plus the additive
//! `migrate()`); [`catalog`] mirrors it column by column. The reader:
//!
//! - opens the file read-only (`SQLITE_OPEN_READ_ONLY`, never creating it)
//!   with `PRAGMA query_only`, so no statement can write to it; a library
//!   that no connection holds (no `-wal` file) is opened immutable, so not
//!   even `-wal`/`-shm` side files appear ([`OpenMode`]);
//! - reads everything inside one read transaction, so every query sees the
//!   same snapshot even while the desktop app keeps writing (WAL);
//! - streams rows table by table as typed records ([`rows`]);
//! - tolerates older files: a table the file lacks reads as empty, and a
//!   column added by a later desktop migration reads as that migration's
//!   default.
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §4 and
//! `docs/web-port/spikes/01-legacy-mapping.md`.

pub mod catalog;
pub mod convert;
#[doc(hidden)]
pub mod fixture;
pub mod rows;
pub mod schema;
pub mod web;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

pub use rows::{
    CollectionRow, DownloadRow, Fields, JobRow, LegacyRecord, PostCollectionRow, PostEntityRow,
    PostFacetRow, PostMediaRow, PostRow, PostTagRow, TagAliasRow, TagClusterMembershipRow,
    TagClusterRow, WebSnapshotRow,
};
pub use schema::{ColumnStatus, Coverage, LegacySchema, TableStatus};

/// How long a read waits for a lock held by the desktop app.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors of the legacy reader.
#[derive(Debug, thiserror::Error)]
pub enum LegacyError {
    #[error("cannot open the desktop library read-only")]
    Open(#[source] rusqlite::Error),
    #[error("not a Shelfy desktop library: {0}")]
    NotALibrary(&'static str),
    #[error("cannot read the desktop library")]
    Read(#[from] rusqlite::Error),
}

/// How the file was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenMode {
    /// No `-wal` (or `-journal`) file next to the library: no connection has
    /// it open, so it is read as immutable, without locks and without
    /// creating `-wal`/`-shm` files.
    Immutable,
    /// A `-wal` file exists: the desktop app may be running (or exited
    /// uncleanly). The file is read as a shared read-only connection, which
    /// sees the WAL and takes one consistent snapshot; SQLite updates the
    /// shared `-shm` index as for any reader.
    SharedReadOnly,
}

/// A desktop library opened read-only.
pub struct LegacyDb {
    conn: Connection,
    path: PathBuf,
    mode: OpenMode,
    schema: LegacySchema,
}

impl std::fmt::Debug for LegacyDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacyDb")
            .field("path", &self.path)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl LegacyDb {
    /// Opens a desktop library read-only and starts the read snapshot.
    ///
    /// Fails if the file does not exist (it is never created), is not an
    /// SQLite database, or has no `posts(id, platform)` table.
    pub fn open(path: impl AsRef<Path>) -> Result<LegacyDb, LegacyError> {
        let path = path.as_ref().to_path_buf();
        let (conn, mode) = open_read_only(&path).map_err(LegacyError::Open)?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(LegacyError::Open)?;
        conn.execute_batch("PRAGMA query_only = ON; BEGIN DEFERRED;")
            .map_err(LegacyError::Open)?;
        let schema = match LegacySchema::introspect(&conn, header_journal_mode(&path)) {
            Ok(schema) => schema,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::NotADatabase =>
            {
                return Err(LegacyError::NotALibrary(
                    "the file is not an SQLite database",
                ));
            }
            Err(e) => return Err(LegacyError::Open(e)),
        };
        if !schema.has_column("posts", "id") || !schema.has_column("posts", "platform") {
            return Err(LegacyError::NotALibrary(
                "there is no posts(id, platform) table",
            ));
        }
        Ok(LegacyDb {
            conn,
            path,
            mode,
            schema,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn open_mode(&self) -> OpenMode {
        self.mode
    }

    /// The tables and columns the file actually has.
    pub fn schema(&self) -> &LegacySchema {
        &self.schema
    }

    /// True: the connection cannot write (checked by SQLite, not assumed).
    pub fn is_read_only(&self) -> bool {
        self.conn.is_readonly(rusqlite::MAIN_DB).unwrap_or(false)
    }

    /// `COUNT(*)` of a table of the file; 0 when the file lacks it.
    pub fn row_count(&self, table: &str) -> Result<u64, LegacyError> {
        if !self.schema.has_table(table) {
            return Ok(0);
        }
        let n: i64 =
            self.conn
                .query_row(&format!("SELECT COUNT(*) FROM {}", quote(table)), [], |r| {
                    r.get(0)
                })?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// For every column of a table of the file: how many values of each
    /// SQLite storage class (`typeof()`: `null`, `integer`, `real`, `text`,
    /// `blob`) it holds. One scan of the table; empty when the file lacks it.
    pub fn storage_classes(
        &self,
        table: &str,
    ) -> Result<BTreeMap<String, BTreeMap<String, u64>>, LegacyError> {
        let mut out: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        let Some(columns) = self.schema.columns(table) else {
            return Ok(out);
        };
        if columns.is_empty() {
            return Ok(out);
        }
        let select: Vec<String> = columns
            .iter()
            .map(|c| format!("typeof({})", quote(&c.name)))
            .collect();
        let sql = format!("SELECT {} FROM {}", select.join(", "), quote(table));
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            for (i, column) in columns.iter().enumerate() {
                let class: String = row.get(i)?;
                *out.entry(column.name.clone())
                    .or_default()
                    .entry(class)
                    .or_default() += 1;
            }
        }
        Ok(out)
    }

    /// Streams every row of `R::TABLE` in rowid order. A table the file lacks
    /// yields nothing.
    pub fn stream<R, E>(&self, mut f: impl FnMut(R) -> Result<(), E>) -> Result<(), E>
    where
        R: LegacyRecord,
        E: From<LegacyError>,
    {
        let Some(sql) = self.select_sql(R::TABLE) else {
            return Ok(());
        };
        let mut stmt = self.conn.prepare(&sql).map_err(LegacyError::from)?;
        let mut rows = stmt.query([]).map_err(LegacyError::from)?;
        while let Some(row) = rows.next().map_err(LegacyError::from)? {
            let mut fields = Fields::new(row);
            let record = R::read(&mut fields).map_err(LegacyError::from)?;
            debug_assert_eq!(
                fields.consumed(),
                row.as_ref().column_count(),
                "{} reads every selected column",
                R::TABLE
            );
            f(record)?;
        }
        Ok(())
    }

    /// Collects every row of `R::TABLE`. Prefer [`LegacyDb::stream`] for the
    /// large tables.
    pub fn read_all<R: LegacyRecord>(&self) -> Result<Vec<R>, LegacyError> {
        let mut out = Vec::new();
        self.stream(|r: R| {
            out.push(r);
            Ok::<_, LegacyError>(())
        })?;
        Ok(out)
    }

    /// `SELECT` of a catalog table in catalog column order; absent columns
    /// read as their migration default. `None` when the file lacks the table.
    fn select_sql(&self, table: &str) -> Option<String> {
        let spec = catalog::table(table)?;
        if !self.schema.has_table(table) {
            return None;
        }
        let columns: Vec<String> = spec
            .columns
            .iter()
            .map(|c| {
                if self.schema.has_column(table, c.name) {
                    quote(c.name)
                } else {
                    let absent = match c.presence {
                        catalog::Presence::Added { absent } => absent,
                        catalog::Presence::Base => "NULL",
                    };
                    format!("{absent} AS {}", quote(c.name))
                }
            })
            .collect();
        Some(format!(
            "SELECT {} FROM {} ORDER BY rowid",
            columns.join(", "),
            quote(table)
        ))
    }
}

impl Drop for LegacyDb {
    fn drop(&mut self) {
        // Ends the read snapshot; there is nothing to commit.
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

/// An SQL identifier, double-quoted.
fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// Opens `path` read-only, never creating it. Without a `-wal` or `-journal`
/// file next to it the library is opened immutable, so not even `-wal`/`-shm`
/// files get created; otherwise as a shared read-only connection.
fn open_read_only(path: &Path) -> rusqlite::Result<(Connection, OpenMode)> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let sidecar = |suffix: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name).exists()
    };
    if path.is_file()
        && !sidecar("-wal")
        && !sidecar("-journal")
        && let Some(uri) = immutable_uri(path)
    {
        let conn = Connection::open_with_flags(uri, flags | OpenFlags::SQLITE_OPEN_URI)?;
        return Ok((conn, OpenMode::Immutable));
    }
    let conn = Connection::open_with_flags(path, flags)?;
    Ok((conn, OpenMode::SharedReadOnly))
}

/// The journal mode recorded in the file header: byte 18, the file format
/// write version, is 2 for WAL and 1 for a rollback journal.
fn header_journal_mode(path: &Path) -> String {
    use std::io::Read;
    let mut header = [0u8; 19];
    let read = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut header));
    match (read, header[18]) {
        (Ok(()), 2) => "wal",
        (Ok(()), 1) => "rollback",
        _ => "unknown",
    }
    .to_owned()
}

/// `file:<absolute path>?immutable=1`, percent-encoded. `None` for a path that
/// is not valid UTF-8 (the caller then opens it without the URI).
fn immutable_uri(path: &Path) -> Option<String> {
    let absolute = std::path::absolute(path).ok()?;
    let mut text = absolute.to_str()?.replace('\\', "/");
    if !text.starts_with('/') {
        // A Windows drive path: file:///C:/…
        text.insert(0, '/');
    }
    let mut uri = String::from("file://");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~' | b':') {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str("?immutable=1");
    Some(uri)
}

#[cfg(test)]
mod tests;
