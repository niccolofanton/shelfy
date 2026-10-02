//! `GET /api/v1/events`: the realtime stream of a tab (plan §2.10, D7).
//!
//! **A stub until P1-01.** Today the stream sends `hello`, then a comment
//! heartbeat every 20 s, and ends when the server shuts down. P1-01 replaces
//! the body with the per-user bus ([`crate::events`]): typed events with
//! `id:`, `Last-Event-ID` replay, `resync` and the `topics` filter. What stays:
//! the route, its `streams` group (no handler time limit), `hello` first, the
//! heartbeat and the headers below.
//!
//! Headers: `Content-Type: text/event-stream`, `Cache-Control: no-store`, and
//! `X-Accel-Buffering: no` so nginx forwards every event at once. Compression
//! skips event streams ([`crate::app`]).

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::{StreamExt as _, future, stream};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::current_user::CurrentUser;
use crate::state::AppState;

/// Interval of the comment heartbeat. Cloudflare drops a proxied connection
/// after 100 s without a byte (SPIKE-10 measured the cut at about 125 s).
pub const HEARTBEAT: Duration = Duration::from_secs(20);

/// `X-Accel-Buffering`: tells nginx not to buffer the stream.
pub const X_ACCEL_BUFFERING: HeaderName = HeaderName::from_static("x-accel-buffering");

/// The `data` of the `hello` event, the first event of every stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HelloEvent {
    /// Version of the server build; a change means a new deploy.
    pub version: String,
    /// Milliseconds between two heartbeat comments. A stream silent for much
    /// longer is dead: reconnect.
    pub heartbeat_ms: u64,
}

/// The realtime stream of the signed-in user.
///
/// A `text/event-stream`: the `hello` event (data: `HelloEvent`), then a
/// comment line every 20 s. The stream ends when the server shuts down;
/// reconnect with backoff.
#[utoipa::path(
    get,
    path = "/api/v1/events",
    tag = "platform",
    operation_id = "streamEvents",
    responses(
        (
            status = OK,
            description = "Server-sent events: `hello` first, then a heartbeat comment every 20 s.",
            content_type = "text/event-stream",
            body = String,
            headers(
                ("Cache-Control" = String, description = "`no-store`."),
                ("X-Accel-Buffering" = String, description = "`no`: proxies must not buffer the stream."),
            )
        ),
    )
)]
pub async fn stream_events(State(state): State<AppState>, _user: CurrentUser) -> Response {
    let hello = HelloEvent {
        version: crate::VERSION.to_owned(),
        heartbeat_ms: u64::try_from(HEARTBEAT.as_millis()).unwrap_or(u64::MAX),
    };
    let hello = Event::default()
        .event("hello")
        .json_data(hello)
        .expect("the hello event serializes");
    // `hello`, then nothing until shutdown; the keep-alive fills the silence.
    let shutdown = state.shutdown_token().clone();
    let events = stream::once(future::ready(Ok::<_, Infallible>(hello)))
        .chain(stream::once(shutdown.cancelled_owned()).filter_map(|()| future::ready(None)));
    let sse = Sse::new(events).keep_alive(KeepAlive::new().interval(HEARTBEAT).text("heartbeat"));
    (
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (X_ACCEL_BUFFERING, HeaderValue::from_static("no")),
        ],
        sse,
    )
        .into_response()
}
