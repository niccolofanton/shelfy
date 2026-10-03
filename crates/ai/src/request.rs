//! What a call sends.
//!
//! Prompts, captions and images are library content: the `Debug` of these
//! types prints sizes, never text or bytes, so a logged request leaks nothing
//! (plan §3.7).

use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::error::AiError;

/// One chat completion.
#[derive(Clone, Debug)]
pub struct ChatRequest {
    /// The model id. No model is a default (G3-23): it comes from the
    /// operator's settings or the user's provider.
    pub model: String,
    /// The system prompt.
    pub system: Option<String>,
    /// The conversation, first message from the user.
    pub messages: Vec<Message>,
    /// Sampling temperature, sent only when set (some models refuse it).
    pub temperature: Option<f64>,
    /// The answer's token cap. Anthropic requires one: 4,096 when unset.
    pub max_tokens: Option<u32>,
    /// Reasoning effort, sent only when set, for models that take it.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Text, or JSON for a schema.
    pub output: Output,
    /// Whether to stream the answer (text to [`crate::CallOptions::on_text`]).
    pub stream: bool,
    /// Extra top-level fields for OpenAI-compatible servers, such as
    /// llama.cpp's `chat_template_kwargs` (operator settings, P3-09). They
    /// never replace a field the adapter sets.
    pub extra_body: Option<Map<String, Value>>,
}

impl ChatRequest {
    /// A text request for `model` with `messages`, not streamed.
    #[must_use]
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            system: None,
            messages,
            temperature: None,
            max_tokens: None,
            reasoning_effort: None,
            output: Output::Text,
            stream: false,
            extra_body: None,
        }
    }

    /// With the system prompt `system`.
    #[must_use]
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// With `temperature`.
    #[must_use]
    pub fn with_temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// With a token cap.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// With a reasoning effort.
    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Asking for JSON that matches `output`'s schema.
    #[must_use]
    pub fn with_json(mut self, output: JsonOutput) -> Self {
        self.output = Output::Json(output);
        self
    }

    /// Streamed or not.
    #[must_use]
    pub fn streamed(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// With an extra top-level field (OpenAI-compatible servers only).
    #[must_use]
    pub fn with_extra(mut self, name: impl Into<String>, value: Value) -> Self {
        self.extra_body
            .get_or_insert_with(Map::new)
            .insert(name.into(), value);
        self
    }
}

/// Who wrote a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The user (the prompt, the post's content).
    User,
    /// The model (earlier turns of a chat).
    Assistant,
}

/// One message: text and images.
#[derive(Clone, Debug)]
pub struct Message {
    /// Its author.
    pub role: Role,
    /// Its parts, in order.
    pub parts: Vec<Part>,
}

impl Message {
    /// A user message of `parts`.
    #[must_use]
    pub fn user(parts: Vec<Part>) -> Self {
        Self {
            role: Role::User,
            parts,
        }
    }

    /// A user message of one text.
    #[must_use]
    pub fn user_text(text: impl Into<String>) -> Self {
        Self::user(vec![Part::Text(text.into())])
    }

    /// An assistant message of one text.
    #[must_use]
    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            parts: vec![Part::Text(text.into())],
        }
    }

    /// The message's text parts, joined with blank lines.
    pub(crate) fn text(&self) -> String {
        let texts: Vec<&str> = self
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::Text(text) => Some(text.as_str()),
                Part::Image(_) => None,
            })
            .collect();
        texts.join("\n\n")
    }
}

/// Part of a message.
#[derive(Clone)]
pub enum Part {
    /// Text.
    Text(String),
    /// An image, sent inline as base64 (never as a URL, P3 lane rule 8).
    Image(Image),
}

impl Part {
    /// A text part.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// A WebP image part (the `g480` renditions are WebP).
    #[must_use]
    pub fn webp(data: impl Into<Bytes>) -> Self {
        Self::Image(Image {
            media_type: ImageType::Webp,
            data: data.into(),
        })
    }
}

impl fmt::Debug for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => write!(f, "Text({} chars)", text.chars().count()),
            Self::Image(image) => write!(f, "{image:?}"),
        }
    }
}

/// An image's bytes and type.
#[derive(Clone)]
pub struct Image {
    /// Its type.
    pub media_type: ImageType,
    /// Its bytes.
    pub data: Bytes,
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Image({}, {} bytes)",
            self.media_type.mime(),
            self.data.len()
        )
    }
}

/// The image types every target provider accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageType {
    /// `image/webp`
    Webp,
    /// `image/png`
    Png,
    /// `image/jpeg`
    Jpeg,
    /// `image/gif`
    Gif,
}

impl ImageType {
    /// The MIME type.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Webp => "image/webp",
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
        }
    }
}

/// Reasoning effort for models that take it (`reasoning_effort` on
/// OpenAI-compatible servers, `output_config.effort` on Anthropic).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReasoningEffort {
    /// `low`: the plan's choice for cataloging and chat (§2.15).
    Low,
    /// `medium`
    Medium,
    /// `high`
    High,
}

impl ReasoningEffort {
    /// The wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// What the answer must be.
#[derive(Clone, Debug)]
pub enum Output {
    /// Free text.
    Text,
    /// JSON that matches a schema.
    Json(JsonOutput),
}

/// A JSON schema the answer must match, compiled once and shared by every
/// call that uses it.
#[derive(Clone)]
pub struct JsonOutput {
    name: String,
    schema: Arc<Value>,
    /// The schema's text in its own key order, when it was given as such.
    raw: Option<Arc<RawValue>>,
    validator: Arc<jsonschema::Validator>,
    strict: bool,
}

impl JsonOutput {
    /// The schema `schema`, named `name` (`[A-Za-z0-9_-]{1,64}`, as OpenAI's
    /// `json_schema.name` and Anthropic's tool names require). Strict by
    /// default: every object closes `additionalProperties` and requires every
    /// property, as the shared catalog schemas do.
    ///
    /// # Errors
    ///
    /// [`crate::ErrorKind::BadRequest`] for a bad name or a schema that does
    /// not compile.
    pub fn new(name: impl Into<String>, schema: Value) -> Result<Self, AiError> {
        let name = name.into();
        let valid_name = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !valid_name {
            return Err(AiError::bad_request(
                "a schema name has 1 to 64 letters, digits, '_' or '-'",
            ));
        }
        let validator = jsonschema::validator_for(&schema).map_err(|error| {
            AiError::bad_request(format!(
                "the JSON schema does not compile: {}",
                error.masked()
            ))
        })?;
        Ok(Self {
            name,
            schema: Arc::new(schema),
            raw: None,
            validator: Arc::new(validator),
            strict: true,
        })
    }

    /// Like [`JsonOutput::new`], for a schema given as JSON text. The request
    /// body carries that text as it is, keys in the file's order: a provider
    /// fills the answer's fields in the schema's order, and a parsed
    /// [`Value`] sorts them.
    ///
    /// # Errors
    ///
    /// As [`JsonOutput::new`].
    pub fn from_raw(name: impl Into<String>, raw: &RawValue) -> Result<Self, AiError> {
        let schema = serde_json::from_str(raw.get())
            .map_err(|_| AiError::bad_request("the JSON schema is not JSON"))?;
        let mut output = Self::new(name, schema)?;
        output.raw = Some(Arc::from(raw.to_owned()));
        Ok(output)
    }

    /// Non-strict: OpenAI-compatible servers get `strict: false`, Anthropic
    /// tools no `strict`. The answer is validated either way.
    #[must_use]
    pub fn non_strict(mut self) -> Self {
        self.strict = false;
        self
    }

    /// The schema's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The schema.
    #[must_use]
    pub fn schema(&self) -> &Value {
        &self.schema
    }

    /// The schema as it goes on the wire: the given text when there is one,
    /// else the parsed schema (keys sorted).
    pub(crate) fn schema_text(&self) -> String {
        self.raw
            .as_ref()
            .map_or_else(|| self.schema.to_string(), |raw| raw.get().to_owned())
    }

    /// Whether the provider is asked to enforce it strictly.
    #[must_use]
    pub fn is_strict(&self) -> bool {
        self.strict
    }

    pub(crate) fn validator(&self) -> &jsonschema::Validator {
        &self.validator
    }
}

impl fmt::Debug for JsonOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonOutput")
            .field("name", &self.name)
            .field("strict", &self.strict)
            .finish_non_exhaustive()
    }
}

/// Embeddings for a batch of texts (OpenAI-compatible `/embeddings`).
#[derive(Clone)]
pub struct EmbedRequest {
    /// The model id.
    pub model: String,
    /// The texts, embedded in order.
    pub input: Vec<String>,
    /// The vector size, for models that can shorten theirs.
    pub dimensions: Option<u32>,
}

impl fmt::Debug for EmbedRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmbedRequest")
            .field("model", &self.model)
            .field("inputs", &self.input.len())
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

/// A transcription of one recording: a 16 kHz mono 16-bit WAV, as the
/// desktop's AudioWorklet recorder makes it (G3-26).
#[derive(Clone)]
pub struct TranscribeRequest {
    /// The WAV file.
    pub wav: Bytes,
    /// The spoken language (ISO 639-1), from the UI.
    pub language: Option<String>,
    /// The model, for OpenAI-compatible `/audio/transcriptions`; whisper.cpp
    /// serves the one it loaded and ignores it.
    pub model: Option<String>,
}

impl fmt::Debug for TranscribeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TranscribeRequest")
            .field("wav_bytes", &self.wav.len())
            .field("language", &self.language)
            .field("model", &self.model)
            .finish()
    }
}
