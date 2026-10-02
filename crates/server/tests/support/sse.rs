//! Reading `GET /api/v1/events` in tests: open a stream through the router
//! and take its frames one by one, and check payloads against the OpenAPI
//! document.

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use shelfy_server::routes;

use super::send;

/// One frame of an event stream: an event, or a comment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// `event:`.
    pub event: Option<String>,
    /// `id:`.
    pub id: Option<String>,
    /// `data:` (lines joined with `\n`).
    pub data: Option<String>,
    /// A comment line, without the colon.
    pub comment: Option<String>,
}

impl Frame {
    /// Parses one frame as the server writes it (one field per line, a blank
    /// line at the end).
    pub fn parse(text: &str) -> Self {
        let body = text
            .strip_suffix("\n\n")
            .unwrap_or_else(|| panic!("a frame ends with a blank line: {text:?}"));
        let mut frame = Self::default();
        for line in body.split('\n') {
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value).to_owned();
            match field {
                "" => frame.comment = Some(value),
                "event" => frame.event = Some(value),
                "id" => frame.id = Some(value),
                "data" => {
                    frame.data = Some(match frame.data.take() {
                        Some(data) => format!("{data}\n{value}"),
                        None => value,
                    });
                }
                other => panic!("unexpected field {other:?} in {text:?}"),
            }
        }
        frame
    }

    /// The event name; panics on a comment.
    pub fn name(&self) -> &str {
        self.event
            .as_deref()
            .unwrap_or_else(|| panic!("not an event: {self:?}"))
    }

    /// The data as JSON.
    pub fn json(&self) -> Value {
        let data = self
            .data
            .as_deref()
            .unwrap_or_else(|| panic!("no data: {self:?}"));
        serde_json::from_str(data).expect("the data is one JSON document")
    }

    /// Whether this is the heartbeat comment.
    pub fn is_heartbeat(&self) -> bool {
        self.comment.as_deref() == Some("heartbeat") && self.event.is_none()
    }
}

/// An open event stream.
pub struct Stream {
    pub status: StatusCode,
    pub headers: HeaderMap,
    body: Body,
}

impl Stream {
    /// Opens `uri` on `app` with extra request headers; the status may be an
    /// error (then the body is a problem).
    pub async fn open(app: &Router, uri: &str, headers: &[(&str, &str)]) -> Self {
        let mut request = Request::get(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = send(app, request.body(Body::empty()).unwrap()).await;
        let (parts, body) = response.into_parts();
        Self {
            status: parts.status,
            headers: parts.headers,
            body,
        }
    }

    /// Opens `uri` and checks it streams.
    pub async fn connect(app: &Router, uri: &str, headers: &[(&str, &str)]) -> Self {
        let stream = Self::open(app, uri, headers).await;
        assert_eq!(stream.status, StatusCode::OK, "GET {uri}");
        stream
    }

    /// The next frame; panics when the stream ended.
    pub async fn next(&mut self) -> Frame {
        self.next_or_end().await.expect("the stream is still open")
    }

    /// The next frame, or `None` when the stream ended.
    pub async fn next_or_end(&mut self) -> Option<Frame> {
        let frame = self.body.frame().await?.expect("the stream does not fail");
        let data: Bytes = frame.into_data().expect("a data frame");
        Some(Frame::parse(std::str::from_utf8(&data).expect("UTF-8")))
    }

    /// The next frame, which must be the event `name`.
    pub async fn expect(&mut self, name: &str) -> Frame {
        let frame = self.next().await;
        assert_eq!(frame.event.as_deref(), Some(name), "{frame:?}");
        frame
    }

    /// The `hello` that opens every stream.
    pub async fn hello(&mut self) -> Frame {
        self.expect("hello").await
    }

    /// Whether the stream ended.
    pub async fn ended(&mut self) -> bool {
        self.body.frame().await.is_none()
    }
}

/// Checks `value` against the schema `name` of the OpenAPI document.
pub fn assert_schema(value: &Value, name: &str) {
    let doc = serde_json::to_value(routes::openapi()).unwrap();
    let schema = json!({
        "$ref": format!("#/components/schemas/{name}"),
        "components": doc["components"],
    });
    let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{name}: {errors:#?}");
}

/// Checks that a frame is a well-formed event of the stream: its name and
/// data match the `ServerEvent` schema.
pub fn assert_event_schema(frame: &Frame) {
    let event = json!({ "event": frame.name(), "data": frame.json() });
    assert_schema(&event, "ServerEvent");
}
