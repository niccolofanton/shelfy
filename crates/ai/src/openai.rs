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
use serde_json::{Value, json};

use crate::error::{AiError, RetryHint};
use crate::multipart::Form;
use crate::provider::ProviderConfig;
use crate::request::{ChatRequest, EmbedRequest, Message, Output, Part, Role, TranscribeRequest};
use crate::response::{Embeddings, FinishReason, ModelInfo, ServerTimings, Usage};
use crate::sse::SseEvent;
use crate::structured::{StructuredMode, with_json_instruction};
use crate::turn::{Delta, StreamDecoder, Turn, content_text, count, event_json};
use crate::wire::{self, Body, from_error_value, object};

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
    let mut body = Body::default();
    body.insert("model", &request.model);
    let schema = match &request.output {
        Output::Json(output) => Some(output),
        Output::Text => None,
    };
    let system = match (mode, schema) {
        (Some(StructuredMode::JsonObject), Some(output)) => Some(with_json_instruction(
            request.system.as_deref(),
            &output.schema_text(),
        )),
        _ => request.system.clone(),
    };
    let mut messages = Vec::with_capacity(request.messages.len() + extra.len() + 1);
    if let Some(system) = system.filter(|system| !system.is_empty()) {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.extend(request.messages.iter().chain(extra).map(message));
    body.insert("messages", &messages);
    if let Some(temperature) = request.temperature.filter(|_| config.send_temperature) {
        body.insert("temperature", &temperature);
    }
    if let Some(max_tokens) = request.max_tokens {
        body.insert(config.max_tokens_field.as_str(), &max_tokens);
    }
    if let Some(effort) = request.reasoning_effort {
        body.insert("reasoning_effort", effort.as_str());
    }
    match (mode, schema) {
        (Some(StructuredMode::JsonSchema), Some(output)) => {
            // Members sorted by key, as a `Map` would write them, but the
            // schema's own text: its keys keep the file's order.
            let json_schema = object(&[
                ("name", json!(output.name()).to_string()),
                ("schema", output.schema_text()),
                ("strict", json!(output.is_strict()).to_string()),
            ]);
            body.insert_json(
                "response_format",
                object(&[
                    ("json_schema", json_schema),
                    ("type", json!("json_schema").to_string()),
                ]),
            );
        }
        (Some(StructuredMode::JsonObject), Some(_)) => {
            body.insert("response_format", &json!({"type": "json_object"}));
        }
        _ => {}
    }
    if stream {
        body.insert("stream", &true);
        if config.stream_usage {
            body.insert("stream_options", &json!({"include_usage": true}));
        }
    }
    // The request's extras first: they win over the provider's.
    for extra_body in [request.extra_body.as_ref(), config.extra_body.as_ref()]
        .into_iter()
        .flatten()
    {
        for (name, value) in extra_body {
            body.insert_if_absent(name, value);
        }
    }
    body.into_bytes()
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
    use crate::request::JsonOutput;
    use crate::structured::JSON_INSTRUCTION;

    /// The schema texts of `shared/ai`, as the desktop writes them
    /// (`JSON.stringify` of the parsed file: compact, keys in file order).
    fn shared_schemas() -> Vec<(String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shared/ai");
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if let Some(task) = name.strip_suffix(".schema.json") {
                let text = std::fs::read_to_string(&path).unwrap();
                found.push((task.to_owned(), compact(&text)));
            }
        }
        assert!(found.len() >= 6, "the shared schemas were found");
        found.sort();
        found
    }

    fn compact(text: &str) -> String {
        let (mut out, mut in_string, mut escaped) = (String::new(), false, false);
        for c in text.chars() {
            if in_string {
                out.push(c);
                match (escaped, c) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => in_string = false,
                    _ => {}
                }
            } else if c == '"' {
                in_string = true;
                out.push(c);
            } else if !c.is_whitespace() {
                out.push(c);
            }
        }
        out
    }

    fn output_of(name: &str, text: &str) -> JsonOutput {
        let raw = serde_json::value::RawValue::from_string(text.to_owned()).unwrap();
        JsonOutput::from_raw(name.replace('_', "-"), &raw).unwrap()
    }

    fn request_with(output: JsonOutput) -> ChatRequest {
        ChatRequest::new("m", vec![Message::user_text("a lamp")])
            .with_system("Catalog the post.")
            .with_json(output)
    }

    fn config() -> ProviderConfig {
        ProviderConfig::new(
            crate::ProviderKind::OpenAiCompatible,
            crate::Source::Operator,
            "http://localhost:1/v1".parse().unwrap(),
        )
    }

    fn body_text(output: JsonOutput, mode: StructuredMode, stream: bool) -> String {
        let bytes = chat_body(&config(), &request_with(output), &[], Some(mode), stream);
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn a_json_schema_body_carries_the_schema_in_the_files_order() {
        for (task, schema) in shared_schemas() {
            let text = body_text(output_of(&task, &schema), StructuredMode::JsonSchema, false);
            let name = task.replace('_', "-");
            let expected = format!(
                r#""response_format":{{"json_schema":{{"name":"{name}","schema":{schema},"strict":true}},"type":"json_schema"}}"#
            );
            assert!(text.contains(&expected), "{task}: {text}");
            // The body is still one JSON object.
            serde_json::from_str::<Value>(&text).unwrap();
        }
    }

    #[test]
    fn the_catalog_schema_is_not_sorted_on_the_wire() {
        let (_, schema) = shared_schemas()
            .into_iter()
            .find(|(task, _)| task == "catalog")
            .unwrap();
        let sorted = serde_json::from_str::<Value>(&schema).unwrap().to_string();
        assert_ne!(schema, sorted, "the file's order is not the sorted one");
        let text = body_text(
            output_of("catalog", &schema),
            StructuredMode::JsonSchema,
            false,
        );
        assert!(text.contains(&schema));
        assert!(!text.contains(&sorted));
    }

    #[test]
    fn json_object_mode_puts_the_schema_in_the_prompt_in_the_files_order() {
        for (task, schema) in shared_schemas() {
            let text = body_text(output_of(&task, &schema), StructuredMode::JsonObject, false);
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                body["messages"][0]["content"],
                format!("Catalog the post.\n\n{JSON_INSTRUCTION}\n{schema}"),
                "{task}"
            );
        }
    }

    #[test]
    fn the_other_members_keep_their_sorted_order() {
        let output = JsonOutput::new("t", json!({"type": "object"})).unwrap();
        let request = request_with(output).with_max_tokens(7);
        let bytes = chat_body(
            &config(),
            &request,
            &[],
            Some(StructuredMode::JsonSchema),
            true,
        );
        assert_eq!(
            String::from_utf8(bytes.to_vec()).unwrap(),
            concat!(
                r#"{"max_tokens":7,"messages":[{"content":"Catalog the post.","role":"system"},"#,
                r#"{"content":"a lamp","role":"user"}],"model":"m","#,
                r#""response_format":{"json_schema":{"name":"t","schema":{"type":"object"},"strict":true},"#,
                r#""type":"json_schema"},"stream":true,"stream_options":{"include_usage":true}}"#
            )
        );
    }

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
