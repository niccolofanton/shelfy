//! Quotas, the media budget and usage accounting (P4-07): reservations,
//! commits and releases, refusals, the over-quota notification, `GET
//! /me/usage` after a commit, a day of writes with no drift for the count,
//! the disk sample behind the budget, the daily counters and `admin user
//! limits`.

mod support;

use std::collections::HashSet;
use std::process::{Command, Output};
use std::time::Duration;

use axum::http::StatusCode;
use rusqlite::{Connection, params};
use serde_json::Value;
use shelfy_core::repo::RepoError;
use shelfy_media::refs::{self, ObjectMeta, Origin, Role};
use shelfy_media::store::{IngestLimits, MediaStore, StagedObject, UserMedia};
use shelfy_server::admin::owner::create_owner;
use shelfy_server::admin::user::limits;
use shelfy_server::config::DataDir;
use shelfy_server::control::usage_daily::{self, Field};
use shelfy_server::control::users::Limits;
use shelfy_server::error::{ApiError, ErrorCode};
use shelfy_server::events::model::JobState;
use shelfy_server::jobs::{kinds, usage};
use shelfy_server::quota::{self, GIB, NOTIFICATION_CODE, Reservation};
use shelfy_server::state::{AppState, blocking};
use shelfy_server::telemetry::metrics::{USERS_AREA, area_bytes, sample_disk};
use support::TestState;
use support::auth::{owner, sign_in, with_session};
use support::library::{ALICE, BOB};
use support::{get, json, send};
use tokio_util::sync::CancellationToken;

const HOUR: Duration = Duration::from_secs(3600);

/// `len` bytes that sniff as a JPEG, different for each `seed`.
fn jpeg_like(seed: u64, len: usize) -> Vec<u8> {
    let mut bytes = b"\xFF\xD8\xFF\xE0\0\x10JFIF\0".to_vec();
    bytes.extend_from_slice(&seed.to_le_bytes());
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    while bytes.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        bytes.push((x & 0xff) as u8);
    }
    bytes.truncate(len.max(20));
    bytes
}

/// Sets `user`'s quota.
fn set_quota(t: &TestState, user: &str, bytes: i64) {
    t.control()
        .execute(
            "UPDATE users SET quota_bytes = ?2 WHERE id = ?1",
            params![user, bytes],
        )
        .unwrap();
}

/// `(media, db, used, counted at)` of `user`'s row.
fn usage_row(t: &TestState, user: &str) -> (i64, i64, i64, Option<i64>) {
    t.control()
        .query_row(
            "SELECT usage_media_bytes, usage_db_bytes, usage_bytes, usage_updated_at
             FROM users WHERE id = ?1",
            [user],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
}

fn media(t: &TestState, user: &str) -> UserMedia {
    MediaStore::new(t.data_dir().users_dir())
        .user(user)
        .unwrap()
}

/// Stages `contents` in `user`'s store, as a fetch or an upload would.
async fn stage(t: &TestState, user: &str, contents: Vec<Vec<u8>>) -> Vec<StagedObject> {
    let media = media(t, user);
    blocking(move || -> Result<Vec<StagedObject>, ApiError> {
        Ok(contents
            .into_iter()
            .map(|c| media.ingest(&c[..], IngestLimits::UPLOAD).unwrap())
            .collect())
    })
    .await
    .unwrap()
}

/// Records `staged` in `user`'s library and commits what the rows add, in
/// one write transaction, as a writer does. Returns the bytes committed.
async fn record(
    t: &TestState,
    user: &str,
    staged: Vec<StagedObject>,
    reservation: Reservation,
) -> Result<u64, ApiError> {
    let (added, reservation) = record_part(t, user, staged, reservation).await?;
    drop(reservation);
    Ok(added)
}

/// [`record`] with [`Reservation::commit_part`]: the reservation comes back
/// with what it still holds.
async fn record_part(
    t: &TestState,
    user: &str,
    staged: Vec<StagedObject>,
    mut reservation: Reservation,
) -> Result<(u64, Reservation), ApiError> {
    let media = media(t, user);
    let db = t.state.user_db(user).await?;
    let now = t.state.jobs().clock().now_ms();
    blocking(move || {
        db.write(|tx| -> Result<(u64, Reservation), RepoError> {
            let objects: Vec<_> = staged.iter().map(|s| (s.digest(), s.size())).collect();
            let added = quota::new_bytes(tx, &objects)?;
            for object in staged {
                let meta = ObjectMeta::new(Role::Image, Origin::Server);
                refs::publish_and_record(tx, &media, object, &[], &meta, now)?;
            }
            reservation.commit_part(added)?;
            Ok((added, reservation))
        })
    })
    .await
}

/// Deletes the objects `ids` as the GC does (stamped, then collected) and
/// releases their bytes in the same transaction; returns them.
async fn collect(t: &TestState, user: &str, ids: Vec<i64>) -> u64 {
    let media = media(t, user);
    let db = t.state.user_db(user).await.unwrap();
    let state = t.state.clone();
    let user = user.to_owned();
    let now = t.state.jobs().clock().now_ms();
    blocking(move || {
        db.write(|tx| -> Result<u64, RepoError> {
            refs::stamp_unreferenced(tx, &ids, now)?;
            let garbage = refs::collect_garbage(tx, &media, now, 100)?;
            let bytes = garbage.iter().map(|g| u64::try_from(g.size).unwrap()).sum();
            quota::release(&state, &user, bytes)?;
            Ok(bytes)
        })
    })
    .await
    .unwrap()
}

/// The sum of `user`'s `media_objects.bytes`.
fn library_media(t: &TestState, user: &str) -> i64 {
    Connection::open(t.data_dir().library_db(user))
        .unwrap()
        .query_row(
            "SELECT coalesce(sum(bytes), 0) FROM media_objects",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// The ids of `user`'s objects, oldest first.
fn object_ids(t: &TestState, user: &str) -> Vec<i64> {
    let conn = Connection::open(t.data_dir().library_db(user)).unwrap();
    let mut stmt = conn
        .prepare("SELECT id FROM media_objects ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The `quota.exceeded` notifications of `user`.
fn notices(t: &TestState, user: &str) -> Vec<(String, Option<String>, i64)> {
    let conn = Connection::open(t.data_dir().library_db(user)).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT params_json, target, created_at FROM notifications WHERE code = ?1
             ORDER BY id",
        )
        .unwrap();
    stmt.query_map([NOTIFICATION_CODE], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

/// A state whose media budget is `bytes`, with the `users` area sampled
/// empty, so only what the test reserves and commits counts.
fn with_budget(bytes: u64) -> TestState {
    let t = TestState::with_config(|config| config.quota.media_budget_bytes = bytes);
    let quotas = t.state.quota();
    quotas.record_users_sample(0, quotas.sample_mark());
    t
}

#[tokio::test]
async fn a_reservation_commits_what_the_rows_add_and_drops_the_rest() {
    let t = TestState::new();
    t.add_user(ALICE);
    set_quota(&t, ALICE, 1_000_000);
    let quotas = t.state.quota();

    // Held, then dropped: nothing is counted.
    let held = quota::reserve(&t.state, ALICE, 400_000).await.unwrap();
    assert_eq!(held.bytes(), 400_000);
    assert_eq!(quotas.reserved(ALICE), 400_000);
    assert_eq!(quotas.reserved_total(), 400_000);
    drop(held);
    assert_eq!(quotas.reserved(ALICE), 0);
    assert_eq!(usage_row(&t, ALICE), (0, 0, 0, None));

    // Committed: the rows' bytes, at once, without a count.
    let a = jpeg_like(1, 30_000);
    let b = jpeg_like(2, 20_000);
    let reservation = quota::reserve(&t.state, ALICE, 100_000).await.unwrap();
    let staged = stage(&t, ALICE, vec![a.clone(), b, a.clone()]).await;
    let added = record(&t, ALICE, staged, reservation).await.unwrap();
    assert_eq!(added, 50_000, "the same content counts once");
    assert_eq!(quotas.reserved(ALICE), 0, "the rest is released");
    assert_eq!(usage_row(&t, ALICE), (50_000, 0, 50_000, None));
    assert_eq!(library_media(&t, ALICE), 50_000);

    // An object the library has adds nothing.
    let reservation = quota::reserve(&t.state, ALICE, 30_000).await.unwrap();
    let staged = stage(&t, ALICE, vec![a]).await;
    assert_eq!(record(&t, ALICE, staged, reservation).await.unwrap(), 0);
    assert_eq!(usage_row(&t, ALICE).0, 50_000);

    // Today's bytes_in counts what was committed.
    let today = t
        .state
        .control()
        .read(|c| usage_daily::of_day(c, ALICE, t.state.jobs().clock().now_ms()))
        .unwrap();
    assert_eq!(today.bytes_in, 50_000);

    // A release takes deleted objects off, never below 0.
    quota::release(&t.state, ALICE, 20_000).unwrap();
    assert_eq!(usage_row(&t, ALICE).0, 30_000);
    quota::release(&t.state, ALICE, 1_000_000).unwrap();
    assert_eq!(usage_row(&t, ALICE), (0, 0, 0, None));

    // A user that does not exist cannot reserve.
    let err = quota::reserve(&t.state, BOB, 1).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotFound);
}

#[tokio::test]
async fn the_quota_and_the_budget_refuse_what_does_not_fit() {
    let t = with_budget(1_000_000);
    t.add_user(ALICE);
    t.add_user(BOB);
    set_quota(&t, ALICE, 100_000);

    // The quota: usage + live reservations + the new bytes.
    let first = quota::reserve(&t.state, ALICE, 60_000).await.unwrap();
    let refused = quota::reserve(&t.state, ALICE, 40_001).await.unwrap_err();
    assert_eq!(refused.code(), ErrorCode::QuotaExceeded);
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert!(quota::is_refused(&refused));
    let fits = quota::reserve(&t.state, ALICE, 40_000).await.unwrap();
    drop((first, fits));
    // A quota of 0 is unlimited, up to the budget.
    let big = quota::reserve(&t.state, BOB, 950_000).await.unwrap();

    // The budget: every user's reservations, the commits since the last
    // sample, and the sample. Alice's quota has room; the budget has not.
    let full = quota::reserve(&t.state, ALICE, 60_000).await.unwrap_err();
    assert_eq!(full.code(), ErrorCode::StorageFull);
    assert_eq!(full.status().as_u16(), 507);
    assert!(quota::is_refused(&full));
    assert!(quota::check(&t.state, ALICE, 50_000).await.is_ok());
    let staged = stage(&t, BOB, vec![jpeg_like(9, 500_000)]).await;
    assert_eq!(record(&t, BOB, staged, big).await.unwrap(), 500_000);
    let budget = t.state.quota().budget().await;
    assert_eq!(
        (budget.limit_bytes, budget.used_bytes),
        (1_000_000, 500_000)
    );
    assert!(quota::check(&t.state, BOB, 500_001).await.is_err());
    // A new sample replaces what was committed before it started.
    let quotas = t.state.quota();
    let mark = quotas.sample_mark();
    quotas.record_users_sample(100_000, mark);
    assert_eq!(quotas.budget().await.used_bytes, 100_000);
    assert!(quota::check(&t.state, BOB, 900_000).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_concurrent_reservations_never_overshoot() {
    // The quota: ten of fifty fit, whether they hold or commit.
    let t = TestState::new();
    t.add_user(ALICE);
    set_quota(&t, ALICE, 1_000_000);
    let attempts: Vec<_> = (0..50)
        .map(|_| {
            let state = t.state.clone();
            tokio::spawn(async move { quota::reserve(&state, ALICE, 100_000).await })
        })
        .collect();
    let mut held = Vec::new();
    for attempt in attempts {
        match attempt.await.unwrap() {
            Ok(reservation) => held.push(reservation),
            Err(err) => assert_eq!(err.code(), ErrorCode::QuotaExceeded),
        }
    }
    assert_eq!(held.len(), 10);
    assert_eq!(t.state.quota().reserved(ALICE), 1_000_000);
    drop(held);
    assert_eq!(t.state.quota().reserved(ALICE), 0);

    let racers = |t: &TestState, user: &'static str, seed: u64| -> Vec<_> {
        (0..50)
            .map(|i| {
                let state = t.state.clone();
                let media = media(t, user);
                tokio::spawn(async move {
                    let reservation = quota::reserve(&state, user, 100_000).await?;
                    let db = state.user_db(user).await?;
                    let now = state.jobs().clock().now_ms();
                    let content = jpeg_like(seed + i, 100_000);
                    blocking(move || {
                        let staged = media.ingest(&content[..], IngestLimits::UPLOAD).unwrap();
                        db.write(|tx| -> Result<u64, RepoError> {
                            let added = quota::new_bytes(tx, &[(staged.digest(), staged.size())])?;
                            let meta = ObjectMeta::new(Role::Image, Origin::Server);
                            refs::publish_and_record(tx, &media, staged, &[], &meta, now)?;
                            reservation.commit(added)?;
                            Ok(added)
                        })
                    })
                    .await
                })
            })
            .collect()
    };
    async fn settle(
        tasks: Vec<tokio::task::JoinHandle<Result<u64, ApiError>>>,
        refusal: ErrorCode,
    ) -> u64 {
        let mut stored = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(bytes) => {
                    assert_eq!(bytes, 100_000);
                    stored += 1;
                }
                Err(err) => assert_eq!(err.code(), refusal, "{err}"),
            }
        }
        stored
    }
    // Commits racing the reservations: still ten.
    let stored = settle(racers(&t, ALICE, 1_000), ErrorCode::QuotaExceeded).await;
    assert_eq!(stored, 10);
    assert_eq!(usage_row(&t, ALICE).0, 1_000_000);
    assert_eq!(library_media(&t, ALICE), 1_000_000);
    assert_eq!(t.state.quota().reserved_total(), 0);

    // The media budget, for an unlimited user: ten again.
    let t = with_budget(1_000_000);
    t.add_user(BOB);
    let stored = settle(racers(&t, BOB, 2_000), ErrorCode::StorageFull).await;
    assert_eq!(stored, 10);
    assert_eq!(library_media(&t, BOB), 1_000_000);
    assert_eq!(t.state.quota().budget().await.used_bytes, 1_000_000);
}

/// A day of stores, duplicates, failed and partial stores, GC deletions,
/// refusals and objects stored again, on the job system's clock: the count
/// at the end finds no drift.
#[tokio::test(start_paused = true)]
async fn a_day_of_mixed_writes_leaves_no_drift_for_the_count() {
    let t = TestState::with_jobs(kinds::registry());
    t.add_user(ALICE);
    set_quota(&t, ALICE, 3_000_000);
    let clock = *t.state.jobs().clock();
    let day_start = clock.now_ms();
    let mut seed = 0_u64;
    let mut next = |len: usize| {
        seed += 1;
        jpeg_like(seed, len)
    };
    let mut kept: Vec<Vec<u8>> = Vec::new();
    let mut committed = 0_u64;
    let mut refused = 0;
    for hour in 0..24 {
        tokio::time::advance(HOUR).await;
        match hour % 6 {
            // Two new objects.
            0 => {
                let (a, b) = (next(40_000), next(25_000));
                kept.extend([a.clone(), b.clone()]);
                let reservation = quota::reserve(&t.state, ALICE, 2 * 50_000).await.unwrap();
                let staged = stage(&t, ALICE, vec![a, b]).await;
                committed += record(&t, ALICE, staged, reservation).await.unwrap();
            }
            // A new object and one the library has.
            1 => {
                let fresh = next(30_000);
                let known = kept.last().unwrap().clone();
                kept.push(fresh.clone());
                let reservation = quota::reserve(&t.state, ALICE, 100_000).await.unwrap();
                let staged = stage(&t, ALICE, vec![fresh, known]).await;
                let added = record(&t, ALICE, staged, reservation).await.unwrap();
                assert_eq!(added, 30_000);
                committed += added;
            }
            // A store that fails after reserving: nothing.
            2 => {
                let reservation = quota::reserve(&t.state, ALICE, 80_000).await.unwrap();
                let _ = stage(&t, ALICE, vec![next(10_000)]).await;
                drop(reservation);
            }
            // A store in two transactions.
            3 => {
                let (a, b) = (next(20_000), next(15_000));
                kept.extend([a.clone(), b.clone()]);
                let reservation = quota::reserve(&t.state, ALICE, 60_000).await.unwrap();
                let staged = stage(&t, ALICE, vec![a]).await;
                let (first, reservation) =
                    record_part(&t, ALICE, staged, reservation).await.unwrap();
                assert_eq!(reservation.bytes(), 40_000);
                let staged = stage(&t, ALICE, vec![b]).await;
                let (second, reservation) =
                    record_part(&t, ALICE, staged, reservation).await.unwrap();
                assert_eq!(reservation.bytes(), 25_000);
                committed += first + second;
            }
            // The GC deletes the oldest object.
            4 => {
                let oldest = object_ids(&t, ALICE)[0];
                let freed = collect(&t, ALICE, vec![oldest]).await;
                assert!(freed > 0);
                committed -= freed;
            }
            // A refusal, then an object deleted earlier stored again.
            _ => {
                let err = quota::reserve(&t.state, ALICE, 3_000_000)
                    .await
                    .unwrap_err();
                assert_eq!(err.code(), ErrorCode::QuotaExceeded);
                refused += 1;
                let again = kept[0].clone();
                let reservation = quota::reserve(&t.state, ALICE, 50_000).await.unwrap();
                let staged = stage(&t, ALICE, vec![again]).await;
                committed += record(&t, ALICE, staged, reservation).await.unwrap();
            }
        }
        assert_eq!(t.state.quota().reserved(ALICE), 0, "hour {hour}");
    }
    assert_eq!(refused, 4);
    let kept_media = usage_row(&t, ALICE).0;
    assert_eq!(kept_media, i64::try_from(committed).unwrap());
    assert_eq!(kept_media, library_media(&t, ALICE));

    // The day's bytes_in, over its two UTC days, is what was stored.
    let bytes_in: i64 = t
        .control()
        .query_row(
            "SELECT sum(bytes_in) FROM usage_daily WHERE user_id = ?1",
            [ALICE],
            |r| r.get(0),
        )
        .unwrap();
    assert!(bytes_in >= kept_media);
    assert_eq!(
        usage_daily::day_of(day_start),
        "2026-10-02",
        "the test clock's day"
    );

    // The count: the same media, the database measured now. Time flows
    // again, so the scheduler's timers do not jump ahead of its workers.
    tokio::time::resume();
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let job = usage::enqueue(t.state.jobs(), ALICE).await.unwrap().job;
    t.wait_job(ALICE, job.id, |j| j.state == JobState::Succeeded)
        .await;
    let (media_after, db, used, counted) = usage_row(&t, ALICE);
    assert_eq!(media_after, kept_media, "no drift");
    assert!(db >= 4_096);
    assert_eq!(used, media_after + db);
    assert!(counted.unwrap() > day_start);
    let again = usage::recount(&t.state, ALICE).await.unwrap();
    assert_eq!(again.drift_bytes, 0);
    assert_eq!(again.media_bytes, kept_media);
}

#[tokio::test]
async fn a_count_corrects_a_drift() {
    let t = TestState::new();
    t.add_user(ALICE);
    let reservation = quota::reserve(&t.state, ALICE, 50_000).await.unwrap();
    let staged = stage(&t, ALICE, vec![jpeg_like(7, 50_000)]).await;
    record(&t, ALICE, staged, reservation).await.unwrap();
    // A writer that forgot its commit, and one that forgot its release.
    t.control()
        .execute(
            "UPDATE users SET usage_media_bytes = 1234, usage_bytes = 1234 WHERE id = ?1",
            [ALICE],
        )
        .unwrap();
    let counted = usage::recount(&t.state, ALICE).await.unwrap();
    assert_eq!(counted.media_bytes, 50_000);
    assert_eq!(counted.drift_bytes, 50_000 - 1234);
    assert_eq!(usage_row(&t, ALICE).0, 50_000);
    assert_eq!(
        usage::recount(&t.state, ALICE).await.unwrap().drift_bytes,
        0
    );

    // A user without a library uses nothing, and gets none.
    t.add_user(BOB);
    let none = usage::recount(&t.state, BOB).await.unwrap();
    assert_eq!(
        (none.media_bytes, none.db_bytes, none.drift_bytes),
        (0, 0, 0)
    );
    assert!(!t.data_dir().library_db(BOB).exists());
}

#[tokio::test(start_paused = true)]
async fn over_quota_users_are_notified_at_most_once_a_day() {
    let t = TestState::with_jobs(kinds::registry());
    t.add_user(ALICE);
    set_quota(&t, ALICE, 10_000);
    drop(t.state.user_db(ALICE).await.unwrap());
    let mut events = t.state.events().subscribe(ALICE, None);

    for _ in 0..3 {
        let err = quota::reserve(&t.state, ALICE, 20_000).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::QuotaExceeded);
    }
    let sent = notices(&t, ALICE);
    assert_eq!(sent.len(), 1, "{sent:?}");
    let params: Value = serde_json::from_str(&sent[0].0).unwrap();
    assert_eq!(params["quotaBytes"], 10_000);
    assert_eq!(params["usedBytes"], 0);
    assert_eq!(sent[0].1.as_deref(), Some("/settings/storage"));
    // It reaches the user's streams as a `notification` event.
    match tokio::time::timeout(Duration::from_secs(5), events.next()).await {
        Ok(shelfy_server::events::Delivery::Event(event)) => {
            assert!(event.data.contains(NOTIFICATION_CODE), "{}", event.data);
        }
        other => panic!("no notification event: {other:?}"),
    }

    // A restarted server remembers it from the library.
    let restarted = AppState::open(t.state.config().clone()).unwrap();
    let err = quota::reserve(&restarted, ALICE, 20_000).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::QuotaExceeded);
    assert_eq!(notices(&t, ALICE).len(), 1);
    drop(restarted);

    // A day later, one more.
    tokio::time::advance(HOUR * 23).await;
    quota::reserve(&t.state, ALICE, 20_000).await.unwrap_err();
    assert_eq!(notices(&t, ALICE).len(), 1, "23 hours later");
    tokio::time::advance(HOUR).await;
    quota::reserve(&t.state, ALICE, 20_000).await.unwrap_err();
    quota::reserve(&t.state, ALICE, 20_000).await.unwrap_err();
    assert_eq!(notices(&t, ALICE).len(), 2);
    // The media budget is not the user's to fix: no notification.
}

#[tokio::test]
async fn get_me_usage_shows_a_commit_at_once() {
    let t = TestState::new();
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let owner_id = owner(&t);
    let read =
        || async { json(send(&app, with_session(get("/api/v1/me/usage"), &cookie)).await).await };
    assert_eq!(read().await["usedBytes"], 0);

    let reservation = quota::reserve(&t.state, &owner_id, 70_000).await.unwrap();
    let staged = stage(&t, &owner_id, vec![jpeg_like(3, 64_000)]).await;
    record(&t, &owner_id, staged, reservation).await.unwrap();
    let usage = read().await;
    assert_eq!(usage["mediaBytes"], 64_000);
    assert_eq!(usage["usedBytes"], 64_000);
    assert_eq!(usage["quotaBytes"], 0, "the owner is unlimited");
    assert_eq!(usage["updatedAt"], Value::Null, "a commit is not a count");
}

#[tokio::test]
async fn the_budget_starts_from_the_disk_sample_of_the_users_area() {
    let t = TestState::new();
    t.add_user(ALICE);
    let reservation = quota::reserve(&t.state, ALICE, 40_000).await.unwrap();
    let staged = stage(&t, ALICE, vec![jpeg_like(5, 40_000)]).await;
    record(&t, ALICE, staged, reservation).await.unwrap();

    sample_disk(&t.state).await;
    let on_disk = area_bytes(t.data_dir().root(), USERS_AREA);
    assert!(on_disk >= 40_000);
    let budget = t.state.quota().budget().await;
    assert_eq!(budget.limit_bytes, 30 * GIB, "the default");
    assert_eq!(budget.used_bytes, on_disk);
    // What is reserved or committed after the sample counts on top of it.
    let held = quota::reserve(&t.state, ALICE, 1_000).await.unwrap();
    assert_eq!(t.state.quota().budget().await.used_bytes, on_disk + 1_000);
    drop(held);

    // Before any sample, the first reservation measures the area.
    let fresh = AppState::open(t.state.config().clone()).unwrap();
    drop(quota::reserve(&fresh, ALICE, 1).await.unwrap());
    let measured = fresh.quota().budget().await.used_bytes;
    assert_eq!(measured, area_bytes(t.data_dir().root(), USERS_AREA));
}

#[tokio::test]
async fn daily_counters_add_up_per_day() {
    let t = TestState::new();
    t.add_user(ALICE);
    quota::bump_daily(&t.state, ALICE, Field::Captures, 1)
        .await
        .unwrap();
    quota::bump_daily(&t.state, ALICE, Field::Captures, 1)
        .await
        .unwrap();
    quota::bump_daily(&t.state, ALICE, Field::IngestItems, 120)
        .await
        .unwrap();
    let err = quota::bump_daily(&t.state, ALICE, Field::BytesIn, -5)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::ValidationFailed);
    let today = t
        .state
        .control()
        .read(|c| usage_daily::of_day(c, ALICE, t.state.jobs().clock().now_ms()))
        .unwrap();
    assert_eq!(
        (today.captures, today.ingest_items, today.bytes_in),
        (2, 120, 0)
    );
}

fn admin(data: &DataDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_shelfy-server"))
        .arg("admin")
        .args(args)
        .env("SHELFY_DATA_DIR", data.root())
        .env_remove("SHELFY_OWNER_EMAIL")
        .env_remove("RUST_LOG")
        .output()
        .expect("run shelfy-server")
}

fn audit_rows(data: &DataDir) -> Vec<(String, String, String)> {
    let conn = Connection::open(data.control_db()).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT action, target, meta_json FROM audit_log WHERE action = 'user.limits'
             ORDER BY id",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test]
async fn admin_user_limits_sets_audits_and_prints_the_limits() {
    let t = TestState::new();
    let data = t.data_dir();
    let id = create_owner(&data, "owner@example.test")
        .unwrap()
        .user_id()
        .to_owned();

    // In process: only what is given changes, and the change is audited.
    let change = limits(&data, &id, Some(5 * 1024 * 1024), None).unwrap();
    assert_eq!(
        change.before,
        Limits {
            quota_bytes: 0,
            capture_daily_limit: 20,
        }
    );
    assert_eq!(change.after.quota_bytes, 5 * 1024 * 1024);
    assert_eq!(change.after.capture_daily_limit, 20);
    let rows = audit_rows(&data);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, id);
    let meta: Value = serde_json::from_str(&rows[0].2).unwrap();
    assert_eq!(meta["via"], "cli");
    assert_eq!(meta["quotaBytes"], 5 * 1024 * 1024);
    assert_eq!(meta["previous"]["quotaBytes"], 0);
    // The running server's next reservation reads the new quota.
    let err = quota::reserve(&t.state, &id, 6 * 1024 * 1024)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::QuotaExceeded);
    // Reading changes nothing and audits nothing.
    assert_eq!(limits(&data, &id, None, None).unwrap().after, change.after);
    assert_eq!(audit_rows(&data).len(), 1);

    // Through the binary.
    let output = admin(
        &data,
        &[
            "user",
            "limits",
            &id,
            "--quota-gb",
            "2.5",
            "--capture-daily",
            "0",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        format!(
            "updated the limits of user {id}\nquota: 2.500 GiB (2684354560 bytes)\ncaptures a \
             day: unlimited\n"
        )
    );
    let output = admin(&data, &["user", "limits", &id, "--quota-gb", "0"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("quota: unlimited\n"), "{stdout}");
    let output = admin(&data, &["user", "limits", &id]);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("limits of user {id}\nquota: unlimited\ncaptures a day: unlimited\n")
    );
    assert_eq!(audit_rows(&data).len(), 3, "two changes, one read");

    // Refusals: an unknown user, a bad value. Nothing on stdout.
    for args in [
        vec![
            "user",
            "limits",
            "01NOBODY000000000000000000",
            "--quota-gb",
            "1",
        ],
        vec!["user", "limits", &id, "--quota-gb", "-1"],
        vec!["user", "limits", &id, "--capture-daily", "many"],
        vec!["user", "limits", "../etc", "--quota-gb", "1"],
    ] {
        let output = admin(&data, &args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
    assert_eq!(audit_rows(&data).len(), 3);
    let distinct: HashSet<_> = audit_rows(&data).into_iter().map(|r| r.2).collect();
    assert_eq!(distinct.len(), 3);
}
