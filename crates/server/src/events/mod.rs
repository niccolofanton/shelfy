//! Realtime events (plan §2.3 Push, §2.10): the per-user bus behind
//! `GET /api/v1/events` ([`crate::routes::events`]).
//!
//! **Publishing.** Code that changes a user's data tells the [`EventBus`]
//! (`state.events()`) after its transaction commits, so a client that reacts
//! to the event reads the change:
//!
//! | Call | Event | Throttle (§2.10) |
//! |---|---|---|
//! | [`EventBus::posts_changed`] | `posts.changed` `{keys, reason}` | per reason: leading edge, then at most one event per 2 s with the merged keys |
//! | [`EventBus::stats_changed`] | `stats.changed` `{}` | leading edge, then at most one per second |
//! | [`EventBus::job_updated`] | `job.updated` | per job: leading edge, then the latest state at most every 250 ms |
//! | [`notify`] (or [`EventBus::notification`]) | `notification` | none |
//! | [`EventBus::extension_status`] | `extension.status` | none: sent on change only ([`crate::extension::presence`]) |
//!
//! The throttles are leading edge (P1 assumption G1): a lone change goes out
//! at once, follow-ups merge, and nothing waits longer than its window
//! (`coalesce.rs`). The client keeps its own 400 ms debounce.
//!
//! **Delivery.** Each user has a bus (`bus.rs`): a `tokio::sync::broadcast`
//! channel of capacity 256 shared by their open streams, and a ring of their
//! last 256 events, kept 5 minutes, for `Last-Event-ID` replay. An event id is
//! `<bus epoch>-<sequence>`; the epoch is random per bus, so an id from
//! another user, or from before a restart, never resolves. A resume point
//! that the ring no longer covers, or a connection that falls 256 events
//! behind, gets `resync`: the client reloads everything.
//!
//! **Isolation.** A bus is found by the user id of the request's
//! [`CurrentUser`](crate::current_user::CurrentUser) and nothing else, so a
//! stream only ever carries its own user's events.
//!
//! **Memory.** Buses are created on first use. [`EventBus::sweep`], run by the
//! server's maintenance timer, drops ring events older than 5 minutes and
//! buses that nobody used for 5 minutes.
//!
//! Seams: the job system (P1-07) calls [`EventBus::job_updated`]; P1-15 reads
//! [`EventBus::connections`] for `shelfy_sse_connections`.

mod bus;
mod coalesce;
pub mod model;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use shelfy_core::repo::notifications::{self, NewNotification};
use tokio::time::Instant;

pub use bus::{Delivery, Published, Subscription};
pub use coalesce::MAX_EVENT_KEYS;
use model::{ChangeReason, EventTopic, ExtensionStatusEvent, JobUpdatedEvent, Notification};

use crate::error::ApiError;
use crate::ids::now_ms;
use crate::state::{AppState, blocking};
use bus::{FlushKey, UserBus};
use coalesce::PostKeys;

/// Capacity of a user's broadcast channel: a connection that falls this far
/// behind gets `resync` (§2.3).
pub const CHANNEL_CAPACITY: usize = 256;
/// Events kept per user for `Last-Event-ID` replay (§2.10).
pub const REPLAY_EVENTS: usize = 256;
/// How long an event stays replayable (§2.10).
pub const REPLAY_WINDOW: Duration = Duration::from_secs(5 * 60);
/// Throttle window of `posts.changed`, per reason: G1's "flush within 2 s".
pub const POSTS_WINDOW: Duration = Duration::from_secs(2);
/// Throttle window of `stats.changed`.
pub const STATS_WINDOW: Duration = Duration::from_secs(1);
/// Throttle window of `job.updated`, per job.
pub const JOB_WINDOW: Duration = Duration::from_millis(250);

/// The buses of every user. Cheap to clone.
#[derive(Clone, Default)]
pub struct EventBus {
    users: Arc<Mutex<HashMap<Box<str>, Arc<UserBus>>>>,
}

impl EventBus {
    /// An empty bus.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn users(&self) -> MutexGuard<'_, HashMap<Box<str>, Arc<UserBus>>> {
        self.users.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The bus of `user_id`, created on first use.
    fn bus(users: &mut HashMap<Box<str>, Arc<UserBus>>, user_id: &str) -> Arc<UserBus> {
        if let Some(bus) = users.get(user_id) {
            return Arc::clone(bus);
        }
        let bus = Arc::new(UserBus::new(Instant::now()));
        users.insert(user_id.into(), Arc::clone(&bus));
        bus
    }

    fn user(&self, user_id: &str) -> Arc<UserBus> {
        Self::bus(&mut self.users(), user_id)
    }

    /// Posts of `user_id` changed. `keys` names them; pass `None` when the
    /// change has no cheap list (a bulk action by filter, an import): the
    /// client reloads the whole view. More than [`MAX_EVENT_KEYS`] keys also
    /// become `None`.
    pub fn posts_changed(&self, user_id: &str, reason: ChangeReason, keys: Option<Vec<String>>) {
        let bus = self.user(user_id);
        let now = Instant::now();
        if let Some(at) = bus.offer_posts(reason, PostKeys::new(keys), now) {
            schedule(bus, FlushKey::Posts(reason), at);
        }
    }

    /// The library counters of `user_id` changed.
    pub fn stats_changed(&self, user_id: &str) {
        let bus = self.user(user_id);
        if let Some(at) = bus.offer_stats(Instant::now()) {
            schedule(bus, FlushKey::Stats, at);
        }
    }

    /// A job of `user_id` changed state or progress.
    pub fn job_updated(&self, user_id: &str, job: JobUpdatedEvent) {
        let bus = self.user(user_id);
        let id = job.id;
        if let Some(at) = bus.offer_job(job, Instant::now()) {
            schedule(bus, FlushKey::Job(id), at);
        }
    }

    /// Publishes a notification that is already stored; [`notify`] does both.
    pub fn notification(&self, user_id: &str, notification: &Notification) {
        self.user(user_id)
            .publish(EventTopic::Notification, notification, Instant::now());
    }

    /// Publishes the new status of `user_id`'s browser extension. Its
    /// presence calls this only when the status changed.
    pub fn extension_status(&self, user_id: &str, status: &ExtensionStatusEvent) {
        self.user(user_id)
            .publish(EventTopic::ExtensionStatus, status, Instant::now());
    }

    /// Opens a subscription to `user_id`'s events, resuming after the event
    /// `resume` when the client sent one.
    #[must_use]
    pub fn subscribe(&self, user_id: &str, resume: Option<&str>) -> Subscription {
        // Under the map lock, so a sweep cannot drop the bus in between.
        let mut users = self.users();
        Self::bus(&mut users, user_id).subscribe(resume, Instant::now())
    }

    /// Drops ring events older than [`REPLAY_WINDOW`], and the buses that
    /// nobody holds, hold nothing, and saw no activity for that long. A
    /// client resuming from a dropped bus gets `resync`.
    pub fn sweep(&self) {
        let now = Instant::now();
        self.users().retain(|_, bus| {
            bus.prune(now);
            // Subscriptions, pending flushes and publishers in progress hold
            // their own reference.
            Arc::strong_count(bus) > 1 || !bus.is_idle(now)
        });
    }

    /// Open streams, over every user.
    #[must_use]
    pub fn connections(&self) -> usize {
        self.users().values().map(|bus| bus.receivers()).sum()
    }

    /// Users with a bus.
    #[must_use]
    pub fn users_len(&self) -> usize {
        self.users().len()
    }
}

/// Flushes the held change of `key` at `at`.
fn schedule(bus: Arc<UserBus>, key: FlushKey, at: Instant) {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => {
            runtime.spawn(async move {
                tokio::time::sleep_until(at).await;
                bus.flush(key, Instant::now());
            });
        }
        // Outside a runtime there is no timer: send it now.
        Err(_) => bus.flush(key, Instant::now()),
    }
}

/// Stores a notification in `user_id`'s library and publishes it to their
/// open streams.
///
/// # Errors
///
/// 422 `validation_failed` for an invalid notification; database errors.
pub async fn notify(
    state: &AppState,
    user_id: &str,
    new: NewNotification,
) -> Result<Notification, ApiError> {
    let db = state.user_db(user_id).await?;
    let created =
        blocking(move || db.write(|tx| notifications::create(tx, &new, now_ms()))).await?;
    let notification = Notification::from(created);
    state.events().notification(user_id, &notification);
    Ok(notification)
}

#[cfg(test)]
mod tests {
    use super::model::{JobState, ResyncReason};
    use super::*;

    const ALICE: &str = "01J9Z3B8K4QW6TFX0V7G2N5RCA";

    fn job(id: i64, progress: f64) -> JobUpdatedEvent {
        JobUpdatedEvent {
            id,
            kind: "archive.drain".into(),
            state: JobState::Running,
            progress: Some(progress),
            stage: None,
            post_key: None,
            error_code: None,
        }
    }

    async fn next_event(sub: &mut Subscription) -> Arc<Published> {
        match sub.next().await {
            Delivery::Event(event) => event,
            Delivery::Resync { reason, .. } => panic!("unexpected resync: {reason:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_held_change_is_flushed_on_time() {
        let events = EventBus::new();
        let mut sub = events.subscribe(ALICE, None);
        let started = Instant::now();
        events.job_updated(ALICE, job(1, 0.1));
        events.job_updated(ALICE, job(1, 0.2));
        events.job_updated(ALICE, job(1, 0.3));
        let first = next_event(&mut sub).await;
        assert!(first.data.contains("0.1"), "{}", first.data);
        assert_eq!(started.elapsed(), Duration::ZERO);
        let second = next_event(&mut sub).await;
        assert!(
            second.data.contains("0.3"),
            "only the latest: {}",
            second.data
        );
        assert_eq!(started.elapsed(), JOB_WINDOW);
        assert_eq!(second.seq, first.seq + 1);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_buses_are_swept_and_busy_ones_kept() {
        let events = EventBus::new();
        let sub = events.subscribe(ALICE, None);
        let old_id = sub.head_id().to_owned();
        events.stats_changed("01J9Z3B8K4QW6TFX0V7G2N5RCB");
        assert_eq!(events.users_len(), 2);
        assert_eq!(events.connections(), 1);

        tokio::time::advance(REPLAY_WINDOW).await;
        events.sweep();
        assert_eq!(events.users_len(), 1, "the connected user stays");
        drop(sub);
        assert_eq!(events.connections(), 0);
        events.sweep();
        assert_eq!(events.users_len(), 1, "a disconnection counts as activity");
        tokio::time::advance(REPLAY_WINDOW).await;
        events.sweep();
        assert_eq!(events.users_len(), 0);

        // A new bus: the old id no longer resolves.
        let mut sub = events.subscribe(ALICE, Some(&old_id));
        match sub.next().await {
            Delivery::Resync { reason, id } => {
                assert_eq!(reason, ResyncReason::Unknown);
                assert_eq!(id, sub.head_id());
            }
            Delivery::Event(event) => panic!("unexpected event {event:?}"),
        }
    }

    #[test]
    fn without_a_runtime_held_changes_go_out_at_once() {
        let events = EventBus::new();
        events.stats_changed(ALICE);
        events.stats_changed(ALICE);
        let bus = events.user(ALICE);
        assert_eq!(bus.head_id().rsplit('-').next(), Some("2"));
    }
}
