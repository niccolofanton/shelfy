//! SQLite access (plan §2.3): the per-user [`UserDb`], the server-wide
//! [`ControlDb`] and the [`UserDbCache`] of open user databases.
//!
//! Every database has one writer connection and a few read-only connections.
//! [`UserDb::write`] runs a closure in a `BEGIN IMMEDIATE` transaction on the
//! writer (callers are serialized by a mutex); [`UserDb::read`] runs a closure in
//! a read transaction on a reader, so all its queries see one snapshot. In WAL
//! mode readers never block the writer and the writer never blocks readers.
//!
//! The API is synchronous, like the rest of `shelfy-core`: the server calls it
//! from `spawn_blocking`. Holding the writer is therefore cheap to wait for
//! (writes are short); if writers ever queue up, the server can put an async
//! permit in front of [`UserDb::write`] so waiting tasks do not each hold a
//! blocking thread.

mod cache;
mod conn;
mod pool;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, Transaction};

pub use cache::{LIBRARY_FILE_NAME, UserDbCache, UserDbCacheConfig};
pub use conn::Pragmas;

use crate::schema::Kind;
use pool::{Database, PoolConfig};

/// Errors from opening or using a database.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// SQLite reported an error.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// A schema migration failed, or the file is newer than this build.
    #[error(transparent)]
    Migration(#[from] rusqlite_migration::Error),
    /// The file system refused an operation (for example creating a user directory).
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The file is a SQLite database of another application or kind.
    #[error("not a Shelfy {expected} database (application_id {found})")]
    WrongApplication {
        /// The expected kind (`library` or `control`).
        expected: &'static str,
        /// The `application_id` found in the file.
        found: i32,
    },
    /// SQLite refused to switch to WAL mode (for example on a network file system).
    #[error("journal_mode is {0}, expected wal")]
    JournalMode(String),
    /// Every reader stayed busy for the whole wait timeout.
    #[error("timed out waiting for a reader connection")]
    ReaderTimeout,
    /// A user id that cannot name a directory safely.
    #[error("invalid user id")]
    InvalidUserId,
    /// Opening a database through the cache failed; concurrent callers share the error.
    #[error("{0}")]
    Open(Arc<DbError>),
}

/// Identifies the state of a database for cache validation (ETags, cached
/// counts): it changes whenever a write transaction changes at least one row.
///
/// `instance` is unique per opened handle, so counters restarting at zero after
/// a reopen or a server restart never repeat an earlier value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Generation {
    /// Random-ish id of the open handle.
    pub instance: u64,
    /// Number of committed write transactions that changed rows.
    pub counter: u64,
}

/// Settings of a [`UserDb`] (plan §2.3: up to 2 readers, closed after 60 s idle).
#[derive(Clone, Debug)]
pub struct UserDbConfig {
    /// Maximum reader connections (at least 1).
    pub max_readers: usize,
    /// A reader unused for this long is closed.
    pub reader_idle_timeout: Duration,
    /// How long [`UserDb::read`] waits when every reader is busy.
    pub reader_wait_timeout: Duration,
    /// Connection settings.
    pub pragmas: Pragmas,
}

impl Default for UserDbConfig {
    fn default() -> Self {
        Self {
            max_readers: 2,
            reader_idle_timeout: Duration::from_secs(60),
            reader_wait_timeout: Duration::from_secs(5),
            pragmas: Pragmas::default(),
        }
    }
}

/// Settings of the [`ControlDb`] (plan §2.3: 1 writer and 4 readers, always open).
#[derive(Clone, Debug)]
pub struct ControlDbConfig {
    /// Reader connections, all opened up front and never closed (at least 1).
    pub readers: usize,
    /// How long [`ControlDb::read`] waits when every reader is busy.
    pub reader_wait_timeout: Duration,
    /// Connection settings.
    pub pragmas: Pragmas,
}

impl Default for ControlDbConfig {
    fn default() -> Self {
        Self {
            readers: 4,
            reader_wait_timeout: Duration::from_secs(5),
            pragmas: Pragmas::default(),
        }
    }
}

/// One user's `library.sqlite`.
///
/// Readers open lazily and close after [`UserDbConfig::reader_idle_timeout`];
/// call [`UserDb::prune_idle_readers`] periodically (the [`UserDbCache`] does it
/// in [`UserDbCache::run_maintenance`]) so idle readers also close when no
/// further read arrives.
pub struct UserDb(Database);

impl UserDb {
    /// Opens (creating it if missing) and migrates a library database.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be opened in WAL mode, belongs to another
    /// application, is newer than this build, or a migration fails.
    pub fn open(path: impl AsRef<Path>, config: &UserDbConfig) -> Result<Self, DbError> {
        let pool = PoolConfig {
            max_readers: config.max_readers,
            reader_idle_timeout: Some(config.reader_idle_timeout),
            eager_readers: false,
            reader_wait_timeout: config.reader_wait_timeout,
            pragmas: config.pragmas.clone(),
        };
        Database::open(path.as_ref(), Kind::Library, pool).map(Self)
    }

    /// Runs `f` in a write transaction and commits it; an `Err` rolls back.
    ///
    /// # Errors
    ///
    /// Returns `f`'s error, or a [`DbError`] when the transaction cannot start
    /// or commit.
    pub fn write<T, E>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<DbError>,
    {
        self.0.write(f)
    }

    /// Runs `f` in a read transaction on a reader connection.
    ///
    /// # Errors
    ///
    /// Returns `f`'s error, or a [`DbError`] when no reader is available within
    /// the wait timeout or the transaction fails.
    pub fn read<T, E>(&self, f: impl FnOnce(&Connection) -> Result<T, E>) -> Result<T, E>
    where
        E: From<DbError>,
    {
        self.0.read(f)
    }

    /// The current [`Generation`].
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.0.generation()
    }

    /// Path of the database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.0.path()
    }

    /// Runs `PRAGMA wal_checkpoint(TRUNCATE)`.
    ///
    /// # Errors
    ///
    /// Fails when the writer cannot be reopened or the pragma fails.
    pub fn checkpoint(&self) -> Result<(), DbError> {
        self.0.checkpoint()
    }

    /// Closes readers idle for longer than the idle timeout.
    pub fn prune_idle_readers(&self) {
        self.0.prune_idle_readers();
    }

    /// Closes every connection that is not in use, checkpointing the WAL first.
    /// The handle stays valid: the next call reopens what it needs.
    pub fn release(&self) {
        self.0.release();
    }

    /// Open connections, for metrics and tests: `(writer open, readers open)`.
    #[must_use]
    pub fn open_connections(&self) -> (bool, usize) {
        self.0.open_connections()
    }
}

/// The server's `control.sqlite`.
pub struct ControlDb(Database);

impl ControlDb {
    /// Opens (creating it if missing) and migrates the control database, and
    /// opens all of its readers.
    ///
    /// # Errors
    ///
    /// Fails like [`UserDb::open`].
    pub fn open(path: impl AsRef<Path>, config: &ControlDbConfig) -> Result<Self, DbError> {
        let pool = PoolConfig {
            max_readers: config.readers,
            reader_idle_timeout: None,
            eager_readers: true,
            reader_wait_timeout: config.reader_wait_timeout,
            pragmas: config.pragmas.clone(),
        };
        Database::open(path.as_ref(), Kind::Control, pool).map(Self)
    }

    /// Runs `f` in a write transaction and commits it; an `Err` rolls back.
    ///
    /// # Errors
    ///
    /// Like [`UserDb::write`].
    pub fn write<T, E>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<DbError>,
    {
        self.0.write(f)
    }

    /// Runs `f` in a read transaction on a reader connection.
    ///
    /// # Errors
    ///
    /// Like [`UserDb::read`].
    pub fn read<T, E>(&self, f: impl FnOnce(&Connection) -> Result<T, E>) -> Result<T, E>
    where
        E: From<DbError>,
    {
        self.0.read(f)
    }

    /// The current [`Generation`].
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.0.generation()
    }

    /// Path of the database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.0.path()
    }

    /// Runs `PRAGMA wal_checkpoint(TRUNCATE)`.
    ///
    /// # Errors
    ///
    /// Like [`UserDb::checkpoint`].
    pub fn checkpoint(&self) -> Result<(), DbError> {
        self.0.checkpoint()
    }

    /// Open connections, for metrics and tests: `(writer open, readers open)`.
    #[must_use]
    pub fn open_connections(&self) -> (bool, usize) {
        self.0.open_connections()
    }
}
