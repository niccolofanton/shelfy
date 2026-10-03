//! The throttles of plan §2.10, leading edge (P1 assumption G1).
//!
//! A [`Throttle`] guards one stream of changes (one user's `posts.changed` of
//! one reason, their `stats.changed`, one job's `job.updated`):
//!
//! - when nothing was sent within the last `window`, a change goes out at
//!   once, so a lone write reaches the client without delay (§6.2: write →
//!   event p95 ≤ 300 ms);
//! - otherwise it is held, and later changes merge into it ([`Merge`]);
//! - the held change is flushed `window` after the previous send, so a burst
//!   costs one event per window and no change waits longer than `window`.
//!
//! The 400 ms quiet debounce of §2.10 stays in the client (`usePosts`). The
//! state machine is pure: callers pass the time, and the bus schedules the
//! flush that [`Offer::FlushAt`] asks for.

use std::collections::HashSet;
use std::time::Duration;

use tokio::time::Instant;

use super::model::{JobUpdatedEvent, StatsChangedEvent, SyncProgressEvent};

/// Most keys a `posts.changed` event lists; past it, `keys` is `null`
/// ("reload the view"). Also the size of `POST /posts/batch-get` (§2.9).
pub const MAX_EVENT_KEYS: usize = 200;

/// A held change that absorbs a later one.
pub trait Merge {
    /// Folds `later` into `self`.
    fn merge(&mut self, later: Self);
}

/// What to do with an offered change.
#[derive(Debug, PartialEq)]
pub enum Offer<P> {
    /// Send it now.
    Now(P),
    /// Held: call [`Throttle::flush`] at this time.
    FlushAt(Instant),
    /// Merged into the held change, whose flush is already due.
    Merged,
}

/// One throttled stream of changes.
#[derive(Debug)]
pub struct Throttle<P> {
    window: Duration,
    last_sent: Option<Instant>,
    held: Option<P>,
}

impl<P: Merge> Throttle<P> {
    /// A throttle that sends at most once per `window`, leading edge.
    #[must_use]
    pub const fn new(window: Duration) -> Self {
        Self {
            window,
            last_sent: None,
            held: None,
        }
    }

    /// Offers `change` at `now`.
    pub fn offer(&mut self, change: P, now: Instant) -> Offer<P> {
        if let Some(held) = &mut self.held {
            held.merge(change);
            return Offer::Merged;
        }
        match self.last_sent {
            Some(sent) if now < sent + self.window => {
                self.held = Some(change);
                Offer::FlushAt(sent + self.window)
            }
            _ => {
                self.last_sent = Some(now);
                Offer::Now(change)
            }
        }
    }

    /// The held change, to send at `now`; `None` when nothing is held.
    pub fn flush(&mut self, now: Instant) -> Option<P> {
        let held = self.held.take()?;
        self.last_sent = Some(now);
        Some(held)
    }

    /// Whether a change is held for a later flush.
    #[must_use]
    pub fn is_holding(&self) -> bool {
        self.held.is_some()
    }

    /// Whether the throttle is back to its initial behavior at `now`: nothing
    /// held and the window of the last send over. Such a throttle can be
    /// dropped.
    #[must_use]
    pub fn is_idle(&self, now: Instant) -> bool {
        self.held.is_none() && self.last_sent.is_none_or(|sent| now >= sent + self.window)
    }
}

/// The `keys` of a `posts.changed` event: distinct keys in first-seen order,
/// or `None` for "any post" (no list given, or more than [`MAX_EVENT_KEYS`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostKeys(Option<Vec<String>>);

impl PostKeys {
    /// The keys of one change, deduplicated and capped.
    #[must_use]
    pub fn new(keys: Option<Vec<String>>) -> Self {
        let mut this = Self(Some(Vec::new()));
        this.merge(Self(keys));
        this
    }

    /// The list for the event.
    #[must_use]
    pub fn into_keys(self) -> Option<Vec<String>> {
        self.0
    }
}

impl Merge for PostKeys {
    fn merge(&mut self, later: Self) {
        let (Some(keys), Some(more)) = (&mut self.0, later.0) else {
            self.0 = None;
            return;
        };
        let mut seen: HashSet<String> = keys.iter().cloned().collect();
        for key in more {
            if seen.insert(key.clone()) {
                keys.push(key);
            }
        }
        if keys.len() > MAX_EVENT_KEYS {
            self.0 = None;
        }
    }
}

impl Merge for StatsChangedEvent {
    fn merge(&mut self, _later: Self) {}
}

/// Only the latest state of a job matters.
impl Merge for JobUpdatedEvent {
    fn merge(&mut self, later: Self) {
        *self = later;
    }
}

/// Only the latest progress of a run matters (the counters only grow).
impl Merge for SyncProgressEvent {
    fn merge(&mut self, later: Self) {
        *self = later;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Duration = Duration::from_secs(2);

    fn keys(list: &[&str]) -> PostKeys {
        PostKeys::new(Some(list.iter().map(|&k| k.to_owned()).collect()))
    }

    #[test]
    fn a_lone_change_goes_out_at_once() {
        let t0 = Instant::now();
        let mut throttle = Throttle::new(WINDOW);
        assert_eq!(throttle.offer(keys(&["a"]), t0), Offer::Now(keys(&["a"])));
        assert!(!throttle.is_idle(t0));
        assert!(throttle.is_idle(t0 + WINDOW));
        // Once the window is over, the next change is a leading edge again.
        assert_eq!(
            throttle.offer(keys(&["b"]), t0 + WINDOW),
            Offer::Now(keys(&["b"]))
        );
    }

    #[test]
    fn follow_ups_merge_and_flush_one_window_after_the_send() {
        let t0 = Instant::now();
        let mut throttle = Throttle::new(WINDOW);
        assert!(matches!(throttle.offer(keys(&["a"]), t0), Offer::Now(_)));
        let at = t0 + Duration::from_millis(100);
        assert_eq!(
            throttle.offer(keys(&["b"]), at),
            Offer::FlushAt(t0 + WINDOW)
        );
        assert!(throttle.is_holding());
        assert_eq!(
            throttle.offer(keys(&["c", "b"]), at + Duration::from_millis(500)),
            Offer::Merged
        );
        let flushed = throttle.flush(t0 + WINDOW).unwrap();
        assert_eq!(flushed.into_keys().unwrap(), ["b", "c"]);
        assert!(!throttle.is_holding());
        assert_eq!(throttle.flush(t0 + WINDOW), None);
        // The flush counts as a send: the next change waits for its window.
        assert_eq!(
            throttle.offer(keys(&["d"]), t0 + WINDOW + Duration::from_millis(1)),
            Offer::FlushAt(t0 + WINDOW * 2)
        );
    }

    #[test]
    fn keys_are_distinct_and_capped() {
        assert_eq!(
            keys(&["a", "b", "a"]).into_keys().unwrap(),
            ["a", "b"],
            "duplicates collapse"
        );
        let many: Vec<String> = (0..=MAX_EVENT_KEYS).map(|n| format!("k{n}")).collect();
        assert_eq!(PostKeys::new(Some(many.clone())).into_keys(), None);
        let mut most = PostKeys::new(Some(many[..MAX_EVENT_KEYS].to_vec()));
        assert_eq!(most.clone().into_keys().unwrap().len(), MAX_EVENT_KEYS);
        most.merge(keys(&["k0"]));
        assert!(
            most.clone().into_keys().is_some(),
            "a repeat does not grow it"
        );
        most.merge(keys(&["new"]));
        assert_eq!(most.into_keys(), None);
        // "Any post" absorbs lists, either way round.
        let mut any = PostKeys::new(None);
        any.merge(keys(&["a"]));
        assert_eq!(any.into_keys(), None);
        let mut list = keys(&["a"]);
        list.merge(PostKeys::new(None));
        assert_eq!(list.into_keys(), None);
    }

    #[test]
    fn a_job_keeps_its_latest_state() {
        use crate::events::model::JobState;
        let job = |state| JobUpdatedEvent {
            id: 1,
            kind: "media.video".into(),
            state,
            progress: None,
            stage: None,
            post_key: None,
            error_code: None,
        };
        let mut held = job(JobState::Running);
        held.merge(job(JobState::Succeeded));
        assert_eq!(held.state, JobState::Succeeded);
    }
}
