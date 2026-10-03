//! The job system in tests: a scripted worker that records every try, test
//! kinds on a tokio clock, users in the control database, and waiting for a
//! job to reach a state.
//!
//! A job's payload scripts its worker: `{"mode": "ok"}`, or `{"modes": [...]}`
//! with one mode per try (the last one repeats). Modes:
//!
//! | Mode | The worker |
//! |---|---|
//! | `ok` | succeeds |
//! | `transient`, `permanent` | fails so (code `unavailable` or `validation_failed`) |
//! | `limited` | fails transiently with `Retry-After: 120 s` |
//! | `wait` | runs until its token fires, then returns `cancelled` |
//! | `hang` | never returns and shows no sign of life |
//! | `gate` | waits for a permit of [`Probe::open`], then succeeds |
//! | `alive` | heartbeats every 10 s for 60 s, then succeeds |
//! | `drain` | reports progress every second until paused or cancelled; succeeds on its second start |
//! | `progress` | reports progress 100 times, 10 ms apart, then succeeds |

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use shelfy_server::control::jobs::JobRow;
use shelfy_server::events::Delivery;
use shelfy_server::ids::now_ms;
use shelfy_server::jobs::{
    Backoff, BoxFuture, Clock, JobContext, JobError, JobResult, Kind, KindSpec, NewJob, Outcome,
    Registry,
};
use tokio::sync::{Semaphore, watch};

use super::TestState;

/// 2026-10-02T01:00:00Z: the tokio clock of the tests starts here.
pub const START: i64 = 1_790_902_800_000;

/// One try, as the worker saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Start {
    /// Job id.
    pub id: i64,
    /// The user.
    pub user: String,
    /// The try, from 1.
    pub attempt: u32,
}

/// Records the tries of the scripted worker and drives `gate` jobs.
#[derive(Clone)]
pub struct Probe {
    inner: Arc<ProbeInner>,
}

struct ProbeInner {
    starts: Mutex<Vec<Start>>,
    count: watch::Sender<usize>,
    gate: Semaphore,
    stopped: Mutex<Vec<i64>>,
    /// Every mode runs as `ok`: the worker of a server that "restarted".
    all_ok: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Records that a `hang` or `wait` try was stopped (dropped or cancelled).
struct StopGuard(Probe, i64);

impl Drop for StopGuard {
    fn drop(&mut self) {
        lock(&self.0.inner.stopped).push(self.1);
    }
}

impl Probe {
    /// A probe whose worker follows the payload.
    pub fn new() -> Self {
        Self::with(false)
    }

    /// A probe whose worker succeeds whatever the payload says.
    pub fn all_ok() -> Self {
        Self::with(true)
    }

    fn with(all_ok: bool) -> Self {
        Self {
            inner: Arc::new(ProbeInner {
                starts: Mutex::new(Vec::new()),
                count: watch::Sender::new(0),
                gate: Semaphore::new(0),
                stopped: Mutex::new(Vec::new()),
                all_ok,
            }),
        }
    }

    /// Every try so far, in start order.
    pub fn starts(&self) -> Vec<Start> {
        lock(&self.inner.starts).clone()
    }

    /// The ids of the jobs started so far, in start order.
    pub fn started_ids(&self) -> Vec<i64> {
        self.starts().into_iter().map(|s| s.id).collect()
    }

    /// The `hang` and `wait` tries that were stopped, by job id.
    pub fn stopped(&self) -> Vec<i64> {
        lock(&self.inner.stopped).clone()
    }

    /// Waits until `n` tries have started.
    pub async fn wait_starts(&self, n: usize) {
        let mut count = self.inner.count.subscribe();
        tokio::time::timeout(Duration::from_secs(24 * 3600), count.wait_for(|c| *c >= n))
            .await
            .unwrap_or_else(|_| panic!("{n} tries never started: {:?}", self.starts()))
            .expect("the probe lives");
    }

    /// Lets `n` `gate` tries finish.
    pub fn open(&self, n: usize) {
        self.inner.gate.add_permits(n);
    }

    /// The scripted worker.
    pub fn worker(&self) -> impl Fn(JobContext) -> BoxFuture<JobResult> + Send + Sync + 'static {
        let probe = self.clone();
        move |ctx: JobContext| -> BoxFuture<JobResult> { Box::pin(probe.clone().run(ctx)) }
    }

    fn mode(&self, ctx: &JobContext) -> String {
        if self.inner.all_ok {
            return "ok".into();
        }
        let payload = ctx.payload();
        if let Some(modes) = payload["modes"].as_array() {
            let index = (ctx.attempt() as usize - 1).min(modes.len() - 1);
            return modes[index].as_str().unwrap_or("ok").to_owned();
        }
        payload["mode"].as_str().unwrap_or("ok").to_owned()
    }

    async fn run(self, ctx: JobContext) -> JobResult {
        let mode = self.mode(&ctx);
        let starts_of_job = {
            let mut starts = lock(&self.inner.starts);
            starts.push(Start {
                id: ctx.id(),
                user: ctx.user_id().to_owned(),
                attempt: ctx.attempt(),
            });
            starts.iter().filter(|s| s.id == ctx.id()).count()
        };
        self.inner.count.send_modify(|c| *c += 1);
        match mode.as_str() {
            "transient" => Err(JobError::transient("unavailable").with_detail("scripted")),
            "permanent" => Err(JobError::permanent("validation_failed")),
            "limited" => {
                Err(JobError::transient("rate_limited").with_retry_after(Duration::from_secs(120)))
            }
            "wait" => {
                let _stopped = StopGuard(self.clone(), ctx.id());
                ctx.token().cancelled().await;
                Err(JobError::cancelled())
            }
            "hang" => {
                let _stopped = StopGuard(self.clone(), ctx.id());
                std::future::pending::<()>().await;
                unreachable!("pending never resolves")
            }
            "gate" => {
                self.inner.gate.acquire().await.expect("open").forget();
                Ok(Outcome::Succeeded)
            }
            "alive" => {
                for _ in 0..6 {
                    ctx.heartbeat();
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
                Ok(Outcome::Succeeded)
            }
            "drain" => {
                if starts_of_job > 1 {
                    return Ok(Outcome::Succeeded);
                }
                let mut step = 0.0;
                loop {
                    if ctx.should_yield() {
                        return Ok(Outcome::Requeue { run_at: None });
                    }
                    ctx.progress(Some(step), Some("drain")).await;
                    step = (step + 0.1_f64).min(1.0);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            "progress" => {
                for i in 1..=100 {
                    ctx.progress(Some(f64::from(i) / 100.0), Some("count"))
                        .await;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok(Outcome::Succeeded)
            }
            _ => Ok(Outcome::Succeeded),
        }
    }
}

/// A test kind run by `probe`: `global` and `per_user` slots, a 30-second
/// lease, 3 tries and a 10–60 s backoff.
pub fn kind(name: &'static str, global: usize, per_user: usize, probe: &Probe) -> Kind {
    let spec = KindSpec::new(name)
        .global(global)
        .per_user(per_user)
        .lease(Duration::from_secs(30))
        .backoff(Backoff::new(
            Duration::from_secs(10),
            Duration::from_secs(60),
        ));
    Kind::new(spec, probe.worker())
}

impl TestState {
    /// A state whose job system runs `registry` on a tokio clock starting at
    /// [`START`] (paused-time tests).
    pub fn with_jobs(registry: Registry) -> Self {
        Self::with_jobs_at(registry, START)
    }

    /// [`TestState::with_jobs`] with the clock starting at `start_ms`.
    pub fn with_jobs_at(registry: Registry, start_ms: i64) -> Self {
        Self::with_config(|config| {
            config.jobs.registry = registry;
            config.jobs.clock = Clock::tokio(start_ms);
        })
    }

    /// A read-write connection to the control database, beside the server's.
    pub fn control(&self) -> Connection {
        let conn = Connection::open(self.data_dir().control_db()).expect("control database");
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn
    }

    /// Adds an active member `id` (a ULID-like id) to the control database.
    pub fn add_user(&self, id: &str) {
        self.control()
            .execute(
                "INSERT INTO users (id, email, role, quota_bytes, created_at) \
                 VALUES (?1, ?2, 'member', 0, ?3)",
                params![id, format!("{}@example.test", id.to_lowercase()), now_ms()],
            )
            .expect("insert a user");
    }

    /// Enqueues a job of `kind` for `user` with `payload`; returns its id.
    pub async fn enqueue(&self, user: &str, kind: &str, payload: Value) -> i64 {
        let job = NewJob::new(user, kind).payload(payload);
        self.state
            .jobs()
            .enqueue(job)
            .await
            .expect("enqueue")
            .job
            .id
    }

    /// The job `id` of `user`.
    pub async fn job(&self, user: &str, id: i64) -> JobRow {
        self.state
            .jobs()
            .get(user, id)
            .await
            .expect("read the job")
            .unwrap_or_else(|| panic!("no job {id}"))
    }

    /// Waits until the job `id` of `user` satisfies `done`; returns it. The
    /// wait follows the user's `job.updated` events, so paused time advances
    /// only as far as the scheduler's own timers.
    pub async fn wait_job(&self, user: &str, id: i64, done: impl Fn(&JobRow) -> bool) -> JobRow {
        let mut events = self.state.events().subscribe(user, None);
        loop {
            let job = self.job(user, id).await;
            if done(&job) {
                return job;
            }
            let next = tokio::time::timeout(Duration::from_secs(24 * 3600), events.next()).await;
            match next {
                Ok(Delivery::Event(_) | Delivery::Live(_) | Delivery::Resync { .. }) => {}
                Err(_) => panic!("job {id} never got there; it is {job:?}"),
            }
        }
    }
}

/// `{"mode": mode}`.
pub fn mode(mode: &str) -> Value {
    json!({ "mode": mode })
}

/// `{"modes": modes}`: one mode per try.
pub fn modes(modes: &[&str]) -> Value {
    json!({ "modes": modes })
}

/// Counts the jobs of each state in the control database.
pub fn states(t: &TestState) -> HashMap<String, i64> {
    let conn = t.control();
    let mut statement = conn
        .prepare("SELECT state, COUNT(*) FROM jobs GROUP BY state")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
