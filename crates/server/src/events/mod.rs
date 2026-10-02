//! Realtime events over SSE (plan §2.10). **Empty in P0**: P1-01 fills it.
//!
//! What lands here:
//!
//! - a per-user `tokio::sync::broadcast` bus (capacity 256), a ring buffer of
//!   the last 256 events (5 minutes) for `Last-Event-ID`, and the coalescer of
//!   `posts.changed` and `stats.changed`;
//! - the stream behind `GET /api/v1/events`: typed events with `id:`, the
//!   `topics` filter, `Last-Event-ID` replay, and `resync` when a gap is too
//!   old or a receiver lags.
//!
//! Seams already in place:
//!
//! - the route exists (T11, [`crate::routes::events`]), in the `streams` group
//!   with no handler time limit ([`crate::limits::RouteLimits::STREAM`]). It
//!   already sends `hello` on connect and a comment heartbeat every 20 s with
//!   `X-Accel-Buffering: no` and `Cache-Control: no-store`; P1-01 replaces the
//!   stream between `hello` and the end with the bus;
//! - streams end when the shutdown token fires
//!   ([`crate::state::AppState::shutdown_token`]), so a graceful shutdown never
//!   waits on an open stream;
//! - compression already skips `text/event-stream` ([`crate::app`]);
//! - the library generation ([`shelfy_core::db::Generation`]) changes with
//!   every write, so the ETags of [`crate::conditional`] already expire on the
//!   writes that will emit `posts.changed`.
