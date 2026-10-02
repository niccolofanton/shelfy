//! The cache of open user databases (plan §2.3: at most 64 open, 10-minute
//! time-to-idle, eviction checkpoints and closes).
//!
//! It is a `moka::sync::Cache` with the LRU eviction policy. moka gives what a
//! hand-written LRU would have to reimplement: a concurrent map, time-to-idle
//! expiry, and coalesced initialization, so concurrent first requests for one
//! user open the database once. LRU (instead of moka's default TinyLFU
//! admission) always admits the newly opened database, which is the right call
//! for handles: a rejected entry would be reopened on every request.
//!
//! On eviction the listener calls [`UserDb::release`], which checkpoints and
//! closes the connections that are not in use. A request still holding the
//! evicted handle keeps working (it reopens what it needs), and the handle
//! closes for good when its last `Arc` drops.
//!
//! moka evicts lazily, during cache operations. The capacity can be exceeded
//! briefly and an idle entry outlives its time-to-idle until the next
//! operation, so the server calls [`UserDbCache::run_maintenance`] on a timer
//! (every 30–60 s), which also closes idle readers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use moka::policy::EvictionPolicy;
use moka::sync::Cache;

use super::{DbError, UserDb, UserDbConfig};

/// File name of a user's library inside `<users_dir>/<user_id>/` (plan §2.5).
pub const LIBRARY_FILE_NAME: &str = "library.sqlite";

/// Limits of a [`UserDbCache`].
#[derive(Clone, Debug)]
pub struct UserDbCacheConfig {
    /// Maximum open user databases.
    pub max_open: u64,
    /// A database not used for this long is closed.
    pub time_to_idle: Duration,
}

impl Default for UserDbCacheConfig {
    fn default() -> Self {
        Self {
            max_open: 64,
            time_to_idle: Duration::from_secs(600),
        }
    }
}

/// Open user databases, keyed by user id.
pub struct UserDbCache {
    users_dir: PathBuf,
    db_config: UserDbConfig,
    cache: Cache<String, Arc<UserDb>>,
}

impl UserDbCache {
    /// A cache of the libraries under `users_dir` (`/data/shelfy/users`).
    #[must_use]
    pub fn new(
        users_dir: impl Into<PathBuf>,
        config: &UserDbCacheConfig,
        db_config: UserDbConfig,
    ) -> Self {
        let cache = Cache::builder()
            .max_capacity(config.max_open)
            .time_to_idle(config.time_to_idle)
            .eviction_policy(EvictionPolicy::lru())
            .eviction_listener(|_user, db: Arc<UserDb>, _cause| db.release())
            .build();
        Self {
            users_dir: users_dir.into(),
            db_config,
            cache,
        }
    }

    /// Path of a user's `library.sqlite`.
    ///
    /// # Errors
    ///
    /// [`DbError::InvalidUserId`] unless `user_id` is 1–64 ASCII letters and
    /// digits (a ULID qualifies), which keeps it a single, safe path component.
    pub fn library_path(&self, user_id: &str) -> Result<PathBuf, DbError> {
        validate_user_id(user_id)?;
        Ok(self.users_dir.join(user_id).join(LIBRARY_FILE_NAME))
    }

    /// The open database of `user_id`, opening (and creating and migrating) it
    /// when needed.
    ///
    /// # Errors
    ///
    /// [`DbError::InvalidUserId`], or [`DbError::Open`] wrapping the open error.
    pub fn get(&self, user_id: &str) -> Result<Arc<UserDb>, DbError> {
        let path = self.library_path(user_id)?;
        self.cache
            .try_get_with_by_ref(user_id, || open(&path, &self.db_config))
            .map_err(DbError::Open)
    }

    /// Evicts and releases `user_id`'s database, for example before replacing
    /// or deleting its files.
    pub fn evict(&self, user_id: &str) {
        self.cache.invalidate(user_id);
    }

    /// Applies pending evictions (capacity and time-to-idle) and closes readers
    /// that have been idle too long.
    pub fn run_maintenance(&self) {
        self.cache.run_pending_tasks();
        for (_, db) in &self.cache {
            db.prune_idle_readers();
        }
    }

    /// Number of cached databases (approximate until maintenance runs).
    #[must_use]
    pub fn len(&self) -> u64 {
        self.cache.entry_count()
    }

    /// Whether no database is cached (approximate, like [`UserDbCache::len`]).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn open(path: &Path, config: &UserDbConfig) -> Result<Arc<UserDb>, DbError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    UserDb::open(path, config).map(Arc::new)
}

fn validate_user_id(user_id: &str) -> Result<(), DbError> {
    let ok =
        (1..=64).contains(&user_id.len()) && user_id.bytes().all(|b| b.is_ascii_alphanumeric());
    if ok {
        Ok(())
    } else {
        Err(DbError::InvalidUserId)
    }
}
