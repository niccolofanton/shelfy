//! The migration CLI's device sign-in (RFC 8628) through the real middleware
//! stack (plan §2.11, §4.1 step 2; P1-17): start, approval by a signed-in
//! user who re-authenticated, the poll that delivers a `migrate` token once,
//! the token on the migration routes and nowhere else, pacing (`slow_down`),
//! expiry, replays, the limit on user codes, the CSRF rules and per-route
//! access. The logs are checked in `auth_logs.rs`.

mod support;

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use shelfy_server::admin::login_link::{LinkPurpose, create_link};
use shelfy_server::config::Config;
use shelfy_server::error::ErrorCode;
use shelfy_server::ids::now_ms;
use shelfy_server::tokens::{SecretToken, is_token_shaped};
use support::auth::{
    OWNER_EMAIL, add_member, audit_rows, control_db, from_spa, make_sessions_stale, owner, sign_in,
    sign_in_as, spa, with_session,
};
use support::{TestState, get, json, problem, send};

const MEMBER_EMAIL: &str = "member@example.test";
const WEEK_MS: i64 = 7 * 86_400_000;

/// A state whose sign-in limit leaves room for many requests, with polls
/// allowed back to back. `edit` adjusts the rest.
fn state(edit: impl FnOnce(&mut Config)) -> TestState {
    TestState::with_config(|config: &mut Config| {
        config.auth.ip_limit.max = 1_000;
        config.auth.device_poll_interval = Duration::ZERO;
        edit(config);
    })
}

/// A `POST` as the CLI sends it: JSON, no cookie, no `Origin`, no
/// `X-Shelfy-Client`.
fn cli_post(uri: &str, body: Option<&Value>) -> Request<Body> {
    let builder = Request::post(uri).header(header::USER_AGENT, "shelfy-migrate/0.1");
    match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => builder.body(Body::empty()),
    }
    .unwrap()
}

/// `POST /auth/device/start` from the CLI; checks 200.
async fn start(app: &Router) -> Value {
    let response = send(app, cli_post("/api/v1/auth/device/start", None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    json(response).await
}

/// `POST /auth/device/poll` from the CLI.
async fn poll(app: &Router, device_code: &Value) -> axum::http::Response<Body> {
    let body = json!({ "deviceCode": device_code });
    send(app, cli_post("/api/v1/auth/device/poll", Some(&body))).await
}

/// `POST /auth/device/approve` as the web app's `/device` page sends it.
fn approve(t: &TestState, cookie: &str, user_code: &str) -> Request<Body> {
    let request = Request::post("/api/v1/auth/device/approve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "userCode": user_code }).to_string()))
        .unwrap();
    spa(t, request, cookie)
}

/// Re-authenticates the session `cookie` of `email` with a link from `admin
/// login-link --purpose reauth`.
async fn reauth(app: &Router, t: &TestState, cookie: &str, email: &str) {
    let link = create_link(
        &t.data_dir(),
        &t.state.config().public_url,
        email,
        LinkPurpose::Reauth,
        Duration::from_secs(900),
    )
    .unwrap();
    let token = link.url.expose().split_once('#').unwrap().1.to_owned();
    let request = Request::post("/api/v1/auth/reauth/finish")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "method": "link", "token": token }).to_string(),
        ))
        .unwrap();
    let response = send(app, spa(t, request, cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "re-auth");
}

fn bearer(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    request
}

/// Refused with 400 `invalid_device_code`.
async fn invalid(response: axum::http::Response<Body>) {
    let problem = problem(response, StatusCode::BAD_REQUEST).await;
    assert_eq!(problem.code, ErrorCode::InvalidDeviceCode);
}

#[tokio::test]
async fn the_cli_signs_in_with_a_device_code() {
    let t = state(|_| {});
    let app = t.app();
    let owner_id = owner(&t);

    // The CLI starts: codes, the page, the timing.
    let started = start(&app).await;
    let device_code = started["deviceCode"].clone();
    assert!(is_token_shaped(device_code.as_str().unwrap()));
    let user_code = started["userCode"].as_str().unwrap().to_owned();
    assert_eq!(user_code.len(), 9);
    assert_eq!(&user_code[4..5], "-");
    assert!(
        user_code
            .bytes()
            .filter(|b| *b != b'-')
            .all(|b| b"BCDFGHJKLMNPQRSTVWXZ".contains(&b))
    );
    let page = format!("{}/device", t.state.config().public_url);
    assert_eq!(started["verificationUri"], page.as_str());
    assert_eq!(
        started["verificationUriComplete"],
        format!("{page}#{user_code}").as_str()
    );
    assert_eq!(started["expiresIn"], 600);
    assert_eq!(started["interval"], 0, "as configured here");
    let response = poll(&app, &device_code).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        json(response).await,
        json!({ "status": "pending", "interval": 0 })
    );

    // The owner approves it in the web app, after a re-authentication.
    let cookie = sign_in(&app, &t).await;
    make_sessions_stale(&t);
    let response = send(&app, approve(&t, &cookie, &user_code)).await;
    assert_eq!(
        problem(response, StatusCode::FORBIDDEN).await.code,
        ErrorCode::ReauthRequired
    );
    assert_eq!(
        json(poll(&app, &device_code).await).await["status"],
        "pending"
    );
    reauth(&app, &t, &cookie, OWNER_EMAIL).await;
    let typed = user_code.to_ascii_lowercase().replace('-', " ");
    let response = send(&app, approve(&t, &cookie, &typed)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

    // The next poll delivers the token.
    let response = poll(&app, &device_code).await;
    assert_eq!(response.status(), StatusCode::OK);
    let approved = json(response).await;
    assert_eq!(approved["status"], "approved");
    assert_eq!(approved["scopes"], json!(["migrate"]));
    let token = approved["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("shx_") && is_token_shaped(&token[4..]));
    let expires_at = approved["expiresAt"].as_i64().unwrap();
    assert!((expires_at - now_ms() - WEEK_MS).abs() < 60_000, "7 days");

    // It works on the migration routes, for the approver's library.
    let missing = Request::post("/api/v1/migrations/missing-objects")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "objects": [] }).to_string()))
        .unwrap();
    let response = send(&app, bearer(missing, &token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await, json!({ "missing": [] }));
    // And nowhere else.
    let response = send(&app, bearer(get("/api/v1/me"), &token)).await;
    problem(response, StatusCode::UNAUTHORIZED).await;
    let lookup = Request::post("/api/v1/posts/lookup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "platform": "twitter", "keys": ["1"] }).to_string(),
        ))
        .unwrap();
    let response = send(&app, bearer(lookup, &token)).await;
    assert_eq!(
        problem(response, StatusCode::FORBIDDEN).await.code,
        ErrorCode::Forbidden
    );

    // The account lists it, and the audit log has both steps.
    let response = send(&app, with_session(get("/api/v1/me/tokens"), &cookie)).await;
    let tokens = json(response).await;
    let listed = &tokens["items"][0];
    assert_eq!(listed["id"], approved["tokenId"]);
    assert_eq!(listed["kind"], "migrate");
    assert_eq!(listed["label"], "shelfy-migrate");
    assert_eq!(listed["expiresAt"], expires_at);
    assert!(listed["lastUsedAt"].is_i64(), "used on missing-objects");
    let conn = control_db(&t);
    assert_eq!(
        audit_rows(&conn, "device.approve"),
        [json!({ "scope": "migrate" })]
    );
    assert_eq!(
        audit_rows(&conn, "api_token.create"),
        [json!({ "id": approved["tokenId"], "kind": "migrate", "via": "device" })]
    );
    let actors: Vec<String> = conn
        .prepare(
            "SELECT actor_user_id FROM audit_log \
             WHERE action IN ('device.approve', 'api_token.create') ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(actors, [owner_id.clone(), owner_id]);

    // Replays: the codes are spent.
    invalid(poll(&app, &device_code).await).await;
    invalid(send(&app, approve(&t, &cookie, &user_code)).await).await;
}

#[tokio::test]
async fn polling_follows_the_interval() {
    let t = TestState::with_config(|config: &mut Config| {
        config.auth.ip_limit.max = 1_000;
    });
    let app = t.app();
    let started = start(&app).await;
    assert_eq!(started["interval"], 5, "RFC 8628's default");
    assert_eq!(started["expiresIn"], 600, "10 minutes");
    let device_code = &started["deviceCode"];
    assert_eq!(
        json(poll(&app, device_code).await).await,
        json!({ "status": "pending", "interval": 5 })
    );
    // Back to back: slow down, the interval grows by 5 seconds.
    let response = poll(&app, device_code).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json(response).await,
        json!({ "status": "slow_down", "interval": 10 })
    );
    assert_eq!(
        json(poll(&app, device_code).await).await,
        json!({ "status": "slow_down", "interval": 15 })
    );
}

#[tokio::test]
async fn codes_expire() {
    let t = state(|config: &mut Config| {
        config.auth.device_code_ttl = Duration::from_millis(300);
    });
    let app = t.app();
    let cookie = sign_in(&app, &t).await;

    // Unapproved: both codes die.
    let started = start(&app).await;
    assert_eq!(started["expiresIn"], 0);
    tokio::time::sleep(Duration::from_millis(400)).await;
    invalid(
        send(
            &app,
            approve(&t, &cookie, started["userCode"].as_str().unwrap()),
        )
        .await,
    )
    .await;
    invalid(poll(&app, &started["deviceCode"]).await).await;

    // Approved but never collected: no token after the expiry.
    let started = start(&app).await;
    let response = send(
        &app,
        approve(&t, &cookie, started["userCode"].as_str().unwrap()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    tokio::time::sleep(Duration::from_millis(400)).await;
    invalid(poll(&app, &started["deviceCode"]).await).await;
    let tokens: i64 = control_db(&t)
        .query_row("SELECT count(*) FROM api_tokens", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tokens, 0);
}

#[tokio::test]
async fn unknown_and_malformed_codes_are_refused() {
    let t = state(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let _ = start(&app).await;
    let unknown = SecretToken::generate();
    for code in [json!(unknown.expose()), json!("short"), json!("")] {
        invalid(poll(&app, &code).await).await;
    }
    for code in ["BCDF-GHJK", "nonsense", "", &"B".repeat(100)] {
        invalid(send(&app, approve(&t, &cookie, code)).await).await;
    }
    let response = send(&app, cli_post("/api/v1/auth/device/poll", Some(&json!({})))).await;
    problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
}

#[tokio::test]
async fn user_code_guesses_are_limited_per_account() {
    let t = state(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    owner(&t);
    add_member(&t, MEMBER_EMAIL);
    let member = sign_in_as(&app, &t, MEMBER_EMAIL).await;
    let started = start(&app).await;
    let user_code = started["userCode"].as_str().unwrap().to_owned();
    let wrong = if user_code == "BCDF-GHJK" {
        "BCDF-GHJL"
    } else {
        "BCDF-GHJK"
    };
    // 10 tries per 10 minutes: then even the right code waits.
    for _ in 0..10 {
        invalid(send(&app, approve(&t, &member, wrong)).await).await;
    }
    let response = send(&app, approve(&t, &member, &user_code)).await;
    assert!(response.headers().contains_key(header::RETRY_AFTER));
    assert_eq!(
        problem(response, StatusCode::TOO_MANY_REQUESTS).await.code,
        ErrorCode::RateLimited
    );
    // Per account: the owner still approves.
    let response = send(&app, approve(&t, &cookie, &user_code)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_code_belongs_to_the_account_that_approved_it() {
    let t = state(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    add_member(&t, MEMBER_EMAIL);
    let member = sign_in_as(&app, &t, MEMBER_EMAIL).await;
    let started = start(&app).await;
    let user_code = started["userCode"].as_str().unwrap();

    let response = send(&app, approve(&t, &member, user_code)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    // Someone else cannot take it over; the approver may click twice.
    invalid(send(&app, approve(&t, &cookie, user_code)).await).await;
    let response = send(&app, approve(&t, &member, user_code)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(audit_rows(&control_db(&t), "device.approve").len(), 1);

    // The token acts for the member.
    let approved = json(poll(&app, &started["deviceCode"]).await).await;
    assert_eq!(approved["status"], "approved");
    let tokens = |cookie: &str| {
        let request = with_session(get("/api/v1/me/tokens"), cookie);
        let app = app.clone();
        async move { json(send(&app, request).await).await["items"].clone() }
    };
    assert_eq!(tokens(&member).await[0]["id"], approved["tokenId"]);
    assert_eq!(tokens(&cookie).await, json!([]));
}

#[tokio::test]
async fn the_cli_needs_no_csrf_headers_but_the_approval_does() {
    let t = state(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;

    // The CLI, even with a foreign origin, starts and polls.
    let mut cross_site = cli_post("/api/v1/auth/device/start", None);
    cross_site
        .headers_mut()
        .insert(header::ORIGIN, "https://evil.example.test".parse().unwrap());
    cross_site
        .headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    let response = send(&app, cross_site).await;
    assert_eq!(response.status(), StatusCode::OK);
    let started = json(response).await;
    let user_code = started["userCode"].as_str().unwrap();

    // The approval is a cookie request like any other.
    let forged = [
        Request::post("/api/v1/auth/device/approve")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "userCode": user_code }).to_string()))
            .unwrap(),
        {
            let mut request = approve(&t, &cookie, user_code);
            request.headers_mut().remove("x-shelfy-client");
            request
        },
        {
            let mut request = approve(&t, &cookie, user_code);
            request
                .headers_mut()
                .insert(header::ORIGIN, "https://evil.example.test".parse().unwrap());
            request
        },
    ];
    for request in forged {
        let request = with_session(request, &cookie);
        let refused = problem(send(&app, request).await, StatusCode::FORBIDDEN).await;
        assert_eq!(refused.code, ErrorCode::CsrfFailed);
    }
    assert_eq!(
        json(poll(&app, &started["deviceCode"]).await).await["status"],
        "pending",
        "nothing approved"
    );
}

/// `request` as if it came over TCP from `peer`.
fn from_peer(mut request: Request<Body>, peer: &str) -> Request<Body> {
    let ip = peer.parse().unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip, 40_000)));
    request
}

#[tokio::test]
async fn polls_skip_the_sign_in_limit_and_are_paced_per_device_code() {
    let t = TestState::with_config(|config: &mut Config| {
        config.auth.device_poll_interval = Duration::ZERO;
    });
    let app = t.app();
    let start_from = |peer: &str| from_peer(cli_post("/api/v1/auth/device/start", None), peer);
    let poll_from = |device_code: &Value, peer: &str| {
        let body = json!({ "deviceCode": device_code });
        from_peer(cli_post("/api/v1/auth/device/poll", Some(&body)), peer)
    };
    // Starting counts like every sign-in request: 10 a minute per client.
    let mut device_code = Value::Null;
    for _ in 0..10 {
        let response = send(&app, start_from("198.51.100.7")).await;
        assert_eq!(response.status(), StatusCode::OK);
        device_code = json(response).await["deviceCode"].clone();
    }
    let response = send(&app, start_from("198.51.100.7")).await;
    assert!(response.headers().contains_key(header::RETRY_AFTER));
    problem(response, StatusCode::TOO_MANY_REQUESTS).await;
    let other = json(send(&app, start_from("198.51.100.8")).await).await["deviceCode"].clone();

    // Polls are not counted: the client is over the sign-in limit, and its
    // CLI still polls. A device code takes 20 polls a minute.
    for i in 0..20 {
        let response = send(&app, poll_from(&device_code, "198.51.100.7")).await;
        assert_eq!(response.status(), StatusCode::OK, "poll {i}");
    }
    let response = send(&app, poll_from(&device_code, "198.51.100.7")).await;
    let wait: u64 = response.headers()[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    let refused = problem(response, StatusCode::TOO_MANY_REQUESTS).await;
    assert_eq!(refused.code, ErrorCode::RateLimited);
    assert_eq!(
        refused.detail.as_deref(),
        Some("too many polls of this device code")
    );
    // Another device code, from the same client, has its own pace.
    let response = send(&app, poll_from(&other, "198.51.100.7")).await;
    assert_eq!(json(response).await["status"], "pending");
}

/// `POST /auth/reauth/finish` with the link `token`, from the web app.
fn reauth_with(t: &TestState, cookie: &str, token: &str) -> Request<Body> {
    let request = Request::post("/api/v1/auth/reauth/finish")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "method": "link", "token": token }).to_string(),
        ))
        .unwrap();
    spa(t, request, cookie)
}

#[tokio::test]
async fn approvals_that_need_a_reauth_do_not_spend_the_sign_in_limit() {
    // F10: the owner clicked Approve a few times while the session had to
    // re-authenticate, and got 429 before the re-authentication.
    let t = TestState::with_config(|config: &mut Config| {
        config.auth.device_poll_interval = Duration::ZERO;
    });
    let app = t.app();
    let client = "198.51.100.7";
    let started = json(
        send(
            &app,
            from_peer(cli_post("/api/v1/auth/device/start", None), client),
        )
        .await,
    )
    .await;
    let user_code = started["userCode"].as_str().unwrap().to_owned();
    let cookie = sign_in(&app, &t).await;
    make_sessions_stale(&t);

    // Twice the limit of approvals answer `reauth_required`, never 429.
    for i in 0..20 {
        let response = send(&app, from_peer(approve(&t, &cookie, &user_code), client)).await;
        let refused = problem(response, StatusCode::FORBIDDEN).await;
        assert_eq!(refused.code, ErrorCode::ReauthRequired, "approval {i}");
    }

    // The re-authentication and the approval still pass.
    let link = create_link(
        &t.data_dir(),
        &t.state.config().public_url,
        OWNER_EMAIL,
        LinkPurpose::Reauth,
        Duration::from_secs(900),
    )
    .unwrap();
    let token = link.url.expose().split_once('#').unwrap().1.to_owned();
    let response = send(&app, from_peer(reauth_with(&t, &cookie, &token), client)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "re-auth");
    let response = send(&app, from_peer(approve(&t, &cookie, &user_code), client)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "approval");
    let approved = json(poll(&app, &started["deviceCode"]).await).await;
    assert_eq!(approved["status"], "approved");

    // Real failures still count: start, re-auth and approval used 3 of the
    // 10, so the 8th failed re-authentication is refused.
    let unknown = SecretToken::generate();
    for i in 0..7 {
        let request = from_peer(reauth_with(&t, &cookie, unknown.expose()), client);
        let refused = problem(send(&app, request).await, StatusCode::BAD_REQUEST).await;
        assert_eq!(refused.code, ErrorCode::InvalidLink, "failure {i}");
    }
    let request = from_peer(reauth_with(&t, &cookie, unknown.expose()), client);
    let refused = problem(send(&app, request).await, StatusCode::TOO_MANY_REQUESTS).await;
    assert_eq!(refused.code, ErrorCode::RateLimited);
    assert_eq!(
        refused.detail.as_deref(),
        Some("too many sign-in requests from this address")
    );
}

#[tokio::test]
async fn every_device_route_has_its_access() {
    let t = state(|_| {});
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let started = start(&app).await;
    let user_code = started["userCode"].as_str().unwrap();
    let every = format!("shx_{}", SecretToken::generate().expose());
    control_db(&t)
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
             VALUES ('01TOKEN0000000000000000000', ?1, 'extension', ?2, \
             'ingest tasks uploads lookup links:create migrate', ?3)",
            rusqlite::params![
                owner_id,
                shelfy_server::tokens::hash_token(&every).as_slice(),
                now_ms()
            ],
        )
        .unwrap();

    // The approval needs a session: none, an unknown one, or a token beside
    // a valid cookie are refused.
    let unknown = SecretToken::generate();
    let no_session = from_spa(
        &t,
        Request::post("/api/v1/auth/device/approve")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "userCode": user_code }).to_string()))
            .unwrap(),
    );
    let mut with_token = approve(&t, &cookie, user_code);
    with_token.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {every}").parse().unwrap(),
    );
    for (what, request) in [
        ("no session", no_session),
        (
            "an unknown session",
            approve(&t, unknown.expose(), user_code),
        ),
        ("a token beside a valid cookie", with_token),
    ] {
        let refused = problem(send(&app, request).await, StatusCode::UNAUTHORIZED).await;
        assert_eq!(refused.code, ErrorCode::Unauthorized, "{what}");
    }
    assert_eq!(
        json(poll(&app, &started["deviceCode"]).await).await["status"],
        "pending"
    );
    // The CLI's routes are public: no session, no token.
    let response = send(&app, cli_post("/api/v1/auth/device/start", None)).await;
    assert_eq!(response.status(), StatusCode::OK);
}
