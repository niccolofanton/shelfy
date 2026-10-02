//! The JSON request logs: one line per response with the request id, method,
//! route template, status and latency, and none of the URL, query string,
//! headers or body (plan §3.7). P1-15 extends this to every redacted type.

mod support;

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::post;
use serde_json::Value;
use shelfy_server::error::ApiError;
use shelfy_server::limits::RouteLimits;
use shelfy_server::telemetry::http::REQUEST_ID_HEADER;
use shelfy_server::telemetry::json_layer;
use shelfy_server::{app, routes};
use support::{TestState, send};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use utoipa_axum::router::OpenApiRouter;

/// Collects everything the log layer writes.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

async fn fail(_body: String) -> Result<(), ApiError> {
    Err(ApiError::internal(anyhow::anyhow!("disk exploded")))
}

fn lines(capture: &Capture) -> Vec<Value> {
    let bytes = capture.0.lock().unwrap().clone();
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("every log line is JSON"))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn request_logs_carry_the_route_but_no_request_data() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(json_layer(capture.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);

    let t = TestState::new();
    let failing = OpenApiRouter::new().route("/test/fail", post(fail));
    let app = app::build(
        t.state.clone(),
        routes::router().merge(RouteLimits::STANDARD.apply(failing)),
    );

    let planted = [
        "owner@example.test",
        "query-secret-1",
        "header-secret-2",
        "cookie-secret-3",
        "body-secret-4",
    ];
    let request = Request::get("/api/v1/nope/owner@example.test?token=query-secret-1")
        .header(header::AUTHORIZATION, "Bearer shx_header-secret-2")
        .header(header::COOKIE, "__Host-shelfy_session=cookie-secret-3")
        .body(Body::empty())
        .unwrap();
    let not_found = send(&app, request).await;
    assert_eq!(not_found.status(), StatusCode::NOT_FOUND);
    let not_found_id = not_found.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();

    let request = Request::post("/test/fail?q=query-secret-1")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("body-secret-4"))
        .unwrap();
    let failed = send(&app, request).await;
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let failed_id = failed.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();

    let logs = lines(&capture);
    let text = serde_json::to_string(&logs).unwrap();
    for secret in planted {
        assert!(
            !text.contains(secret),
            "{secret} leaked into the logs:\n{text}"
        );
    }

    let response_line = |id: &str| {
        logs.iter()
            .find(|line| line["span"]["request_id"] == id && line["status"].is_number())
            .unwrap_or_else(|| panic!("no response line for {id} in {text}"))
            .clone()
    };
    let line = response_line(&not_found_id);
    assert_eq!(line["level"], "INFO");
    assert_eq!(line["message"], "request");
    assert_eq!(line["status"], 404);
    assert!(line["latency_ms"].is_number());
    assert!(line["timestamp"].is_string());
    assert_eq!(line["span"]["method"], "GET");
    assert_eq!(line["span"]["route"], "unmatched");

    let line = response_line(&failed_id);
    assert_eq!(line["level"], "ERROR");
    assert_eq!(line["status"], 500);
    assert_eq!(line["span"]["route"], "/test/fail");

    // The cause of a 5xx is logged inside the request span, for the operator.
    let cause = logs
        .iter()
        .find(|line| line["message"] == "internal error")
        .expect("the cause of the 500 is logged");
    assert_eq!(cause["error"], "disk exploded");
    assert_eq!(cause["code"], "internal");
    assert_eq!(cause["span"]["request_id"], failed_id.as_str());
}
