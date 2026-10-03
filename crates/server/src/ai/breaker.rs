//! The circuit breakers of the AI service (plan §2.15 "Reliability", G3-19,
//! G3-25, P3-09).
//!
//! Two kinds, because the owner's node and a user's cloud provider fail
//! differently:
//!
//! - [`UserBreakers`]: a per-(user, provider) breaker for BYOK providers. It
//!   opens for 60 s after 5 transient failures in a row, or 50 % of the last
//!   20, and lets one probe through when the cool-down ends.
//! - [`OperatorBreaker`]: the owner's node. It is not a counting breaker but a
//!   reachability state: `offline` on a failed connect or two timeouts in a
//!   row (queued work waits without spending a try, G3-25), `invalid_key` when
//!   the node refuses the key (the user's `ai.drain` queue pauses). A health
//!   probe (`GET /health`), or a models probe for the key, brings it back.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use shelfy_ai::ErrorKind;
use tokio::time::{Duration, Instant};

use crate::events::model::ProviderState;

/// Failures in a row that open a BYOK breaker.
const CONSECUTIVE_TO_OPEN: usize = 5;
/// The sliding window a BYOK breaker judges its failure ratio over.
const WINDOW: usize = 20;
/// The failure ratio over a full window that opens a BYOK breaker.
const OPEN_RATIO: f64 = 0.5;
/// How long a BYOK breaker stays open.
const OPEN_FOR: Duration = Duration::from_secs(60);
/// Transient failures in a row that turn the operator `offline`.
const OPERATOR_TRANSIENT_TO_OFFLINE: u32 = 2;

/// Whether a call may go to a provider now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// The breaker is closed: proceed.
    Allow,
    /// The breaker is half-open: this one call probes whether it recovered.
    Probe,
    /// The breaker is open: hold the call.
    Refuse,
}

/// One BYOK provider's breaker for one user.
#[derive(Debug)]
struct Breaker {
    recent: VecDeque<bool>,
    consecutive: usize,
    open_until: Option<Instant>,
    half_open: bool,
}

impl Breaker {
    fn new() -> Self {
        Self {
            recent: VecDeque::with_capacity(WINDOW),
            consecutive: 0,
            open_until: None,
            half_open: false,
        }
    }

    fn admit(&mut self, now: Instant) -> Admission {
        match self.open_until {
            Some(until) if now < until => Admission::Refuse,
            Some(_) => {
                self.half_open = true;
                Admission::Probe
            }
            None => Admission::Allow,
        }
    }

    fn record(&mut self, failed: bool, now: Instant) {
        if self.recent.len() == WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back(failed);
        if failed {
            self.consecutive += 1;
        } else {
            self.consecutive = 0;
        }
        if self.half_open {
            // The probe decides: a success closes the breaker, a failure
            // re-opens it for another cool-down.
            self.half_open = false;
            if failed {
                self.open_until = Some(now + OPEN_FOR);
            } else {
                self.reset();
            }
            return;
        }
        let failures = self.recent.iter().filter(|&&f| f).count();
        let ratio = failures as f64 / self.recent.len() as f64;
        if self.consecutive >= CONSECUTIVE_TO_OPEN
            || (self.recent.len() >= WINDOW && ratio >= OPEN_RATIO)
        {
            self.open_until = Some(now + OPEN_FOR);
        }
    }

    fn reset(&mut self) {
        self.recent.clear();
        self.consecutive = 0;
        self.open_until = None;
        self.half_open = false;
    }

    fn state(&self, now: Instant) -> ProviderState {
        match self.open_until {
            Some(until) if now < until => ProviderState::Down,
            _ => ProviderState::Ok,
        }
    }

    fn is_idle(&self, now: Instant) -> bool {
        self.recent.is_empty() && self.open_until.is_none_or(|until| now >= until)
    }
}

/// The BYOK breakers of every user, keyed by (user id, provider id).
#[derive(Default)]
pub struct UserBreakers {
    inner: Mutex<HashMap<(String, String), Breaker>>,
}

impl UserBreakers {
    /// A new, empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears a provider after its configuration changes.
    pub(crate) fn remove(&self, user: &str, provider: &str) {
        self.lock().remove(&(user.to_owned(), provider.to_owned()));
    }

    /// Whether a call to `provider` for `user` may go now.
    #[must_use]
    pub fn admit(&self, user: &str, provider: &str, now: Instant) -> Admission {
        let mut inner = self.lock();
        inner
            .entry((user.to_owned(), provider.to_owned()))
            .or_insert_with(Breaker::new)
            .admit(now)
    }

    /// Records a call's outcome (transient failures and rate limits count as
    /// failures; everything else as a success, since the provider answered).
    pub fn record(&self, user: &str, provider: &str, result: &CallOutcome, now: Instant) {
        let mut inner = self.lock();
        let breaker = inner
            .entry((user.to_owned(), provider.to_owned()))
            .or_insert_with(Breaker::new);
        breaker.record(result.counts_as_breaker_failure(), now);
        if breaker.is_idle(now) {
            inner.remove(&(user.to_owned(), provider.to_owned()));
        }
    }

    /// The state of `provider` for `user`.
    #[must_use]
    pub fn state(&self, user: &str, provider: &str, now: Instant) -> ProviderState {
        self.lock()
            .get(&(user.to_owned(), provider.to_owned()))
            .map_or(ProviderState::Ok, |breaker| breaker.state(now))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Breaker>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// How a call ended, as the breakers read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallOutcome {
    /// The provider answered usefully.
    Ok,
    /// A failure of `kind`.
    Failed(ErrorKind),
}

impl CallOutcome {
    /// A call's result as a breaker outcome.
    #[must_use]
    pub fn of(result: Result<(), ErrorKind>) -> Self {
        match result {
            Ok(()) => Self::Ok,
            Err(kind) => Self::Failed(kind),
        }
    }

    /// Whether a BYOK breaker counts this as a failure. Only transient errors
    /// and rate limits do; a refusal, a bad request or an invalid key is not a
    /// reliability signal.
    fn counts_as_breaker_failure(self) -> bool {
        matches!(
            self,
            Self::Failed(ErrorKind::Transient | ErrorKind::RateLimited)
        )
    }
}

/// The operator node's reachability, shared by every call to it (it is one
/// node, G3-25). Behind a mutex; cheap to read.
#[derive(Debug)]
pub struct OperatorBreaker {
    inner: Mutex<OperatorState>,
}

#[derive(Debug)]
struct OperatorState {
    state: ProviderState,
    consecutive_transient: u32,
    /// Users whose `ai.drain` queue was paused because the node refused the
    /// key; resumed when the node recovers.
    paused: HashSet<String>,
    /// Users who have made an operator call, so a status change reaches their
    /// stream even when they did not trigger it (owner-only today, E4).
    observers: HashSet<String>,
}

/// What a status update means for the service: whether to publish and what to
/// do about the user's AI queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transition {
    /// Nothing changed.
    None,
    /// The state changed to `state`; publish `provider.status`.
    Changed(ProviderState),
}

impl OperatorBreaker {
    /// A reachable operator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(OperatorState {
                state: ProviderState::Ok,
                consecutive_transient: 0,
                paused: HashSet::new(),
                observers: HashSet::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, OperatorState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> ProviderState {
        self.lock().state
    }

    /// Records that `user` uses the operator, so a status change reaches them.
    pub fn observe(&self, user: &str) {
        self.lock().observers.insert(user.to_owned());
    }

    /// The users to send a status change to.
    #[must_use]
    pub fn observers(&self) -> Vec<String> {
        self.lock().observers.iter().cloned().collect()
    }

    /// Adds `user` to the paused set without changing the state (a call that
    /// found the key already refused).
    pub fn mark_paused(&self, user: &str) {
        self.lock().paused.insert(user.to_owned());
    }

    /// Whether calls are held right now (offline or key refused).
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        matches!(
            self.lock().state,
            ProviderState::Offline | ProviderState::InvalidKey
        )
    }

    /// Records a successful or reachable call: the node answered, so it is
    /// `ok` again. Returns the transition and the users to resume.
    pub fn reachable(&self) -> (Transition, Vec<String>) {
        let mut inner = self.lock();
        inner.consecutive_transient = 0;
        if inner.state == ProviderState::Ok {
            return (Transition::None, Vec::new());
        }
        inner.state = ProviderState::Ok;
        let resumed = inner.paused.drain().collect();
        (Transition::Changed(ProviderState::Ok), resumed)
    }

    /// Records a failed call. `user` is the caller, for the pause on an invalid
    /// key. Returns the transition.
    pub fn failed(&self, kind: ErrorKind, user: &str) -> Transition {
        let mut inner = self.lock();
        match kind {
            ErrorKind::InvalidKey => {
                inner.paused.insert(user.to_owned());
                inner.consecutive_transient = 0;
                set_state(&mut inner.state, ProviderState::InvalidKey)
            }
            ErrorKind::Offline => {
                inner.consecutive_transient = 0;
                set_state(&mut inner.state, ProviderState::Offline)
            }
            ErrorKind::Transient | ErrorKind::RateLimited => {
                inner.consecutive_transient += 1;
                if inner.consecutive_transient >= OPERATOR_TRANSIENT_TO_OFFLINE
                    && inner.state == ProviderState::Ok
                {
                    set_state(&mut inner.state, ProviderState::Offline)
                } else {
                    Transition::None
                }
            }
            // The node answered (a refusal, a bad request, a schema miss): it
            // is reachable, so the transient streak is broken. The state is
            // cleared only by a success or a health probe ([`Self::reachable`]),
            // which also resumes the users an invalid key paused.
            _ => {
                inner.consecutive_transient = 0;
                Transition::None
            }
        }
    }
}

impl Default for OperatorBreaker {
    fn default() -> Self {
        Self::new()
    }
}

fn set_state(state: &mut ProviderState, next: ProviderState) -> Transition {
    if *state == next {
        Transition::None
    } else {
        *state = next;
        Transition::Changed(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "01J9Z3B8K4QW6TFX0V7G2N5RCA";
    const PROVIDER: &str = "byok-1";

    fn transient() -> CallOutcome {
        CallOutcome::Failed(ErrorKind::Transient)
    }

    #[tokio::test(start_paused = true)]
    async fn a_byok_breaker_opens_after_five_in_a_row_and_recovers_on_a_probe() {
        let breakers = UserBreakers::new();
        let now = Instant::now();
        assert_eq!(breakers.admit(USER, PROVIDER, now), Admission::Allow);
        for _ in 0..5 {
            breakers.record(USER, PROVIDER, &transient(), now);
        }
        assert_eq!(breakers.admit(USER, PROVIDER, now), Admission::Refuse);
        assert_eq!(breakers.state(USER, PROVIDER, now), ProviderState::Down);

        // After the cool-down one probe is let through; a success closes it.
        let later = now + Duration::from_secs(61);
        assert_eq!(breakers.admit(USER, PROVIDER, later), Admission::Probe);
        breakers.record(USER, PROVIDER, &CallOutcome::Ok, later);
        assert_eq!(breakers.admit(USER, PROVIDER, later), Admission::Allow);
        assert_eq!(breakers.state(USER, PROVIDER, later), ProviderState::Ok);
    }

    #[tokio::test(start_paused = true)]
    async fn a_byok_breaker_ignores_non_reliability_failures() {
        let breakers = UserBreakers::new();
        let now = Instant::now();
        for _ in 0..10 {
            breakers.record(
                USER,
                PROVIDER,
                &CallOutcome::Failed(ErrorKind::BadRequest),
                now,
            );
        }
        assert_eq!(breakers.admit(USER, PROVIDER, now), Admission::Allow);
    }

    #[test]
    fn the_operator_goes_offline_on_connect_and_on_two_timeouts() {
        let breaker = OperatorBreaker::new();
        assert_eq!(
            breaker.failed(ErrorKind::Offline, USER),
            Transition::Changed(ProviderState::Offline)
        );
        assert!(breaker.is_blocked());
        let (back, resumed) = breaker.reachable();
        assert_eq!(back, Transition::Changed(ProviderState::Ok));
        assert!(resumed.is_empty());

        // One transient alone does not; two in a row do.
        assert_eq!(breaker.failed(ErrorKind::Transient, USER), Transition::None);
        assert_eq!(
            breaker.failed(ErrorKind::Transient, USER),
            Transition::Changed(ProviderState::Offline)
        );
    }

    /// Every kind the adapters return, as `AiServiceError::Call` carries it.
    const KINDS: [ErrorKind; 10] = [
        ErrorKind::Offline,
        ErrorKind::InvalidKey,
        ErrorKind::RateLimited,
        ErrorKind::QuotaExhausted,
        ErrorKind::Transient,
        ErrorKind::BadRequest,
        ErrorKind::Refused,
        ErrorKind::SchemaInvalid,
        ErrorKind::Unsupported,
        ErrorKind::Cancelled,
    ];

    #[test]
    fn every_kind_moves_the_operator_as_its_action_needs() {
        // Offline holds at once; an invalid key pauses; a transient error or
        // a rate limit is backed off, and only two in a row hold. The rest
        // mean the node answered. A new kind fails to compile here.
        for kind in KINDS {
            let (first, second) = match kind {
                ErrorKind::Offline => (Some(ProviderState::Offline), None),
                ErrorKind::InvalidKey => (Some(ProviderState::InvalidKey), None),
                ErrorKind::Transient | ErrorKind::RateLimited => {
                    (None, Some(ProviderState::Offline))
                }
                ErrorKind::QuotaExhausted
                | ErrorKind::BadRequest
                | ErrorKind::Refused
                | ErrorKind::SchemaInvalid
                | ErrorKind::Unsupported
                | ErrorKind::Cancelled => (None, None),
            };
            let breaker = OperatorBreaker::new();
            let transition =
                |state: Option<ProviderState>| state.map_or(Transition::None, Transition::Changed);
            assert_eq!(breaker.failed(kind, USER), transition(first), "{kind}");
            assert_eq!(breaker.failed(kind, USER), transition(second), "{kind}");
            assert_eq!(breaker.is_blocked(), first.or(second).is_some(), "{kind}");
        }
    }

    #[test]
    fn an_invalid_key_pauses_the_user_and_recovery_resumes_them() {
        let breaker = OperatorBreaker::new();
        assert_eq!(
            breaker.failed(ErrorKind::InvalidKey, USER),
            Transition::Changed(ProviderState::InvalidKey)
        );
        // A second failure for the same key does not re-publish.
        assert_eq!(
            breaker.failed(ErrorKind::InvalidKey, USER),
            Transition::None
        );
        let (back, resumed) = breaker.reachable();
        assert_eq!(back, Transition::Changed(ProviderState::Ok));
        assert_eq!(resumed, vec![USER.to_owned()]);
    }
}
