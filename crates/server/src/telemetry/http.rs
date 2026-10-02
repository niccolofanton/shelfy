//! Per-request observation: request id, `request` span, response log line and
//! HTTP metrics, in one middleware ([`observe`]).
//!
//! Only the route template (`/api/v1/posts/{key}`), never the URL, reaches the
//! logs and the metric labels, so paths with user data and query strings stay
//! out. The span declares an empty `user_id` field that the authentication
//! layer (T10) records once it knows the user.

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument as _;

use crate::ids::new_ulid;

/// Header that carries the request id on every response.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Route label of requests that matched no route.
pub const UNMATCHED_ROUTE: &str = "unmatched";

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

/// Middleware: assigns the request id, runs the request inside its span,
/// then logs the response and records the HTTP metrics.
///
/// It must run after routing (`Router::layer`), where [`MatchedPath`] is set.
pub async fn observe(mut request: Request, next: Next) -> Response {
    let started = Instant::now();
    let id = RequestId::new();
    request.extensions_mut().insert(id.clone());
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or(UNMATCHED_ROUTE, MatchedPath::as_str)
        .to_owned();
    let method = request.method().clone();
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
