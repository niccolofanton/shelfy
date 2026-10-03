//! The write path of a user's library (plan §2.9 Library and Collections,
//! P1-03), and the caches keyed by the library generation (§2.14).
//!
//! **Writes.** [`write`] runs a change in one transaction on the user's
//! library and tells whether it changed rows. A write that did has moved the
//! library [`Generation`] (the handle's write path bumps it after the commit;
//! see [`shelfy_core::generation`]), so every ETag of the library's views
//! (list, post, search, stats, collections, counts) and every cached count
//! is stale at once. Right after the commit, still on the blocking thread,
//! [`write`] then:
//!
//! 1. retires the generation when the cache now serves the library through a
//!    handle with another generation cell: an explicit eviction (a lock, a
//!    migration install, a job's own check) landed while the write held its
//!    handle, so the commit moved a cell that no new reader sees;
//! 2. announces the change ([`announce`]): the user's open streams get
//!    `posts.changed` (the posts it changed, `[]` when it changed only
//!    collections, `null` for more than 200 or "any") and `stats.changed`.
//!
//! Both happen even when the request that started the write is gone (a 504
//! from the time limit, a closed connection): the blocking task runs to the
//! end. A write that changed nothing announces nothing.
//!
//! **Caches** ([`LibraryCaches`]): post counts per filter (`GET
//! /posts/count`, and `total` of `GET /posts` and `GET /search`), the
//! stats (`GET /stats`) and the relevance rankings that search results are
//! paged through (P1-05, plan §2.14), keyed by `(user, generation, view)`.
//! Values are computed on a read snapshot opened after the generation was
//! read, the order the ETags rely on too.
//!
//! Seams: the bulk and trash routes (P1-11) write through [`write`] with
//! their own [`ChangeReason`] (`delete`); a job that changes posts calls
//! [`committed`] after its chunk commits, on the chunk's blocking task.
//!
//! [`Generation`]: shelfy_core::generation::Generation

use std::sync::Arc;
use std::time::Duration;

use rusqlite::Transaction;
use serde::Serialize;
use sha2::{Digest, Sha256};
use shelfy_core::db::{UserDb, UserDbCache};
use shelfy_core::generation::{GenerationCache, ViewDigest};
use shelfy_core::repo::RepoError;
use shelfy_core::repo::stats::Stats;

use crate::error::ApiError;
use crate::events::model::ChangeReason;
use crate::events::{EventBus, MAX_EVENT_KEYS};
use crate::state::{AppState, blocking};

/// Cached counts, over every user (§2.14). An entry is a few dozen bytes.
const COUNT_ENTRIES: u64 = 4_096;
/// Cached stats, over every user.
const STATS_ENTRIES: u64 = 1_024;
/// Cached relevance rankings, over every user: at most 1,000 post ids each,
/// so at most about 8 MB.
const RANKING_ENTRIES: u64 = 1_024;
/// An entry nobody read for this long is dropped.
const TIME_TO_IDLE: Duration = Duration::from_secs(10 * 60);

/// What a change gives [`write`]: its value for the route, and the posts it
/// touched, for the `posts.changed` event of a write that changed rows.
#[derive(Debug)]
pub struct Change<T> {
    /// What the change returns to the route.
    pub value: T,
    /// The keys of the posts it touched (see [`event_keys`]): `[]` when it
    /// touched only collections, `None` for "any".
    pub keys: Option<Vec<String>>,
}

impl<T> Change<T> {
    /// A change that touched only collections: `posts.changed` gets `[]`.
    pub fn collections(value: T) -> Self {
        Self {
            value,
            keys: Some(Vec::new()),
        }
    }
}

/// The outcome of a [`write`].
#[derive(Debug)]
pub struct Written<T> {
    /// What the change returned.
    pub value: T,
    /// Whether it changed rows: the generation moved, and the change was
    /// announced.
    pub changed: bool,
}

/// Runs `change` in a write transaction on `user_id`'s library, off the
/// async workers, and, when it changed rows, retires a generation that no
/// reader would see and announces the change as `reason` (module docs). An
/// error rolls the transaction back and announces nothing.
///
/// # Errors
///
/// `change`'s error; the library cannot be opened or the commit fails.
pub async fn write<T, F>(
    state: &AppState,
    user_id: &str,
    reason: ChangeReason,
    change: F,
) -> Result<Written<T>, ApiError>
where
    F: FnOnce(&Transaction<'_>) -> Result<Change<T>, RepoError> + Send + 'static,
    T: Send + 'static,
{
    let db = state.user_db(user_id).await?;
    let cache = Arc::clone(state.user_dbs());
    let events = state.events().clone();
    let user = user_id.to_owned();
    blocking(move || {
        let (Change { value, keys }, changed) = db.write(|tx| {
            let before = tx.total_changes();
            let change = change(tx)?;
            Ok::<_, RepoError>((change, tx.total_changes() != before))
        })?;
        if changed {
            committed(&cache, &events, &user, &db, reason, keys);
        }
        Ok::<_, RepoError>(Written { value, changed })
    })
    .await
}

/// What follows a commit through `db` that changed `user_id`'s posts, on the
/// thread that committed it: the generation is retired when no reader would
/// see it move ([`write`], step 1), then the change is announced
/// ([`announce`]). A job's chunk calls it from its blocking task too, so an
/// abort of the worker after the commit still announces it (P1-11 review
/// L7, as F6 L2 for requests).
pub fn committed(
    cache: &UserDbCache,
    events: &EventBus,
    user_id: &str,
    db: &UserDb,
    reason: ChangeReason,
    keys: Option<Vec<String>>,
) {
    retire_if_unseen(cache, user_id, db);
    announce(events, user_id, reason, keys);
}

/// After a write through `db` moved its generation: when the cache serves
/// `user_id`'s library through a handle with another generation cell (an
/// explicit eviction retired `db`'s cell and a reader opened a new one), no
/// reader sees the move, so retire the new cell too. The job system checks
/// the same after each chunk.
fn retire_if_unseen(cache: &UserDbCache, user_id: &str, db: &UserDb) {
    if let Some(current) = cache.get_if_present(user_id)
        && current.generation().instance != db.generation().instance
    {
        cache.evict(user_id);
    }
}

/// The `keys` of a `posts.changed` event for these posts: the list, or
/// `None` ("reload the view") past [`MAX_EVENT_KEYS`].
#[must_use]
pub fn event_keys(keys: Vec<String>) -> Option<Vec<String>> {
    (keys.len() <= MAX_EVENT_KEYS).then_some(keys)
}

/// Tells `user_id`'s open streams that a committed change touched their
/// library: `posts.changed` with `reason` and `keys` (see [`event_keys`];
/// `[]` when only collections changed), and `stats.changed`.
pub fn announce(events: &EventBus, user_id: &str, reason: ChangeReason, keys: Option<Vec<String>>) {
    events.posts_changed(user_id, reason, keys);
    events.stats_changed(user_id);
}

/// The digest that keys a cached value: `view` (what was computed) and its
/// normalized parameters.
///
/// # Panics
///
/// When `params` cannot be serialized to JSON; parameter types always can.
#[must_use]
pub fn view_digest(view: &str, params: &impl Serialize) -> ViewDigest {
    let params = serde_json::to_vec(params).expect("view parameters serialize");
    let mut hash = Sha256::new();
    for part in [view.as_bytes(), &params] {
        // Length-prefixed, so no two inputs concatenate to the same bytes.
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    let digest = hash.finalize();
    let mut view = [0_u8; 16];
    view.copy_from_slice(&digest[..16]);
    view
}

/// Values computed from one state of a user's library.
pub struct LibraryCaches {
    /// Posts matching a filter (`GET /posts/count`).
    pub counts: GenerationCache<u64>,
    /// The library counters (`GET /stats`).
    pub stats: GenerationCache<Stats>,
    /// The relevance order of a search (`shelfy_core::repo::posts::rank`):
    /// the snapshot its pages are cut from.
    pub rankings: GenerationCache<Arc<[i64]>>,
}

impl LibraryCaches {
    /// Empty caches with the default bounds.
    #[must_use]
    pub fn new() -> Self {
        Self {
            counts: GenerationCache::new(COUNT_ENTRIES, TIME_TO_IDLE),
            stats: GenerationCache::new(STATS_ENTRIES, TIME_TO_IDLE),
            rankings: GenerationCache::new(RANKING_ENTRIES, TIME_TO_IDLE),
        }
    }
}

impl Default for LibraryCaches {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_follow_the_view_and_its_parameters() {
        let a = view_digest("posts.count", &("q", 1));
        assert_eq!(a, view_digest("posts.count", &("q", 1)));
        assert_ne!(a, view_digest("posts.count", &("q", 2)));
        assert_ne!(a, view_digest("stats", &("q", 1)));
    }

    #[test]
    fn event_keys_fall_back_to_any_past_the_cap() {
        let keys: Vec<String> = (0..=MAX_EVENT_KEYS).map(|n| format!("k{n}")).collect();
        assert_eq!(event_keys(keys[..2].to_vec()), Some(keys[..2].to_vec()));
        assert_eq!(event_keys(Vec::new()), Some(Vec::new()));
        assert_eq!(event_keys(keys), None);
    }
}
