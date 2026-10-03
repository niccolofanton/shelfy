//! The entry point: a [`Provider`] makes the calls of one configured
//! provider (plan §2.15).
//!
//! ```no_run
//! # async fn example(transport: std::sync::Arc<dyn shelfy_ai::Transport>) -> Result<(), shelfy_ai::AiError> {
//! use secrecy::SecretString;
//! use shelfy_ai::guard::{EgressPolicy, Origin};
//! use shelfy_ai::{
//!     CallOptions, ChatRequest, Image, ImageType, JsonOutput, Message, Part, Provider,
//!     ProviderConfig, ProviderKind, Source, Timeouts,
//! };
//!
//! // The operator provider (P3-09): its origin is the one allowlisted endpoint.
//! // A llama.cpp server decodes no WebP, so it gets JPEG.
//! let base = url::Url::parse("http://100.94.10.20:8080/v1").unwrap();
//! let policy = EgressPolicy::new().allow(Origin::of(&base).unwrap());
//! let config = ProviderConfig::new(ProviderKind::OpenAiCompatible, Source::Operator, base)
//!     .with_key(SecretString::from("…from SHELFY_OPERATOR_AI_KEY…"))
//!     .with_llama_health()
//!     .without_webp();
//! let provider = Provider::new(config, &policy, transport)?;
//!
//! let schema = serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false});
//! let cover = Image { media_type: ImageType::Jpeg, data: vec![/* g480 as JPEG */].into() };
//! let request = ChatRequest::new(
//!     "qwen3.8-27b",
//!     vec![Message::user(vec![Part::text("…the post…"), Part::Image(cover)])],
//! )
//! .with_system("…catalog.system.md…")
//! .with_json(JsonOutput::new("catalog", schema)?)
//! .with_temperature(0.2)
//! .with_max_tokens(768)
//! // Qwen thinks by default; without it the answer is about 5 times faster.
//! .with_extra("chat_template_kwargs", serde_json::json!({"enable_thinking": false}))
//! .streamed(true);
//! let options = CallOptions::new(Timeouts::CATALOG).with_text_callback(|text| {
//!     let _ = text; // forward to `ai.stream`
//! });
//! let answer = provider.chat(&request, &options).await?;
//! let _catalog = answer.json; // validated against the schema
//! # Ok(())
//! # }
//! ```

use std::borrow::Cow;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderValue, Method};
use secrecy::SecretString;
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::{AiError, ErrorKind, RetryHint};
use crate::guard::{Egress, EgressPolicy};
use crate::options::CallOptions;
use crate::request::{
    ChatRequest, EmbedRequest, ImageType, Message, Output, Part, Role, TranscribeRequest,
};
use crate::response::{
    ChatResponse, Embeddings, FinishReason, ModelInfo, ServerTimings, Timings, Transcript, Usage,
};
use crate::retry::retrying;
use crate::sse::SseParser;
use crate::structured::{self, StructuredMode};
use crate::transport::{HttpRequest, Transport};
use crate::turn::{Delta, StreamDecoder, Turn};
use crate::wire::{self, Deadline, MAX_ANSWER_BYTES, Opened, endpoint, from_transport};
use crate::{anthropic, openai, whisper};

/// The protocol a provider speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// `{base}/chat/completions` and friends: OpenAI, Gemini, OpenRouter,
    /// Groq, Mistral, Together, llama.cpp, custom servers.
    OpenAiCompatible,
    /// `{base}/v1/messages`.
    Anthropic,
    /// A whisper.cpp server's `/inference` endpoint (the base URL is the
    /// endpoint itself): transcription only.
    WhisperCpp,
}

/// Who configured a provider, which decides how its URLs are judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The server, from its environment: allowlisted origins pass
    /// ([`EgressPolicy::classify`]).
    Operator,
    /// A user: https, public, never an operator host
    /// ([`EgressPolicy::check_user_url`]).
    User,
}

/// The field that carries the token cap on OpenAI-compatible servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum MaxTokensField {
    /// `max_tokens`: most servers, llama.cpp included.
    #[serde(rename = "max_tokens")]
    MaxTokens,
    /// `max_completion_tokens`: OpenAI's current models.
    #[serde(rename = "max_completion_tokens")]
    MaxCompletionTokens,
}

impl MaxTokensField {
    /// The field's name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaxTokens => "max_tokens",
            Self::MaxCompletionTokens => "max_completion_tokens",
        }
    }
}

/// One provider's settings.
#[derive(Clone)]
pub struct ProviderConfig {
    /// The protocol.
    pub kind: ProviderKind,
    /// Who configured it.
    pub source: Source,
    /// The base URL, used verbatim plus each call's suffix; for whisper.cpp,
    /// the `/inference` URL itself.
    pub base_url: Url,
    /// The key, if the provider takes one.
    pub key: Option<SecretString>,
    /// How JSON answers are asked for, unless a call overrides it.
    pub structured: StructuredMode,
    /// The token cap's field (OpenAI-compatible only).
    pub max_tokens_field: MaxTokensField,
    /// Whether streamed calls ask for a last chunk with the usage
    /// (`stream_options.include_usage`, OpenAI-compatible only).
    pub stream_usage: bool,
    /// Whether the provider decodes WebP images. llama.cpp servers, the
    /// owner's node among them, decode images with stb_image, which has no
    /// WebP: they answer 400 "Failed to load image". With `false`, a call that
    /// carries a WebP image fails with [`ErrorKind::Unsupported`] before
    /// sending, and the caller sends JPEG or PNG instead.
    pub webp_images: bool,
    /// Whether to send a request's `temperature`. Off for providers whose
    /// current models refuse it: Anthropic's newest models answer 400 to any
    /// sampling field.
    pub send_temperature: bool,
    /// The optional health probe, `GET` without generation (llama.cpp's
    /// `/health`).
    pub health_url: Option<Url>,
    /// Extra top-level fields of every chat body (OpenAI-compatible only),
    /// such as llama.cpp's `chat_template_kwargs`. They never replace a field
    /// the adapter sets, and a request's own extras win over them.
    pub extra_body: Option<Map<String, Value>>,
}

impl ProviderConfig {
    /// `kind` at `base_url`, without a key: JSON through a strict schema,
    /// `max_tokens`, WebP images, and the usage chunk on OpenAI-compatible
    /// streams.
    #[must_use]
    pub fn new(kind: ProviderKind, source: Source, base_url: Url) -> Self {
        Self {
            kind,
            source,
            base_url,
            key: None,
            structured: StructuredMode::JsonSchema,
            max_tokens_field: MaxTokensField::MaxTokens,
            stream_usage: kind == ProviderKind::OpenAiCompatible,
            webp_images: true,
            send_temperature: true,
            health_url: None,
            extra_body: None,
        }
    }

    /// Without WebP images (llama.cpp servers).
    #[must_use]
    pub fn without_webp(mut self) -> Self {
        self.webp_images = false;
        self
    }

    /// With `key`.
    #[must_use]
    pub fn with_key(mut self, key: SecretString) -> Self {
        self.key = Some(key);
        self
    }

    /// With the structured-output mode `mode`.
    #[must_use]
    pub fn with_structured(mut self, mode: StructuredMode) -> Self {
        self.structured = mode;
        self
    }

    /// With the health probe at `url`.
    #[must_use]
    pub fn with_health_url(mut self, url: Url) -> Self {
        self.health_url = Some(url);
        self
    }

    /// With llama.cpp's probe: `/health` at the base URL's origin.
    #[must_use]
    pub fn with_llama_health(mut self) -> Self {
        self.health_url = self.base_url.join("/health").ok();
        self
    }

    /// With an extra top-level field of every chat body.
    #[must_use]
    pub fn with_extra(mut self, name: impl Into<String>, value: Value) -> Self {
        self.extra_body
            .get_or_insert_with(Map::new)
            .insert(name.into(), value);
        self
    }
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("kind", &self.kind)
            .field("source", &self.source)
            .field("base_url", &display_url(&self.base_url))
            .field("key", &self.key.as_ref().map(|_| "[redacted]"))
            .field("structured", &self.structured)
            .field("max_tokens_field", &self.max_tokens_field)
            .field("stream_usage", &self.stream_usage)
            .field("webp_images", &self.webp_images)
            .field("send_temperature", &self.send_temperature)
            .field("health_url", &self.health_url.as_ref().map(display_url))
            .field(
                "extra_body",
                &self
                    .extra_body
                    .as_ref()
                    .map(|extra| extra.keys().collect::<Vec<_>>()),
            )
            .finish()
    }
}

/// `url` without credentials, query or fragment.
fn display_url(url: &Url) -> String {
    let mut shown = url.clone();
    let _ = shown.set_username("");
    let _ = shown.set_password(None);
    shown.set_query(None);
    shown.set_fragment(None);
    shown.to_string()
}

/// A configured provider, ready to call. Cheap to clone.
#[derive(Clone)]
pub struct Provider {
    inner: Arc<Inner>,
}

struct Inner {
    config: ProviderConfig,
    egress: Egress,
    health_egress: Option<Egress>,
    transport: Arc<dyn Transport>,
}

impl fmt::Debug for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Provider")
            .field("config", &self.inner.config)
            .field("egress", &self.inner.egress)
            .finish_non_exhaustive()
    }
}

/// What a call sums over its exchanges.
#[derive(Default)]
struct Tally {
    requests: u32,
    usage: Option<Usage>,
    server_timings: Option<ServerTimings>,
    first_token: Option<Duration>,
    stream_fallback: bool,
    repaired: bool,
}

impl Tally {
    fn absorb(&mut self, turn: &Turn) {
        if let Some(usage) = turn.usage {
            self.usage.get_or_insert_with(Usage::default).add(usage);
        }
        if turn.server_timings.is_some() {
            self.server_timings = turn.server_timings;
        }
        if self.first_token.is_none() {
            self.first_token = turn.first_token;
        }
    }
}

/// One call: its options, when it started, and its own deadline.
struct Call<'a> {
    options: &'a CallOptions,
    started: Instant,
    deadline: Option<Deadline>,
}

impl<'a> Call<'a> {
    fn new(options: &'a CallOptions) -> Self {
        let started = Instant::now();
        Self {
            options,
            started,
            deadline: options
                .timeouts
                .overall
                .map(|overall| Deadline::overall(started, overall)),
        }
    }

    fn cancel(&self) -> Option<&CancellationToken> {
        self.options.cancel.as_ref()
    }

    /// Runs `attempt` with the call's retries.
    async fn retrying<T, F, Fut>(&self, attempt: F) -> (Result<T, AiError>, u32)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, AiError>>,
    {
        let deadline = self.deadline.as_ref().map(|deadline| deadline.at);
        retrying(&self.options.retry, self.cancel(), deadline, attempt).await
    }
}

impl Provider {
    /// The provider of `config`, calling through `transport`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Refused`] when `policy` refuses the base URL or the health
    /// URL.
    pub fn new(
        config: ProviderConfig,
        policy: &EgressPolicy,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, AiError> {
        let egress = route(policy, config.source, &config.base_url)?;
        let health_egress = config
            .health_url
            .as_ref()
            .map(|url| route(policy, config.source, url))
            .transpose()?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                egress,
                health_egress,
                transport,
            }),
        })
    }

    /// The settings.
    #[must_use]
    pub fn config(&self) -> &ProviderConfig {
        &self.inner.config
    }

    /// The protocol.
    #[must_use]
    pub fn kind(&self) -> ProviderKind {
        self.inner.config.kind
    }

    /// How the guard routes the base URL.
    #[must_use]
    pub fn egress(&self) -> Egress {
        self.inner.egress
    }

    fn key(&self) -> Option<&SecretString> {
        self.inner.config.key.as_ref()
    }

    /// A chat completion: text, or JSON validated against the request's
    /// schema (one repair call when it does not match), streamed or not.
    ///
    /// A streamed answer that comes back empty, unreadable or cut off gets one
    /// non-streaming retry (desktop AI-13). Transient and rate-limited errors
    /// are retried per [`CallOptions::retry`].
    ///
    /// # Errors
    ///
    /// An [`AiError`] of any kind; [`ErrorKind::Unsupported`] on whisper.cpp,
    /// for [`StructuredMode::Tool`] on an OpenAI-compatible server, or for a
    /// WebP image when [`ProviderConfig::webp_images`] is off.
    pub async fn chat(
        &self,
        request: &ChatRequest,
        options: &CallOptions,
    ) -> Result<ChatResponse, AiError> {
        let call = Call::new(options);
        if self.kind() == ProviderKind::WhisperCpp {
            return Err(AiError::unsupported(
                "a whisper.cpp server only transcribes",
            ));
        }
        if !self.inner.config.webp_images && has_webp(request) {
            return Err(AiError::unsupported(
                "this provider does not decode WebP images: send JPEG or PNG",
            ));
        }
        let mode = self.mode(request, options)?;
        let mut tally = Tally::default();
        let first = self.first_answer(request, mode, &call, &mut tally).await?;
        tally.absorb(&first);
        if first.refusal.is_some() {
            return Err(refused());
        }
        let Output::Json(output) = &request.output else {
            return Ok(respond(first, None, tally, &call, request));
        };
        let invalid = match structured::validate(output, &first.text) {
            Ok(value) => return Ok(respond(first, Some(value), tally, &call, request)),
            Err(invalid) => invalid,
        };
        if first.finish == Some(FinishReason::Length) {
            return Err(AiError::new(
                ErrorKind::SchemaInvalid,
                format!("the answer was cut at the token cap: {}", invalid.for_log),
            ));
        }
        tracing::debug!(reason = %invalid.for_log, "repairing a JSON answer");
        let prompt = structured::repair_prompt(&invalid);
        let (repair, extra) = if first.text.trim().is_empty() {
            // No answer to quote: the instruction joins the last user turn, so
            // that no template sees two user turns in a row.
            (
                Cow::Owned(with_repair_in_last_turn(request, prompt)),
                Vec::new(),
            )
        } else {
            (
                Cow::Borrowed(request),
                vec![
                    Message::assistant_text(first.text.clone()),
                    Message::user_text(prompt),
                ],
            )
        };
        let (result, tries) = call
            .retrying(|| self.chat_once(&repair, &extra, mode, false, &call))
            .await;
        tally.requests += tries;
        let repaired = result?;
        tally.absorb(&repaired);
        if repaired.refusal.is_some() {
            return Err(refused());
        }
        match structured::validate(output, &repaired.text) {
            Ok(value) => {
                tally.repaired = true;
                Ok(respond(repaired, Some(value), tally, &call, request))
            }
            Err(invalid) => Err(AiError::new(ErrorKind::SchemaInvalid, invalid.for_log)),
        }
    }

    /// The structured-output mode of `request`: none for text.
    fn mode(
        &self,
        request: &ChatRequest,
        options: &CallOptions,
    ) -> Result<Option<StructuredMode>, AiError> {
        if matches!(request.output, Output::Text) {
            return Ok(None);
        }
        let mode = options.structured.unwrap_or(self.inner.config.structured);
        if mode == StructuredMode::Tool && self.kind() != ProviderKind::Anthropic {
            return Err(AiError::unsupported(
                "forced tool use is an Anthropic mode: use json_schema or json_object",
            ));
        }
        Ok(Some(mode))
    }

    /// The first answer: streamed when asked, with one non-streaming retry
    /// when the stream breaks or brings nothing usable.
    async fn first_answer(
        &self,
        request: &ChatRequest,
        mode: Option<StructuredMode>,
        call: &Call<'_>,
        tally: &mut Tally,
    ) -> Result<Turn, AiError> {
        if request.stream {
            let (result, tries) = call
                .retrying(|| self.chat_once(request, &[], mode, true, call))
                .await;
            tally.requests += tries;
            match result {
                Ok(turn) if usable(&turn, request) => return Ok(turn),
                Ok(turn) => {
                    tracing::debug!("an empty or unreadable stream: retrying without streaming");
                    tally.absorb(&turn);
                }
                Err(error) if error.hint() == RetryHint::StreamBroken => {
                    tracing::debug!(reason = %error, "a broken stream: retrying without streaming");
                }
                Err(error) => return Err(error),
            }
            tally.stream_fallback = true;
        }
        let (result, tries) = call
            .retrying(|| self.chat_once(request, &[], mode, false, call))
            .await;
        tally.requests += tries;
        result
    }

    /// One chat exchange.
    async fn chat_once(
        &self,
        request: &ChatRequest,
        extra: &[Message],
        mode: Option<StructuredMode>,
        stream: bool,
        call: &Call<'_>,
    ) -> Result<Turn, AiError> {
        let config = &self.inner.config;
        let key = self.key();
        let (url, headers, body) = match config.kind {
            ProviderKind::OpenAiCompatible => (
                endpoint(&config.base_url, "/chat/completions")?,
                openai::headers(key, stream)?,
                openai::chat_body(config, request, extra, mode, stream),
            ),
            ProviderKind::Anthropic => (
                endpoint(&config.base_url, "/v1/messages")?,
                anthropic::headers(key, stream)?,
                anthropic::chat_body(config, request, extra, mode, stream),
            ),
            ProviderKind::WhisperCpp => {
                return Err(AiError::unsupported(
                    "a whisper.cpp server only transcribes",
                ));
            }
        };
        let opened = self
            .exchange(
                Method::POST,
                url,
                self.inner.egress,
                headers,
                body,
                call,
                stream,
            )
            .await?;
        if !stream {
            let body = opened.read_all(MAX_ANSWER_BYTES, call.cancel()).await?;
            return match config.kind {
                ProviderKind::Anthropic => anthropic::parse_chat(&body, key),
                _ => openai::parse_chat(&body, key),
            };
        }
        let decoder: Box<dyn StreamDecoder> = match config.kind {
            ProviderKind::Anthropic => Box::new(anthropic::ChatStream::new(key)),
            _ => Box::new(openai::ChatStream::new(key)),
        };
        read_stream(opened, decoder, call).await
    }

    /// Sends one request; returns once a 2xx answer's headers arrived, or the
    /// error of any other answer. A streamed call's head must also arrive
    /// before the first-token deadline: llama.cpp sends it with the first
    /// token.
    #[allow(clippy::too_many_arguments)]
    async fn exchange(
        &self,
        method: Method,
        url: Url,
        egress: Egress,
        headers: HeaderMap,
        body: Bytes,
        call: &Call<'_>,
        streamed: bool,
    ) -> Result<Opened, AiError> {
        let timeouts = &call.options.timeouts;
        let sent_at = Instant::now();
        let body_deadline = Deadline::total(sent_at, timeouts.total).min(call.deadline.clone());
        let head_deadline = match timeouts.first_token {
            Some(wait) if streamed => body_deadline
                .clone()
                .min(Some(Deadline::first_token(sent_at, wait))),
            _ => body_deadline.clone(),
        };
        let request = HttpRequest {
            method,
            url,
            headers,
            body,
            egress,
            connect_timeout: timeouts.connect,
            timeout: body_deadline.at.saturating_duration_since(sent_at),
        };
        let response = head_deadline
            .run(self.inner.transport.send(request), call.cancel())
            .await?
            .map_err(from_transport)?;
        let status = response.status;
        let opened = Opened::new(response.headers, response.body, sent_at, body_deadline);
        if !status.is_success() {
            return Err(opened.into_error(status, call.cancel(), self.key()).await);
        }
        Ok(opened)
    }

    /// Sends a body-less `GET` and reads the answer with `parse`, with
    /// retries (an unreadable answer is transient, so it is retried too).
    async fn get<T>(
        &self,
        url: Url,
        egress: Egress,
        options: &CallOptions,
        parse: impl Fn(&[u8]) -> Result<T, AiError>,
    ) -> Result<T, AiError> {
        let call = Call::new(options);
        let mut headers = match self.kind() {
            ProviderKind::Anthropic => anthropic::headers(self.key(), false)?,
            _ => openai::headers(self.key(), false)?,
        };
        headers.remove(CONTENT_TYPE);
        let (call, parse) = (&call, &parse);
        let (result, _) = call
            .retrying(|| {
                let url = url.clone();
                let headers = headers.clone();
                async move {
                    let opened = self
                        .exchange(Method::GET, url, egress, headers, Bytes::new(), call, false)
                        .await?;
                    parse(&opened.read_all(MAX_ANSWER_BYTES, call.cancel()).await?)
                }
            })
            .await;
        result
    }

    /// Embedding vectors for `request.input`, in order (OpenAI-compatible
    /// `/embeddings`).
    ///
    /// # Errors
    ///
    /// An [`AiError`]; [`ErrorKind::Unsupported`] on Anthropic and
    /// whisper.cpp.
    pub async fn embed(
        &self,
        request: &EmbedRequest,
        options: &CallOptions,
    ) -> Result<Embeddings, AiError> {
        let call = Call::new(options);
        if self.kind() != ProviderKind::OpenAiCompatible {
            return Err(AiError::unsupported(format!(
                "{} serves no embeddings",
                kind_name(self.kind())
            )));
        }
        if request.input.is_empty() {
            return Ok(Embeddings {
                vectors: Vec::new(),
                model: None,
                usage: None,
                requests: 0,
            });
        }
        let url = endpoint(&self.inner.config.base_url, "/embeddings")?;
        let headers = openai::headers(self.key(), false)?;
        let body = openai::embeddings_body(request);
        let call = &call;
        let (result, tries) = call
            .retrying(|| {
                let (url, headers, body) = (url.clone(), headers.clone(), body.clone());
                async move {
                    let opened = self
                        .exchange(
                            Method::POST,
                            url,
                            self.inner.egress,
                            headers,
                            body,
                            call,
                            false,
                        )
                        .await?;
                    let answer = opened.read_all(MAX_ANSWER_BYTES, call.cancel()).await?;
                    openai::parse_embeddings(&answer, request.input.len())
                }
            })
            .await;
        Ok(Embeddings {
            requests: tries,
            ..result?
        })
    }

    /// The text of one recording: whisper.cpp's `/inference`, or an
    /// OpenAI-compatible `/audio/transcriptions` (which needs
    /// [`TranscribeRequest::model`]).
    ///
    /// # Errors
    ///
    /// An [`AiError`]; [`ErrorKind::BadRequest`] for a file that is not a WAV;
    /// [`ErrorKind::Unsupported`] on Anthropic.
    pub async fn transcribe(
        &self,
        request: &TranscribeRequest,
        options: &CallOptions,
    ) -> Result<Transcript, AiError> {
        let call = Call::new(options);
        if !whisper::looks_like_wav(&request.wav) {
            return Err(AiError::bad_request("the recording is not a WAV file"));
        }
        let config = &self.inner.config;
        let (url, (content_type, body)) = match config.kind {
            ProviderKind::OpenAiCompatible => {
                let model = request
                    .model
                    .as_deref()
                    .filter(|model| !model.is_empty())
                    .ok_or_else(|| AiError::bad_request("a transcription model is needed"))?;
                (
                    endpoint(&config.base_url, "/audio/transcriptions")?,
                    openai::transcription_form(request, model),
                )
            }
            ProviderKind::WhisperCpp => (config.base_url.clone(), whisper::inference_form(request)),
            ProviderKind::Anthropic => {
                return Err(AiError::unsupported("Anthropic serves no transcription"));
            }
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(&content_type)
                .map_err(|_| AiError::bad_request("bad form type"))?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static(wire::AGENT));
        if let Some(key) = self.key() {
            wire::put_key(&mut headers, AUTHORIZATION, "Bearer ", key)?;
        }
        let call = &call;
        let (result, tries) = call
            .retrying(|| {
                let (url, headers, body) = (url.clone(), headers.clone(), body.clone());
                async move {
                    let opened = self
                        .exchange(
                            Method::POST,
                            url,
                            self.inner.egress,
                            headers,
                            body,
                            call,
                            false,
                        )
                        .await?;
                    let answer = opened.read_all(MAX_ANSWER_BYTES, call.cancel()).await?;
                    openai::parse_transcription(&answer, self.key())
                }
            })
            .await;
        Ok(Transcript {
            text: result?,
            requests: tries,
            total: call.started.elapsed(),
        })
    }

    /// The models the provider serves (`/models`, Anthropic's `/v1/models`).
    ///
    /// # Errors
    ///
    /// An [`AiError`]; [`ErrorKind::Unsupported`] on whisper.cpp.
    pub async fn models(&self, options: &CallOptions) -> Result<Vec<ModelInfo>, AiError> {
        let config = &self.inner.config;
        let url = match config.kind {
            ProviderKind::OpenAiCompatible => endpoint(&config.base_url, "/models")?,
            ProviderKind::Anthropic => endpoint(&config.base_url, "/v1/models?limit=1000")?,
            ProviderKind::WhisperCpp => {
                return Err(AiError::unsupported("a whisper.cpp server lists no models"));
            }
        };
        let parse = match config.kind {
            ProviderKind::Anthropic => anthropic::parse_models,
            _ => openai::parse_models,
        };
        self.get(url, self.inner.egress, options, parse).await
    }

    /// Probes the health URL (llama.cpp's `/health`): `Ok` on any 2xx. No
    /// generation runs.
    ///
    /// # Errors
    ///
    /// An [`AiError`] ([`ErrorKind::Offline`] when the server does not
    /// answer); [`ErrorKind::Unsupported`] without a health URL.
    pub async fn health(&self, options: &CallOptions) -> Result<(), AiError> {
        let (Some(url), Some(egress)) = (
            self.inner.config.health_url.clone(),
            self.inner.health_egress,
        ) else {
            return Err(AiError::unsupported("no health URL is configured"));
        };
        self.get(url, egress, options, |_| Ok(())).await
    }
}

/// The guard's route for `url`, as an AI error when refused.
fn route(policy: &EgressPolicy, source: Source, url: &Url) -> Result<Egress, AiError> {
    match source {
        Source::Operator => policy.classify(url),
        Source::User => policy.check_user_url(url),
    }
    .map_err(|error| {
        AiError::new(
            ErrorKind::Refused,
            format!("the endpoint was refused: {error}"),
        )
    })
}

fn kind_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::OpenAiCompatible => "this server",
        ProviderKind::Anthropic => "Anthropic",
        ProviderKind::WhisperCpp => "a whisper.cpp server",
    }
}

/// The error of a refused answer. The refusal's text stays out: it may quote
/// the content.
fn refused() -> AiError {
    AiError::new(
        ErrorKind::Refused,
        "the model or the provider's filter refused to answer",
    )
}

/// Whether `request` carries a WebP image.
fn has_webp(request: &ChatRequest) -> bool {
    request
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .any(|part| matches!(part, Part::Image(image) if image.media_type == ImageType::Webp))
}

/// `request` with `prompt` added to its last user turn, or as a new user turn
/// when the last one is the model's.
fn with_repair_in_last_turn(request: &ChatRequest, prompt: String) -> ChatRequest {
    let mut repaired = request.clone();
    match repaired.messages.last_mut() {
        Some(last) if last.role == Role::User => last.parts.push(Part::Text(prompt)),
        _ => repaired.messages.push(Message::user_text(prompt)),
    }
    repaired
}

/// Whether a streamed turn can stand: a refusal, or a non-empty answer that
/// parses as JSON when JSON was asked for (a cut answer goes on to
/// validation, which reports it).
fn usable(turn: &Turn, request: &ChatRequest) -> bool {
    if turn.refusal.is_some() {
        return true;
    }
    let text = turn.text.trim();
    if text.is_empty() {
        return false;
    }
    match request.output {
        Output::Text => true,
        Output::Json(_) => {
            turn.finish == Some(FinishReason::Length)
                || serde_json::from_str::<Value>(structured::strip_code_fence(text)).is_ok()
        }
    }
}

/// Reads a streamed answer, feeding the callback. The answer may not grow
/// past [`MAX_ANSWER_BYTES`].
async fn read_stream(
    mut opened: Opened,
    mut decoder: Box<dyn StreamDecoder>,
    call: &Call<'_>,
) -> Result<Turn, AiError> {
    let first_deadline = call
        .options
        .timeouts
        .first_token
        .map(|wait| Deadline::first_token(opened.sent_at, wait));
    let mut first_token = None;
    let mut parser = SseParser::new();
    'read: loop {
        let early = if first_token.is_none() {
            first_deadline.clone()
        } else {
            None
        };
        let chunk = opened.next_chunk(early, call.cancel()).await?;
        let ended = chunk.is_none();
        let events = match chunk {
            Some(chunk) => parser.push(&chunk).map_err(malformed)?,
            None => parser.finish().map_err(malformed)?.into_iter().collect(),
        };
        for event in &events {
            match decoder.event(event)? {
                Delta::None => {}
                Delta::Token => {
                    first_token.get_or_insert_with(|| call.started.elapsed());
                }
                Delta::Output => {
                    first_token.get_or_insert_with(|| call.started.elapsed());
                    if decoder.output().len() > MAX_ANSWER_BYTES {
                        return Err(AiError::transient(format!(
                            "the streamed answer is larger than {MAX_ANSWER_BYTES} bytes"
                        ))
                        .with_hint(RetryHint::Never));
                    }
                    if let Some(callback) = &call.options.on_text {
                        callback(decoder.output());
                    }
                }
            }
            if decoder.is_done() {
                break 'read;
            }
        }
        if ended {
            break;
        }
    }
    let mut turn = decoder.finish()?;
    turn.first_token = first_token;
    Ok(turn)
}

fn malformed(error: crate::sse::SseError) -> AiError {
    AiError::transient(format!("the stream is malformed: {error}"))
        .with_hint(RetryHint::StreamBroken)
}

/// The response of a finished chat call.
fn respond(
    turn: Turn,
    json: Option<Value>,
    tally: Tally,
    call: &Call<'_>,
    request: &ChatRequest,
) -> ChatResponse {
    if request.stream && (tally.stream_fallback || tally.repaired) {
        // The streamed text was replaced: show the final one.
        if let Some(callback) = &call.options.on_text {
            callback(&turn.text);
        }
    }
    ChatResponse {
        text: turn.text,
        json,
        finish: turn.finish.unwrap_or(FinishReason::Unknown),
        model: turn.model,
        usage: tally.usage,
        server_timings: turn.server_timings.or(tally.server_timings),
        timings: Timings {
            total: call.started.elapsed(),
            first_token: tally.first_token,
        },
        requests: tally.requests,
        stream_fallback: tally.stream_fallback,
        repaired: tally.repaired,
    }
}
