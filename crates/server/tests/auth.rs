//! Owner sign-in through the real middleware stack (plan §2.11, §7.1, E4):
//! email links end to end (dev mailbox and SMTP), `admin login-link`,
//! redemption by `POST` only, sessions (cookie flags, expiry, rotation,
//! sign-out, the miss cache), the CSRF guard on every unsafe request, rate
//! limits keyed by the TCP peer or a trusted proxy's header, API tokens,
//! re-authentication, no account enumeration, and deny-by-default access on
//! every route. The logs are checked in `auth_logs.rs`.

mod support;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode, header};
use axum::routing::{get as get_route, post as post_route};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use shelfy_server::auth::RecentAuth;
use shelfy_server::auth::access::{Access, AccessPolicy};
use shelfy_server::auth::bearer::{Scope, TokenUser, scopes};
use shelfy_server::auth::rate_limit::RateLimit;
use shelfy_server::config::Config;
use shelfy_server::current_user::CurrentUser;
use shelfy_server::error::ErrorCode;
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::limits::RouteLimits;
use shelfy_server::mail::{MailConfig, SmtpConfig, SmtpTls};
use shelfy_server::net::TrustedProxies;
use shelfy_server::serve::Server;
use shelfy_server::telemetry::http::REQUEST_ID_HEADER;
use shelfy_server::telemetry::metrics;
use shelfy_server::tokens::{SecretToken, hash_token};
use shelfy_server::{app, routes};
use support::auth::{
    OWNER_EMAIL, REDEEM, from_spa, link_in, link_token, mailbox, mailbox_dir, owner, post,
    redeem_request, session_cookie, sign_in, spa, token_of, with_session,
};
use support::{TestState, body, get, json, post_json, problem, send};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use utoipa_axum::router::OpenApiRouter;

const DAY_MS: i64 = 86_400_000;
const MINUTE_MS: i64 = 60_000;

/// A state with the dev mailbox on.
fn with_mailbox() -> TestState {
    TestState::with_config(|config: &mut Config| {
        config.mail = MailConfig::dev_mailbox(&config.data_dir);
    })
}

/// Lifts the sign-in limit per client, for the tests that sweep every route
/// from one in-process client: every `/api/v1/auth/*` request counts.
fn without_sign_in_limit(config: &mut Config) {
    config.auth.ip_limit = RateLimit {
        max: 100_000,
        window: Duration::from_secs(60),
    };
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

/// `request` as if it came over TCP from `peer`.
fn from_peer(mut request: Request<Body>, peer: &str) -> Request<Body> {
    let ip = peer.parse().unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip, 40_000)));
    request
}

/// `request` with `CF-Connecting-IP: ip`.
fn claiming(mut request: Request<Body>, ip: &str) -> Request<Body> {
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
        json!({ "emailLink": true, "passkeys": true })
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
    // The token travels in the fragment of the SPA's sign-in page.
    let url = link_in(message);
    let public = t.state.config().public_url.as_str();
    let token = token_of(&url);
    assert_eq!(url, format!("{public}/login/magic#{token}"));

    let response = send(&app, redeem_request(&t, &token)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
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
async fn the_email_goes_to_the_stored_address() {
    let t = with_mailbox();
    let app = t.app();
    // An address stored with capitals (older data, or another normalization):
    // the lookup ignores case, the email uses the stored spelling.
    control(&t)
        .execute(
            "INSERT INTO users (id, email, role, quota_bytes, created_at) \
             VALUES ('01MIXED0000000000000000000', 'Mixed.Case@Example.test', 'owner', 0, 0)",
            [],
        )
        .unwrap();
    let response = send(&app, email_request("mixed.case@example.test")).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let messages = mailbox(&t).await;
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].contains("To: Mixed.Case@Example.test"),
        "{}",
        messages[0]
    );
}

#[tokio::test]
async fn without_email_the_cli_link_is_the_way_in() {
    let t = TestState::new();
    let app = t.app();
    owner(&t);

    assert_eq!(
        json(send(&app, get("/api/v1/auth/methods")).await).await,
        json!({ "emailLink": false, "passkeys": true })
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
    let mut request = redeem_request(&t, &token);
    request
        .headers_mut()
        .insert(header::USER_AGENT, "TestBrowser/1.0".parse().unwrap());
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

    // A non-ASCII cookie in the same header does not hide the session.
    let mut request = get("/api/v1/me");
    request.headers_mut().insert(
        header::COOKIE,
        HeaderValue::from_bytes(
            format!("note=caf\u{e9}; __Host-shelfy_session={value}").as_bytes(),
        )
        .unwrap(),
    );
    assert!(request.headers()[header::COOKIE].to_str().is_err());
    assert_eq!(send(&app, request).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_link_works_once_and_nothing_redeems_it_on_get() {
    let t = TestState::new();
    let app = t.app();
    owner(&t);
    let token = link_token(&t, OWNER_EMAIL);

    // Scanners and prefetchers fetch links: no GET or HEAD uses one up.
    for path in [
        format!("/api/v1/auth/magic/{token}"),
        "/login/magic".to_owned(),
    ] {
        for method in [Method::GET, Method::HEAD] {
            let request = Request::builder()
                .method(method.clone())
                .uri(&path)
                .body(Body::empty())
                .unwrap();
            let response = send(&app, request).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
            assert!(session_cookie(&response).is_none());
        }
    }
    assert_eq!(
        count(
            &control(&t),
            "SELECT COUNT(*) FROM magic_links WHERE used_at IS NULL"
        ),
        1,
        "still unused"
    );

    let first = send(&app, redeem_request(&t, &token)).await;
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    assert!(session_cookie(&first).is_some());

    let second = send(&app, redeem_request(&t, &token)).await;
    assert!(session_cookie(&second).is_none());
    let refused = problem(second, StatusCode::BAD_REQUEST).await;
    assert_eq!(refused.code, ErrorCode::InvalidLink);
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
        let response = send(&app, redeem_request(&t, token)).await;
        assert!(session_cookie(&response).is_none());
        let refused = problem(response, StatusCode::BAD_REQUEST).await;
        assert_eq!(refused.code, ErrorCode::InvalidLink, "{token}");
    }
    assert_eq!(count(&control(&t), "SELECT COUNT(*) FROM sessions"), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_links_never_wait_for_the_writer() {
    let t = TestState::new();
    let app = t.app();
    owner(&t);

    // Another connection holds the control database's write lock.
    let path = t.data_dir().control_db();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let mut conn = Connection::open(path).unwrap();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        tx.rollback().unwrap();
    });
    held_rx.recv().unwrap();

    // An unknown token is refused on a reader, at once.
    let started = Instant::now();
    let unknown = SecretToken::generate();
    let response = send(&app, redeem_request(&t, unknown.expose())).await;
    let elapsed = started.elapsed();
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    let refused = problem(response, StatusCode::BAD_REQUEST).await;
    assert_eq!(refused.code, ErrorCode::InvalidLink);
    assert!(
        elapsed < Duration::from_secs(2),
        "took {elapsed:?}: the writer's busy timeout is 5 s"
    );

    // A real link still signs in once the writer is free.
    let cookie = sign_in(&app, &t).await;
    assert_eq!(me(&app, &cookie).await, StatusCode::OK);
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
async fn link_requests_are_limited_per_address_whoever_asks() {
    let t = with_mailbox();
    let app = t.app();
    owner(&t);

    for (i, peer) in ["192.0.2.1", "192.0.2.2", "192.0.2.3"].iter().enumerate() {
        let response = send(&app, from_peer(email_request("nobody@example.test"), peer)).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED, "request {i}");
    }
    let response = send(
        &app,
        from_peer(email_request("Nobody@Example.test"), "192.0.2.4"),
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
    let other = send(&app, from_peer(email_request(OWNER_EMAIL), "192.0.2.4")).await;
    assert_eq!(other.status(), StatusCode::ACCEPTED, "other addresses pass");
    assert_eq!(
        mailbox(&t).await.len(),
        1,
        "only the owner's request sent one"
    );
}

/// Numbers the addresses of [`passed_before_429`], so its link requests never
/// meet the per-address limit.
static ADDRESSES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Sends sign-in requests (link requests and redemptions, alternately) until
/// one is refused; returns how many passed, at most `max`.
async fn passed_before_429(
    app: &Router,
    t: &TestState,
    max: u32,
    edit: impl Fn(Request<Body>) -> Request<Body>,
) -> u32 {
    for i in 0..max {
        let request = if i % 2 == 0 {
            let n = ADDRESSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            email_request(&format!("user{n}@example.test"))
        } else {
            redeem_request(t, SecretToken::generate().expose())
        };
        let response = send(app, edit(request)).await;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            assert!(response.headers().contains_key(header::RETRY_AFTER));
            return i;
        }
    }
    max
}

#[tokio::test]
async fn without_a_trusted_proxy_the_tcp_peer_is_the_client() {
    let t = TestState::new();
    let app = t.app();

    // A client that varies CF-Connecting-IP still counts as its peer.
    let n = std::sync::atomic::AtomicU32::new(0);
    let passed = passed_before_429(&app, &t, 12, |request| {
        let i = n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        from_peer(claiming(request, &format!("203.0.113.{i}")), "198.51.100.7")
    })
    .await;
    assert_eq!(passed, 10, "10 a minute per client, the header ignored");

    // Another peer has its own budget.
    let other = passed_before_429(&app, &t, 3, |request| from_peer(request, "198.51.100.8")).await;
    assert_eq!(other, 3);

    // An IPv6 client counts by its /64: changing the host part does not help.
    let n = std::sync::atomic::AtomicU32::new(0);
    let passed = passed_before_429(&app, &t, 12, |request| {
        let i = n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        from_peer(request, &format!("2001:db8:1:2::{:x}", i + 1))
    })
    .await;
    assert_eq!(passed, 10, "one /64, one budget");
    let next_block =
        passed_before_429(&app, &t, 3, |request| from_peer(request, "2001:db8:1:3::1")).await;
    assert_eq!(next_block, 3);
}

#[tokio::test]
async fn a_trusted_proxy_names_the_client() {
    let t = TestState::with_config(|config| {
        config.trusted_proxies = TrustedProxies::parse("172.18.0.0/16").unwrap();
    });
    let app = t.app();

    // Through the trusted proxy, each CF-Connecting-IP has its own budget.
    let n = std::sync::atomic::AtomicU32::new(0);
    let passed = passed_before_429(&app, &t, 12, |request| {
        let i = n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        from_peer(claiming(request, &format!("203.0.113.{i}")), "172.18.0.2")
    })
    .await;
    assert_eq!(passed, 12, "every request named another client");

    // One client behind the proxy is limited...
    let passed = passed_before_429(&app, &t, 12, |request| {
        from_peer(claiming(request, "203.0.113.200"), "172.18.0.2")
    })
    .await;
    assert_eq!(passed, 10);
    // ...and a peer outside the trusted block cannot borrow another's name.
    let passed = passed_before_429(&app, &t, 12, |request| {
        from_peer(claiming(request, "203.0.113.201"), "198.51.100.9")
    })
    .await;
    assert_eq!(passed, 10, "keyed by the untrusted peer");
    let peer_spent = passed_before_429(&app, &t, 3, |request| {
        from_peer(claiming(request, "203.0.113.202"), "198.51.100.9")
    })
    .await;
    assert_eq!(peer_spent, 0, "the same peer, whatever it claims");
}

/// A raw HTTP/1.1 `POST` of `body` with the web app's headers; the status.
async fn raw_post(addr: SocketAddr, path: &str, body: &str, extra: &str) -> u16 {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\
         Origin: http://localhost:8080\r\nX-Shelfy-Client: web\r\n{extra}\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);
    text.split(' ').nth(1).unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_server_hands_the_tcp_peer_to_the_limits() {
    let TestState { dir: _dir, state } = TestState::new();
    let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let metrics_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = Server::new(
        state.clone(),
        app::app(state),
        api,
        metrics_listener,
        metrics::install(),
    );
    let addr = server.api_addr().unwrap();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stop_rx.await;
    }));

    // Every request comes from 127.0.0.1, whatever it claims.
    let mut statuses = Vec::new();
    for i in 0..11 {
        let body = json!({ "email": format!("user{i}@example.test") }).to_string();
        let claim = format!("CF-Connecting-IP: 203.0.113.{i}\r\n");
        statuses.push(raw_post(addr, "/api/v1/auth/magic-links", &body, &claim).await);
    }
    assert_eq!(statuses[..10], [202; 10]);
    assert_eq!(statuses[10], 429, "keyed by the TCP peer");

    stop_tx.send(()).unwrap();
    running.await.unwrap().unwrap();
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
async fn cookies_that_sign_nobody_in_are_remembered_briefly() {
    let t = TestState::with_config(|config| {
        config.auth.session_miss_ttl = Duration::from_millis(300);
    });
    let app = t.app();
    let owner_id = owner(&t);
    let forged = SecretToken::generate();
    assert_eq!(me(&app, forged.expose()).await, StatusCode::UNAUTHORIZED);

    // A row with that hash appears behind the server's back: the remembered
    // miss still answers, without a database read...
    control(&t)
        .execute(
            "INSERT INTO sessions (id_hash, user_id, created_at, expires_at, last_seen_at) \
             VALUES (?1, ?2, ?3, ?4, ?3)",
            params![
                hash_token(forged.expose()).as_slice(),
                owner_id,
                now_ms(),
                now_ms() + DAY_MS
            ],
        )
        .unwrap();
    assert_eq!(me(&app, forged.expose()).await, StatusCode::UNAUTHORIZED);
    // ...until the miss expires.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(me(&app, forged.expose()).await, StatusCode::OK);
}

#[tokio::test]
async fn signing_in_rotates_the_session_the_browser_held() {
    let t = TestState::new();
    let app = t.app();
    let first = sign_in(&app, &t).await;
    assert_eq!(me(&app, &first).await, StatusCode::OK); // now cached

    let token = link_token(&t, OWNER_EMAIL);
    let response = send(&app, with_session(redeem_request(&t, &token), &first)).await;
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

    // Signing out again clears the stale cookie; without one, nothing is set.
    let response = send(&app, spa(&t, post("/api/v1/auth/logout"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(session_cookie(&response).as_deref(), Some(""));
    let response = send(&app, from_spa(&t, post("/api/v1/auth/logout"))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        response.headers().get(header::SET_COOKIE).is_none(),
        "no cookie to clear"
    );
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

/// `request` with `headers` added.
fn with_headers(mut request: Request<Body>, headers: &[(&str, &str)]) -> Request<Body> {
    for (name, value) in headers {
        request.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    request
}

#[tokio::test]
async fn every_state_changing_request_must_come_from_the_app() {
    let t = TestState::with_config(|config: &mut Config| {
        config.mail = MailConfig::dev_mailbox(&config.data_dir);
        without_sign_in_limit(config);
    });
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let public = t.state.config().public_url.as_str().to_owned();

    let forged: [(&str, Vec<(&str, &str)>); 7] = [
        ("a cross-site form", vec![]),
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
    let token = link_token(&t, OWNER_EMAIL);
    let bodies = [
        ("/api/v1/auth/logout", String::new()),
        ("/api/v1/auth/logout-all", String::new()),
        (
            "/api/v1/auth/magic-links",
            json!({ "email": OWNER_EMAIL }).to_string(),
        ),
        (REDEEM, json!({ "token": token }).to_string()),
    ];
    for (uri, json_body) in &bodies {
        for (case, headers) in &forged {
            let make = || {
                let request = Request::post(*uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json_body.clone()))
                    .unwrap();
                with_headers(request, headers)
            };
            // With the session cookie, and without: logout CSRF, login CSRF.
            for request in [with_session(make(), &cookie), make()] {
                let refused = problem(send(&app, request).await, StatusCode::FORBIDDEN).await;
                assert_eq!(refused.code, ErrorCode::CsrfFailed, "{uri}: {case}");
                assert!(refused.detail.is_some());
            }
        }
    }
    // Nothing happened: still signed in, the link unused, no email sent.
    assert_eq!(me(&app, &cookie).await, StatusCode::OK);
    assert_eq!(
        count(
            &control(&t),
            "SELECT COUNT(*) FROM magic_links WHERE used_at IS NULL"
        ),
        1
    );
    assert!(mailbox(&t).await.is_empty());

    // Safe methods are not checked.
    let cross_site_read = with_headers(
        with_session(get("/api/v1/me"), &cookie),
        &[("sec-fetch-site", "cross-site")],
    );
    assert_eq!(send(&app, cross_site_read).await.status(), StatusCode::OK);

    // The app's own requests pass.
    let response = send(&app, redeem_request(&t, &token)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
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
    format!("{}:{}", user.id(), user.token_id().len())
}

async fn user_route(user: CurrentUser) -> String {
    user.id().to_owned()
}

async fn sensitive_route(RecentAuth(user): RecentAuth) -> String {
    user.id().to_owned()
}

async fn open_route() -> &'static str {
    "anyone"
}

/// The real routes plus test routes:
/// - `POST /test/token`: tokens with `lookup` only;
/// - `POST /test/either`: a `lookup` token or a session;
/// - `POST /test/cookie`, `GET /test/sensitive`: a session (the default);
/// - `GET /test/public`: anyone, declared public;
/// - `GET /test/unlisted`: no extractor and no rule, so a session.
fn app_with_test_routes(t: &TestState) -> Router {
    let routes = OpenApiRouter::new()
        .route("/test/token", post_route(token_route))
        .route("/test/either", post_route(user_route))
        .route("/test/cookie", post_route(user_route))
        .route("/test/sensitive", get_route(sensitive_route))
        .route("/test/public", get_route(open_route))
        .route("/test/unlisted", get_route(open_route));
    let access = routes::access()
        .token(Method::POST, "/test/token", Scope::Lookup, false)
        .token(Method::POST, "/test/either", Scope::Lookup, true)
        .public(Method::GET, "/test/public");
    app::build_with_access(
        t.state.clone(),
        routes::router().merge(RouteLimits::STANDARD.apply(routes)),
        access,
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
async fn routes_need_a_session_unless_declared_otherwise() {
    let t = TestState::new();
    let app = app_with_test_routes(&t);
    let cookie = sign_in(&app, &t).await;

    // A route without an extractor or a rule is still closed.
    problem(
        send(&app, get("/test/unlisted")).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let response = send(&app, with_session(get("/test/unlisted"), &cookie)).await;
    assert_eq!(body(response).await, "anyone");
    // A declared public route is open, HEAD included.
    assert_eq!(body(send(&app, get("/test/public")).await).await, "anyone");
    let head = Request::head("/test/public").body(Body::empty()).unwrap();
    assert_eq!(send(&app, head).await.status(), StatusCode::OK);

    // Public covers the declared method only: an anonymous DELETE /health
    // is a session request (401); signed in, it is the router's 405.
    let response = send(
        &app,
        from_spa(&t, Request::delete("/health").body(Body::empty()).unwrap()),
    )
    .await;
    problem(response, StatusCode::UNAUTHORIZED).await;
    let response = send(
        &app,
        spa(
            &t,
            Request::delete("/health").body(Body::empty()).unwrap(),
            &cookie,
        ),
    )
    .await;
    assert!(
        response.headers()[header::ALLOW]
            .to_str()
            .unwrap()
            .contains("GET")
    );
    problem(response, StatusCode::METHOD_NOT_ALLOWED).await;
    // Unknown paths are 404 for everyone.
    problem(send(&app, get("/test/nope")).await, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn tokens_reach_token_routes_only_and_skip_the_cookie_check() {
    let t = TestState::new();
    let app = app_with_test_routes(&t);
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let token = api_token(&t, &owner_id, "ingest lookup");

    // A cross-site request with a token and the cookie: the CSRF check is
    // skipped, the token authenticates.
    let request = with_headers(
        bearer(with_session(post("/test/token"), &cookie), &token),
        &[
            ("origin", "https://evil.example.test"),
            ("sec-fetch-site", "cross-site"),
        ],
    );
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, format!("{owner_id}:26"));

    // A token never reaches a session route, even beside a valid cookie.
    for request in [
        bearer(with_session(post("/test/cookie"), &cookie), &token),
        bearer(
            with_session(post("/api/v1/auth/logout-all"), &cookie),
            &token,
        ),
        bearer(with_session(get("/api/v1/me"), &cookie), &token),
        bearer(with_session(get("/test/sensitive"), &cookie), &token),
        bearer(
            with_session(get(&format!("/media/{}.jpg", "a".repeat(64))), &cookie),
            &token,
        ),
    ] {
        let uri = request.uri().to_string();
        let refused = problem(send(&app, request).await, StatusCode::UNAUTHORIZED).await;
        assert_eq!(refused.code, ErrorCode::Unauthorized, "{uri}");
    }
    assert_eq!(me(&app, &cookie).await, StatusCode::OK, "still signed in");

    // A token-only route refuses the session; a token-or-session one takes both.
    let response = send(&app, spa(&t, post("/test/token"), &cookie)).await;
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    problem(response, StatusCode::UNAUTHORIZED).await;
    let response = send(&app, spa(&t, post("/test/either"), &cookie)).await;
    assert_eq!(body(response).await, owner_id);
    let response = send(&app, bearer(post("/test/either"), &token)).await;
    assert_eq!(body(response).await, owner_id);
    problem(
        send(&app, from_spa(&t, post("/test/either"))).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;

    // Token checks: scheme, prefix, scope, revocation.
    let missing_scope = api_token(&t, &owner_id, "ingest");
    let response = send(&app, bearer(post("/test/token"), &missing_scope)).await;
    let refused = problem(response, StatusCode::FORBIDDEN).await;
    assert_eq!(refused.code, ErrorCode::Forbidden);
    assert!(refused.detail.unwrap().contains("lookup"));
    let unprefixed = token.trim_start_matches("shx_").to_owned();
    for bad in [unprefixed.as_str(), "shx_short", "shx_"] {
        let response = send(&app, bearer(post("/test/token"), bad)).await;
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        problem(response, StatusCode::UNAUTHORIZED).await;
    }
    let response = send(&app, from_spa(&t, post("/test/token"))).await;
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

    /// The scopes of the `bearer` requirements, sorted, if the operation
    /// takes tokens. Each requirement names one scope: the scopes of one
    /// requirement must all be held, and a route takes a token with any one
    /// of its scopes, so each is a requirement of its own.
    fn bearer_scopes(&self) -> Option<Vec<String>> {
        let mut scopes: Vec<String> = self
            .security
            .iter()
            .filter_map(|requirement| requirement.get("bearer"))
            .map(|scopes| {
                let scopes = scopes.as_array().unwrap();
                assert_eq!(scopes.len(), 1, "one scope per bearer requirement");
                scopes[0].as_str().unwrap().to_owned()
            })
            .collect();
        scopes.sort();
        (!scopes.is_empty()).then_some(scopes)
    }

    fn accepts_session(&self) -> bool {
        self.security
            .iter()
            .any(|requirement| requirement.get("session").is_some())
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

/// Routes outside the OpenAPI document; every one needs a session.
const UNDOCUMENTED: &[(&str, &str)] = &[("get", "/media/{file}"), ("head", "/media/{file}")];

/// A concrete request for `path`, its parameters filled with placeholders, as
/// the SPA sends it (so the CSRF guard lets it through).
fn request_for(t: &TestState, method: &str, path: &str) -> Request<Body> {
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
    let request = builder
        .body(if with_body {
            Body::from("{}")
        } else {
            Body::empty()
        })
        .unwrap();
    from_spa(t, request)
}

#[tokio::test]
async fn the_access_policy_matches_the_document() {
    let operations = operations();
    let lower = |method: &Method| method.as_str().to_ascii_lowercase();

    let documented: BTreeSet<(String, String)> = operations
        .iter()
        .filter(|op| op.is_public())
        .map(|op| (op.method.clone(), op.path.clone()))
        .collect();
    let policy = routes::access();
    let declared: BTreeSet<(String, String)> = policy
        .rules()
        .iter()
        .filter(|rule| rule.access == Access::Public)
        .map(|rule| (lower(&rule.method), rule.path.clone()))
        .collect();
    assert_eq!(
        documented, declared,
        "public operations (security(())) and routes::PUBLIC_ROUTES differ"
    );

    let documented: BTreeSet<(String, String, Vec<String>, bool)> = operations
        .iter()
        .filter_map(|op| {
            let scopes = op.bearer_scopes()?;
            Some((
                op.method.clone(),
                op.path.clone(),
                scopes,
                op.accepts_session(),
            ))
        })
        .collect();
    let declared: BTreeSet<(String, String, Vec<String>, bool)> = policy
        .rules()
        .iter()
        .filter_map(|rule| match rule.access {
            Access::Token { scopes, session } => {
                let mut names: Vec<String> = scopes.iter().map(|s| s.as_str().to_owned()).collect();
                names.sort();
                Some((lower(&rule.method), rule.path.clone(), names, session))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        documented, declared,
        "token operations (security bearer) and routes::TOKEN_ROUTES differ"
    );
    for op in operations.iter().filter(|op| !op.is_public()) {
        assert!(
            op.accepts_session() || op.bearer_scopes().is_some(),
            "{} {} names no security scheme",
            op.method,
            op.path
        );
    }
}

#[tokio::test]
async fn every_protected_route_answers_401_without_a_session() {
    let t = TestState::with_config(without_sign_in_limit);
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let token = api_token(
        &t,
        &owner_id,
        "ingest tasks uploads lookup links:create migrate",
    );
    let unknown = SecretToken::generate();

    let mut routes: Vec<(String, String, bool)> = operations()
        .into_iter()
        .filter(|op| !op.is_public())
        .map(|op| {
            let takes_tokens = op.bearer_scopes().is_some();
            (op.method, op.path, takes_tokens)
        })
        .collect();
    routes.extend(
        UNDOCUMENTED
            .iter()
            .map(|(method, path)| ((*method).to_owned(), (*path).to_owned(), false)),
    );
    for (method, path, takes_tokens) in &routes {
        let route = format!("{} {path}", method.to_ascii_uppercase());
        let mut attempts = vec![
            ("no credentials", request_for(&t, method, path)),
            (
                "a malformed cookie",
                with_session(request_for(&t, method, path), "garbage"),
            ),
            (
                "an unknown session",
                with_session(request_for(&t, method, path), unknown.expose()),
            ),
        ];
        if !takes_tokens {
            attempts.push((
                "an API token beside a valid cookie",
                bearer(with_session(request_for(&t, method, path), &cookie), &token),
            ));
        }
        for (what, request) in attempts {
            let response = send(&app, request).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{route} with {what}"
            );
            if method != "head" {
                let refused = problem(response, StatusCode::UNAUTHORIZED).await;
                assert_eq!(refused.code, ErrorCode::Unauthorized, "{route}");
            }
        }
    }
    assert!(
        routes.len() >= 10,
        "the read API, account and media routes are protected"
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
    let token = token_of(&link_in(&message));
    let response = send(&app, redeem_request(&t, &token)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookie = session_cookie(&response).unwrap();
    assert_eq!(me(&app, &cookie).await, StatusCode::OK);
}

#[test]
fn the_policy_api_is_what_routes_use() {
    // A route opened to tokens by a later task looks like this.
    let policy: AccessPolicy =
        routes::access().token(Method::POST, "/api/v1/migrations", Scope::Migrate, false);
    assert_eq!(
        policy.access(&Method::POST, "/api/v1/migrations"),
        Access::Token {
            scopes: Scope::Migrate.into(),
            session: false
        }
    );
    assert_eq!(policy.access(&Method::GET, "/api/v1/me"), Access::Session);
    assert_eq!(policy.access(&Method::HEAD, "/health"), Access::Public);
}
