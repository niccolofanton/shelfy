//! Realtime events over SSE (plan §2.10). **Empty in P0**: P1-01 fills it.
//!
//! What lands here:
//!
//! - a per-user `tokio::sync::broadcast` bus (capacity 256), a ring buffer of
//!   the last 256 events (5 minutes) for `Last-Event-ID`, and the coalescer of
//!   `posts.changed` and `stats.changed`;
//! - `GET /api/v1/events`: `hello` on connect, a comment heartbeat every 20 s,
//!   `X-Accel-Buffering: no` and `Cache-Control: no-store`, `resync` when a
//!   gap is too old or a receiver lags.
//!
//! Seams already in place:
//!
//! - [`crate::routes::router`] has a `streams` group with no handler time
//!   limit ([`crate::limits::RouteLimits::STREAM`]); the route goes there;
//! - streams end when the shutdown token fires
//!   ([`crate::state::AppState::shutdown_token`]), so a graceful shutdown never
//!   waits on an open stream;
//! - compression already skips `text/event-stream` ([`crate::app`]).
