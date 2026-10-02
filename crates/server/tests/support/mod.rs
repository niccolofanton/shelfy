//! Shared helpers of the server integration tests: a state on a temporary
//! data directory and request helpers over `tower::ServiceExt`.

#![allow(dead_code)] // each test binary uses a different subset

pub mod auth;
pub mod library;
pub mod sse;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, Response, StatusCode, header};
use http_body_util::BodyExt as _;
use shelfy_server::config::{Config, DataDir};
use shelfy_server::error::{PROBLEM_JSON, Problem};
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
    pub fn with_config(edit: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut config = Config::with_data_dir(DataDir::new(dir.path()).expect("data dir"));
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

/// A `POST` with a JSON body.
pub fn post_json(uri: &str, body: impl Into<Body>) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.into())
        .expect("request")
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
