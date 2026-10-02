//! Signing in from tests: the owner account, sign-in links, session cookies
//! and the requests the SPA sends.
//!
//! A protected route's test signs in with [`sign_in`] and sends its request
//! through [`with_session`] (safe methods) or [`spa`] (state-changing ones,
//! which the CSRF guard checks).

use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Request, Response, StatusCode, header};
use serde_json::json;
use shelfy_server::admin::login_link::create_login_link;
use shelfy_server::admin::owner::create_owner;
use shelfy_server::auth::cookie::SESSION_COOKIE;
use shelfy_server::auth::csrf::{CLIENT_HEADER, CLIENT_WEB};
use shelfy_server::mail::DEV_MAILBOX_DIR;

use super::{TestState, send};

/// Email of the test owner.
pub const OWNER_EMAIL: &str = "owner@example.test";

/// The SPA page a sign-in link opens; the token follows `#`.
pub const LINK_PAGE: &str = "/login/magic#";

/// The route that redeems a link's token.
pub const REDEEM: &str = "/api/v1/auth/magic-links/redeem";

/// Creates the owner (idempotent); returns its id.
pub fn owner(t: &TestState) -> String {
    create_owner(&t.data_dir(), OWNER_EMAIL)
        .expect("create the owner")
        .user_id()
        .to_owned()
}

/// Mints a sign-in link for `email`, as `admin login-link` does; returns its
/// token.
pub fn link_token(t: &TestState, email: &str) -> String {
    let link = create_login_link(
        &t.data_dir(),
        &t.state.config().public_url,
        email,
        Duration::from_secs(15 * 60),
    )
    .expect("mint a sign-in link");
    token_of(link.url.expose())
}

/// The token of a link URL: what follows `#`.
pub fn token_of(url: &str) -> String {
    let (_, token) = url.split_once(LINK_PAGE).expect("a sign-in link URL");
    token.to_owned()
}

/// The request the sign-in page sends to redeem `token`, as the SPA sends it.
pub fn redeem_request(t: &TestState, token: &str) -> Request<Body> {
    let request = Request::post(REDEEM)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "token": token }).to_string()))
        .expect("request");
    from_spa(t, request)
}

/// Signs the owner in with a fresh link, redeemed as the sign-in page does;
/// returns the session cookie's value. Creates the owner if needed.
pub async fn sign_in(app: &Router, t: &TestState) -> String {
    owner(t);
    let token = link_token(t, OWNER_EMAIL);
    let response = send(app, redeem_request(t, &token)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "sign-in");
    session_cookie(&response).expect("the sign-in sets the session cookie")
}

/// The value the response's `Set-Cookie` gives the session cookie, if any
/// (empty when it clears it).
pub fn session_cookie(response: &Response<Body>) -> Option<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _attributes) = value.split_once(';').unwrap_or((value, ""));
            let (name, value) = pair.split_once('=')?;
            (name == SESSION_COOKIE).then(|| value.to_owned())
        })
}

/// `request` with the session cookie `value`.
pub fn with_session(mut request: Request<Body>, value: &str) -> Request<Body> {
    request.headers_mut().append(
        header::COOKIE,
        format!("{SESSION_COOKIE}={value}").parse().unwrap(),
    );
    request
}

/// `request` with the headers the SPA sends on a state-changing request:
/// `Origin` (the public URL of `t`), `Sec-Fetch-Site: same-origin` and
/// `X-Shelfy-Client: web`.
pub fn from_spa(t: &TestState, mut request: Request<Body>) -> Request<Body> {
    let headers = request.headers_mut();
    headers.insert(
        header::ORIGIN,
        t.state.config().public_url.as_str().parse().unwrap(),
    );
    headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
    headers.insert(CLIENT_HEADER, HeaderValue::from_static(CLIENT_WEB));
    request
}

/// `request` as the SPA sends it when signed in: [`from_spa`] plus the
/// session cookie `value`.
pub fn spa(t: &TestState, request: Request<Body>, value: &str) -> Request<Body> {
    with_session(from_spa(t, request), value)
}

/// An empty `POST`.
pub fn post(uri: &str) -> Request<Body> {
    Request::post(uri).body(Body::empty()).expect("request")
}

/// A read-write connection to the control database of `t`, beside the
/// server's.
pub fn control_db(t: &TestState) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(t.data_dir().control_db()).expect("control database");
    conn.busy_timeout(Duration::from_secs(5))
        .expect("busy timeout");
    conn
}

/// The `meta_json` of every audit row with `action`, oldest first.
pub fn audit_rows(conn: &rusqlite::Connection, action: &str) -> Vec<serde_json::Value> {
    let mut statement = conn
        .prepare("SELECT meta_json FROM audit_log WHERE action = ?1 ORDER BY id")
        .expect("audit query");
    statement
        .query_map([action], |row| row.get::<_, String>(0))
        .expect("audit rows")
        .map(|meta| serde_json::from_str(&meta.expect("meta")).expect("JSON meta"))
        .collect()
}

/// Makes every session's last proof of identity 6 minutes old, so routes
/// that need a recent sign-in refuse it, and drops the cached lookups.
pub fn make_sessions_stale(t: &TestState) {
    control_db(t)
        .execute(
            "UPDATE sessions SET reauth_at = ?1",
            [shelfy_server::ids::now_ms() - 6 * 60_000],
        )
        .expect("stale sessions");
    t.state.auth().forget_all_sessions();
}

/// Adds an active member with `email`; returns its id.
pub fn add_member(t: &TestState, email: &str) -> String {
    let id = shelfy_server::ids::new_ulid();
    control_db(t)
        .execute(
            "INSERT INTO users (id, email, role, quota_bytes, created_at) \
             VALUES (?1, ?2, 'member', 0, 0)",
            [&id, email],
        )
        .expect("member");
    id
}

/// Signs in the account with `email` with a fresh link; returns the session
/// cookie.
pub async fn sign_in_as(app: &Router, t: &TestState, email: &str) -> String {
    let token = link_token(t, email);
    let response = send(app, redeem_request(t, &token)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "sign-in");
    session_cookie(&response).expect("the sign-in sets the session cookie")
}

/// The dev mailbox directory of `t`.
pub fn mailbox_dir(t: &TestState) -> PathBuf {
    t.data_dir().root().join(DEV_MAILBOX_DIR)
}

/// Every message in the dev mailbox once no email is in flight, with
/// quoted-printable soft line breaks removed (long lines, such as the link,
/// are wrapped on the wire).
pub async fn mailbox(t: &TestState) -> Vec<String> {
    t.state.auth().mail_idle().await;
    let Ok(entries) = std::fs::read_dir(mailbox_dir(t)) else {
        return Vec::new();
    };
    entries
        .map(|entry| {
            let raw = std::fs::read_to_string(entry.expect("entry").path()).expect("message");
            raw.replace("=\r\n", "")
        })
        .collect()
}

/// The sign-in link URL in an email.
pub fn link_in(message: &str) -> String {
    message
        .split_whitespace()
        .find(|word| word.contains(LINK_PAGE))
        .expect("a sign-in link in the message")
        .to_owned()
}
