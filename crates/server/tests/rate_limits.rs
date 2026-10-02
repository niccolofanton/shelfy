//! The rate limits of plan §2.9 through the real application: per user, for
//! searches, for client error reports and for sign-in per client address.
//! Over a limit, the answer is 429 `rate_limited` with `Retry-After`, the
//! security headers and a request id, like any other answer.
//!
//! The quotas here are small and slow (a few requests an hour), so the
//! bursts decide and the clock does not; `rate_limit.rs` checks the plan's
//! numbers on a test clock.

mod support;

use std::net::SocketAddr;
use std::ops::RangeInclusive;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, Response, StatusCode, header};
use serde_json::json;
use shelfy_server::admin::migrate_token::{MIGRATE_TOKEN_TTL, create_migrate_token};
use shelfy_server::config::Config;
use shelfy_server::error::{ErrorCode, Problem};
use shelfy_server::rate_limit::{Quota, RateLimitConfig};
use shelfy_server::security_headers::CONTENT_SECURITY_POLICY;
use shelfy_server::telemetry::http::REQUEST_ID_HEADER;
use support::auth::{OWNER_EMAIL, from_spa, owner, post, sign_in, spa, with_session};
use support::library::{ALICE, BOB};
use support::{TestState, get, post_json, problem, send};

/// A state with the plan's user limits, adjusted by `edit`.
fn limited(edit: impl FnOnce(&mut RateLimitConfig)) -> TestState {
    TestState::with_config(|config: &mut Config| {
        let mut limits = RateLimitConfig::default();
        edit(&mut limits);
        config.rate_limits = limits;
    })
}

/// Checks that `response` is a 429 with `Retry-After` in `retry_after`
/// (seconds), the security headers and a request id; returns the problem.
async fn too_many(response: Response<Body>, retry_after: RangeInclusive<u32>) -> Problem {
    let headers = response.headers();
    let seconds: u32 = headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(retry_after.contains(&seconds), "Retry-After {seconds}");
    assert_eq!(
        headers[header::CONTENT_SECURITY_POLICY],
        CONTENT_SECURITY_POLICY
    );
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert!(headers.contains_key(REQUEST_ID_HEADER));
    let refused = problem(response, StatusCode::TOO_MANY_REQUESTS).await;
    assert_eq!(refused.code, ErrorCode::RateLimited);
    refused
}

async fn status(app: &Router, request: Request<Body>) -> StatusCode {
    send(app, request).await.status()
}

/// `request` from the TCP peer `ip`.
fn from_peer(mut request: Request<Body>, ip: &str) -> Request<Body> {
    let addr = SocketAddr::new(ip.parse().unwrap(), 40_000);
    request.extensions_mut().insert(ConnectInfo(addr));
    request
}

#[tokio::test]
async fn a_user_is_limited_over_the_api_whatever_the_credential() {
    // 5 at once, then one every 12 minutes.
    let t = limited(|limits| limits.user = Some(Quota::per_hour(5)));
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let token = create_migrate_token(&t.data_dir(), OWNER_EMAIL, MIGRATE_TOKEN_TTL)
        .unwrap()
        .token
        .expose()
        .clone();
    let with_token = || {
        Request::post("/api/v1/migrations/missing-objects")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"objects":[]}"#))
            .unwrap()
    };

    for _ in 0..3 {
        let request = with_session(get("/api/v1/version"), &cookie);
        assert_eq!(status(&app, request).await, StatusCode::OK);
    }
    for _ in 0..2 {
        assert_eq!(status(&app, with_token()).await, StatusCode::OK);
    }
    // The session and the token are the same user: the budget is spent.
    let refused = too_many(send(&app, with_token()).await, 700..=720).await;
    assert_eq!(
        refused.detail.as_deref(),
        Some("too many requests from this user")
    );
    let request = with_session(get("/api/v1/stats"), &cookie);
    too_many(send(&app, request).await, 700..=720).await;

    // Not counted: the gallery's thumbnails, the health check, anonymous
    // requests (refused at the gate).
    for _ in 0..10 {
        let thumbnail = format!("/media/{}.g480.webp", "ab".repeat(32));
        let request = with_session(get(&thumbnail), &cookie);
        assert_eq!(status(&app, request).await, StatusCode::NOT_FOUND);
        assert_eq!(status(&app, get("/health")).await, StatusCode::OK);
        let anonymous = get("/api/v1/version");
        assert_eq!(status(&app, anonymous).await, StatusCode::UNAUTHORIZED);
    }

    // Another user has a budget of their own.
    let bob = t.app_as(BOB);
    for _ in 0..5 {
        assert_eq!(status(&bob, get("/api/v1/version")).await, StatusCode::OK);
    }
    too_many(send(&bob, get("/api/v1/version")).await, 700..=720).await;
}

#[tokio::test]
async fn searches_have_a_limit_of_their_own() {
    // 3 searches at once, then one every 20 minutes; the user limit stays
    // the plan's (60 at once).
    let t = limited(|limits| limits.search = Some(Quota::per_hour(3)));
    let app = t.app_as(ALICE);
    for uri in [
        "/api/v1/search?q=lamp",
        "/api/v1/posts?q=lamp",
        "/api/v1/posts/count?concept=chair",
    ] {
        assert_eq!(status(&app, get(uri)).await, StatusCode::OK, "{uri}");
    }
    for uri in ["/api/v1/search?q=table", "/api/v1/posts?q=table&limit=10"] {
        let refused = too_many(send(&app, get(uri)).await, 1_150..=1_200).await;
        assert_eq!(
            refused.detail.as_deref(),
            Some("too many searches from this user"),
            "{uri}"
        );
    }
    // Browsing without text is no search.
    for uri in [
        "/api/v1/posts",
        "/api/v1/posts?q=",
        "/api/v1/posts?tag=lamp",
        "/api/v1/posts/count",
        "/api/v1/stats",
    ] {
        assert_eq!(status(&app, get(uri)).await, StatusCode::OK, "{uri}");
    }
}

#[tokio::test]
async fn client_error_reports_are_limited_per_user() {
    let t = limited(|limits| limits.client_errors = Some(Quota::per_hour(2)));
    let app = t.app_as(ALICE);
    let report = || {
        post_json(
            "/api/v1/client-errors",
            json!({ "view": "gallery", "message": "boom" }).to_string(),
        )
    };
    for _ in 0..2 {
        assert_eq!(status(&app, report()).await, StatusCode::NO_CONTENT);
    }
    let refused = too_many(send(&app, report()).await, 1_750..=1_800).await;
    assert_eq!(
        refused.detail.as_deref(),
        Some("too many error reports from this user")
    );
    // Other requests of the user still pass, and so do other users' reports.
    assert_eq!(status(&app, get("/api/v1/version")).await, StatusCode::OK);
    let bob = t.app_as(BOB);
    assert_eq!(status(&bob, report()).await, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn every_sign_in_route_shares_one_limit_per_client() {
    // The plan's limit: 10 a minute per client address.
    let t = TestState::new();
    let app = t.app();
    owner(&t);
    let client = "198.51.100.7";
    let from_app = |request: Request<Body>| from_peer(from_spa(&t, request), client);
    let mut statuses = Vec::new();
    for _ in 0..3 {
        statuses.push(status(&app, from_peer(get("/api/v1/auth/methods"), client)).await);
        statuses.push(status(&app, from_app(post("/api/v1/auth/logout"))).await);
    }
    for _ in 0..2 {
        let start = from_app(post("/api/v1/auth/passkeys/login/start"));
        statuses.push(status(&app, start).await);
    }
    let email = json!({ "email": OWNER_EMAIL }).to_string();
    for _ in 0..2 {
        let request = from_app(post_json("/api/v1/auth/magic-links", email.clone()));
        statuses.push(status(&app, request).await);
    }
    assert_eq!(statuses.len(), 10);
    assert!(
        !statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );

    // The 11th request, on any sign-in route, waits for the oldest to age out.
    let request = from_app(post_json("/api/v1/auth/magic-links", email.clone()));
    let refused = too_many(send(&app, request).await, 55..=60).await;
    assert_eq!(
        refused.detail.as_deref(),
        Some("too many sign-in requests from this address")
    );
    let methods = from_peer(get("/api/v1/auth/methods"), client);
    too_many(send(&app, methods).await, 55..=60).await;
    // Counted before the CSRF guard and the access gate.
    let forged = from_peer(post("/api/v1/auth/logout-all"), client);
    too_many(send(&app, forged).await, 55..=60).await;

    // Other clients, and the rest of the API, are not affected.
    let other = from_peer(get("/api/v1/auth/methods"), "198.51.100.8");
    assert_eq!(status(&app, other).await, StatusCode::OK);
    assert_eq!(
        status(&app, from_peer(get("/health"), client)).await,
        StatusCode::OK
    );
    let cookie_route = spa(&t, get("/api/v1/version"), "not-a-session");
    assert_eq!(
        status(&app, from_peer(cookie_route, client)).await,
        StatusCode::UNAUTHORIZED
    );
}
