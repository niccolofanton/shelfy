//! The browser extension's side of the server through the real middleware
//! stack (P2-03; plan §2.11 device tokens, §2.16; contracts C1–C3 and C8):
//! pairing codes and their exchange (round trip, single use, expiry on a
//! test clock, another account's code, re-pairing an installation, the
//! token cap, the sign-in limit, the CSRF exemption), the configuration and
//! its flags (defaults, precedence, the 30-second refresh, `admin flags`),
//! the version gate (426), presence (`extension.status`) and per-route
//! access. The logs are checked in `auth_logs.rs`.

mod support;

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, Response, StatusCode, header};
use rusqlite::params;
use serde_json::{Value, json};
use shelfy_server::admin::flags::{self as admin_flags, FlagsArgs, FlagsCommand};
use shelfy_server::config::Config;
use shelfy_server::error::{ErrorCode, Problem};
use shelfy_server::events::model::EventTopic;
use shelfy_server::events::{Delivery, Subscription};
use shelfy_server::extension::flags::FlagValue;
use shelfy_server::extension::{self, FLAGS_TTL, VERSION_HEADER};
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::jobs::Clock;
use shelfy_server::routes;
use shelfy_server::tokens::{SecretToken, hash_token, is_token_shaped};
use support::auth::{
    add_member, audit_rows, control_db, from_spa, make_sessions_stale, owner, post, sign_in,
    sign_in_as, spa, with_session,
};
use support::sse::{Stream, assert_event_schema, assert_schema};
use support::{TestState, body, get, is_ulid, json, problem, send};

const MEMBER_EMAIL: &str = "member@example.test";
const PAIR: &str = "/api/v1/extension/pair";
const CONFIG: &str = "/api/v1/extension/config";
const STATUS: &str = "/api/v1/extension/status";
const PAIRING_CODE: &str = "/api/v1/me/tokens/pairing-code";
const LOOKUP: &str = "/api/v1/posts/lookup";
/// The origin of the extension's service worker, as Chrome sends it.
const EXTENSION_ORIGIN: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop";
/// Installation ids, as the extension makes them.
const INSTALL: &str = "3f2b8c1e-5d4a-4f7e-9a6b-1c2d3e4f5a6b";
const OTHER_INSTALL: &str = "9c8d7e6f-5a4b-4c3d-8e2f-1a0b9c8d7e6f";

/// A state whose sign-in limit leaves room for many requests (every
/// in-process request counts as the same client); `edit` adjusts the rest.
fn state(edit: impl FnOnce(&mut Config)) -> TestState {
    TestState::with_config(|config: &mut Config| {
        config.auth.ip_limit.max = 1_000;
        edit(config);
    })
}

/// `POST /me/tokens/pairing-code` as the web app sends it; checks 201.
async fn pairing_code(app: &Router, t: &TestState, cookie: &str) -> Value {
    let response = send(app, spa(t, post(PAIRING_CODE), cookie)).await;
    assert_eq!(response.status(), StatusCode::CREATED, "pairing code");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    json(response).await
}

/// The code of a new pairing code.
async fn code(app: &Router, t: &TestState, cookie: &str) -> String {
    pairing_code(app, t, cookie).await["code"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn pair_body(code: &str, install: &str, version: &str) -> Value {
    json!({ "code": code, "installId": install, "label": "Chrome on macOS", "version": version })
}

/// `POST /extension/pair` as the extension's service worker sends it: JSON,
/// its own origin, no cookie, no CSRF headers.
fn pair_request(body: &Value) -> Request<Body> {
    Request::post(PAIR)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, EXTENSION_ORIGIN)
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Pairs installation `install` with `code` as version 0.2.0; checks 201
/// and returns the answer.
async fn pair(app: &Router, code: &str, install: &str) -> Value {
    let response = send(app, pair_request(&pair_body(code, install, "0.2.0"))).await;
    assert_eq!(response.status(), StatusCode::CREATED, "pairing");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    json(response).await
}

/// Signs the owner in and pairs installation `install`; returns the session
/// cookie, the token and its id.
async fn paired(app: &Router, t: &TestState, install: &str) -> (String, String, String) {
    let cookie = sign_in(app, t).await;
    let code = code(app, t, &cookie).await;
    let answer = pair(app, &code, install).await;
    let token = answer["token"].as_str().unwrap().to_owned();
    let id = answer["tokenId"].as_str().unwrap().to_owned();
    (cookie, token, id)
}

/// `request` as the extension sends it: its token, and the version header
/// when `version` is given.
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

/// `POST /posts/lookup` with a small body: a token route with `lookup`.
fn lookup() -> Request<Body> {
    Request::post(LOOKUP)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "platform": "instagram", "keys": ["1"] }).to_string(),
        ))
        .unwrap()
}

/// Checks that `response` is the problem `code` with `status`.
async fn refused(response: Response<Body>, status: StatusCode, code: ErrorCode) -> Problem {
    let problem = problem(response, status).await;
    assert_eq!(problem.code, code);
    problem
}

/// `request` with extra headers.
fn with_headers(mut request: Request<Body>, headers: &[(&str, &str)]) -> Request<Body> {
    for (name, value) in headers {
        request.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    request
}

/// `request` as if it came over TCP from `peer`.
fn from_peer(mut request: Request<Body>, peer: &str) -> Request<Body> {
    let ip = peer.parse().unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip, 40_000)));
    request
}

/// Inserts a working token of `user_id` with `kind` and `scopes`; returns
/// its value.
fn insert_token(t: &TestState, user_id: &str, kind: &str, scopes: &str) -> String {
    let token = format!("shx_{}", SecretToken::generate().expose());
    control_db(t)
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                new_ulid(),
                user_id,
                kind,
                hash_token(&token).as_slice(),
                scopes,
                now_ms()
            ],
        )
        .unwrap();
    token
}

fn count(t: &TestState, sql: &str) -> i64 {
    control_db(t).query_row(sql, [], |row| row.get(0)).unwrap()
}

/// The account's working tokens, as `GET /me/tokens` lists them.
async fn token_list(app: &Router, cookie: &str) -> Vec<Value> {
    let response = send(app, with_session(get("/api/v1/me/tokens"), cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    json(response).await["items"].as_array().unwrap().clone()
}

/// `GET /extension/status` for the session `cookie`.
async fn status(app: &Router, cookie: &str) -> Value {
    let response = send(app, with_session(get(STATUS), cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    json(response).await
}

#[tokio::test]
async fn the_extension_pairs_with_a_code_from_the_web_app() {
    let t = state(|_| {});
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;

    // The web app asks for a code: 43 characters, valid 60 seconds.
    let before = now_ms();
    let created = pairing_code(&app, &t, &cookie).await;
    let code = created["code"].as_str().unwrap().to_owned();
    assert!(is_token_shaped(&code), "{code}");
    let expires_at = created["expiresAt"].as_i64().unwrap();
    assert!(
        (before + 60_000..=now_ms() + 60_000).contains(&expires_at),
        "{expires_at}"
    );
    assert_eq!(created.as_object().unwrap().len(), 2);

    // Stored as its SHA-256, for this account, unused; audited without it.
    let conn = control_db(&t);
    let row: (Vec<u8>, String, String, i64, Option<i64>) = conn
        .query_row(
            "SELECT code_hash, user_id, kind, expires_at, used_at FROM pairing_codes",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(row.0, hash_token(&code).to_vec());
    assert_eq!(
        (row.1.as_str(), row.2.as_str(), row.3, row.4),
        (owner_id.as_str(), "extension", expires_at, None)
    );
    assert_eq!(
        audit_rows(&conn, "pairing_code.create"),
        [json!({ "kind": "extension", "expiresAt": expires_at })]
    );

    // The extension exchanges it from its own origin: no cookie, no CSRF
    // headers.
    let answer = pair(&app, &code, INSTALL).await;
    let token = answer["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("shx_") && is_token_shaped(&token[4..]));
    let token_id = answer["tokenId"].as_str().unwrap().to_owned();
    assert!(is_ulid(&token_id), "{token_id}");
    assert_eq!(
        answer["scopes"],
        json!(["ingest", "tasks", "uploads", "lookup"])
    );
    assert_eq!(answer.as_object().unwrap().len(), 3);

    // The code is spent; the token is an extension token of the owner, tied
    // to the installation's digest, minted `via: pairing`.
    let used: Option<i64> = conn
        .query_row("SELECT used_at FROM pairing_codes", [], |r| r.get(0))
        .unwrap();
    assert!(used.is_some());
    let stored: (String, String, String, String, Vec<u8>) = conn
        .query_row(
            "SELECT user_id, kind, label, scopes, install_hash FROM api_tokens WHERE id = ?1",
            [&token_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        (
            stored.0.as_str(),
            stored.1.as_str(),
            stored.2.as_str(),
            stored.3.as_str()
        ),
        (
            owner_id.as_str(),
            "extension",
            "Chrome on macOS",
            "ingest tasks uploads lookup"
        )
    );
    assert_eq!(
        stored.4,
        extension::pairing::install_hash(INSTALL).unwrap().to_vec()
    );
    assert_eq!(
        audit_rows(&conn, "api_token.create"),
        [json!({ "id": token_id, "kind": "extension", "via": "pairing" })]
    );
    let actor: String = conn
        .query_row(
            "SELECT actor_user_id FROM audit_log WHERE action = 'api_token.create'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(actor, owner_id);
    let secrets_in_audit: i64 = conn
        .query_row(
            "SELECT count(*) FROM audit_log WHERE meta_json LIKE '%' || ?1 || '%' \
             OR meta_json LIKE '%' || ?2 || '%' OR meta_json LIKE '%' || ?3 || '%'",
            params![code, &token[4..], INSTALL],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(secrets_in_audit, 0);

    // The web app lists it; the extension uses it.
    let list = token_list(&app, &cookie).await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], token_id.as_str());
    assert_eq!(list[0]["kind"], "extension");
    assert_eq!(list[0]["label"], "Chrome on macOS");
    let response = send(&app, ext(get(CONFIG), &token, Some("0.2.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(&app, ext(lookup(), &token, Some("0.2.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    // It never reaches a cookie route, even beside the session.
    let request = ext(
        with_session(get("/api/v1/me"), &cookie),
        &token,
        Some("0.2.0"),
    );
    refused(
        send(&app, request).await,
        StatusCode::UNAUTHORIZED,
        ErrorCode::Unauthorized,
    )
    .await;
}

#[tokio::test]
async fn a_code_pairs_once() {
    let t = state(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let code = code(&app, &t, &cookie).await;
    pair(&app, &code, INSTALL).await;

    // Replayed, from the same installation or another: refused.
    for install in [INSTALL, OTHER_INSTALL] {
        let response = send(&app, pair_request(&pair_body(&code, install, "0.2.0"))).await;
        refused(
            response,
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPairingCode,
        )
        .await;
    }
    // Unknown and malformed codes answer the same.
    let unknown = SecretToken::generate();
    for bad in [
        unknown.expose().to_owned(),
        "x".repeat(43),
        code[..42].to_owned(),
        format!("{code}="),
        String::new(),
    ] {
        let response = send(&app, pair_request(&pair_body(&bad, INSTALL, "0.2.0"))).await;
        refused(
            response,
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPairingCode,
        )
        .await;
    }
    assert_eq!(count(&t, "SELECT count(*) FROM api_tokens"), 1);

    // A malformed body is a 422 and leaves a good code usable.
    let code = self::code(&app, &t, &cookie).await;
    for body in [
        json!({ "code": code, "installId": "short", "version": "0.2.0" }),
        json!({ "code": code, "installId": "has spaces in it, sixteen+", "version": "0.2.0" }),
        json!({ "code": code, "installId": INSTALL, "version": "two" }),
        json!({ "code": code, "installId": INSTALL, "version": "0.2.0", "label": "x".repeat(65) }),
        json!({ "code": code, "installId": INSTALL }),
    ] {
        let response = send(&app, pair_request(&body)).await;
        refused(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::ValidationFailed,
        )
        .await;
    }
    pair(&app, &code, OTHER_INSTALL).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_racing_exchanges_of_one_code_mint_one_token() {
    let t = state(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    for _ in 0..5 {
        let code = code(&app, &t, &cookie).await;
        let first = send(&app, pair_request(&pair_body(&code, INSTALL, "0.2.0")));
        let second = send(
            &app,
            pair_request(&pair_body(&code, OTHER_INSTALL, "0.2.0")),
        );
        let (first, second) = tokio::join!(first, second);
        let mut statuses = [first.status(), second.status()];
        statuses.sort();
        assert_eq!(statuses, [StatusCode::CREATED, StatusCode::BAD_REQUEST]);
    }
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM audit_log WHERE action = 'api_token.create'"
        ),
        5
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_codes_never_wait_for_the_writer() {
    let t = state(|_| {});
    let app = t.app();
    owner(&t);

    // Another connection holds the control database's write lock.
    let path = t.data_dir().control_db();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let mut conn = rusqlite::Connection::open(path).unwrap();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        tx.rollback().unwrap();
    });
    held_rx.recv().unwrap();

    // An unknown code is refused on a reader, at once.
    let started = std::time::Instant::now();
    let unknown = SecretToken::generate();
    let response = send(
        &app,
        pair_request(&pair_body(unknown.expose(), INSTALL, "0.2.0")),
    )
    .await;
    let elapsed = started.elapsed();
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    refused(
        response,
        StatusCode::BAD_REQUEST,
        ErrorCode::InvalidPairingCode,
    )
    .await;
    assert!(
        elapsed < Duration::from_secs(2),
        "took {elapsed:?}: the writer's busy timeout is 5 s"
    );
}

#[tokio::test(start_paused = true)]
async fn a_code_expires_after_a_minute() {
    let t = state(|config: &mut Config| {
        config.extension.clock = Clock::tokio(now_ms());
    });
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let old = code(&app, &t, &cookie).await;
    tokio::time::advance(Duration::from_secs(30)).await;
    let young = code(&app, &t, &cookie).await;
    tokio::time::advance(Duration::from_secs(30)).await;

    // 60 seconds old: refused. 30 seconds old: still good, until 59.999.
    let response = send(&app, pair_request(&pair_body(&old, INSTALL, "0.2.0"))).await;
    refused(
        response,
        StatusCode::BAD_REQUEST,
        ErrorCode::InvalidPairingCode,
    )
    .await;
    tokio::time::advance(Duration::from_millis(29_999)).await;
    pair(&app, &young, INSTALL).await;

    // The next code prunes the expired ones.
    assert_eq!(count(&t, "SELECT count(*) FROM pairing_codes"), 2);
    tokio::time::advance(Duration::from_secs(60)).await;
    code(&app, &t, &cookie).await;
    assert_eq!(count(&t, "SELECT count(*) FROM pairing_codes"), 1);
}

#[tokio::test]
async fn a_code_pairs_the_account_that_asked_for_it() {
    let t = state(|_| {});
    let app = t.app();
    let owner_cookie = sign_in(&app, &t).await;
    add_member(&t, MEMBER_EMAIL);
    let member_cookie = sign_in_as(&app, &t, MEMBER_EMAIL).await;

    // The member's code, sent beside the owner's session: the route reads
    // no cookie, the code alone says whose token it is.
    let member_code = code(&app, &t, &member_cookie).await;
    let request = with_session(
        pair_request(&pair_body(&member_code, INSTALL, "0.2.0")),
        &owner_cookie,
    );
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
    let member_token = json(response).await["token"].as_str().unwrap().to_owned();
    assert_eq!(token_list(&app, &member_cookie).await.len(), 1);
    assert!(token_list(&app, &owner_cookie).await.is_empty());

    // Its requests are the member's: presence and all.
    let response = send(&app, ext(get(CONFIG), &member_token, Some("0.2.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(status(&app, &member_cookie).await["connected"], true);
    assert_eq!(status(&app, &owner_cookie).await["connected"], false);

    // The owner pairing an installation with the same id leaves the
    // member's token alone.
    let owner_code = code(&app, &t, &owner_cookie).await;
    pair(&app, &owner_code, INSTALL).await;
    let response = send(&app, ext(lookup(), &member_token, Some("0.2.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(token_list(&app, &member_cookie).await.len(), 1);
    assert_eq!(token_list(&app, &owner_cookie).await.len(), 1);
}

#[tokio::test]
async fn pairing_an_installation_again_replaces_its_token() {
    let t = state(|_| {});
    let app = t.app();
    let (cookie, first, first_id) = paired(&app, &t, INSTALL).await;
    let second = pair(&app, &code(&app, &t, &cookie).await, INSTALL).await;
    let second_token = second["token"].as_str().unwrap();

    // The installation's earlier token stops working at once.
    let response = send(&app, ext(get(CONFIG), &first, Some("0.2.0"))).await;
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    refused(response, StatusCode::UNAUTHORIZED, ErrorCode::Unauthorized).await;
    let response = send(&app, ext(get(CONFIG), second_token, Some("0.2.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        audit_rows(&control_db(&t), "api_token.revoke"),
        [json!({ "id": first_id, "kind": "extension", "via": "pairing" })]
    );

    // Another installation adds a token.
    let third = pair(&app, &code(&app, &t, &cookie).await, OTHER_INSTALL).await;
    let response = send(&app, ext(get(CONFIG), second_token, Some("0.2.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let ids: Vec<Value> = token_list(&app, &cookie)
        .await
        .into_iter()
        .map(|item| item["id"].clone())
        .collect();
    assert_eq!(ids, [third["tokenId"].clone(), second["tokenId"].clone()]);
}

#[tokio::test]
async fn an_account_holds_at_most_50_working_tokens() {
    let t = state(|_| {});
    let app = t.app();
    let owner_id = owner(&t);
    let (cookie, _token, _id) = paired(&app, &t, INSTALL).await;
    for _ in 0..49 {
        insert_token(&t, &owner_id, "shortcut", "links:create");
    }
    // A new installation: refused, and the code stays usable.
    let code = code(&app, &t, &cookie).await;
    let response = send(
        &app,
        pair_request(&pair_body(&code, OTHER_INSTALL, "0.2.0")),
    )
    .await;
    refused(response, StatusCode::CONFLICT, ErrorCode::Conflict).await;
    let unused = "SELECT count(*) FROM pairing_codes WHERE used_at IS NULL";
    assert_eq!(count(&t, unused), 1, "nothing changed");
    // The paired installation pairs again: its own token makes room.
    pair(&app, &code, INSTALL).await;
    let working = "SELECT count(*) FROM api_tokens WHERE revoked_at IS NULL";
    assert_eq!(count(&t, working), 50);
}

#[tokio::test]
async fn pairing_codes_need_a_recent_sign_in_in_the_web_app() {
    let t = state(|_| {});
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let token = insert_token(&t, &owner_id, "extension", "ingest tasks uploads lookup");

    // Refused: an old sign-in, the CSRF headers missing or another origin,
    // a token (beside the cookie or alone), no session.
    make_sessions_stale(&t);
    refused(
        send(&app, spa(&t, post(PAIRING_CODE), &cookie)).await,
        StatusCode::FORBIDDEN,
        ErrorCode::ReauthRequired,
    )
    .await;
    let cookie = sign_in(&app, &t).await;
    let cross_site = with_headers(
        spa(&t, post(PAIRING_CODE), &cookie),
        &[
            ("origin", "https://evil.example.test"),
            ("sec-fetch-site", "cross-site"),
        ],
    );
    for request in [with_session(post(PAIRING_CODE), &cookie), cross_site] {
        refused(
            send(&app, request).await,
            StatusCode::FORBIDDEN,
            ErrorCode::CsrfFailed,
        )
        .await;
    }
    for request in [
        ext(spa(&t, post(PAIRING_CODE), &cookie), &token, Some("0.2.0")),
        ext(from_spa(&t, post(PAIRING_CODE)), &token, Some("0.2.0")),
        from_spa(&t, post(PAIRING_CODE)),
    ] {
        refused(
            send(&app, request).await,
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
        )
        .await;
    }
    assert_eq!(count(&t, "SELECT count(*) FROM pairing_codes"), 0);

    // At most 10 usable codes at once.
    for _ in 0..10 {
        pairing_code(&app, &t, &cookie).await;
    }
    let response = send(&app, spa(&t, post(PAIRING_CODE), &cookie)).await;
    let wait: u64 = response.headers()[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    refused(
        response,
        StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::RateLimited,
    )
    .await;
    assert_eq!(count(&t, "SELECT count(*) FROM pairing_codes"), 10);
}

#[tokio::test]
async fn only_the_pair_route_skips_the_csrf_check() {
    let t = state(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let code = code(&app, &t, &cookie).await;
    let cross_site = [
        ("origin", "https://evil.example.test"),
        ("sec-fetch-site", "cross-site"),
    ];

    // A cross-site page can send a code (it is the credential) but never
    // reads the answer: no CORS.
    let request = with_headers(
        pair_request(&pair_body(&code, INSTALL, "0.2.0")),
        &cross_site,
    );
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
    // The routes around it are checked like any cookie request.
    for request in [
        with_headers(spa(&t, post(PAIRING_CODE), &cookie), &cross_site),
        with_headers(spa(&t, post("/api/v1/me/tokens"), &cookie), &cross_site),
    ] {
        refused(
            send(&app, request).await,
            StatusCode::FORBIDDEN,
            ErrorCode::CsrfFailed,
        )
        .await;
    }
    // The exemption list: the CLI's device flow and the pairing, each public.
    assert_eq!(
        routes::CSRF_EXEMPT_ROUTES
            .iter()
            .map(|(method, path)| format!("{method} {path}"))
            .collect::<Vec<_>>(),
        [
            "POST /api/v1/auth/device/start",
            "POST /api/v1/auth/device/poll",
            "POST /api/v1/extension/pair"
        ]
    );
    for (method, path) in routes::CSRF_EXEMPT_ROUTES {
        assert!(routes::PUBLIC_ROUTES.contains(&(method.clone(), *path)));
    }
}

#[tokio::test]
async fn pairing_counts_against_the_sign_in_limit() {
    // The default limit: 10 sign-in requests a minute per client.
    let t = TestState::new();
    let app = t.app();
    let unknown = SecretToken::generate();
    let attempt = |peer: &str| {
        from_peer(
            pair_request(&pair_body(unknown.expose(), INSTALL, "0.2.0")),
            peer,
        )
    };
    for _ in 0..10 {
        let response = send(&app, attempt("198.51.100.7")).await;
        refused(
            response,
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPairingCode,
        )
        .await;
    }
    let response = send(&app, attempt("198.51.100.7")).await;
    assert!(response.headers().contains_key(header::RETRY_AFTER));
    refused(
        response,
        StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::RateLimited,
    )
    .await;
    // The budget is the sign-in routes' one, per client.
    let methods = from_peer(get("/api/v1/auth/methods"), "198.51.100.7");
    assert_eq!(
        send(&app, methods).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    let response = send(&app, attempt("198.51.100.8")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn outdated_or_unversioned_extensions_get_426_but_on_the_config() {
    let t = state(|_| {});
    let app = t.app();
    let owner_id = owner(&t);
    let (cookie, token, _id) = paired(&app, &t, INSTALL).await;

    for version in [
        None,
        Some("0.1.9"),
        Some("garbage"),
        Some("0.2.0-beta"),
        Some(""),
    ] {
        let response = send(&app, ext(lookup(), &token, version)).await;
        let problem = refused(
            response,
            StatusCode::UPGRADE_REQUIRED,
            ErrorCode::ExtensionOutdated,
        )
        .await;
        assert_eq!(
            problem.detail.as_deref(),
            Some("X-Shelfy-Extension must be 0.2.0 or newer"),
            "{version:?}"
        );
    }
    for version in [
        "0.2.0",
        "0.2",
        "0.10.0",
        "1.0.0.1",
        "0.3.0-beta.1",
        "0.2.1+build.7",
    ] {
        let response = send(&app, ext(lookup(), &token, Some(version))).await;
        assert_eq!(response.status(), StatusCode::OK, "{version}");
    }
    // The config answers every version, and none.
    for version in [None, Some("0.1.0"), Some("garbage")] {
        let response = send(&app, ext(get(CONFIG), &token, version)).await;
        assert_eq!(response.status(), StatusCode::OK, "{version:?}");
    }

    // The operator raises the minimum.
    admin_flags::set(&t.data_dir(), "extension.minVersion", "0.3.0").unwrap();
    t.state.extension().flags().invalidate();
    let response = send(&app, ext(lookup(), &token, Some("0.2.0"))).await;
    let problem = refused(
        response,
        StatusCode::UPGRADE_REQUIRED,
        ErrorCode::ExtensionOutdated,
    )
    .await;
    assert_eq!(
        problem.detail.as_deref(),
        Some("X-Shelfy-Extension must be 0.3.0 or newer")
    );
    let response = send(&app, ext(get(CONFIG), &token, Some("0.2.0"))).await;
    assert_eq!(json(response).await["minVersion"], "0.3.0");
    let response = send(&app, ext(lookup(), &token, Some("0.3.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);

    // An outdated extension cannot pair, and its code stays usable.
    let code = code(&app, &t, &cookie).await;
    let response = send(&app, pair_request(&pair_body(&code, INSTALL, "0.2.0"))).await;
    refused(
        response,
        StatusCode::UPGRADE_REQUIRED,
        ErrorCode::ExtensionOutdated,
    )
    .await;
    let response = send(&app, pair_request(&pair_body(&code, INSTALL, "0.3.0"))).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // A token without the route's scope is a 403 first, whatever its
    // version; tokens of other kinds are not versioned.
    let lookup_only = insert_token(&t, &owner_id, "extension", "lookup");
    refused(
        send(&app, ext(get(CONFIG), &lookup_only, None)).await,
        StatusCode::FORBIDDEN,
        ErrorCode::Forbidden,
    )
    .await;
    let not_extension = insert_token(&t, &owner_id, "shortcut", "lookup");
    let response = send(&app, ext(lookup(), &not_extension, None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    // Sessions are not versioned either.
    let response = send(&app, spa(&t, lookup(), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn the_config_serves_the_flags_within_30_seconds_of_a_change() {
    assert!(FLAGS_TTL <= Duration::from_secs(30));
    let t = state(|_| {});
    let app = t.app();
    let (_cookie, token, _id) = paired(&app, &t, INSTALL).await;
    let config = || ext(get(CONFIG), &token, Some("0.2.0"));

    // The defaults are the contract (C3).
    let response = send(&app, config()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-cache"
    );
    let etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let defaults = json(response).await;
    assert_eq!(
        defaults,
        json!({
            "minVersion": "0.2.0",
            "platforms": {
                "instagram": {
                    "passive": true, "replay": true, "scroll": true, "stopAfterKnown": 10,
                    "replayGapMs": 700, "replayMaxPages": 100, "scrollSettleMs": 650
                },
                "twitter": {
                    "passive": true, "scroll": true, "stopAfterKnown": 20, "scrollSettleMs": 750
                },
                "pinterest": {
                    "passive": true, "scroll": true, "stopAfterKnown": 25, "scrollSettleMs": 650
                }
            },
            "maxSteps": 16000, "maxRunMs": 1_800_000, "taskPollMinutes": 5,
            "refreshPerSession": 200
        })
    );
    assert_schema(&defaults, "ExtensionConfig");
    let response = send(
        &app,
        with_headers(config(), &[("if-none-match", etag.as_str())]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(response.headers()[header::ETAG], etag.as_str());
    assert!(body(response).await.is_empty());

    // The operator kills the Instagram replay and raises X's threshold, in
    // another process: the server serves what it read for up to 30 s.
    let data = t.data_dir();
    admin_flags::set(&data, "extension.instagram.replay", "false").unwrap();
    admin_flags::set(&data, "extension.twitter.stopAfterKnown", "40").unwrap();
    tokio::time::advance(Duration::from_secs(29)).await;
    let response = send(&app, config()).await;
    assert_eq!(response.headers()[header::ETAG], etag.as_str());
    tokio::time::advance(Duration::from_secs(1)).await;
    let response = send(
        &app,
        with_headers(config(), &[("if-none-match", etag.as_str())]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "a new ETag");
    let changed_etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert_ne!(changed_etag, etag);
    let changed = json(response).await;
    assert_eq!(changed["platforms"]["instagram"]["replay"], false);
    assert_eq!(changed["platforms"]["twitter"]["stopAfterKnown"], 40);
    let mut expected = defaults.clone();
    expected["platforms"]["instagram"]["replay"] = json!(false);
    expected["platforms"]["twitter"]["stopAfterKnown"] = json!(40);
    assert_eq!(changed, expected, "nothing else moved");

    // A hand-written row that does not fit its flag is ignored; unsetting a
    // flag brings its default back.
    control_db(&t)
        .execute(
            "INSERT INTO feature_flags (key, value_json, updated_at) \
             VALUES ('extension.maxSteps', '\"lots\"', 1)",
            [],
        )
        .unwrap();
    admin_flags::unset(&data, "extension.instagram.replay").unwrap();
    tokio::time::advance(Duration::from_secs(30)).await;
    let response = send(&app, config()).await;
    let config_now = json(response).await;
    assert_eq!(config_now["maxSteps"], 16_000);
    assert_eq!(config_now["platforms"]["instagram"]["replay"], true);
    assert_eq!(config_now["platforms"]["twitter"]["stopAfterKnown"], 40);
}

#[tokio::test]
async fn admin_flags_takes_only_the_typed_keys_and_values() {
    let t = state(|_| {});
    owner(&t);
    let data = t.data_dir();

    let listed = admin_flags::list(&data).unwrap();
    assert_eq!(listed.len(), 20);
    assert!(listed.iter().all(|line| line.source == "default"));
    assert_eq!(
        admin_flags::get(&data, "extension.instagram.replayGapMs").unwrap(),
        json!(700)
    );

    for (key, value) in [
        ("extension.instagram.turbo", "true"),
        ("capture", "true"),
        ("extension.twitter.replay", "false"),
        ("extension.instagram.replay", "\"off\""),
        ("extension.instagram.replay", "off"),
        ("extension.instagram.replayGapMs", "10"),
        ("extension.maxRunMs", "1.5"),
        ("extension.minVersion", "latest"),
    ] {
        assert!(
            admin_flags::set(&data, key, value).is_err(),
            "{key} = {value}"
        );
    }
    assert!(admin_flags::get(&data, "extension.instagram.turbo").is_err());
    assert_eq!(count(&t, "SELECT count(*) FROM feature_flags"), 0);

    assert_eq!(
        admin_flags::set(&data, "extension.instagram.replay", "false").unwrap(),
        FlagValue::Bool(false)
    );
    admin_flags::set(&data, "extension.minVersion", "0.3.0").unwrap();
    assert_eq!(
        admin_flags::get(&data, "extension.instagram.replay").unwrap(),
        json!(false)
    );
    assert_eq!(
        admin_flags::get(&data, "extension.minVersion").unwrap(),
        json!("0.3.0")
    );
    let stored: String = control_db(&t)
        .query_row(
            "SELECT value_json FROM feature_flags WHERE key = 'extension.minVersion'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, "\"0.3.0\"");

    // A key another build left is listed as unknown, and can be removed.
    control_db(&t)
        .execute(
            "INSERT INTO feature_flags (key, value_json, updated_at) \
             VALUES ('extension.retired', 'true', 1)",
            [],
        )
        .unwrap();
    let listed = admin_flags::list(&data).unwrap();
    assert_eq!(listed.len(), 21);
    let sources: Vec<(&str, &str)> = listed
        .iter()
        .filter(|line| line.source != "default")
        .map(|line| (line.key.as_str(), line.source))
        .collect();
    assert_eq!(
        sources,
        [
            ("extension.minVersion", "set"),
            ("extension.instagram.replay", "set"),
            ("extension.retired", "unknown"),
        ]
    );
    let mut out = Vec::new();
    let list = FlagsArgs {
        command: FlagsCommand::List,
    };
    admin_flags::run(&data, &list, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.lines().count(), 21);
    assert!(text.contains("extension.instagram.replay"), "{text}");

    assert!(admin_flags::unset(&data, "extension.instagram.replay").unwrap());
    assert!(!admin_flags::unset(&data, "extension.instagram.replay").unwrap());
    assert!(admin_flags::unset(&data, "extension.retired").unwrap());
    assert!(admin_flags::unset(&data, "extension.never-was").is_err());
    assert_eq!(
        admin_flags::get(&data, "extension.instagram.replay").unwrap(),
        json!(true)
    );

    let conn = control_db(&t);
    assert_eq!(
        audit_rows(&conn, "flag.set"),
        [
            json!({ "key": "extension.instagram.replay", "value": false }),
            json!({ "key": "extension.minVersion", "value": "0.3.0" }),
        ]
    );
    assert_eq!(
        audit_rows(&conn, "flag.unset"),
        [
            json!({ "key": "extension.instagram.replay" }),
            json!({ "key": "extension.retired" }),
        ]
    );
}

/// The next `extension.status` of `subscription`, as JSON; other events are
/// skipped.
async fn next_status(subscription: &mut Subscription) -> Value {
    loop {
        match subscription.next().await {
            Delivery::Event(event) if event.topic == EventTopic::ExtensionStatus => {
                return serde_json::from_str(&event.data).unwrap();
            }
            Delivery::Event(_) => {}
            Delivery::Resync { reason, .. } => panic!("unexpected resync: {reason:?}"),
            Delivery::Live(_) => {}
        }
    }
}

/// Whether `subscription` stays without an `extension.status` for a second
/// (of tokio's clock).
async fn no_status(subscription: &mut Subscription) -> bool {
    tokio::time::timeout(Duration::from_secs(1), next_status(subscription))
        .await
        .is_err()
}

#[tokio::test(start_paused = true)]
async fn presence_follows_the_extension_requests() {
    let start = now_ms();
    let t = state(|config: &mut Config| {
        config.extension.clock = Clock::tokio(start);
    });
    let app = t.app();
    let owner_id = owner(&t);
    let member_id = add_member(&t, MEMBER_EMAIL);
    let member_cookie = sign_in_as(&app, &t, MEMBER_EMAIL).await;
    let mut events = t.state.events().subscribe(&owner_id, None);
    let mut member_events = t.state.events().subscribe(&member_id, None);
    let (cookie, token, token_id) = paired(&app, &t, INSTALL).await;
    assert_eq!(
        status(&app, &cookie).await,
        json!({ "connected": false, "version": null, "lastSeenAt": null }),
        "pairing alone is no request of the extension"
    );

    // The first request connects it.
    let response = send(&app, ext(get(CONFIG), &token, Some("0.2.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let connected = next_status(&mut events).await;
    let first_seen = connected["lastSeenAt"].as_i64().unwrap();
    assert!(first_seen >= start, "{first_seen}");
    assert_eq!(
        connected,
        json!({ "connected": true, "version": "0.2.0", "lastSeenAt": first_seen })
    );
    assert_eq!(status(&app, &cookie).await, connected);
    assert_schema(&connected, "ExtensionStatusEvent");

    // More requests of the same version announce nothing; a new version
    // does, and a request refused as outdated still counts as a request.
    tokio::time::advance(Duration::from_secs(60)).await;
    let response = send(&app, ext(lookup(), &token, Some("0.2.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(no_status(&mut events).await);
    let response = send(&app, ext(lookup(), &token, Some("0.3.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let newer = next_status(&mut events).await;
    assert_eq!(newer["connected"], true);
    assert_eq!(newer["version"], "0.3.0");
    let response = send(&app, ext(lookup(), &token, Some("0.0.1"))).await;
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    assert_eq!(next_status(&mut events).await["version"], "0.0.1");
    let response = send(&app, ext(lookup(), &token, Some("0.3.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let last = next_status(&mut events).await;
    assert_eq!(last["version"], "0.3.0");
    let last_seen = last["lastSeenAt"].as_i64().unwrap();

    // Silent for less than 10 minutes: still connected at the sweep.
    tokio::time::advance(Duration::from_secs(9 * 60)).await;
    extension::sweep(&t.state);
    assert!(no_status(&mut events).await);
    // 10 minutes: the sweep disconnects it, keeping the last version and time.
    tokio::time::advance(Duration::from_secs(59)).await;
    extension::sweep(&t.state);
    let disconnected = next_status(&mut events).await;
    assert_eq!(
        disconnected,
        json!({ "connected": false, "version": "0.3.0", "lastSeenAt": last_seen })
    );
    assert_eq!(status(&app, &cookie).await, disconnected);
    extension::sweep(&t.state);
    assert!(no_status(&mut events).await, "announced once");

    // The next request reconnects; revoking the token disconnects at once.
    let response = send(&app, ext(get(CONFIG), &token, Some("0.3.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(next_status(&mut events).await["connected"], true);
    let revoke = Request::delete(format!("/api/v1/me/tokens/{token_id}"))
        .body(Body::empty())
        .unwrap();
    let response = send(&app, spa(&t, revoke, &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(next_status(&mut events).await["connected"], false);

    // None of it reached another account.
    assert!(no_status(&mut member_events).await);
    assert_eq!(
        status(&app, &member_cookie).await,
        json!({ "connected": false, "version": null, "lastSeenAt": null })
    );
}

#[tokio::test]
async fn every_extension_token_request_marks_the_extension_present() {
    let t = state(|_| {});
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    // A token minted from the account with `lookup` only: refused on the
    // config, which needs `ingest`, yet a request of the extension.
    let lookup_only = insert_token(&t, &owner_id, "extension", "lookup");
    refused(
        send(&app, ext(get(CONFIG), &lookup_only, Some("0.2.0"))).await,
        StatusCode::FORBIDDEN,
        ErrorCode::Forbidden,
    )
    .await;
    let seen = status(&app, &cookie).await;
    assert_eq!(seen["connected"], true);
    assert_eq!(seen["version"], "0.2.0");
    // Tokens of other kinds never do.
    let t = state(|_| {});
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let shortcut = insert_token(&t, &owner_id, "shortcut", "lookup links:create");
    let response = send(&app, ext(lookup(), &shortcut, Some("0.2.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(status(&app, &cookie).await["connected"], false);
}

#[tokio::test]
async fn the_web_app_hears_the_status_on_its_stream() {
    let t = state(|_| {});
    let app = t.app();
    let (cookie, token, _id) = paired(&app, &t, INSTALL).await;
    let session = format!("{}={cookie}", shelfy_server::auth::cookie::SESSION_COOKIE);
    let mut stream = Stream::connect(
        &app,
        "/api/v1/events?topics=extension.status",
        &[("cookie", session.as_str())],
    )
    .await;
    stream.hello().await;
    let response = send(&app, ext(get(CONFIG), &token, Some("0.2.0"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let frame = loop {
        let frame = stream.next().await;
        if !frame.is_heartbeat() {
            break frame;
        }
    };
    assert_eq!(frame.name(), "extension.status");
    assert_event_schema(&frame);
    assert_eq!(frame.json()["connected"], true);
    assert_eq!(frame.json()["version"], "0.2.0");
}

#[tokio::test]
async fn every_extension_route_has_its_access() {
    let t = state(|_| {});
    let app = t.app();
    let owner_id = owner(&t);
    let (cookie, token, _id) = paired(&app, &t, INSTALL).await;
    add_member(&t, MEMBER_EMAIL);
    let member_cookie = sign_in_as(&app, &t, MEMBER_EMAIL).await;
    let member_token = {
        let code = code(&app, &t, &member_cookie).await;
        pair(&app, &code, OTHER_INSTALL).await["token"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let unknown = SecretToken::generate();

    // GET /extension/config: an API token with `ingest`, nothing else.
    let response = send(&app, get(CONFIG)).await;
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    refused(response, StatusCode::UNAUTHORIZED, ErrorCode::Unauthorized).await;
    for request in [
        with_session(get(CONFIG), &cookie),
        with_session(get(CONFIG), unknown.expose()),
        ext(
            get(CONFIG),
            &format!("shx_{}", unknown.expose()),
            Some("0.2.0"),
        ),
    ] {
        refused(
            send(&app, request).await,
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
        )
        .await;
    }
    let shortcut = insert_token(&t, &owner_id, "shortcut", "links:create");
    refused(
        send(&app, ext(get(CONFIG), &shortcut, Some("0.2.0"))).await,
        StatusCode::FORBIDDEN,
        ErrorCode::Forbidden,
    )
    .await;
    // The same document for every account: nothing in it is per user.
    let owners = json(send(&app, ext(get(CONFIG), &token, Some("0.2.0"))).await).await;
    let members = json(send(&app, ext(get(CONFIG), &member_token, Some("0.2.0"))).await).await;
    assert_eq!(owners, members);

    // GET /extension/status: a session, never a token; one's own only.
    for request in [
        get(STATUS),
        with_session(get(STATUS), unknown.expose()),
        ext(get(STATUS), &token, Some("0.2.0")),
        ext(with_session(get(STATUS), &cookie), &token, Some("0.2.0")),
    ] {
        refused(
            send(&app, request).await,
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
        )
        .await;
    }
    assert_eq!(status(&app, &cookie).await["connected"], true);
    let revoke = |id: &str| {
        let request = Request::delete(format!("/api/v1/me/tokens/{id}"))
            .body(Body::empty())
            .unwrap();
        spa(&t, request, &member_cookie)
    };
    // The member's pairing never touched the owner's presence, and the
    // owner's token is not the member's to revoke.
    let owner_token_id = token_list(&app, &cookie).await[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    refused(
        send(&app, revoke(&owner_token_id)).await,
        StatusCode::NOT_FOUND,
        ErrorCode::NotFound,
    )
    .await;
    assert_eq!(status(&app, &cookie).await["connected"], true);

    // POST /me/tokens/pairing-code: a session with the app's headers.
    for request in [
        from_spa(&t, post(PAIRING_CODE)),
        ext(from_spa(&t, post(PAIRING_CODE)), &token, Some("0.2.0")),
        ext(spa(&t, post(PAIRING_CODE), &cookie), &token, Some("0.2.0")),
    ] {
        refused(
            send(&app, request).await,
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
        )
        .await;
    }

    // POST /extension/pair: public, the code is the credential; a token
    // beside it changes nothing.
    let code = code(&app, &t, &cookie).await;
    let request = ext(
        pair_request(&pair_body(&code, INSTALL, "0.2.0")),
        &member_token,
        Some("0.2.0"),
    );
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let answer = json(response).await;
    let owner_ids: Vec<Value> = token_list(&app, &cookie)
        .await
        .into_iter()
        .filter(|item| item["kind"] == "extension")
        .map(|item| item["id"].clone())
        .collect();
    assert_eq!(
        owner_ids,
        [answer["tokenId"].clone()],
        "the owner's, re-paired"
    );
    assert_eq!(token_list(&app, &member_cookie).await.len(), 1);

    // An unsupported method on these routes is a 405 for those who may call
    // them, a 401 for anyone else.
    let put_config = Request::builder()
        .method(Method::PUT)
        .uri(CONFIG)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&app, from_spa(&t, put_config)).await.status(),
        StatusCode::UNAUTHORIZED
    );
}
