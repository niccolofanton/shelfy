//! F21: library tokens through real authentication, without a test user layer.
//! Scopes are independent, device scopes retain their narrow rights, and
//! every library request stays inside the token owner's resources.

mod support;

use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_media::store::{IngestLimits, MediaStore};
use shelfy_server::auth::bearer::Scope;
use shelfy_server::error::ErrorCode;
use shelfy_server::routes;
use shelfy_server::tokens::hash_token;
use support::auth::{
    add_member, control_db, make_sessions_stale, owner, sign_in, spa, with_session,
};
use support::library::{Fixture, NOW, bob_library, fixture};
use support::{TestState, body, get, json, problem, send};

// Deliberately independent of TOKEN_ROUTES/OpenAPI: changing both registries
// must not silently give a library scope additional rights.
const READ: &[(&str, &str)] = &[
    ("GET", "/api/v1/posts"),
    ("GET", "/api/v1/posts/{key}"),
    ("GET", "/api/v1/posts/count"),
    ("POST", "/api/v1/posts/batch-get"),
    ("POST", "/api/v1/posts/lookup"),
    ("GET", "/api/v1/search"),
    ("GET", "/api/v1/stats"),
    ("GET", "/api/v1/collections"),
    ("GET", "/api/v1/trash"),
    ("GET", "/media/{file}"),
];
const WRITE: &[(&str, &str)] = &[
    ("PATCH", "/api/v1/posts/{key}"),
    ("POST", "/api/v1/posts/bulk"),
    ("POST", "/api/v1/collections"),
    ("PATCH", "/api/v1/collections/{id}"),
    ("DELETE", "/api/v1/collections/{id}"),
    ("POST", "/api/v1/collections/{id}/posts"),
    ("DELETE", "/api/v1/collections/{id}/posts/{key}"),
    ("POST", "/api/v1/collections/from-query"),
    ("POST", "/api/v1/trash/restore"),
    ("POST", "/api/v1/trash/empty"),
    ("POST", "/api/v1/links"),
];

#[test]
fn library_scope_rights_are_pinned() {
    for (scope, expected) in [(Scope::LibraryRead, READ), (Scope::LibraryWrite, WRITE)] {
        let actual: BTreeSet<_> = routes::TOKEN_ROUTES
            .iter()
            .filter(|(_, _, scopes, _)| scopes.contains(&scope))
            .map(|(method, path, _, session)| {
                assert!(*session, "library routes still accept sessions");
                (method.as_str(), *path)
            })
            .collect();
        assert_eq!(actual, expected.iter().copied().collect(), "{scope:?}");
    }
}

fn request(method: Method, uri: &str, data: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match data {
        Some(data) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(data.to_string())
        }
        None => Body::empty(),
    };
    builder.body(body).unwrap()
}

fn post(uri: &str, data: Value) -> Request<Body> {
    request(Method::POST, uri, Some(data))
}

fn bearer(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    request
}

async fn mint(t: &TestState, cookie: &str, scopes: Option<&[&str]>) -> Value {
    let mut data = json!({ "kind": "library", "label": "Test client" });
    if let Some(scopes) = scopes {
        data["scopes"] = json!(scopes);
    }
    let response = send(&t.app(), spa(t, post("/api/v1/me/tokens", data), cookie)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json(response).await
}

fn store(t: &TestState, user: &str, seed: u8) -> (String, Vec<u8>) {
    let bytes = [vec![0xff, 0xd8, 0xff, 0xe0], vec![seed; 128]].concat();
    let stored = MediaStore::new(t.data_dir().users_dir())
        .user(user)
        .unwrap()
        .ingest(bytes.as_slice(), IngestLimits::UPLOAD)
        .unwrap()
        .publish()
        .unwrap();
    (format!("/media/{}", stored.name()), bytes)
}

fn read_requests(media: &str) -> Vec<Request<Body>> {
    vec![
        get("/api/v1/posts"),
        get("/api/v1/posts/ig_1001"),
        get("/api/v1/posts/count"),
        post("/api/v1/posts/batch-get", json!({ "keys": ["ig_1001"] })),
        post(
            "/api/v1/posts/lookup",
            json!({ "platform": "instagram", "keys": ["1001"] }),
        ),
        get("/api/v1/search?q=design"),
        get("/api/v1/stats"),
        get("/api/v1/collections"),
        get("/api/v1/trash"),
        get(media),
        request(Method::HEAD, media, None),
    ]
}

fn write_requests(ids: Fixture) -> Vec<Request<Body>> {
    let collection = format!("/api/v1/collections/{}", ids.inspiration);
    vec![
        request(
            Method::PATCH,
            "/api/v1/posts/x_2001",
            Some(json!({ "userNote": "API note", "userTags": ["api"] })),
        ),
        post("/api/v1/collections", json!({ "name": "API folder" })),
        request(
            Method::PATCH,
            &collection,
            Some(json!({ "name": "Renamed" })),
        ),
        post(
            &format!("{collection}/posts"),
            json!({ "selector": { "keys": ["x_2001"] } }),
        ),
        request(Method::DELETE, &format!("{collection}/posts/x_2001"), None),
        post(
            "/api/v1/collections/from-query",
            json!({ "name": "API view", "selector": { "filter": { "platform": "twitter" } } }),
        ),
        request(Method::DELETE, &collection, None),
        post(
            "/api/v1/posts/bulk",
            json!({ "selector": { "keys": ["x_2001"] }, "action": "delete" }),
        ),
        post(
            "/api/v1/trash/restore",
            json!({ "selector": { "keys": ["x_2001"] } }),
        ),
        request(Method::POST, "/api/v1/trash/empty", None),
        post(
            "/api/v1/links",
            json!({ "url": "https://example.test/saved", "tags": ["api"] }),
        ),
    ]
}

#[tokio::test]
async fn minting_is_cookie_only_recent_and_read_only_by_default() {
    let t = TestState::new();
    let app = t.app();
    let owner = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let created = mint(&t, &cookie, None).await;
    assert_eq!(created["apiToken"]["kind"], "library");
    assert_eq!(created["apiToken"]["scopes"], json!(["library:read"]));
    let token = created["token"].as_str().unwrap();
    let id = created["apiToken"]["id"].as_str().unwrap();
    let stored: (String, String, Vec<u8>) = control_db(&t)
        .query_row(
            "SELECT user_id, scopes, token_hash FROM api_tokens WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        stored,
        (owner, "library:read".to_owned(), hash_token(token).to_vec())
    );
    let list = json(send(&app, with_session(get("/api/v1/me/tokens"), &cookie)).await).await;
    assert_eq!(list["items"], json!([created["apiToken"]]));
    assert!(!list.to_string().contains(token));

    for kind in ["extension", "shortcut", "migrate"] {
        let data = json!({ "kind": kind, "scopes": ["library:read"] });
        problem(
            send(&app, spa(&t, post("/api/v1/me/tokens", data), &cookie)).await,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    for scopes in [
        json!([]),
        json!(["lookup"]),
        json!(["library:write", "migrate"]),
    ] {
        problem(
            send(
                &app,
                spa(
                    &t,
                    post(
                        "/api/v1/me/tokens",
                        json!({ "kind": "library", "scopes": scopes }),
                    ),
                    &cookie,
                ),
            )
            .await,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    problem(
        send(
            &app,
            spa(
                &t,
                bearer(
                    post("/api/v1/me/tokens", json!({ "kind": "library" })),
                    token,
                ),
                &cookie,
            ),
        )
        .await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    make_sessions_stale(&t);
    let refused = problem(
        send(
            &app,
            spa(
                &t,
                post("/api/v1/me/tokens", json!({ "kind": "library" })),
                &cookie,
            ),
        )
        .await,
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(refused.code, ErrorCode::ReauthRequired);
}

#[tokio::test]
async fn read_and_write_scopes_are_independent_in_the_real_stack() {
    let t = TestState::new();
    let app = t.app();
    let owner = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let ids = t.write(&owner, |tx| fixture(tx)).await;
    let (media, bytes) = store(&t, &owner, 1);
    let reader = mint(&t, &cookie, None).await;
    let reader = reader["token"].as_str().unwrap();
    let writer = mint(&t, &cookie, Some(&["library:write"])).await;
    assert_eq!(writer["apiToken"]["scopes"], json!(["library:write"]));
    let writer = writer["token"].as_str().unwrap();

    for request in read_requests(&media) {
        let route = format!("{} {}", request.method(), request.uri());
        let response = send(&app, bearer(request, reader)).await;
        assert_eq!(response.status(), StatusCode::OK, "{route}");
    }
    assert_eq!(
        body(send(&app, bearer(get(&media), reader)).await).await,
        bytes
    );
    for with_cookie in [false, true] {
        for request in write_requests(ids) {
            let request = if with_cookie {
                with_session(request, &cookie)
            } else {
                request
            };
            let refused = problem(
                send(&app, bearer(request, reader)).await,
                StatusCode::FORBIDDEN,
            )
            .await;
            assert!(refused.detail.unwrap().contains("library:write"));
        }
    }
    let unchanged = json(send(&app, bearer(get("/api/v1/posts/x_2001"), reader)).await).await;
    assert_eq!(unchanged["userNote"], Value::Null);
    for request in read_requests(&media) {
        let method = request.method().clone();
        let response = send(&app, bearer(request, writer)).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        if method != Method::HEAD {
            assert!(
                problem(response, StatusCode::FORBIDDEN)
                    .await
                    .detail
                    .unwrap()
                    .contains("library:read")
            );
        }
    }
    // No browser Origin or CSRF headers: each permitted bearer write works.
    for request in write_requests(ids) {
        let route = format!("{} {}", request.method(), request.uri());
        let response = send(&app, bearer(request, writer)).await;
        assert!(
            response.status().is_success(),
            "{route}: {}",
            response.status()
        );
    }
    let edited = json(send(&app, bearer(get("/api/v1/posts/x_2001"), reader)).await).await;
    assert_eq!(edited["userNote"], "API note");
    assert_eq!(edited["userTags"], json!(["api"]));
}

#[tokio::test]
async fn library_tokens_keep_resource_isolation_and_cookie_only_boundaries() {
    let t = TestState::new();
    let app = t.app();
    let owner = owner(&t);
    let member = add_member(&t, "member@example.test");
    let cookie = sign_in(&app, &t).await;
    t.write(&owner, |tx| fixture(tx)).await;
    let foreign_collection = t
        .write(&member, |tx| {
            bob_library(tx)?;
            collections::create(
                tx,
                &NewCollection {
                    name: "Second".into(),
                    ..NewCollection::default()
                },
                NOW,
            )?;
            collections::create(
                tx,
                &NewCollection {
                    name: "Foreign only".into(),
                    ..NewCollection::default()
                },
                NOW,
            )
        })
        .await
        .id;
    let (foreign_media, _) = store(&t, &member, 2);
    let token = mint(
        &t,
        &cookie,
        Some(&["library:write", "library:read", "library:read"]),
    )
    .await;
    assert_eq!(
        token["apiToken"]["scopes"],
        json!(["library:read", "library:write"])
    );
    let token = token["token"].as_str().unwrap();
    let foreign = format!("/api/v1/collections/{foreign_collection}");
    for request in [
        get("/api/v1/posts/ig_9001"),
        request(
            Method::PATCH,
            "/api/v1/posts/ig_9001",
            Some(json!({ "userNote": "forged" })),
        ),
        request(Method::PATCH, &foreign, Some(json!({ "name": "forged" }))),
        request(Method::DELETE, &foreign, None),
        post(
            &format!("{foreign}/posts"),
            json!({ "selector": { "keys": ["ig_1001"] } }),
        ),
        request(Method::DELETE, &format!("{foreign}/posts/ig_9001"), None),
        get(&foreign_media),
    ] {
        problem(
            send(&app, bearer(request, token)).await,
            StatusCode::NOT_FOUND,
        )
        .await;
    }
    for request in [
        post("/api/v1/posts/batch-get", json!({ "keys": ["ig_9001"] })),
        post(
            "/api/v1/posts/lookup",
            json!({ "platform": "instagram", "keys": ["9001"] }),
        ),
    ] {
        assert_eq!(
            json(send(&app, bearer(request, token)).await).await["items"],
            json!([])
        );
    }
    for request in [
        get("/api/v1/me"),
        get("/api/v1/me/tokens"),
        get("/api/v1/me/sessions"),
        get("/api/v1/me/settings"),
        get("/api/v1/me/providers"),
        get("/api/v1/jobs"),
        get("/api/v1/ai/queue"),
        post("/api/v1/ai/analyze", json!({})),
        post("/api/v1/ai/queue/retry", json!({})),
        get("/api/v1/exports"),
        post("/api/v1/exports", json!({})),
        get("/api/v1/exports/01J9Z3B8K4QW6TFX0V7G2N5RCE/download"),
        request(
            Method::DELETE,
            "/api/v1/exports/01J9Z3B8K4QW6TFX0V7G2N5RCE",
            None,
        ),
        post("/api/v1/auth/reauth/start", json!({})),
        request(Method::POST, "/api/v1/auth/logout-all", None),
        post("/api/v1/uploads", json!({})),
    ] {
        let route = format!("{} {}", request.method(), request.uri());
        let response = send(&app, spa(&t, bearer(request, token), &cookie)).await;
        // Uploads accept tokens, but only uploads/migrate scopes.
        let expected = if route.ends_with("/uploads") {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNAUTHORIZED
        };
        assert_eq!(response.status(), expected, "{route}");
    }
    let bob = t.app_as(&member);
    let unchanged = json(send(&bob, get("/api/v1/posts/ig_9001")).await).await;
    assert_eq!(unchanged["userNote"], Value::Null);
    let revoke = request(
        Method::DELETE,
        &format!("/api/v1/me/tokens/{}", token_id(&t, token)),
        None,
    );
    assert_eq!(
        send(&app, spa(&t, revoke, &cookie)).await.status(),
        StatusCode::NO_CONTENT
    );
    problem(
        send(&app, bearer(get("/api/v1/posts"), token)).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
}

fn token_id(t: &TestState, token: &str) -> String {
    control_db(t)
        .query_row(
            "SELECT id FROM api_tokens WHERE token_hash = ?1",
            [hash_token(token).as_slice()],
            |r| r.get(0),
        )
        .unwrap()
}
