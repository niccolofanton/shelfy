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
//! (every 30–60 s), which also closes idle readers and releases the libraries
//! of locked users.
//!
//! **Schema upgrades** (plan §3.8). A library is upgraded when it is opened:
//! lazily, by the first request that needs it ([`UserDbCache::get`], which
//! reports the upgrade to the [`UpgradeListener`]), or by the server's sweep
//! after boot, one library at a time ([`UserDbCache::upgrade`]).
//!
//! **Locks** (plan §3.5). A library locked for maintenance
//! ([`super::lock_library`]) is never opened through the cache.
//!
//! **Generations** ([`crate::generation`]). Every handle the cache opens on
//! a user's library shares the user's [`Generation`] cell: a write through a
//! handle that the cache has since evicted for idleness or capacity still
//! moves the generation the API's ETags read. [`UserDbCache::evict`] and a
//! lock retire the cell instead (the library may be replaced from another
//! connection): the next handle starts a new generation.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use moka::policy::EvictionPolicy;
use moka::sync::Cache;

use super::lock::is_library_locked;
use super::{DbError, Generation, UserDb, UserDbConfig};
use crate::generation::{GenerationCell, Generations};
use crate::schema::{Kind, Upgrade};

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

/// Called with the user id when [`UserDbCache::get`] opens a library whose
/// schema changed or is ahead of this build (anything but
/// [`Upgrade::Current`]). It runs while the open is in progress: keep it
/// short (a log line).
pub type UpgradeListener = Arc<dyn Fn(&str, Upgrade) + Send + Sync>;

/// What [`UserDbCache::upgrade`] found for one library.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LibraryUpgrade {
    /// At this build's latest version, or already open (and so upgraded).
    Current,
    /// Migrated from `from` to `to`.
    Upgraded {
        /// The version before.
        from: usize,
        /// The version after.
        to: usize,
    },
    /// Written by a newer release; used as is ([`Upgrade::Ahead`]).
    Ahead {
        /// The file's version.
        found: usize,
    },
    /// Locked for maintenance: left alone.
    Locked,
    /// The user has no library.
    Missing,
}

/// Open user databases, keyed by user id.
pub struct UserDbCache {
    users_dir: PathBuf,
    db_config: UserDbConfig,
    cache: Cache<String, Arc<UserDb>>,
    on_upgrade: Option<UpgradeListener>,
    generations: Generations,
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
            on_upgrade: None,
            generations: Generations::new(),
        }
    }

    /// Reports the schema upgrades of [`UserDbCache::get`] to `listener`.
    #[must_use]
    pub fn with_upgrade_listener(
        mut self,
        listener: impl Fn(&str, Upgrade) + Send + Sync + 'static,
    ) -> Self {
        self.on_upgrade = Some(Arc::new(listener));
        self
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

    /// Whether `user_id`'s library is locked for maintenance. One `stat`.
    ///
    /// # Errors
    ///
    /// [`DbError::InvalidUserId`], or the file system refused the check.
    pub fn is_locked(&self, user_id: &str) -> Result<bool, DbError> {
        is_library_locked(&self.users_dir, user_id)
    }

    /// The open database of `user_id`, opening (and creating and upgrading)
    /// it when needed.
    ///
    /// # Errors
    ///
    /// [`DbError::InvalidUserId`]; [`DbError::Locked`] while the library is
    /// locked, after releasing its cached handle; or [`DbError::Open`]
    /// wrapping the open error.
    pub fn get(&self, user_id: &str) -> Result<Arc<UserDb>, DbError> {
        let path = self.library_path(user_id)?;
        if self.is_locked(user_id)? {
            self.release_locked(user_id);
            return Err(DbError::Locked);
        }
        self.cache
            .try_get_with_by_ref(user_id, || {
                let db = open(&path, &self.db_config, self.generations.cell(user_id))?;
                let upgrade = db.schema_upgrade();
                if let Some(listener) = &self.on_upgrade
                    && upgrade != Upgrade::Current
                {
                    listener(user_id, upgrade);
                }
                Ok(db)
            })
            .map_err(DbError::Open)
    }

    /// The cached database of `user_id`, without opening it when it is not
    /// cached. Lets a caller check whether the handle it used is still the one
    /// the cache serves (the server's job system does, after each chunk of
    /// work, so that its writes always move the [`Generation`] that the API's
    /// ETags read).
    ///
    /// [`Generation`]: super::Generation
    #[must_use]
    pub fn get_if_present(&self, user_id: &str) -> Option<Arc<UserDb>> {
        self.cache.get(user_id)
    }

    /// The current generation of `user_id`'s library when some handle on it
    /// is open, without opening anything.
    #[must_use]
    pub fn generation(&self, user_id: &str) -> Option<Generation> {
        self.generations.current(user_id)
    }

    /// Upgrades `user_id`'s library if it is behind this build: one step of
    /// the sweep after boot.
    ///
    /// A library that is open is current (opening upgraded it). Otherwise the
    /// version is read from the file header first, so a current library costs
    /// one small read; an older one is opened outside the cache (the sweep
    /// must not evict the handles of active users), upgraded and closed. A
    /// request that opens the same library meanwhile is safe: migrations take
    /// the write lock first, so the second opener finds nothing left to do.
    ///
    /// # Errors
    ///
    /// [`DbError::InvalidUserId`], or the open or the migration failed.
    pub fn upgrade(&self, user_id: &str) -> Result<LibraryUpgrade, DbError> {
        let path = self.library_path(user_id)?;
        if self.is_locked(user_id)? {
            return Ok(LibraryUpgrade::Locked);
        }
        if !path.is_file() {
            return Ok(LibraryUpgrade::Missing);
        }
        if self.cache.contains_key(user_id) {
            return Ok(LibraryUpgrade::Current);
        }
        let latest = Kind::Library.latest_version();
        match header_version(&path)? {
            Some(found) if found == latest => return Ok(LibraryUpgrade::Current),
            Some(found) if found > latest => {
                // Opening checks the compat floor of a newer file.
                drop(UserDb::open(&path, &self.db_config)?);
                return Ok(LibraryUpgrade::Ahead { found });
            }
            _ => {}
        }
        let db = UserDb::open(&path, &self.db_config)?;
        let upgrade = db.schema_upgrade();
        drop(db);
        Ok(match upgrade {
            Upgrade::Current => LibraryUpgrade::Current,
            Upgrade::Upgraded { from, to } => LibraryUpgrade::Upgraded { from, to },
            Upgrade::Ahead { found } => LibraryUpgrade::Ahead { found },
        })
    }

    /// Evicts and releases `user_id`'s database, for example after replacing
    /// its content from another connection or before deleting its files, and
    /// retires its generation ([`Generations::retire`]): no ETag or cached
    /// count taken before matches after, not even one a handle still held
    /// computes.
    pub fn evict(&self, user_id: &str) {
        self.cache.invalidate(user_id);
        self.generations.retire(user_id);
    }

    /// Releases the cached handle of a locked library and retires its
    /// generation: the library may be replaced while it is locked.
    fn release_locked(&self, user_id: &str) {
        self.cache.invalidate(user_id);
        self.generations.retire(user_id);
    }

    /// Applies pending evictions (capacity and time-to-idle), closes readers
    /// that have been idle too long, releases the databases of locked users,
    /// and forgets the generation of libraries no handle holds any more.
    /// Blocking: one `stat` per open database.
    pub fn run_maintenance(&self) {
        self.cache.run_pending_tasks();
        let mut locked = Vec::new();
        for (user, db) in &self.cache {
            if self.is_locked(&user).unwrap_or(false) {
                locked.push(user);
            } else {
                db.prune_idle_readers();
            }
        }
        for user in locked {
            self.release_locked(user.as_str());
        }
        // moka also lets go of an evicted handle on this run of pending tasks.
        self.cache.run_pending_tasks();
        self.generations.prune();
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

    /// Whether `user_id`'s database is in the cache.
    #[must_use]
    pub fn is_open(&self, user_id: &str) -> bool {
        self.cache.contains_key(user_id)
    }
}

fn open(
    path: &Path,
    config: &UserDbConfig,
    generation: Arc<GenerationCell>,
) -> Result<Arc<UserDb>, DbError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    UserDb::open_with_generation(path, config, generation).map(Arc::new)
}

/// Whether `user_id` can name a user's directory: 1–64 ASCII letters and
/// digits (a ULID qualifies), so it is a single, safe path component.
#[must_use]
pub fn is_valid_user_id(user_id: &str) -> bool {
    (1..=64).contains(&user_id.len()) && user_id.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub(super) fn validate_user_id(user_id: &str) -> Result<(), DbError> {
    if is_valid_user_id(user_id) {
        Ok(())
    } else {
        Err(DbError::InvalidUserId)
    }
}

/// The ids of the users with a library under `users_dir`, sorted. Entries
/// that are not valid user ids are ignored.
///
/// # Errors
///
/// The directory cannot be listed. A missing directory has no libraries.
pub fn library_ids(users_dir: &Path) -> io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(users_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry?;
        if let Some(id) = entry.file_name().to_str()
            && is_valid_user_id(id)
            && entry.path().join(LIBRARY_FILE_NAME).is_file()
        {
            ids.push(id.to_owned());
        }
    }
    ids.sort();
    Ok(ids)
}

/// The schema version in the header of the SQLite file at `path`, without
/// opening it: `None` when the file is shorter than a header or not SQLite.
///
/// The header can lag behind a write-ahead log that was not checkpointed
/// yet; versions only grow, so a lagging header only makes the sweep open a
/// library that turns out to be current.
fn header_version(path: &Path) -> io::Result<Option<usize>> {
    let mut header = [0_u8; 100];
    match File::open(path)?.read_exact(&mut header) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    if &header[..16] != b"SQLite format 3\0" {
        return Ok(None);
    }
    let version = u32::from_be_bytes([header[60], header[61], header[62], header[63]]);
    Ok(usize::try_from(version).ok())
}
