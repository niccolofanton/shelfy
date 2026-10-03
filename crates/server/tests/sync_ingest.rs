//! `POST /ingest/batches`, the sync-run routes and `GET /extension/sources`
//! (P2-09; plan §2.16, contracts C4, C5) through the real middleware stack:
//!
//! - a run walks a folder and its batch is merged, mapped into the folder,
//!   given archive states and counted, with `posts.changed`, `sync.progress`,
//!   the daily ingest count and an `archive.drain` job;
//! - a merge that changes nothing writes nothing, and the search index stays
//!   consistent;
//! - a replayed `Idempotency-Key` is not ingested twice;
//! - a killed capture mode is 409 `source_disabled`;
//! - the folder, existing and none mapping modes, a renamed folder keeping
//!   its name;
//! - the incremental flag and the resume cursor across runs;
//! - the end-of-run notification of a manual run;
//! - the authz rules every new route ships: no credentials, a token without
//!   the scope, the version gate, and one user's runs never reaching another.

mod support;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use rusqlite::params;
use serde_json::{Value, json};
use shelfy_server::error::ErrorCode;
use shelfy_server::events::Delivery;
use shelfy_server::events::model::EventTopic;
use shelfy_server::extension::VERSION_HEADER;
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::tokens::{SecretToken, hash_token};
use support::auth::owner;
use support::library::{ALICE, BOB};
use support::{TestState, get, json, post_json, problem, send};

const VERSION: &str = "0.2.0";

/// A state with ALICE and BOB.
fn bench() -> TestState {
    let t = TestState::new();
    t.add_user(ALICE);
    t.add_user(BOB);
    t
}

/// An `extension`-kind token with the four extension scopes for `user`.
fn ext_token(t: &TestState, user: &str) -> String {
    let token = format!("shx_{}", SecretToken::generate().expose());
    t.control()
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
             VALUES (?1, ?2, 'extension', ?3, 'ingest tasks uploads lookup', ?4)",
            params![new_ulid(), user, hash_token(&token).as_slice(), now_ms()],
        )
        .unwrap();
    token
}

/// `request` as the extension sends it: a bearer token and the version header.
fn ext(mut request: Request<Body>, token: &str, version: Option<&str>) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    if let Some(version) = version {
        request
            .headers_mut()
            .insert(VERSION_HEADER, version.parse().unwrap());
    }
    request
}

/// A `POST`/`PATCH` with a JSON body, a bearer token and the version header.
fn bearer(method: Method, uri: &str, body: &Value, token: &str) -> Request<Body> {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    ext(request, token, Some(VERSION))
}

/// The items of a capture page: a valid carousel, a non-id (rejected), and a
/// second post whose cover URL is far in the future (so the server keeps it).
fn page(caption: &str) -> Value {
    json!([
        {
            "id": "3191575067010950169_25025320",
            "shortcode": "CxKwJ0fLmQZ",
            "postUrl": "https://www.instagram.com/p/CxKwJ0fLmQZ/",
            "text": caption,
            "timestamp": "2023-09-14T09:56:58.000Z",
            "thumbnailUrl": "https://scontent.cdninstagram.com/v/1.jpg?oe=7FFFFF00",
            "mediaType": "carousel",
            "media": [
                {"type": "image", "url": "https://scontent.cdninstagram.com/v/1.jpg?oe=7FFFFF00"},
                {"type": "image", "url": "https://scontent.cdninstagram.com/v/2.jpg?oe=7FFFFF00"},
            ],
        },
        {"id": "not an id"},
        {
            "id": "3191575067010950170",
            "thumbnailUrl": "https://scontent.cdninstagram.com/v/3.jpg?oe=7FFFFF00",
            "mediaType": "image",
        },
    ])
}

const KEY_A: &str = "ig_3191575067010950169";
const KEY_B: &str = "ig_3191575067010950170";

/// Opens a run as `token`, returning the response (expects 201).
async fn open_run(app: &Router, token: &str, body: Value) -> Value {
    let response = send(app, bearer(Method::POST, "/api/v1/sync-runs", &body, token)).await;
    assert_eq!(response.status(), StatusCode::CREATED, "open run");
    json(response).await
}

/// A batch body for `run`.
fn batch(run: &str, source: &str, items: Value) -> Value {
    json!({
        "syncRunId": run,
        "platform": "instagram",
        "source": source,
        "hasNextPage": false,
        "client": { "ext": VERSION, "parser": "hook-1" },
        "items": items,
    })
}

/// Ingests `body` with idempotency key `key`.
async fn ingest(app: &Router, token: &str, key: &str, body: &Value) -> axum::response::Response {
    let mut request = bearer(Method::POST, "/api/v1/ingest/batches", body, token);
    request
        .headers_mut()
        .insert("idempotency-key", key.parse().unwrap());
    send(app, request).await
}

/// The next `sync.progress` payload on `events`.
async fn next_progress(events: &mut shelfy_server::events::Subscription) -> Value {
    loop {
        let next = tokio::time::timeout(std::time::Duration::from_secs(10), events.next())
            .await
            .expect("an event");
        if let Delivery::Event(event) = next
            && event.topic == EventTopic::SyncProgress
        {
            return serde_json::from_str(&event.data).unwrap();
        }
    }
}

fn job_count(t: &TestState, user: &str, kind: &str) -> i64 {
    t.control()
        .query_row(
            "SELECT count(*) FROM jobs WHERE user_id = ?1 AND kind = ?2",
            params![user, kind],
            |r| r.get(0),
        )
        .unwrap()
}

fn ingest_items(t: &TestState, user: &str) -> i64 {
    t.control()
        .query_row(
            "SELECT COALESCE(sum(ingest_items), 0) FROM usage_daily WHERE user_id = ?1",
            [user],
            |r| r.get(0),
        )
        .unwrap()
}

// ── The happy path ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_run_walks_a_folder_and_ingests_its_posts() {
    let t = bench();
    let app = t.app();
    let token = ext_token(&t, ALICE);
    let mut events = t.state.events().subscribe(ALICE, None);

    let opened = open_run(
        &app,
        &token,
        json!({
            "platform": "instagram",
            "trigger": "manual",
            "listing": { "kind": "ig_collection", "externalId": "42", "name": "Lighting" },
            "collection": { "mode": "auto" },
        }),
    )
    .await;
    assert_eq!(opened["incremental"], false, "no full walk yet");
    assert_eq!(opened["stopAfterKnown"], 10, "the IG default");
    assert_eq!(opened["resumeCursor"], Value::Null);
    let run = opened["id"].as_str().unwrap().to_owned();
    let collection_id = opened["collectionId"].as_i64().unwrap();

    let response = ingest(
        &app,
        &token,
        &new_ulid(),
        &batch(&run, "replay", page("A lamp")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(
        (
            body["inserted"].as_i64(),
            body["updated"].as_i64(),
            body["known"].as_i64()
        ),
        (Some(2), Some(0), Some(0))
    );
    assert_eq!(body["results"].as_array().unwrap().len(), 2);
    assert_eq!(
        body["results"][0],
        json!({ "index": 0, "key": KEY_A, "outcome": "inserted", "changed": true })
    );
    assert_eq!(
        body["results"][1],
        json!({ "index": 2, "key": KEY_B, "outcome": "inserted", "changed": true })
    );
    assert_eq!(body["rejected"], json!([{ "index": 1, "code": "bad_id" }]));

    // The posts are saved and mapped into the folder, which kept its name.
    let session = t.app_as(ALICE);
    let post = json(send(&session, get(&format!("/api/v1/posts/{KEY_A}"))).await).await;
    assert_eq!(post["mediaType"], "carousel");
    let collections = json(send(&session, get("/api/v1/collections")).await).await;
    let folder = collections["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == collection_id)
        .unwrap();
    assert_eq!(folder["name"], "Lighting");
    assert_eq!(folder["count"], 2);

    // The counters, the daily usage and the drain.
    let progress = next_progress(&mut events).await;
    assert_eq!(progress["runId"], run);
    assert_eq!(progress["platform"], "instagram");
    assert_eq!(
        progress["listing"],
        json!({ "kind": "ig_collection", "externalId": "42", "name": "Lighting" })
    );
    assert_eq!(
        (
            progress["scanned"].as_i64(),
            progress["inserted"].as_i64(),
            progress["known"].as_i64()
        ),
        (Some(2), Some(2), Some(0))
    );
    assert_eq!(progress["state"], "running");
    assert_eq!(ingest_items(&t, ALICE), 2, "the accepted items");
    assert_eq!(
        job_count(&t, ALICE, "archive.drain"),
        1,
        "pending covers to fetch"
    );
}

// ── Idempotency and merge idempotence ──────────────────────────────────────────

#[tokio::test]
async fn a_replayed_batch_id_is_not_ingested_twice() {
    let t = bench();
    let app = t.app();
    let token = ext_token(&t, ALICE);
    let run = open_run(&app, &token, auto_saved_run()).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = batch(&run, "scroll", page("A lamp"));
    let key = new_ulid();

    let first = ingest(&app, &token, &key, &body).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(json(first).await["inserted"], 2);

    let replay = ingest(&app, &token, &key, &body).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers().get("idempotent-replayed").unwrap(), "true");
    assert_eq!(json(replay).await["inserted"], 2, "the first answer, again");

    // The run was counted once, and the library was written once.
    let session = t.app_as(ALICE);
    let runs = json(send(&session, get("/api/v1/sync-runs")).await).await;
    assert_eq!(runs["items"][0]["scanned"], 2, "not doubled");
    assert_eq!(
        json(send(&session, get("/api/v1/stats")).await).await["total"],
        2
    );
}

#[tokio::test]
async fn a_merge_that_changes_nothing_writes_nothing_and_keeps_the_index() {
    let t = bench();
    let app = t.app();
    let token = ext_token(&t, ALICE);
    let run = open_run(&app, &token, auto_saved_run()).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ingest(
        &app,
        &token,
        &new_ulid(),
        &batch(&run, "scroll", page("A lamp")),
    )
    .await;

    // The same posts again, under a new key: all known, none changed.
    let again = ingest(
        &app,
        &token,
        &new_ulid(),
        &batch(&run, "scroll", page("A lamp")),
    )
    .await;
    let body = json(again).await;
    assert_eq!(
        (
            body["inserted"].as_i64(),
            body["updated"].as_i64(),
            body["known"].as_i64()
        ),
        (Some(0), Some(0), Some(2))
    );
    assert!(
        body["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["outcome"] == "known" && r["changed"] == false)
    );

    // A new caption changes one post.
    let changed = ingest(
        &app,
        &token,
        &new_ulid(),
        &batch(&run, "scroll", page("A glass lamp")),
    )
    .await;
    let body = json(changed).await;
    assert_eq!(
        (
            body["inserted"].as_i64(),
            body["updated"].as_i64(),
            body["known"].as_i64()
        ),
        (Some(0), Some(1), Some(2))
    );

    // The FTS index is still consistent with the posts.
    let stale = t
        .state
        .user_db(ALICE)
        .await
        .unwrap()
        .read(|conn| {
            shelfy_core::search::index::verify(conn).map_err(shelfy_core::repo::RepoError::from)
        })
        .unwrap();
    assert_eq!(stale, Vec::<i64>::new(), "no stale index rows");
}

// ── Kill switch ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_killed_capture_mode_refuses_the_batch() {
    let t = bench();
    let app = t.app();
    let token = ext_token(&t, ALICE);
    let run = open_run(&app, &token, auto_saved_run()).await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    shelfy_server::admin::flags::set(&t.data_dir(), "extension.instagram.replay", "false").unwrap();
    t.state.extension().flags().invalidate();

    let killed = ingest(
        &app,
        &token,
        &new_ulid(),
        &batch(&run, "replay", page("A lamp")),
    )
    .await;
    let refused = problem(killed, StatusCode::CONFLICT).await;
    assert_eq!(refused.code, ErrorCode::SourceDisabled);

    // Scroll is still on, and the selection import is never a kill switch.
    for source in ["scroll", "selection"] {
        let ok = ingest(
            &app,
            &token,
            &new_ulid(),
            &batch(&run, source, page("A lamp")),
        )
        .await;
        assert_eq!(ok.status(), StatusCode::OK, "{source}");
    }
}

// ── Mapping modes ────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_three_mapping_modes_and_a_rename() {
    let t = bench();
    let app = t.app();
    let session = t.app_as(ALICE);
    let token = ext_token(&t, ALICE);

    // none: no membership.
    let none = open_run(
        &app,
        &token,
        json!({ "platform": "instagram", "trigger": "manual",
                "listing": { "kind": "ig_saved", "externalId": null, "name": null },
                "collection": { "mode": "none" } }),
    )
    .await;
    assert_eq!(none["collectionId"], Value::Null);
    ingest(
        &app,
        &token,
        &new_ulid(),
        &batch(none["id"].as_str().unwrap(), "scroll", page("A lamp")),
    )
    .await;
    assert!(
        json(send(&session, get("/api/v1/collections")).await).await["items"]
            .as_array()
            .unwrap()
            .is_empty(),
        "none maps into no folder"
    );

    // existing: into a collection the user already has.
    let made = json(
        send(
            &session,
            post_json(
                "/api/v1/collections",
                json!({ "name": "Picks" }).to_string(),
            ),
        )
        .await,
    )
    .await;
    let picks = made["id"].as_i64().unwrap();
    let existing = open_run(
        &app,
        &token,
        json!({ "platform": "instagram", "trigger": "manual",
                "listing": { "kind": "ig_saved", "externalId": null, "name": null },
                "collection": { "mode": "existing", "id": picks } }),
    )
    .await;
    assert_eq!(existing["collectionId"], picks);
    ingest(
        &app,
        &token,
        &new_ulid(),
        &batch(existing["id"].as_str().unwrap(), "scroll", page("A lamp")),
    )
    .await;
    let picks_now = find_collection(&session, picks).await;
    assert_eq!(picks_now["count"], 2);

    // auto: finds or creates by (platform, external id); a rename is kept.
    let first = open_run(
        &app,
        &token,
        json!({ "platform": "instagram", "trigger": "manual",
                "listing": { "kind": "ig_collection", "externalId": "77", "name": "Wood" },
                "collection": { "mode": "auto" } }),
    )
    .await;
    let folder = first["collectionId"].as_i64().unwrap();
    assert_eq!(find_collection(&session, folder).await["name"], "Wood");
    rename(&session, folder, "Woodwork").await;
    let again = open_run(
        &app,
        &token,
        json!({ "platform": "instagram", "trigger": "manual",
                "listing": { "kind": "ig_collection", "externalId": "77", "name": "Wood" },
                "collection": { "mode": "auto" } }),
    )
    .await;
    assert_eq!(again["collectionId"], folder, "the same folder");
    assert_eq!(
        find_collection(&session, folder).await["name"],
        "Woodwork",
        "the rename is kept"
    );
}

async fn find_collection(session: &Router, id: i64) -> Value {
    let collections = json(send(session, get("/api/v1/collections")).await).await;
    collections["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
        .clone()
}

async fn rename(session: &Router, id: i64, name: &str) {
    let request = support::from_app(
        Request::builder()
            .method(Method::PATCH)
            .uri(format!("/api/v1/collections/{id}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "name": name }).to_string()))
            .unwrap(),
    );
    let response = send(session, request).await;
    assert_eq!(response.status(), StatusCode::OK, "rename");
}

// ── Incremental and resume (P2-G1, P2-G2) ───────────────────────────────────────

#[tokio::test]
async fn the_feed_end_makes_the_next_run_incremental_and_a_cap_leaves_a_cursor() {
    let t = bench();
    let app = t.app();
    let token = ext_token(&t, ALICE);

    // A first run reaches the end of the feed.
    let first = open_run(&app, &token, auto_saved_run()).await;
    assert_eq!(first["incremental"], false);
    patch_run(
        &app,
        &token,
        first["id"].as_str().unwrap(),
        json!({ "state": "done", "pages": 2, "stopReason": "end_of_feed" }),
    )
    .await;

    // The next run of the same source is incremental.
    let second = open_run(&app, &token, auto_saved_run()).await;
    assert_eq!(second["incremental"], true, "the feed ended before");
    // It is capped and leaves a cursor.
    patch_run(&app, &token, second["id"].as_str().unwrap(),
        json!({ "state": "stopped", "pages": 100, "stopReason": "page_cap", "resumeCursor": "page-100" })).await;

    // The third run resumes from the cursor.
    let third = open_run(&app, &token, auto_saved_run()).await;
    assert_eq!(third["resumeCursor"], "page-100");
    assert_eq!(third["incremental"], true);

    // A full walk clears the cursor.
    patch_run(
        &app,
        &token,
        third["id"].as_str().unwrap(),
        json!({ "state": "done", "pages": 1, "stopReason": "end_of_feed" }),
    )
    .await;
    let fourth = open_run(&app, &token, auto_saved_run()).await;
    assert_eq!(fourth["resumeCursor"], Value::Null, "the end cleared it");
}

async fn patch_run(app: &Router, token: &str, id: &str, body: Value) -> Value {
    let response = send(
        app,
        bearer(
            Method::PATCH,
            &format!("/api/v1/sync-runs/{id}"),
            &body,
            token,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "patch run");
    json(response).await
}

// ── End-of-run notification and sources ─────────────────────────────────────────

#[tokio::test]
async fn a_manual_run_end_notifies_and_lists_its_source() {
    let t = bench();
    let app = t.app();
    let token = ext_token(&t, ALICE);
    let session = t.app_as(ALICE);

    let run = open_run(
        &app,
        &token,
        json!({ "platform": "instagram", "trigger": "manual",
                "listing": { "kind": "ig_collection", "externalId": "9", "name": "Refs" },
                "collection": { "mode": "auto" } }),
    )
    .await;
    ingest(
        &app,
        &token,
        &new_ulid(),
        &batch(run["id"].as_str().unwrap(), "scroll", page("A lamp")),
    )
    .await;
    patch_run(
        &app,
        &token,
        run["id"].as_str().unwrap(),
        json!({ "state": "done", "pages": 1, "scanned": 2, "stopReason": "end_of_feed" }),
    )
    .await;

    // The notification of the end of a manual run.
    let notifications = json(send(&session, get("/api/v1/notifications")).await).await;
    let items = notifications["items"].as_array().unwrap();
    let sync = items
        .iter()
        .find(|n| n["kind"] == "sync")
        .expect("a sync notification");
    assert_eq!(sync["code"], "sync.done");
    assert_eq!(sync["params"]["inserted"], 2);

    // The source is listed for the extension's planner.
    let sources = json(
        send(
            &app,
            ext(get("/api/v1/extension/sources"), &token, Some(VERSION)),
        )
        .await,
    )
    .await;
    let source = &sources["items"][0];
    assert_eq!(source["platform"], "instagram");
    assert_eq!(
        source["listing"],
        json!({ "kind": "ig_collection", "externalId": "9", "name": "Refs" })
    );
    assert!(source["lastRunAt"].is_number());
    assert!(source["lastFullAt"].is_number(), "a full walk recorded");
}

#[tokio::test]
async fn a_passive_run_end_does_not_notify() {
    let t = bench();
    let app = t.app();
    let token = ext_token(&t, ALICE);
    let session = t.app_as(ALICE);
    let run = open_run(
        &app,
        &token,
        json!({ "platform": "instagram", "trigger": "passive",
                "listing": { "kind": "ig_saved", "externalId": null, "name": null },
                "collection": { "mode": "none" } }),
    )
    .await;
    patch_run(
        &app,
        &token,
        run["id"].as_str().unwrap(),
        json!({ "state": "done", "stopReason": "end_of_feed" }),
    )
    .await;
    let notifications = json(send(&session, get("/api/v1/notifications")).await).await;
    assert!(
        notifications["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["kind"] != "sync"),
        "a passive run is silent"
    );
}

// ── Authz (the three per-route rules) ───────────────────────────────────────────

#[tokio::test]
async fn the_routes_need_an_ingest_token_and_the_version() {
    let t = bench();
    let app = t.app();
    let owner_id = owner(&t);
    let run = open_run(&app, &ext_token(&t, &owner_id), auto_saved_run()).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = batch(&run, "scroll", page("A lamp"));

    // No credentials (but past the CSRF guard, as the web app sends it):
    // 401 with WWW-Authenticate: Bearer, because the route takes tokens.
    let response = send(&app, post_json("/api/v1/ingest/batches", body.to_string())).await;
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    assert_eq!(
        problem(response, StatusCode::UNAUTHORIZED).await.code,
        ErrorCode::Unauthorized
    );

    // A token without the ingest scope (a links:create Shortcut token): 403.
    let shortcut = {
        let token = format!("shx_{}", SecretToken::generate().expose());
        t.control()
            .execute(
                "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
             VALUES (?1, ?2, 'shortcut', ?3, 'links:create', ?4)",
                params![
                    new_ulid(),
                    owner_id,
                    hash_token(&token).as_slice(),
                    now_ms()
                ],
            )
            .unwrap();
        token
    };
    let forbidden = send(
        &app,
        bearer(Method::POST, "/api/v1/ingest/batches", &body, &shortcut),
    )
    .await;
    assert_eq!(
        problem(forbidden, StatusCode::FORBIDDEN).await.code,
        ErrorCode::Forbidden
    );

    // An extension token without the version header: 426, on every route but config.
    let token = ext_token(&t, &owner_id);
    let mut outdated = bearer(Method::POST, "/api/v1/ingest/batches", &body, &token);
    outdated.headers_mut().remove(VERSION_HEADER);
    assert_eq!(
        problem(send(&app, outdated).await, StatusCode::UPGRADE_REQUIRED)
            .await
            .code,
        ErrorCode::ExtensionOutdated
    );
    let mut sources = ext(get("/api/v1/extension/sources"), &token, None);
    sources.headers_mut().remove(VERSION_HEADER);
    assert_eq!(
        send(&app, sources).await.status(),
        StatusCode::UPGRADE_REQUIRED
    );
}

#[tokio::test]
async fn one_users_runs_never_reach_another() {
    let t = bench();
    let app = t.app();
    let alice = ext_token(&t, ALICE);
    let bob = ext_token(&t, BOB);
    let run = open_run(&app, &alice, auto_saved_run()).await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // BOB's token ingesting into ALICE's run: the run is not in his library.
    let body = batch(&run, "scroll", page("A lamp"));
    let response = ingest(&app, &bob, &new_ulid(), &body).await;
    assert_eq!(
        problem(response, StatusCode::NOT_FOUND).await.code,
        ErrorCode::SyncRunNotFound
    );

    // BOB's run list never shows ALICE's run.
    let bobs = json(send(&t.app_as(BOB), get("/api/v1/sync-runs")).await).await;
    assert!(bobs["items"].as_array().unwrap().is_empty());
    // A batch whose platform differs from the run's is refused.
    let alice_app = t.app();
    let mismatched = {
        let mut b = batch(&run, "scroll", page("A lamp"));
        b["platform"] = json!("twitter");
        b
    };
    let refused = ingest(&alice_app, &alice, &new_ulid(), &mismatched).await;
    assert_eq!(
        problem(refused, StatusCode::UNPROCESSABLE_ENTITY)
            .await
            .code,
        ErrorCode::ValidationFailed
    );
}

/// An auto-mapped run over Instagram saved (all posts).
fn auto_saved_run() -> Value {
    json!({
        "platform": "instagram",
        "trigger": "manual",
        "listing": { "kind": "ig_saved", "externalId": null, "name": null },
        "collection": { "mode": "auto" },
    })
}

// ── Bench (§6.2: 500-item batch, half new, 6k library, p95 ≤ 300 ms) ─────────────

/// A 500-item page of sequential Instagram posts starting at `start`.
fn seq_page(start: u64) -> Value {
    let items: Vec<Value> = (start..start + 500)
        .map(|pk| {
            json!({
                "id": format!("{pk}_25025320"),
                "postUrl": format!("https://www.instagram.com/p/C{pk:08}/"),
                "text": "A synthetic post for the ingest bench",
                "mediaType": "image",
                "thumbnailUrl": format!("https://scontent.cdninstagram.com/v/{pk}.jpg?oe=7FFFFF00"),
            })
        })
        .collect();
    Value::Array(items)
}

/// §6.2: `POST /ingest/batches` of 500 items, about half already known, on a
/// 6,000-post library, must have a p95 ≤ 300 ms. Prints the numbers; run with
/// `cargo test -p shelfy-server --test sync_ingest -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "bench: builds a 6k-post library; run explicitly"]
async fn bench_ingest_500_on_a_6k_library() {
    let t = bench();
    let app = t.app();
    let token = ext_token(&t, ALICE);
    t.write(ALICE, |tx| {
        support::library::synthetic_library(tx, 6_000, 42).map(|_| ())
    })
    .await;
    let run = open_run(&app, &token, auto_saved_run()).await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Warm the first batch, then time 30 batches overlapping the previous by
    // 250 items (so about half of each is already known).
    ingest(
        &app,
        &token,
        &new_ulid(),
        &batch(&run, "scroll", seq_page(0)),
    )
    .await;
    let mut samples = Vec::new();
    for k in 1..=30_u64 {
        let body = batch(&run, "scroll", seq_page(k * 250));
        let started = std::time::Instant::now();
        let response = ingest(&app, &token, &new_ulid(), &body).await;
        samples.push(started.elapsed());
        assert_eq!(response.status(), StatusCode::OK);
    }
    samples.sort();
    let p = |q: f64| samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)];
    eprintln!(
        "ingest 500/6k: median {:?}, p95 {:?}, max {:?} over {} batches",
        p(0.50),
        p(0.95),
        samples.last().unwrap(),
        samples.len()
    );
}
