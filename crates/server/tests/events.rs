//! `GET /api/v1/events`, the T11 stub of the realtime stream (plan §2.10):
//! `hello` first, a comment heartbeat every 20 s, the stream headers, and the
//! end of the stream at shutdown.

mod support;

use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use shelfy_server::error::ErrorCode;
use shelfy_server::routes::events::{HEARTBEAT, HelloEvent};
use support::library::ALICE;
use support::{TestState, get, problem, send};
use tokio::time::Instant;

/// The next data frame of `body`, as text.
async fn next_frame(body: &mut Body) -> String {
    let frame = body
        .frame()
        .await
        .expect("the stream is still open")
        .expect("the stream does not fail");
    let data: Bytes = frame.into_data().expect("a data frame");
    String::from_utf8(data.to_vec()).expect("UTF-8")
}

// Paused time: the heartbeat timer fires as soon as the test waits for it,
// and the elapsed time shows its interval exactly.
#[tokio::test(start_paused = true)]
async fn the_stream_says_hello_then_heartbeats_until_shutdown() {
    let t = TestState::new();
    let request = Request::get("/api/v1/events")
        .header(header::ACCEPT_ENCODING, "gzip, br")
        .body(Body::empty())
        .unwrap();
    let response = send(&t.app_as(ALICE), request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert_eq!(headers[header::CONTENT_TYPE], "text/event-stream");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers["x-accel-buffering"], "no");
    assert!(
        headers.get(header::CONTENT_ENCODING).is_none(),
        "an event stream is never compressed"
    );

    let mut body = response.into_body();
    let started = Instant::now();
    let hello = next_frame(&mut body).await;
    let data = hello
        .strip_prefix("event: hello\ndata: ")
        .and_then(|rest| rest.strip_suffix("\n\n"))
        .unwrap_or_else(|| panic!("not a hello event: {hello:?}"));
    let hello: HelloEvent = serde_json::from_str(data).unwrap();
    assert_eq!(
        hello,
        HelloEvent {
            version: shelfy_server::VERSION.to_owned(),
            heartbeat_ms: 20_000,
        }
    );
    assert_eq!(started.elapsed(), Duration::ZERO, "hello comes at once");

    // Three beats: a minute, past the 30 s limit of the standard routes.
    for n in 1..=3 {
        assert_eq!(next_frame(&mut body).await, ": heartbeat\n\n");
        assert_eq!(started.elapsed(), HEARTBEAT * n);
    }

    t.state.shutdown_token().cancel();
    assert!(body.frame().await.is_none(), "the stream ends at shutdown");
}

#[tokio::test]
async fn the_stream_needs_a_user() {
    let t = TestState::new();
    let problem = problem(
        send(&t.app(), get("/api/v1/events")).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert_eq!(problem.code, ErrorCode::Unauthorized);
}
