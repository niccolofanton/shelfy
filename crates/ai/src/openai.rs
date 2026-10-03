//! The OpenAI-compatible protocol: OpenAI, Gemini (`…/v1beta/openai`),
//! OpenRouter, Groq, Mistral, Together, llama.cpp and other custom servers
//! (plan §2.15).
//!
//! Calls go to the base URL verbatim plus `/chat/completions`, `/embeddings`,
//! `/audio/transcriptions` or `/models`. Images travel as `image_url` data
//! URLs. Streamed answers are `data:` chunks ending in `data: [DONE]`; with
//! [`crate::ProviderConfig::stream_usage`] the request asks for a last chunk
//! with the usage.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use http::HeaderMap;
use http::header::AUTHORIZATION;
use secrecy::SecretString;
use serde_json::{Map, Value, json};

use crate::error::{AiError, RetryHint};
use crate::multipart::Form;
use crate::provider::ProviderConfig;
use crate::request::{ChatRequest, EmbedRequest, Message, Output, Part, Role, TranscribeRequest};
use crate::response::{Embeddings, FinishReason, ModelInfo, ServerTimings, Usage};
use crate::sse::SseEvent;
use crate::structured::{StructuredMode, with_json_instruction};
use crate::turn::{Delta, StreamDecoder, Turn, content_text, count, event_json};
use crate::wire::{self, from_error_value};

/// The headers of a call, with the key as a Bearer token when there is one.
pub(crate) fn headers(key: Option<&SecretString>, stream: bool) -> Result<HeaderMap, AiError> {
    let mut headers = wire::json_headers(stream);
    if let Some(key) = key {
        wire::put_key(&mut headers, AUTHORIZATION, "Bearer ", key)?;
    }
    Ok(headers)
}

/// The body of a chat completion: `request`, then `extra` messages (the
/// repair turn), with `mode` for JSON answers.
pub(crate) fn chat_body(
    config: &ProviderConfig,
    request: &ChatRequest,
    extra: &[Message],
    mode: Option<StructuredMode>,
    stream: bool,
) -> Bytes {
    let mut body = Map::new();
    body.insert("model".into(), request.model.clone().into());
    let schema = match &request.output {
        Output::Json(output) => Some(output),
        Output::Text => None,
    };
    let system = match (mode, schema) {
        (Some(StructuredMode::JsonObject), Some(output)) => Some(with_json_instruction(
            request.system.as_deref(),
            output.schema(),
        )),
        _ => request.system.clone(),
    };
    let mut messages = Vec::with_capacity(request.messages.len() + extra.len() + 1);
    if let Some(system) = system.filter(|system| !system.is_empty()) {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.extend(request.messages.iter().chain(extra).map(message));
    body.insert("messages".into(), messages.into());
    if let Some(temperature) = request.temperature {
        body.insert("temperature".into(), temperature.into());
    }
    if let Some(max_tokens) = request.max_tokens {
        body.insert(config.max_tokens_field.as_str().into(), max_tokens.into());
    }
    if let Some(effort) = request.reasoning_effort {
        body.insert("reasoning_effort".into(), effort.as_str().into());
    }
    match (mode, schema) {
        (Some(StructuredMode::JsonSchema), Some(output)) => {
            body.insert(
                "response_format".into(),
                json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": output.name(),
                        "schema": output.schema(),
                        "strict": output.is_strict(),
                    }
                }),
            );
        }
        (Some(StructuredMode::JsonObject), Some(_)) => {
            body.insert("response_format".into(), json!({"type": "json_object"}));
        }
        _ => {}
    }
    if stream {
        body.insert("stream".into(), true.into());
        if config.stream_usage {
            body.insert("stream_options".into(), json!({"include_usage": true}));
        }
    }
    // The request's extras first: they win over the provider's.
    for extra_body in [request.extra_body.as_ref(), config.extra_body.as_ref()]
        .into_iter()
        .flatten()
    {
        for (name, value) in extra_body {
            body.entry(name.clone()).or_insert_with(|| value.clone());
        }
    }
    Bytes::from(Value::Object(body).to_string())
}

/// One message: a string when it is text only (every server takes that),
/// else an array of text and `image_url` parts.
fn message(message: &Message) -> Value {
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let text_only = message
        .parts
        .iter()
        .all(|part| matches!(part, Part::Text(_)));
    if text_only || message.role == Role::Assistant {
        return json!({"role": role, "content": message.text()});
    }
    let parts: Vec<Value> = message
        .parts
        .iter()
        .map(|part| match part {
            Part::Text(text) => json!({"type": "text", "text": text}),
            Part::Image(image) => json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", image.media_type.mime(), BASE64.encode(&image.data)),
                },
            }),
        })
        .collect();
    json!({"role": role, "content": parts})
}

/// The answer of a chat completion that was not streamed.
pub(crate) fn parse_chat(body: &[u8], key: Option<&SecretString>) -> Result<Turn, AiError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| AiError::transient("the answer is not JSON"))?;
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        return Err(from_error_value(error, key));
    }
    let choice = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .ok_or_else(|| AiError::transient("the answer has no choice"))?;
    let message = choice.get("message").unwrap_or(&Value::Null);
    let finish = finish_reason(choice.get("finish_reason"));
    let refusal = message
        .get("refusal")
        .and_then(Value::as_str)
        .filter(|refusal| !refusal.is_empty())
        .map(str::to_owned)
        .or_else(|| content_filtered(finish.as_ref()));
    Ok(Turn {
        text: message
            .get("content")
            .and_then(content_text)
            .unwrap_or_default(),
        finish,
        refusal,
        usage: value.get("usage").and_then(usage),
        server_timings: value.get("timings").and_then(server_timings),
        model: value
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        first_token: None,
    })
}

/// `finish_reason` as a [`FinishReason`].
fn finish_reason(reason: Option<&Value>) -> Option<FinishReason> {
    let reason = reason?.as_str()?;
    Some(match reason {
        "stop" | "eos" => FinishReason::Stop,
        "length" | "max_tokens" => FinishReason::Length,
        "tool_calls" | "function_call" => FinishReason::ToolUse,
        other => FinishReason::Other(other.to_owned()),
    })
}

/// The refusal a `content_filter` finish stands for.
fn content_filtered(finish: Option<&FinishReason>) -> Option<String> {
    matches!(finish, Some(FinishReason::Other(reason)) if reason == "content_filter")
        .then(|| "the provider's content filter stopped the answer".to_owned())
}

/// `usage`, when the server sent one.
fn usage(usage: &Value) -> Option<Usage> {
    if !usage.is_object() {
        return None;
    }
    let input = count(usage, "prompt_tokens").or_else(|| count(usage, "input_tokens"));
    let output = count(usage, "completion_tokens").or_else(|| count(usage, "output_tokens"));
    if input.is_none() && output.is_none() {
        return None;
    }
    let cached = usage
        .get("prompt_tokens_details")
        .and_then(|details| count(details, "cached_tokens"))
        .unwrap_or(0);
    Some(Usage {
        input_tokens: input.unwrap_or(0),
        output_tokens: output.unwrap_or(0),
        cached_input_tokens: cached,
    })
}

/// llama.cpp's `timings`.
fn server_timings(timings: &Value) -> Option<ServerTimings> {
    if !timings.is_object() {
        return None;
    }
    Some(ServerTimings {
        prompt_tokens: count(timings, "prompt_n"),
        prompt_ms: timings.get("prompt_ms").and_then(Value::as_f64),
        predicted_tokens: count(timings, "predicted_n"),
        predicted_ms: timings.get("predicted_ms").and_then(Value::as_f64),
        cached_tokens: count(timings, "cache_n"),
    })
}

/// Decodes a streamed chat completion.
#[derive(Default)]
pub(crate) struct ChatStream {
    turn: Turn,
    refusal: String,
    done: bool,
    key: Option<SecretString>,
}

impl ChatStream {
    pub(crate) fn new(key: Option<&SecretString>) -> Self {
        Self {
            key: key.cloned(),
            ..Self::default()
        }
    }
}

impl StreamDecoder for ChatStream {
    fn event(&mut self, event: &SseEvent) -> Result<Delta, AiError> {
        let data = event.data.trim();
        if data.is_empty() {
            return Ok(Delta::None);
        }
        if data == "[DONE]" {
            self.done = true;
            return Ok(Delta::None);
        }
        let value = event_json(event)?;
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            let error = from_error_value(error, self.key.as_ref());
            let hint = if error.kind().is_retryable() {
                RetryHint::StreamBroken
            } else {
                RetryHint::Normal
            };
            return Err(error.with_hint(hint));
        }
        if let Some(model) = value.get("model").and_then(Value::as_str) {
            self.turn.model = Some(model.to_owned());
        }
        if let Some(usage) = value.get("usage").and_then(usage) {
            self.turn.usage = Some(usage);
        }
        if let Some(timings) = value.get("timings").and_then(server_timings) {
            self.turn.server_timings = Some(timings);
        }
        let Some(choice) = value.get("choices").and_then(|choices| choices.get(0)) else {
            return Ok(Delta::None);
        };
        if let Some(finish) = finish_reason(choice.get("finish_reason")) {
            self.turn.finish = Some(finish);
        }
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        let mut step = Delta::None;
        let reasoning = ["reasoning_content", "reasoning"].iter().any(|name| {
            delta
                .get(*name)
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty())
        });
        if reasoning {
            step = Delta::Token;
        }
        if let Some(refusal) = delta.get("refusal").and_then(Value::as_str) {
            self.refusal.push_str(refusal);
            step = Delta::Token;
        }
        if let Some(text) = delta
            .get("content")
            .and_then(content_text)
            .filter(|text| !text.is_empty())
        {
            self.turn.text.push_str(&text);
            step = Delta::Output;
        }
        Ok(step)
    }

    fn output(&self) -> &str {
        &self.turn.text
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn finish(self: Box<Self>) -> Result<Turn, AiError> {
        if !self.done && self.turn.finish.is_none() {
            return Err(
                AiError::transient("the stream ended before the answer finished")
                    .with_hint(RetryHint::StreamBroken),
            );
        }
        let mut turn = self.turn;
        turn.refusal = (!self.refusal.is_empty())
            .then_some(self.refusal)
            .or_else(|| content_filtered(turn.finish.as_ref()));
        Ok(turn)
    }
}

/// The body of an embeddings call.
pub(crate) fn embeddings_body(request: &EmbedRequest) -> Bytes {
    let mut body = json!({
        "model": request.model,
        "input": request.input,
        "encoding_format": "float",
    });
    if let Some(dimensions) = request.dimensions {
        body["dimensions"] = dimensions.into();
    }
    Bytes::from(body.to_string())
}

/// The vectors of an embeddings answer, in input order (`requests` is left
/// for the caller).
pub(crate) fn parse_embeddings(body: &[u8], inputs: usize) -> Result<Embeddings, AiError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| AiError::transient("the answer is not JSON"))?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| AiError::transient("the answer has no data"))?;
    let mut indexed = Vec::with_capacity(data.len());
    for (position, item) in data.iter().enumerate() {
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .unwrap_or(position);
        let vector = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| AiError::transient("an embedding is not an array of numbers"))?
            .iter()
            .map(|number| number.as_f64().map(|number| number as f32))
            .collect::<Option<Vec<f32>>>()
            .ok_or_else(|| AiError::transient("an embedding is not an array of numbers"))?;
        indexed.push((index, vector));
    }
    indexed.sort_by_key(|(index, _)| *index);
    if indexed.len() != inputs
        || indexed
            .iter()
            .enumerate()
            .any(|(n, (index, _))| n != *index)
    {
        return Err(AiError::transient(format!(
            "the answer has {} vectors for {inputs} inputs",
            indexed.len()
        )));
    }
    Ok(Embeddings {
        vectors: indexed.into_iter().map(|(_, vector)| vector).collect(),
        model: value
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        usage: value.get("usage").and_then(usage),
        requests: 0,
    })
}

/// The form of an OpenAI-style `/audio/transcriptions` call.
pub(crate) fn transcription_form(request: &TranscribeRequest, model: &str) -> (String, Bytes) {
    let mut form = Form::new();
    form.file("file", "audio.wav", "audio/wav", &request.wav);
    form.text("model", model);
    form.text("response_format", "json");
    form.text("temperature", "0");
    if let Some(language) = request
        .language
        .as_deref()
        .filter(|language| !language.is_empty())
    {
        form.text("language", language);
    }
    form.finish()
}

/// The text of a transcription answer (`{"text": …}`, OpenAI and whisper.cpp).
pub(crate) fn parse_transcription(
    body: &[u8],
    key: Option<&SecretString>,
) -> Result<String, AiError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| AiError::transient("the transcription answer is not JSON"))?;
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        // whisper.cpp answers some errors with 200 and `{"error": "…"}`.
        let mut error = from_error_value(error, key);
        if error.kind() == crate::ErrorKind::Transient && error.status().is_none() {
            error = AiError::bad_request(error.message().to_owned());
        }
        return Err(error);
    }
    value
        .get("text")
        .and_then(Value::as_str)
        .map(|text| text.trim().to_owned())
        .ok_or_else(|| AiError::transient("the transcription answer has no text"))
}

/// The models of a `/models` answer.
pub(crate) fn parse_models(body: &[u8]) -> Result<Vec<ModelInfo>, AiError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| AiError::transient("the answer is not JSON"))?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| AiError::transient("the answer has no model list"))?;
    Ok(data
        .iter()
        .filter_map(|model| {
            let id = model.get("id").and_then(Value::as_str)?;
            Some(ModelInfo {
                id: id.to_owned(),
                display_name: model
                    .get("display_name")
                    .or_else(|| model.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    #[test]
    fn non_streamed_answers_carry_usage_and_llama_timings() {
        let body = json!({
            "model": "ornith",
            "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant", "content": "hi"}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 8}},
            "timings": {"prompt_n": 4, "prompt_ms": 12.5, "predicted_n": 3, "predicted_ms": 30.0, "cache_n": 8}
        });
        let turn = parse_chat(body.to_string().as_bytes(), None).unwrap();
        assert_eq!(turn.text, "hi");
        assert_eq!(turn.finish, Some(FinishReason::Stop));
        assert_eq!(
            turn.usage,
            Some(Usage {
                input_tokens: 12,
                output_tokens: 3,
                cached_input_tokens: 8
            })
        );
        assert_eq!(turn.server_timings.unwrap().predicted_ms, Some(30.0));
        assert_eq!(turn.model.as_deref(), Some("ornith"));
    }

    #[test]
    fn refusals_and_content_filters_are_refusals() {
        let refusal = json!({"choices": [{"finish_reason": "stop", "message": {"content": null, "refusal": "I can't help"}}]});
        assert_eq!(
            parse_chat(refusal.to_string().as_bytes(), None)
                .unwrap()
                .refusal
                .as_deref(),
            Some("I can't help")
        );
        let filtered =
            json!({"choices": [{"finish_reason": "content_filter", "message": {"content": ""}}]});
        assert!(
            parse_chat(filtered.to_string().as_bytes(), None)
                .unwrap()
                .refusal
                .is_some()
        );
    }

    #[test]
    fn content_parts_are_joined() {
        let body = json!({"choices": [{"finish_reason": "stop", "message": {"content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}}]});
        assert_eq!(
            parse_chat(body.to_string().as_bytes(), None).unwrap().text,
            "ab"
        );
    }

    #[test]
    fn an_error_in_a_200_answer_is_an_error() {
        let body = json!({"error": {"code": 429, "message": "Rate limit exceeded"}});
        assert_eq!(
            parse_chat(body.to_string().as_bytes(), None)
                .unwrap_err()
                .kind(),
            ErrorKind::RateLimited
        );
        assert_eq!(
            parse_chat(b"not json", None).unwrap_err().kind(),
            ErrorKind::Transient
        );
    }

    #[test]
    fn whisper_errors_in_200_answers_are_bad_requests() {
        assert_eq!(
            parse_transcription(br#"{"text":" hello \n"}"#, None).unwrap(),
            "hello"
        );
        let error = parse_transcription(br#"{"error":"no 'file' field in the request"}"#, None)
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::BadRequest);
    }

    #[test]
    fn embeddings_come_back_in_input_order() {
        let body = json!({"data": [{"index": 1, "embedding": [0.5]}, {"index": 0, "embedding": [0.25, 1]}], "usage": {"prompt_tokens": 4}});
        let Embeddings { vectors, usage, .. } =
            parse_embeddings(body.to_string().as_bytes(), 2).unwrap();
        assert_eq!(vectors, vec![vec![0.25, 1.0], vec![0.5]]);
        assert_eq!(usage.unwrap().input_tokens, 4);
        assert!(parse_embeddings(body.to_string().as_bytes(), 3).is_err());
    }
}
