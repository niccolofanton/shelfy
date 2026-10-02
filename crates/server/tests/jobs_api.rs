//! The job routes through the real middleware stack (plan §2.9 Jobs):
//! listing, the summary, cancel and retry, the queue actions, the
//! `Idempotency-Key` replay, and the authz rules every new route ships (P1
//! lane rule 4: 401 without a session, 404 for another user's job, API
//! tokens refused). The scheduler itself is checked in `jobs.rs`.
//!
//! The authz tests sign the owner in for real; the others carry a user
//! through the test-only stand-in for authentication (`TestState::app_as`).

mod support;

use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use rusqlite::params;
use serde_json::{Value, json};
use shelfy_server::error::ErrorCode;
use shelfy_server::events::model::JobState;
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::jobs::idempotency::{IDEMPOTENCY_KEY, REPLAYED};
use shelfy_server::jobs::{NewJob, Registry};
use shelfy_server::routes::{self, IDEMPOTENT_ROUTES};
use shelfy_server::tokens::{SecretToken, hash_token};
use support::auth::{owner, sign_in, spa, with_session};
use support::jobs::{Probe, START, kind, mode};
use support::library::{ALICE, BOB};
use support::sse::assert_schema;
use support::{TestState, from_app, get, json, problem, send};
use tokio_util::sync::CancellationToken;

const JOBS: &str = "/api/v1/jobs";
const SUMMARY: &str = "/api/v1/jobs/summary";

/// A state with the kinds `test.a` and `test.b`, Alice and Bob.
fn state() -> (TestState, Probe) {
    let probe = Probe::new();
    let t = TestState::with_jobs(
        Registry::new()
            .register(kind("test.a", 1, 1, &probe))
            .register(kind("test.b", 1, 1, &probe)),
    );
    t.add_user(ALICE);
    t.add_user(BOB);
    (t, probe)
}

/// A `POST` without a body, as the web app sends it.
fn post(uri: &str) -> Request<Body> {
    from_app(Request::post(uri).body(Body::empty()).unwrap())
}

/// `POST` with an `Idempotency-Key`.
fn post_with_key(uri: &str, key: &str) -> Request<Body> {
    let mut request = post(uri);
    request
        .headers_mut()
        .insert(IDEMPOTENCY_KEY, key.parse().unwrap());
    request
}

/// Sets a job's state as if it had run.
fn set_state(t: &TestState, id: i64, state: &str, error: Option<&str>) {
    t.control()
        .execute(
            "UPDATE jobs SET state = ?2, error_code = ?3, attempts = 3, \
             finished_at = CASE WHEN ?2 IN ('succeeded', 'failed', 'cancelled') THEN ?4 END \
             WHERE id = ?1",
            params![id, state, error, START + 1],
        )
        .unwrap();
}

/// Every request of the job routes, for a job id and a kind.
fn job_requests(id: i64, kind: &str) -> Vec<Request<Body>> {
    vec![
        get(JOBS),
        get(SUMMARY),
        post(&format!("{JOBS}/{id}/cancel")),
        post(&format!("{JOBS}/{id}/retry")),
        post(&format!("/api/v1/queues/{kind}/pause")),
        post(&format!("/api/v1/queues/{kind}/resume")),
        post(&format!("/api/v1/queues/{kind}/cancel-all")),
        post(&format!("/api/v1/queues/{kind}/clear-finished")),
    ]
}

/// An API token of `user_id` with every scope (P1-17 adds the minting route).
fn api_token(t: &TestState, user_id: &str) -> String {
    let token = format!("shx_{}", SecretToken::generate().expose());
    t.control()
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
             VALUES (?1, ?2, 'extension', ?3, ?4, ?5)",
            params![
                new_ulid(),
                user_id,
                hash_token(&token).as_slice(),
                "ingest tasks uploads lookup links:create migrate",
                now_ms()
            ],
        )
        .unwrap();
    token
}

fn bearer(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    request
}

#[tokio::test(start_paused = true)]
async fn every_job_route_needs_a_session() {
    let (t, _) = state();
    let job = t.enqueue(ALICE, "test.a", mode("ok")).await;
    let app = t.app();
    for request in job_requests(job, "test.a") {
        let route = format!("{} {}", request.method(), request.uri());
        let refused = problem(send(&app, request).await, StatusCode::UNAUTHORIZED).await;
        assert_eq!(refused.code, ErrorCode::Unauthorized, "{route}");
    }
    assert_eq!(t.job(ALICE, job).await.state, JobState::Queued, "untouched");
}

/// Lane rule 4: the job routes are cookie-only. A valid token with every
/// scope is refused, alone or beside a valid session cookie; the cookie
/// alone works.
#[tokio::test(start_paused = true)]
async fn api_tokens_never_call_the_job_routes() {
    let (t, _) = state();
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let token = api_token(&t, &owner_id);
    let job = t.enqueue(&owner_id, "test.a", mode("ok")).await;

    for with_cookie in [false, true] {
        for request in job_requests(job, "test.a") {
            let route = format!("{} {}", request.method(), request.uri());
            let request = if with_cookie {
                with_session(request, &cookie)
            } else {
                request
            };
            let response = send(&app, bearer(request, &token)).await;
            let refused = problem(response, StatusCode::UNAUTHORIZED).await;
            assert_eq!(refused.code, ErrorCode::Unauthorized, "{route}");
        }
    }
    assert_eq!(t.job(&owner_id, job).await.state, JobState::Queued);

    // The session alone, through the gate and the CSRF guard.
    for request in job_requests(job, "test.a") {
        let route = format!("{} {}", request.method(), request.uri());
        let response = send(&app, spa(&t, request, &cookie)).await;
        let status = response.status();
        assert!(
            status.is_success() || status == StatusCode::CONFLICT,
            "{route}: {status}"
        );
    }
    let mut forged = with_session(post(&format!("{JOBS}/{job}/cancel")), &cookie);
    forged.headers_mut().remove("x-shelfy-client");
    let refused = problem(send(&app, forged).await, StatusCode::FORBIDDEN).await;
    assert_eq!(refused.code, ErrorCode::CsrfFailed);
}

#[tokio::test(start_paused = true)]
async fn another_users_jobs_are_out_of_reach() {
    let (t, _) = state();
    let bobs = t.enqueue(BOB, "test.a", mode("ok")).await;
    let failed = t.enqueue(BOB, "test.a", mode("ok")).await;
    set_state(&t, failed, "failed", Some("unavailable"));
    let alice = t.app_as(ALICE);

    for uri in [
        format!("{JOBS}/{bobs}/cancel"),
        format!("{JOBS}/{failed}/retry"),
        format!("{JOBS}/987654/cancel"),
    ] {
        let missing = problem(send(&alice, post(&uri)).await, StatusCode::NOT_FOUND).await;
        assert_eq!(missing.code, ErrorCode::NotFound, "{uri}");
    }
    let page = json(send(&alice, get(JOBS)).await).await;
    assert_eq!(page["items"], json!([]));
    let summary = json(send(&alice, get(SUMMARY)).await).await;
    assert!(
        summary["queues"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["queued"] == 0 && q["failed"] == 0),
        "{summary}"
    );
    // Alice's queue actions never reach Bob's jobs.
    for action in ["pause", "cancel-all", "clear-finished"] {
        let response = send(&alice, post(&format!("/api/v1/queues/test.a/{action}"))).await;
        assert_eq!(response.status(), StatusCode::OK, "{action}");
    }
    assert_eq!(t.job(BOB, bobs).await.state, JobState::Queued);
    assert_eq!(t.job(BOB, failed).await.state, JobState::Failed);
    assert!(!t.state.jobs().is_paused(BOB, "test.a"));
    // An unknown kind is no queue of anyone's.
    let unknown = send(&alice, post("/api/v1/queues/test.zzz/pause")).await;
    assert_eq!(
        problem(unknown, StatusCode::NOT_FOUND).await.code,
        ErrorCode::NotFound
    );
    // A malformed id is a bad request.
    let malformed = send(&alice, post(&format!("{JOBS}/abc/cancel"))).await;
    assert_eq!(
        problem(malformed, StatusCode::BAD_REQUEST).await.code,
        ErrorCode::BadRequest
    );
}

#[tokio::test(start_paused = true)]
async fn jobs_are_listed_newest_first_with_filters_and_a_cursor() {
    let (t, _) = state();
    let mut ids = Vec::new();
    for (kind, payload) in [
        ("test.a", json!({ "postKey": "ig_1001" })),
        ("test.b", json!({})),
        ("test.a", json!({ "mode": "ok" })),
        ("test.a", json!({})),
    ] {
        let job = NewJob::new(ALICE, kind).payload(payload);
        ids.push(t.state.jobs().enqueue(job).await.unwrap().job.id);
    }
    set_state(&t, ids[2], "failed", Some("unavailable"));
    let app = t.app_as(ALICE);

    let response = send(&app, get(JOBS)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let page = json(response).await;
    assert_schema(&page, "JobPage");
    let listed: Vec<i64> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["id"].as_i64().unwrap())
        .collect();
    assert_eq!(listed, [ids[3], ids[2], ids[1], ids[0]]);
    let oldest = &page["items"][3];
    assert_eq!(oldest["postKey"], "ig_1001");
    assert_eq!(oldest["kind"], "test.a");
    assert_eq!(oldest["state"], "queued");
    assert_eq!(oldest["attempts"], 0);
    assert_eq!(oldest["maxAttempts"], 3);
    assert_eq!(oldest["progress"], Value::Null);
    assert!(
        oldest.get("payload").is_none(),
        "the payload stays server-side"
    );
    let failed = &page["items"][1];
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["errorCode"], "unavailable");
    assert_eq!(failed["finishedAt"], START + 1);

    let filtered = json(send(&app, get(&format!("{JOBS}?kind=test.a&state=queued"))).await).await;
    let filtered: Vec<i64> = filtered["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["id"].as_i64().unwrap())
        .collect();
    assert_eq!(filtered, [ids[3], ids[0]]);
    let either = json(
        send(
            &app,
            get(&format!("{JOBS}?state=failed&state=queued&kind=test.b")),
        )
        .await,
    )
    .await;
    assert_eq!(either["items"].as_array().unwrap().len(), 1);

    let mut seen = Vec::new();
    let mut uri = format!("{JOBS}?limit=3");
    loop {
        let page = json(send(&app, get(&uri)).await).await;
        seen.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|job| job["id"].as_i64().unwrap()),
        );
        match page["nextCursor"].as_str() {
            Some(cursor) => uri = format!("{JOBS}?limit=3&cursor={cursor}"),
            None => break,
        }
    }
    assert_eq!(seen, listed);

    for (uri, code) in [
        (format!("{JOBS}?cursor=garbage"), ErrorCode::InvalidCursor),
        (format!("{JOBS}?state=done"), ErrorCode::BadRequest),
        (
            format!("{JOBS}?kind={}", "k".repeat(65)),
            ErrorCode::ValidationFailed,
        ),
    ] {
        let response = send(&app, get(&uri)).await;
        let status = response.status();
        assert!(status.is_client_error(), "{uri}");
        assert_eq!(problem(response, status).await.code, code, "{uri}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_summary_counts_jobs_by_kind_and_state() {
    let (t, _) = state();
    let app = t.app_as(ALICE);
    let empty = json(send(&app, get(SUMMARY)).await).await;
    assert_schema(&empty, "JobsSummary");
    assert_eq!(
        empty,
        json!({ "queues": [
            { "kind": "test.a", "paused": false, "queued": 0, "running": 0, "succeeded": 0, "failed": 0, "cancelled": 0 },
            { "kind": "test.b", "paused": false, "queued": 0, "running": 0, "succeeded": 0, "failed": 0, "cancelled": 0 },
        ]}),
        "every kind of the server, even without jobs"
    );

    let a = t.enqueue(ALICE, "test.a", mode("ok")).await;
    t.enqueue(ALICE, "test.a", mode("ok")).await;
    set_state(&t, a, "succeeded", None);
    t.enqueue(ALICE, "test.b", mode("ok")).await;
    // A kind this build no longer runs still shows, with its jobs.
    t.control()
        .execute(
            "INSERT INTO jobs (user_id, kind, state, payload_json, max_attempts, run_at, \
             created_at, updated_at, finished_at) \
             VALUES (?1, 'test.old', 'failed', '{}', 1, ?2, ?2, ?2, ?2)",
            params![ALICE, START],
        )
        .unwrap();
    t.state.jobs().pause(ALICE, "test.b").await.unwrap();
    let summary = json(send(&app, get(SUMMARY)).await).await;
    assert_schema(&summary, "JobsSummary");
    assert_eq!(
        summary["queues"],
        json!([
            { "kind": "test.a", "paused": false, "queued": 1, "running": 0, "succeeded": 1, "failed": 0, "cancelled": 0 },
            { "kind": "test.b", "paused": true, "queued": 1, "running": 0, "succeeded": 0, "failed": 0, "cancelled": 0 },
            { "kind": "test.old", "paused": false, "queued": 0, "running": 0, "succeeded": 0, "failed": 1, "cancelled": 0 },
        ])
    );
}

#[tokio::test(start_paused = true)]
async fn cancel_and_retry_answer_with_the_job() {
    let (t, probe) = state();
    let app = t.app_as(ALICE);
    let queued = t.enqueue(ALICE, "test.a", mode("ok")).await;
    let response = send(&app, post(&format!("{JOBS}/{queued}/cancel"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let cancelled = json(response).await;
    assert_schema(&cancelled, "Job");
    assert_eq!(cancelled["state"], "cancelled");
    assert!(cancelled["finishedAt"].is_i64());
    // Again: no change.
    let again = json(send(&app, post(&format!("{JOBS}/{queued}/cancel"))).await).await;
    assert_eq!(again, cancelled);

    let retried = json(send(&app, post(&format!("{JOBS}/{queued}/retry"))).await).await;
    assert_eq!(retried["state"], "queued");
    assert_eq!(retried["finishedAt"], Value::Null);
    let conflict = send(&app, post(&format!("{JOBS}/{queued}/retry"))).await;
    assert_eq!(
        problem(conflict, StatusCode::CONFLICT).await.code,
        ErrorCode::Conflict,
        "a queued job is not retried"
    );

    let failed = t.enqueue(ALICE, "test.a", mode("ok")).await;
    set_state(&t, failed, "failed", Some("unavailable"));
    let retried = json(send(&app, post(&format!("{JOBS}/{failed}/retry"))).await).await;
    assert_eq!(retried["state"], "queued");
    assert_eq!(retried["attempts"], 0, "every try available again");
    assert_eq!(retried["errorCode"], Value::Null);

    // Run them, then a finished job can be neither cancelled nor retried.
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    t.wait_job(ALICE, failed, |job| job.state == JobState::Succeeded)
        .await;
    for action in ["cancel", "retry"] {
        let response = send(&app, post(&format!("{JOBS}/{failed}/{action}"))).await;
        assert_eq!(
            problem(response, StatusCode::CONFLICT).await.code,
            ErrorCode::Conflict,
            "{action}"
        );
    }
    assert_eq!(probe.starts().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn queue_actions_report_what_they_changed() {
    let (t, _) = state();
    let app = t.app_as(ALICE);
    let action = |name: &str| {
        let app = app.clone();
        let uri = format!("/api/v1/queues/test.a/{name}");
        async move {
            let response = send(&app, post(&uri)).await;
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            let result = json(response).await;
            assert_schema(&result, "QueueResult");
            result
        }
    };
    let paused = action("pause").await;
    assert_eq!(paused["affected"], 1);
    assert_eq!(paused["queue"]["paused"], true);
    assert_eq!(action("pause").await["affected"], 0, "already paused");

    for _ in 0..3 {
        t.enqueue(ALICE, "test.a", mode("ok")).await;
    }
    let done = t.enqueue(ALICE, "test.a", mode("ok")).await;
    set_state(&t, done, "succeeded", None);
    t.enqueue(ALICE, "test.b", mode("ok")).await;

    let cancelled = action("cancel-all").await;
    assert_eq!(cancelled["affected"], 3);
    assert_eq!(cancelled["queue"]["cancelled"], 3);
    assert_eq!(cancelled["queue"]["queued"], 0);
    let cleared = action("clear-finished").await;
    assert_eq!(
        cleared["affected"], 4,
        "the cancelled and the succeeded jobs"
    );
    assert_eq!(
        cleared["queue"],
        json!({ "kind": "test.a", "paused": true, "queued": 0, "running": 0, "succeeded": 0, "failed": 0, "cancelled": 0 })
    );
    let resumed = action("resume").await;
    assert_eq!(resumed["affected"], 1);
    assert_eq!(resumed["queue"]["paused"], false);
    let others = json(send(&app, get(&format!("{JOBS}?kind=test.b"))).await).await;
    assert_eq!(
        others["items"][0]["state"], "queued",
        "other kinds untouched"
    );
}

#[tokio::test(start_paused = true)]
async fn a_repeated_idempotency_key_replays_the_first_response() {
    let (t, probe) = state();
    let app = t.app_as(ALICE);
    let job = t.enqueue(ALICE, "test.a", mode("permanent")).await;
    set_state(&t, job, "failed", Some("validation_failed"));
    let retry = format!("{JOBS}/{job}/retry");

    let first = send(&app, post_with_key(&retry, "retry-1")).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert!(first.headers().get(REPLAYED).is_none());
    let first = json(first).await;
    assert_eq!(first["state"], "queued");

    // The job runs and fails again; a repeat of the same click changes
    // nothing and gets the first answer back.
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    t.wait_job(ALICE, job, |job| job.state == JobState::Failed)
        .await;
    let replayed = send(&app, post_with_key(&retry, "retry-1")).await;
    assert_eq!(replayed.status(), StatusCode::OK);
    assert_eq!(replayed.headers()[REPLAYED], "true");
    assert_eq!(replayed.headers()[header::CONTENT_TYPE], "application/json");
    assert_eq!(json(replayed).await, first);
    assert_eq!(
        t.job(ALICE, job).await.state,
        JobState::Failed,
        "not retried twice"
    );
    assert_eq!(probe.starts().len(), 1);

    // A new key retries again; a key is the user's own.
    let second = send(&app, post_with_key(&retry, "retry-2")).await;
    assert_eq!(json(second).await["state"], "queued");
    t.wait_job(ALICE, job, |job| job.state == JobState::Failed)
        .await;
    let bob = t.app_as(BOB);
    let bobs = send(&bob, post_with_key(&retry, "retry-1")).await;
    assert_eq!(
        problem(bobs, StatusCode::NOT_FOUND).await.code,
        ErrorCode::NotFound,
        "Bob's key runs Bob's request"
    );

    // Error answers are replayed too: the 409 of a job that is not failed.
    let queued = t.enqueue(ALICE, "test.b", mode("gate")).await;
    let refused = send(
        &app,
        post_with_key(&format!("{JOBS}/{queued}/retry"), "k-409"),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let again = send(
        &app,
        post_with_key(&format!("{JOBS}/{queued}/retry"), "k-409"),
    )
    .await;
    assert_eq!(again.headers()[REPLAYED], "true");
    assert_eq!(
        problem(again, StatusCode::CONFLICT).await.code,
        ErrorCode::Conflict
    );
}

#[tokio::test(start_paused = true)]
async fn idempotency_keys_are_checked() {
    let (t, _) = state();
    let app = t.app_as(ALICE);
    let job = t.enqueue(ALICE, "test.a", mode("ok")).await;
    let other = t.enqueue(ALICE, "test.a", mode("ok")).await;
    set_state(&t, job, "failed", None);
    set_state(&t, other, "failed", None);
    let retry = |id: i64| format!("{JOBS}/{id}/retry");

    for bad in ["has space", &"k".repeat(256)] {
        let response = send(&app, post_with_key(&retry(job), bad)).await;
        assert_eq!(
            problem(response, StatusCode::BAD_REQUEST).await.code,
            ErrorCode::BadRequest
        );
    }
    assert_eq!(
        send(&app, post_with_key(&retry(job), "k")).await.status(),
        StatusCode::OK
    );
    // The same key for another request.
    let reused = send(&app, post_with_key(&retry(other), "k")).await;
    let reused = problem(reused, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(reused.code, ErrorCode::ValidationFailed);
    assert_eq!(reused.errors[0].field, "Idempotency-Key");
    assert_eq!(t.job(ALICE, other).await.state, JobState::Failed);

    // After 24 hours a key is new again.
    t.control()
        .execute(
            "UPDATE idempotency SET created_at = ?1 WHERE key = 'k'",
            [START - 86_400_001],
        )
        .unwrap();
    let fresh = send(&app, post_with_key(&retry(other), "k")).await;
    assert_eq!(fresh.status(), StatusCode::OK);
    assert!(fresh.headers().get(REPLAYED).is_none());

    // Without a key, nothing is stored.
    set_state(&t, other, "failed", None);
    let plain = send(&app, post(&retry(other))).await;
    assert_eq!(plain.status(), StatusCode::OK);
    let stored: i64 = t
        .control()
        .query_row("SELECT COUNT(*) FROM idempotency", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored, 1, "only k");
}

#[tokio::test(start_paused = true)]
async fn a_key_in_flight_answers_409() {
    let (t, _) = state();
    let app = t.app_as(ALICE);
    let job = t.enqueue(ALICE, "test.a", mode("ok")).await;
    set_state(&t, job, "failed", None);
    let retry = format!("{JOBS}/{job}/retry");
    // A reservation for this very request, as the middleware makes it while
    // the request runs: send it once, then turn its row back into one.
    assert_eq!(
        send(&app, post_with_key(&retry, "k")).await.status(),
        StatusCode::OK
    );
    t.control()
        .execute(
            "UPDATE idempotency SET status = 0, created_at = ?1 WHERE key = 'k'",
            [START],
        )
        .unwrap();
    let busy = send(&app, post_with_key(&retry, "k")).await;
    assert_eq!(
        problem(busy, StatusCode::CONFLICT).await.code,
        ErrorCode::Conflict
    );
}

/// The routes of `IDEMPOTENT_ROUTES` and the operations that document the
/// `Idempotency-Key` header are the same.
#[test]
fn idempotent_routes_document_the_header() {
    let doc = serde_json::to_value(routes::openapi()).unwrap();
    let mut documented = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            let takes_key = operation["parameters"].as_array().is_some_and(|params| {
                params
                    .iter()
                    .any(|p| p["name"] == "Idempotency-Key" && p["in"] == "header")
            });
            if takes_key {
                documented.insert((method.clone(), path.clone()));
            }
        }
    }
    let declared: BTreeSet<(String, String)> = IDEMPOTENT_ROUTES
        .iter()
        .map(|route| {
            (
                route.method.as_str().to_ascii_lowercase(),
                route.path.to_owned(),
            )
        })
        .collect();
    assert_eq!(documented, declared);
    assert!(declared.contains(&(
        Method::POST.as_str().to_ascii_lowercase(),
        "/api/v1/jobs/{id}/retry".to_owned()
    )));
}
