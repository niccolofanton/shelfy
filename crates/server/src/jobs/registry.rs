//! The job-kind registry: what each kind of job is allowed (its row of the
//! §2.12 table) and the code that runs it.
//!
//! A kind is a [`KindSpec`] plus a [`Worker`], and optionally a [`Sweep`]
//! (the drain sweeper's check). The server's kinds are registered in
//! [`super::kinds::registry`]; tests build registries of their own.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::JobError;
use super::context::{JobContext, JobResult, SweepContext};

/// A boxed, sendable future: what workers and sweeps return.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Delays between the tries of a job: `base × 2^(n−1)` after the n-th
/// failed try, capped at `max`, with jitter (see [`Backoff::jittered`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    /// The delay after the first failed try.
    pub base: Duration,
    /// The longest delay.
    pub max: Duration,
}

impl Backoff {
    /// The default of the plan (§2.12, archive items): 30 s × 2ⁿ, at most
    /// 6 h.
    pub const DEFAULT: Self = Self::new(Duration::from_secs(30), Duration::from_secs(6 * 3600));

    /// A backoff from `base` up to `max`.
    #[must_use]
    pub const fn new(base: Duration, max: Duration) -> Self {
        Self { base, max }
    }

    /// The delay after `failures` failed tries (at least 1), without jitter.
    #[must_use]
    pub fn delay(&self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(31);
        self.base
            .checked_mul(1_u32 << doublings)
            .map_or(self.max, |d| d.min(self.max))
    }

    /// [`Backoff::delay`] with equal jitter: a point in its upper half
    /// chosen by `random`, so retries of many jobs that failed together
    /// spread out, while none comes back sooner than half the delay.
    #[must_use]
    pub fn jittered(&self, failures: u32, random: u64) -> Duration {
        let delay = self.delay(failures).as_nanos();
        let half = delay / 2;
        let spread = delay - half + 1;
        let nanos = half + u128::from(random) % spread;
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

/// The limits and the retry policy of a kind of job (a row of the §2.12
/// table). Build one with [`KindSpec::new`] and the setters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KindSpec {
    /// The name, stored in `jobs.kind` and used in the queue routes
    /// (`/queues/{kind}/pause`): lowercase letters, digits, `.`, `_` and
    /// `-`, at most 64 characters (`archive.drain`, `migrate`).
    pub name: &'static str,
    /// Jobs of this kind running at once, over every user.
    pub global: usize,
    /// Jobs of this kind running at once for one user. Users take turns
    /// (round robin), so one user's backlog never starves another user.
    pub per_user: usize,
    /// Tries before a job fails for good. A transient error and an expired
    /// lease use one; a permanent error fails the job at once.
    pub max_attempts: u32,
    /// How long a running job may go without showing it is alive (see
    /// [`JobContext::heartbeat`]) before the scheduler presumes it dead:
    /// its worker is stopped and the job queued again, using a try.
    pub lease: Duration,
    /// Delays between tries.
    pub backoff: Backoff,
    /// Enqueued for every active user at 03:00 UTC, with the dedupe key
    /// `nightly` (purge, GC, usage recomputation).
    pub nightly: bool,
}

impl KindSpec {
    /// A kind with the defaults: one job at a time, per user and overall;
    /// 3 tries; a 5-minute lease; [`Backoff::DEFAULT`]; not nightly.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            global: 1,
            per_user: 1,
            max_attempts: 3,
            lease: Duration::from_secs(300),
            backoff: Backoff::DEFAULT,
            nightly: false,
        }
    }

    /// Sets [`KindSpec::global`].
    #[must_use]
    pub const fn global(mut self, global: usize) -> Self {
        self.global = global;
        self
    }

    /// Sets [`KindSpec::per_user`].
    #[must_use]
    pub const fn per_user(mut self, per_user: usize) -> Self {
        self.per_user = per_user;
        self
    }

    /// Sets [`KindSpec::max_attempts`].
    #[must_use]
    pub const fn max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Sets [`KindSpec::lease`].
    #[must_use]
    pub const fn lease(mut self, lease: Duration) -> Self {
        self.lease = lease;
        self
    }

    /// Sets [`KindSpec::backoff`].
    #[must_use]
    pub const fn backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    /// Sets [`KindSpec::nightly`].
    #[must_use]
    pub const fn nightly(mut self, nightly: bool) -> Self {
        self.nightly = nightly;
        self
    }

    /// Why this spec is unusable, if it is.
    fn problem(&self) -> Option<&'static str> {
        let name_ok = (1..=64).contains(&self.name.len())
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
        if !name_ok {
            Some("the name must be 1–64 characters among a–z, 0–9, '.', '_' and '-'")
        } else if self.global == 0 || self.per_user == 0 {
            Some("global and per_user must be at least 1")
        } else if self.max_attempts == 0 {
            Some("max_attempts must be at least 1")
        } else if self.lease < Duration::from_millis(3) {
            Some("the lease must be at least 3 ms")
        } else {
            None
        }
    }
}

/// The code that runs a kind of job.
///
/// Any `Fn(JobContext) -> impl Future<Output = JobResult>` is a worker, so
/// an `async fn run(ctx: JobContext) -> JobResult` registers as is.
pub trait Worker: Send + Sync + 'static {
    /// Runs one try of the job of `ctx`.
    fn run(&self, ctx: JobContext) -> BoxFuture<JobResult>;
}

impl<F, Fut> Worker for F
where
    F: Fn(JobContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = JobResult> + Send + 'static,
{
    fn run(&self, ctx: JobContext) -> BoxFuture<JobResult> {
        Box::pin(self(ctx))
    }
}

/// The drain sweeper's check (plan §2.12): every 10 minutes, for each active
/// user without an active job of the kind, it says whether the user has
/// pending items. `Some(run_at)` enqueues the drain (dedupe key = the kind)
/// to run at `run_at`; `None` means nothing is pending.
///
/// Like [`Worker`], any `Fn(SweepContext) -> impl Future` qualifies.
pub trait Sweep: Send + Sync + 'static {
    /// Checks the user of `ctx`.
    fn pending(&self, ctx: SweepContext) -> BoxFuture<Result<Option<i64>, JobError>>;
}

impl<F, Fut> Sweep for F
where
    F: Fn(SweepContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Option<i64>, JobError>> + Send + 'static,
{
    fn pending(&self, ctx: SweepContext) -> BoxFuture<Result<Option<i64>, JobError>> {
        Box::pin(self(ctx))
    }
}

/// A registered kind: its spec, its worker and, for drains, its sweep.
#[derive(Clone)]
pub struct Kind {
    spec: KindSpec,
    worker: Arc<dyn Worker>,
    sweep: Option<Arc<dyn Sweep>>,
}

impl Kind {
    /// The kind `spec`, run by `worker`.
    #[must_use]
    pub fn new(spec: KindSpec, worker: impl Worker) -> Self {
        Self {
            spec,
            worker: Arc::new(worker),
            sweep: None,
        }
    }

    /// Adds the drain sweeper's check.
    #[must_use]
    pub fn with_sweep(mut self, sweep: impl Sweep) -> Self {
        self.sweep = Some(Arc::new(sweep));
        self
    }

    /// The spec.
    #[must_use]
    pub fn spec(&self) -> &KindSpec {
        &self.spec
    }

    /// The name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.spec.name
    }

    pub(super) fn worker(&self) -> &Arc<dyn Worker> {
        &self.worker
    }

    pub(super) fn sweep(&self) -> Option<&Arc<dyn Sweep>> {
        self.sweep.as_ref()
    }
}

impl fmt::Debug for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Kind")
            .field("spec", &self.spec)
            .field("sweep", &self.sweep.is_some())
            .finish_non_exhaustive()
    }
}

/// Every kind the scheduler runs, by name. Cheap to clone.
#[derive(Clone, Default)]
pub struct Registry {
    kinds: Arc<BTreeMap<&'static str, Kind>>,
}

impl Registry {
    /// No kinds.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `kind`.
    ///
    /// # Panics
    ///
    /// When the name is taken or the spec is unusable: a programming error,
    /// found when the server builds its registry at start.
    #[must_use]
    pub fn register(mut self, kind: Kind) -> Self {
        if let Some(problem) = kind.spec.problem() {
            panic!("job kind {:?}: {problem}", kind.spec.name);
        }
        let kinds = Arc::make_mut(&mut self.kinds);
        assert!(
            !kinds.contains_key(kind.spec.name),
            "job kind {:?} is registered twice",
            kind.spec.name
        );
        kinds.insert(kind.spec.name, kind);
        self
    }

    /// The kind called `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Kind> {
        self.kinds.get(name)
    }

    /// Every kind, by name.
    pub fn kinds(&self) -> impl Iterator<Item = &Kind> {
        self.kinds.values()
    }

    /// Whether no kind is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.kinds.keys()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::Outcome;

    async fn noop(_ctx: JobContext) -> JobResult {
        Ok(Outcome::Succeeded)
    }

    #[test]
    fn backoff_doubles_up_to_its_cap() {
        let backoff = Backoff::DEFAULT;
        assert_eq!(backoff.delay(1), Duration::from_secs(30));
        assert_eq!(backoff.delay(2), Duration::from_secs(60));
        assert_eq!(backoff.delay(5), Duration::from_secs(480));
        assert_eq!(backoff.delay(10), Duration::from_secs(15_360));
        assert_eq!(backoff.delay(11), Duration::from_secs(6 * 3600));
        assert_eq!(backoff.delay(u32::MAX), Duration::from_secs(6 * 3600));
        assert_eq!(backoff.delay(0), Duration::from_secs(30));
    }

    #[test]
    fn jitter_stays_in_the_upper_half() {
        let backoff = Backoff::new(Duration::from_secs(10), Duration::from_secs(60));
        let full = backoff.delay(2);
        for random in [0, 1, 7, u64::MAX / 3, u64::MAX] {
            let d = backoff.jittered(2, random);
            assert!(d >= full / 2 && d <= full, "{d:?}");
        }
        assert_eq!(backoff.jittered(2, 0), full / 2);
    }

    #[test]
    fn kinds_are_registered_once_by_name() {
        let registry = Registry::new()
            .register(Kind::new(KindSpec::new("b.two"), noop))
            .register(Kind::new(
                KindSpec::new("a.one").global(4).per_user(2),
                noop,
            ));
        let names: Vec<_> = registry.kinds().map(Kind::name).collect();
        assert_eq!(names, ["a.one", "b.two"]);
        assert_eq!(registry.get("a.one").unwrap().spec().global, 4);
        assert!(registry.get("c").is_none());
        assert_eq!(format!("{registry:?}"), r#"["a.one", "b.two"]"#);

        let twice = std::panic::catch_unwind(|| {
            Registry::new()
                .register(Kind::new(KindSpec::new("x"), noop))
                .register(Kind::new(KindSpec::new("x"), noop))
        });
        assert!(twice.is_err());
        for bad in [
            KindSpec::new("Upper"),
            KindSpec::new(""),
            KindSpec::new("a/b"),
            KindSpec::new("x").global(0),
            KindSpec::new("x").max_attempts(0),
            KindSpec::new("x").lease(Duration::ZERO),
        ] {
            let refused =
                std::panic::catch_unwind(|| Registry::new().register(Kind::new(bad, noop)));
            assert!(refused.is_err(), "{bad:?}");
        }
    }
}
