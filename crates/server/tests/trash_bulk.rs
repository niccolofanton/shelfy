//! Bulk actions by selector and the trash (P1-11) through the real
//! middleware stack:
//!
//! - `POST /posts/bulk` inline, with its events, and as a `bulk` job: select
//!   all minus exceptions on a 20k synthetic library, with `job.updated`
//!   progress;
//! - `GET /trash`, `POST /trash/restore` (by key, by filter, by a delete's
//!   stamp), and the restore round trip that leaves both search indexes and
//!   the folders identical, inline and through jobs;
//! - `POST /trash/empty` and the `purge` job (up to the request, idempotent),
//!   and the nightly 30-day retention on a test clock;
//! - `DELETE /collections/{id}?mode=withPosts` (UI-17);
//! - the `Idempotency-Key` replay, the 423 of a locked library, the problems
//!   of bad requests, and the authz rules every new route ships (P1 lane
//!   rule 4: 401 without a session, 404 for another user's resource, API
//!   tokens refused).
//!
//! Job tests run on real time: a chunk's work on the blocking pool would let
//! paused time run past a lease. The retention test runs on paused time
//! (its chunks are tiny and its lease an hour).

mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use shelfy_core::repo::posts::{self, NewPost, UserContentPatch};
use shelfy_core::repo::{Platform, collections};
use shelfy_core::search::index;
use shelfy_server::config::{Config, DataDir};
use shelfy_server::control::jobs::JobRow;
use shelfy_server::error::ErrorCode;
use shelfy_server::events::Delivery;
use shelfy_server::events::model::{EventTopic, JobState};
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::jobs::idempotency::{IDEMPOTENCY_KEY, REPLAYED};
use shelfy_server::jobs::{Scheduler, kinds};
use shelfy_server::limits::RouteLimits;
use shelfy_server::tokens::{SecretToken, hash_token};
use support::auth::{owner, sign_in, spa, with_session};
use support::jobs::START;
use support::library::{
    ALICE, BOB, DAY, FIXTURE_TRASHED, Fixture, NOW, bob_library, fixture, synthetic_library,
};
use support::sse::{Stream, assert_event_schema, assert_schema};
use support::{TestState, from_app, get, json, post_json, problem, send};
use tokio_util::sync::CancellationToken;

// ── Requests ─────────────────────────────────────────────────────────────────

fn post(uri: &str, value: &Value) -> Request<Body> {
    post_json(uri, value.to_string())
}

fn post_empty(uri: &str) -> Request<Body> {
    from_app(Request::post(uri).body(Body::empty()).unwrap())
}

fn delete(uri: &str) -> Request<Body> {
    from_app(Request::delete(uri).body(Body::empty()).unwrap())
}

fn with_key(mut request: Request<Body>, key: &str) -> Request<Body> {
    request
        .headers_mut()
        .insert(IDEMPOTENCY_KEY, key.parse().unwrap());
    request
}

/// `POST /posts/bulk` with `selector`, `action` and, when not null, `params`.
fn bulk(selector: Value, action: &str, params: Value) -> Request<Body> {
    let mut body = json!({ "selector": selector, "action": action });
    if !params.is_null() {
        body["params"] = params;
    }
    post("/api/v1/posts/bulk", &body)
}

fn restore(body: Value) -> Request<Body> {
    post("/api/v1/trash/restore", &body)
}

/// Sends `request` and expects `status` with a JSON body.
async fn call(app: &Router, request: Request<Body>, status: StatusCode) -> Value {
    let route = format!("{} {}", request.method(), request.uri());
    let response = send(app, request).await;
    assert_eq!(response.status(), status, "{route}");
    json(response).await
}

async fn ok(app: &Router, request: Request<Body>) -> Value {
    call(app, request, StatusCode::OK).await
}

/// Sends `request` and expects a 422 `validation_failed` naming `field`.
async fn invalid(app: &Router, request: Request<Body>, field: &str) {
    let route = format!("{} {}", request.method(), request.uri());
    let refused = problem(send(app, request).await, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(refused.code, ErrorCode::ValidationFailed, "{route}");
    assert_eq!(refused.errors[0].field, field, "{route}");
}

async fn not_found(app: &Router, request: Request<Body>) {
    let route = format!("{} {}", request.method(), request.uri());
    let missing = problem(send(app, request).await, StatusCode::NOT_FOUND).await;
    assert_eq!(missing.code, ErrorCode::NotFound, "{route}");
}

fn keys(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|p| p["key"].as_str().expect("key").to_owned())
        .collect()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

// ── State ────────────────────────────────────────────────────────────────────

/// Alice's fixture library and Bob's library on one server; both users are
/// in the control database, so their jobs can be stored.
async fn two_libraries() -> (TestState, Fixture) {
    let t = TestState::new();
    t.add_user(ALICE);
    t.add_user(BOB);
    let ids = t.write(ALICE, |tx| fixture(tx)).await;
    t.write(BOB, |tx| bob_library(tx)).await;
    (t, ids)
}

/// Runs the job scheduler of `t` (with the server's kinds) until dropped.
fn scheduler(t: &TestState) -> Scheduler {
    t.state
        .jobs()
        .start(t.state.clone(), CancellationToken::new())
}

/// A connection of its own to `user`'s library (checks build temporary
/// tables).
fn library(t: &TestState, user: &str) -> Connection {
    Connection::open(t.state.user_dbs().library_path(user).unwrap()).unwrap()
}

fn assert_index_consistent(t: &TestState, user: &str, after: &str) {
    assert_eq!(
        index::verify(&library(t, user)).unwrap(),
        Vec::<i64>::new(),
        "the index differs from the posts after {after}"
    );
}

/// What a restore must bring back as it was: both search indexes token by
/// token, the folder memberships, the tag rows, and the post columns but
/// `updated_at`.
#[derive(Debug, PartialEq)]
struct Snapshot {
    fts: Vec<(String, i64, String, i64)>,
    infix: Vec<(String, i64, String, i64)>,
    memberships: Vec<(i64, i64, i64)>,
    tags: Vec<(i64, String, String)>,
    posts: Vec<(i64, String, Option<String>, Option<i64>)>,
}

fn snapshot(t: &TestState, user: &str) -> Snapshot {
    let conn = library(t, user);
    let vocab = |table: &str| -> Vec<(String, i64, String, i64)> {
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS temp.vocab_{table}
             USING fts5vocab(main, {table}, instance);"
        ))
        .unwrap();
        conn.prepare(&format!(
            "SELECT term, doc, col, offset FROM temp.vocab_{table} ORDER BY term, doc, col, offset"
        ))
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    };
    Snapshot {
        fts: vocab("posts_fts"),
        infix: vocab("posts_infix"),
        memberships: conn
            .prepare("SELECT post_id, collection_id, added_at FROM post_collections ORDER BY 1, 2")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap(),
        tags: conn
            .prepare("SELECT post_id, tag_norm, source FROM post_tags ORDER BY 1, 2, 3")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap(),
        posts: conn
            .prepare("SELECT id, key, user_note, deleted_at FROM posts ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap(),
    }
}

/// The trashed posts of `user` and their stamps.
fn trash_stamps(t: &TestState, user: &str) -> Vec<(String, i64)> {
    library(t, user)
        .prepare("SELECT key, deleted_at FROM posts WHERE deleted_at IS NOT NULL ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Jobs of `kind` of `user` in `state`.
fn jobs_in(t: &TestState, user: &str, kind: &str, state: &str) -> i64 {
    t.control()
        .query_row(
            "SELECT count(*) FROM jobs WHERE user_id = ?1 AND kind = ?2 AND state = ?3",
            params![user, kind, state],
            |r| r.get(0),
        )
        .unwrap()
}

/// Waits until `user` has `n` succeeded jobs of `kind`, following the
/// user's events.
async fn wait_succeeded(t: &TestState, user: &str, kind: &str, n: i64) {
    let mut events = t.state.events().subscribe(user, None);
    loop {
        if jobs_in(t, user, kind, "succeeded") >= n {
            return;
        }
        let next = tokio::time::timeout(Duration::from_secs(3 * 86_400), events.next()).await;
        assert!(next.is_ok(), "no {n} succeeded {kind} jobs");
    }
}

/// A write transaction of the test's own on a library: the server's writer
/// waits for it (up to its 5-second busy timeout) until it is released, so a
/// test can hold a job inside a chunk.
struct HeldWriter(Connection);

impl HeldWriter {
    fn take(t: &TestState, user: &str) -> Self {
        let conn = library(t, user);
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        Self(conn)
    }

    fn release(self) {
        self.0.execute_batch("ROLLBACK").unwrap();
    }
}

/// Sends `request`, which starts a bulk job (202), and holds the library's
/// writer before the job starts: its first chunk waits for the returned
/// [`HeldWriter`]. Returns the answer, the job's id and the held writer.
async fn start_held(
    t: &TestState,
    app: &Router,
    request: Request<Body>,
) -> (Value, i64, HeldWriter) {
    ok(app, post_empty("/api/v1/queues/bulk/pause")).await;
    let started = call(app, request, StatusCode::ACCEPTED).await;
    let id = started["job"]["id"].as_i64().unwrap();
    let held = HeldWriter::take(t, ALICE);
    ok(app, post_empty("/api/v1/queues/bulk/resume")).await;
    (started, id, held)
}

/// Polls job `id` of `user` until `done` holds; returns it.
async fn poll_job(
    state: &shelfy_server::state::AppState,
    user: &str,
    id: i64,
    done: impl Fn(&JobRow) -> bool,
) -> JobRow {
    for _ in 0..6_000 {
        let job = state.jobs().get(user, id).await.unwrap().expect("the job");
        if done(&job) {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job {id} never got there");
}

/// Waits until job `id` of `user` runs its first chunk: it reported its
/// stage, and the worker has had time to reach the library's writer.
async fn wait_in_first_chunk(t: &TestState, user: &str, id: i64, stage: &str) {
    poll_job(&t.state, user, id, |job| {
        job.state == JobState::Running && job.stage.as_deref() == Some(stage)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// The checkpoint of a bulk job's payload: the largest id it went through,
/// and how many posts it went through.
fn checkpoint(job: &JobRow) -> (i64, u64) {
    let payload: Value = serde_json::from_str(&job.payload_json).unwrap();
    (
        payload["after"].as_i64().unwrap(),
        payload["done"].as_u64().unwrap(),
    )
}

/// A `job.updated` of one job, as the stream carried it.
#[derive(Clone, Debug, PartialEq)]
struct Update {
    state: String,
    progress: Option<f64>,
    stage: Option<String>,
}

/// Follows `user`'s events until job `id` succeeds; returns its updates and
/// the `posts.changed` events seen meanwhile.
async fn follow_job(
    events: &mut shelfy_server::events::Subscription,
    id: i64,
) -> (Vec<Update>, Vec<Value>) {
    let mut updates = Vec::new();
    let mut changes = Vec::new();
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(300), events.next())
            .await
            .unwrap_or_else(|_| panic!("job {id} never succeeded: {updates:?}"));
        let Delivery::Event(event) = delivery else {
            panic!("events were lost");
        };
        let data: Value = serde_json::from_str(&event.data).unwrap();
        match event.topic {
            EventTopic::JobUpdated if data["id"] == id => {
                let update = Update {
                    state: data["state"].as_str().unwrap().to_owned(),
                    progress: data["progress"].as_f64(),
                    stage: data["stage"].as_str().map(str::to_owned),
                };
                assert_ne!(update.state, "failed", "{data}");
                let done = update.state == "succeeded";
                updates.push(update);
                if done {
                    return (updates, changes);
                }
            }
            EventTopic::PostsChanged => changes.push(data),
            _ => {}
        }
    }
}

/// An API token of `user_id` with every scope.
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

// ── Authz (lane rule 4) ──────────────────────────────────────────────────────

/// One request of every new route, in an order where each succeeds for the
/// owner of the fixture library and `collection`.
fn every_route(collection: i64) -> Vec<Request<Body>> {
    vec![
        bulk(json!({ "keys": ["x_2001"] }), "delete", Value::Null),
        get("/api/v1/trash"),
        restore(json!({ "selector": { "keys": ["x_2001"] } })),
        post_empty("/api/v1/trash/empty"),
        delete(&format!("/api/v1/collections/{collection}?mode=withPosts")),
    ]
}

#[tokio::test]
async fn every_new_route_needs_a_session() {
    let (t, ids) = two_libraries().await;
    let app = t.app();
    for request in every_route(ids.lighting) {
        let route = format!("{} {}", request.method(), request.uri());
        let refused = problem(send(&app, request).await, StatusCode::UNAUTHORIZED).await;
        assert_eq!(refused.code, ErrorCode::Unauthorized, "{route}");
    }
    // Nothing changed.
    assert_eq!(trash_stamps(&t, ALICE), [(FIXTURE_TRASHED.to_owned(), NOW)]);
    assert_eq!(jobs_in(&t, ALICE, "purge", "queued"), 0);
}

/// Existing device scopes cannot access trash or bulk writes, even beside
/// a valid session cookie. Cookie requests still use the CSRF guard.
#[tokio::test]
async fn device_tokens_lack_library_scopes() {
    let t = TestState::new();
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let token = api_token(&t, &owner_id);
    let ids = t.write(&owner_id, |tx| fixture(tx)).await;

    for with_cookie in [false, true] {
        for request in every_route(ids.lighting) {
            let route = format!("{} {}", request.method(), request.uri());
            let request = if with_cookie {
                with_session(request, &cookie)
            } else {
                request
            };
            let refused = problem(
                send(&app, bearer(request, &token)).await,
                StatusCode::FORBIDDEN,
            )
            .await;
            assert_eq!(refused.code, ErrorCode::Forbidden, "{route}");
        }
    }
    assert_eq!(trash_stamps(&t, &owner_id).len(), 1, "nothing changed");

    for request in every_route(ids.lighting) {
        let route = format!("{} {}", request.method(), request.uri());
        let response = send(&app, spa(&t, request, &cookie)).await;
        assert!(
            response.status().is_success(),
            "{route}: {}",
            response.status()
        );
    }
    // Every unsafe route refuses a cookie request without the web app's
    // headers (CSRF), and changes nothing.
    let other = call(
        &app,
        spa(
            &t,
            post("/api/v1/collections", &json!({ "name": "other" })),
            &cookie,
        ),
        StatusCode::CREATED,
    )
    .await;
    for request in [
        bulk(json!({ "keys": ["x_2002"] }), "delete", Value::Null),
        restore(json!({ "selector": { "keys": ["x_2001"] } })),
        post_empty("/api/v1/trash/empty"),
        delete(&format!(
            "/api/v1/collections/{}?mode=withPosts",
            other["id"]
        )),
    ] {
        let route = format!("{} {}", request.method(), request.uri());
        let mut forged = with_session(request, &cookie);
        forged.headers_mut().remove("x-shelfy-client");
        let refused = problem(send(&app, forged).await, StatusCode::FORBIDDEN).await;
        assert_eq!(refused.code, ErrorCode::CsrfFailed, "{route}");
    }
    let folders = ok(&app, spa(&t, get("/api/v1/collections"), &cookie)).await;
    assert!(
        folders["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["id"] == other["id"]),
        "the folder is still there"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_users_posts_and_collections_are_out_of_reach() {
    let (t, _) = two_libraries().await;
    // Bob's third collection has an id Alice's library does not have.
    let bob_collection = t
        .write(BOB, |tx| {
            for name in ["b2", "b3"] {
                collections::create(
                    tx,
                    &collections::NewCollection {
                        name: name.into(),
                        ..Default::default()
                    },
                    NOW,
                )?;
            }
            Ok(collections::list(tx)?.last().unwrap().id)
        })
        .await;
    let bob = t.app_as(BOB);
    ok(
        &bob,
        bulk(json!({ "keys": ["x_9002"] }), "delete", Value::Null),
    )
    .await;
    let alice = t.app_as(ALICE);

    // Bob's keys are unknown keys to Alice.
    let none = ok(
        &alice,
        bulk(
            json!({ "keys": ["ig_9001", "x_9002"] }),
            "delete",
            Value::Null,
        ),
    )
    .await;
    assert_eq!(
        (none["selected"].clone(), none["changed"].clone()),
        (json!(0), json!(0))
    );
    let none = ok(
        &alice,
        restore(json!({ "selector": { "keys": ["x_9002"] } })),
    )
    .await;
    assert_eq!(none["changed"], 0);
    // Bob's collection is a 404.
    not_found(
        &alice,
        bulk(
            json!({ "keys": ["ig_1001"] }),
            "addToCollections",
            json!({ "collectionIds": [bob_collection] }),
        ),
    )
    .await;
    not_found(
        &alice,
        bulk(
            json!({ "filter": {} }),
            "removeFromCollection",
            json!({ "collectionId": bob_collection }),
        ),
    )
    .await;
    not_found(
        &alice,
        delete(&format!(
            "/api/v1/collections/{bob_collection}?mode=withPosts"
        )),
    )
    .await;
    // Alice's trash is hers alone, and emptying it leaves Bob's.
    let trash = ok(&alice, get("/api/v1/trash")).await;
    assert_eq!(keys(&trash), [FIXTURE_TRASHED]);
    let _scheduler = scheduler(&t);
    let emptying = call(
        &alice,
        post_empty("/api/v1/trash/empty"),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(emptying["selected"], 1);
    wait_succeeded(&t, ALICE, "purge", 1).await;
    assert!(trash_stamps(&t, ALICE).is_empty());
    let bobs = ok(&bob, get("/api/v1/trash")).await;
    assert_eq!(keys(&bobs), ["x_9002"]);
    let folders = ok(&bob, get("/api/v1/collections")).await;
    assert_eq!(folders["items"].as_array().unwrap().len(), 3);
    assert_eq!(
        ok(&bob, get("/api/v1/posts/ig_9001")).await["deletedAt"],
        Value::Null
    );
}

// ── Inline ───────────────────────────────────────────────────────────────────

/// The next event of `stream`, past heartbeats.
async fn next_event(stream: &mut Stream) -> (String, Value) {
    loop {
        let frame = stream.next().await;
        if frame.is_heartbeat() {
            continue;
        }
        assert_event_schema(&frame);
        return (frame.name().to_owned(), frame.json());
    }
}

/// The `posts.changed` and `stats.changed` of one write, in any order;
/// returns the reason and the sorted keys of the former.
async fn announced(stream: &mut Stream) -> (String, Option<Vec<String>>) {
    let mut posts = None;
    let mut stats = false;
    while posts.is_none() || !stats {
        match next_event(stream).await {
            (name, data) if name == "posts.changed" && posts.is_none() => posts = Some(data),
            (name, _) if name == "stats.changed" && !stats => stats = true,
            other => panic!("unexpected event {other:?}"),
        }
    }
    let event = posts.unwrap();
    let keys = event["keys"].as_array().map(|keys| {
        let mut keys: Vec<String> = keys
            .iter()
            .map(|k| k.as_str().unwrap().to_owned())
            .collect();
        keys.sort();
        keys
    });
    (event["reason"].as_str().unwrap().to_owned(), keys)
}

/// Review L1 and L2: every delete gets a stamp of its own, so an undo
/// restores its own posts and no other delete's: ten deletes at once (most
/// in the same millisecond), a job's stamp reserved at its request beside an
/// inline delete, a delete after an undo. An inline delete that moves
/// nothing answers no stamp.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_delete_gets_a_stamp_of_its_own() {
    let (t, _) = two_libraries().await;
    let keys = t.write(ALICE, |tx| synthetic_library(tx, 700, 56)).await;
    let app = t.app_as(ALICE);
    let delete = |keys: &[String]| bulk(json!({ "keys": keys }), "delete", Value::Null);

    let at_once: Vec<_> = keys[..10]
        .iter()
        .map(|key| {
            let (app, request) = (app.clone(), delete(std::slice::from_ref(key)));
            tokio::spawn(async move { ok(&app, request).await["deletedAt"].as_i64().unwrap() })
        })
        .collect();
    let mut stamps = Vec::new();
    for task in at_once {
        stamps.push(task.await.unwrap());
    }
    let distinct: BTreeSet<i64> = stamps.iter().copied().collect();
    assert_eq!(distinct.len(), 10, "{stamps:?}");
    let undone = ok(&app, restore(json!({ "deletedAt": stamps[3] }))).await;
    assert_eq!(undone["changed"], 1);
    let back = ok(&app, get(&format!("/api/v1/posts/{}", keys[3]))).await;
    assert_eq!(back["deletedAt"], Value::Null);
    assert_eq!(trash_stamps(&t, ALICE).len(), 1 + 9);

    // A delete right after that undo: a stamp no delete had.
    let next = ok(&app, delete(&keys[3..4])).await["deletedAt"]
        .as_i64()
        .unwrap();
    assert!(next > *distinct.last().unwrap(), "{next} {stamps:?}");

    // A job's stamp is reserved at its request (no scheduler runs here, so
    // the job waits); an inline delete right after gets another one.
    let job = call(
        &app,
        bulk(json!({ "filter": {} }), "delete", Value::Null),
        StatusCode::ACCEPTED,
    )
    .await;
    let reserved = job["deletedAt"].as_i64().unwrap();
    assert!(reserved > next);
    let inline = ok(&app, delete(&keys[690..692])).await["deletedAt"]
        .as_i64()
        .unwrap();
    assert!(inline > reserved);
    let undone = ok(&app, restore(json!({ "deletedAt": inline }))).await;
    assert_eq!(undone["changed"], 2);

    // Nothing moved, no stamp.
    let none = ok(&app, delete(&[FIXTURE_TRASHED.to_owned(), keys[0].clone()])).await;
    assert_eq!(
        (none["changed"].clone(), none["deletedAt"].clone()),
        (json!(0), Value::Null)
    );
}

#[tokio::test(start_paused = true)]
async fn small_selections_run_inline_and_are_announced() {
    let (t, ids) = two_libraries().await;
    let app = t.app_as(ALICE);
    let mut stream = Stream::connect(&app, "/api/v1/events", &[]).await;
    stream.hello().await;
    let pause = || tokio::time::advance(Duration::from_secs(3));
    let reason_keys = |reason: &str, keys: &[&str]| (reason.to_owned(), Some(strings(keys)));

    // Delete by key: the trashed post given is selected but not changed.
    let before = now_ms();
    let deleted = ok(
        &app,
        bulk(
            json!({ "keys": ["x_2001", "x_2002", FIXTURE_TRASHED, "ig_missing"] }),
            "delete",
            Value::Null,
        ),
    )
    .await;
    assert_schema(&deleted, "BulkResult");
    assert_eq!(deleted["action"], "delete");
    assert_eq!(deleted["selected"], 3);
    assert_eq!(deleted["changed"], 2);
    assert_eq!(deleted["job"], Value::Null);
    let stamp = deleted["deletedAt"].as_i64().unwrap();
    assert!(stamp >= before);
    assert_eq!(
        announced(&mut stream).await,
        reason_keys("delete", &["x_2001", "x_2002"])
    );
    assert_eq!(
        trash_stamps(&t, ALICE),
        [
            (FIXTURE_TRASHED.to_owned(), NOW),
            ("x_2001".to_owned(), stamp),
            ("x_2002".to_owned(), stamp)
        ]
    );
    assert_eq!(ok(&app, get("/api/v1/stats")).await["trashed"], 3);

    // The trash: the latest delete first, then by id.
    let trash = ok(&app, get("/api/v1/trash")).await;
    assert_schema(&trash, "TrashPage");
    assert_eq!(keys(&trash), ["x_2002", "x_2001", FIXTURE_TRASHED]);
    assert_eq!(trash["total"], 3);
    assert_eq!(trash["retentionDays"], 30);
    assert_eq!(trash["items"][0]["deletedAt"], stamp);
    assert_eq!(trash["nextCursor"], Value::Null);
    let first = ok(&app, get("/api/v1/trash?limit=2")).await;
    assert_eq!(keys(&first), ["x_2002", "x_2001"]);
    let cursor = first["nextCursor"].as_str().unwrap();
    let rest = ok(&app, get(&format!("/api/v1/trash?limit=2&cursor={cursor}"))).await;
    assert_eq!(keys(&rest), [FIXTURE_TRASHED]);
    assert_eq!(rest["nextCursor"], Value::Null);
    pause().await;

    // Adding to a collection skips the trash, and reaches it only by filter
    // with `trash`.
    let added = ok(
        &app,
        bulk(
            json!({ "filter": { "platform": "twitter", "trash": true } }),
            "addToCollections",
            json!({ "collectionIds": [ids.inspiration] }),
        ),
    )
    .await;
    assert_eq!(
        (added["selected"].clone(), added["changed"].clone()),
        (json!(2), json!(0))
    );
    let added = ok(
        &app,
        bulk(
            json!({ "filter": { "platform": "instagram" } }),
            "addToCollections",
            json!({ "collectionIds": [ids.inspiration, ids.lighting] }),
        ),
    )
    .await;
    assert_eq!(
        (added["selected"].clone(), added["changed"].clone()),
        (json!(2), json!(2))
    );
    assert_eq!(
        announced(&mut stream).await,
        reason_keys("edit", &["ig_1001", "ig_1002"])
    );
    pause().await;
    let removed = ok(
        &app,
        bulk(
            json!({ "keys": ["ig_1001", "pin_3001", "x_2001"] }),
            "removeFromCollection",
            json!({ "collectionId": ids.inspiration }),
        ),
    )
    .await;
    assert_eq!(removed["changed"], 2);
    assert_eq!(
        announced(&mut stream).await,
        reason_keys("edit", &["ig_1001", "pin_3001"])
    );
    let shown = ok(&app, get("/api/v1/posts/ig_1001")).await;
    assert_eq!(shown["collectionIds"], json!([ids.lighting]));
    pause().await;

    // Clearing AI fields: by filter, the trash is out of reach; by key it is
    // not.
    let cleared = ok(
        &app,
        bulk(json!({ "filter": {} }), "clearAiDescription", Value::Null),
    )
    .await;
    assert_eq!(cleared["changed"], 1);
    assert_eq!(
        announced(&mut stream).await,
        reason_keys("ai", &["ig_1001"])
    );
    let shown = ok(&app, get("/api/v1/posts/ig_1001")).await;
    assert_eq!(shown["aiDescription"], Value::Null);
    assert_eq!(shown["aiStatus"], Value::Null);
    assert_eq!(shown["aiTags"], json!(["glass", "lamp"]));
    pause().await;
    let cleared = ok(
        &app,
        bulk(json!({ "keys": ["x_2002"] }), "clearAiTags", Value::Null),
    )
    .await;
    assert_eq!(cleared["changed"], 1);
    assert_eq!(announced(&mut stream).await, reason_keys("ai", &["x_2002"]));
    pause().await;

    // The undo of the delete, by its stamp.
    let back = ok(&app, restore(json!({ "deletedAt": stamp }))).await;
    assert_schema(&back, "BulkResult");
    assert_eq!(back["action"], "restore");
    assert_eq!(
        (back["selected"].clone(), back["changed"].clone()),
        (json!(2), json!(2))
    );
    assert_eq!(back["deletedAt"], Value::Null);
    assert_eq!(
        announced(&mut stream).await,
        reason_keys("delete", &["x_2001", "x_2002"])
    );
    assert_eq!(trash_stamps(&t, ALICE), [(FIXTURE_TRASHED.to_owned(), NOW)]);
    pause().await;
    // By filter over the trash.
    let back = ok(
        &app,
        restore(json!({ "selector": { "filter": { "trash": true, "platform": "instagram" } } })),
    )
    .await;
    assert_eq!(back["changed"], 1);
    assert_eq!(
        announced(&mut stream).await,
        reason_keys("delete", &[FIXTURE_TRASHED])
    );
    assert!(trash_stamps(&t, ALICE).is_empty());
    pause().await;
    // A restore of nothing announces nothing: the next event is a real change.
    ok(&app, restore(json!({ "selector": { "keys": ["x_2001"] } }))).await;
    ok(
        &app,
        bulk(json!({ "keys": ["pin_3001"] }), "delete", Value::Null),
    )
    .await;
    assert_eq!(
        announced(&mut stream).await,
        reason_keys("delete", &["pin_3001"])
    );
    assert_index_consistent(&t, ALICE, "inline bulk actions");
}

/// A `POST /posts/bulk` delete by filter with `exceptKeys`, whose body is
/// exactly `size` bytes.
fn delete_body_of(size: usize) -> String {
    let body = |keys: &[String]| {
        json!({ "selector": { "filter": {}, "exceptKeys": keys }, "action": "delete" }).to_string()
    };
    let mut keys: Vec<String> = Vec::new();
    while body(&keys).len() + 203 < size {
        keys.push("k".repeat(200));
    }
    let rest = size.saturating_sub(body(&keys).len() + 3);
    keys.push("k".repeat(rest));
    let text = body(&keys);
    assert_eq!(text.len(), size);
    text
}

/// Selectors that would widen a write or that cannot run (P1-11 review):
/// a null or blank filter member, a key longer than any key, more than
/// 1,000 exceptions, and a selection too large to store as a job, each a
/// 422 naming the member; nothing changes.
#[tokio::test]
async fn selectors_that_would_widen_or_overflow_are_refused() {
    let (t, _) = two_libraries().await;
    t.write(ALICE, |tx| synthetic_library(tx, 600, 57)).await;
    let app = t.app_as(ALICE);
    for (selector, field) in [
        (
            json!({ "filter": { "collection": null } }),
            "selector.filter.collection",
        ),
        (json!({ "filter": { "tag": "" } }), "selector.filter.tag"),
        (json!({ "filter": { "q": " " } }), "selector.filter.q"),
        (json!({ "filter": { "tags": [] } }), "selector.filter.tags"),
        (json!({ "keys": ["k".repeat(201)] }), "selector.keys"),
        (
            json!({ "filter": {}, "exceptKeys": vec!["ig_1"; 1_001] }),
            "selector.exceptKeys",
        ),
    ] {
        invalid(&app, bulk(selector, "delete", Value::Null), field).await;
    }
    invalid(
        &app,
        restore(json!({ "selector": { "filter": { "trash": true, "platform": null } } })),
        "selector.filter.platform",
    )
    .await;

    // A body the route takes, whose job would be over the job system's
    // 64 KiB: the 422 names the selector, not the job's payload.
    let largest = delete_body_of(RouteLimits::STANDARD.body_bytes);
    invalid(&app, post_json("/api/v1/posts/bulk", largest), "selector").await;
    assert_eq!(jobs_in(&t, ALICE, "bulk", "queued"), 0);
    assert_eq!(trash_stamps(&t, ALICE).len(), 1, "nothing changed");
    // A large one that fits runs as a job.
    let large = delete_body_of(60_000);
    call(
        &app,
        post_json("/api/v1/posts/bulk", large),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(jobs_in(&t, ALICE, "bulk", "queued"), 1);
}

#[tokio::test]
async fn bad_bulk_and_trash_requests_are_problems() {
    let (t, ids) = two_libraries().await;
    let app = t.app_as(ALICE);
    let keys_selector = json!({ "keys": ["x_2001"] });

    // Later phases.
    for action in ["analyze", "fetchMedia", "removeStoredMedia"] {
        let refused = problem(
            send(&app, bulk(keys_selector.clone(), action, Value::Null)).await,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
        assert_eq!(refused.code, ErrorCode::NotAvailable, "{action}");
    }
    // Parameters.
    invalid(
        &app,
        bulk(keys_selector.clone(), "addToCollections", Value::Null),
        "params.collectionIds",
    )
    .await;
    invalid(
        &app,
        bulk(
            keys_selector.clone(),
            "addToCollections",
            json!({ "collectionIds": [] }),
        ),
        "params.collectionIds",
    )
    .await;
    invalid(
        &app,
        bulk(
            keys_selector.clone(),
            "addToCollections",
            json!({ "collectionIds": [ids.lighting, ids.lighting] }),
        ),
        "params.collectionIds",
    )
    .await;
    invalid(
        &app,
        bulk(keys_selector.clone(), "removeFromCollection", json!({})),
        "params.collectionId",
    )
    .await;
    invalid(
        &app,
        bulk(
            keys_selector.clone(),
            "delete",
            json!({ "collectionId": ids.lighting }),
        ),
        "params.collectionId",
    )
    .await;
    not_found(
        &app,
        bulk(
            keys_selector.clone(),
            "addToCollections",
            json!({ "collectionIds": [99] }),
        ),
    )
    .await;
    // Shapes serde refuses: an unknown action, parameter or member.
    for body in [
        json!({ "selector": keys_selector, "action": "explode" }),
        json!({ "selector": keys_selector, "action": "delete", "params": { "folder": 1 } }),
        json!({ "selector": keys_selector, "action": "delete", "extra": true }),
        json!({ "action": "delete" }),
    ] {
        let refused = problem(
            send(&app, post("/api/v1/posts/bulk", &body)).await,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
        assert_eq!(refused.code, ErrorCode::ValidationFailed, "{body}");
    }
    // Selectors (review M1: a misspelled filter must not select everything).
    invalid(
        &app,
        bulk(
            json!({ "filter": { "colection": 1 } }),
            "delete",
            Value::Null,
        ),
        "selector.filter.colection",
    )
    .await;
    invalid(
        &app,
        bulk(json!({ "keys": [], "filter": {} }), "delete", Value::Null),
        "selector",
    )
    .await;
    invalid(
        &app,
        bulk(json!({ "keys": vec!["k"; 501] }), "delete", Value::Null),
        "selector.keys",
    )
    .await;
    // Restores.
    for body in [
        json!({}),
        json!({ "selector": keys_selector, "deletedAt": 5 }),
    ] {
        invalid(&app, restore(body), "selector").await;
    }
    for filter in [json!({}), json!({ "trash": false })] {
        invalid(
            &app,
            restore(json!({ "selector": { "filter": filter } })),
            "selector.filter.trash",
        )
        .await;
    }
    // The trash list's cursor, and the delete mode.
    let refused = problem(
        send(&app, get("/api/v1/trash?cursor=not-a-cursor")).await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(refused.code, ErrorCode::InvalidCursor);
    let refused = problem(
        send(
            &app,
            delete(&format!("/api/v1/collections/{}?mode=bogus", ids.lighting)),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(refused.code, ErrorCode::BadRequest);
    // Nothing changed.
    assert_eq!(trash_stamps(&t, ALICE), [(FIXTURE_TRASHED.to_owned(), NOW)]);
    let folders = ok(&app, get("/api/v1/collections")).await;
    assert_eq!(folders["items"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn deleting_a_collection_with_its_posts_moves_them_to_the_trash() {
    let (t, ids) = two_libraries().await;
    // ig_1002 is in both folders.
    t.write(ALICE, |tx| {
        let id = posts::id_for_key(tx, "ig_1002")?.unwrap();
        collections::add_posts(tx, &[id], &[ids.lighting, ids.inspiration], NOW)
    })
    .await;
    let app = t.app_as(ALICE);
    let before = now_ms();
    let deleted = ok(
        &app,
        delete(&format!(
            "/api/v1/collections/{}?mode=withPosts",
            ids.lighting
        )),
    )
    .await;
    assert_schema(&deleted, "CollectionDeleted");
    assert_eq!(deleted["trashed"], 2);
    let stamp = deleted["deletedAt"].as_i64().unwrap();
    assert!(stamp >= before);
    let folders = ok(&app, get("/api/v1/collections")).await;
    assert_eq!(folders["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        folders["items"][0]["count"], 1,
        "trashed members do not count"
    );
    let trash = ok(&app, get("/api/v1/trash")).await;
    assert_eq!(keys(&trash), ["ig_1002", "ig_1001", FIXTURE_TRASHED]);
    assert_index_consistent(&t, ALICE, "a delete with posts");

    // The undo brings the posts back, in the folders that still exist.
    let back = ok(&app, restore(json!({ "deletedAt": stamp }))).await;
    assert_eq!(back["changed"], 2);
    let shown = ok(&app, get("/api/v1/posts/ig_1002")).await;
    assert_eq!(shown["collectionIds"], json!([ids.inspiration]));
    let shown = ok(&app, get("/api/v1/posts/ig_1001")).await;
    assert_eq!(shown["collectionIds"], json!([]));
    let folders = ok(&app, get("/api/v1/collections")).await;
    assert_eq!(folders["items"][0]["count"], 2);

    // The label only: the posts stay; an empty folder moves nothing.
    let deleted = ok(
        &app,
        delete(&format!("/api/v1/collections/{}", ids.inspiration)),
    )
    .await;
    assert_eq!(deleted, json!({ "trashed": 0, "deletedAt": null }));
    let empty = call(
        &app,
        post("/api/v1/collections", &json!({ "name": "empty" })),
        StatusCode::CREATED,
    )
    .await;
    let deleted = ok(
        &app,
        delete(&format!(
            "/api/v1/collections/{}?mode=withPosts",
            empty["id"]
        )),
    )
    .await;
    assert_eq!(deleted, json!({ "trashed": 0, "deletedAt": null }));
    assert_eq!(trash_stamps(&t, ALICE).len(), 1);
    assert_index_consistent(&t, ALICE, "a restore");
}

/// While a library is locked for maintenance (P1-12) the new routes answer
/// 423 `user_locked` and change nothing.
#[tokio::test]
async fn a_locked_library_answers_423() {
    let (t, ids) = two_libraries().await;
    let app = t.app_as(ALICE);
    let users = t.data_dir().users_dir();
    assert!(shelfy_core::db::lock_library(&users, ALICE, "restore").unwrap());
    for request in [
        bulk(json!({ "keys": ["x_2001"] }), "delete", Value::Null),
        bulk(json!({ "filter": {} }), "delete", Value::Null),
        get("/api/v1/trash"),
        restore(json!({ "deletedAt": NOW })),
        post_empty("/api/v1/trash/empty"),
        delete(&format!(
            "/api/v1/collections/{}?mode=withPosts",
            ids.lighting
        )),
    ] {
        let route = format!("{} {}", request.method(), request.uri());
        let locked = problem(send(&app, request).await, StatusCode::LOCKED).await;
        assert_eq!(locked.code, ErrorCode::UserLocked, "{route}");
    }
    assert!(shelfy_core::db::unlock_library(&users, ALICE).unwrap());
    assert_eq!(trash_stamps(&t, ALICE), [(FIXTURE_TRASHED.to_owned(), NOW)]);
    assert_eq!(jobs_in(&t, ALICE, "purge", "queued"), 0);
}

// ── Jobs ─────────────────────────────────────────────────────────────────────

/// The card's job path: "select all matching" minus exceptions on a 20k
/// synthetic library is a `bulk` job, which reports its progress on
/// `job.updated` and announces its chunks; emptying the trash afterwards is
/// a `purge` job.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn select_all_minus_exceptions_on_a_20k_library_runs_as_a_job() {
    const POSTS: usize = 20_000;
    let t = TestState::new();
    t.add_user(ALICE);
    let all = t.write(ALICE, |tx| synthetic_library(tx, POSTS, 20)).await;
    let except: Vec<String> = all.iter().step_by(400).cloned().collect();
    assert_eq!(except.len(), 50);
    let app = t.app_as(ALICE);
    let _scheduler = scheduler(&t);
    let mut events = t.state.events().subscribe(ALICE, None);

    let before = now_ms();
    let started = call(
        &app,
        bulk(
            json!({ "filter": {}, "exceptKeys": except }),
            "delete",
            Value::Null,
        ),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_schema(&started, "BulkResult");
    assert_eq!(started["action"], "delete");
    assert_eq!(started["selected"], POSTS - 50);
    assert_eq!(started["changed"], Value::Null);
    let stamp = started["deletedAt"].as_i64().unwrap();
    assert!(stamp >= before);
    let job = &started["job"];
    assert_eq!(job["kind"], "bulk");
    assert_eq!(job["state"], "queued");
    let id = job["id"].as_i64().unwrap();

    let (updates, changes) = follow_job(&mut events, id).await;
    // The claim's update has no stage yet; then the worker reports.
    let reported: Vec<&Update> = updates
        .iter()
        .filter(|u| u.state == "running" && u.stage.is_some())
        .collect();
    assert!(reported.len() >= 2, "{updates:?}");
    assert!(
        reported
            .iter()
            .all(|u| u.stage.as_deref() == Some("delete"))
    );
    let progress: Vec<f64> = reported.iter().filter_map(|u| u.progress).collect();
    assert!(progress.windows(2).all(|w| w[0] <= w[1]), "{progress:?}");
    let last = updates.last().unwrap();
    assert_eq!(last.progress, Some(1.0));
    assert!(!changes.is_empty());
    for change in &changes {
        assert_eq!(change["reason"], "delete");
        assert_eq!(
            change["keys"],
            Value::Null,
            "chunks of 500 are past the key cap"
        );
    }
    let row = t.job(ALICE, id).await;
    assert_eq!((row.attempts, row.progress), (0, Some(1.0)));

    // Exactly the selection went, all with the request's stamp.
    let count = ok(&app, get("/api/v1/posts/count")).await;
    assert_eq!(count["total"], 50);
    let live = ok(
        &app,
        post("/api/v1/posts/batch-get", &json!({ "keys": except })),
    )
    .await;
    assert!(
        live["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["deletedAt"].is_null())
    );
    let conn = library(&t, ALICE);
    let stamps: Vec<(i64, i64)> = conn
        .prepare("SELECT deleted_at, count(*) FROM posts WHERE deleted_at IS NOT NULL GROUP BY 1")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(stamps, [(stamp, (POSTS - 50) as i64)]);
    let trash = ok(&app, get("/api/v1/trash?limit=1")).await;
    assert_eq!(trash["total"], POSTS - 50);
    assert_index_consistent(&t, ALICE, "a bulk delete job");

    // Emptying the trash is a purge job; the storage is counted again after.
    let usage_jobs = jobs_in(&t, ALICE, "usage.recompute", "queued")
        + jobs_in(&t, ALICE, "usage.recompute", "succeeded");
    let emptying = call(
        &app,
        post_empty("/api/v1/trash/empty"),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_schema(&emptying, "TrashEmptying");
    assert_eq!(emptying["selected"], POSTS - 50);
    assert_eq!(emptying["job"]["kind"], "purge");
    let id = emptying["job"]["id"].as_i64().unwrap();
    let (updates, changes) = follow_job(&mut events, id).await;
    assert!(updates.iter().any(|u| u.stage.as_deref() == Some("purge")));
    assert!(changes.iter().all(|c| c["reason"] == "delete"));
    let trash = ok(&app, get("/api/v1/trash")).await;
    assert_eq!(trash["total"], 0);
    let left: i64 = conn
        .query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 50);
    let usage_after = jobs_in(&t, ALICE, "usage.recompute", "queued")
        + jobs_in(&t, ALICE, "usage.recompute", "running")
        + jobs_in(&t, ALICE, "usage.recompute", "succeeded");
    assert!(usage_after > usage_jobs, "a purge recounts the storage");
    assert_index_consistent(&t, ALICE, "a purge job");
}

/// Trash then restore, inline and through jobs: both search indexes, the
/// folders and the tags come back exactly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_round_trip_leaves_the_indexes_and_folders_identical() {
    let t = TestState::new();
    t.add_user(ALICE);
    let ids = t.write(ALICE, |tx| fixture(tx)).await;
    let all = t
        .write(ALICE, |tx| {
            let keys = synthetic_library(tx, 1_100, 9)?;
            let posts: Vec<i64> = keys
                .iter()
                .map(|k| posts::id_for_key(tx, k).map(Option::unwrap))
                .collect::<Result<_, _>>()?;
            collections::add_posts(tx, &posts[..700], &[ids.lighting], NOW)?;
            collections::add_posts(tx, &posts[500..900], &[ids.inspiration], NOW)?;
            for &id in posts.iter().step_by(9) {
                posts::update_user_content(
                    tx,
                    id,
                    &UserContentPatch {
                        note: Some(Some(format!("note {id} brutalist"))),
                        tags: Some(vec!["Concrete".into()]),
                    },
                    NOW,
                )?;
            }
            Ok(keys)
        })
        .await;
    let app = t.app_as(ALICE);
    let _scheduler = scheduler(&t);
    let mut events = t.state.events().subscribe(ALICE, None);
    let before = snapshot(&t, ALICE);
    let counts = |folders: &Value| -> Vec<u64> {
        folders["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["count"].as_u64().unwrap())
            .collect()
    };
    let folders = counts(&ok(&app, get("/api/v1/collections")).await);
    assert_eq!(folders, [701, 401]);

    // Inline, by keys.
    let some: Vec<&String> = all[450..750].iter().collect();
    let deleted = ok(&app, bulk(json!({ "keys": some }), "delete", Value::Null)).await;
    assert_eq!(deleted["changed"], 300);
    assert_ne!(snapshot(&t, ALICE), before);
    let back = ok(&app, restore(json!({ "selector": { "keys": some } }))).await;
    assert_eq!(back["changed"], 300);
    assert_eq!(snapshot(&t, ALICE), before);

    // Through jobs: everything, then the undo by the stamp.
    let started = call(
        &app,
        bulk(json!({ "filter": {} }), "delete", Value::Null),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(started["selected"], 1_107);
    let stamp = started["deletedAt"].as_i64().unwrap();
    follow_job(&mut events, started["job"]["id"].as_i64().unwrap()).await;
    let trashed = snapshot(&t, ALICE);
    assert!(trashed.fts.is_empty() && trashed.infix.is_empty());
    assert_eq!(trashed.memberships, before.memberships, "memberships stay");
    assert_eq!(counts(&ok(&app, get("/api/v1/collections")).await), [0, 0]);
    let undo = call(
        &app,
        restore(json!({ "deletedAt": stamp })),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_schema(&undo, "BulkResult");
    assert_eq!(
        (undo["action"].clone(), undo["selected"].clone()),
        (json!("restore"), json!(1_107))
    );
    let (updates, _) = follow_job(&mut events, undo["job"]["id"].as_i64().unwrap()).await;
    assert!(
        updates
            .iter()
            .any(|u| u.stage.as_deref() == Some("restore"))
    );
    assert_eq!(snapshot(&t, ALICE), before);
    assert_eq!(counts(&ok(&app, get("/api/v1/collections")).await), folders);
    assert_index_consistent(&t, ALICE, "a restore job");
}

/// Emptying the trash purges what was in it when the request came, and
/// nothing after; a second purge finds nothing and changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emptying_the_trash_purges_up_to_the_request_and_is_idempotent() {
    let (t, _) = two_libraries().await;
    let app = t.app_as(ALICE);
    let _scheduler = scheduler(&t);
    ok(
        &app,
        bulk(
            json!({ "keys": ["x_2001", "x_2002"] }),
            "delete",
            Value::Null,
        ),
    )
    .await;
    // The queue waits, so the purge runs after a later delete.
    ok(&app, post_empty("/api/v1/queues/purge/pause")).await;
    let emptying = with_key(post_empty("/api/v1/trash/empty"), "empty-1");
    let emptying = call(&app, emptying, StatusCode::ACCEPTED).await;
    assert_eq!(emptying["selected"], 3);
    let id = emptying["job"]["id"].as_i64().unwrap();
    // A repeat with the same key is the same job.
    let again = send(&app, with_key(post_empty("/api/v1/trash/empty"), "empty-1")).await;
    assert_eq!(again.headers()[REPLAYED], "true");
    assert_eq!(json(again).await["job"]["id"], id);
    tokio::time::sleep(Duration::from_millis(50)).await;
    ok(
        &app,
        bulk(json!({ "keys": ["pin_3001"] }), "delete", Value::Null),
    )
    .await;
    ok(&app, post_empty("/api/v1/queues/purge/resume")).await;
    wait_succeeded(&t, ALICE, "purge", 1).await;
    assert_eq!(jobs_in(&t, ALICE, "purge", "succeeded"), 1, "one job");
    let trash = ok(&app, get("/api/v1/trash")).await;
    assert_eq!(keys(&trash), ["pin_3001"], "trashed after the request");
    for gone in ["x_2001", "x_2002", FIXTURE_TRASHED] {
        not_found(&app, get(&format!("/api/v1/posts/{gone}"))).await;
    }
    assert_index_consistent(&t, ALICE, "a purge");

    // Again: the rest goes. Then once more: nothing to purge, so the purge
    // writes nothing and counts no storage (the request itself records the
    // cut of its emptying, see `shelfy_core::trash::emptying`).
    call(
        &app,
        post_empty("/api/v1/trash/empty"),
        StatusCode::ACCEPTED,
    )
    .await;
    wait_succeeded(&t, ALICE, "purge", 2).await;
    let usage_jobs: i64 = t
        .control()
        .query_row(
            "SELECT count(*) FROM jobs WHERE kind = 'usage.recompute'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    ok(&app, post_empty("/api/v1/queues/purge/pause")).await;
    let emptying = call(
        &app,
        post_empty("/api/v1/trash/empty"),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(emptying["selected"], 0);
    let response = send(&app, get("/api/v1/trash")).await;
    let etag = response.headers()[header::ETAG].clone();
    assert_eq!(json(response).await["total"], 0);
    ok(&app, post_empty("/api/v1/queues/purge/resume")).await;
    wait_succeeded(&t, ALICE, "purge", 3).await;
    let unchanged = send(
        &app,
        Request::get("/api/v1/trash")
            .header(header::IF_NONE_MATCH, etag)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED, "no write");
    let usage_after: i64 = t
        .control()
        .query_row(
            "SELECT count(*) FROM jobs WHERE kind = 'usage.recompute'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(usage_after, usage_jobs);
    // Bob's trash was never touched.
    let bobs = t.app_as(BOB);
    ok(&bobs, get("/api/v1/posts/ig_9001")).await;
}

/// Review H1: emptying the trash purges the posts in it at the request, and
/// none of those that a bulk delete asked before moves after it. The delete
/// stamps its posts with the request and dates them by its chunks, so they
/// also keep their full 30 days (review M4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emptying_the_trash_spares_what_a_bulk_delete_moves_after_it() {
    let (t, _) = two_libraries().await;
    t.write(ALICE, |tx| synthetic_library(tx, 1_200, 31)).await;
    let app = t.app_as(ALICE);
    let _scheduler = scheduler(&t);

    // The reviewer's sequence: the delete of everything is stamped now and
    // waits in its queue; the trash is emptied meanwhile.
    ok(&app, post_empty("/api/v1/queues/bulk/pause")).await;
    let started = call(
        &app,
        bulk(json!({ "filter": {} }), "delete", Value::Null),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(started["selected"], 1_207);
    let stamp = started["deletedAt"].as_i64().unwrap();
    ok(&app, post_empty("/api/v1/queues/purge/pause")).await;
    let emptying = call(
        &app,
        post_empty("/api/v1/trash/empty"),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(emptying["selected"], 1, "the fixture's trashed post");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let resumed = now_ms();
    ok(&app, post_empty("/api/v1/queues/bulk/resume")).await;
    wait_succeeded(&t, ALICE, "bulk", 1).await;
    ok(&app, post_empty("/api/v1/queues/purge/resume")).await;
    wait_succeeded(&t, ALICE, "purge", 1).await;

    not_found(&app, get(&format!("/api/v1/posts/{FIXTURE_TRASHED}"))).await;
    let trash = ok(&app, get("/api/v1/trash?limit=1")).await;
    assert_eq!(
        trash["total"], 1_207,
        "the delete's posts are all in the trash"
    );
    let dates: Vec<(i64, i64, i64)> = library(&t, ALICE)
        .prepare(
            "SELECT deleted_at, min(updated_at), count(*) FROM posts
             WHERE deleted_at IS NOT NULL GROUP BY deleted_at",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(dates.len(), 1, "one stamp: {dates:?}");
    let (stamped, dated, count) = dates[0];
    assert_eq!((stamped, count), (stamp, 1_207));
    assert!(
        dated >= resumed,
        "dated by the chunks, after the queue resumed"
    );
    assert_index_consistent(&t, ALICE, "a purge beside a bulk delete");
}

/// Review H1, the other side: a purge waits for the user's bulk jobs asked
/// before it, so a restore job asked before emptying the trash (the undo of
/// a delete of over 500 posts) brings its posts back before the purge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_purge_waits_for_a_restore_asked_before_it() {
    let (t, _) = two_libraries().await;
    t.write(ALICE, |tx| synthetic_library(tx, 1_200, 32)).await;
    let app = t.app_as(ALICE);
    let _scheduler = scheduler(&t);
    let mut events = t.state.events().subscribe(ALICE, None);
    let started = call(
        &app,
        bulk(json!({ "filter": {} }), "delete", Value::Null),
        StatusCode::ACCEPTED,
    )
    .await;
    let stamp = started["deletedAt"].as_i64().unwrap();
    follow_job(&mut events, started["job"]["id"].as_i64().unwrap()).await;

    // The undo waits in the bulk queue; the trash is emptied after it.
    ok(&app, post_empty("/api/v1/queues/bulk/pause")).await;
    let undo = call(
        &app,
        restore(json!({ "deletedAt": stamp })),
        StatusCode::ACCEPTED,
    )
    .await;
    let emptying = call(
        &app,
        post_empty("/api/v1/trash/empty"),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(emptying["selected"], 1_208);
    let purge = emptying["job"]["id"].as_i64().unwrap();
    // The purge starts, finds the restore ahead of it, and goes back to the
    // queue without using a try or deleting anything.
    let waiting = t
        .wait_job(ALICE, purge, |job| {
            job.state == JobState::Queued && job.stage.as_deref() == Some("purge")
        })
        .await;
    assert_eq!(waiting.attempts, 0);
    assert!(waiting.run_at > waiting.created_at);
    assert_eq!(ok(&app, get("/api/v1/trash?limit=1")).await["total"], 1_208);

    ok(&app, post_empty("/api/v1/queues/bulk/resume")).await;
    follow_job(&mut events, undo["job"]["id"].as_i64().unwrap()).await;
    wait_succeeded(&t, ALICE, "purge", 1).await;
    assert_eq!(ok(&app, get("/api/v1/trash")).await["total"], 0);
    not_found(&app, get(&format!("/api/v1/posts/{FIXTURE_TRASHED}"))).await;
    let count = ok(&app, get("/api/v1/posts/count")).await;
    assert_eq!(count["total"], 1_207, "every post the undo restored stayed");
    assert_index_consistent(&t, ALICE, "a purge after a restore");
}

/// Review L4: a purge try that purged posts and then stopped (its queue
/// paused) still has the storage counted again; the next try finds less to
/// purge and would not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_purge_stopped_after_a_chunk_still_recounts_the_storage() {
    let t = TestState::new();
    t.add_user(ALICE);
    t.write(ALICE, |tx| {
        let keys = synthetic_library(tx, 700, 58)?;
        let ids: Vec<i64> = keys
            .iter()
            .map(|k| posts::id_for_key(tx, k).map(Option::unwrap))
            .collect::<Result<_, _>>()?;
        posts::trash(tx, &ids, NOW)
    })
    .await;
    let app = t.app_as(ALICE);
    let _jobs = scheduler(&t);
    let usage_jobs = || {
        ["queued", "running", "succeeded"]
            .iter()
            .map(|state| jobs_in(&t, ALICE, "usage.recompute", state))
            .sum::<i64>()
    };
    ok(&app, post_empty("/api/v1/queues/purge/pause")).await;
    let emptying = call(
        &app,
        post_empty("/api/v1/trash/empty"),
        StatusCode::ACCEPTED,
    )
    .await;
    let id = emptying["job"]["id"].as_i64().unwrap();
    let held = HeldWriter::take(&t, ALICE);
    ok(&app, post_empty("/api/v1/queues/purge/resume")).await;
    wait_in_first_chunk(&t, ALICE, id, "purge").await;
    ok(&app, post_empty("/api/v1/queues/purge/pause")).await;
    assert_eq!(usage_jobs(), 0);
    held.release();
    poll_job(&t.state, ALICE, id, |job| {
        job.state == JobState::Queued && job.progress.is_some_and(|p| p > 0.0)
    })
    .await;
    assert_eq!(ok(&app, get("/api/v1/trash?limit=1")).await["total"], 200);
    assert_eq!(usage_jobs(), 1, "counted again after the stopped try");
}

/// How a test stops a bulk job right after its first chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interruption {
    /// The user pauses the queue, then resumes it.
    Pause,
    /// The server shuts down, then starts again.
    Shutdown,
    /// An operator locks the library (`admin user lock`), then unlocks it.
    Lock,
    /// The queue pauses, the user cancels the job, then retries it.
    CancelThenRetry,
}

/// A state of its own on `t`'s data directory: the server after a restart.
fn restarted(t: &TestState) -> shelfy_server::state::AppState {
    let mut config = Config::with_data_dir(DataDir::new(t.dir.path()).unwrap());
    config.rate_limits = shelfy_server::rate_limit::RateLimitConfig::disabled();
    shelfy_server::state::AppState::open(config).unwrap()
}

/// Review M1: a bulk delete of 1,600 posts stops after its first chunk.
/// Meanwhile the user restores 40 of the posts it trashed, and a post is
/// added. The job then goes on from where it stopped: the 40 stay restored,
/// the new post is never reached, and no try is used.
async fn a_stopped_bulk_job_goes_on_where_it_stopped(interruption: Interruption) {
    let t = TestState::new();
    t.add_user(ALICE);
    let keys = t.write(ALICE, |tx| synthetic_library(tx, 1_600, 41)).await;
    let app = t.app_as(ALICE);
    let jobs = scheduler(&t);

    // The job's first chunk waits for the test's write transaction.
    let request = bulk(json!({ "filter": {} }), "delete", Value::Null);
    let (_, id, held) = start_held(&t, &app, request).await;
    wait_in_first_chunk(&t, ALICE, id, "delete").await;
    let users = t.data_dir().users_dir();
    let jobs = match interruption {
        Interruption::Pause | Interruption::CancelThenRetry => {
            ok(&app, post_empty("/api/v1/queues/bulk/pause")).await;
            held.release();
            Some(jobs)
        }
        Interruption::Shutdown => {
            let stopping =
                tokio::spawn(jobs.stop(tokio::time::Instant::now() + Duration::from_secs(30)));
            tokio::time::sleep(Duration::from_millis(200)).await;
            held.release();
            assert!(stopping.await.unwrap(), "the worker stopped in time");
            None
        }
        Interruption::Lock => {
            assert!(shelfy_core::db::lock_library(&users, ALICE, "restore").unwrap());
            held.release();
            Some(jobs)
        }
    };
    // The first chunk committed, then the job went back to the queue with
    // its checkpoint, without using a try.
    let stopped = poll_job(&t.state, ALICE, id, |job| {
        job.state == JobState::Queued && checkpoint(job).1 > 0
    })
    .await;
    assert_eq!(stopped.attempts, 0, "{interruption:?}");
    let first_ids: Vec<i64> = {
        let conn = library(&t, ALICE);
        conn.prepare("SELECT id FROM posts WHERE deleted_at IS NOT NULL ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(first_ids.len(), 500, "{interruption:?}: one chunk");
    assert_eq!(
        checkpoint(&stopped),
        (*first_ids.last().unwrap(), 500),
        "{interruption:?}"
    );
    if interruption == Interruption::Lock {
        assert!(stopped.run_at > stopped.updated_at, "held for the unlock");
        assert!(shelfy_core::db::unlock_library(&users, ALICE).unwrap());
    }

    // In between: the user restores 40 of the trashed posts, by key, and a
    // post arrives that the filter matches.
    let restored: Vec<&String> = keys[..40].iter().collect();
    let back = ok(&app, restore(json!({ "selector": { "keys": restored } }))).await;
    assert_eq!(back["changed"], 40);
    t.write(ALICE, |tx| {
        let post = NewPost::new("ig_77777", Platform::Instagram, "77777", "image", NOW);
        posts::insert(tx, &post, NOW)
    })
    .await;

    // The job goes on.
    let done = match interruption {
        Interruption::Pause => {
            ok(&app, post_empty("/api/v1/queues/bulk/resume")).await;
            let done = poll_job(&t.state, ALICE, id, |job| job.state.is_final()).await;
            drop(jobs);
            done
        }
        Interruption::CancelThenRetry => {
            let cancelled = ok(&app, post_empty(&format!("/api/v1/jobs/{id}/cancel"))).await;
            assert_eq!(cancelled["state"], "cancelled");
            ok(&app, post_empty("/api/v1/queues/bulk/resume")).await;
            let retried = ok(&app, post_empty(&format!("/api/v1/jobs/{id}/retry"))).await;
            assert_eq!(retried["state"], "queued");
            let done = poll_job(&t.state, ALICE, id, |job| job.state.is_final()).await;
            drop(jobs);
            done
        }
        Interruption::Shutdown => {
            let _jobs = scheduler(&t);
            poll_job(&t.state, ALICE, id, |job| job.state.is_final()).await
        }
        Interruption::Lock => {
            // The library stays held for a minute after a lock: a server
            // that restarts once that time is over runs the job again.
            drop(jobs);
            t.control()
                .execute("UPDATE jobs SET run_at = 0 WHERE id = ?1", [id])
                .unwrap();
            let state = restarted(&t);
            let _jobs = state.jobs().start(state.clone(), CancellationToken::new());
            poll_job(&state, ALICE, id, |job| job.state.is_final()).await
        }
    };
    assert_eq!(
        done.state,
        JobState::Succeeded,
        "{interruption:?}: {done:?}"
    );
    assert_eq!(done.attempts, 0, "{interruption:?}");
    assert_eq!(checkpoint(&done).1, 1_600, "{interruption:?}");
    let trash = ok(&app, get("/api/v1/trash?limit=1")).await;
    assert_eq!(trash["total"], 1_560, "{interruption:?}");
    let live = ok(
        &app,
        post("/api/v1/posts/batch-get", &json!({ "keys": restored })),
    )
    .await;
    assert!(
        live["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["deletedAt"].is_null()),
        "{interruption:?}: the restored posts stayed restored"
    );
    assert_eq!(
        ok(&app, get("/api/v1/posts/ig_77777")).await["deletedAt"],
        Value::Null,
        "{interruption:?}: the new post was not reached"
    );
    assert_index_consistent(&t, ALICE, "an interrupted bulk job");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paused_bulk_job_goes_on_where_it_stopped() {
    a_stopped_bulk_job_goes_on_where_it_stopped(Interruption::Pause).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bulk_job_stopped_by_a_shutdown_goes_on_where_it_stopped() {
    a_stopped_bulk_job_goes_on_where_it_stopped(Interruption::Shutdown).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bulk_job_stopped_by_a_user_lock_goes_on_where_it_stopped() {
    a_stopped_bulk_job_goes_on_where_it_stopped(Interruption::Lock).await;
}

/// The manual retry of a cancelled delete goes on from its checkpoint too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retried_bulk_job_goes_on_where_it_stopped() {
    a_stopped_bulk_job_goes_on_where_it_stopped(Interruption::CancelThenRetry).await;
}

/// Review M2: the undo of a delete that still waits in the bulk queue
/// cancels it, so it never moves a post. On a locked library the undo
/// answers 423 and cancels nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_undo_of_a_queued_delete_job_cancels_it() {
    let (t, _) = two_libraries().await;
    t.write(ALICE, |tx| synthetic_library(tx, 800, 51)).await;
    let app = t.app_as(ALICE);
    let _jobs = scheduler(&t);
    ok(&app, post_empty("/api/v1/queues/bulk/pause")).await;
    let started = call(
        &app,
        bulk(json!({ "filter": {} }), "delete", Value::Null),
        StatusCode::ACCEPTED,
    )
    .await;
    let id = started["job"]["id"].as_i64().unwrap();
    let stamp = started["deletedAt"].as_i64().unwrap();

    let users = t.data_dir().users_dir();
    assert!(shelfy_core::db::lock_library(&users, ALICE, "restore").unwrap());
    let locked = problem(
        send(&app, restore(json!({ "deletedAt": stamp }))).await,
        StatusCode::LOCKED,
    )
    .await;
    assert_eq!(locked.code, ErrorCode::UserLocked);
    assert_eq!(t.job(ALICE, id).await.state, JobState::Queued);
    assert!(shelfy_core::db::unlock_library(&users, ALICE).unwrap());

    let undone = ok(&app, restore(json!({ "deletedAt": stamp }))).await;
    assert_eq!(
        (undone["selected"].clone(), undone["changed"].clone()),
        (json!(0), json!(0))
    );
    assert_eq!(t.job(ALICE, id).await.state, JobState::Cancelled);
    ok(&app, post_empty("/api/v1/queues/bulk/resume")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(t.job(ALICE, id).await.state, JobState::Cancelled);
    assert_eq!(trash_stamps(&t, ALICE), [(FIXTURE_TRASHED.to_owned(), NOW)]);
}

/// Review M2: the undo of a delete whose first chunk is under way cancels
/// it; the chunk sees the cancel under the library's writer and moves
/// nothing, so nothing of that delete is left in the trash.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_undo_of_a_running_delete_job_leaves_nothing_behind() {
    let (t, _) = two_libraries().await;
    t.write(ALICE, |tx| synthetic_library(tx, 800, 52)).await;
    let app = t.app_as(ALICE);
    let _jobs = scheduler(&t);
    let request = bulk(json!({ "filter": {} }), "delete", Value::Null);
    let (started, id, held) = start_held(&t, &app, request).await;
    let stamp = started["deletedAt"].as_i64().unwrap();
    wait_in_first_chunk(&t, ALICE, id, "delete").await;

    // The undo cancels the job, then waits for the writer to restore.
    let undo = tokio::spawn({
        let app = app.clone();
        async move { send(&app, restore(json!({ "deletedAt": stamp }))).await }
    });
    poll_job(&t.state, ALICE, id, |job| job.state == JobState::Cancelled).await;
    held.release();
    let undone = json(undo.await.unwrap()).await;
    assert_eq!(undone["changed"], 0, "{undone}");
    assert_eq!(trash_stamps(&t, ALICE), [(FIXTURE_TRASHED.to_owned(), NOW)]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let job = t.job(ALICE, id).await;
    assert_eq!(job.state, JobState::Cancelled);
    assert_eq!(checkpoint(&job), (0, 0), "the chunk moved nothing");
    assert_eq!(trash_stamps(&t, ALICE).len(), 1);
}

/// Review M2: the undo of a delete job that moved 1,000 posts before it was
/// stopped cancels it and restores the 1,000 as a job of its own; the
/// delete never moves another post.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_undo_of_a_partly_done_delete_job_restores_all_it_moved() {
    let t = TestState::new();
    t.add_user(ALICE);
    t.write(ALICE, |tx| synthetic_library(tx, 1_600, 53)).await;
    let app = t.app_as(ALICE);
    let _jobs = scheduler(&t);
    let mut events = t.state.events().subscribe(ALICE, None);
    let before = snapshot(&t, ALICE);

    // Two chunks, each held until the queue is paused.
    let request = bulk(json!({ "filter": {} }), "delete", Value::Null);
    let (started, id, held) = start_held(&t, &app, request).await;
    let stamp = started["deletedAt"].as_i64().unwrap();
    wait_in_first_chunk(&t, ALICE, id, "delete").await;
    ok(&app, post_empty("/api/v1/queues/bulk/pause")).await;
    held.release();
    poll_job(&t.state, ALICE, id, |job| {
        job.state == JobState::Queued && checkpoint(job).1 == 500
    })
    .await;
    let held = HeldWriter::take(&t, ALICE);
    ok(&app, post_empty("/api/v1/queues/bulk/resume")).await;
    wait_in_first_chunk(&t, ALICE, id, "delete").await;
    ok(&app, post_empty("/api/v1/queues/bulk/pause")).await;
    held.release();
    poll_job(&t.state, ALICE, id, |job| {
        job.state == JobState::Queued && checkpoint(job).1 == 1_000
    })
    .await;
    assert_eq!(trash_stamps(&t, ALICE).len(), 1_000);

    let undo = call(
        &app,
        restore(json!({ "deletedAt": stamp })),
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(undo["selected"], 1_000);
    assert_eq!(t.job(ALICE, id).await.state, JobState::Cancelled);
    ok(&app, post_empty("/api/v1/queues/bulk/resume")).await;
    follow_job(&mut events, undo["job"]["id"].as_i64().unwrap()).await;
    assert!(trash_stamps(&t, ALICE).is_empty());
    assert_eq!(snapshot(&t, ALICE), before);
    assert_eq!(t.job(ALICE, id).await.state, JobState::Cancelled);
}

/// The nightly purge (03:00 UTC) deletes what has been in the trash for 30
/// days, on the job system's clock, and keeps the rest for later nights.
#[tokio::test(start_paused = true)]
async fn the_nightly_purge_keeps_thirty_days_of_trash() {
    // 02:59 UTC on 2026-10-02: the first nightly run is a minute away.
    let start = START + 119 * 60_000;
    let three = start + 60_000;
    let t = TestState::with_jobs_at(kinds::registry(), start);
    t.add_user(ALICE);
    t.write(ALICE, |tx| fixture(tx)).await;
    let stamps = [
        ("x_2001", three - 31 * DAY),
        ("x_2002", three - 31 * DAY),
        ("pin_3001", three - 29 * DAY),
        ("ig_1002", three - DAY),
    ];
    t.write(ALICE, |tx| {
        for (key, at) in stamps {
            let id = posts::id_for_key(tx, key)?.unwrap();
            posts::trash(tx, &[id], at)?;
        }
        Ok(())
    })
    .await;
    let trashed = |t: &TestState| -> Vec<String> {
        trash_stamps(t, ALICE).into_iter().map(|(k, _)| k).collect()
    };
    let _scheduler = scheduler(&t);

    wait_succeeded(&t, ALICE, "purge", 1).await;
    assert!(t.state.jobs().clock().now_ms() >= three);
    assert_eq!(
        trashed(&t),
        ["ig_1002", FIXTURE_TRASHED, "pin_3001"],
        "older than 30 days: gone"
    );
    let app = t.app_as(ALICE);
    not_found(&app, get("/api/v1/posts/x_2001")).await;

    // The next night, the post trashed 29 days before the first is 30 days
    // old.
    wait_succeeded(&t, ALICE, "purge", 2).await;
    assert!(t.state.jobs().clock().now_ms() >= three + DAY);
    assert_eq!(trashed(&t), ["ig_1002", FIXTURE_TRASHED]);
    // The library is untouched otherwise.
    let count = ok(&app, get("/api/v1/posts/count")).await;
    assert_eq!(count["total"], 3);
    assert_index_consistent(&t, ALICE, "the nightly purges");
}

/// Review M3: a 423 of a locked library is not kept for the key, so the same
/// request with the same key runs once the library is unlocked; a repeat of
/// the job request then gets the first 202 back, and one job is queued.
#[tokio::test]
async fn idempotency_keys_forget_locks_and_replay_jobs() {
    let (t, _) = two_libraries().await;
    t.write(ALICE, |tx| synthetic_library(tx, 700, 54)).await;
    let app = t.app_as(ALICE);
    let users = t.data_dir().users_dir();
    let request = |key: &str| with_key(bulk(json!({ "filter": {} }), "delete", Value::Null), key);

    assert!(shelfy_core::db::lock_library(&users, ALICE, "restore").unwrap());
    let locked = send(&app, request("bulk-job-1")).await;
    assert_eq!(locked.status(), StatusCode::LOCKED);
    assert_eq!(locked.headers()[header::RETRY_AFTER], "60");
    assert!(locked.headers().get(REPLAYED).is_none());
    for (route, body) in [
        ("/api/v1/trash/restore", json!({ "deletedAt": NOW })),
        ("/api/v1/trash/empty", Value::Null),
    ] {
        let request = if body.is_null() {
            post_empty(route)
        } else {
            post(route, &body)
        };
        let again = send(&app, with_key(request, &format!("locked-{route}"))).await;
        assert_eq!(again.status(), StatusCode::LOCKED, "{route}");
    }
    assert!(shelfy_core::db::unlock_library(&users, ALICE).unwrap());

    let first = send(&app, request("bulk-job-1")).await;
    assert_eq!(first.status(), StatusCode::ACCEPTED, "the key was released");
    assert!(first.headers().get(REPLAYED).is_none());
    let first = json(first).await;
    let again = send(&app, request("bulk-job-1")).await;
    assert_eq!(again.status(), StatusCode::ACCEPTED);
    assert_eq!(again.headers()[REPLAYED], "true");
    assert_eq!(json(again).await, first);
    assert_eq!(jobs_in(&t, ALICE, "bulk", "queued"), 1, "one job");
    // The emptying's key works after the unlock too.
    let emptying = send(
        &app,
        with_key(
            post_empty("/api/v1/trash/empty"),
            "locked-/api/v1/trash/empty",
        ),
    )
    .await;
    assert_eq!(emptying.status(), StatusCode::ACCEPTED);
    assert!(emptying.headers().get(REPLAYED).is_none());
}

/// A repeated `POST /posts/bulk` with the same `Idempotency-Key` gets the
/// first answer back and acts once; another body with that key is refused.
#[tokio::test]
async fn idempotency_keys_make_a_bulk_action_act_once() {
    let (t, _) = two_libraries().await;
    let app = t.app_as(ALICE);
    let request = || {
        with_key(
            bulk(json!({ "keys": ["x_2001"] }), "delete", Value::Null),
            "bulk-1",
        )
    };
    let first = send(&app, request()).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert!(first.headers().get(REPLAYED).is_none());
    let first = json(first).await;
    assert_eq!(first["changed"], 1);
    // Restored meanwhile: a replay must not delete it again.
    ok(&app, restore(json!({ "selector": { "keys": ["x_2001"] } }))).await;
    let replay = send(&app, request()).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()[REPLAYED], "true");
    assert_eq!(json(replay).await, first);
    assert_eq!(
        ok(&app, get("/api/v1/posts/x_2001")).await["deletedAt"],
        Value::Null
    );
    // The same key on another request.
    invalid(
        &app,
        with_key(
            bulk(json!({ "keys": ["x_2002"] }), "delete", Value::Null),
            "bulk-1",
        ),
        "Idempotency-Key",
    )
    .await;
    let documented: BTreeSet<&str> = shelfy_server::routes::IDEMPOTENT_ROUTES
        .iter()
        .map(|r| r.path)
        .collect();
    for path in [
        "/api/v1/posts/bulk",
        "/api/v1/trash/restore",
        "/api/v1/trash/empty",
    ] {
        assert!(documented.contains(path), "{path}");
    }
}
