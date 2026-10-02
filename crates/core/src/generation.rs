//! The library generation (plan §2.9 conditional GET, §2.14 cached counts): a
//! per-user version of the library that moves with every committed write,
//! and the caches keyed by it.
//!
//! **What a generation is.** A [`Generation`] is a pair: `instance`, random
//! per [`GenerationCell`], and `counter`, the committed write transactions
//! that changed rows. Two equal generations of a user's library mean that no
//! write happened in between, so anything computed from the library (a
//! response's ETag, a count, the stats) is still valid. The server never
//! needs SQLite to tell.
//!
//! **One cell per user, not per handle.** Every handle that the
//! [`UserDbCache`] opens on a user's library shares the user's cell, which
//! [`Generations`] keeps by user id. A write through any of them moves the
//! generation that every reader sees: a request or a job that still holds a
//! handle the cache has since evicted for idleness or capacity (and
//! reopened) cannot write behind the back of the ETags (the *From T11* note
//! of P1-07). A cell outlives such evictions as long as some handle holds
//! it; once none does, it is never handed out again: the next open starts a
//! cell with a new `instance`, even before [`Generations::prune`] drops the
//! old one, so a counter that restarts never repeats an earlier pair, and a
//! library replaced while no handle held it (a restore) cannot come back
//! with the generation it had before. A restart of the process does the
//! same.
//!
//! **Replacing a library** (the migration install, a restore under a
//! maintenance lock) changes it through another connection, which no handle
//! sees as a write. [`UserDbCache::evict`] and the cache's lock checks
//! therefore *retire* the cell ([`Generations::retire`]): it moves on, so
//! nothing computed before matches, and is forgotten, so the next handle
//! starts a new instance that nothing computed by a handle still held can
//! match either.
//!
//! **Caches keyed by the generation.** A [`GenerationCache`] keeps values
//! computed from one state of a library, keyed by `(user, generation, view)`
//! where `view` is a digest of what was computed (a filter, the stats). A
//! write moves the generation, so every older entry becomes unreachable at
//! once: lookups only ever use the current generation. Unreachable entries
//! leave by the cache's bound on entries and its time-to-idle.
//!
//! **Ordering.** Read the generation *before* opening the read snapshot. A
//! write that commits in between makes the value newer than its key, which
//! costs one recomputation later; the reverse, a key newer than its value,
//! cannot happen: a write bumps the generation only after its commit.
//!
//! Writes from another process (an operator command on a live server) do not
//! move the generation; they must go through the server or evict.
//!
//! [`UserDbCache`]: crate::db::UserDbCache
//! [`UserDbCache::evict`]: crate::db::UserDbCache::evict

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use moka::sync::Cache;

/// The state of a library for cache validation (ETags, cached counts): it
/// changes whenever a write transaction changes at least one row.
///
/// `instance` is unique per [`GenerationCell`], so counters restarting at
/// zero in a new cell (a new process, or a library no handle held for a
/// while) never repeat an earlier value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Generation {
    /// Random-ish id of the cell.
    pub instance: u64,
    /// Number of committed write transactions that changed rows.
    pub counter: u64,
}

/// The live generation of one library, shared by every handle open on it.
#[derive(Debug)]
pub struct GenerationCell {
    instance: u64,
    counter: AtomicU64,
}

impl GenerationCell {
    /// A cell with a fresh instance and a zero counter.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            instance: next_instance_id(),
            counter: AtomicU64::new(0),
        })
    }

    /// The current generation.
    #[must_use]
    pub fn current(&self) -> Generation {
        Generation {
            instance: self.instance,
            counter: self.counter.load(Ordering::Acquire),
        }
    }

    /// Moves the generation on. Call it after a change committed, never
    /// before.
    pub fn bump(&self) {
        self.counter.fetch_add(1, Ordering::AcqRel);
    }
}

/// The cells of every user whose library some handle holds, by user id.
#[derive(Debug, Default)]
pub struct Generations {
    cells: Mutex<HashMap<String, Arc<GenerationCell>>>,
}

impl Generations {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn cells(&self) -> MutexGuard<'_, HashMap<String, Arc<GenerationCell>>> {
        // A panic cannot leave the map half-updated: recover.
        self.cells.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The cell of `user_id`: the one a handle on the user's library holds,
    /// else a new one. A handle opened on the library takes it and keeps it
    /// alive.
    ///
    /// A cell that no handle holds any more is not reused, even before
    /// [`Self::prune`] drops it: nothing can move it, but the library may
    /// have been replaced since its last handle closed.
    #[must_use]
    pub fn cell(&self, user_id: &str) -> Arc<GenerationCell> {
        let mut cells = self.cells();
        if let Some(cell) = cells.get(user_id)
            && is_held(cell)
        {
            return Arc::clone(cell);
        }
        let cell = GenerationCell::new();
        cells.insert(user_id.to_owned(), Arc::clone(&cell));
        cell
    }

    /// The current generation of `user_id`, if a handle holds the user's
    /// cell.
    #[must_use]
    pub fn current(&self, user_id: &str) -> Option<Generation> {
        self.cells()
            .get(user_id)
            .filter(|cell| is_held(cell))
            .map(|cell| cell.current())
    }

    /// Moves `user_id`'s generation on and forgets its cell: handles still
    /// holding the cell report the moved value, and the next handle opened
    /// gets a cell with a new instance. For a library whose content another
    /// connection replaced.
    pub fn retire(&self, user_id: &str) {
        if let Some(cell) = self.cells().remove(user_id) {
            cell.bump();
        }
    }

    /// Drops the cells that no handle holds any more; returns how many.
    pub fn prune(&self) -> usize {
        let mut cells = self.cells();
        let before = cells.len();
        cells.retain(|_, cell| is_held(cell));
        before - cells.len()
    }

    /// Users with a cell.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells().len()
    }

    /// Whether no user has a cell.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Whether a handle holds `cell`, a cell of the map: the map holds one
/// reference, and any other belongs to a handle (or to a caller of
/// [`Generations::cell`] about to open one).
fn is_held(cell: &Arc<GenerationCell>) -> bool {
    Arc::strong_count(cell) > 1
}

/// A digest of what a cached value was computed from (a normalized filter,
/// "the stats"), chosen by the caller: 128 bits of a cryptographic hash.
pub type ViewDigest = [u8; 16];

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    user: Box<str>,
    generation: Generation,
    view: ViewDigest,
}

/// Values computed from one state of a user's library (counts, stats), keyed
/// by `(user, generation, view)`. Bounded in entries, with a time-to-idle;
/// entries of older generations are never returned (see the module docs).
pub struct GenerationCache<V> {
    cache: Cache<CacheKey, V>,
}

impl<V: Clone + Send + Sync + 'static> GenerationCache<V> {
    /// A cache of at most `max_entries` values, each dropped after
    /// `time_to_idle` without a read.
    #[must_use]
    pub fn new(max_entries: u64, time_to_idle: Duration) -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(max_entries)
                .time_to_idle(time_to_idle)
                .build(),
        }
    }

    /// The value of `view` for `user_id` at `generation`, if cached.
    #[must_use]
    pub fn get(&self, user_id: &str, generation: Generation, view: &ViewDigest) -> Option<V> {
        self.cache.get(&CacheKey {
            user: user_id.into(),
            generation,
            view: *view,
        })
    }

    /// Stores the value of `view` for `user_id` at `generation`: the
    /// generation read before the snapshot the value was computed on.
    pub fn insert(&self, user_id: &str, generation: Generation, view: ViewDigest, value: V) {
        self.cache.insert(
            CacheKey {
                user: user_id.into(),
                generation,
                view,
            },
            value,
        );
    }

    /// Applies pending evictions (capacity and time-to-idle), which moka
    /// otherwise runs lazily during other operations.
    pub fn run_pending_tasks(&self) {
        self.cache.run_pending_tasks();
    }

    /// Cached values (approximate until [`Self::run_pending_tasks`]).
    #[must_use]
    pub fn entry_count(&self) -> u64 {
        self.cache.entry_count()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_count_bumps_and_differ_by_instance() {
        let a = GenerationCell::new();
        let b = GenerationCell::new();
        assert_ne!(a.current().instance, b.current().instance);
        let g0 = a.current();
        a.bump();
        let g1 = a.current();
        assert_eq!(g1.instance, g0.instance);
        assert_eq!(g1.counter, g0.counter + 1);
    }

    #[test]
    fn a_user_keeps_one_cell_while_a_handle_holds_it() {
        let generations = Generations::new();
        let held = generations.cell("u1");
        assert!(Arc::ptr_eq(&held, &generations.cell("u1")));
        held.bump();
        assert_eq!(generations.current("u1"), Some(held.current()));
        assert_eq!(generations.prune(), 0, "a handle holds it");

        let instance = held.current().instance;
        drop(held);
        assert_eq!(generations.prune(), 1);
        assert!(generations.is_empty());
        assert_eq!(generations.current("u1"), None);
        assert_ne!(generations.cell("u1").current().instance, instance);
    }

    #[test]
    fn a_cell_no_handle_holds_is_never_handed_out_again() {
        let generations = Generations::new();
        let held = generations.cell("u1");
        held.bump();
        let before = held.current();
        drop(held);
        // No prune ran: the map still has the cell, but nothing reports it.
        assert_eq!(generations.len(), 1);
        assert_eq!(generations.current("u1"), None);
        let next = generations.cell("u1");
        assert_ne!(next.current().instance, before.instance);
        assert_eq!(generations.current("u1"), Some(next.current()));
        assert_eq!(generations.len(), 1, "the new cell replaced the old one");
    }

    #[test]
    fn retiring_moves_the_cell_on_and_starts_a_new_one() {
        let generations = Generations::new();
        let held = generations.cell("u1");
        let before = held.current();
        generations.retire("u1");
        assert_eq!(held.current().counter, before.counter + 1);
        assert_eq!(generations.current("u1"), None);
        let next = generations.cell("u1");
        assert!(!Arc::ptr_eq(&held, &next));
        assert_ne!(next.current().instance, before.instance);
        generations.retire("u2"); // no cell: nothing to do
        assert_eq!(generations.len(), 1);
    }

    #[test]
    fn the_cache_answers_only_for_the_generation_it_was_given() {
        let cache: GenerationCache<u64> = GenerationCache::new(16, Duration::from_secs(60));
        let cell = GenerationCell::new();
        let g0 = cell.current();
        cache.insert("u1", g0, [1; 16], 42);
        assert_eq!(cache.get("u1", g0, &[1; 16]), Some(42));
        assert_eq!(cache.get("u1", g0, &[2; 16]), None, "another view");
        assert_eq!(cache.get("u2", g0, &[1; 16]), None, "another user");
        cell.bump();
        assert_eq!(cache.get("u1", cell.current(), &[1; 16]), None);
    }

    #[test]
    fn the_cache_is_bounded() {
        let cache: GenerationCache<u64> = GenerationCache::new(8, Duration::from_secs(60));
        let cell = GenerationCell::new();
        for n in 0..100_u8 {
            cell.bump();
            cache.insert("u1", cell.current(), [n; 16], u64::from(n));
        }
        cache.run_pending_tasks();
        assert!(cache.entry_count() <= 8, "{}", cache.entry_count());
    }
}
