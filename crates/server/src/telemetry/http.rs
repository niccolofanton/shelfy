//! Per-request observation: request id, `request` span, response log line and
//! HTTP metrics, in one middleware ([`observe`]).
//!
//! Only the route template (`/api/v1/posts/{key}`), never the URL, reaches the
//! logs and the metric labels, so paths with user data and query strings stay
//! out. A request no route matched is labelled [`SPA_ROUTE`] when the web
//! app's files answer it, [`UNMATCHED_ROUTE`] otherwise. The span declares an
//! empty `user_id` field that the authentication layer (T10) records once it
//! knows the user.

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument as _;

use crate::ids::new_ulid;
use crate::static_files;

/// Header that carries the request id on every response.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Route label of requests that matched no route and that the web app's
/// files do not answer: unknown API paths, probes, other methods.
pub const UNMATCHED_ROUTE: &str = "unmatched";

/// Route label of the requests the web app's files answer (the router's
/// fallback, [`crate::static_files`]): its pages, assets and other files.
pub const SPA_ROUTE: &str = "spa";

/// Route whose successful requests are logged at `debug` (probes hit it every
/// few seconds).
const HEALTH_ROUTE: &str = "/health";

/// The id of one request: a ULID minted by the server. A client-supplied
/// `x-request-id` is ignored, so log correlation cannot be spoofed.
///
/// Handlers can extract it with `Extension<RequestId>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestId(Arc<str>);

impl RequestId {
    /// A fresh id.
    #[must_use]
    pub fn new() -> Self {
        Self(new_ulid().into())
    }

    /// The id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What [`observe`] knows of the router it watches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Observe {
    web_app: bool,
}

impl Observe {
    /// For a router whose fallback serves the web app's files when
    /// `web_app` is set (`SHELFY_WEB_DIR`), or answers 404 otherwise.
    #[must_use]
    pub fn new(web_app: bool) -> Self {
        Self { web_app }
    }
}

/// Middleware: assigns the request id, runs the request inside its span,
/// then logs the response and records the HTTP metrics.
///
/// It must run after routing (`Router::layer`), where [`MatchedPath`] is set.
pub async fn observe(State(observe): State<Observe>, mut request: Request, next: Next) -> Response {
    let started = Instant::now();
    let id = RequestId::new();
    request.extensions_mut().insert(id.clone());
    let route = match request.extensions().get::<MatchedPath>() {
        Some(path) => path.as_str(),
        None if observe.web_app && static_files::serves(request.method(), request.uri().path()) => {
            SPA_ROUTE
        }
        None => UNMATCHED_ROUTE,
    }
    .to_owned();
    let method = request.method().clone();
    let media_path =
        (route == super::metrics::MEDIA_ROUTE).then(|| request.uri().path().to_owned());
    let span = tracing::info_span!(
        "request",
        request_id = %id,
        method = %method,
        route = %route,
        user_id = tracing::field::Empty,
    );

    let mut response = next.run(request).instrument(span.clone()).await;

    let header = HeaderValue::from_str(id.as_str()).expect("a ULID is a valid header value");
    response.headers_mut().insert(REQUEST_ID_HEADER, header);
    let status = response.status();
    let elapsed = started.elapsed();
    super::metrics::record_http_request(&route, &method, status, elapsed);
    if let Some(path) = media_path {
        super::metrics::record_media_request(&path, elapsed);
    }

    let latency_ms = elapsed.as_secs_f64() * 1000.0;
    let status = status.as_u16();
    span.in_scope(|| {
        if status >= 500 {
            tracing::error!(status, latency_ms, "request failed");
        } else if route == HEALTH_ROUTE && status < 400 {
            tracing::debug!(status, latency_ms, "request");
        } else {
            tracing::info!(status, latency_ms, "request");
        }
    });
    response
}
