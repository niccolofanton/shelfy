//! The middleware stack around the routes (plan §2.3, §2.9, §3.7).
//!
//! From the outside in, every request passes through:
//!
//! 1. [`observe`](crate::telemetry::http::observe): request id, `request`
//!    span, response log line, HTTP metrics;
//! 2. compression (gzip, brotli) of responses over 1 KiB, except images,
//!    audio, video and event streams;
//! 3. [`problem_fallback`](crate::error::problem_fallback): error responses
//!    from any layer become problems;
//! 4. panic catching: a panicking handler answers 500 `internal`;
//! 5. the CSRF and Origin guard ([`auth::csrf`]): a state-changing request
//!    with the session cookie must come from the public origin, or it
//!    answers 403 `csrf_failed`;
//! 6. authentication ([`auth::session::authenticate`]): a valid session
//!    cookie puts the user in the request extensions
//!    ([`CurrentUser`](crate::current_user::CurrentUser),
//!    [`auth::SessionUser`]) and in the request span;
//! 7. the route group's body limit and time limit ([`crate::limits`]);
//! 8. the handler. Protected handlers take the user as an extractor, which
//!    answers 401 when authentication found none.
//!
//! The stack is applied after routing, so the route template is known to the
//! logs and metrics. Rate limits (P1-15) and the security headers (P1-09)
//! join here.

use axum::Router;
use axum::middleware;
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{NotForContentType, Predicate as _, SizeAbove};
use utoipa_axum::router::OpenApiRouter;

use crate::auth;
use crate::error::{self, ApiError};
use crate::routes;
use crate::state::AppState;
use crate::telemetry;

/// Smallest response body worth compressing, in bytes.
const COMPRESSION_MIN_BYTES: u64 = 1024;

/// The application: every route of [`routes::router`] behind the stack.
pub fn app(state: AppState) -> Router {
    build(state, routes::router())
}

/// Puts `routes` behind the stack. [`app`] uses it with the real routes;
/// tests use it with extra routes.
pub fn build(state: AppState, routes: OpenApiRouter<AppState>) -> Router {
    let (router, _document) = routes.split_for_parts();
    let compression = CompressionLayer::new().compress_when(
        SizeAbove::new(COMPRESSION_MIN_BYTES)
            .and(NotForContentType::GRPC)
            .and(NotForContentType::IMAGES)
            .and(NotForContentType::SSE)
            .and(NotForContentType::const_new("audio/"))
            .and(NotForContentType::const_new("video/")),
    );
    router
        .fallback(not_found)
        .layer(
            ServiceBuilder::new()
                .layer(middleware::from_fn(telemetry::http::observe))
                .layer(compression)
                .layer(middleware::from_fn(error::problem_fallback))
                .layer(CatchPanicLayer::custom(error::panic_response))
                .layer(middleware::from_fn_with_state(
                    state.clone(),
                    auth::csrf::protect,
                ))
                .layer(middleware::from_fn_with_state(
                    state.clone(),
                    auth::session::authenticate,
                )),
        )
        .with_state(state)
}

async fn not_found() -> ApiError {
    ApiError::not_found()
}
