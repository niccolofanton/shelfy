//! The scheduler's memory (plan §2.12): for each kind, a FIFO of ready jobs
//! per user, served round robin; the delayed jobs by `run_at`; the running
//! attempts; the paused queues.
//!
//! It mirrors the `queued` and `running` rows of `jobs` and is rebuilt from
//! them at boot. It only decides what to try next: a claim in the database
//! ([`crate::control::jobs::claim`]) has the last word, so a stale entry
//! costs a failed claim, never a duplicate run.
//!
//! **Fairness.** A user with ready jobs of a kind holds one place in that
//! kind's turn order. Starting a job moves its user to the back, so each
//! user gets one job per round: a user with 6,000 queued jobs and a user
//! with one alternate, and the second user's job starts within one round.
//! Within a user's queue, lower `priority` runs first, then older jobs.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicI64;

use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;

use super::registry::Registry;

/// A queued job's place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Waiting {
    pub(super) kind: &'static str,
    pub(super) user: Arc<str>,
    pub(super) priority: i64,
    pub(super) run_at: i64,
    /// Due (in a ready queue) or waiting for `run_at`.
    ready: bool,
}

/// Why the scheduler stopped a running attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stop {
    /// The user cancelled the job: the database already says `cancelled`.
    Cancelled,
    /// Its lease expired: the watchdog already queued (or failed) the job.
    LeaseLost,
}

/// A running attempt. Its entry holds the attempt's slot: whoever removes
/// the entry gives the slot back ([`Queues::end_run`]).
pub(super) struct Running {
    /// Unique per attempt in this process; tells attempts of one job apart.
    pub(super) run: u64,
    pub(super) kind: &'static str,
    pub(super) user: Arc<str>,
    /// Fires to stop the worker.
    pub(super) token: CancellationToken,
    /// Set when the scheduler stopped the attempt.
    pub(super) stop: Option<Stop>,
    /// The worker's last sign of life, unix ms.
    pub(super) alive: Arc<AtomicI64>,
    /// The worker task, once spawned.
    pub(super) worker: Option<AbortHandle>,
}

/// One kind's ready jobs and running counts.
#[derive(Debug, Default)]
struct KindQueue {
    global: usize,
    per_user: usize,
    running: usize,
    running_per_user: HashMap<Arc<str>, usize>,
    /// Ready jobs per user, as `(priority, id)`: the lowest starts first.
    ready: HashMap<Arc<str>, BTreeSet<(i64, i64)>>,
    /// Users with ready jobs, in turn order.
    rotation: VecDeque<Arc<str>>,
}

/// What the scheduler holds of one kind, for metrics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindStats {
    /// The kind.
    pub kind: &'static str,
    /// Jobs due and waiting for a slot.
    pub ready: usize,
    /// Jobs waiting for their `run_at`.
    pub delayed: usize,
    /// Attempts running.
    pub running: usize,
}

/// The scheduler's memory.
#[derive(Default)]
pub(super) struct Queues {
    kinds: HashMap<&'static str, KindQueue>,
    waiting: HashMap<i64, Waiting>,
    /// Delayed jobs, as `(run_at, id)`.
    delayed: BTreeSet<(i64, i64)>,
    pub(super) running: HashMap<i64, Running>,
    /// Paused users, by kind.
    paused: HashMap<&'static str, HashSet<Arc<str>>>,
}

impl Queues {
    /// Empty queues for the kinds of `registry`.
    pub(super) fn new(registry: &Registry) -> Self {
        let kinds = registry
            .kinds()
            .map(|kind| {
                let spec = kind.spec();
                let queue = KindQueue {
                    global: spec.global,
                    per_user: spec.per_user,
                    ..KindQueue::default()
                };
                (kind.name(), queue)
            })
            .collect();
        Self {
            kinds,
            ..Self::default()
        }
    }

    /// Adds a queued job, or moves it when it is already here. False when
    /// its kind is not registered, or an attempt of it still runs.
    pub(super) fn add(
        &mut self,
        id: i64,
        kind: &str,
        user: &str,
        priority: i64,
        run_at: i64,
        now: i64,
    ) -> bool {
        let Some((&kind, _)) = self.kinds.get_key_value(kind) else {
            return false;
        };
        if self.running.contains_key(&id) {
            return false;
        }
        self.remove(id);
        let user: Arc<str> = user.into();
        let ready = run_at <= now;
        if ready {
            self.push_ready(kind, &user, priority, id);
        } else {
            self.delayed.insert((run_at, id));
        }
        self.waiting.insert(
            id,
            Waiting {
                kind,
                user,
                priority,
                run_at,
                ready,
            },
        );
        true
    }

    fn push_ready(&mut self, kind: &'static str, user: &Arc<str>, priority: i64, id: i64) {
        let Some(queue) = self.kinds.get_mut(kind) else {
            return;
        };
        let jobs = queue.ready.entry(Arc::clone(user)).or_default();
        if jobs.is_empty() {
            queue.rotation.push_back(Arc::clone(user));
        }
        jobs.insert((priority, id));
    }

    /// Takes a waiting job out; `None` when it is not waiting here.
    pub(super) fn remove(&mut self, id: i64) -> Option<Waiting> {
        let waiting = self.waiting.remove(&id)?;
        if waiting.ready {
            if let Some(queue) = self.kinds.get_mut(waiting.kind)
                && let Some(jobs) = queue.ready.get_mut(&waiting.user)
            {
                jobs.remove(&(waiting.priority, id));
                if jobs.is_empty() {
                    queue.ready.remove(&waiting.user);
                    queue.rotation.retain(|user| *user != waiting.user);
                }
            }
        } else {
            self.delayed.remove(&(waiting.run_at, id));
        }
        Some(waiting)
    }

    /// Moves the delayed jobs due at `now` into their ready queues.
    pub(super) fn promote(&mut self, now: i64) {
        while let Some(&(run_at, id)) = self.delayed.first() {
            if run_at > now {
                break;
            }
            self.delayed.pop_first();
            let Some(waiting) = self.waiting.get_mut(&id) else {
                continue;
            };
            waiting.ready = true;
            let (kind, user, priority) =
                (waiting.kind, Arc::clone(&waiting.user), waiting.priority);
            self.push_ready(kind, &user, priority, id);
        }
    }

    /// The earliest `run_at` of the delayed jobs.
    pub(super) fn next_delayed(&self) -> Option<i64> {
        self.delayed.first().map(|&(run_at, _)| run_at)
    }

    /// The next job of `kind` to start, taking its slot: the first user in
    /// turn order whose queue is not paused and who is under the per-user
    /// limit; `None` when the kind is at its global limit or nobody can
    /// start.
    pub(super) fn pick(&mut self, kind: &str) -> Option<(i64, Waiting)> {
        let Self {
            kinds,
            waiting,
            paused,
            ..
        } = self;
        let queue = kinds.get_mut(kind)?;
        if queue.running >= queue.global {
            return None;
        }
        let paused = paused.get(kind);
        let turn = queue.rotation.iter().position(|user| {
            !paused.is_some_and(|users| users.contains(user))
                && queue.running_per_user.get(user).copied().unwrap_or(0) < queue.per_user
        })?;
        let user = queue.rotation.remove(turn)?;
        let (_, id) = queue.ready.get_mut(&user)?.pop_first()?;
        if queue.ready.get(&user).is_some_and(|jobs| !jobs.is_empty()) {
            queue.rotation.push_back(Arc::clone(&user));
        } else {
            queue.ready.remove(&user);
        }
        let job = waiting.remove(&id)?;
        queue.running += 1;
        *queue.running_per_user.entry(user).or_default() += 1;
        Some((id, job))
    }

    /// Gives back a slot of `kind` for `user`.
    pub(super) fn release(&mut self, kind: &str, user: &str) {
        let Some(queue) = self.kinds.get_mut(kind) else {
            return;
        };
        queue.running = queue.running.saturating_sub(1);
        if let Some(count) = queue.running_per_user.get_mut(user) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                queue.running_per_user.remove(user);
            }
        }
    }

    /// Ends the attempt `run` of job `id` and gives its slot back; `None`
    /// when that attempt already ended.
    pub(super) fn end_run(&mut self, id: i64, run: u64) -> Option<Running> {
        if self
            .running
            .get(&id)
            .is_none_or(|running| running.run != run)
        {
            return None;
        }
        let running = self.running.remove(&id)?;
        self.release(running.kind, &running.user);
        Some(running)
    }

    /// Pauses or resumes `user`'s queue of `kind`.
    pub(super) fn set_paused(&mut self, user: &str, kind: &'static str, paused: bool) {
        let users = self.paused.entry(kind).or_default();
        if paused {
            users.insert(user.into());
        } else {
            users.remove(user);
        }
    }

    /// Whether `user` paused `kind`.
    pub(super) fn is_paused(&self, user: &str, kind: &str) -> bool {
        self.paused
            .get(kind)
            .is_some_and(|users| users.contains(user))
    }

    /// Counts per registered kind, by name.
    pub(super) fn stats(&self) -> Vec<KindStats> {
        let mut stats: Vec<KindStats> = self
            .kinds
            .iter()
            .map(|(&kind, queue)| KindStats {
                kind,
                ready: queue.ready.values().map(BTreeSet::len).sum(),
                delayed: 0,
                running: queue.running,
            })
            .collect();
        for waiting in self.waiting.values().filter(|w| !w.ready) {
            if let Some(entry) = stats.iter_mut().find(|s| s.kind == waiting.kind) {
                entry.delayed += 1;
            }
        }
        stats.sort_by_key(|s| s.kind);
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{JobContext, JobResult, Kind, KindSpec, Outcome};

    const A: &str = "01USERA0000000000000000000";
    const B: &str = "01USERB0000000000000000000";
    const C: &str = "01USERC0000000000000000000";
    const NOW: i64 = 1_000_000;

    async fn noop(_ctx: JobContext) -> JobResult {
        Ok(Outcome::Succeeded)
    }

    fn queues(global: usize, per_user: usize) -> Queues {
        let registry = Registry::new().register(Kind::new(
            KindSpec::new("k").global(global).per_user(per_user),
            noop,
        ));
        Queues::new(&registry)
    }

    /// Starts every job it can, then finishes them all; returns the order.
    fn drain(q: &mut Queues) -> Vec<i64> {
        let mut order = Vec::new();
        loop {
            let mut started = Vec::new();
            while let Some((id, job)) = q.pick("k") {
                started.push((id, job));
            }
            if started.is_empty() {
                return order;
            }
            for (id, job) in started {
                order.push(id);
                q.release(job.kind, &job.user);
            }
        }
    }

    #[test]
    fn users_take_turns() {
        let mut q = queues(1, 1);
        for id in 1..=6 {
            q.add(id, "k", A, 100, NOW, NOW);
        }
        q.add(7, "k", B, 100, NOW, NOW);
        q.add(8, "k", C, 100, NOW, NOW);
        q.add(9, "k", B, 100, NOW, NOW);
        assert_eq!(drain(&mut q), [1, 7, 8, 2, 9, 3, 4, 5, 6]);
    }

    #[test]
    fn a_newcomer_waits_at_most_one_round() {
        let mut q = queues(1, 1);
        for id in 1..=6_000 {
            q.add(id, "k", A, 100, NOW, NOW);
        }
        let (first, job) = q.pick("k").unwrap();
        assert_eq!(first, 1);
        // B arrives while A's first job runs.
        q.add(10_000, "k", B, 100, NOW, NOW);
        q.release(job.kind, &job.user);
        let (second, job) = q.pick("k").unwrap();
        q.release(job.kind, &job.user);
        let (third, _) = q.pick("k").unwrap();
        assert_eq!(
            (second, third),
            (2, 10_000),
            "B starts after one more turn of A"
        );
    }

    #[test]
    fn limits_pauses_and_priorities() {
        let mut q = queues(3, 2);
        for id in 1..=4 {
            q.add(id, "k", A, 100, NOW, NOW);
        }
        q.add(5, "k", A, 10, NOW, NOW);
        q.add(6, "k", B, 100, NOW, NOW);
        q.set_paused(B, "k", true);
        let picked: Vec<i64> = std::iter::from_fn(|| q.pick("k").map(|(id, _)| id)).collect();
        assert_eq!(picked, [5, 1], "per-user cap 2, B paused, priority first");
        assert!(q.is_paused(B, "k"));
        q.set_paused(B, "k", false);
        assert_eq!(q.pick("k").map(|(id, _)| id), Some(6));
        assert_eq!(q.pick("k"), None, "global cap 3");
        assert_eq!(
            q.stats(),
            [KindStats {
                kind: "k",
                ready: 3,
                delayed: 0,
                running: 3
            }]
        );
    }

    #[test]
    fn delayed_jobs_become_ready_on_time() {
        let mut q = queues(1, 1);
        q.add(1, "k", A, 100, NOW + 500, NOW);
        q.add(2, "k", A, 100, NOW + 100, NOW);
        assert_eq!(q.next_delayed(), Some(NOW + 100));
        assert_eq!(q.pick("k"), None);
        q.promote(NOW + 100);
        assert_eq!(q.next_delayed(), Some(NOW + 500));
        assert_eq!(q.stats()[0].delayed, 1);
        assert_eq!(drain(&mut q), [2]);
        q.promote(NOW + 1_000);
        assert_eq!(drain(&mut q), [1]);
        assert_eq!(q.next_delayed(), None);
    }

    #[test]
    fn adding_again_moves_a_job_and_removing_forgets_it() {
        let mut q = queues(1, 1);
        assert!(!q.add(1, "other", A, 100, NOW, NOW), "unknown kind");
        q.add(1, "k", A, 100, NOW + 1_000, NOW);
        q.add(1, "k", A, 100, NOW, NOW);
        assert_eq!(q.next_delayed(), None, "moved to the ready queue");
        q.add(2, "k", B, 100, NOW, NOW);
        assert_eq!(q.remove(1).map(|w| w.run_at), Some(NOW));
        assert_eq!(q.remove(1), None);
        assert_eq!(drain(&mut q), [2]);

        // A job whose attempt still runs is not queued twice.
        q.add(3, "k", A, 100, NOW, NOW);
        let (id, job) = q.pick("k").unwrap();
        q.running.insert(
            id,
            Running {
                run: 1,
                kind: job.kind,
                user: Arc::clone(&job.user),
                token: CancellationToken::new(),
                stop: None,
                alive: Arc::new(AtomicI64::new(NOW)),
                worker: None,
            },
        );
        assert!(!q.add(3, "k", A, 100, NOW, NOW));
        assert!(q.end_run(3, 2).is_none(), "another attempt");
        assert!(q.end_run(3, 1).is_some());
        assert!(q.end_run(3, 1).is_none(), "ends once");
        assert!(q.add(3, "k", A, 100, NOW, NOW));
        assert_eq!(drain(&mut q), [3], "the slot came back");
    }
}
