//! One SQLite database file behind one writer connection and a small pool of
//! read-only connections (plan §2.3).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::conn::{Pragmas, open_reader, open_writer};
use super::{DbError, Generation};
use crate::schema::{self, Kind, Upgrade};

/// How a [`Database`] manages its connections.
#[derive(Clone, Debug)]
pub(crate) struct PoolConfig {
    pub max_readers: usize,
    /// Close a reader after this long unused; `None` keeps readers open.
    pub reader_idle_timeout: Option<Duration>,
    /// Open every reader at start instead of on demand.
    pub eager_readers: bool,
    /// How long `read` waits for a reader when all of them are busy.
    pub reader_wait_timeout: Duration,
    pub pragmas: Pragmas,
}

struct IdleReader {
    conn: Connection,
    since: Instant,
}

#[derive(Default)]
struct ReaderState {
    /// Idle connections, most recently used last.
    idle: Vec<IdleReader>,
    /// Open readers, idle or checked out.
    open: usize,
}

/// A database file with its connections. `UserDb` and `ControlDb` wrap it.
pub(crate) struct Database {
    path: PathBuf,
    kind: Kind,
    config: PoolConfig,
    /// `None` after `release` closed it; reopened on the next use.
    writer: Mutex<Option<Connection>>,
    readers: Mutex<ReaderState>,
    reader_returned: Condvar,
    instance: u64,
    generation: AtomicU64,
    /// What opening did to the schema.
    upgrade: Upgrade,
}

/// Process-unique instance ids: the open time in nanoseconds plus a counter, so
/// ids differ across instances in one process and, in practice, across restarts.
fn next_instance_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    #[allow(clippy::cast_possible_truncation)] // the low 64 bits are enough
    let nanos = nanos as u64;
    nanos.wrapping_add(
        COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15),
    )
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic inside a closure drops (and so rolls back) its transaction before
    // the guard is released, which leaves the connection usable: recover.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Database {
    /// Opens the writer, checks the file belongs to `kind`, upgrades its
    /// schema ([`schema::upgrade`]), and opens the readers when they are eager.
    pub(crate) fn open(path: &Path, kind: Kind, config: PoolConfig) -> Result<Self, DbError> {
        assert!(
            config.max_readers > 0,
            "a database needs at least one reader"
        );
        let mut writer = open_writer(path, &config.pragmas)?;
        check_application_id(&writer, kind)?;
        let upgrade = schema::upgrade(&mut writer, kind)?;
        let db = Self {
            path: path.to_path_buf(),
            kind,
            config,
            writer: Mutex::new(Some(writer)),
            readers: Mutex::new(ReaderState::default()),
            reader_returned: Condvar::new(),
            instance: next_instance_id(),
            generation: AtomicU64::new(0),
            upgrade,
        };
        if db.config.eager_readers {
            let mut readers = Vec::with_capacity(db.config.max_readers);
            for _ in 0..db.config.max_readers {
                readers.push(open_reader(&db.path, &db.config.pragmas)?);
            }
            let mut state = lock(&db.readers);
            let now = Instant::now();
            state.open = readers.len();
            state.idle = readers
                .into_iter()
                .map(|conn| IdleReader { conn, since: now })
                .collect();
        }
        Ok(db)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn upgrade(&self) -> Upgrade {
        self.upgrade
    }

    pub(crate) fn generation(&self) -> Generation {
        Generation {
            instance: self.instance,
            counter: self.generation.load(Ordering::Acquire),
        }
    }

    /// Runs `f` in a write transaction (`BEGIN IMMEDIATE`) on the writer
    /// connection and commits it. An `Err` from `f` rolls the transaction back.
    pub(crate) fn write<T, E>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<DbError>,
    {
        let mut slot = lock(&self.writer);
        let conn = self.writer_conn(&mut slot)?;
        let before = conn.total_changes();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(DbError::from)?;
        let out = f(&tx)?;
        tx.commit().map_err(DbError::from)?;
        if conn.total_changes() != before {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        Ok(out)
    }

    /// Runs `f` in a read transaction on a reader connection, so every query in
    /// `f` sees the same snapshot.
    pub(crate) fn read<T, E>(&self, f: impl FnOnce(&Connection) -> Result<T, E>) -> Result<T, E>
    where
        E: From<DbError>,
    {
        let mut reader = self.checkout()?;
        let tx = reader
            .conn_mut()
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(DbError::from)?;
        let out = f(&tx)?;
        tx.commit().map_err(DbError::from)?;
        Ok(out)
    }

    /// Runs `PRAGMA wal_checkpoint(TRUNCATE)` on the writer.
    pub(crate) fn checkpoint(&self) -> Result<(), DbError> {
        let mut slot = lock(&self.writer);
        let conn = self.writer_conn(&mut slot)?;
        checkpoint_truncate(conn)?;
        Ok(())
    }

    /// Closes readers that have been idle longer than the idle timeout.
    pub(crate) fn prune_idle_readers(&self) {
        let Some(timeout) = self.config.reader_idle_timeout else {
            return;
        };
        let mut state = lock(&self.readers);
        prune(&mut state, timeout, Instant::now());
    }

    /// Releases the connections, as the handle cache does on eviction: idle
    /// readers are closed and, when no reader is checked out and no write is
    /// running, the writer runs `PRAGMA optimize` and a TRUNCATE checkpoint and
    /// is closed. Anything busy is left alone and closed on drop instead. A later
    /// call on this handle reopens what it needs.
    pub(crate) fn release(&self) {
        let readers_busy = {
            let mut state = lock(&self.readers);
            state.open -= state.idle.len();
            state.idle.clear();
            state.open > 0
        };
        if readers_busy {
            return;
        }
        let mut slot = match self.writer.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        if let Some(conn) = slot.take() {
            close_writer(conn);
        }
    }

    /// Open connections: `(writer, readers)`.
    pub(crate) fn open_connections(&self) -> (bool, usize) {
        let writer = match self.writer.try_lock() {
            Ok(slot) => slot.is_some(),
            Err(TryLockError::Poisoned(p)) => p.into_inner().is_some(),
            // A write is running, so the writer is open.
            Err(TryLockError::WouldBlock) => true,
        };
        let readers = lock(&self.readers).open;
        (writer, readers)
    }

    fn writer_conn<'a>(
        &self,
        slot: &'a mut Option<Connection>,
    ) -> Result<&'a mut Connection, DbError> {
        if slot.is_none() {
            // Reopened after `release`; the schema was migrated at first open.
            let conn = open_writer(&self.path, &self.config.pragmas)?;
            check_application_id(&conn, self.kind)?;
            *slot = Some(conn);
        }
        Ok(slot.as_mut().expect("writer was just opened"))
    }

    fn checkout(&self) -> Result<ReaderGuard<'_>, DbError> {
        let deadline = Instant::now() + self.config.reader_wait_timeout;
        let mut state = lock(&self.readers);
        loop {
            if let Some(timeout) = self.config.reader_idle_timeout {
                prune(&mut state, timeout, Instant::now());
            }
            if let Some(idle) = state.idle.pop() {
                return Ok(ReaderGuard {
                    db: self,
                    conn: Some(idle.conn),
                });
            }
            if state.open < self.config.max_readers {
                state.open += 1;
                drop(state);
                return match self.open_new_reader() {
                    Ok(conn) => Ok(ReaderGuard {
                        db: self,
                        conn: Some(conn),
                    }),
                    Err(e) => {
                        lock(&self.readers).open -= 1;
                        self.reader_returned.notify_one();
                        Err(e)
                    }
                };
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(DbError::ReaderTimeout);
            }
            state = self
                .reader_returned
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn open_new_reader(&self) -> Result<Connection, DbError> {
        // A read-only connection cannot create the WAL index, so the writer must
        // be open (`release` may have closed it). `try_lock` keeps a read issued
        // from inside a write closure from deadlocking on the writer's mutex.
        match self.writer.try_lock() {
            Ok(mut slot) => {
                self.writer_conn(&mut slot)?;
            }
            Err(TryLockError::Poisoned(p)) => {
                self.writer_conn(&mut p.into_inner())?;
            }
            // A write is running, so the writer is open.
            Err(TryLockError::WouldBlock) => {}
        }
        open_reader(&self.path, &self.config.pragmas)
    }

    fn checkin(&self, conn: Connection) {
        let mut state = lock(&self.readers);
        state.idle.push(IdleReader {
            conn,
            since: Instant::now(),
        });
        drop(state);
        self.reader_returned.notify_one();
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let state = self
            .readers
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        state.idle.clear();
        state.open = 0;
        if let Some(conn) = self
            .writer
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            close_writer(conn);
        }
    }
}

/// A checked-out reader; returns to the pool on drop.
struct ReaderGuard<'a> {
    db: &'a Database,
    conn: Option<Connection>,
}

impl ReaderGuard<'_> {
    fn conn_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("reader guard holds a connection")
    }
}

impl Drop for ReaderGuard<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.db.checkin(conn);
        }
    }
}

fn prune(state: &mut ReaderState, timeout: Duration, now: Instant) {
    // `idle` is ordered by last use, so expired readers form a prefix.
    let expired = state
        .idle
        .iter()
        .take_while(|r| now.duration_since(r.since) >= timeout)
        .count();
    if expired > 0 {
        state.idle.drain(..expired);
        state.open -= expired;
    }
}

fn checkpoint_truncate(conn: &Connection) -> rusqlite::Result<()> {
    // Returns (busy, log frames, checkpointed frames); a busy checkpoint is not
    // an error, the WAL is simply not truncated this time.
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
}

fn close_writer(conn: Connection) {
    // Best effort: closing must not fail the caller, and SQLite checkpoints on
    // the last close anyway.
    let _ = conn.execute_batch("PRAGMA optimize");
    let _ = checkpoint_truncate(&conn);
    let _ = conn.close();
}

/// Refuses files that are not a `kind` database. A brand-new file (no schema,
/// no application id) is accepted: the first migration stamps it.
fn check_application_id(conn: &Connection, kind: Kind) -> Result<(), DbError> {
    let found: i32 = conn.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if found == kind.application_id() {
        return Ok(());
    }
    let empty = found == 0
        && !conn.query_row("SELECT EXISTS (SELECT 1 FROM sqlite_schema)", [], |row| {
            row.get::<_, bool>(0)
        })?;
    if empty {
        Ok(())
    } else {
        Err(DbError::WrongApplication {
            expected: kind.name(),
            found,
        })
    }
}
