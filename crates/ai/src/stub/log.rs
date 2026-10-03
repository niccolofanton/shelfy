//! The request log: what each request carried, for assertions. It never holds
//! a key: auth headers are reduced to which one was sent and whether it
//! matched, and only allowlisted headers are kept.

use std::collections::BTreeMap;

use http::HeaderMap;
use serde::Serialize;
use serde_json::Value;

use super::fault::Endpoint;

/// The headers the log keeps.
const KEPT_HEADERS: [&str; 6] = [
    "accept",
    "anthropic-beta",
    "anthropic-version",
    "content-type",
    "host",
    "user-agent",
];

/// Which credential a request sent, and whether it matched the stub's key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSeen {
    /// No credential.
    None,
    /// The configured key, as `Authorization: Bearer` or `x-api-key`.
    Valid,
    /// Some other credential.
    Invalid,
}

/// A file a multipart request carried: its metadata, never its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileSeen {
    /// The part's field name.
    pub field: String,
    /// The file name.
    pub filename: Option<String>,
    /// The part's content type.
    pub content_type: Option<String>,
    /// Its size.
    pub bytes: usize,
    /// Its SHA-256, lowercase hex.
    pub sha256: String,
}

/// One request the stub received.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LoggedRequest {
    /// The endpoint.
    pub endpoint: Endpoint,
    /// `GET` or `POST`.
    pub method: String,
    /// The path, without the query.
    pub path: String,
    /// The query, if any.
    pub query: Option<String>,
    /// The credential seen.
    pub auth: AuthSeen,
    /// The allowlisted headers.
    pub headers: BTreeMap<String, String>,
    /// A JSON body.
    pub body: Option<Value>,
    /// A multipart body's text fields.
    pub form: Option<BTreeMap<String, String>>,
    /// A multipart body's file.
    pub file: Option<FileSeen>,
    /// The request key: the hash of the request's text, or of the file.
    pub key: Option<String>,
    /// The fault applied, by name.
    pub fault: Option<String>,
}

impl LoggedRequest {
    pub(crate) fn new(
        endpoint: Endpoint,
        method: &str,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
        auth: AuthSeen,
    ) -> Self {
        let headers = KEPT_HEADERS
            .iter()
            .filter_map(|name| {
                let value = headers.get(*name)?.to_str().ok()?;
                Some(((*name).to_owned(), value.to_owned()))
            })
            .collect();
        Self {
            endpoint,
            method: method.to_owned(),
            path: path.to_owned(),
            query: query.map(str::to_owned),
            auth,
            headers,
            body: None,
            form: None,
            file: None,
            key: None,
            fault: None,
        }
    }

    /// The body's `model`, if any.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.body.as_ref()?.get("model")?.as_str()
    }

    /// Whether the body asked for a stream.
    #[must_use]
    pub fn streamed(&self) -> bool {
        self.body
            .as_ref()
            .and_then(|body| body.get("stream"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// The images the messages carried (`image_url` parts or `image` blocks).
    #[must_use]
    pub fn images(&self) -> usize {
        let Some(messages) = self
            .body
            .as_ref()
            .and_then(|body| body.get("messages"))
            .and_then(Value::as_array)
        else {
            return 0;
        };
        messages
            .iter()
            .filter_map(|message| message.get("content")?.as_array())
            .flatten()
            .filter(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("image_url" | "image")
                )
            })
            .count()
    }
}
