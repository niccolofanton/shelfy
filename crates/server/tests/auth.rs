//! Owner sign-in through the real middleware stack (plan §2.11, §7.1, E4):
//! email links end to end (dev mailbox and SMTP), `admin login-link`,
//! sessions (cookie flags, expiry, rotation, sign-out), the CSRF guard,
//! bearer requests, re-authentication, rate limits, no account enumeration,
//! and a 401 for every protected route without a session. The logs are
//! checked in `auth_logs.rs`.

mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode, header};
use axum::routing::{get as get_route, post as post_route};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use shelfy_server::auth::RecentAuth;
use shelfy_server::auth::bearer::{TokenUser, scopes};
use shelfy_server::config::Config;
use shelfy_server::current_user::CurrentUser;
use shelfy_server::error::ErrorCode;
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::limits::RouteLimits;
use shelfy_server::mail::{MailConfig, SmtpConfig, SmtpTls};
use shelfy_server::telemetry::http::REQUEST_ID_HEADER;
use shelfy_server::tokens::{SecretToken, hash_token};
use shelfy_server::{app, routes};
use support::auth::{
    LINK_PATH, OWNER_EMAIL, link_in, link_token, mailbox, mailbox_dir, owner, post, session_cookie,
    sign_in, spa, with_session,
};
use support::{TestState, body, get, json, post_json, problem, send};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpListener;
use utoipa_axum::router::OpenApiRouter;

const DAY_MS: i64 = 86_400_000;
const MINUTE_MS: i64 = 60_000;

/// A state with the dev mailbox on.
fn with_mailbox() -> TestState {
    TestState::with_config(|config: &mut Config| {
        config.mail = MailConfig::dev_mailbox(&config.data_dir);
    })
}

/// A read-write connection to the control database, beside the server's.
fn control(t: &TestState) -> Connection {
    let conn = Connection::open(t.data_dir().control_db()).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn audit(conn: &Connection, action: &str) -> Vec<Value> {
    let mut statement = conn
        .prepare("SELECT meta_json FROM audit_log WHERE action = ?1 ORDER BY id")
        .unwrap();
    statement
        .query_map([action], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|meta| serde_json::from_str(&meta.unwrap()).unwrap())
        .collect()
}

async fn me(app: &Router, cookie: &str) -> StatusCode {
    send(app, with_session(get("/api/v1/me"), cookie))
        .await
        .status()
}

fn with_ip(mut request: Request<Body>, ip: &str) -> Request<Body> {
    request
        .headers_mut()
        .insert("cf-connecting-ip", ip.parse().unwrap());
    request
}

fn email_request(email: &str) -> Request<Body> {
    post_json(
        "/api/v1/auth/magic-links",
        json!({ "email": email }).to_string(),
    )
}

#[tokio::test]
async fn the_owner_signs_in_with_an_emailed_link() {
    let t = with_mailbox();
    let app = t.app();
    let owner_id = owner(&t);

    let methods = send(&app, get("/api/v1/auth/methods")).await;
    assert_eq!(methods.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        json(methods).await,
        json!({ "emailLink": true, "passkeys": false })
    );

    let response = send(&app, email_request("  Owner@Example.TEST ")).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert!(body(response).await.is_empty());

    let messages = mailbox(&t).await;
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert!(message.contains("To: owner@example.test"), "{message}");
    assert!(
        message.contains("Subject: Your Shelfy sign-in link"),
        "{message}"
    );
    assert!(message.contains("expires in 15 minutes"), "{message}");
    let url = link_in(message);
    let path = url
        .strip_prefix(t.state.config().public_url.as_str())
        .expect("the link is on the public URL");
    assert!(path.starts_with(LINK_PATH), "{path}");

    let response = send(&app, get(path)).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], "/");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
    let cookie = session_cookie(&response).expect("a session cookie");

    let response = send(&app, with_session(get("/api/v1/me"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let me = json(response).await;
    assert_eq!(me["id"], owner_id.as_str());
    assert_eq!(me["email"], OWNER_EMAIL);
    assert_eq!(me["role"], "owner");
    assert!(me["createdAt"].as_i64().unwrap() > 0);

    let conn = control(&t);
    assert_eq!(
        audit(&conn, "magic_link.create"),
        [json!({ "via": "email", "purpose": "login" })]
    );
    assert_eq!(
        audit(&conn, "session.create"),
        [json!({ "method": "magic_link", "rotated": false })]
    );
    let used: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM magic_links WHERE used_at IS NOT NULL",
    );
    assert_eq!(used, 1);
}

#[tokio::test]
async fn without_email_the_cli_link_is_the_way_in() {
    let t = TestState::new();
    let app = t.app();
    owner(&t);

    assert_eq!(
        json(send(&app, get("/api/v1/auth/methods")).await).await,
        json!({ "emailLink": false, "passkeys": false })
    );
    // Still 202, and nothing is minted or written.
    let response = send(&app, email_request(OWNER_EMAIL)).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(mailbox(&t).await.is_empty());
    assert!(!mailbox_dir(&t).exists());
    assert_eq!(count(&control(&t), "SELECT COUNT(*) FROM magic_links"), 0);

    let cookie = sign_in(&app, &t).await;
    assert_eq!(me(&app, &cookie).await, StatusCode::OK);
    assert_eq!(
        audit(&control(&t), "magic_link.create"),
        [json!({ "via": "cli", "purpose": "login" })]
    );
}

#[tokio::test]
async fn the_session_cookie_is_host_only_secure_and_stored_as_a_hash() {
    let t = TestState::new();
    let app = t.app();
    owner(&t);
    let token = link_token(&t, OWNER_EMAIL);
    let request = Request::get(format!("{LINK_PATH}{token}"))
        .header(header::USER_AGENT, "TestBrowser/1.0")
        .body(Body::empty())
        .unwrap();
    let response = send(&app, request).await;
    let set_cookies: Vec<&str> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert_eq!(set_cookies.len(), 1);
    let set_cookie = set_cookies[0];
    let mut parts = set_cookie.split("; ");
    let (name, value) = parts.next().unwrap().split_once('=').unwrap();
    assert_eq!(name, "__Host-shelfy_session");
    assert_eq!(value.len(), 43, "256 bits, base64url");
    let attributes: BTreeSet<&str> = parts.collect();
    assert_eq!(
        attributes,
        BTreeSet::from([
            "Path=/",
            "Max-Age=7776000",
            "HttpOnly",
            "Secure",
            "SameSite=Lax"
        ]),
        "{set_cookie}"
    );
    assert!(!set_cookie.contains("Domain"), "host-only");

    let conn = control(&t);
    let (stored, user_agent): (i64, String) = conn
        .query_row(
            "SELECT COUNT(*), user_agent FROM sessions WHERE id_hash = ?1",
            [hash_token(value).as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, 1);
    assert_eq!(user_agent, "TestBrowser/1.0");
    let plain: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE CAST(id_hash AS TEXT) = ?1",
            [value],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(plain, 0, "the cookie value is stored nowhere");
    let (created, expires, seen, reauth): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT created_at, expires_at, last_seen_at, reauth_at FROM sessions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(expires - created, 90 * DAY_MS);
    assert_eq!((seen, reauth), (created, created));
}

#[tokio::test]
async fn a_link_works_once_and_head_does_not_use_it() {
    let t = TestState::new();
    let app = t.app();
    owner(&t);
    let token = link_token(&t, OWNER_EMAIL);
    let path = format!("{LINK_PATH}{token}");

    // Link checkers probe with HEAD.
    let head = Request::head(&path).body(Body::empty()).unwrap();
    let response = send(&app, head).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(session_cookie(&response).is_none());

    let first = send(&app, get(&path)).await;
    assert_eq!(first.headers()[header::LOCATION], "/");
    assert!(session_cookie(&first).is_some());

    let second = send(&app, get(&path)).await;
    assert_eq!(second.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        second.headers()[header::LOCATION],
        "/login?error=invalid_link"
    );
    assert!(session_cookie(&second).is_none());

    let redeem = post_json(
        "/api/v1/auth/magic-links/redeem",
        json!({ "token": token }).to_string(),
    );
    let refused = problem(send(&app, redeem).await, StatusCode::BAD_REQUEST).await;
    assert_eq!(refused.code, ErrorCode::InvalidLink);
}

#[tokio::test]
async fn a_page_can_redeem_a_link_with_post() {
    let t = TestState::new();
    let app = t.app();
    owner(&t);
    let token = link_token(&t, OWNER_EMAIL);
    let redeem = post_json(
        "/api/v1/auth/magic-links/redeem",
        json!({ "token": token }).to_string(),
    );
    let response = send(&app, redeem).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let cookie = session_cookie(&response).expect("a session cookie");
    assert_eq!(me(&app, &cookie).await, StatusCode::OK);
}

#[tokio::test]
async fn expired_unknown_and_malformed_links_are_refused() {
    let t = TestState::new();
    let app = t.app();
    owner(&t);
    let expired = link_token(&t, OWNER_EMAIL);
    control(&t)
        .execute("UPDATE magic_links SET expires_at = ?1", [now_ms() - 1])
        .unwrap();
    let unknown = SecretToken::generate();
    let too_long = format!("{}x", unknown.expose());
    let padded = format!("{}=", &unknown.expose()[1..]);
    for token in [
        expired.as_str(),
        unknown.expose(),
        "short",
        too_long.as_str(),
        padded.as_str(),
    ] {
        let response = send(&app, get(&format!("{LINK_PATH}{token}"))).await;
        assert_eq!(
            response.headers()[header::LOCATION],
            "/login?error=invalid_link",
            "{token}"
        );
        assert!(session_cookie(&response).is_none());
    }
    let redeem = post_json(
        "/api/v1/auth/magic-links/redeem",
        json!({ "token": expired }).to_string(),
    );
    let refused = problem(send(&app, redeem).await, StatusCode::BAD_REQUEST).await;
    assert_eq!(refused.code, ErrorCode::InvalidLink);
    assert_eq!(count(&control(&t), "SELECT COUNT(*) FROM sessions"), 0);
}

#[tokio::test]
async fn link_requests_do_not_reveal_whether_an_account_exists() {
    let t = with_mailbox();
    let app = t.app();
    owner(&t);
    let conn = control(&t);
    conn.execute(
        "INSERT INTO users (id, email, role, status, quota_bytes, created_at) \
         VALUES ('01MEMBER000000000000000000', 'gone@example.test', 'member', 'disabled', 0, 0)",
        [],
    )
    .unwrap();

    let mut answers = Vec::new();
    for email in [OWNER_EMAIL, "nobody@example.test", "gone@example.test"] {
        let response = send(&app, email_request(email)).await;
        let status = response.status();
        let mut headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .filter(|(name, _)| *name != REQUEST_ID_HEADER)
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
            .collect();
        headers.sort();
        answers.push((status, headers, body(response).await));
    }
    assert_eq!(answers[0].0, StatusCode::ACCEPTED);
    assert_eq!(answers[0], answers[1], "unknown address");
    assert_eq!(answers[0], answers[2], "disabled account");

    let messages = mailbox(&t).await;
    assert_eq!(messages.len(), 1, "only the owner gets an email");
    assert!(messages[0].contains("To: owner@example.test"));
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM magic_links"), 1);

    // A malformed address is refused for its shape, never for its account.
    let response = send(&app, email_request("not an email")).await;
    let refused = problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(refused.code, ErrorCode::ValidationFailed);
    assert_eq!(refused.errors[0].field, "email");
}

#[tokio::test]
async fn link_requests_are_rate_limited_per_address_and_per_client() {
    let t = with_mailbox();
    let app = t.app();
    owner(&t);

    // Per address: 3 an hour, whichever client asks, known address or not.
    for (i, ip) in ["192.0.2.1", "192.0.2.2", "192.0.2.3"].iter().enumerate() {
        let response = send(&app, with_ip(email_request("nobody@example.test"), ip)).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED, "request {i}");
    }
    let response = send(
        &app,
        with_ip(email_request("Nobody@Example.test"), "192.0.2.4"),
    )
    .await;
    let retry_after: u32 = response.headers()[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((3000..=3600).contains(&retry_after), "{retry_after}");
    let limited = problem(response, StatusCode::TOO_MANY_REQUESTS).await;
    assert_eq!(limited.code, ErrorCode::RateLimited);
    let other = send(&app, with_ip(email_request(OWNER_EMAIL), "192.0.2.4")).await;
    assert_eq!(other.status(), StatusCode::ACCEPTED, "other addresses pass");

    // Per client: 10 a minute across the sign-in routes.
    for i in 0..10 {
        let request = if i % 2 == 0 {
            email_request(&format!("user{i}@example.test"))
        } else {
            get(&format!("{LINK_PATH}{}", SecretToken::generate().expose()))
        };
        let response = send(&app, with_ip(request, "198.51.100.7")).await;
        assert_ne!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "request {i}"
        );
    }
    let response = send(
        &app,
        with_ip(email_request("late@example.test"), "198.51.100.7"),
    )
    .await;
    problem(response, StatusCode::TOO_MANY_REQUESTS).await;
    let link = get(&format!("{LINK_PATH}{}", SecretToken::generate().expose()));
    problem(
        send(&app, with_ip(link, "198.51.100.7")).await,
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    let response = send(
        &app,
        with_ip(email_request("late@example.test"), "198.51.100.8"),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "other clients pass"
    );

    // Only the owner's request sent anything.
    assert_eq!(mailbox(&t).await.len(), 1);
}

#[tokio::test]
async fn sessions_end_when_idle_or_past_their_lifetime() {
    let t = TestState::new();
    let app = t.app();
    let idle = sign_in(&app, &t).await;
    let old = sign_in(&app, &t).await;
    let active = sign_in(&app, &t).await;
    let conn = control(&t);
    let now = now_ms();
    let set = |column: &str, value: i64, cookie: &str| {
        conn.execute(
            &format!("UPDATE sessions SET {column} = ?1 WHERE id_hash = ?2"),
            params![value, hash_token(cookie).as_slice()],
        )
        .unwrap();
    };
    set("last_seen_at", now - 30 * DAY_MS - MINUTE_MS, &idle);
    set("expires_at", now - 1, &old);
    set("last_seen_at", now - 29 * DAY_MS, &active);

    assert_eq!(me(&app, &idle).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&app, &old).await, StatusCode::UNAUTHORIZED);
    // Used within 30 days: still valid, and the use slides the idle expiry.
    assert_eq!(me(&app, &active).await, StatusCode::OK);
    let seen: i64 = conn
        .query_row(
            "SELECT last_seen_at FROM sessions WHERE id_hash = ?1",
            [hash_token(&active).as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(seen >= now, "last use recorded");

    // The next sign-in prunes the expired sessions.
    sign_in(&app, &t).await;
    for gone in [&idle, &old] {
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id_hash = ?1",
                [hash_token(gone).as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
    }
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM sessions"), 2);
}

#[tokio::test]
async fn signing_in_rotates_the_session_the_browser_held() {
    let t = TestState::new();
    let app = t.app();
    let first = sign_in(&app, &t).await;
    assert_eq!(me(&app, &first).await, StatusCode::OK); // now cached

    let token = link_token(&t, OWNER_EMAIL);
    let response = send(
        &app,
        with_session(get(&format!("{LINK_PATH}{token}")), &first),
    )
    .await;
    let second = session_cookie(&response).expect("a new session");
    assert_ne!(first, second);
    assert_eq!(me(&app, &first).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&app, &second).await, StatusCode::OK);
    let conn = control(&t);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM sessions"), 1);
    assert_eq!(
        audit(&conn, "session.create").last().unwrap()["rotated"],
        true
    );
}

#[tokio::test]
async fn logout_ends_the_session_and_clears_the_cookie() {
    let t = TestState::new();
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let other = sign_in(&app, &t).await;
    assert_eq!(me(&app, &cookie).await, StatusCode::OK); // now cached

    let response = send(&app, spa(&t, post("/api/v1/auth/logout"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(session_cookie(&response).as_deref(), Some(""));
    let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(set_cookie.contains("Max-Age=0"), "{set_cookie}");

    assert_eq!(me(&app, &cookie).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        me(&app, &other).await,
        StatusCode::OK,
        "other sessions live on"
    );
    let conn = control(&t);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM sessions"), 1);
    assert_eq!(
        audit(&conn, "session.delete"),
        [json!({ "scope": "current", "count": 1 })]
    );

    // Signing out again, or without a session, is harmless.
    let response = send(&app, spa(&t, post("/api/v1/auth/logout"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = send(&app, post("/api/v1/auth/logout")).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(audit(&conn, "session.delete").len(), 1);
}

#[tokio::test]
async fn logout_all_ends_every_session_of_the_user() {
    let t = TestState::new();
    let app = t.app();
    let laptop = sign_in(&app, &t).await;
    let phone = sign_in(&app, &t).await;
    for cookie in [&laptop, &phone] {
        assert_eq!(me(&app, cookie).await, StatusCode::OK); // now cached
    }

    let response = send(&app, spa(&t, post("/api/v1/auth/logout-all"), &phone)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(session_cookie(&response).as_deref(), Some(""));
    for cookie in [&laptop, &phone] {
        assert_eq!(me(&app, cookie).await, StatusCode::UNAUTHORIZED);
    }
    let conn = control(&t);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM sessions"), 0);
    assert_eq!(
        audit(&conn, "session.delete"),
        [json!({ "scope": "all", "count": 2 })]
    );

    let response = send(&app, spa(&t, post("/api/v1/auth/logout-all"), &phone)).await;
    let refused = problem(response, StatusCode::UNAUTHORIZED).await;
    assert_eq!(refused.code, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn cookie_requests_that_change_state_must_come_from_the_app() {
    let t = TestState::new();
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let public = t.state.config().public_url.as_str().to_owned();

    let forged: [(&str, Vec<(&str, &str)>); 6] = [
        ("no client header", vec![("origin", public.as_str())]),
        (
            "foreign origin",
            vec![
                ("origin", "https://evil.example.test"),
                ("x-shelfy-client", "web"),
            ],
        ),
        ("no origin", vec![("x-shelfy-client", "web")]),
        (
            "null origin",
            vec![("origin", "null"), ("x-shelfy-client", "web")],
        ),
        (
            "cross-site",
            vec![
                ("origin", public.as_str()),
                ("x-shelfy-client", "web"),
                ("sec-fetch-site", "cross-site"),
            ],
        ),
        (
            "same-site",
            vec![
                ("origin", public.as_str()),
                ("x-shelfy-client", "web"),
                ("sec-fetch-site", "same-site"),
            ],
        ),
    ];
    for uri in ["/api/v1/auth/logout", "/api/v1/auth/logout-all"] {
        for (case, headers) in &forged {
            let mut request = with_session(post(uri), &cookie);
            for (name, value) in headers {
                request.headers_mut().insert(
                    HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    HeaderValue::from_str(value).unwrap(),
                );
            }
            let refused = problem(send(&app, request).await, StatusCode::FORBIDDEN).await;
            assert_eq!(refused.code, ErrorCode::CsrfFailed, "{uri}: {case}");
            assert!(refused.detail.is_some());
            assert_eq!(me(&app, &cookie).await, StatusCode::OK, "{uri}: {case}");
        }
    }

    // Safe methods are not checked.
    let mut cross_site_read = with_session(get("/api/v1/me"), &cookie);
    cross_site_read
        .headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    assert_eq!(send(&app, cross_site_read).await.status(), StatusCode::OK);
    // Cookieless requests are not checked (their bodies are JSON).
    let mut cookieless = email_request(OWNER_EMAIL);
    cookieless
        .headers_mut()
        .insert("origin", "https://evil.example.test".parse().unwrap());
    assert_eq!(send(&app, cookieless).await.status(), StatusCode::ACCEPTED);

    // The app's own request passes.
    let response = send(&app, spa(&t, post("/api/v1/auth/logout"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

/// Inserts an API token for `user_id` with `scopes`; returns its value.
fn api_token(t: &TestState, user_id: &str, scopes: &str) -> String {
    let token = format!("shx_{}", SecretToken::generate().expose());
    control(t)
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
             VALUES (?1, ?2, 'extension', ?3, ?4, ?5)",
            params![
                new_ulid(),
                user_id,
                hash_token(&token).as_slice(),
                scopes,
                now_ms()
            ],
        )
        .unwrap();
    token
}

async fn token_route(user: TokenUser<scopes::Lookup>) -> String {
    user.id().to_owned()
}

async fn cookie_route(user: CurrentUser) -> String {
    user.id().to_owned()
}

async fn sensitive_route(RecentAuth(user): RecentAuth) -> String {
    user.id().to_owned()
}

fn app_with_test_routes(t: &TestState) -> Router {
    let routes = OpenApiRouter::new()
        .route("/test/token", post_route(token_route))
        .route("/test/cookie", post_route(cookie_route))
        .route("/test/sensitive", get_route(sensitive_route));
    app::build(
        t.state.clone(),
        routes::router().merge(RouteLimits::STANDARD.apply(routes)),
    )
}

fn bearer(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    request
}

#[tokio::test]
async fn token_requests_skip_the_cookie_check_but_never_use_cookies() {
    let t = TestState::new();
    let app = app_with_test_routes(&t);
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let token = api_token(&t, &owner_id, "ingest lookup");

    // A cross-site request with a token and the cookie: the CSRF check is
    // skipped, the token authenticates.
    let mut request = bearer(with_session(post("/test/token"), &cookie), &token);
    request
        .headers_mut()
        .insert("origin", "https://evil.example.test".parse().unwrap());
    request
        .headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, owner_id);

    // A token never reaches cookie routes, even with a valid cookie beside it.
    for uri in ["/test/cookie", "/api/v1/auth/logout-all"] {
        let mut request = bearer(with_session(post(uri), &cookie), &token);
        request
            .headers_mut()
            .insert("origin", "https://evil.example.test".parse().unwrap());
        let refused = problem(send(&app, request).await, StatusCode::UNAUTHORIZED).await;
        assert_eq!(refused.code, ErrorCode::Unauthorized, "{uri}");
    }
    let response = send(
        &app,
        bearer(with_session(get("/api/v1/me"), &cookie), &token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(me(&app, &cookie).await, StatusCode::OK, "still signed in");

    // Token checks: scheme, prefix, scope, revocation.
    let missing_scope = api_token(&t, &owner_id, "ingest");
    let response = send(&app, bearer(post("/test/token"), &missing_scope)).await;
    assert_eq!(
        problem(response, StatusCode::FORBIDDEN).await.code,
        ErrorCode::Forbidden
    );
    let unprefixed = token.trim_start_matches("shx_").to_owned();
    for bad in [unprefixed.as_str(), "shx_short", "shx_"] {
        let response = send(&app, bearer(post("/test/token"), bad)).await;
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        problem(response, StatusCode::UNAUTHORIZED).await;
    }
    let response = send(&app, post("/test/token")).await;
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    control(&t)
        .execute("UPDATE api_tokens SET revoked_at = 1", [])
        .unwrap();
    let response = send(&app, bearer(post("/test/token"), &token)).await;
    problem(response, StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
async fn sensitive_actions_need_a_sign_in_from_the_last_five_minutes() {
    let t = TestState::new();
    let app = app_with_test_routes(&t);
    let cookie = sign_in(&app, &t).await;
    let sensitive = || with_session(get("/test/sensitive"), &cookie);
    assert_eq!(send(&app, sensitive()).await.status(), StatusCode::OK);

    control(&t)
        .execute(
            "UPDATE sessions SET reauth_at = ?1",
            [now_ms() - 6 * MINUTE_MS],
        )
        .unwrap();
    t.state.auth().forget_all_sessions();
    let refused = problem(send(&app, sensitive()).await, StatusCode::FORBIDDEN).await;
    assert_eq!(refused.code, ErrorCode::ReauthRequired);
    assert_eq!(me(&app, &cookie).await, StatusCode::OK, "still signed in");

    let response = send(&app, get("/test/sensitive")).await;
    assert_eq!(
        problem(response, StatusCode::UNAUTHORIZED).await.code,
        ErrorCode::Unauthorized
    );
}

/// Operations that work without a session. The document must declare
/// exactly these as public (`security(())`); every other operation inherits
/// the session requirement.
const PUBLIC: &[(&str, &str)] = &[
    ("get", "/health"),
    ("get", "/api/v1/openapi.json"),
    ("get", "/api/v1/auth/methods"),
    ("post", "/api/v1/auth/magic-links"),
    ("get", "/api/v1/auth/magic/{token}"),
    ("post", "/api/v1/auth/magic-links/redeem"),
    ("post", "/api/v1/auth/logout"),
];

/// One operation of the OpenAPI document.
struct Operation {
    method: String,
    path: String,
    /// The security requirements that apply: the operation's own, or the
    /// document's default.
    security: Vec<Value>,
}

impl Operation {
    /// Whether the operation may be called anonymously (`{}` requirement).
    fn is_public(&self) -> bool {
        self.security.iter().any(|requirement| {
            requirement
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        })
    }

    fn accepts(&self, scheme: &str) -> bool {
        self.security
            .iter()
            .any(|requirement| requirement.get(scheme).is_some())
    }
}

/// Every operation of the OpenAPI document.
fn operations() -> Vec<Operation> {
    let doc = serde_json::to_value(routes::openapi()).unwrap();
    let default = doc["security"].as_array().cloned().unwrap_or_default();
    let mut operations = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            let security = operation["security"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| default.clone());
            operations.push(Operation {
                method: method.clone(),
                path: path.clone(),
                security,
            });
        }
    }
    operations
}

/// A concrete request for `path`, its parameters filled with placeholders.
fn request_for(method: &str, path: &str) -> Request<Body> {
    let uri: String = path
        .split('/')
        .map(|segment| {
            if segment.starts_with('{') {
                "placeholder"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    let method: Method = method.to_ascii_uppercase().parse().unwrap();
    let with_body = matches!(method, Method::POST | Method::PUT | Method::PATCH);
    let mut builder = Request::builder().method(method).uri(uri);
    if with_body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(if with_body {
            Body::from("{}")
        } else {
            Body::empty()
        })
        .unwrap()
}

#[tokio::test]
async fn every_protected_route_answers_401_without_a_session() {
    let t = TestState::new();
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let token = api_token(
        &t,
        &owner_id,
        "ingest tasks uploads lookup links:create migrate",
    );
    let unknown = SecretToken::generate();

    let operations = operations();
    let public: BTreeSet<(String, String)> = operations
        .iter()
        .filter(|op| op.is_public())
        .map(|op| (op.method.clone(), op.path.clone()))
        .collect();
    let expected: BTreeSet<(String, String)> = PUBLIC
        .iter()
        .map(|(method, path)| ((*method).to_owned(), (*path).to_owned()))
        .collect();
    assert_eq!(
        public, expected,
        "the document's public operations (security(())) differ from PUBLIC"
    );

    let mut protected = 0;
    for op in operations.iter().filter(|op| !op.is_public()) {
        protected += 1;
        let (method, path) = (op.method.as_str(), op.path.as_str());
        let route = format!("{} {path}", method.to_ascii_uppercase());
        assert!(
            op.accepts("session"),
            "{route} is neither public nor behind the session"
        );

        let mut attempts = vec![
            ("no credentials", request_for(method, path)),
            (
                "a malformed cookie",
                spa(&t, request_for(method, path), "garbage"),
            ),
            (
                "an unknown session",
                spa(&t, request_for(method, path), unknown.expose()),
            ),
        ];
        if !op.accepts("bearer") {
            attempts.push((
                "an API token beside a valid cookie",
                bearer(spa(&t, request_for(method, path), &cookie), &token),
            ));
        }
        for (what, request) in attempts {
            let response = send(&app, request).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{route} with {what}: take CurrentUser as the first extractor, or declare the route public"
            );
            let refused = problem(response, StatusCode::UNAUTHORIZED).await;
            assert_eq!(refused.code, ErrorCode::Unauthorized, "{route}");
        }
    }
    assert!(
        protected >= 8,
        "the read API and the account routes are protected"
    );
    assert_eq!(me(&app, &cookie).await, StatusCode::OK, "still signed in");
}

#[tokio::test]
async fn a_signed_in_session_reaches_the_read_api() {
    let t = TestState::new();
    let app = t.app();
    let cookie = sign_in(&app, &t).await;

    let response = send(&app, with_session(get("/api/v1/stats"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let stats = json(response).await;
    assert!(stats.is_object(), "{stats}");
    for uri in [
        "/api/v1/posts",
        "/api/v1/collections",
        "/api/v1/search?q=anything",
    ] {
        let response = send(&app, with_session(get(uri), &cookie)).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
    let missing = send(&app, with_session(get("/api/v1/posts/ig_1"), &cookie)).await;
    assert_eq!(
        problem(missing, StatusCode::NOT_FOUND).await.code,
        ErrorCode::NotFound
    );

    // No session, an expired one, or a signed-out one: 401.
    let response = send(&app, get("/api/v1/stats")).await;
    assert_eq!(
        problem(response, StatusCode::UNAUTHORIZED).await.code,
        ErrorCode::Unauthorized
    );
    let expired = sign_in(&app, &t).await;
    control(&t)
        .execute(
            "UPDATE sessions SET expires_at = ?1 WHERE id_hash = ?2",
            params![now_ms() - 1, hash_token(&expired).as_slice()],
        )
        .unwrap();
    let response = send(&app, with_session(get("/api/v1/stats"), &expired)).await;
    problem(response, StatusCode::UNAUTHORIZED).await;
    send(&app, spa(&t, post("/api/v1/auth/logout"), &cookie)).await;
    let response = send(&app, with_session(get("/api/v1/stats"), &cookie)).await;
    problem(response, StatusCode::UNAUTHORIZED).await;
}

/// A minimal SMTP relay: accepts one message and returns its DATA.
async fn fake_relay(listener: TcpListener) -> String {
    let (stream, _) = listener.accept().await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    write.write_all(b"220 relay.test ESMTP\r\n").await.unwrap();
    let mut data = String::new();
    let mut in_data = false;
    while let Some(line) = lines.next_line().await.unwrap() {
        if in_data {
            if line == "." {
                in_data = false;
                write.write_all(b"250 2.0.0 queued\r\n").await.unwrap();
            } else {
                data.push_str(&line);
                data.push('\n');
            }
            continue;
        }
        let command = line.to_ascii_uppercase();
        let reply: &[u8] = if command.starts_with("EHLO") || command.starts_with("HELO") {
            b"250-relay.test\r\n250 8BITMIME\r\n"
        } else if command == "DATA" {
            in_data = true;
            b"354 go ahead\r\n"
        } else if command == "QUIT" {
            write.write_all(b"221 bye\r\n").await.unwrap();
            break;
        } else {
            b"250 2.1.0 ok\r\n"
        };
        write.write_all(reply).await.unwrap();
    }
    data
}

#[tokio::test]
async fn smtp_delivers_the_link_through_a_relay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let relay = tokio::spawn(fake_relay(listener));
    let t = TestState::with_config(|config| {
        config.mail = MailConfig::Smtp(SmtpConfig {
            host: "127.0.0.1".into(),
            port,
            tls: SmtpTls::None,
            credentials: None,
            from: "Shelfy <login@example.test>".parse().unwrap(),
        });
    });
    let app = t.app();
    owner(&t);
    assert_eq!(
        json(send(&app, get("/api/v1/auth/methods")).await).await["emailLink"],
        true
    );
    let response = send(&app, email_request(OWNER_EMAIL)).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    t.state.auth().mail_idle().await;

    let data = tokio::time::timeout(Duration::from_secs(10), relay)
        .await
        .expect("the relay got a message")
        .unwrap();
    let message = data.replace("=\n", "");
    assert!(
        message.contains("From: Shelfy <login@example.test>"),
        "{message}"
    );
    assert!(message.contains("To: owner@example.test"), "{message}");
    let link = link_in(&message);
    let path = link
        .strip_prefix(t.state.config().public_url.as_str())
        .unwrap();
    let response = send(&app, get(path)).await;
    assert_eq!(response.headers()[header::LOCATION], "/");
    let cookie = session_cookie(&response).unwrap();
    assert_eq!(me(&app, &cookie).await, StatusCode::OK);
}
