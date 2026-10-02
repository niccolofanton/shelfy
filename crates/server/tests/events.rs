//! `GET /api/v1/events` (plan §2.10, P1-01), one test per behavior: `hello`
//! and the heartbeat, the stream headers, the `topics` filter, the per-user
//! broadcast, `Last-Event-ID` replay from the 256-event, 5-minute ring,
//! `resync` for old gaps, foreign ids and lagging streams, the §2.10
//! throttles, isolation between users, and the end of a stream at shutdown
//! and with its session.
//!
//! Most tests run on paused time: timers fire as soon as the test waits for
//! them, and the elapsed time shows each throttle exactly.

mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use axum::Router;
use axum::http::{StatusCode, header};
use rusqlite::{Connection, params};
use serde_json::json;
use shelfy_server::auth::cookie::SESSION_COOKIE;
use shelfy_server::error::ErrorCode;
use shelfy_server::events::model::{ChangeReason, JobState, JobUpdatedEvent, Notification};
use shelfy_server::events::{
    CHANNEL_CAPACITY, JOB_WINDOW, MAX_EVENT_KEYS, POSTS_WINDOW, REPLAY_EVENTS, REPLAY_WINDOW,
    STATS_WINDOW,
};
use shelfy_server::ids::now_ms;
use shelfy_server::routes::events::{HEARTBEAT, HelloEvent};
use shelfy_server::tokens::hash_token;
use support::auth::{LINK_PATH, OWNER_EMAIL, link_token, owner, post, sign_in, spa, with_session};
use support::library::{ALICE, BOB, NOW};
use support::sse::{Stream, assert_event_schema};
use support::{TestState, get, problem, send};
use tokio::time::Instant;

const EVENTS: &str = "/api/v1/events";

/// A notification that is not stored: an unthrottled event to publish.
fn note(id: i64) -> Notification {
    Notification {
        id,
        kind: "job".into(),
        code: "job.failed".into(),
        params: BTreeMap::new(),
        target: None,
        created_at: NOW,
        read_at: None,
    }
}

fn job(id: i64, state: JobState, progress: f64) -> JobUpdatedEvent {
    JobUpdatedEvent {
        id,
        kind: "archive.drain".into(),
        state,
        progress: Some(progress),
        stage: None,
        post_key: None,
        error_code: None,
    }
}

fn keys(list: &[&str]) -> Option<Vec<String>> {
    Some(list.iter().map(|&k| k.to_owned()).collect())
}

/// A stream of `user` that has read its `hello`; returns the `hello` data.
async fn open(app: &Router, uri: &str, headers: &[(&str, &str)]) -> (Stream, HelloEvent) {
    let mut stream = Stream::connect(app, uri, headers).await;
    let hello = stream.hello().await;
    assert_event_schema(&hello);
    (stream, serde_json::from_value(hello.json()).unwrap())
}

#[tokio::test(start_paused = true)]
async fn the_stream_says_hello_then_heartbeats_until_shutdown() {
    let t = TestState::new();
    let mut stream = Stream::connect(&t.app_as(ALICE), EVENTS, &[]).await;
    let started = Instant::now();
    let hello = stream.hello().await;
    assert_event_schema(&hello);
    let data: HelloEvent = serde_json::from_value(hello.json()).unwrap();
    assert_eq!(data.version, shelfy_server::VERSION);
    assert_eq!(data.heartbeat_ms, 20_000);
    // A fresh stream starts at the current position, and says so in its id.
    assert_eq!(hello.id.as_deref(), Some(data.last_event_id.as_str()));
    assert_eq!(started.elapsed(), Duration::ZERO, "hello comes at once");

    // Three beats: a minute, past the 30 s limit of the standard routes.
    for n in 1..=3 {
        assert!(stream.next().await.is_heartbeat());
        assert_eq!(started.elapsed(), HEARTBEAT * n);
    }

    t.state.shutdown_token().cancel();
    assert!(stream.ended().await, "the stream ends at shutdown");
}

#[tokio::test]
async fn the_stream_is_never_buffered_cached_or_compressed() {
    let t = TestState::new();
    let stream =
        Stream::connect(&t.app_as(ALICE), EVENTS, &[("accept-encoding", "gzip, br")]).await;
    assert_eq!(stream.headers[header::CONTENT_TYPE], "text/event-stream");
    assert_eq!(stream.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(stream.headers["x-accel-buffering"], "no");
    assert!(stream.headers.get(header::CONTENT_ENCODING).is_none());
}

#[tokio::test(start_paused = true)]
async fn topics_select_the_events_of_a_stream() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let events = t.state.events();
    let uri = format!("{EVENTS}?topics=stats.changed&topics=notification");
    let (mut filtered, _) = open(&app, &uri, &[]).await;
    let (mut all, _) = open(&app, EVENTS, &[]).await;
    let started = Instant::now();

    events.posts_changed(ALICE, ChangeReason::Edit, keys(&["ig_1"]));
    events.job_updated(ALICE, job(1, JobState::Running, 0.5));
    events.stats_changed(ALICE);
    events.notification(ALICE, &note(1));

    assert_event_schema(&filtered.expect("stats.changed").await);
    assert_event_schema(&filtered.expect("notification").await);
    assert!(filtered.next().await.is_heartbeat(), "nothing else came");
    assert_eq!(started.elapsed(), HEARTBEAT);
    // Without `topics`, everything comes.
    for name in [
        "posts.changed",
        "job.updated",
        "stats.changed",
        "notification",
    ] {
        assert_event_schema(&all.expect(name).await);
    }

    // Unknown topics are refused, as are absurd lists.
    for (uri, status, code) in [
        (
            format!("{EVENTS}?topics=ai.stream"),
            StatusCode::BAD_REQUEST,
            ErrorCode::BadRequest,
        ),
        (
            format!("{EVENTS}?{}", "topics=notification&".repeat(51)),
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::ValidationFailed,
        ),
    ] {
        let problem = problem(send(&app, get(&uri)).await, status).await;
        assert_eq!(problem.code, code, "{uri}");
    }
}

#[tokio::test]
async fn every_stream_of_a_user_gets_every_event() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let (mut tab1, hello1) = open(&app, EVENTS, &[]).await;
    let (mut tab2, hello2) = open(&app, EVENTS, &[]).await;
    assert_eq!(
        hello1.last_event_id, hello2.last_event_id,
        "one stream per user"
    );
    assert_eq!(t.state.events().connections(), 2);

    t.state.events().notification(ALICE, &note(7));
    let (a, b) = (
        tab1.expect("notification").await,
        tab2.expect("notification").await,
    );
    assert_eq!(a, b);
    assert_eq!(a.json()["id"], 7);
    assert_ne!(
        a.id,
        Some(hello1.last_event_id),
        "the event has the next id"
    );
    drop((tab1, tab2));
    assert_eq!(t.state.events().connections(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_resumed_stream_replays_what_it_missed_then_goes_live() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let events = t.state.events();
    let (mut first, _) = open(&app, EVENTS, &[]).await;
    events.notification(ALICE, &note(1));
    let seen = first.expect("notification").await.id.unwrap();
    drop(first);
    events.notification(ALICE, &note(2));
    events.notification(ALICE, &note(3));

    // `EventSource` reconnecting by itself sends `Last-Event-ID`.
    let mut resumed = Stream::connect(&app, EVENTS, &[("last-event-id", &seen)]).await;
    let hello = resumed.hello().await;
    assert_eq!(
        hello.id, None,
        "a resumed hello leaves the client's position alone"
    );
    for n in [2, 3] {
        assert_eq!(resumed.expect("notification").await.json()["id"], n);
    }
    events.notification(ALICE, &note(4));
    let live = resumed.expect("notification").await;
    assert_eq!(live.json()["id"], 4);
    let newest = live.id.unwrap();

    // A client that opens a new `EventSource` passes `lastEventId`.
    let uri = format!("{EVENTS}?lastEventId={seen}");
    let mut by_query = Stream::connect(&app, &uri, &[]).await;
    by_query.hello().await;
    for n in [2, 3, 4] {
        assert_eq!(by_query.expect("notification").await.json()["id"], n);
    }

    // The header wins over the query: nothing was missed after `newest`.
    let started = Instant::now();
    let mut both = Stream::connect(&app, &uri, &[("last-event-id", &newest)]).await;
    both.hello().await;
    assert!(both.next().await.is_heartbeat());
    assert_eq!(started.elapsed(), HEARTBEAT);
}

#[tokio::test]
async fn the_ring_replays_the_last_256_events() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let events = t.state.events();
    let (mut live, _) = open(&app, EVENTS, &[]).await;
    let total = i64::try_from(REPLAY_EVENTS).unwrap() + 44;
    let mut ids = Vec::new();
    for n in 1..=total {
        events.notification(ALICE, &note(n));
        ids.push(live.expect("notification").await.id.unwrap());
    }

    // After event 44, the 256 events 45..=300 are all still kept.
    let mut resumed = Stream::connect(&app, EVENTS, &[("last-event-id", &ids[43])]).await;
    resumed.hello().await;
    for n in 45..=total {
        let frame = resumed.expect("notification").await;
        assert_eq!(frame.json()["id"], n);
        assert_eq!(
            frame.id.as_ref(),
            Some(&ids[usize::try_from(n - 1).unwrap()])
        );
    }

    // One more back is gone: resync, positioned at the newest event.
    let mut late = Stream::connect(&app, EVENTS, &[("last-event-id", &ids[42])]).await;
    late.hello().await;
    let resync = late.expect("resync").await;
    assert_event_schema(&resync);
    assert_eq!(resync.json(), json!({ "reason": "expired" }));
    assert_eq!(resync.id.as_ref(), ids.last());
    events.notification(ALICE, &note(total + 1));
    assert_eq!(late.expect("notification").await.json()["id"], total + 1);
}

#[tokio::test(start_paused = true)]
async fn the_ring_forgets_events_after_five_minutes() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let events = t.state.events();
    let (stream, hello) = open(&app, EVENTS, &[]).await;
    drop(stream);
    let start = hello.last_event_id;
    events.notification(ALICE, &note(1));

    tokio::time::advance(REPLAY_WINDOW - Duration::from_millis(1)).await;
    let mut resumed = Stream::connect(&app, EVENTS, &[("last-event-id", &start)]).await;
    resumed.hello().await;
    let replayed = resumed.expect("notification").await;
    drop(resumed);

    tokio::time::advance(Duration::from_millis(1)).await;
    let mut late = Stream::connect(&app, EVENTS, &[("last-event-id", &start)]).await;
    late.hello().await;
    assert_eq!(late.expect("resync").await.json()["reason"], "expired");

    // A client that had seen everything missed nothing, however long ago.
    let started = Instant::now();
    let newest = replayed.id.unwrap();
    let mut current = Stream::connect(&app, EVENTS, &[("last-event-id", &newest)]).await;
    current.hello().await;
    assert!(current.next().await.is_heartbeat());
    assert_eq!(started.elapsed(), HEARTBEAT);
}

#[tokio::test]
async fn resume_points_from_elsewhere_get_resync() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let (_alice, alice) = open(&app, EVENTS, &[]).await;
    let (_bob, bob) = open(&t.app_as(BOB), EVENTS, &[]).await;
    let (epoch, _) = alice.last_event_id.rsplit_once('-').unwrap();
    let future = format!("{epoch}-99");
    for resume in [
        bob.last_event_id.as_str(),
        future.as_str(),
        "garbage",
        "zz-1",
        "-",
    ] {
        let mut stream = Stream::connect(&app, EVENTS, &[("last-event-id", resume)]).await;
        stream.hello().await;
        let resync = stream.expect("resync").await;
        assert_eq!(resync.json()["reason"], "unknown", "{resume}");
        assert_eq!(resync.id.as_ref(), Some(&alice.last_event_id), "{resume}");
    }
}

#[tokio::test]
async fn a_stream_that_falls_256_events_behind_gets_resync() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let events = t.state.events();
    // The handler subscribed; nothing reads the stream yet.
    let mut slow = Stream::connect(&app, EVENTS, &[]).await;
    let (mut fast, _) = open(&app, EVENTS, &[]).await;
    let capacity = i64::try_from(CHANNEL_CAPACITY).unwrap();

    // Exactly the capacity behind: nothing is lost.
    for n in 1..=capacity {
        events.notification(ALICE, &note(n));
        fast.expect("notification").await;
    }
    slow.hello().await;
    for n in 1..=capacity {
        assert_eq!(slow.expect("notification").await.json()["id"], n);
    }

    // One more than that: the stream skips to the newest event and says so.
    let mut newest = None;
    for n in capacity + 1..=2 * capacity + 1 {
        events.notification(ALICE, &note(n));
        newest = fast.expect("notification").await.id;
    }
    let resync = slow.expect("resync").await;
    assert_event_schema(&resync);
    assert_eq!(resync.json(), json!({ "reason": "lagged" }));
    assert_eq!(resync.id, newest);
    // Then live events again.
    events.notification(ALICE, &note(1_000));
    assert_eq!(slow.expect("notification").await.json()["id"], 1_000);
}

#[tokio::test(start_paused = true)]
async fn posts_changed_goes_out_at_once_then_merges_for_two_seconds() {
    let t = TestState::new();
    let events = t.state.events();
    let (mut stream, _) = open(&t.app_as(ALICE), EVENTS, &[]).await;
    let started = Instant::now();

    events.posts_changed(ALICE, ChangeReason::Edit, keys(&["ig_1"]));
    let first = stream.expect("posts.changed").await;
    assert_event_schema(&first);
    assert_eq!(first.json(), json!({ "keys": ["ig_1"], "reason": "edit" }));
    assert_eq!(started.elapsed(), Duration::ZERO, "leading edge");

    tokio::time::advance(Duration::from_millis(100)).await;
    events.posts_changed(ALICE, ChangeReason::Edit, keys(&["ig_2"]));
    tokio::time::advance(Duration::from_millis(500)).await;
    events.posts_changed(ALICE, ChangeReason::Edit, keys(&["ig_3", "ig_2"]));
    // Each reason has its own throttle.
    events.posts_changed(ALICE, ChangeReason::Ai, None);
    let other = stream.expect("posts.changed").await;
    assert_eq!(other.json(), json!({ "keys": null, "reason": "ai" }));
    assert_eq!(started.elapsed(), Duration::from_millis(600));

    let merged = stream.expect("posts.changed").await;
    assert_eq!(
        merged.json(),
        json!({ "keys": ["ig_2", "ig_3"], "reason": "edit" })
    );
    assert_eq!(started.elapsed(), POSTS_WINDOW, "flushed within 2 s");

    // Past the window, the next change is a leading edge again; too many
    // keys become `null`.
    tokio::time::advance(POSTS_WINDOW).await;
    let many: Vec<String> = (0..=MAX_EVENT_KEYS).map(|n| format!("ig_{n}")).collect();
    events.posts_changed(ALICE, ChangeReason::Ingest, Some(many));
    let ingest = stream.expect("posts.changed").await;
    assert_eq!(ingest.json(), json!({ "keys": null, "reason": "ingest" }));
    assert_eq!(started.elapsed(), POSTS_WINDOW * 2);
}

#[tokio::test(start_paused = true)]
async fn stats_changed_goes_out_at_most_once_a_second() {
    let t = TestState::new();
    let events = t.state.events();
    let (mut stream, _) = open(&t.app_as(ALICE), EVENTS, &[]).await;
    let started = Instant::now();
    for _ in 0..5 {
        events.stats_changed(ALICE);
    }
    let first = stream.expect("stats.changed").await;
    assert_event_schema(&first);
    assert_eq!(first.json(), json!({}));
    assert_eq!(started.elapsed(), Duration::ZERO);
    stream.expect("stats.changed").await;
    assert_eq!(started.elapsed(), STATS_WINDOW);
    assert!(
        stream.next().await.is_heartbeat(),
        "five changes, two events"
    );
}

#[tokio::test(start_paused = true)]
async fn job_updated_sends_the_latest_state_of_each_job_every_250_ms() {
    let t = TestState::new();
    let events = t.state.events();
    let (mut stream, _) = open(&t.app_as(ALICE), EVENTS, &[]).await;
    let started = Instant::now();

    events.job_updated(ALICE, job(1, JobState::Running, 0.1));
    events.job_updated(ALICE, job(2, JobState::Running, 0.5));
    events.job_updated(ALICE, job(1, JobState::Running, 0.2));
    events.job_updated(ALICE, job(1, JobState::Succeeded, 1.0));

    let mut at_once = Vec::new();
    for _ in 0..2 {
        let frame = stream.expect("job.updated").await;
        assert_event_schema(&frame);
        at_once.push((frame.json()["id"].clone(), frame.json()["progress"].clone()));
    }
    assert_eq!(at_once, [(json!(1), json!(0.1)), (json!(2), json!(0.5))]);
    assert_eq!(started.elapsed(), Duration::ZERO);

    let latest = stream.expect("job.updated").await.json();
    assert_eq!(
        (latest["id"].clone(), latest["state"].clone()),
        (json!(1), json!("succeeded"))
    );
    assert_eq!(latest["progress"], 1.0);
    assert_eq!(started.elapsed(), JOB_WINDOW);
    assert!(stream.next().await.is_heartbeat());
}

#[tokio::test(start_paused = true)]
async fn notifications_are_never_held() {
    let t = TestState::new();
    let events = t.state.events();
    let (mut stream, _) = open(&t.app_as(ALICE), EVENTS, &[]).await;
    let started = Instant::now();
    for n in 1..=3 {
        events.notification(ALICE, &note(n));
    }
    for n in 1..=3 {
        let frame = stream.expect("notification").await;
        assert_event_schema(&frame);
        assert_eq!(frame.json()["id"], n);
    }
    assert_eq!(started.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn users_never_receive_each_others_events() {
    let t = TestState::new();
    let events = t.state.events();
    let (mut alice, _) = open(&t.app_as(ALICE), EVENTS, &[]).await;
    let (mut bob, _) = open(&t.app_as(BOB), EVENTS, &[]).await;
    let started = Instant::now();

    events.posts_changed(BOB, ChangeReason::Edit, keys(&["ig_9001"]));
    events.stats_changed(BOB);
    events.job_updated(BOB, job(5, JobState::Failed, 0.0));
    events.notification(BOB, &note(9));
    let mut last = None;
    for name in [
        "posts.changed",
        "stats.changed",
        "job.updated",
        "notification",
    ] {
        last = bob.expect(name).await.id;
    }

    assert!(alice.next().await.is_heartbeat(), "Alice got none of it");
    assert_eq!(started.elapsed(), HEARTBEAT);
    // Bob's ids mean nothing on Alice's stream: no replay of his events.
    let app = t.app_as(ALICE);
    let mut resumed = Stream::connect(&app, EVENTS, &[("last-event-id", &last.unwrap())]).await;
    resumed.hello().await;
    assert_eq!(resumed.expect("resync").await.json()["reason"], "unknown");
}

/// Reads `stream` to its end, which must come within one heartbeat of
/// `since`, with nothing but heartbeats before it.
async fn ends_within_a_heartbeat(stream: &mut Stream, since: Instant) {
    while let Some(frame) = stream.next_or_end().await {
        assert!(frame.is_heartbeat(), "{frame:?}");
    }
    assert!(
        since.elapsed() <= HEARTBEAT,
        "ended after {:?}",
        since.elapsed()
    );
}

/// A stream on a new session of the owner, its `hello` read; and the
/// session cookie.
async fn signed_in_stream(app: &Router, t: &TestState) -> (Stream, String) {
    let cookie = sign_in(app, t).await;
    let session = format!("{SESSION_COOKIE}={cookie}");
    let (stream, _) = open(app, EVENTS, &[("cookie", &session)]).await;
    (stream, cookie)
}

// Each session below ends before its stream's first check. (A check already
// in flight when its session ends could cache the session again: the T10
// hardening, F1, closes that race in the session cache.)
#[tokio::test(start_paused = true)]
async fn a_stream_ends_within_a_heartbeat_of_its_session() {
    let t = TestState::new();
    let app = t.app();
    let owner_id = owner(&t);

    // Signed out: that stream ends, while another session's stream goes on.
    let (mut signed_out, cookie) = signed_in_stream(&app, &t).await;
    let (mut other, _) = signed_in_stream(&app, &t).await;
    let started = Instant::now();
    let response = send(&app, spa(&t, post("/api/v1/auth/logout"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    ends_within_a_heartbeat(&mut signed_out, started).await;
    t.state.events().notification(&owner_id, &note(1));
    assert_eq!(other.expect("notification").await.json()["id"], 1);
    drop(other);

    // Replaced: the same browser signs in again.
    let (mut replaced, cookie) = signed_in_stream(&app, &t).await;
    let started = Instant::now();
    let link = format!("{LINK_PATH}{}", link_token(&t, OWNER_EMAIL));
    let response = send(&app, with_session(get(&link), &cookie)).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    ends_within_a_heartbeat(&mut replaced, started).await;

    // Expired, once the 60 s session cache lets go of it.
    let (mut expired, cookie) = signed_in_stream(&app, &t).await;
    let started = Instant::now();
    Connection::open(t.data_dir().control_db())
        .unwrap()
        .execute(
            "UPDATE sessions SET expires_at = ?1 WHERE id_hash = ?2",
            params![now_ms() - 1, hash_token(&cookie).as_slice()],
        )
        .unwrap();
    t.state.auth().forget_all_sessions();
    ends_within_a_heartbeat(&mut expired, started).await;

    // Signed out everywhere: every stream of the user ends.
    let (mut first, cookie) = signed_in_stream(&app, &t).await;
    let (mut second, _) = signed_in_stream(&app, &t).await;
    let started = Instant::now();
    let response = send(&app, spa(&t, post("/api/v1/auth/logout-all"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    ends_within_a_heartbeat(&mut first, started).await;
    ends_within_a_heartbeat(&mut second, started).await;

    // And the reconnect is refused.
    let session = format!("{SESSION_COOKIE}={cookie}");
    let refused = Stream::open(&app, EVENTS, &[("cookie", &session)]).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
}
