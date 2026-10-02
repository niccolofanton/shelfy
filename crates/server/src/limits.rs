//! Per-route request limits (plan §2.3, limits table).
//!
//! Routes are registered in groups ([`crate::routes`]); each group gets one
//! [`RouteLimits`]: a request body cap and, unless the route streams, a
//! handler time limit. A body over the cap answers 413 `payload_too_large`,
//! whether `Content-Length` announces it or the body only grows past it; a
//! handler over its time answers 504 `timeout`.

use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use utoipa_axum::router::OpenApiRouter;

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

/// Time limit of a request handler (§2.3). SSE, chat and uploads are exempt.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The limits of a group of routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteLimits {
    /// Largest accepted request body, in bytes.
    pub body_bytes: usize,
    /// Handler time limit; `None` for streams and uploads.
    pub timeout: Option<Duration>,
}

impl RouteLimits {
    /// JSON API routes: 64 KiB bodies, 30 s.
    pub const STANDARD: Self = Self {
        body_bytes: 64 * KIB,
        timeout: Some(REQUEST_TIMEOUT),
    };

    /// `POST /ingest/batches` (P2): 8 MiB, 30 s.
    pub const INGEST: Self = Self {
        body_bytes: 8 * MIB,
        timeout: Some(REQUEST_TIMEOUT),
    };

    /// tus `PATCH /uploads/{id}` (T9): 16 MiB chunks, no handler time limit.
    pub const UPLOAD_CHUNK: Self = Self {
        body_bytes: 16 * MIB,
        timeout: None,
    };

    /// `POST /stt/transcriptions` (P3): 25 MiB, 30 s.
    pub const STT: Self = Self {
        body_bytes: 25 * MIB,
        timeout: Some(REQUEST_TIMEOUT),
    };

    /// Streamed responses (`GET /events`, `POST /search/chat`): 64 KiB
    /// bodies, no handler time limit. Streams end on shutdown instead.
    pub const STREAM: Self = Self {
        body_bytes: 64 * KIB,
        timeout: None,
    };

    /// Applies these limits to every route of `router`.
    ///
    /// Both body limits are set: axum's [`DefaultBodyLimit`] for its body
    /// extractors (its own default is 2 MB) and [`RequestBodyLimitLayer`] for
    /// everything else, including a `Content-Length` that is too large.
    #[must_use]
    pub fn apply<S>(self, router: OpenApiRouter<S>) -> OpenApiRouter<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        let router = router
            .layer(DefaultBodyLimit::max(self.body_bytes))
            .layer(RequestBodyLimitLayer::new(self.body_bytes));
        match self.timeout {
            Some(timeout) => router.layer(TimeoutLayer::with_status_code(
                StatusCode::GATEWAY_TIMEOUT,
                timeout,
            )),
            None => router,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_match_the_plan() {
        assert_eq!(RouteLimits::STANDARD.body_bytes, 65_536);
        assert_eq!(RouteLimits::INGEST.body_bytes, 8_388_608);
        assert_eq!(RouteLimits::UPLOAD_CHUNK.body_bytes, 16_777_216);
        assert_eq!(RouteLimits::STT.body_bytes, 26_214_400);
        assert_eq!(RouteLimits::STANDARD.timeout, Some(REQUEST_TIMEOUT));
        assert_eq!(RouteLimits::STREAM.timeout, None);
        assert_eq!(RouteLimits::UPLOAD_CHUNK.timeout, None);
    }
}
