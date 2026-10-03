//! One answer of one exchange, as every protocol decodes it.

use std::time::Duration;

use serde_json::Value;

use crate::error::AiError;
use crate::response::{FinishReason, ServerTimings, Usage};
use crate::sse::SseEvent;

/// One exchange's answer.
#[derive(Debug, Default)]
pub(crate) struct Turn {
    /// The answer: text, or the forced tool's input as JSON text.
    pub(crate) text: String,
    pub(crate) finish: Option<FinishReason>,
    /// Set when the model or the provider's filter refused to answer.
    pub(crate) refusal: Option<String>,
    pub(crate) usage: Option<Usage>,
    pub(crate) server_timings: Option<ServerTimings>,
    pub(crate) model: Option<String>,
    pub(crate) first_token: Option<Duration>,
}

/// What one stream event did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delta {
    /// Nothing visible (metadata, keep-alives).
    None,
    /// A token that is not part of the answer (reasoning): the provider is
    /// alive.
    Token,
    /// The answer grew.
    Output,
}

/// Decodes a streamed answer, event by event.
pub(crate) trait StreamDecoder: Send {
    /// Reads one event.
    fn event(&mut self, event: &crate::sse::SseEvent) -> Result<Delta, AiError>;
    /// The answer so far.
    fn output(&self) -> &str;
    /// Whether the end marker arrived (`[DONE]`, `message_stop`).
    fn is_done(&self) -> bool;
    /// The answer, or an error when the stream ended before it finished.
    fn finish(self: Box<Self>) -> Result<Turn, AiError>;
}

/// A JSON value's text, or the texts of an array of `{"type": "text"}`
/// parts (some OpenAI-compatible servers answer with parts).
pub(crate) fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect();
            (!texts.is_empty()).then(|| texts.concat())
        }
        _ => None,
    }
}

/// A non-negative integer field.
pub(crate) fn count(value: &Value, name: &str) -> Option<u64> {
    value.get(name).and_then(Value::as_u64)
}

/// An event's data as JSON; a chunk that is not JSON broke the stream.
pub(crate) fn event_json(event: &SseEvent) -> Result<Value, AiError> {
    serde_json::from_str(event.data.trim()).map_err(|_| {
        AiError::transient("a stream event is not JSON")
            .with_hint(crate::error::RetryHint::StreamBroken)
    })
}
