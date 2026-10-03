//! Shelfy's AI provider layer (plan §2.15): one [`Provider`] type over three
//! protocols, OpenAI-compatible servers, the Anthropic Messages API and
//! whisper.cpp, with structured output, streaming, retries and the egress
//! guard.
//!
//! | Module | Contents |
//! |---|---|
//! | [`provider`] | [`Provider`], [`ProviderConfig`]; chat, embeddings, transcription, models, health |
//! | [`request`], [`response`] | what a call sends and gets back |
//! | [`options`] | timeouts, retries, cancellation, the streaming callback |
//! | [`structured`] | how JSON is asked for and checked |
//! | [`error`] | the error kinds callers act on |
//! | [`guard`] | which endpoints a call may reach (plan §7.1) |
//! | [`transport`] | the HTTP seam the server plugs its outbound client into (L11) |
//! | [`presets`] | the cloud providers a user can pick |
//! | [`sse`] | the parser of streamed answers |
//! | `direct` (feature `direct`) | a plain-HTTP transport to loopback and allowlisted endpoints, for tests and harnesses |
//! | `stub` (feature `stub`) | the test provider, also the `shelfy-ai-stub` binary |
//!
//! The crate depends on no other workspace crate, so the desktop can reuse it
//! (P6). It builds no HTTP client for the server, records no metric and holds
//! no state between calls: the AI service (P3-09) owns limits, breakers,
//! usage and metrics. Keys live in [`secrecy::SecretString`] and never appear
//! in a `Debug` or `Display` output, a request log or an error.

pub mod anthropic;
pub mod error;
pub mod guard;
mod multipart;
mod openai;
pub mod options;
pub mod presets;
pub mod provider;
pub mod request;
pub mod response;
mod retry;
pub mod sse;
pub mod structured;
pub mod transport;
mod turn;
mod whisper;
mod wire;

#[cfg(feature = "direct")]
pub mod direct;
#[cfg(feature = "stub")]
pub mod stub;

/// The key types, re-exported so that callers build keys with the same
/// version.
pub use secrecy;

pub use error::{AiError, ErrorKind};
pub use guard::{Egress, EgressPolicy, GuardError, Origin};
pub use options::{CallOptions, RetryPolicy, TextCallback, Timeouts};
pub use provider::{MaxTokensField, Provider, ProviderConfig, ProviderKind, Source};
pub use request::{
    ChatRequest, EmbedRequest, Image, ImageType, JsonOutput, Message, Output, Part,
    ReasoningEffort, Role, TranscribeRequest,
};
pub use response::{
    ChatResponse, Embeddings, FinishReason, ModelInfo, ServerTimings, Timings, Transcript, Usage,
};
pub use structured::StructuredMode;
pub use transport::{HttpRequest, HttpResponse, Transport, TransportError};
