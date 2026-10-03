//! What a call gets back. Answers carry token usage when the provider reports
//! it, and the call's timings; the crate records no metric (P3-09 does).

use std::fmt;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

/// Token usage, summed over every exchange of a call that reported it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    /// Prompt tokens, cached ones included.
    pub input_tokens: u64,
    /// Answer tokens, reasoning included.
    pub output_tokens: u64,
    /// Prompt tokens served from the provider's cache, when it says.
    pub cached_input_tokens: u64,
}

impl Usage {
    /// Adds `other`, saturating: the counts come from the wire.
    pub(crate) fn add(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
    }
}

/// Timings the server measured itself (llama.cpp's `timings`), from the last
/// exchange that sent them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct ServerTimings {
    /// Prompt tokens evaluated (cached ones excluded).
    pub prompt_tokens: Option<u64>,
    /// Milliseconds spent on the prompt.
    pub prompt_ms: Option<f64>,
    /// Tokens generated.
    pub predicted_tokens: Option<u64>,
    /// Milliseconds spent generating.
    pub predicted_ms: Option<f64>,
    /// Prompt tokens reused from the server's cache.
    pub cached_tokens: Option<u64>,
}

/// How long a call took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Timings {
    /// From the call to its answer, retries included.
    pub total: Duration,
    /// From the call to the first streamed token, when it streamed.
    pub first_token: Option<Duration>,
}

/// Why the model stopped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The answer is complete.
    Stop,
    /// The token cap cut the answer.
    Length,
    /// The model called a tool (Anthropic's forced tool).
    ToolUse,
    /// Any other reason the provider gave.
    Other(String),
    /// The provider gave none.
    Unknown,
}

/// A chat answer.
#[derive(Clone, PartialEq, Serialize)]
pub struct ChatResponse {
    /// The answer's text; for JSON answers, the JSON text.
    pub text: String,
    /// For JSON answers, the value, validated against the schema.
    pub json: Option<Value>,
    /// Why the model stopped.
    pub finish: FinishReason,
    /// The model the provider says answered.
    pub model: Option<String>,
    /// Tokens, when the provider reported them.
    pub usage: Option<Usage>,
    /// The server's own timings (llama.cpp), when it sent them.
    pub server_timings: Option<ServerTimings>,
    /// The call's timings.
    pub timings: Timings,
    /// HTTP requests the call sent: retries, the non-streaming retry and the
    /// repair call included.
    pub requests: u32,
    /// Whether a broken or empty stream was answered by a non-streaming retry.
    pub stream_fallback: bool,
    /// Whether the answer came from the repair call.
    pub repaired: bool,
}

impl fmt::Debug for ChatResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The answer is derived from library content: its size only.
        f.debug_struct("ChatResponse")
            .field("text_chars", &self.text.chars().count())
            .field("json", &self.json.is_some())
            .field("finish", &self.finish)
            .field("model", &self.model)
            .field("usage", &self.usage)
            .field("server_timings", &self.server_timings)
            .field("timings", &self.timings)
            .field("requests", &self.requests)
            .field("stream_fallback", &self.stream_fallback)
            .field("repaired", &self.repaired)
            .finish()
    }
}

/// Embedding vectors, one per input, in input order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Embeddings {
    /// The vectors.
    pub vectors: Vec<Vec<f32>>,
    /// The model the provider says answered.
    pub model: Option<String>,
    /// Tokens, when reported.
    pub usage: Option<Usage>,
    /// HTTP requests sent, retries included.
    pub requests: u32,
}

/// A transcription.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Transcript {
    /// The text, trimmed.
    pub text: String,
    /// HTTP requests sent, retries included.
    pub requests: u32,
    /// The call's duration.
    pub total: Duration,
}

impl fmt::Debug for Transcript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transcript")
            .field("text_chars", &self.text.chars().count())
            .field("requests", &self.requests)
            .field("total", &self.total)
            .finish()
    }
}

/// A model a provider serves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModelInfo {
    /// The id to send as `model`.
    pub id: String,
    /// A display name, when the provider has one (Anthropic).
    pub display_name: Option<String>,
}
