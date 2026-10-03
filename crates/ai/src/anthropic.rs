//! The Anthropic Messages protocol (plan §2.15): `{base}/v1/messages` with
//! `x-api-key` and `anthropic-version`, a top-level system prompt, base64
//! image blocks, and JSON answers through `output_config.format`
//! ([`StructuredMode::JsonSchema`]) or one forced tool whose `input_schema` is
//! the schema ([`StructuredMode::Tool`]). Streamed answers are typed events;
//! a forced tool's input arrives as `input_json_delta` fragments.
//!
//! Anthropic serves no embeddings and no transcription: those calls answer
//! [`crate::ErrorKind::Unsupported`], and the AI service routes them to
//! another provider.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue};
use secrecy::SecretString;
use serde_json::{Value, json};

use crate::error::{AiError, RetryHint};
use crate::provider::ProviderConfig;
use crate::request::{ChatRequest, Message, Output, Part, Role};
use crate::response::{FinishReason, ModelInfo, Usage};
use crate::sse::SseEvent;
use crate::structured::{StructuredMode, with_json_instruction};
use crate::turn::{Delta, StreamDecoder, Turn, count, event_json};
use crate::wire::{self, Body, from_error_value, object};

/// The API version every call names.
pub const VERSION: &str = "2023-06-01";

/// The token cap sent when the request sets none (the API requires one).
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// The headers of a call.
pub(crate) fn headers(key: Option<&SecretString>, stream: bool) -> Result<HeaderMap, AiError> {
    let mut headers = wire::json_headers(stream);
    headers.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static(VERSION),
    );
    if let Some(key) = key {
        wire::put_key(&mut headers, HeaderName::from_static("x-api-key"), "", key)?;
    }
    Ok(headers)
}

/// The body of a Messages call: `request`, then `extra` messages (the repair
/// turn), with `mode` for JSON answers.
pub(crate) fn chat_body(
    config: &ProviderConfig,
    request: &ChatRequest,
    extra: &[Message],
    mode: Option<StructuredMode>,
    stream: bool,
) -> Bytes {
    let mut body = Body::default();
    body.insert("model", &request.model);
    body.insert(
        "max_tokens",
        &request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
    );
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
    if let Some(system) = system.filter(|system| !system.is_empty()) {
        body.insert("system", &system);
    }
    let messages: Vec<Value> = request.messages.iter().chain(extra).map(message).collect();
    body.insert("messages", &messages);
    if let Some(temperature) = request.temperature.filter(|_| config.send_temperature) {
        body.insert("temperature", &temperature);
    }
    let mut output_config = Body::default();
    if let Some(effort) = request.reasoning_effort {
        output_config.insert("effort", effort.as_str());
    }
    // Objects holding the schema list their members sorted by key, as a
    // `Map` would, but carry the schema's own text: its keys keep the file's
    // order.
    match (mode, schema) {
        (Some(StructuredMode::JsonSchema), Some(output)) => {
            output_config.insert_json(
                "format",
                object(&[
                    ("schema", output.schema_text()),
                    ("type", json!("json_schema").to_string()),
                ]),
            );
        }
        (Some(StructuredMode::Tool), Some(output)) => {
            let mut members = vec![(
                "description",
                json!("Record the answer. Its input is the whole answer.").to_string(),
            )];
            if stream {
                // Fragments as they are generated, not one per finished
                // parameter; the adapter validates the whole input anyway.
                members.push(("eager_input_streaming", "true".to_owned()));
            }
            members.push(("input_schema", output.schema_text()));
            members.push(("name", json!(output.name()).to_string()));
            if output.is_strict() {
                members.push(("strict", "true".to_owned()));
            }
            body.insert_json("tools", format!("[{}]", object(&members)));
            body.insert(
                "tool_choice",
                &json!({"type": "tool", "name": output.name()}),
            );
        }
        _ => {}
    }
    if !output_config.is_empty() {
        body.insert_json("output_config", output_config.into_json());
    }
    if stream {
        body.insert("stream", &true);
    }
    body.into_bytes()
}

/// One message: a string when it is text only, else text and base64 image
/// blocks.
fn message(message: &Message) -> Value {
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let text_only = message
        .parts
        .iter()
        .all(|part| matches!(part, Part::Text(_)));
    if text_only {
        return json!({"role": role, "content": message.text()});
    }
    let blocks: Vec<Value> = message
        .parts
        .iter()
        .map(|part| match part {
            Part::Text(text) => json!({"type": "text", "text": text}),
            Part::Image(image) => json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.media_type.mime(),
                    "data": BASE64.encode(&image.data),
                },
            }),
        })
        .collect();
    json!({"role": role, "content": blocks})
}

/// `stop_reason` as a [`FinishReason`], and whether it is a refusal.
fn stop_reason(reason: &str) -> (FinishReason, bool) {
    match reason {
        "end_turn" | "stop_sequence" => (FinishReason::Stop, false),
        "max_tokens" | "model_context_window_exceeded" => (FinishReason::Length, false),
        "tool_use" => (FinishReason::ToolUse, false),
        "refusal" => (FinishReason::Other("refusal".into()), true),
        other => (FinishReason::Other(other.to_owned()), false),
    }
}

/// Usage from a `usage` object: cache reads and writes count as input.
fn usage(usage: &Value) -> Option<Usage> {
    if !usage.is_object() {
        return None;
    }
    let read = count(usage, "cache_read_input_tokens").unwrap_or(0);
    let written = count(usage, "cache_creation_input_tokens").unwrap_or(0);
    Some(Usage {
        input_tokens: count(usage, "input_tokens")
            .unwrap_or(0)
            .saturating_add(read)
            .saturating_add(written),
        output_tokens: count(usage, "output_tokens").unwrap_or(0),
        cached_input_tokens: read,
    })
}

/// The answer of a Messages call that was not streamed: the forced tool's
/// input when there is one, else the text blocks.
pub(crate) fn parse_chat(body: &[u8], key: Option<&SecretString>) -> Result<Turn, AiError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| AiError::transient("the answer is not JSON"))?;
    if value.get("type").and_then(Value::as_str) == Some("error") {
        return Err(from_error_value(
            value.get("error").unwrap_or(&Value::Null),
            key,
        ));
    }
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| AiError::transient("the answer has no content"))?;
    let mut text = String::new();
    let mut tool_input = None;
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            Some("tool_use") => tool_input = block.get("input").map(Value::to_string),
            _ => {}
        }
    }
    let (finish, refused) =
        value
            .get("stop_reason")
            .and_then(Value::as_str)
            .map_or((None, false), |reason| {
                let (finish, refused) = stop_reason(reason);
                (Some(finish), refused)
            });
    Ok(Turn {
        text: tool_input.unwrap_or(text),
        finish,
        refusal: refused.then(|| "the model refused to answer".to_owned()),
        usage: value.get("usage").and_then(usage),
        server_timings: None,
        model: value
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        first_token: None,
    })
}

/// Decodes a streamed Messages answer.
pub(crate) struct ChatStream {
    text: String,
    tool_json: String,
    stop: Option<String>,
    usage: Usage,
    has_usage: bool,
    model: Option<String>,
    done: bool,
    key: Option<SecretString>,
}

impl ChatStream {
    pub(crate) fn new(key: Option<&SecretString>) -> Self {
        Self {
            text: String::new(),
            tool_json: String::new(),
            stop: None,
            usage: Usage::default(),
            has_usage: false,
            model: None,
            done: false,
            key: key.cloned(),
        }
    }

    fn add_usage(&mut self, value: Option<&Value>) {
        let Some(value) = value.filter(|value| value.is_object()) else {
            return;
        };
        self.has_usage = true;
        if let Some(input) = count(value, "input_tokens") {
            let read = count(value, "cache_read_input_tokens").unwrap_or(0);
            let written = count(value, "cache_creation_input_tokens").unwrap_or(0);
            self.usage.input_tokens = input.saturating_add(read).saturating_add(written);
            self.usage.cached_input_tokens = read;
        }
        if let Some(output) = count(value, "output_tokens") {
            self.usage.output_tokens = output;
        }
    }
}

impl StreamDecoder for ChatStream {
    fn event(&mut self, event: &SseEvent) -> Result<Delta, AiError> {
        let value = event_json(event)?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .or(event.event.as_deref())
            .unwrap_or("");
        match kind {
            "message_start" => {
                let message = value.get("message").unwrap_or(&Value::Null);
                self.model = message
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.add_usage(message.get("usage"));
                Ok(Delta::None)
            }
            "content_block_start" => {
                let block = value.get("content_block").unwrap_or(&Value::Null);
                if let Some(text) = block
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    self.text.push_str(text);
                    return Ok(Delta::Output);
                }
                Ok(Delta::None)
            }
            "content_block_delta" => {
                let delta = value.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        self.text
                            .push_str(delta.get("text").and_then(Value::as_str).unwrap_or(""));
                        Ok(if self.tool_json.is_empty() {
                            Delta::Output
                        } else {
                            Delta::Token
                        })
                    }
                    Some("input_json_delta") => {
                        self.tool_json.push_str(
                            delta
                                .get("partial_json")
                                .and_then(Value::as_str)
                                .unwrap_or(""),
                        );
                        Ok(Delta::Output)
                    }
                    Some("thinking_delta" | "signature_delta") => Ok(Delta::Token),
                    _ => Ok(Delta::None),
                }
            }
            "message_delta" => {
                if let Some(reason) = value
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop = Some(reason.to_owned());
                }
                self.add_usage(value.get("usage"));
                Ok(Delta::None)
            }
            "message_stop" => {
                self.done = true;
                Ok(Delta::None)
            }
            "error" => {
                let error = from_error_value(
                    value.get("error").unwrap_or(&Value::Null),
                    self.key.as_ref(),
                );
                let hint = if error.kind().is_retryable() {
                    RetryHint::StreamBroken
                } else {
                    RetryHint::Normal
                };
                Err(error.with_hint(hint))
            }
            _ => Ok(Delta::None),
        }
    }

    fn output(&self) -> &str {
        if self.tool_json.is_empty() {
            &self.text
        } else {
            &self.tool_json
        }
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn finish(self: Box<Self>) -> Result<Turn, AiError> {
        if !self.done && self.stop.is_none() {
            return Err(
                AiError::transient("the stream ended before the answer finished")
                    .with_hint(RetryHint::StreamBroken),
            );
        }
        let (finish, refused) = self.stop.as_deref().map_or((None, false), |reason| {
            let (finish, refused) = stop_reason(reason);
            (Some(finish), refused)
        });
        let text = if self.tool_json.is_empty() {
            self.text
        } else {
            self.tool_json
        };
        Ok(Turn {
            text,
            finish,
            refusal: refused.then(|| "the model refused to answer".to_owned()),
            usage: self.has_usage.then_some(self.usage),
            server_timings: None,
            model: self.model,
            first_token: None,
        })
    }
}

/// The models of a `/v1/models` answer.
pub(crate) fn parse_models(body: &[u8]) -> Result<Vec<ModelInfo>, AiError> {
    crate::openai::parse_models(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use crate::request::JsonOutput;
    use crate::sse::SseParser;
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
            crate::ProviderKind::Anthropic,
            crate::Source::Operator,
            "http://localhost:1".parse().unwrap(),
        )
    }

    fn body_text(output: JsonOutput, mode: StructuredMode, stream: bool) -> String {
        let bytes = chat_body(&config(), &request_with(output), &[], Some(mode), stream);
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn output_config_carries_the_schema_in_the_files_order() {
        for (task, schema) in shared_schemas() {
            let text = body_text(output_of(&task, &schema), StructuredMode::JsonSchema, false);
            let expected = format!(
                r#""output_config":{{"format":{{"schema":{schema},"type":"json_schema"}}}}"#
            );
            assert!(text.contains(&expected), "{task}: {text}");
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
    fn a_forced_tool_carries_the_schema_in_the_files_order() {
        for (task, schema) in shared_schemas() {
            let name = task.replace('_', "-");
            let text = body_text(output_of(&task, &schema), StructuredMode::Tool, true);
            let expected = format!(
                r#""tools":[{{"description":"Record the answer. Its input is the whole answer.","eager_input_streaming":true,"input_schema":{schema},"name":"{name}","strict":true}}]"#
            );
            assert!(text.contains(&expected), "{task}: {text}");
            serde_json::from_str::<Value>(&text).unwrap();
        }
    }

    #[test]
    fn json_object_mode_puts_the_schema_in_the_prompt_in_the_files_order() {
        for (task, schema) in shared_schemas() {
            let text = body_text(output_of(&task, &schema), StructuredMode::JsonObject, false);
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                body["system"],
                format!("Catalog the post.\n\n{JSON_INSTRUCTION}\n{schema}"),
                "{task}"
            );
        }
    }

    #[test]
    fn the_other_members_keep_their_sorted_order() {
        let output = JsonOutput::new("t", json!({"type": "object"})).unwrap();
        let bytes = chat_body(
            &config(),
            &request_with(output),
            &[],
            Some(StructuredMode::JsonSchema),
            false,
        );
        assert_eq!(
            String::from_utf8(bytes.to_vec()).unwrap(),
            concat!(
                r#"{"max_tokens":4096,"messages":[{"content":"a lamp","role":"user"}],"model":"m","#,
                r#""output_config":{"format":{"schema":{"type":"object"},"type":"json_schema"}},"#,
                r#""system":"Catalog the post."}"#
            )
        );
    }

    fn decode(stream: &str) -> Result<Turn, AiError> {
        let mut decoder = Box::new(ChatStream::new(None));
        let mut parser = SseParser::new();
        for event in parser.push(stream.as_bytes()).unwrap() {
            decoder.event(&event)?;
        }
        decoder.finish()
    }

    #[test]
    fn a_forced_tool_answer_is_its_input() {
        let body = json!({
            "model": "claude-x",
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "catalog", "input": {"tags": ["a"]}}],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "cache_read_input_tokens": 5, "output_tokens": 7}
        });
        let turn = parse_chat(body.to_string().as_bytes(), None).unwrap();
        assert_eq!(turn.text, r#"{"tags":["a"]}"#);
        assert_eq!(turn.finish, Some(FinishReason::ToolUse));
        assert_eq!(
            turn.usage,
            Some(Usage {
                input_tokens: 15,
                output_tokens: 7,
                cached_input_tokens: 5
            })
        );
    }

    #[test]
    fn a_refusal_stop_is_a_refusal() {
        let body = json!({"content": [], "stop_reason": "refusal"});
        assert!(
            parse_chat(body.to_string().as_bytes(), None)
                .unwrap()
                .refusal
                .is_some()
        );
    }

    #[test]
    fn streamed_tool_input_joins_its_fragments() {
        let stream = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-x\",\"usage\":{\"input_tokens\":9,\"output_tokens\":1}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"catalog\",\"input\":{}}}\n\n",
            "event: ping\ndata: {\"type\":\"ping\"}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"tags\\\": [\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a\\\"]}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":12}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let turn = decode(stream).unwrap();
        assert_eq!(turn.text, r#"{"tags": ["a"]}"#);
        assert_eq!(turn.usage.unwrap().output_tokens, 12);
        assert_eq!(turn.usage.unwrap().input_tokens, 9);
        assert_eq!(turn.model.as_deref(), Some("claude-x"));
    }

    #[test]
    fn a_stream_error_event_maps_by_type() {
        let stream = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        let error = decode(stream).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Transient);
        assert_eq!(error.code(), Some("overloaded_error"));
    }

    #[test]
    fn a_stream_without_its_end_is_broken() {
        let stream = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n";
        assert_eq!(decode(stream).unwrap_err().kind(), ErrorKind::Transient);
    }
}
