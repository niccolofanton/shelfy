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
use shelfy_core::repo::collections;
use shelfy_core::repo::posts::{self, UserContentPatch};
use shelfy_core::search::index;
use shelfy_server::error::ErrorCode;
use shelfy_server::events::Delivery;
use shelfy_server::events::model::{EventTopic, JobState};
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::jobs::idempotency::{IDEMPOTENCY_KEY, REPLAYED};
use shelfy_server::jobs::{Scheduler, kinds};
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

/// The new routes are cookie-only: a valid token with every scope is
/// refused, alone or beside a valid session cookie; the cookie alone works,
/// through the CSRF guard.
#[tokio::test]
async fn api_tokens_never_call_the_new_routes() {
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
                StatusCode::UNAUTHORIZED,
            )
            .await;
            assert_eq!(refused.code, ErrorCode::Unauthorized, "{route}");
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
    let mut forged = with_session(
        bulk(json!({ "keys": ["x_2002"] }), "delete", Value::Null),
        &cookie,
    );
    forged.headers_mut().remove("x-shelfy-client");
    let refused = problem(send(&app, forged).await, StatusCode::FORBIDDEN).await;
    assert_eq!(refused.code, ErrorCode::CsrfFailed);
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
