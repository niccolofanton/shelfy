//! Shared helpers of the server integration tests: a state on a temporary
//! data directory and request helpers over `tower::ServiceExt`.

#![allow(dead_code)] // each test binary uses a different subset

pub mod auth;
pub mod cdn;
pub mod jobs;
pub mod library;
pub mod passkey;
pub mod sleeping;
pub mod sse;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, Request, Response, StatusCode, header};
use http_body_util::BodyExt as _;
use shelfy_server::config::{Config, DEFAULT_PUBLIC_URL, DataDir};
use shelfy_server::error::{PROBLEM_JSON, Problem};
use shelfy_server::outbound::Lookup;
use shelfy_server::rate_limit::RateLimitConfig;
use shelfy_server::state::AppState;
use tempfile::TempDir;
use tower::ServiceExt as _;

/// A state on its own temporary data directory, removed on drop.
pub struct TestState {
    pub dir: TempDir,
    pub state: AppState,
}

impl TestState {
    /// A state with the default configuration.
    pub fn new() -> Self {
        Self::with_config(|_| {})
    }

    /// A state whose configuration `edit` adjusts first.
    ///
    /// The user rate limits are off: tests send their requests in bursts.
    /// `tests/rate_limits.rs` turns them on; the sign-in limit per client
    /// stays on. No host name resolves, so no test reaches the network (a
    /// queued archive drain, for one): tests that fetch configure the
    /// fixture CDN's outbound settings.
    pub fn with_config(edit: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut config = Config::with_data_dir(DataDir::new(dir.path()).expect("data dir"));
        config.rate_limits = RateLimitConfig::disabled();
        config.outbound.lookup = Lookup::fixed::<_, &str>([]);
        edit(&mut config);
        let state = AppState::open(config).expect("open state");
        Self { dir, state }
    }

    /// The real application on this state.
    pub fn app(&self) -> Router {
        shelfy_server::app::app(self.state.clone())
    }

    /// The data directory.
    pub fn data_dir(&self) -> DataDir {
        self.state.config().data_dir.clone()
    }
}

/// Sends one request through `app`.
pub async fn send(app: &Router, request: Request<Body>) -> Response<Body> {
    app.clone()
        .oneshot(request)
        .await
        .expect("the router is infallible")
}

/// A `GET` request.
pub fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).expect("request")
}

/// A `POST` with a JSON body, as the web app sends it ([`from_app`]).
pub fn post_json(uri: &str, body: impl Into<Body>) -> Request<Body> {
    from_app(
        Request::post(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.into())
            .expect("request"),
    )
}

/// `request` with the headers the web app sends on every state-changing
/// request, which the CSRF guard checks: `Origin` (the default public URL),
/// `Sec-Fetch-Site: same-origin` and `X-Shelfy-Client: web`.
pub fn from_app(mut request: Request<Body>) -> Request<Body> {
    let headers = request.headers_mut();
    headers.insert(header::ORIGIN, HeaderValue::from_static(DEFAULT_PUBLIC_URL));
    headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
    headers.insert("x-shelfy-client", HeaderValue::from_static("web"));
    request
}

/// The whole body.
pub async fn body(response: Response<Body>) -> Bytes {
    response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
}

/// The body as JSON.
pub async fn json(response: Response<Body>) -> serde_json::Value {
    serde_json::from_slice(&body(response).await).expect("JSON body")
}

/// Checks that `response` is a problem with `status` and returns it.
pub async fn problem(response: Response<Body>, status: StatusCode) -> Problem {
    assert_eq!(response.status(), status, "status");
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        PROBLEM_JSON,
        "content type"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let problem: Problem = serde_json::from_slice(&body(response).await).expect("problem body");
    assert_eq!(problem.status, status.as_u16(), "status member");
    assert_eq!(problem.kind, "about:blank");
    problem
}

/// Whether `id` looks like a ULID: 26 Crockford base32 characters.
pub fn is_ulid(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_uppercase() && !b"ILOU".contains(&b)))
}
