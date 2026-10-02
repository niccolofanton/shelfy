//! What `POST /api/v1/client-errors` logs: one `warn` line in the request
//! span with the report's technical fields, free text clipped, and nothing of
//! a refused report (plan §3.7). A binary of its own, like `logging.rs`: a
//! scoped log subscriber can miss spans that tests on other threads register
//! at the same time.

mod support;

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use shelfy_server::auth::csrf::{CLIENT_HEADER, CLIENT_WEB};
use shelfy_server::error::ErrorCode;
use shelfy_server::telemetry::json_layer;
use support::library::ALICE;
use support::{TestState, post_json, problem, send};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;

const CLIENT_ERRORS: &str = "/api/v1/client-errors";

/// A JSON `POST` with the headers the web app sends for the CSRF guard.
fn post(t: &TestState, uri: &str, body: &Value) -> Request<Body> {
    let mut request = post_json(uri, body.to_string());
    let headers = request.headers_mut();
    headers.insert(
        header::ORIGIN,
        t.state.config().public_url.as_str().parse().unwrap(),
    );
    headers.insert("sec-fetch-site", "same-origin".parse().unwrap());
    headers.insert(CLIENT_HEADER, CLIENT_WEB.parse().unwrap());
    request
}

fn report() -> Value {
    json!({
        "view": "postModal",
        "name": "TypeError",
        "message": "Cannot read properties of undefined (reading 'slides')",
        "stack": "TypeError: Cannot read properties of undefined\n    at Modal (/assets/index-3f2a.js:1:2345)",
        "componentStack": "\n    at PostModal\n    at App",
        "route": "/p/:key",
        "clientVersion": "0.1.0",
        "occurredAt": 1_790_899_200_000_i64,
    })
}

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

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    fn client_errors(&self) -> Vec<Value> {
        self.text()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("every log line is JSON"))
            .filter(|line| line["message"] == "client error")
            .collect()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn client_errors_are_logged_with_technical_fields_only() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(json_layer(capture.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);
    let t = TestState::new();
    let app = t.app_as(ALICE);

    let response = send(&app, post(&t, CLIENT_ERRORS, &report())).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let logged = capture.client_errors();
    assert_eq!(logged.len(), 1);
    let line = &logged[0];
    assert_eq!(line["level"], "WARN");
    assert_eq!(line["view"], "postModal");
    assert_eq!(line["error_name"], "TypeError");
    assert_eq!(
        line["error_message"],
        "Cannot read properties of undefined (reading 'slides')"
    );
    assert_eq!(line["client_route"], "/p/:key");
    assert_eq!(line["client_version"], "0.1.0");
    assert_eq!(line["occurred_at"], 1_790_899_200_000_i64);
    assert!(line["stack"].as_str().unwrap().contains("index-3f2a.js"));
    assert_eq!(line["span"]["route"], CLIENT_ERRORS, "in the request span");

    // A report that carries post content is refused before anything is logged.
    let mut leaky = report();
    leaky["caption"] = json!("planted-caption-secret");
    let problem = problem(
        send(&app, post(&t, CLIENT_ERRORS, &leaky)).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(problem.code, ErrorCode::ValidationFailed);
    assert!(!capture.text().contains("planted-caption-secret"));
    assert_eq!(capture.client_errors().len(), 1);

    // Long free text is clipped, not refused.
    let mut long = report();
    long["message"] = json!("m".repeat(5_000));
    long["stack"] = json!("s".repeat(20_000));
    let response = send(&app, post(&t, CLIENT_ERRORS, &long)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let line = capture.client_errors().pop().unwrap();
    assert_eq!(line["error_message"].as_str().unwrap().len(), 1_000);
    assert_eq!(line["stack"].as_str().unwrap().len(), 8_000);
}
