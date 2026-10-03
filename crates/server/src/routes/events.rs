//! `GET /api/v1/events`: the realtime stream of a tab (plan §2.10, D7), fed
//! by the signed-in user's bus ([`crate::events`]).
//!
//! **Frames.** `hello` first; then, for a resuming client, the events it
//! missed (or `resync`); then replayable events, each with an `id:`, and
//! explicitly requested `ai.stream` frames without an id. A comment heartbeat
//! comes after 20 s without a frame. The stream ends
//! when the server shuts down: reconnect with backoff.
//!
//! **Session.** A stream lives no longer than the session that opened it.
//! Every 20 s the stream looks its session up again, through the
//! authentication layer's cache ([`session::resolve`]), and ends when the
//! session is gone: signed out, signed out everywhere, replaced by a new
//! sign-in, or expired. Sign-outs in this process drop the cached session at
//! once, so their streams end within 20 s; a change made by another process
//! (the admin CLI) takes up to the cache's 60 s more. The reconnect then
//! answers 401.
//!
//! **Resuming.** A client resumes after the last event it saw, from the
//! `Last-Event-ID` header (what `EventSource` sends when it reconnects by
//! itself) or the `lastEventId` query parameter (for a client that opens a
//! new `EventSource` to reconnect with its own backoff); the header wins.
//! Event ids are opaque. The `id:` fields keep a resume lossless:
//!
//! - a fresh stream (no resume point) puts the current position on `hello`,
//!   so a reconnect before any event still resumes from there;
//! - a resuming stream sends `hello` without an id, then the missed events
//!   with theirs, or `resync` with the current position when they are gone.
//!
//! A client that remembers the last non-empty `lastEventId` of the events it
//! received therefore always holds the right resume point.
//!
//! **Topics.** `?topics=job.updated&topics=notification` keeps only those
//! events; without `topics`, every replayable topic comes. `ai.stream` needs
//! an explicit opt-in. `hello` and `resync` always come. A filtered stream that resumes gets `resync` when the ring no longer
//! covers its gap, even if the lost events were of other topics.
//!
//! Headers: `Content-Type: text/event-stream`, `Cache-Control: no-store`, and
//! `X-Accel-Buffering: no` so nginx forwards every event at once. Compression
//! skips event streams ([`crate::app`]).

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::{StreamExt as _, future, stream};
use serde::Deserialize;
use utoipa::IntoParams;

use tokio::time::{Instant, MissedTickBehavior};

use super::listing::check_count;
use crate::auth::{cookie, session};
use crate::current_user::CurrentUser;
use crate::error::ApiError;
pub use crate::events::model::HelloEvent;
use crate::events::model::{EventTopic, ResyncEvent, ResyncReason, TopicSet};
use crate::events::{Delivery, LiveEvent, Published, Subscription};
use crate::extract::Query;
use crate::ids::now_ms;
use crate::state::AppState;

/// Interval of the comment heartbeat. Cloudflare drops a proxied connection
/// after 100 s without a byte (SPIKE-10 measured the cut at about 125 s).
pub const HEARTBEAT: Duration = Duration::from_secs(20);

/// `X-Accel-Buffering`: tells nginx not to buffer the stream.
pub const X_ACCEL_BUFFERING: HeaderName = HeaderName::from_static("x-accel-buffering");

/// `Last-Event-ID`: the resume point `EventSource` sends when it reconnects.
pub const LAST_EVENT_ID: HeaderName = HeaderName::from_static("last-event-id");

/// Name of the first event of every stream.
pub const HELLO: &str = "hello";
/// Name of the event that asks the client to reload everything.
pub const RESYNC: &str = "resync";

/// Filters and resume point of `GET /api/v1/events`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(default, rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct EventsQuery {
    /// Only these events. Repeatable. Default: every replayable topic;
    /// `ai.stream` requires an explicit opt-in.
    #[param(style = Form, explode)]
    pub topics: Vec<EventTopic>,
    /// Resume after this event id, like `Last-Event-ID`; for clients that
    /// reconnect with a new `EventSource`. The header wins when both are sent.
    #[param(nullable = false)]
    pub last_event_id: Option<String>,
}

/// The request header of a resuming stream, for the OpenAPI document.
#[derive(Clone, Debug, Default, IntoParams)]
#[into_params(parameter_in = Header)]
pub struct ResumeHeaders {
    /// The id of the last event received; `EventSource` sends it when it
    /// reconnects by itself.
    #[param(rename = "Last-Event-ID", nullable = false)]
    pub last_event_id: Option<String>,
}

/// The realtime stream of the signed-in user.
///
/// A `text/event-stream`. Every frame's `data:` is one line of JSON; the
/// `ServerEvent` schema maps each event name to its payload:
///
/// | `event:` | `data:` |
/// |---|---|
/// | `hello` | `HelloEvent`, first on every stream |
/// | `resync` | `ResyncEvent`: events were lost, reload everything |
/// | `posts.changed` | `PostsChangedEvent` |
/// | `stats.changed` | `StatsChangedEvent` |
/// | `job.updated` | `JobUpdatedEvent` |
/// | `notification` | `Notification` |
///
/// Published events carry an `id:`. To resume after a disconnection, send the
/// last one received as `Last-Event-ID` (`EventSource` does it when it
/// reconnects by itself) or as `lastEventId`: the missed events follow
/// `hello`, or `resync` when they are no longer kept (256 events, 5 minutes).
/// A fresh stream's `hello` carries the current position as its `id:`; a
/// resuming stream's `hello` has none. Keep the last non-empty
/// `lastEventId` seen.
///
/// A comment line comes after 20 s without a frame. The stream ends when the
/// server shuts down, and within 20 s of the end of its session (sign-out,
/// expiry); reconnect with backoff, and sign in again on 401.
#[utoipa::path(
    get,
    path = "/api/v1/events",
    tag = "platform",
    operation_id = "streamEvents",
    params(EventsQuery, ResumeHeaders),
    responses(
        (
            status = OK,
            description = "Server-sent events: `hello`, the missed events or `resync` when \
                           resuming, then live events; a heartbeat comment after 20 s without \
                           a frame.",
            content_type = "text/event-stream",
            body = String,
            headers(
                ("Cache-Control" = String, description = "`no-store`."),
                ("X-Accel-Buffering" = String, description = "`no`: proxies must not buffer the stream."),
            )
        ),
    )
)]
pub async fn stream_events(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<Response, ApiError> {
    check_count("topics", query.topics.len())?;
    let topics = TopicSet::from_topics(&query.topics);
    let resume = resume_point(&headers, query.last_event_id.as_deref());
    let subscription = state.events().subscribe(user.id(), resume);
    let hello = hello(&subscription);
    let live = stream::unfold(subscription, move |mut subscription| async move {
        let frame = next_frame(&mut subscription, topics).await;
        Some((Ok::<_, Infallible>(frame), subscription))
    });
    let shutdown = state.shutdown_token().clone();
    let session = cookie::session_token(&headers).map(str::to_owned);
    let ended = session_ended(state, session, user.id().to_owned());
    let until = async move {
        tokio::select! {
            () = shutdown.cancelled() => {}
            () = ended => {}
        }
    };
    let events = stream::once(future::ready(Ok(hello)))
        .chain(live)
        .take_until(until);
    let sse = Sse::new(events).keep_alive(KeepAlive::new().interval(HEARTBEAT).text("heartbeat"));
    Ok((
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (X_ACCEL_BUFFERING, HeaderValue::from_static("no")),
        ],
        sse,
    )
        .into_response())
}

/// Resolves when the session of `token` no longer signs `user_id` in, checked
/// every [`HEARTBEAT`]. A failing lookup ends the stream too: the client
/// reconnects and resumes without loss. A stream opened without a session
/// cookie (the test stand-in for authentication) never ends this way.
pub(super) async fn session_ended(state: AppState, token: Option<String>, user_id: String) {
    let Some(token) = token else {
        return std::future::pending().await;
    };
    let mut ticks = tokio::time::interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        match session::resolve(&state, &token, now_ms()).await {
            Ok(Some(signed_in)) if signed_in.id() == user_id => {}
            Ok(_) => return,
            Err(err) => {
                tracing::warn!(error = %err, "event stream: the session lookup failed; closing");
                return;
            }
        }
    }
}

/// The resume point: the `Last-Event-ID` header, else `lastEventId`. Empty
/// values count as absent.
fn resume_point<'a>(headers: &'a HeaderMap, query: Option<&'a str>) -> Option<&'a str> {
    let present = |id: &&str| !id.is_empty();
    headers
        .get(LAST_EVENT_ID)
        .map(|value| value.to_str().unwrap_or("?"))
        .filter(present)
        .or(query.filter(present))
}

/// The `hello` frame; on a fresh stream it carries the current position.
fn hello(subscription: &Subscription) -> Event {
    let hello = HelloEvent {
        version: crate::VERSION.to_owned(),
        heartbeat_ms: u64::try_from(HEARTBEAT.as_millis()).unwrap_or(u64::MAX),
        last_event_id: subscription.head_id().to_owned(),
    };
    let event = Event::default().event(HELLO);
    let event = if subscription.is_fresh() {
        event.id(subscription.head_id())
    } else {
        event
    };
    event.json_data(hello).expect("the hello event serializes")
}

/// The next frame for this stream's topics.
async fn next_frame(subscription: &mut Subscription, topics: TopicSet) -> Event {
    loop {
        match subscription.next().await {
            Delivery::Event(event) if topics.contains(event.topic) => return frame(&event),
            Delivery::Live(event) if topics.contains(event.topic) => return live_frame(&event),
            Delivery::Event(_) | Delivery::Live(_) => {}
            Delivery::Resync { reason, id } => return resync(reason, &id),
        }
    }
}

fn frame(event: &Published) -> Event {
    Event::default()
        .event(event.topic.as_str())
        .id(&*event.id)
        .data(&*event.data)
}

/// A live-only event (`ai.stream`): the name and data, no `id:` (it is not a
/// resume point and never enters the ring, G3-7).
fn live_frame(event: &LiveEvent) -> Event {
    Event::default()
        .event(event.topic.as_str())
        .data(&*event.data)
}

fn resync(reason: ResyncReason, id: &str) -> Event {
    Event::default()
        .event(RESYNC)
        .id(id)
        .json_data(ResyncEvent { reason })
        .expect("the resync event serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_wins_and_empty_values_count_as_absent() {
        let mut headers = HeaderMap::new();
        assert_eq!(resume_point(&headers, None), None);
        assert_eq!(resume_point(&headers, Some("")), None);
        assert_eq!(resume_point(&headers, Some("a-1")), Some("a-1"));
        headers.insert(LAST_EVENT_ID, HeaderValue::from_static("a-2"));
        assert_eq!(resume_point(&headers, Some("a-1")), Some("a-2"));
        headers.insert(LAST_EVENT_ID, HeaderValue::from_static(""));
        assert_eq!(resume_point(&headers, Some("a-1")), Some("a-1"));
        assert_eq!(resume_point(&headers, None), None);
        // A header that is not text resolves to nothing known: resync.
        headers.insert(LAST_EVENT_ID, HeaderValue::from_bytes(b"\xff").unwrap());
        assert_eq!(resume_point(&headers, None), Some("?"));
    }
}
