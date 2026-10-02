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
//!
//! Opening a database upgrades its schema ([`crate::schema::upgrade`]); a
//! library can also be locked for maintenance ([`lock_library`]), which
//! [`UserDb`] and the [`UserDbCache`] enforce.

mod cache;
mod conn;
mod lock;
mod pool;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rusqlite::{Connection, Transaction};

pub use crate::generation::Generation;
pub use cache::{
    LIBRARY_FILE_NAME, LibraryUpgrade, UpgradeListener, UserDbCache, UserDbCacheConfig,
    is_valid_user_id, library_ids,
};
pub use conn::Pragmas;
pub use lock::{
    LOCK_FILE_NAME, is_library_file_locked, is_library_locked, library_lock_path, lock_library,
    unlock_library,
};

use crate::generation::GenerationCell;
use crate::schema::{Kind, Upgrade};
use pool::{Database, PoolConfig};

/// Errors from opening or using a database.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// SQLite reported an error. Build it with `DbError::from`, which counts
    /// the lock failures ([`sqlite_busy_total`]).
    #[error(transparent)]
    Sqlite(rusqlite::Error),
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
    /// The user's library is locked for maintenance ([`lock_library`]).
    #[error("the library is locked for maintenance")]
    Locked,
    /// The file comes from a newer release whose migrations this build cannot
    /// run on ([`crate::schema::upgrade`]).
    #[error(
        "the {kind} database is at schema v{found} and needs a build that supports v{needs}; \
         this build supports up to v{supported}"
    )]
    SchemaTooNew {
        /// `library` or `control`.
        kind: &'static str,
        /// The file's version.
        found: usize,
        /// The oldest supported version a build needs (`schema_compat`).
        needs: usize,
        /// This build's latest version.
        supported: usize,
    },
    /// Opening a database through the cache failed; concurrent callers share the error.
    #[error("{0}")]
    Open(Arc<DbError>),
}

impl DbError {
    /// Whether this is [`DbError::Locked`], directly or through the cache.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        match self {
            Self::Locked => true,
            Self::Open(inner) => inner.is_locked(),
            _ => false,
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(err: rusqlite::Error) -> Self {
        if is_lock_failure(&err) {
            SQLITE_BUSY.fetch_add(1, Ordering::Relaxed);
        }
        Self::Sqlite(err)
    }
}

/// Whether SQLite gave up on a lock: `SQLITE_BUSY` or `SQLITE_LOCKED`.
fn is_lock_failure(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// See [`sqlite_busy_total`].
static SQLITE_BUSY: AtomicU64 = AtomicU64::new(0);

/// SQLite calls of this process that gave up on a lock, on any database:
/// `SQLITE_BUSY` (another connection held the lock for the whole
/// `busy_timeout`, 5 s) or `SQLITE_LOCKED`. Every such error passes through
/// `DbError::from` (a [`crate::repo::RepoError`] too), which counts it. The
/// server exports the count as `shelfy_sqlite_busy_total` (plan §3.6).
#[must_use]
pub fn sqlite_busy_total() -> u64 {
    SQLITE_BUSY.load(Ordering::Relaxed)
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
    /// Opens (creating it if missing) and upgrades a library database
    /// ([`crate::schema::upgrade`]).
    ///
    /// A library locked for maintenance ([`lock_library`]: its directory
    /// holds the marker) is refused, and so is reopening it after
    /// [`UserDb::release`] once it is locked.
    ///
    /// # Errors
    ///
    /// [`DbError::Locked`] for a locked library. Fails when the file cannot
    /// be opened in WAL mode, belongs to another application, comes from a
    /// release this build cannot run on, or a migration fails.
    pub fn open(path: impl AsRef<Path>, config: &UserDbConfig) -> Result<Self, DbError> {
        Self::open_with_generation(path, config, GenerationCell::new())
    }

    /// Opens a library like [`UserDb::open`], with `generation` as its
    /// generation: the [`UserDbCache`] gives every handle on one user's
    /// library the same cell ([`crate::generation`]).
    ///
    /// # Errors
    ///
    /// Like [`UserDb::open`].
    pub fn open_with_generation(
        path: impl AsRef<Path>,
        config: &UserDbConfig,
        generation: Arc<GenerationCell>,
    ) -> Result<Self, DbError> {
        let pool = PoolConfig {
            max_readers: config.max_readers,
            reader_idle_timeout: Some(config.reader_idle_timeout),
            eager_readers: false,
            reader_wait_timeout: config.reader_wait_timeout,
            pragmas: config.pragmas.clone(),
        };
        Database::open(path.as_ref(), Kind::Library, pool, generation).map(Self)
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

    /// The current [`Generation`] of the library: shared with the other
    /// handles on it when the [`UserDbCache`] opened this one.
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.0.generation()
    }

    /// Path of the database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.0.path()
    }

    /// What opening did to the schema.
    #[must_use]
    pub fn schema_upgrade(&self) -> Upgrade {
        self.0.upgrade()
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
    /// The handle stays valid: the next call reopens what it needs, or fails
    /// with [`DbError::Locked`] if the library was locked meanwhile.
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
        Database::open(path.as_ref(), Kind::Control, pool, GenerationCell::new()).map(Self)
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

    /// What opening did to the schema: the server migrates the control
    /// database at boot.
    #[must_use]
    pub fn schema_upgrade(&self) -> Upgrade {
        self.0.upgrade()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::RepoError;

    #[test]
    fn lock_failures_are_counted_on_their_way_to_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let config = UserDbConfig {
            pragmas: Pragmas {
                busy_timeout: Duration::from_millis(20),
                ..Pragmas::default()
            },
            ..UserDbConfig::default()
        };
        let db = UserDb::open(&path, &config).unwrap();
        // Another connection holds the write lock.
        let other = Connection::open(&path).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();

        // Tests running beside this one may count too: the counter only grows.
        let before = sqlite_busy_total();
        let err = db
            .write(|tx| tx.execute_batch("SELECT 1").map_err(DbError::from))
            .unwrap_err();
        let DbError::Sqlite(busy) = &err else {
            panic!("{err}")
        };
        assert!(is_lock_failure(busy), "{err}");
        let after_write = sqlite_busy_total();
        assert!(after_write > before, "{before} -> {after_write}");

        // A repository error passes through the same conversion.
        let nested = other.execute_batch("BEGIN IMMEDIATE").unwrap_err();
        let syntax = other.execute_batch("NOT SQL").unwrap_err();
        assert!(!is_lock_failure(&nested), "a nested BEGIN is a misuse");
        assert!(!is_lock_failure(&syntax));
        let blocked = Connection::open(&path).unwrap();
        blocked.busy_timeout(Duration::from_millis(20)).unwrap();
        let locked = blocked.execute_batch("BEGIN IMMEDIATE").unwrap_err();
        assert!(is_lock_failure(&locked), "{locked}");
        let repo = RepoError::from(locked);
        assert!(matches!(repo, RepoError::Db(DbError::Sqlite(_))));
        assert!(sqlite_busy_total() > after_write);

        other.execute_batch("ROLLBACK").unwrap();
        db.write(|tx| tx.execute_batch("SELECT 1").map_err(DbError::from))
            .unwrap();
    }
}
