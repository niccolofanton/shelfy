//! The library writes (P1-03) through the real middleware stack: `PATCH
//! /posts/{key}`, the collection writes, `GET /posts/count`, `POST
//! /posts/batch-get` and `POST /posts/lookup`; the events every write
//! announces; the ETags and cached counts every write invalidates; the
//! search index after every kind of write; and the authz rules every new
//! route ships (P1 lane rule 4: 401 without a session, 404 for another
//! user's resource, API tokens refused).
//!
//! The authz tests sign the owner in for real; the others carry a user
//! through the test-only stand-in for authentication (`TestState::app_as`).
//! Responses are checked against the OpenAPI document.

mod support;

use std::collections::BTreeSet;
use std::sync::mpsc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use shelfy_core::repo::RepoError;
use shelfy_core::search::index;
use shelfy_server::error::ErrorCode;
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::tokens::{SecretToken, hash_token};
use support::auth::{owner, sign_in, spa, with_session};
use support::library::{
    ALICE, BOB, FIXTURE_NEWEST, FIXTURE_TRASHED, Fixture, bob_library, fixture, synthetic_library,
};
use support::sse::{Stream, assert_event_schema, assert_schema};
use support::{TestState, body, from_app, get, json, post_json, problem, send};

/// Alice's fixture library and Bob's library on one server.
async fn two_libraries() -> (TestState, Fixture) {
    let t = TestState::new();
    let ids = t.write(ALICE, |tx| fixture(tx)).await;
    t.write(BOB, |tx| bob_library(tx)).await;
    (t, ids)
}

/// A request with a JSON body, as the web app sends it.
fn with_json(method: Method, uri: &str, value: &Value) -> Request<Body> {
    from_app(
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
    )
}

fn patch(uri: &str, value: &Value) -> Request<Body> {
    with_json(Method::PATCH, uri, value)
}

fn post(uri: &str, value: &Value) -> Request<Body> {
    post_json(uri, value.to_string())
}

fn delete(uri: &str) -> Request<Body> {
    from_app(Request::delete(uri).body(Body::empty()).unwrap())
}

fn get_if_none_match(uri: &str, etag: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::IF_NONE_MATCH, etag)
        .body(Body::empty())
        .unwrap()
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

/// Sends `request` and expects a 422 naming `field`.
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

/// The ETag of `GET uri`.
async fn etag(app: &Router, uri: &str) -> String {
    let response = send(app, get(uri)).await;
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned()
}

/// Checks the search index of `user`'s library against its posts, on a
/// connection of its own (the check builds temporary tables).
fn assert_index_consistent(t: &TestState, user: &str, after: &str) {
    let path = t.state.user_dbs().library_path(user).unwrap();
    let conn = Connection::open(path).unwrap();
    assert_eq!(
        index::verify(&conn).unwrap(),
        Vec::<i64>::new(),
        "the index differs from the posts after {after}"
    );
}

fn keys(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|p| p["key"].as_str().expect("key").to_owned())
        .collect()
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

/// One request of every new route, in an order where each succeeds for the
/// owner of `collection` and `key`.
fn every_route(collection: i64, key: &str) -> Vec<Request<Body>> {
    vec![
        get("/api/v1/posts/count?platform=instagram"),
        post("/api/v1/posts/batch-get", &json!({ "keys": [key] })),
        post(
            "/api/v1/posts/lookup",
            &json!({ "platform": "instagram", "keys": ["C0ffeeAbCdE"] }),
        ),
        patch(&format!("/api/v1/posts/{key}"), &json!({ "userNote": "x" })),
        post("/api/v1/collections", &json!({ "name": "New" })),
        patch(
            &format!("/api/v1/collections/{collection}"),
            &json!({ "name": "Renamed", "position": 0 }),
        ),
        post(
            &format!("/api/v1/collections/{collection}/posts"),
            &json!({ "selector": { "keys": [key] } }),
        ),
        delete(&format!("/api/v1/collections/{collection}/posts/{key}")),
        post(
            "/api/v1/collections/from-query",
            &json!({ "name": "Query", "selector": { "filter": { "platform": "twitter" } } }),
        ),
        delete(&format!("/api/v1/collections/{collection}")),
    ]
}

#[tokio::test]
async fn every_new_route_needs_a_session() {
    let (t, ids) = two_libraries().await;
    let app = t.app();
    for request in every_route(ids.lighting, "ig_1001") {
        let route = format!("{} {}", request.method(), request.uri());
        let refused = problem(send(&app, request).await, StatusCode::UNAUTHORIZED).await;
        assert_eq!(refused.code, ErrorCode::Unauthorized, "{route}");
    }
    // Nothing changed.
    let alice = t.app_as(ALICE);
    let shown = ok(&alice, get("/api/v1/posts/ig_1001")).await;
    assert_eq!(shown["userNote"], Value::Null);
    let folders = ok(&alice, get("/api/v1/collections")).await;
    assert_eq!(folders["items"].as_array().unwrap().len(), 2);
}

/// Lane rule 4: these routes are cookie-only. A valid token with every
/// scope (`lookup` included: it joins `POST /posts/lookup` in P1-17) is
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
        for request in every_route(ids.lighting, "ig_1001") {
            let route = format!("{} {}", request.method(), request.uri());
            let request = if with_cookie {
                with_session(request, &cookie)
            } else {
                request
            };
            // Since P1-17 the lookup takes a `lookup` token (TOKEN_ROUTES).
            if request.uri() == "/api/v1/posts/lookup" {
                let response = send(&app, bearer(request, &token)).await;
                assert_eq!(response.status(), StatusCode::OK, "{route}");
                continue;
            }
            let refused = problem(
                send(&app, bearer(request, &token)).await,
                StatusCode::UNAUTHORIZED,
            )
            .await;
            assert_eq!(refused.code, ErrorCode::Unauthorized, "{route}");
        }
    }
    let unchanged = ok(&app, with_session(get("/api/v1/posts/ig_1001"), &cookie)).await;
    assert_eq!(unchanged["userNote"], Value::Null);

    for request in every_route(ids.lighting, "ig_1001") {
        let route = format!("{} {}", request.method(), request.uri());
        let response = send(&app, spa(&t, request, &cookie)).await;
        assert!(
            response.status().is_success(),
            "{route}: {}",
            response.status()
        );
    }
    let mut forged = with_session(
        patch("/api/v1/posts/ig_1001", &json!({ "userNote": "forged" })),
        &cookie,
    );
    forged.headers_mut().remove("x-shelfy-client");
    let refused = problem(send(&app, forged).await, StatusCode::FORBIDDEN).await;
    assert_eq!(refused.code, ErrorCode::CsrfFailed);
}

#[tokio::test]
async fn another_users_posts_and_collections_are_out_of_reach() {
    let (t, _) = two_libraries().await;
    // Bob's third collection has an id Alice's library does not have.
    let bobs = t
        .write(BOB, |tx| {
            for name in ["b2", "b3"] {
                shelfy_core::repo::collections::create(
                    tx,
                    &shelfy_core::repo::collections::NewCollection {
                        name: name.into(),
                        ..Default::default()
                    },
                    support::library::NOW,
                )?;
            }
            shelfy_core::repo::collections::list(tx)
        })
        .await;
    let bob_collection = bobs.last().unwrap().id;
    let alice = t.app_as(ALICE);
    let folders = ok(&alice, get("/api/v1/collections")).await;
    assert!(
        folders["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["id"] != bob_collection)
    );

    not_found(
        &alice,
        patch("/api/v1/posts/ig_9001", &json!({ "userNote": "mine" })),
    )
    .await;
    not_found(
        &alice,
        patch(
            &format!("/api/v1/collections/{bob_collection}"),
            &json!({ "name": "x" }),
        ),
    )
    .await;
    not_found(
        &alice,
        delete(&format!("/api/v1/collections/{bob_collection}")),
    )
    .await;
    not_found(
        &alice,
        post(
            &format!("/api/v1/collections/{bob_collection}/posts"),
            &json!({ "selector": { "keys": ["ig_1001"] } }),
        ),
    )
    .await;
    not_found(
        &alice,
        delete(&format!(
            "/api/v1/collections/{bob_collection}/posts/ig_1001"
        )),
    )
    .await;
    not_found(&alice, delete("/api/v1/collections/1/posts/ig_9001")).await;
    // Reads find nothing of Bob's.
    let batch = ok(
        &alice,
        post(
            "/api/v1/posts/batch-get",
            &json!({ "keys": ["ig_9001", "x_9002"] }),
        ),
    )
    .await;
    assert_eq!(batch["items"], json!([]));
    let found = ok(
        &alice,
        post(
            "/api/v1/posts/lookup",
            &json!({ "platform": "instagram", "keys": ["9001"] }),
        ),
    )
    .await;
    assert_eq!(found["items"], json!([]));
    // Selecting Bob's keys adds nothing to Alice's folder.
    let added = ok(
        &alice,
        post(
            "/api/v1/collections/2/posts",
            &json!({ "selector": { "keys": ["ig_9001"] } }),
        ),
    )
    .await;
    assert_eq!(added["added"], 0);

    // Bob's library is untouched.
    let bob = t.app_as(BOB);
    let shown = ok(&bob, get("/api/v1/posts/ig_9001")).await;
    assert_eq!(shown["userNote"], Value::Null);
    assert_eq!(shown["collectionIds"], json!([1]));
    let folders = ok(&bob, get("/api/v1/collections")).await;
    assert_eq!(folders["items"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn patch_writes_the_note_the_tags_and_a_manual_ai_edit() {
    let (t, _) = two_libraries().await;
    let app = t.app_as(ALICE);
    let uri = "/api/v1/posts/x_2001";

    let note = ok(
        &app,
        patch(uri, &json!({ "userNote": "Brutalist type specimens" })),
    )
    .await;
    assert_schema(&note, "PostDetail");
    assert_eq!(note["userNote"], "Brutalist type specimens");
    assert_eq!(note["userTags"], json!(["design"]), "the tags are kept");
    assert_index_consistent(&t, ALICE, "a note");
    let found = ok(&app, get("/api/v1/posts?q=brutalist")).await;
    assert_eq!(keys(&found), ["x_2001"]);

    let tags = ok(
        &app,
        patch(uri, &json!({ "userTags": ["Type", " type ", "Grid"] })),
    )
    .await;
    assert_eq!(
        tags["userTags"],
        json!(["Type", " type ", "Grid"]),
        "stored as given"
    );
    let manual: Vec<&Value> = tags["tags"].as_array().unwrap().iter().collect();
    assert_eq!(
        manual,
        [
            &json!({ "tag": "Grid", "norm": "grid", "source": "manual", "tier": null }),
            &json!({ "tag": "Type", "norm": "type", "source": "manual", "tier": null }),
        ]
    );
    assert_index_consistent(&t, ALICE, "manual tags");

    // An earlier analysis that failed, then the manual edit: the layer is
    // the user's, with no provider, error or schema version left over.
    t.write(ALICE, |tx| {
        tx.execute(
            "UPDATE posts SET ai_status = 'error', ai_provider = 'openai', ai_model = 'model-a',
                              ai_schema_version = 2, ai_error = 'timeout'
             WHERE key = 'x_2001'",
            [],
        )
        .map_err(Into::into)
    })
    .await;
    let before = now_ms();
    let edit = ok(
        &app,
        patch(
            uri,
            &json!({
                "aiDescription": "A typographic grid poster",
                "aiTags": ["type", "Poster"],
                "aiSaveReason": "reference",
            }),
        ),
    )
    .await;
    assert_eq!(edit["aiStatus"], "done");
    assert_eq!(edit["aiModel"], "manual");
    for field in ["aiProvider", "aiError", "aiSchemaVersion"] {
        assert_eq!(edit[field], Value::Null, "{field}");
    }
    assert!(edit["aiAnalyzedAt"].as_i64().unwrap() >= before);
    assert_eq!(edit["aiDescription"], "A typographic grid poster");
    assert_eq!(edit["aiTags"], json!(["type", "Poster"]));
    assert_eq!(edit["aiSaveReason"], "reference");
    assert_eq!(
        edit["userNote"], "Brutalist type specimens",
        "the user layer is kept"
    );
    // §1.2 #3: the AI tag "type" and the manual tag "Type" are two tags.
    let rows: Vec<(String, String)> = edit["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["norm"].as_str().unwrap().to_owned(),
                t["source"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("poster".to_owned(), "ai".to_owned()),
            ("type".to_owned(), "ai".to_owned()),
            ("grid".to_owned(), "manual".to_owned()),
            ("type".to_owned(), "manual".to_owned()),
        ]
    );
    assert_index_consistent(&t, ALICE, "a manual AI edit");
    let tagged = ok(&app, get("/api/v1/posts?aiTagged=yes&tag=type")).await;
    assert_eq!(keys(&tagged), ["x_2001"]);

    // Clearing: the AI tags go, the manual ones stay; `null` clears the note.
    let cleared = ok(
        &app,
        patch(uri, &json!({ "aiTags": null, "userNote": null })),
    )
    .await;
    assert_eq!(cleared["aiTags"], json!([]));
    assert_eq!(cleared["userNote"], Value::Null);
    assert!(
        cleared["tags"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["source"] == "manual")
    );
    let cleared = ok(&app, patch(uri, &json!({ "userTags": null }))).await;
    assert_eq!(cleared["userTags"], json!([]));
    assert_eq!(cleared["tags"], json!([]));
    assert_index_consistent(&t, ALICE, "clearing fields");

    // A trashed post can be edited; it stays out of the index.
    let trashed = ok(
        &app,
        patch(
            &format!("/api/v1/posts/{FIXTURE_TRASHED}"),
            &json!({ "userNote": "still here" }),
        ),
    )
    .await;
    assert_eq!(trashed["userNote"], "still here");
    assert_index_consistent(&t, ALICE, "an edit in the trash");
    assert!(keys(&ok(&app, get("/api/v1/posts?q=still")).await).is_empty());
}

#[tokio::test]
async fn bad_patches_are_problems() {
    let (t, _) = two_libraries().await;
    let app = t.app_as(ALICE);
    let uri = "/api/v1/posts/x_2001";
    not_found(
        &app,
        patch("/api/v1/posts/ig_404", &json!({ "userNote": "x" })),
    )
    .await;
    let long_key = format!("/api/v1/posts/{}", "k".repeat(201));
    not_found(&app, patch(&long_key, &json!({ "userNote": "x" }))).await;
    invalid(
        &app,
        patch(uri, &json!({ "userNote": "n".repeat(20_001) })),
        "userNote",
    )
    .await;
    invalid(
        &app,
        patch(uri, &json!({ "userTags": vec!["t"; 101] })),
        "userTags",
    )
    .await;
    invalid(
        &app,
        patch(uri, &json!({ "aiCategory": "c".repeat(201) })),
        "aiCategory",
    )
    .await;
    for body in [
        json!({ "note": "the desktop's name" }),
        json!({ "aiStatus": "done" }),
        json!({ "userTags": "not a list" }),
    ] {
        let refused = problem(
            send(&app, patch(uri, &body)).await,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
        assert_eq!(refused.code, ErrorCode::ValidationFailed, "{body}");
    }
    let unchanged = ok(&app, get(uri)).await;
    assert_eq!(unchanged["userNote"], Value::Null);
    assert_eq!(unchanged["aiStatus"], Value::Null);
}

#[tokio::test]
async fn collections_are_created_renamed_recolored_and_reordered() {
    let (t, ids) = two_libraries().await;
    let app = t.app_as(ALICE);
    let created = call(
        &app,
        post(
            "/api/v1/collections",
            &json!({ "name": "  Kitchens  ", "color": "#ABC" }),
        ),
        StatusCode::CREATED,
    )
    .await;
    assert_schema(&created, "Collection");
    assert_eq!(created["name"], "Kitchens");
    assert_eq!(created["color"], "#abc");
    assert_eq!(created["count"], 0);
    assert_eq!(created["platform"], Value::Null);
    let kitchens = created["id"].as_i64().unwrap();
    let names = |list: &Value| -> Vec<String> {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let list = ok(&app, get("/api/v1/collections")).await;
    assert_eq!(names(&list), ["Lighting", "Inspiration", "Kitchens"]);

    let moved = ok(
        &app,
        patch(
            &format!("/api/v1/collections/{kitchens}"),
            &json!({ "position": 0 }),
        ),
    )
    .await;
    assert_eq!(moved["position"], 0);
    let list = ok(&app, get("/api/v1/collections")).await;
    assert_eq!(names(&list), ["Kitchens", "Lighting", "Inspiration"]);
    let positions: Vec<i64> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["position"].as_i64().unwrap())
        .collect();
    assert_eq!(positions, [0, 1, 2]);

    let renamed = ok(
        &app,
        patch(
            &format!("/api/v1/collections/{}", ids.lighting),
            &json!({ "name": "Lamps", "color": "#123456", "position": 99 }),
        ),
    )
    .await;
    assert_eq!(renamed["name"], "Lamps");
    assert_eq!(renamed["color"], "#123456");
    assert_eq!(
        renamed["platform"], "instagram",
        "a platform folder keeps its link"
    );
    assert_eq!(renamed["externalId"], "17900000000000001");
    let list = ok(&app, get("/api/v1/collections")).await;
    assert_eq!(names(&list), ["Kitchens", "Inspiration", "Lamps"]);
    // An empty change is no change.
    let same = ok(
        &app,
        patch(&format!("/api/v1/collections/{kitchens}"), &json!({})),
    )
    .await;
    assert_eq!(same, moved);

    invalid(
        &app,
        post("/api/v1/collections", &json!({ "name": "  " })),
        "name",
    )
    .await;
    invalid(
        &app,
        post("/api/v1/collections", &json!({ "name": "n".repeat(201) })),
        "name",
    )
    .await;
    invalid(
        &app,
        post(
            "/api/v1/collections",
            &json!({ "name": "x", "color": "red" }),
        ),
        "color",
    )
    .await;
    let uri = format!("/api/v1/collections/{kitchens}");
    invalid(&app, patch(&uri, &json!({ "name": "" })), "name").await;
    invalid(&app, patch(&uri, &json!({ "color": "#12" })), "color").await;
    let negative = problem(
        send(&app, patch(&uri, &json!({ "position": -1 }))).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(negative.code, ErrorCode::ValidationFailed);
    not_found(
        &app,
        patch("/api/v1/collections/404", &json!({ "name": "x" })),
    )
    .await;
    let bad_id = problem(
        send(
            &app,
            patch("/api/v1/collections/abc", &json!({ "name": "x" })),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(bad_id.code, ErrorCode::BadRequest);
    assert_index_consistent(&t, ALICE, "collection metadata writes");
}

#[tokio::test]
async fn posts_join_and_leave_collections_by_selector() {
    let t = TestState::new();
    let all = t.write(ALICE, |tx| synthetic_library(tx, 400, 5)).await;
    let ids = t.write(ALICE, |tx| fixture(tx)).await;
    let app = t.app_as(ALICE);
    let folder = ids.inspiration;
    let posts_uri = format!("/api/v1/collections/{folder}/posts");

    // By keys: unknown and trashed keys are skipped, members are not counted.
    let added = ok(
        &app,
        post(
            &posts_uri,
            &json!({ "selector": { "keys": ["x_2001", "ig_1002", "pin_3001", FIXTURE_TRASHED, "ig_404"] } }),
        ),
    )
    .await;
    assert_schema(&added, "CollectionPostsAdded");
    assert_eq!(added["added"], 2);
    assert_eq!(added["collection"]["count"], 3);

    // By filter, minus exceptions: every matching post, past any page size.
    let count = ok(&app, get("/api/v1/posts/count?platform=twitter")).await["total"]
        .as_u64()
        .unwrap();
    assert!(count > 100, "{count}");
    let added = ok(
        &app,
        post(
            &posts_uri,
            &json!({ "selector": { "filter": { "platform": "twitter" }, "exceptKeys": ["x_2001", "x_2002"] } }),
        ),
    )
    .await;
    // x_2001 was a member already; x_2002 is an exception.
    assert_eq!(added["added"], count - 2);
    let members = ok(
        &app,
        get(&format!(
            "/api/v1/posts/count?collection={folder}&platform=twitter"
        )),
    )
    .await;
    assert_eq!(members["total"], count - 1);
    let folder_list = ok(
        &app,
        get(&format!(
            "/api/v1/posts?collection={folder}&includeTotal=true"
        )),
    )
    .await;
    assert_eq!(folder_list["total"], added["collection"]["count"]);

    // The trash selects only with `trash`, and trashed posts never join.
    let added = ok(
        &app,
        post(
            &posts_uri,
            &json!({ "selector": { "filter": { "trash": true } } }),
        ),
    )
    .await;
    assert_eq!(added["added"], 0);

    // Taking one out (§1.2 #12), twice.
    let removed = ok(&app, delete(&format!("{posts_uri}/x_2001"))).await;
    assert_schema(&removed, "CollectionPostRemoved");
    assert_eq!(removed["removed"], true);
    let again = ok(&app, delete(&format!("{posts_uri}/x_2001"))).await;
    assert_eq!(again["removed"], false);
    assert_eq!(again["collection"]["count"], removed["collection"]["count"]);
    let shown = ok(&app, get("/api/v1/posts/x_2001")).await;
    assert_eq!(shown["collectionIds"], json!([]));
    not_found(&app, delete(&format!("{posts_uri}/ig_404"))).await;
    not_found(&app, delete("/api/v1/collections/404/posts/x_2001")).await;

    // Selector problems name their field.
    let too_many: Vec<&String> = all.iter().cycle().take(501).collect();
    invalid(
        &app,
        post(&posts_uri, &json!({ "selector": { "keys": too_many } })),
        "selector.keys",
    )
    .await;
    invalid(
        &app,
        post(&posts_uri, &json!({ "selector": {} })),
        "selector",
    )
    .await;
    invalid(
        &app,
        post(
            &posts_uri,
            &json!({ "selector": { "keys": [], "filter": {} } }),
        ),
        "selector",
    )
    .await;
    invalid(
        &app,
        post(
            &posts_uri,
            &json!({ "selector": { "filter": {}, "exceptKeys": vec!["k"; 1001] } }),
        ),
        "selector.exceptKeys",
    )
    .await;
    invalid(
        &app,
        post(
            &posts_uri,
            &json!({ "selector": { "filter": { "q": "q".repeat(501) } } }),
        ),
        "selector.filter.q",
    )
    .await;
    not_found(
        &app,
        post(
            "/api/v1/collections/404/posts",
            &json!({ "selector": { "keys": ["x_2001"] } }),
        ),
    )
    .await;
    assert_index_consistent(&t, ALICE, "membership changes");
}

#[tokio::test]
async fn a_collection_is_made_from_a_query_and_deleted_without_its_posts() {
    let (t, ids) = two_libraries().await;
    let app = t.app_as(ALICE);
    let made = call(
        &app,
        post(
            "/api/v1/collections/from-query",
            &json!({ "name": "Design", "color": "#00ff00", "selector": { "filter": { "q": "design" } } }),
        ),
        StatusCode::CREATED,
    )
    .await;
    assert_schema(&made, "CollectionPostsAdded");
    let expected = ok(&app, get("/api/v1/posts?q=design&sort=newest")).await;
    assert_eq!(made["added"], keys(&expected).len());
    assert_eq!(made["collection"]["name"], "Design");
    assert_eq!(made["collection"]["count"], made["added"]);
    let id = made["collection"]["id"].as_i64().unwrap();
    let members = ok(
        &app,
        get(&format!("/api/v1/posts?collection={id}&sort=newest")),
    )
    .await;
    assert_eq!(keys(&members), keys(&expected));
    invalid(
        &app,
        post(
            "/api/v1/collections/from-query",
            &json!({ "name": "", "selector": { "keys": [] } }),
        ),
        "name",
    )
    .await;
    invalid(
        &app,
        post(
            "/api/v1/collections/from-query",
            &json!({ "name": "x", "selector": {} }),
        ),
        "selector",
    )
    .await;

    // Deleting keeps the posts, out of the collection.
    let deleted = ok(
        &app,
        delete(&format!("/api/v1/collections/{}", ids.lighting)),
    )
    .await;
    assert_schema(&deleted, "CollectionDeleted");
    assert_eq!(deleted, json!({ "trashed": 0 }));
    let shown = ok(&app, get("/api/v1/posts/ig_1001")).await;
    assert_eq!(shown["deletedAt"], Value::Null);
    assert_eq!(shown["collectionIds"], json!([]));
    let shown = ok(&app, get("/api/v1/posts/x_2001")).await;
    assert_eq!(
        shown["collectionIds"],
        json!([id]),
        "other memberships stay"
    );
    let page = ok(&app, get("/api/v1/posts")).await;
    assert_eq!(keys(&page), FIXTURE_NEWEST);
    not_found(
        &app,
        delete(&format!("/api/v1/collections/{}", ids.lighting)),
    )
    .await;
    let with_posts = problem(
        send(
            &app,
            delete(&format!("/api/v1/collections/{id}?mode=withPosts")),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(
        with_posts.code,
        ErrorCode::BadRequest,
        "withPosts comes in P1-11"
    );
    ok(
        &app,
        delete(&format!("/api/v1/collections/{id}?mode=label")),
    )
    .await;
    assert_index_consistent(&t, ALICE, "deleting collections");
}

#[tokio::test]
async fn counts_batches_and_lookups_answer_within_their_caps() {
    let (t, ids) = two_libraries().await;
    let app = t.app_as(ALICE);

    let count = ok(&app, get("/api/v1/posts/count")).await;
    assert_schema(&count, "PostCount");
    assert_eq!(count, json!({ "total": FIXTURE_NEWEST.len() }));
    for (query, total) in [
        ("platform=instagram", 2),
        ("mediaType=video&mediaType=text", 2),
        ("trash=1", 1),
        ("trash=true", 1),
        ("q=design", 3),
        (&format!("collection={}", ids.inspiration), 1),
        ("platform=manual", 1),
    ] {
        let page = ok(
            &app,
            get(&format!("/api/v1/posts?{query}&includeTotal=true")),
        )
        .await;
        assert_eq!(page["total"], total, "{query}");
        let count = ok(&app, get(&format!("/api/v1/posts/count?{query}"))).await;
        assert_eq!(count["total"], total, "{query}");
    }
    let problem_of = |query: &str| format!("/api/v1/posts/count?{query}");
    invalid(
        &app,
        get(&problem_of(&format!("q={}", "q".repeat(501)))),
        "q",
    )
    .await;
    let bad = problem(
        send(&app, get(&problem_of("platform=tiktok"))).await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(bad.code, ErrorCode::BadRequest);

    let batch = ok(
        &app,
        post(
            "/api/v1/posts/batch-get",
            &json!({ "keys": ["pin_3001", "ig_404", FIXTURE_TRASHED, "pin_3001", "x_2001"] }),
        ),
    )
    .await;
    assert_schema(&batch, "PostBatch");
    assert_eq!(keys(&batch), ["pin_3001", FIXTURE_TRASHED, "x_2001"]);
    let many: Vec<String> = (0..201).map(|n| format!("ig_{n}")).collect();
    invalid(
        &app,
        post("/api/v1/posts/batch-get", &json!({ "keys": many })),
        "keys",
    )
    .await;
    invalid(
        &app,
        post(
            "/api/v1/posts/batch-get",
            &json!({ "keys": ["k".repeat(201)] }),
        ),
        "keys",
    )
    .await;

    let found = ok(
        &app,
        post(
            "/api/v1/posts/lookup",
            &json!({ "platform": "instagram", "keys": ["C0ffeeAbCdE", "1002", "1003", "404", "2001"] }),
        ),
    )
    .await;
    assert_schema(&found, "LookupResult");
    assert_eq!(
        found["items"],
        json!([
            { "key": "C0ffeeAbCdE", "postKey": "ig_1001", "trashed": false },
            { "key": "1002", "postKey": "ig_1002", "trashed": false },
            { "key": "1003", "postKey": FIXTURE_TRASHED, "trashed": true },
        ])
    );
    let tweets = ok(
        &app,
        post(
            "/api/v1/posts/lookup",
            &json!({ "platform": "twitter", "keys": ["2001", "2002"] }),
        ),
    )
    .await;
    assert_eq!(tweets["items"].as_array().unwrap().len(), 2);
    let many: Vec<String> = (0..1001).map(|n| n.to_string()).collect();
    invalid(
        &app,
        post(
            "/api/v1/posts/lookup",
            &json!({ "platform": "twitter", "keys": many }),
        ),
        "keys",
    )
    .await;
    let web = problem(
        send(
            &app,
            post(
                "/api/v1/posts/lookup",
                &json!({ "platform": "web", "keys": [] }),
            ),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(web.code, ErrorCode::ValidationFailed);
}

/// A count or a selection covers what the list shows: the query of `GET
/// /posts/count` and the selector's `filter` (`FilterParams`) take exactly
/// the filters of `GET /posts`, under the same names.
#[test]
fn counts_and_selectors_take_every_list_filter() {
    let doc = serde_json::to_value(shelfy_server::routes::openapi()).unwrap();
    let params = |path: &str, skip: &[&str]| -> BTreeSet<String> {
        doc["paths"][path]["get"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_owned())
            .filter(|name| !skip.contains(&name.as_str()))
            .collect()
    };
    let list = params(
        "/api/v1/posts",
        &["sort", "limit", "cursor", "includeTotal", "If-None-Match"],
    );
    let count = params("/api/v1/posts/count", &["If-None-Match"]);
    let filter: BTreeSet<String> = doc["components"]["schemas"]["FilterParams"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(count, list);
    assert_eq!(filter, list);
    assert!(list.len() >= 17, "{list:?}");
}

/// The next event of `stream`, past heartbeats (paused time may reach one
/// while a write runs on the blocking pool).
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

/// The `posts.changed` and `stats.changed` of one write, in any order.
async fn announced(stream: &mut Stream) -> Value {
    let mut posts = None;
    let mut stats = false;
    while posts.is_none() || !stats {
        match next_event(stream).await {
            (name, data) if name == "posts.changed" && posts.is_none() => posts = Some(data),
            (name, data) if name == "stats.changed" && !stats => {
                assert_eq!(data, json!({}));
                stats = true;
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
    posts.unwrap()
}

/// Keys of a `posts.changed`, sorted (`null` for "any").
fn changed_keys(event: &Value) -> Option<Vec<String>> {
    assert_eq!(event["reason"], "edit");
    event["keys"].as_array().map(|keys| {
        let mut keys: Vec<String> = keys
            .iter()
            .map(|k| k.as_str().unwrap().to_owned())
            .collect();
        keys.sort();
        keys
    })
}

#[tokio::test(start_paused = true)]
async fn every_write_announces_its_posts_and_the_stats() {
    let (t, ids) = two_libraries().await;
    let app = t.app_as(ALICE);
    let mut stream = Stream::connect(&app, "/api/v1/events", &[]).await;
    stream.hello().await;
    let mut bobs = Stream::connect(&t.app_as(BOB), "/api/v1/events", &[]).await;
    bobs.hello().await;
    let pause = || tokio::time::advance(std::time::Duration::from_secs(3));
    let some = |keys: &[&str]| Some(keys.iter().map(|k| (*k).to_owned()).collect::<Vec<_>>());

    ok(
        &app,
        patch("/api/v1/posts/ig_1001", &json!({ "userNote": "n" })),
    )
    .await;
    assert_eq!(
        changed_keys(&announced(&mut stream).await),
        some(&["ig_1001"])
    );
    pause().await;
    call(
        &app,
        post("/api/v1/collections", &json!({ "name": "c" })),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        changed_keys(&announced(&mut stream).await),
        some(&[]),
        "no post changed"
    );
    pause().await;
    ok(
        &app,
        post(
            &format!("/api/v1/collections/{}/posts", ids.inspiration),
            &json!({ "selector": { "filter": { "platform": "twitter" } } }),
        ),
    )
    .await;
    assert_eq!(
        changed_keys(&announced(&mut stream).await),
        some(&["x_2001", "x_2002"])
    );
    pause().await;
    ok(
        &app,
        delete(&format!("/api/v1/collections/{}", ids.inspiration)),
    )
    .await;
    assert_eq!(
        changed_keys(&announced(&mut stream).await),
        some(&["pin_3001", "x_2001", "x_2002"]),
        "the members of the deleted collection"
    );
    pause().await;
    // A write that changes nothing announces nothing, an edit that repeats
    // the stored values included: the next event is the next real change.
    ok(&app, patch("/api/v1/posts/ig_1001", &json!({}))).await;
    ok(
        &app,
        patch(
            "/api/v1/posts/m_01J9Z3B8K4QW6TFX0V7G2N5RCE",
            &json!({ "userNote": "Brief from the client", "userTags": ["work"] }),
        ),
    )
    .await;
    ok(
        &app,
        delete(&format!(
            "/api/v1/collections/{}/posts/ig_1001",
            ids.lighting
        )),
    )
    .await;
    assert_eq!(
        changed_keys(&announced(&mut stream).await),
        some(&["ig_1001"])
    );

    // Bob heard none of it: his first event is his own write.
    ok(
        &t.app_as(BOB),
        patch("/api/v1/posts/ig_9001", &json!({ "aiTags": ["lamp"] })),
    )
    .await;
    assert_eq!(
        changed_keys(&announced(&mut bobs).await),
        some(&["ig_9001"])
    );
}

/// While a library is locked for maintenance (P1-12) writes answer 423
/// `user_locked` and change nothing; after the unlock, no ETag from before
/// matches (the library may have been replaced), even with a handle from
/// before the lock still held.
#[tokio::test]
async fn writes_wait_for_a_locked_library_and_its_etags_start_over() {
    let (t, _) = two_libraries().await;
    let app = t.app_as(ALICE);
    let uri = "/api/v1/stats";
    let before = etag(&app, uri).await;
    let held = t.state.user_db(ALICE).await.unwrap();
    let users = t.data_dir().users_dir();
    assert!(shelfy_core::db::lock_library(&users, ALICE, "restore").unwrap());
    for request in [
        patch("/api/v1/posts/ig_1001", &json!({ "userNote": "x" })),
        post("/api/v1/collections", &json!({ "name": "x" })),
        post(
            "/api/v1/collections/2/posts",
            &json!({ "selector": { "keys": ["x_2001"] } }),
        ),
    ] {
        let route = format!("{} {}", request.method(), request.uri());
        let locked = problem(send(&app, request).await, StatusCode::LOCKED).await;
        assert_eq!(locked.code, ErrorCode::UserLocked, "{route}");
    }
    assert!(shelfy_core::db::unlock_library(&users, ALICE).unwrap());
    let response = send(&app, get_if_none_match(uri, &before)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a new generation after the lock"
    );
    let shown = ok(&app, get("/api/v1/posts/ig_1001")).await;
    assert_eq!(shown["userNote"], Value::Null, "nothing was written");
    drop(held);
}

/// The *From T11* note, through the API: a request that still holds a
/// handle the cache has dropped for idleness writes through it; the next
/// request reads another handle, and its ETags still see the write.
#[tokio::test]
async fn a_write_through_an_evicted_handle_still_moves_the_etags() {
    let t = TestState::with_config(|config| {
        config.user_db_cache.time_to_idle = std::time::Duration::from_millis(50);
    });
    t.write(ALICE, |tx| fixture(tx)).await;
    let app = t.app_as(ALICE);
    let held = t.state.user_db(ALICE).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    t.state.user_dbs().run_maintenance();
    assert!(
        t.state.user_dbs().get_if_present(ALICE).is_none(),
        "evicted"
    );
    let uri = "/api/v1/collections";
    let tag = etag(&app, uri).await;
    held.write(|tx| {
        shelfy_core::repo::collections::create(
            tx,
            &shelfy_core::repo::collections::NewCollection {
                name: "late".into(),
                ..Default::default()
            },
            support::library::NOW,
        )
    })
    .unwrap();
    let response = send(&app, get_if_none_match(uri, &tag)).await;
    assert_eq!(response.status(), StatusCode::OK, "no stale 304");
    let list = json(response).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 3);
}

/// Every write moves the generation: the list, stats, collections and count
/// ETags all change, cached counts are recomputed, and a write that changed
/// nothing keeps them.
#[tokio::test]
async fn every_write_invalidates_the_etags_and_cached_counts() {
    let (t, ids) = two_libraries().await;
    let app = t.app_as(ALICE);
    let views = [
        "/api/v1/posts?platform=instagram",
        "/api/v1/stats",
        "/api/v1/collections",
        "/api/v1/posts/count?collection=2",
    ];
    let folder = ids.inspiration;
    let writes: Vec<(&str, Request<Body>, StatusCode)> = vec![
        (
            "a note",
            patch("/api/v1/posts/ig_1001", &json!({ "userNote": "n" })),
            StatusCode::OK,
        ),
        (
            "an AI edit",
            patch("/api/v1/posts/ig_1001", &json!({ "aiTags": ["x"] })),
            StatusCode::OK,
        ),
        (
            "a new collection",
            post("/api/v1/collections", &json!({ "name": "c" })),
            StatusCode::CREATED,
        ),
        (
            "a rename",
            patch(
                &format!("/api/v1/collections/{folder}"),
                &json!({ "name": "r" }),
            ),
            StatusCode::OK,
        ),
        (
            "an addition",
            post(
                &format!("/api/v1/collections/{folder}/posts"),
                &json!({ "selector": { "keys": ["x_2001"] } }),
            ),
            StatusCode::OK,
        ),
        (
            "a removal",
            delete(&format!("/api/v1/collections/{folder}/posts/x_2001")),
            StatusCode::OK,
        ),
        (
            "a query collection",
            post(
                "/api/v1/collections/from-query",
                &json!({ "name": "q", "selector": { "keys": [] } }),
            ),
            StatusCode::CREATED,
        ),
        (
            "a deletion",
            delete(&format!("/api/v1/collections/{}", ids.lighting)),
            StatusCode::OK,
        ),
    ];
    for (what, request, status) in writes {
        let mut etags = Vec::new();
        for uri in views {
            let tag = etag(&app, uri).await;
            let response = send(&app, get_if_none_match(uri, &tag)).await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_MODIFIED,
                "{uri} before {what}"
            );
            assert!(body(response).await.is_empty());
            etags.push(tag);
        }
        call(&app, request, status).await;
        for (uri, tag) in views.iter().zip(&etags) {
            let response = send(&app, get_if_none_match(uri, tag)).await;
            assert_eq!(response.status(), StatusCode::OK, "{uri} after {what}");
            assert_ne!(response.headers()[header::ETAG], tag.as_str(), "{uri}");
        }
    }

    // The cached count follows the library.
    let count = |n: u64| json!({ "total": n });
    assert_eq!(
        ok(&app, get("/api/v1/posts/count?collection=2")).await,
        count(1)
    );
    ok(
        &app,
        post(
            "/api/v1/collections/2/posts",
            &json!({ "selector": { "keys": ["x_2002", "ig_1002"] } }),
        ),
    )
    .await;
    assert_eq!(
        ok(&app, get("/api/v1/posts/count?collection=2")).await,
        count(3)
    );

    // Writes that change nothing keep every ETag.
    let mut tags = Vec::new();
    for uri in views {
        tags.push(etag(&app, uri).await);
    }
    ok(&app, patch("/api/v1/posts/ig_1001", &json!({}))).await;
    ok(&app, patch("/api/v1/collections/2", &json!({}))).await;
    ok(
        &app,
        post(
            "/api/v1/collections/2/posts",
            &json!({ "selector": { "keys": ["x_2002"] } }),
        ),
    )
    .await;
    let removed = ok(&app, delete("/api/v1/collections/2/posts/x_2001")).await;
    assert_eq!(removed["removed"], false);
    for (uri, tag) in views.iter().zip(&tags) {
        let response = send(&app, get_if_none_match(uri, tag)).await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_MODIFIED,
            "{uri} after no-op writes"
        );
    }
}

/// F6 (P1-03 review, L4): a PATCH that sends the values the post already
/// has, the note and tags or a manual AI edit, changes no row: every ETag
/// still answers 304, and nothing is announced (see
/// `every_write_announces_its_posts_and_the_stats`).
#[tokio::test]
async fn a_patch_that_repeats_the_stored_values_keeps_every_etag() {
    let (t, _) = two_libraries().await;
    let app = t.app_as(ALICE);
    let uri = "/api/v1/posts/x_2001";
    let views = [uri, "/api/v1/stats", "/api/v1/posts?platform=twitter"];
    for body in [
        json!({ "userNote": "same", "userTags": ["Lamp", "a"] }),
        json!({ "aiDescription": "A lamp", "aiTags": ["lamp"], "aiCategory": null }),
    ] {
        let first = ok(&app, patch(uri, &body)).await;
        let mut tags = Vec::new();
        for view in views {
            tags.push(etag(&app, view).await);
        }
        let again = ok(&app, patch(uri, &body)).await;
        assert_eq!(again, first, "the same post after repeating {body}");
        for (view, tag) in views.iter().zip(&tags) {
            let response = send(&app, get_if_none_match(view, tag)).await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_MODIFIED,
                "{view} after repeating {body}"
            );
        }
    }
    assert_index_consistent(&t, ALICE, "repeated patches");
}

/// F6 (P1-03 review, L2): a write whose request is gone (the 30 s time limit
/// answered 504, or the client closed the connection) while the write waited
/// for the writer still commits, and is announced: the announcement is made
/// by the write itself, not by the request after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_whose_request_was_dropped_is_still_announced() {
    let t = TestState::new();
    t.write(ALICE, |tx| fixture(tx)).await;
    let app = t.app_as(ALICE);
    let mut stream = Stream::connect(&app, "/api/v1/events", &[]).await;
    stream.hello().await;

    // Another write (a job chunk, a large from-query) holds the writer until
    // the request is gone.
    let db = t.state.user_db(ALICE).await.unwrap();
    let (held, holding) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let blocker = std::thread::spawn(move || {
        db.write(|_tx| {
            held.send(()).unwrap();
            released.recv().unwrap();
            Ok::<_, RepoError>(())
        })
        .unwrap();
    });
    tokio::task::spawn_blocking(move || holding.recv().unwrap())
        .await
        .unwrap();
    let request = send(
        &app,
        patch("/api/v1/posts/x_2001", &json!({ "userNote": "applied" })),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(250), request)
            .await
            .is_err(),
        "the request is dropped while its write waits for the writer"
    );
    release.send(()).unwrap();
    tokio::task::spawn_blocking(move || blocker.join().unwrap())
        .await
        .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(5), announced(&mut stream))
        .await
        .expect("the committed write is announced");
    assert_eq!(changed_keys(&event), Some(vec!["x_2001".to_owned()]));
    let shown = ok(&app, get("/api/v1/posts/x_2001")).await;
    assert_eq!(shown["userNote"], "applied", "the write committed");
}
