//! The circuit breaker of each host group (§2.13, D14, SPIKE-2, SPIKE-9):
//! the three CDN groups, and the three hydration groups of P2-11
//! ([`super::limits`]).
//!
//! - **Samples.** A fetch admitted while the breaker is closed gives one
//!   sample when the CDN answered it or failed it transiently: *blocked* (a
//!   403 that is not the expiry text, a 429; see [`super::cdn`]) or not
//!   (stored, a 5xx, a timeout). Expired and gone answers never count, and
//!   neither do our own refusals: they are not about the CDN refusing us.
//! - **Opens** when the last [`BreakerConfig::window`] samples (50) hold
//!   [`BreakerConfig::open_at_blocked`] blocked ones (10, so ≥ 20 %), or when
//!   [`BreakerConfig::open_at_streak`] samples in a row (10) are blocked.
//! - **While open**, [`Breaker::admit`] refuses every fetch of the group
//!   before any request leaves: the archive drain moves those items to the
//!   extension (`archive_state = 'client'`, P2-10).
//! - **After [`BreakerConfig::open_for`]** (30 minutes) one fetch goes through
//!   as a probe (half-open). Blocked: open for another 30 minutes. Answered
//!   otherwise (stored, gone, expired, a refused file): closed, with an empty
//!   window. Inconclusive (a 5xx, a timeout, our own refusal, a cancelled
//!   fetch): the next fetch probes again.
//! - **Trip.** [`Breaker::trip`] opens it at once, for callers whose stop
//!   rule is the first block signal (SPIKE-9: a 429, a challenge or a login
//!   wall on a hydration host).
//! - **Signals.** `shelfy_breaker_open{host_group}` is 1 while open or
//!   half-open and 0 when closed (the P1-16 alert rule reads it).
//!   [`Breakers::subscribe`] wakes when a breaker opens or closes.
//!
//! The breaker takes the time as an argument (tokio's [`Instant`]), so tests
//! drive it without waiting.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

use super::limits::HostGroup;
use crate::telemetry::metrics::BREAKER_OPEN;

/// The thresholds of a breaker (§2.13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreakerConfig {
    /// Samples kept: the last 50.
    pub window: usize,
    /// Blocked samples in the window that open the breaker: 10, 20 % of 50.
    pub open_at_blocked: usize,
    /// Blocked samples in a row that open the breaker: 10.
    pub open_at_streak: usize,
    /// How long the breaker stays open before a probe: 30 minutes.
    pub open_for: Duration,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            window: 50,
            open_at_blocked: 10,
            open_at_streak: 10,
            open_for: Duration::from_secs(30 * 60),
        }
    }
}

/// What a fetch tells the breaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// The CDN refused us: a blocked sample; a probe that reopens.
    Blocked,
    /// The CDN served the bytes: a sample that is not blocked; a probe that
    /// closes.
    Served,
    /// A 5xx or a network failure: a sample that is not blocked; an
    /// inconclusive probe.
    Transient,
    /// The CDN answered about this URL (gone, expired, a file it cannot
    /// keep): no sample; a probe that closes.
    Answered,
    /// Our own policy stopped the fetch (a refused redirect or address): no
    /// sample; an inconclusive probe.
    Unknown,
}

/// The answer of [`Breaker::admit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Closed: fetch, then [`Breaker::record`].
    Pass,
    /// Half-open: fetch as the probe, then [`Breaker::record_probe`] (or
    /// [`Breaker::release_probe`] when the fetch ends without an answer).
    Probe,
    /// Open, or another probe is out: send nothing.
    Refuse,
}

/// The state of a breaker, as the archive drain sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakerState {
    /// Fetch normally.
    Closed,
    /// Hand the group's items to the extension until `until`.
    Open {
        /// When a probe becomes due.
        until: Instant,
    },
    /// A probe is due or out: one fetch may go (the next [`Breaker::admit`]
    /// decides which).
    HalfOpen,
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Closed,
    Open { until: Instant },
    HalfOpen { probing: bool },
}

#[derive(Debug)]
struct Inner {
    mode: Mode,
    /// The last samples, `true` for blocked.
    window: VecDeque<bool>,
    /// Blocked samples in the window.
    blocked: usize,
    /// Blocked samples in a row.
    streak: usize,
}

/// The breaker of one host group.
#[derive(Debug)]
pub struct Breaker {
    group: HostGroup,
    config: BreakerConfig,
    inner: Mutex<Inner>,
    changes: Arc<watch::Sender<u64>>,
}

impl Breaker {
    fn new(group: HostGroup, config: BreakerConfig, changes: Arc<watch::Sender<u64>>) -> Self {
        let breaker = Self {
            group,
            config,
            inner: Mutex::new(Inner {
                mode: Mode::Closed,
                window: VecDeque::with_capacity(config.window),
                blocked: 0,
                streak: 0,
            }),
            changes,
        };
        breaker.publish(false);
        breaker
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The host group.
    #[must_use]
    pub fn group(&self) -> HostGroup {
        self.group
    }

    /// Whether a fetch may go at `now`.
    pub fn admit(&self, now: Instant) -> Admission {
        let mut inner = self.lock();
        match inner.mode {
            Mode::Closed => Admission::Pass,
            Mode::Open { until } if now < until => Admission::Refuse,
            Mode::Open { .. } | Mode::HalfOpen { probing: false } => {
                inner.mode = Mode::HalfOpen { probing: true };
                Admission::Probe
            }
            Mode::HalfOpen { probing: true } => Admission::Refuse,
        }
    }

    /// Records what a fetch admitted with [`Admission::Pass`] found. A result
    /// that arrives after the breaker opened is dropped.
    pub fn record(&self, signal: Signal, now: Instant) {
        let mut inner = self.lock();
        if !matches!(inner.mode, Mode::Closed) {
            return;
        }
        let blocked = match signal {
            Signal::Blocked => true,
            Signal::Served | Signal::Transient => false,
            Signal::Answered | Signal::Unknown => return,
        };
        if inner.window.len() == self.config.window && inner.window.pop_front() == Some(true) {
            inner.blocked -= 1;
        }
        inner.window.push_back(blocked);
        if blocked {
            inner.blocked += 1;
            inner.streak += 1;
        } else {
            inner.streak = 0;
        }
        if inner.blocked >= self.config.open_at_blocked
            || inner.streak >= self.config.open_at_streak
        {
            self.open(&mut inner, now);
        }
    }

    /// Records what the probe found.
    pub fn record_probe(&self, signal: Signal, now: Instant) {
        let mut inner = self.lock();
        if !matches!(inner.mode, Mode::HalfOpen { probing: true }) {
            return;
        }
        match signal {
            Signal::Blocked => self.open(&mut inner, now),
            Signal::Served | Signal::Answered => {
                inner.mode = Mode::Closed;
                inner.window.clear();
                inner.blocked = 0;
                inner.streak = 0;
                drop(inner);
                tracing::info!(host_group = self.group.label(), "breaker closed");
                self.publish(false);
            }
            Signal::Transient | Signal::Unknown => inner.mode = Mode::HalfOpen { probing: false },
        }
    }

    /// Opens the breaker now, whatever its samples: for a caller that stops
    /// at the first block signal. Open already: the 30 minutes start again.
    pub fn trip(&self, now: Instant) {
        let mut inner = self.lock();
        self.open(&mut inner, now);
    }

    /// Frees the probe slot of a probe that ended without an answer (the
    /// fetch was dropped): the next fetch probes.
    pub fn release_probe(&self) {
        let mut inner = self.lock();
        if matches!(inner.mode, Mode::HalfOpen { probing: true }) {
            inner.mode = Mode::HalfOpen { probing: false };
        }
    }

    /// The state at `now`.
    #[must_use]
    pub fn state(&self, now: Instant) -> BreakerState {
        match self.lock().mode {
            Mode::Closed => BreakerState::Closed,
            Mode::Open { until } if now < until => BreakerState::Open { until },
            Mode::Open { .. } | Mode::HalfOpen { .. } => BreakerState::HalfOpen,
        }
    }

    fn open(&self, inner: &mut Inner, now: Instant) {
        let was_closed = matches!(inner.mode, Mode::Closed);
        inner.mode = Mode::Open {
            until: now + self.config.open_for,
        };
        if was_closed {
            tracing::warn!(
                host_group = self.group.label(),
                blocked = inner.blocked,
                streak = inner.streak,
                "breaker open: the platform refuses this server"
            );
            self.publish(true);
        } else {
            tracing::info!(host_group = self.group.label(), "breaker open again");
        }
    }

    fn publish(&self, open: bool) {
        metrics::gauge!(BREAKER_OPEN, "host_group" => self.group.label()).set(if open {
            1.0
        } else {
            0.0
        });
        self.changes.send_modify(|generation| *generation += 1);
    }
}

/// The breakers of every host group.
#[derive(Debug)]
pub struct Breakers {
    breakers: [Breaker; 6],
    changes: Arc<watch::Sender<u64>>,
}

impl Breakers {
    /// Closed breakers with `config`; their gauges start at 0, so the alert
    /// rule has data from the first scrape.
    #[must_use]
    pub fn new(config: BreakerConfig) -> Self {
        let changes = Arc::new(watch::Sender::new(0));
        Self {
            breakers: HostGroup::ALL.map(|group| Breaker::new(group, config, Arc::clone(&changes))),
            changes,
        }
    }

    /// The breaker of `group`.
    #[must_use]
    pub fn get(&self, group: HostGroup) -> &Breaker {
        &self.breakers[group.index()]
    }

    /// A receiver that changes whenever a breaker opens or closes; read the
    /// states with [`Breaker::state`].
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    fn breaker() -> Breaker {
        Breaker::new(
            HostGroup::Instagram,
            BreakerConfig::default(),
            Arc::new(watch::Sender::new(0)),
        )
    }

    fn feed(breaker: &Breaker, signals: impl IntoIterator<Item = Signal>, now: Instant) {
        for signal in signals {
            assert_eq!(breaker.admit(now), Admission::Pass);
            breaker.record(signal, now);
        }
    }

    #[test]
    fn ten_blocked_in_a_row_open_it() {
        let b = breaker();
        let now = Instant::now();
        feed(&b, [Signal::Served; 30], now);
        feed(&b, [Signal::Blocked; 9], now);
        assert_eq!(b.state(now), BreakerState::Closed);
        feed(&b, [Signal::Blocked], now);
        assert_eq!(
            b.state(now),
            BreakerState::Open {
                until: now + 30 * MINUTE
            }
        );
        assert_eq!(b.admit(now + 29 * MINUTE), Admission::Refuse);
    }

    #[test]
    fn twenty_percent_of_the_last_fifty_open_it() {
        // One blocked in five: the tenth blocked sample is the 50th sample.
        let b = breaker();
        let now = Instant::now();
        let mut pattern = Vec::new();
        for _ in 0..9 {
            pattern.extend([Signal::Served; 4]);
            pattern.push(Signal::Blocked);
        }
        pattern.extend([Signal::Served; 4]);
        feed(&b, pattern, now);
        assert_eq!(b.state(now), BreakerState::Closed, "9 of 49");
        feed(&b, [Signal::Blocked], now);
        assert!(
            matches!(b.state(now), BreakerState::Open { .. }),
            "10 of 50"
        );
    }

    #[test]
    fn fewer_than_twenty_percent_never_open_it() {
        // One blocked in six is at most 9 of any 50 samples.
        let b = breaker();
        let now = Instant::now();
        for _ in 0..100 {
            feed(
                &b,
                [
                    Signal::Served,
                    Signal::Transient,
                    Signal::Served,
                    Signal::Served,
                    Signal::Served,
                ],
                now,
            );
            feed(&b, [Signal::Blocked], now);
        }
        assert_eq!(b.state(now), BreakerState::Closed);
    }

    #[test]
    fn expired_gone_and_refusals_never_count() {
        let b = breaker();
        let now = Instant::now();
        for _ in 0..9 {
            feed(
                &b,
                [Signal::Answered, Signal::Unknown, Signal::Answered],
                now,
            );
            feed(&b, [Signal::Blocked], now);
        }
        feed(&b, [Signal::Answered; 100], now);
        assert_eq!(b.state(now), BreakerState::Closed, "9 blocked so far");
        feed(&b, [Signal::Blocked], now);
        assert!(
            matches!(b.state(now), BreakerState::Open { .. }),
            "the run of blocked samples was never broken"
        );
    }

    #[test]
    fn a_probe_after_thirty_minutes_closes_or_reopens_it() {
        let b = breaker();
        let t0 = Instant::now();
        feed(&b, [Signal::Blocked; 10], t0);
        let due = t0 + 30 * MINUTE;
        assert_eq!(b.state(due - MINUTE), BreakerState::Open { until: due });
        assert_eq!(b.admit(due - MINUTE), Admission::Refuse);
        assert_eq!(b.state(due), BreakerState::HalfOpen);

        // One probe at a time.
        assert_eq!(b.admit(due), Admission::Probe);
        assert_eq!(b.admit(due), Admission::Refuse);
        // A blocked probe reopens for another 30 minutes.
        b.record_probe(Signal::Blocked, due);
        assert_eq!(
            b.state(due),
            BreakerState::Open {
                until: due + 30 * MINUTE
            }
        );
        assert_eq!(b.admit(due + 29 * MINUTE), Admission::Refuse);

        // An inconclusive probe lets the next fetch probe.
        let later = due + 30 * MINUTE;
        assert_eq!(b.admit(later), Admission::Probe);
        b.record_probe(Signal::Transient, later);
        assert_eq!(b.state(later), BreakerState::HalfOpen);
        assert_eq!(b.admit(later), Admission::Probe);
        // A dropped probe frees the slot too.
        b.release_probe();
        assert_eq!(b.admit(later), Admission::Probe);
        // A served probe closes it with a fresh window.
        b.record_probe(Signal::Served, later);
        assert_eq!(b.state(later), BreakerState::Closed);
        feed(&b, [Signal::Blocked; 9], later);
        assert_eq!(
            b.state(later),
            BreakerState::Closed,
            "the old samples are gone"
        );
    }

    #[test]
    fn a_probe_the_cdn_answered_closes_it() {
        let b = breaker();
        let t0 = Instant::now();
        feed(&b, [Signal::Blocked; 10], t0);
        let due = t0 + 30 * MINUTE;
        assert_eq!(b.admit(due), Admission::Probe);
        b.record_probe(Signal::Answered, due);
        assert_eq!(b.state(due), BreakerState::Closed);
        // A late result of a fetch admitted before the opening is ignored.
        b.record_probe(Signal::Blocked, due);
        assert_eq!(b.state(due), BreakerState::Closed);
    }

    #[test]
    fn results_while_open_are_dropped() {
        let b = breaker();
        let t0 = Instant::now();
        feed(&b, [Signal::Blocked; 10], t0);
        // Fetches admitted before the opening finish while it is open.
        b.record(Signal::Served, t0);
        b.record(Signal::Blocked, t0);
        assert_eq!(
            b.state(t0),
            BreakerState::Open {
                until: t0 + 30 * MINUTE
            }
        );
    }

    #[test]
    fn a_trip_opens_it_at_once() {
        let breakers = Breakers::new(BreakerConfig::default());
        let web = breakers.get(HostGroup::InstagramWeb);
        let t0 = Instant::now();
        assert_eq!(web.admit(t0), Admission::Pass);
        web.trip(t0);
        assert_eq!(
            web.state(t0),
            BreakerState::Open {
                until: t0 + 30 * MINUTE
            }
        );
        assert_eq!(web.admit(t0), Admission::Refuse);
        // Tripping again restarts the wait.
        web.trip(t0 + 10 * MINUTE);
        assert_eq!(
            web.state(t0 + 10 * MINUTE),
            BreakerState::Open {
                until: t0 + 40 * MINUTE
            }
        );
        assert_eq!(
            breakers.get(HostGroup::Instagram).state(t0),
            BreakerState::Closed,
            "the CDN group has its own breaker"
        );
    }

    #[test]
    fn subscribers_see_openings_and_closings() {
        let breakers = Breakers::new(BreakerConfig::default());
        let mut changes = breakers.subscribe();
        changes.mark_unchanged();
        let ig = breakers.get(HostGroup::Instagram);
        assert_eq!(ig.group(), HostGroup::Instagram);
        let t0 = Instant::now();
        feed(ig, [Signal::Blocked; 10], t0);
        assert!(changes.has_changed().unwrap(), "opened");
        changes.mark_unchanged();
        assert_eq!(
            breakers.get(HostGroup::X).state(t0),
            BreakerState::Closed,
            "groups are separate"
        );
        let due = t0 + 30 * MINUTE;
        assert_eq!(ig.admit(due), Admission::Probe);
        assert!(!changes.has_changed().unwrap(), "half-open is not a change");
        ig.record_probe(Signal::Served, due);
        assert!(changes.has_changed().unwrap(), "closed");
    }
}
