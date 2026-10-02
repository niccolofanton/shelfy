//! The security headers of plan §7.1 on every kind of response (P1-09): the
//! web app's page and assets, the API (public and signed-in routes, a job
//! route, problems, a CSRF refusal) and media.

mod support;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use shelfy_media::store::{IngestLimits, MediaStore};
use shelfy_server::config::PublicUrl;
use shelfy_server::security_headers::{CONTENT_SECURITY_POLICY, STRICT_TRANSPORT_SECURITY};
use shelfy_server::static_files::WebApp;
use support::auth::{owner, sign_in, with_session};
use support::{TestState, get, send};
use tempfile::TempDir;

const HTTPS_HOST: &str = "https://refs.example.test";

/// A server for `public_url` that serves a small web app; the web app's
/// directory lives as long as the returned guard.
fn serve(public_url: &str) -> (TestState, TempDir) {
    let web = tempfile::tempdir().unwrap();
    std::fs::create_dir(web.path().join("assets")).unwrap();
    std::fs::write(web.path().join("index.html"), "<!doctype html>").unwrap();
    std::fs::write(web.path().join("assets/index-Abc123.js"), "export {};").unwrap();
    let app = WebApp::load(web.path()).unwrap();
    let public_url = PublicUrl::parse(public_url).unwrap();
    let t = TestState::with_config(|config| {
        config.public_url = public_url;
        config.web = Some(app);
    });
    (t, web)
}

/// Stores a JPEG-like object for `user`; returns its URL.
fn store_media(t: &TestState, user: &str) -> String {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    bytes.extend((0..3_000_u32).map(|i| (i % 251) as u8));
    let stored = MediaStore::new(t.data_dir().users_dir())
        .user(user)
        .unwrap()
        .ingest(bytes.as_slice(), IngestLimits::UPLOAD)
        .unwrap()
        .publish()
        .unwrap();
    format!("/media/{}", stored.name())
}

/// One response of each kind, signed in where the route needs it.
async fn responses(app: &Router, t: &TestState) -> Vec<(&'static str, Response<Body>)> {
    let cookie = sign_in(app, t).await;
    let media = store_media(t, &owner(t));
    let signed_in = |uri: &str| with_session(get(uri), &cookie);
    let csrf_refused = Request::post("/api/v1/auth/logout")
        .body(Body::empty())
        .unwrap();
    vec![
        ("page", send(app, get("/")).await),
        ("client route", send(app, get("/login/magic")).await),
        ("asset", send(app, get("/assets/index-Abc123.js")).await),
        ("health", send(app, get("/health")).await),
        ("openapi", send(app, get("/api/v1/openapi.json")).await),
        ("version", send(app, signed_in("/api/v1/version")).await),
        ("jobs", send(app, signed_in("/api/v1/jobs")).await),
        ("401", send(app, get("/api/v1/posts")).await),
        ("404", send(app, get("/api/v1/no-such-route")).await),
        ("csrf", send(app, csrf_refused).await),
        ("media", send(app, signed_in(&media)).await),
    ]
}

fn header<'a>(response: &'a Response<Body>, name: &header::HeaderName) -> Option<&'a str> {
    response.headers().get(name).map(|v| v.to_str().unwrap())
}

#[tokio::test]
async fn every_response_carries_the_policy_nosniff_and_hsts() {
    let (t, _web) = serve(HTTPS_HOST);
    let app = t.app();
    let sandboxed = format!("{CONTENT_SECURITY_POLICY}; sandbox");
    for (kind, response) in responses(&app, &t).await {
        let expected_status = match kind {
            "401" => StatusCode::UNAUTHORIZED,
            "404" => StatusCode::NOT_FOUND,
            "csrf" => StatusCode::FORBIDDEN,
            _ => StatusCode::OK,
        };
        assert_eq!(response.status(), expected_status, "{kind}");
        let policy = header(&response, &header::CONTENT_SECURITY_POLICY).unwrap();
        if kind == "media" {
            // The route's own `sandbox` stays, after the app's policy.
            assert_eq!(policy, sandboxed, "{kind}");
        } else {
            assert_eq!(policy, CONTENT_SECURITY_POLICY, "{kind}");
        }
        assert!(policy.contains("frame-ancestors 'none'"), "{kind}");
        assert_eq!(
            header(&response, &header::STRICT_TRANSPORT_SECURITY),
            Some(STRICT_TRANSPORT_SECURITY),
            "{kind}"
        );
        assert_eq!(
            header(&response, &header::X_CONTENT_TYPE_OPTIONS),
            Some("nosniff"),
            "{kind}"
        );
        assert_eq!(
            header(&response, &header::REFERRER_POLICY),
            Some("no-referrer"),
            "{kind}"
        );
        // Each route keeps its own caching.
        let cache = header(&response, &header::CACHE_CONTROL);
        match kind {
            "page" | "client route" => assert_eq!(cache, Some("no-cache")),
            "asset" => assert_eq!(cache, Some("public, max-age=31536000, immutable")),
            "media" => assert_eq!(cache, Some("private, max-age=31536000, immutable")),
            "openapi" => {}
            _ => assert_eq!(cache, Some("no-store"), "{kind}"),
        }
    }
}

#[tokio::test]
async fn a_local_http_server_sends_no_hsts() {
    let (t, _web) = serve("http://localhost:18189");
    let app = t.app();
    for (kind, response) in responses(&app, &t).await {
        assert!(
            response
                .headers()
                .get(header::STRICT_TRANSPORT_SECURITY)
                .is_none(),
            "{kind}"
        );
        let policy = header(&response, &header::CONTENT_SECURITY_POLICY).unwrap();
        assert!(policy.starts_with(CONTENT_SECURITY_POLICY), "{kind}");
        assert_eq!(
            header(&response, &header::X_CONTENT_TYPE_OPTIONS),
            Some("nosniff"),
            "{kind}"
        );
    }
}
