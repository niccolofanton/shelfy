//! The platform routes of P1-01 through the real middleware stack:
//! notifications (pages, marking read, live `notification` events), client
//! error reports and the version, with the authz rules every new route ships
//! (P1 lane rule 4). What a client error report logs is checked in
//! `client_error_logs.rs`, a binary of its own: a scoped log subscriber can
//! miss spans that tests on other threads register at the same time.
//!
//! The authz tests sign the owner in for real (T10's session cookie, the
//! CSRF guard, API tokens); the others carry a user through the test-only
//! stand-in for authentication (`TestState::app_as`).

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use shelfy_core::repo::notifications::NewNotification;
use shelfy_server::auth::cookie::SESSION_COOKIE;
use shelfy_server::auth::csrf::{CLIENT_HEADER, CLIENT_WEB};
use shelfy_server::error::ErrorCode;
use shelfy_server::events::notify;
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::tokens::{SecretToken, hash_token};
use support::auth::{owner, sign_in, spa, with_session};
use support::library::{ALICE, BOB};
use support::sse::{Stream, assert_event_schema, assert_schema};
use support::{TestState, get, json, post_json, problem, send};

const NOTIFICATIONS: &str = "/api/v1/notifications";
const READ: &str = "/api/v1/notifications/read";
const CLIENT_ERRORS: &str = "/api/v1/client-errors";
const VERSION: &str = "/api/v1/version";

/// A token-shaped bearer credential (`shx_` and 43 base64url characters).
const BEARER: &str = "Bearer shx_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// `request` with what the web app sends on every unsafe request, signed in
/// or not: the CSRF guard's `Origin`, `Sec-Fetch-Site` and `X-Shelfy-Client`
/// (`spa` adds the session cookie as well).
fn web(t: &TestState, mut request: Request<Body>) -> Request<Body> {
    let headers = request.headers_mut();
    headers.insert(
        header::ORIGIN,
        t.state.config().public_url.as_str().parse().unwrap(),
    );
    headers.insert("sec-fetch-site", "same-origin".parse().unwrap());
    headers.insert(CLIENT_HEADER, CLIENT_WEB.parse().unwrap());
    request
}

/// A JSON `POST` from the web app.
fn post(t: &TestState, uri: &str, body: &Value) -> Request<Body> {
    web(t, post_json(uri, body.to_string()))
}

/// Creates a notification for `user` the way the server does.
async fn notify_code(t: &TestState, user: &str, code: &str) -> i64 {
    let new = NewNotification {
        kind: "job".into(),
        code: code.into(),
        ..NewNotification::default()
    };
    notify(&t.state, user, new).await.unwrap().id
}

fn report() -> Value {
    json!({
        "view": "postModal",
        "name": "TypeError",
        "message": "Cannot read properties of undefined (reading 'slides')",
        "stack": "TypeError: Cannot read properties of undefined\n    at Modal (/assets/index-3f2a.js:1:2345)",
        "componentStack": "\n    at PostModal\n    at App",
        "route": "/p/:key",
        "clientVersion": "0.1.0",
        "occurredAt": 1_790_899_200_000_i64,
    })
}

/// Every request of the platform routes, as a signed-in client sends them.
fn platform_requests(t: &TestState) -> Vec<Request<Body>> {
    vec![
        get("/api/v1/events"),
        get(NOTIFICATIONS),
        post(t, READ, &json!({ "upTo": 1 })),
        post(t, CLIENT_ERRORS, &report()),
        get(VERSION),
    ]
}

#[tokio::test]
async fn every_platform_route_needs_a_user() {
    let t = TestState::new();
    let app = t.app();
    for request in platform_requests(&t) {
        let uri = request.uri().to_string();
        let problem = problem(send(&app, request).await, StatusCode::UNAUTHORIZED).await;
        assert_eq!(problem.code, ErrorCode::Unauthorized, "{uri}");
    }
    // A bearer token alone never signs in a cookie route (§2.9 Auth).
    for mut request in platform_requests(&t) {
        let uri = request.uri().to_string();
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, BEARER.parse().unwrap());
        let problem = problem(send(&app, request).await, StatusCode::UNAUTHORIZED).await;
        assert_eq!(problem.code, ErrorCode::Unauthorized, "{uri}");
    }
    let users = t.data_dir().users_dir();
    let opened: Vec<_> = std::fs::read_dir(&users).unwrap().collect();
    assert!(opened.is_empty(), "a refused request opened a library");
    assert_eq!(
        t.state.events().users_len(),
        0,
        "a refused stream made a bus"
    );
}

/// An API token of `user_id` with every scope, stored the way T10 reads it
/// (P1-17 adds the minting route); returns the token.
fn api_token(t: &TestState, user_id: &str) -> String {
    let token = format!("shx_{}", SecretToken::generate().expose());
    let control = Connection::open(t.data_dir().control_db()).unwrap();
    control
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

/// `request` with `Authorization: Bearer <token>`.
fn bearer(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    request
}

/// Lane rule 4: the platform routes are cookie-only. A valid token with every
/// scope is refused, alone or beside a valid session cookie, while the cookie
/// alone works.
#[tokio::test]
async fn api_tokens_never_call_the_platform_routes() {
    let t = TestState::new();
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let token = api_token(&t, &owner_id);

    for (with_cookie, what) in [(false, "a token"), (true, "a token beside the cookie")] {
        for request in platform_requests(&t) {
            let uri = request.uri().to_string();
            let request = if with_cookie {
                spa(&t, request, &cookie)
            } else {
                request
            };
            let response = send(&app, bearer(request, &token)).await;
            let refused = problem(response, StatusCode::UNAUTHORIZED).await;
            assert_eq!(refused.code, ErrorCode::Unauthorized, "{uri} with {what}");
        }
    }
    for request in platform_requests(&t) {
        let uri = request.uri().to_string();
        let response = send(&app, spa(&t, request, &cookie)).await;
        assert!(response.status().is_success(), "{uri} with the cookie");
    }
}

/// The signed-in owner reaches every platform route through the real session
/// layer, and the two writes go through the CSRF guard.
#[tokio::test]
async fn a_session_reaches_the_platform_routes_and_writes_pass_the_csrf_guard() {
    let t = TestState::new();
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    notify_code(&t, &owner_id, "job.failed").await;

    let session = format!("{SESSION_COOKIE}={cookie}");
    let mut stream = Stream::connect(&app, "/api/v1/events", &[("cookie", &session)]).await;
    stream.hello().await;
    let page = json(send(&app, with_session(get(NOTIFICATIONS), &cookie)).await).await;
    assert_eq!(page["unreadCount"], 1);
    let version = send(&app, with_session(get(VERSION), &cookie)).await;
    assert_eq!(version.status(), StatusCode::OK);

    // A write with the cookie but without the web app's header is a forgery.
    for request in [
        post(&t, READ, &json!({ "upTo": 1 })),
        post(&t, CLIENT_ERRORS, &report()),
    ] {
        let uri = request.uri().to_string();
        let mut forged = spa(&t, request, &cookie);
        forged.headers_mut().remove(CLIENT_HEADER);
        let refused = problem(send(&app, forged).await, StatusCode::FORBIDDEN).await;
        assert_eq!(refused.code, ErrorCode::CsrfFailed, "{uri}");
    }
    let read = send(
        &app,
        spa(&t, post(&t, READ, &json!({ "upTo": i64::MAX })), &cookie),
    )
    .await;
    assert_eq!(json(read).await, json!({ "updated": 1, "unreadCount": 0 }));
    let reported = send(&app, spa(&t, post(&t, CLIENT_ERRORS, &report()), &cookie)).await;
    assert_eq!(reported.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn notifications_come_newest_first_with_a_cursor() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let empty = json(send(&app, get(NOTIFICATIONS)).await).await;
    assert_eq!(
        empty,
        json!({ "items": [], "nextCursor": null, "unreadCount": 0 })
    );

    let mut ids = Vec::new();
    for code in ["job.a", "job.b", "job.c", "job.d", "job.e"] {
        ids.push(notify_code(&t, ALICE, code).await);
    }
    let mut seen = Vec::new();
    let mut uri = format!("{NOTIFICATIONS}?limit=2");
    loop {
        let response = send(&app, get(&uri)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = json(response).await;
        assert_schema(&page, "NotificationPage");
        assert_eq!(page["unreadCount"], 5);
        let items = page["items"].as_array().unwrap();
        assert!(items.len() <= 2);
        seen.extend(items.iter().map(|n| n["id"].as_i64().unwrap()));
        match page["nextCursor"].as_str() {
            Some(cursor) => uri = format!("{NOTIFICATIONS}?limit=2&cursor={cursor}"),
            None => break,
        }
    }
    ids.reverse();
    assert_eq!(seen, ids);

    let first = json(send(&app, get(NOTIFICATIONS)).await).await["items"][0].clone();
    assert_eq!(first["code"], "job.e");
    assert_eq!(first["kind"], "job");
    assert_eq!(first["params"], json!({}));
    assert_eq!(first["readAt"], Value::Null);
}

#[tokio::test]
async fn new_notifications_arrive_on_the_stream() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let mut stream = Stream::connect(&app, "/api/v1/events", &[]).await;
    stream.hello().await;
    let new = NewNotification {
        kind: "migration".into(),
        code: "migration.done".into(),
        params: json!({ "posts": 6138 }).as_object().unwrap().clone(),
        target: Some("/settings/data".into()),
    };
    notify(&t.state, ALICE, new).await.unwrap();

    let frame = stream.expect("notification").await;
    assert_event_schema(&frame);
    let listed = json(send(&app, get(NOTIFICATIONS)).await).await;
    assert_eq!(
        frame.json(),
        listed["items"][0],
        "the event is the stored notification"
    );
    assert_eq!(frame.json()["params"]["posts"], 6138);
}

#[tokio::test]
async fn notifications_are_marked_read_by_id_or_up_to_an_id() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let mut ids = Vec::new();
    for code in ["job.a", "job.b", "job.c", "job.d"] {
        ids.push(notify_code(&t, ALICE, code).await);
    }
    let mark = |body: Value| {
        let app = app.clone();
        let request = post(&t, READ, &body);
        async move {
            let response = send(&app, request).await;
            assert_eq!(response.status(), StatusCode::OK, "{body}");
            let result = json(response).await;
            assert_schema(&result, "MarkReadResult");
            result
        }
    };

    let unknown = ids[3] + 100;
    let result = mark(json!({ "ids": [ids[0], ids[1], unknown] })).await;
    assert_eq!(result, json!({ "updated": 2, "unreadCount": 2 }));
    let again = mark(json!({ "ids": [ids[0], ids[1]] })).await;
    assert_eq!(
        again,
        json!({ "updated": 0, "unreadCount": 2 }),
        "idempotent"
    );
    let up_to = mark(json!({ "upTo": ids[2] })).await;
    assert_eq!(up_to, json!({ "updated": 1, "unreadCount": 1 }));

    let page = json(send(&app, get(NOTIFICATIONS)).await).await;
    let read: Vec<bool> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["readAt"].is_i64())
        .collect();
    assert_eq!(read, [false, true, true, true], "newest first");
    assert_eq!(page["unreadCount"], 1);
}

#[tokio::test]
async fn marking_read_takes_exactly_one_selector() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let too_many: Vec<i64> = (1..=201).collect();
    for body in [
        json!({}),
        json!({ "ids": [1], "upTo": 1 }),
        json!({ "ids": too_many }),
        json!({ "all": true }),
        json!({ "ids": ["one"] }),
    ] {
        let problem = problem(
            send(&app, post(&t, READ, &body)).await,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
        assert_eq!(problem.code, ErrorCode::ValidationFailed, "{body}");
    }
    let text = Request::post(READ)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("upTo=1"))
        .unwrap();
    let problem = problem(
        send(&app, web(&t, text)).await,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
    )
    .await;
    assert_eq!(problem.code, ErrorCode::UnsupportedMediaType);
}

#[tokio::test]
async fn unchanged_notification_pages_answer_not_modified() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    notify_code(&t, ALICE, "job.failed").await;
    let response = send(&app, get(NOTIFICATIONS)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-cache"
    );
    let etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();

    let conditional = |etag: &str| {
        Request::get(NOTIFICATIONS)
            .header(header::IF_NONE_MATCH, etag)
            .body(Body::empty())
            .unwrap()
    };
    let response = send(&app, conditional(&etag)).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    // Another page size is another view.
    let other = Request::get(format!("{NOTIFICATIONS}?limit=1"))
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, other).await.status(), StatusCode::OK);

    // A new notification, or marking one read, changes the page.
    notify_code(&t, ALICE, "job.failed").await;
    let response = send(&app, conditional(&etag)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    send(&app, post(&t, READ, &json!({ "upTo": i64::MAX }))).await;
    assert_eq!(
        send(&app, conditional(&etag)).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn bad_notification_cursors_are_problems() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let posts_cursor = URL_SAFE_NO_PAD.encode("0011223344556677.n.1.2");
    // An empty cursor is a bad one, as on `GET /posts`.
    for cursor in ["garbage", "", posts_cursor.as_str()] {
        let uri = format!("{NOTIFICATIONS}?cursor={cursor}");
        let problem = problem(send(&app, get(&uri)).await, StatusCode::BAD_REQUEST).await;
        assert_eq!(problem.code, ErrorCode::InvalidCursor, "{cursor}");
    }
    let problem = problem(
        send(&app, get(&format!("{NOTIFICATIONS}?limit=lots"))).await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(problem.code, ErrorCode::BadRequest);
}

#[tokio::test]
async fn users_never_see_or_mark_each_others_notifications() {
    let t = TestState::new();
    let alice = t.app_as(ALICE);
    let bob = t.app_as(BOB);
    let ids = [
        notify_code(&t, ALICE, "job.a").await,
        notify_code(&t, ALICE, "job.b").await,
    ];

    let page = json(send(&bob, get(NOTIFICATIONS)).await).await;
    assert_eq!(page["items"], json!([]));
    assert_eq!(page["unreadCount"], 0);
    // Alice's ids address nothing in Bob's library: like a missing id, they
    // change nothing.
    for body in [json!({ "ids": ids }), json!({ "upTo": i64::MAX })] {
        let result = json(send(&bob, post(&t, READ, &body)).await).await;
        assert_eq!(result, json!({ "updated": 0, "unreadCount": 0 }));
    }
    // A cursor of Alice's pages lists Bob's library only.
    let first = json(send(&alice, get(&format!("{NOTIFICATIONS}?limit=1"))).await).await;
    let cursor = first["nextCursor"].as_str().unwrap();
    let page = json(send(&bob, get(&format!("{NOTIFICATIONS}?cursor={cursor}"))).await).await;
    assert_eq!(page["items"], json!([]));

    let page = json(send(&alice, get(NOTIFICATIONS)).await).await;
    assert_eq!(page["unreadCount"], 2);
}

#[tokio::test]
async fn client_error_reports_are_validated_and_capped() {
    let t = TestState::new();
    let app = t.app_as(ALICE);
    let minimal = json!({ "view": "gallery", "message": "boom" });
    assert_eq!(
        send(&app, post(&t, CLIENT_ERRORS, &minimal)).await.status(),
        StatusCode::NO_CONTENT
    );
    let mut bad = Vec::new();
    for (field, value) in [
        ("view", json!("post modal")),
        ("route", json!("/search?q=lampada")),
        ("route", json!("https://refs.example.test/p/ig_1")),
        ("clientVersion", json!("1.0 beta")),
    ] {
        let mut body = report();
        body[field] = value;
        bad.push(body);
    }
    bad.push(json!({ "view": "gallery" }));
    bad.push(json!({ "view": "gallery", "message": 5 }));
    for body in bad {
        let problem = problem(
            send(&app, post(&t, CLIENT_ERRORS, &body)).await,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
        assert_eq!(problem.code, ErrorCode::ValidationFailed, "{body}");
    }

    // The default 64 KiB body limit.
    let mut huge = report();
    huge["stack"] = json!("s".repeat(64 * 1024));
    let problem = problem(
        send(&app, post(&t, CLIENT_ERRORS, &huge)).await,
        StatusCode::PAYLOAD_TOO_LARGE,
    )
    .await;
    assert_eq!(problem.code, ErrorCode::PayloadTooLarge);
}

#[tokio::test]
async fn the_version_names_the_build_and_the_api() {
    let t = TestState::new();
    let response = send(&t.app_as(ALICE), get(VERSION)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let version = json(response).await;
    assert_schema(&version, "VersionInfo");
    assert_eq!(
        version,
        json!({ "version": shelfy_server::VERSION, "apiVersion": "1" })
    );
}
