//! What a chat request asks for, and the answer the stub gives, before either
//! protocol renders it.

use std::time::Duration;

use bytes::Bytes;
use serde_json::Value;

use super::answer::{self, Answers};
use super::fault::Fault;

/// What the answer must be.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Wanted {
    /// Free text.
    Text,
    /// JSON, for this schema when the request named one.
    Json(Option<Value>),
    /// Anthropic's forced tool, whose input is the answer.
    Tool { name: String, schema: Value },
}

/// A chat request, as the stub reads it.
#[derive(Clone, Debug)]
pub(crate) struct Ask {
    pub(crate) model: String,
    pub(crate) key: String,
    pub(crate) stream: bool,
    pub(crate) wanted: Wanted,
    pub(crate) max_tokens: Option<u64>,
    pub(crate) input_chars: usize,
    pub(crate) images: usize,
    /// OpenAI-compatible: `stream_options.include_usage`.
    pub(crate) include_usage: bool,
}

/// Why the answer stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Done,
    Length,
    Refusal,
}

/// The answer, before rendering.
#[derive(Clone, Debug)]
pub(crate) struct Reply {
    pub(crate) text: String,
    pub(crate) stop: Stop,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
}

/// The JSON that [`Fault::NonConformingJson`] answers.
pub(crate) const NON_CONFORMING: &str = r#"{"stub":"nonconforming"}"#;

/// The text the model sends when it refuses.
pub(crate) const REFUSAL: &str = "I can't help with that.";

/// The reply to `ask`, under `fault`.
pub(crate) fn reply(answers: &Answers, ask: &Ask, fault: Option<&Fault>) -> Reply {
    let input_tokens = answer::tokens(ask.input_chars) + 85 * ask.images as u64;
    if matches!(fault, Some(Fault::Refusal)) {
        return Reply {
            text: String::new(),
            stop: Stop::Refusal,
            input_tokens,
            output_tokens: 1,
        };
    }
    let schema = match &ask.wanted {
        Wanted::Text => None,
        Wanted::Json(schema) => Some(schema.as_ref()),
        Wanted::Tool { schema, .. } => Some(Some(schema)),
    };
    let mut text = match (fault, schema) {
        (Some(Fault::EmptyStream), _) => String::new(),
        (Some(Fault::NonConformingJson), Some(_)) => NON_CONFORMING.to_owned(),
        _ => match answers.known(&ask.key) {
            Some(known) => known,
            None => match schema {
                None => answer::text_answer(&ask.key),
                Some(Some(schema)) => answer::schema_example(schema, &ask.key).to_string(),
                Some(None) => "{}".to_owned(),
            },
        },
    };
    let mut stop = Stop::Done;
    if let Some(cap) = ask.max_tokens {
        let cap_chars = usize::try_from(cap.saturating_mul(4)).unwrap_or(usize::MAX);
        if text.chars().count() > cap_chars {
            text = text.chars().take(cap_chars).collect();
            stop = Stop::Length;
        }
    }
    let output_tokens = answer::tokens(text.chars().count());
    Reply {
        text,
        stop,
        input_tokens,
        output_tokens,
    }
}

/// One frame of a streamed body.
pub(crate) struct Frame {
    pub(crate) delay: Duration,
    pub(crate) bytes: Bytes,
}

impl Frame {
    pub(crate) fn now(text: String) -> Self {
        Self {
            delay: Duration::ZERO,
            bytes: Bytes::from(text),
        }
    }
}

/// `data` as one SSE event, named `event` when given.
pub(crate) fn sse(event: Option<&str>, data: &str) -> String {
    match event {
        Some(event) => format!("event: {event}\ndata: {data}\n\n"),
        None => format!("data: {data}\n\n"),
    }
}

/// The size of the stub's streamed chunks, in characters.
pub(crate) const CHUNK_CHARS: usize = 8;
