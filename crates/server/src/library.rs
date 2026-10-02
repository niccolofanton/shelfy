//! The write path of a user's library (plan §2.9 Library and Collections,
//! P1-03), and the caches keyed by the library generation (§2.14).
//!
//! **Writes.** [`write`] runs a change in one transaction on the user's
//! library and tells whether it changed rows. A write that did has moved the
//! library [`Generation`] (the handle's write path bumps it after the commit;
//! see [`shelfy_core::generation`]), so every ETag of the library's views
//! (list, post, search, stats, collections, counts) and every cached count
//! is stale at once. The route then calls [`announce`], and the user's open
//! streams get `posts.changed` (the posts it changed, `[]` when it changed
//! only collections, `null` for more than 200 or "any") and
//! `stats.changed`. A write that changed nothing announces nothing.
//!
//! **Caches** ([`LibraryCaches`]): post counts per filter (`GET
//! /posts/count`) and the stats (`GET /stats`), keyed by `(user,
//! generation, view)`. Values are computed on a read snapshot opened after
//! the generation was read, the order the ETags rely on too.
//!
//! Seams: the bulk and trash routes (P1-11) write through [`write`] and
//! announce with their own [`ChangeReason`] ([`announce_as`]); a job that
//! changes posts announces the same way.
//!
//! [`Generation`]: shelfy_core::generation::Generation

use std::time::Duration;

use rusqlite::Transaction;
use serde::Serialize;
use sha2::{Digest, Sha256};
use shelfy_core::generation::{GenerationCache, ViewDigest};
use shelfy_core::repo::RepoError;
use shelfy_core::repo::stats::Stats;

use crate::error::ApiError;
use crate::events::MAX_EVENT_KEYS;
use crate::events::model::ChangeReason;
use crate::state::{AppState, blocking};

/// Cached counts, over every user (§2.14). An entry is a few dozen bytes.
const COUNT_ENTRIES: u64 = 4_096;
/// Cached stats, over every user.
const STATS_ENTRIES: u64 = 1_024;
/// An entry nobody read for this long is dropped.
const TIME_TO_IDLE: Duration = Duration::from_secs(10 * 60);

/// The outcome of a [`write`].
#[derive(Debug)]
pub struct Written<T> {
    /// What the change returned.
    pub value: T,
    /// Whether it changed rows: the generation moved, and the change is
    /// worth announcing.
    pub changed: bool,
}

/// Runs `change` in a write transaction on `user_id`'s library, off the
/// async workers. An error rolls the transaction back.
///
/// # Errors
///
/// `change`'s error; the library cannot be opened or the commit fails.
pub async fn write<T, F>(state: &AppState, user_id: &str, change: F) -> Result<Written<T>, ApiError>
where
    F: FnOnce(&Transaction<'_>) -> Result<T, RepoError> + Send + 'static,
    T: Send + 'static,
{
    let db = state.user_db(user_id).await?;
    blocking(move || {
        db.write(|tx| {
            let before = tx.total_changes();
            let value = change(tx)?;
            let changed = tx.total_changes() != before;
            Ok::<_, RepoError>(Written { value, changed })
        })
    })
    .await
}

/// The `keys` of a `posts.changed` event for these posts: the list, or
/// `None` ("reload the view") past [`MAX_EVENT_KEYS`].
#[must_use]
pub fn event_keys(keys: Vec<String>) -> Option<Vec<String>> {
    (keys.len() <= MAX_EVENT_KEYS).then_some(keys)
}

/// Tells `user_id`'s open streams that an edit changed their library:
/// `posts.changed` with reason `edit` and `keys` (see [`event_keys`]; `[]`
/// when only collections changed), and `stats.changed`.
pub fn announce(state: &AppState, user_id: &str, keys: Option<Vec<String>>) {
    announce_as(state, user_id, ChangeReason::Edit, keys);
}

/// [`announce`] with another reason (`delete` for the trash, …).
pub fn announce_as(
    state: &AppState,
    user_id: &str,
    reason: ChangeReason,
    keys: Option<Vec<String>>,
) {
    state.events().posts_changed(user_id, reason, keys);
    state.events().stats_changed(user_id);
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
}

impl LibraryCaches {
    /// Empty caches with the default bounds.
    #[must_use]
    pub fn new() -> Self {
        Self {
            counts: GenerationCache::new(COUNT_ENTRIES, TIME_TO_IDLE),
            stats: GenerationCache::new(STATS_ENTRIES, TIME_TO_IDLE),
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
