//! The job scheduler (plan §2.12) on paused tokio time: fairness across
//! users, claims, leases and their expiry, backoff and retries, restart
//! recovery, cancel, pause and resume, delayed jobs, dedupe, the nightly
//! schedule and pruning, the drain sweeper, `job.updated` throttling, and
//! the library generation behind the ETags. The routes are checked in
//! `jobs_api.rs`; the shutdown with the real server at the end of this file.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use rusqlite::params;
use serde_json::json;
use shelfy_core::db::UserDb;
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_server::events::Delivery;
use shelfy_server::events::model::{EventTopic, JobState};
use shelfy_server::jobs::{
    Clock, JobContext, JobError, Kind, KindSpec, NewJob, Outcome, Registry, SweepContext,
};
use support::jobs::{Probe, START, kind, mode, modes, states};
use support::library::{ALICE, BOB};
use support::{TestState, get, json, send};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const MINUTE: Duration = Duration::from_secs(60);

/// A state on paused time running `registry`, with Alice and Bob.
fn state(registry: Registry) -> TestState {
    let t = TestState::with_jobs(registry);
    t.add_user(ALICE);
    t.add_user(BOB);
    t
}

/// Inserts `n` queued jobs of `kind` for `user` straight into the control
/// database, as if they had piled up before a restart.
fn pile_up(t: &TestState, user: &str, kind: &str, n: usize) {
    let mut conn = t.control();
    let tx = conn.transaction().unwrap();
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO jobs (user_id, kind, state, payload_json, max_attempts, run_at, \
                 created_at, updated_at) VALUES (?1, ?2, 'queued', '{}', 3, ?3, ?3, ?3)",
            )
            .unwrap();
        for _ in 0..n {
            insert.execute(params![user, kind, START]).unwrap();
        }
    }
    tx.commit().unwrap();
}

#[tokio::test(start_paused = true)]
async fn one_users_backlog_never_starves_another_user() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.fair", 1, 1, &probe)));
    pile_up(&t, ALICE, "test.fair", 6_000);
    let bob_job = t.enqueue(BOB, "test.fair", mode("ok")).await;

    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    probe.wait_starts(2).await;
    assert!(scheduler.stop(tokio::time::Instant::now() + MINUTE).await);
    let starts = probe.starts();
    let bob_turn = starts.iter().position(|s| s.id == bob_job);
    assert!(
        bob_turn.is_some_and(|turn| turn <= 1),
        "Bob's job waited behind Alice's 6,000: {bob_turn:?}"
    );
    assert_eq!(t.job(BOB, bob_job).await.state, JobState::Succeeded);
    let rest: i64 = t
        .control()
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE user_id = ?1 AND state = 'queued'",
            [ALICE],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        rest > 5_000,
        "Bob did not wait for the backlog: {rest} left"
    );
}

#[tokio::test(start_paused = true)]
async fn a_newcomer_starts_within_one_round() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.fair", 1, 1, &probe)));
    let mut alice = Vec::new();
    for _ in 0..5 {
        alice.push(t.enqueue(ALICE, "test.fair", mode("gate")).await);
    }
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    probe.wait_starts(1).await;
    // Bob's single job arrives while Alice's first one runs.
    let bob = t.enqueue(BOB, "test.fair", mode("gate")).await;
    probe.open(1);
    probe.wait_starts(2).await;
    probe.open(1);
    probe.wait_starts(3).await;
    assert_eq!(
        probe.started_ids(),
        [alice[0], alice[1], bob],
        "Bob waits for one more turn of Alice, not for her backlog"
    );
    probe.open(10);
    t.wait_job(ALICE, alice[4], |job| job.state == JobState::Succeeded)
        .await;
}

#[tokio::test(start_paused = true)]
async fn limits_hold_globally_and_per_user() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.limits", 3, 2, &probe)));
    let mut ids = Vec::new();
    for user in [ALICE, ALICE, ALICE, BOB, BOB] {
        ids.push(t.enqueue(user, "test.limits", mode("gate")).await);
    }
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    probe.wait_starts(3).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let running: Vec<String> = probe.starts().into_iter().map(|s| s.user).collect();
    assert_eq!(running.len(), 3, "three slots overall");
    assert_eq!(
        running.iter().filter(|u| *u == ALICE).count(),
        2,
        "two per user"
    );
    let stats = t.state.jobs().stats();
    assert_eq!((stats[0].running, stats[0].ready), (3, 2));
    probe.open(5);
    for (user, id) in [ALICE, ALICE, ALICE, BOB, BOB].into_iter().zip(&ids) {
        t.wait_job(user, *id, |job| job.state == JobState::Succeeded)
            .await;
    }
    assert_eq!(probe.starts().len(), 5);
}

#[tokio::test(start_paused = true)]
async fn an_expired_lease_requeues_the_job_with_one_more_attempt() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.lease", 1, 1, &probe)));
    let hung = t.enqueue(ALICE, "test.lease", modes(&["hang", "ok"])).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    probe.wait_starts(1).await;
    let claimed = t.job(ALICE, hung).await;
    assert_eq!(claimed.state, JobState::Running);
    assert_eq!(
        claimed.lease_until,
        Some(START + 30_000),
        "claimed for one lease"
    );

    // No sign of life for a whole lease: the watchdog takes the job back.
    let requeued = t
        .wait_job(ALICE, hung, |job| job.state == JobState::Queued)
        .await;
    assert_eq!(requeued.attempts, 1);
    assert_eq!(requeued.error_code.as_deref(), Some("lease_expired"));
    let now = t.state.jobs().clock().now_ms();
    assert!((START + 30_000..=START + 45_000).contains(&now), "{now}");
    assert!(
        (5_000..=10_000).contains(&(requeued.run_at - now)),
        "a backoff with jitter"
    );
    assert_eq!(probe.stopped(), [hung], "the hung try was stopped");

    let done = t
        .wait_job(ALICE, hung, |job| job.state == JobState::Succeeded)
        .await;
    assert_eq!(done.attempts, 1);
    assert_eq!(done.error_code, None, "a success clears the last error");
    let attempts: Vec<u32> = probe.starts().iter().map(|s| s.attempt).collect();
    assert_eq!(attempts, [1, 2]);
}

#[tokio::test(start_paused = true)]
async fn a_worker_that_shows_life_keeps_its_lease() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.lease", 1, 1, &probe)));
    let alive = t.enqueue(ALICE, "test.lease", mode("alive")).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let done = t.wait_job(ALICE, alive, |job| job.state.is_final()).await;
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(done.attempts, 0, "60 s of heartbeats outlive a 30 s lease");
    assert_eq!(probe.starts().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_orphaned_running_row_is_taken_back_by_the_watchdog() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.lease", 1, 1, &probe)));
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    tokio::time::sleep(Duration::from_secs(1)).await;
    // A row left `running` by an attempt whose outcome could not be written.
    t.control()
        .execute(
            "INSERT INTO jobs (id, user_id, kind, state, payload_json, attempts, max_attempts, \
             run_at, lease_until, created_at, updated_at) \
             VALUES (500, ?1, 'test.lease', 'running', '{}', 2, 3, ?2, ?3, ?2, ?2)",
            params![ALICE, START, START + 10_000],
        )
        .unwrap();
    let failed = t.wait_job(ALICE, 500, |job| job.state.is_final()).await;
    assert_eq!(failed.state, JobState::Failed, "its last try is used up");
    assert_eq!(failed.attempts, 3);
    assert_eq!(failed.error_code.as_deref(), Some("lease_expired"));
    assert!(probe.starts().is_empty());
}

#[tokio::test(start_paused = true)]
async fn transient_errors_back_off_and_permanent_ones_fail_at_once() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.retry", 4, 4, &probe)));
    let flaky = t
        .enqueue(ALICE, "test.retry", modes(&["transient", "ok"]))
        .await;
    let broken = t.enqueue(ALICE, "test.retry", mode("transient")).await;
    let invalid = t.enqueue(ALICE, "test.retry", mode("permanent")).await;
    let limited = t.enqueue(ALICE, "test.retry", mode("limited")).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());

    let first = t.wait_job(ALICE, flaky, |job| job.attempts == 1).await;
    assert_eq!(first.state, JobState::Queued);
    assert_eq!(first.error_code.as_deref(), Some("unavailable"));
    assert_eq!(first.error_detail.as_deref(), Some("scripted"));
    let delay = first.run_at - first.updated_at;
    assert!(
        (5_000..=10_000).contains(&delay),
        "10 s backoff with jitter: {delay}"
    );

    let held = t.wait_job(ALICE, limited, |job| job.attempts == 1).await;
    assert!(
        held.run_at - held.updated_at >= 120_000,
        "Retry-After is the shortest wait"
    );

    let failed = t.wait_job(ALICE, invalid, |job| job.state.is_final()).await;
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.attempts, 1, "a permanent error fails at once");
    assert_eq!(failed.error_code.as_deref(), Some("validation_failed"));

    let done = t.wait_job(ALICE, flaky, |job| job.state.is_final()).await;
    assert_eq!((done.state, done.attempts), (JobState::Succeeded, 1));

    let exhausted = t.wait_job(ALICE, broken, |job| job.state.is_final()).await;
    assert_eq!(exhausted.state, JobState::Failed);
    assert_eq!(exhausted.attempts, 3, "every try used");
    assert!(exhausted.finished_at.is_some());
    let tries = probe.starts().iter().filter(|s| s.id == broken).count();
    assert_eq!(tries, 3);
}

#[tokio::test(start_paused = true)]
async fn a_restart_loses_and_duplicates_nothing() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.restart", 2, 2, &probe)));
    let mut ids = Vec::new();
    for payload in ["ok", "ok", "wait", "wait", "ok", "ok"] {
        ids.push(t.enqueue(ALICE, "test.restart", mode(payload)).await);
    }
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    probe.wait_starts(4).await;
    for id in &ids[..2] {
        t.wait_job(ALICE, *id, |job| job.state == JobState::Succeeded)
            .await;
    }
    // A crash: nothing more is recorded.
    scheduler.abort().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    let before = states(&t);
    assert_eq!(before.get("succeeded"), Some(&2));
    assert_eq!(before.get("running"), Some(&2), "{before:?}");
    assert_eq!(before.get("queued"), Some(&2));

    // The next start, on the same data directory.
    let after = Probe::all_ok();
    let restarted = TestState::with_config(|config| {
        config.data_dir = t.data_dir();
        config.jobs.registry = Registry::new().register(kind("test.restart", 2, 2, &after));
        config.jobs.clock = Clock::tokio(START + 60_000);
    });
    let _scheduler = restarted
        .state
        .jobs()
        .start(restarted.state.clone(), CancellationToken::new());
    for id in &ids {
        let job = restarted
            .wait_job(ALICE, *id, |job| job.state == JobState::Succeeded)
            .await;
        assert_eq!(job.attempts, 0, "an interruption uses no try");
    }
    let mut first = probe.started_ids();
    first.sort_unstable();
    assert_eq!(first, ids[..4]);
    let mut second = after.started_ids();
    second.sort_unstable();
    assert_eq!(
        second,
        ids[2..],
        "each interrupted or queued job ran once more"
    );
    assert_eq!(states(&restarted).get("succeeded"), Some(&6));
    assert_eq!(states(&restarted).len(), 1, "no other row appeared");
}

#[tokio::test(start_paused = true)]
async fn cancel_stops_a_running_job_and_drops_a_queued_one() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.cancel", 1, 1, &probe)));
    let running = t.enqueue(ALICE, "test.cancel", mode("wait")).await;
    let next = t.enqueue(ALICE, "test.cancel", mode("ok")).await;
    let dropped = t.enqueue(ALICE, "test.cancel", mode("ok")).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    probe.wait_starts(1).await;

    let cancelled = t.state.jobs().cancel(ALICE, dropped).await.unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);
    let stopped = t.state.jobs().cancel(ALICE, running).await.unwrap();
    assert_eq!(stopped.state, JobState::Cancelled);
    assert!(stopped.finished_at.is_some());

    t.wait_job(ALICE, next, |job| job.state == JobState::Succeeded)
        .await;
    assert_eq!(probe.stopped(), [running], "the worker saw its token fire");
    assert_eq!(
        probe.started_ids(),
        [running, next],
        "the dropped job never ran"
    );
    let still = t.job(ALICE, running).await;
    assert_eq!(
        still.state,
        JobState::Cancelled,
        "not failed, not re-queued"
    );
    assert_eq!(still.attempts, 0);

    // Cancelling again changes nothing; a finished job cannot be cancelled.
    let again = t.state.jobs().cancel(ALICE, running).await.unwrap();
    assert_eq!(again, still);
    let err = t.state.jobs().cancel(ALICE, next).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::CONFLICT);
    let err = t.state.jobs().cancel(BOB, running).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::NOT_FOUND, "another user's job");
}

#[tokio::test(start_paused = true)]
async fn paused_queues_wait_and_a_drain_yields_until_resumed() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.pause", 2, 1, &probe)));
    let drain = t.enqueue(ALICE, "test.pause", mode("drain")).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    probe.wait_starts(1).await;

    assert!(t.state.jobs().pause(ALICE, "test.pause").await.unwrap());
    assert!(
        !t.state.jobs().pause(ALICE, "test.pause").await.unwrap(),
        "already"
    );
    let yielded = t
        .wait_job(ALICE, drain, |job| job.state == JobState::Queued)
        .await;
    assert_eq!(yielded.attempts, 0, "a yield uses no try");
    assert_eq!(yielded.stage.as_deref(), Some("drain"), "progress is kept");
    let waiting = t.enqueue(ALICE, "test.pause", mode("ok")).await;
    let bobs = t.enqueue(BOB, "test.pause", mode("ok")).await;
    t.wait_job(BOB, bobs, |job| job.state == JobState::Succeeded)
        .await;
    tokio::time::sleep(10 * MINUTE).await;
    assert_eq!(t.job(ALICE, waiting).await.state, JobState::Queued);
    assert_eq!(probe.starts().len(), 2, "only Bob's job ran while paused");

    assert!(t.state.jobs().resume(ALICE, "test.pause").await.unwrap());
    t.wait_job(ALICE, drain, |job| job.state == JobState::Succeeded)
        .await;
    t.wait_job(ALICE, waiting, |job| job.state == JobState::Succeeded)
        .await;
    let err = t
        .state
        .jobs()
        .pause(ALICE, "test.unknown")
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(start_paused = true)]
async fn delayed_jobs_wait_for_their_time() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.later", 1, 1, &probe)));
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let later = NewJob::new(ALICE, "test.later").run_at(START + 3_600_000);
    let id = t.state.jobs().enqueue(later).await.unwrap().job.id;
    tokio::time::sleep(59 * MINUTE).await;
    assert!(probe.starts().is_empty());
    let done = t
        .wait_job(ALICE, id, |job| job.state == JobState::Succeeded)
        .await;
    assert!(done.updated_at >= START + 3_600_000);
    assert!(
        done.updated_at < START + 3_660_000,
        "at its time, not later"
    );
}

#[tokio::test(start_paused = true)]
async fn one_active_job_per_dedupe_key() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.drain", 1, 1, &probe)));
    let jobs = t.state.jobs();
    let first = jobs
        .enqueue(
            NewJob::new(ALICE, "test.drain")
                .dedupe("test.drain")
                .run_at(START + 3_600_000),
        )
        .await
        .unwrap();
    assert!(first.created);
    // New items arrive: the same job, pulled forward to now.
    let again = jobs
        .enqueue(NewJob::new(ALICE, "test.drain").dedupe("test.drain"))
        .await
        .unwrap();
    assert!(!again.created);
    assert_eq!(again.job.id, first.job.id);
    assert_eq!(again.job.run_at, START);

    let _scheduler = jobs.start(t.state.clone(), CancellationToken::new());
    t.wait_job(ALICE, first.job.id, |job| job.state == JobState::Succeeded)
        .await;
    assert!(
        t.state.jobs().clock().now_ms() < START + 60_000,
        "it ran at once"
    );
    let next = jobs
        .enqueue(NewJob::new(ALICE, "test.drain").dedupe("test.drain"))
        .await
        .unwrap();
    assert!(next.created, "the key is free once the job is over");

    for bad in [
        NewJob::new(ALICE, "test.unknown"),
        NewJob::new(ALICE, "test.drain").dedupe(""),
        NewJob::new(ALICE, "test.drain").payload(json!({ "big": "x".repeat(70_000) })),
    ] {
        let err = jobs.enqueue(bad).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[tokio::test(start_paused = true)]
async fn the_nightly_schedule_prunes_and_enqueues_for_active_users() {
    let probe = Probe::new();
    let nightly = Kind::new(KindSpec::new("test.nightly").nightly(true), probe.worker());
    // 02:59 UTC.
    let two_fifty_nine = START + 119 * 60_000;
    let t = TestState::with_jobs_at(Registry::new().register(nightly), two_fifty_nine);
    t.add_user(ALICE);
    t.add_user(BOB);
    t.control()
        .execute("UPDATE users SET status = 'disabled' WHERE id = ?1", [BOB])
        .unwrap();
    let day = 86_400_000;
    let control = t.control();
    // Finished 14 days minus 30 s ago at 02:59, so 14 days and 30 s at 03:00.
    for (id, finished) in [
        (1, two_fifty_nine - 14 * day + 30_000),
        (2, two_fifty_nine - day),
    ] {
        control
            .execute(
                "INSERT INTO jobs (id, user_id, kind, state, payload_json, max_attempts, run_at, \
                 created_at, updated_at, finished_at) \
                 VALUES (?1, ?2, 'test.nightly', 'succeeded', '{}', 3, ?3, ?3, ?3, ?3)",
                params![id, ALICE, finished],
            )
            .unwrap();
    }
    control
        .execute(
            "INSERT INTO idempotency (user_id, key, status, body, created_at) \
             VALUES (?1, 'old', 200, x'', ?2), (?1, 'fresh', 200, x'', ?3)",
            params![ALICE, two_fifty_nine - day + 30_000, two_fifty_nine],
        )
        .unwrap();

    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    tokio::time::sleep(Duration::from_secs(1)).await;
    let count = |sql: &str| -> i64 { t.control().query_row(sql, [], |row| row.get(0)).unwrap() };
    assert_eq!(
        count("SELECT COUNT(*) FROM jobs"),
        2,
        "nothing old enough at boot"
    );
    assert_eq!(count("SELECT COUNT(*) FROM idempotency"), 2);

    probe.wait_starts(1).await;
    let now = t.state.jobs().clock().now_ms();
    assert_eq!(now, two_fifty_nine + 60_000, "at 03:00 UTC");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(probe.starts().len(), 1, "Bob is disabled");
    assert_eq!(probe.starts()[0].user, ALICE);
    assert_eq!(
        count("SELECT COUNT(*) FROM jobs WHERE id = 1"),
        0,
        "finished over 14 days ago"
    );
    assert_eq!(count("SELECT COUNT(*) FROM jobs WHERE id = 2"), 1);
    assert_eq!(
        count("SELECT COUNT(*) FROM jobs WHERE dedupe_key = 'nightly'"),
        1
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM idempotency WHERE key = 'old'"),
        0,
        "keys last 24 hours"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM idempotency WHERE key = 'fresh'"),
        1
    );

    // The next night, again.
    probe.wait_starts(2).await;
    assert_eq!(
        t.state.jobs().clock().now_ms(),
        two_fifty_nine + 60_000 + 86_400_000
    );
}

#[tokio::test(start_paused = true)]
async fn the_sweeper_rearms_drains_with_pending_work() {
    let probe = Probe::new();
    let pending = |ctx: SweepContext| async move {
        let now = ctx.now_ms();
        ctx.user_db(move |db| {
            let rows = db.read(collections::list)?;
            Ok(rows.iter().any(|c| c.name == "pending").then_some(now))
        })
        .await
    };
    let drain = kind("test.drain", 2, 1, &probe).with_sweep(pending);
    let t = state(Registry::new().register(drain));
    t.write(ALICE, |tx| {
        collections::create(
            tx,
            &NewCollection {
                name: "pending".into(),
                ..NewCollection::default()
            },
            START,
        )
    })
    .await;
    t.write(BOB, |tx| {
        collections::create(
            tx,
            &NewCollection {
                name: "done".into(),
                ..NewCollection::default()
            },
            START,
        )
    })
    .await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    tokio::time::sleep(9 * MINUTE).await;
    assert!(
        probe.starts().is_empty(),
        "the first sweep is 10 minutes in"
    );
    probe.wait_starts(1).await;
    let now = t.state.jobs().clock().now_ms();
    assert!((START + 600_000..START + 601_000).contains(&now), "{now}");
    tokio::time::sleep(5 * MINUTE).await;
    assert_eq!(probe.starts().len(), 1, "Bob has nothing pending");
    let drain = &probe.starts()[0];
    assert_eq!(drain.user, ALICE);
    let job = t.job(ALICE, drain.id).await;
    assert_eq!(job.dedupe_key.as_deref(), Some("test.drain"));
    probe.wait_starts(2).await;
    assert_eq!(probe.starts()[1].user, ALICE, "and again at the next sweep");
}

#[tokio::test(start_paused = true)]
async fn job_updates_are_throttled_to_one_per_250_ms() {
    let probe = Probe::new();
    let t = state(Registry::new().register(kind("test.progress", 1, 1, &probe)));
    let mut events = t.state.events().subscribe(ALICE, None);
    let id = t.enqueue(ALICE, "test.progress", mode("progress")).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let mut seen = Vec::new();
    loop {
        let Delivery::Event(event) = events.next().await else {
            panic!("no resync expected")
        };
        assert_eq!(event.topic, EventTopic::JobUpdated);
        let data: serde_json::Value = serde_json::from_str(&event.data).unwrap();
        assert_eq!(data["id"], id);
        let state = data["state"].as_str().unwrap().to_owned();
        seen.push((event.at, state.clone(), data));
        if state == "succeeded" {
            break;
        }
    }
    // 100 reports over a second: a handful of events, 250 ms apart.
    assert!(seen.len() >= 4 && seen.len() <= 10, "{} events", seen.len());
    for pair in seen.windows(2) {
        assert!(
            pair[1].0 - pair[0].0 >= Duration::from_millis(250),
            "{:?} then {:?}",
            pair[0].1,
            pair[1].1
        );
    }
    assert_eq!(seen[0].1, "queued");
    let last_progress = seen
        .iter()
        .rev()
        .find(|(_, state, _)| state == "running")
        .unwrap();
    assert!(last_progress.2["progress"].as_f64().unwrap() > 0.5);
    assert_eq!(last_progress.2["stage"], "count");
    let done = &seen.last().unwrap().2;
    assert_eq!(done["progress"], 1.0);
    let row = t.job(ALICE, id).await;
    assert_eq!(row.stage.as_deref(), Some("count"));
}

/// The *From T11* note: the library generation lives in each open handle,
/// and the API's ETags read the cached one. A job takes the handle from the
/// cache for each chunk, so its writes always show in the ETags, even when
/// the cache evicts the handle between two chunks or during one.
#[tokio::test(start_paused = true)]
async fn job_writes_always_move_the_etag_generation() {
    // The worker writes one collection per chunk and pauses after the first
    // two (`paused`), until the test lets it go on (`go`).
    let go = Arc::new(Semaphore::new(0));
    let (paused_tx, mut paused) = tokio::sync::mpsc::unbounded_channel::<()>();
    let reopened = Arc::new(Mutex::new(None));
    let worker = {
        let (go, reopened) = (Arc::clone(&go), Arc::clone(&reopened));
        move |ctx: JobContext| {
            let (go, paused_tx, reopened) =
                (Arc::clone(&go), paused_tx.clone(), Arc::clone(&reopened));
            async move {
                ctx.user_db(create("one")).await?;
                paused_tx.send(()).unwrap();
                go.acquire().await.unwrap().forget();
                ctx.user_db(create("two")).await?;
                paused_tx.send(()).unwrap();
                go.acquire().await.unwrap().forget();
                // During this chunk the cache evicts the handle, and a
                // request opens a new one before the chunk writes.
                let cache = Arc::clone(ctx.state().user_dbs());
                let user = ctx.user_id().to_owned();
                ctx.user_db(move |db| {
                    cache.evict(&user);
                    let newer = cache.get(&user)?;
                    *reopened.lock().unwrap() = Some((newer.generation(), newer));
                    create("three")(db)
                })
                .await?;
                Ok::<_, JobError>(Outcome::Succeeded)
            }
        }
    };
    let t = state(Registry::new().register(Kind::new(KindSpec::new("test.chunks"), worker)));
    let app = t.app_as(ALICE);
    let list = |etag: &str| {
        Request::get("/api/v1/collections")
            .header(header::IF_NONE_MATCH, etag)
            .body(Body::empty())
            .unwrap()
    };
    let etag_of = |response: &axum::http::Response<Body>| {
        response.headers()[header::ETAG]
            .to_str()
            .unwrap()
            .to_owned()
    };
    let id = t.enqueue(ALICE, "test.chunks", json!({})).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    paused.recv().await.unwrap();

    // Between two chunks the cache evicts the library; the API reopens it.
    t.state.user_dbs().evict(ALICE);
    let response = send(&app, get("/api/v1/collections")).await;
    let before = etag_of(&response);
    assert_eq!(json(response).await["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        send(&app, list(&before)).await.status(),
        StatusCode::NOT_MODIFIED
    );
    go.add_permits(1);
    paused.recv().await.unwrap();
    let response = send(&app, list(&before)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the second chunk wrote through the handle the API reads: no stale 304"
    );
    let after = etag_of(&response);
    assert_eq!(json(response).await["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        send(&app, list(&after)).await.status(),
        StatusCode::NOT_MODIFIED
    );

    go.add_permits(1);
    let done = t.wait_job(ALICE, id, |job| job.state.is_final()).await;
    assert_eq!(done.state, JobState::Succeeded);
    // The handle opened during the last chunk missed its write: it was
    // retired, so no ETag taken from it can match again.
    let (seen, newer) = reopened.lock().unwrap().take().unwrap();
    let current = t.state.user_db(ALICE).await.unwrap();
    assert!(!Arc::ptr_eq(&current, &newer));
    assert_ne!(current.generation(), seen);
    let response = send(&app, list(&after)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let names: Vec<String> = json(response).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["one", "two", "three"]);
}

/// A chunk that creates the collection `name`.
fn create(name: &'static str) -> impl FnOnce(&UserDb) -> Result<(), JobError> + Send + 'static {
    move |db: &UserDb| {
        db.write(|tx| {
            collections::create(
                tx,
                &NewCollection {
                    name: name.into(),
                    ..NewCollection::default()
                },
                START,
            )
        })?;
        Ok(())
    }
}

/// The scheduler starts with the server and stops within the shutdown grace:
/// an interrupted job goes back to the queue without using a try, a worker
/// that ignores its token is aborted at the deadline and its job re-queued
/// at the next boot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_scheduler_runs_with_the_server_and_stops_within_the_grace() {
    use shelfy_server::serve::Server;
    use shelfy_server::telemetry::metrics;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    let probe = Probe::new();
    let t = TestState::with_config(|config| {
        config.jobs.registry = Registry::new().register(kind("test.stop", 2, 2, &probe));
        // 7 s in all: 2 s to drain, 5 s to close the databases.
        config.shutdown_grace = Duration::from_secs(7);
    });
    t.add_user(ALICE);
    let polite = t.enqueue(ALICE, "test.stop", mode("wait")).await;
    let stubborn = t.enqueue(ALICE, "test.stop", mode("hang")).await;
    let server = Server::new(
        t.state.clone(),
        t.app(),
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        metrics::install(),
    );
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stop_rx.await;
    }));
    let real_time = Duration::from_secs(10);
    tokio::time::timeout(real_time, probe.wait_starts(2))
        .await
        .expect("both jobs start with the server");
    let stopping = std::time::Instant::now();
    stop_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("the server stops within its grace")
        .unwrap()
        .unwrap();
    let took = stopping.elapsed();
    assert!(
        took >= Duration::from_secs(2) && took < Duration::from_secs(6),
        "waited for the stubborn worker until the drain deadline: {took:?}"
    );
    let conn = t.control();
    let row = |id: i64| -> (String, i64) {
        conn.query_row(
            "SELECT state, attempts FROM jobs WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(
        row(polite),
        ("queued".into(), 0),
        "interrupted, no try used"
    );
    assert_eq!(
        row(stubborn),
        ("running".into(), 0),
        "aborted at the deadline"
    );

    // The next boot queues it again, and both run.
    let after = Probe::all_ok();
    let restarted = TestState::with_config(|config| {
        config.data_dir = t.data_dir();
        config.jobs.registry = Registry::new().register(kind("test.stop", 2, 2, &after));
    });
    let scheduler = restarted
        .state
        .jobs()
        .start(restarted.state.clone(), CancellationToken::new());
    for id in [polite, stubborn] {
        let done = restarted.wait_job(ALICE, id, |job| job.state == JobState::Succeeded);
        tokio::time::timeout(real_time, done)
            .await
            .expect("the re-queued jobs run");
    }
    assert_eq!(after.starts().len(), 2);
    assert!(scheduler.stop(tokio::time::Instant::now() + MINUTE).await);
}
