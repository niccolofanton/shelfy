//! The stub's Anthropic Messages endpoints: `/v1/messages`, streamed or not,
//! with forced tools and `output_config.format`, and the model list.

use serde_json::{Value, json};

use super::answer::{self, request_key, schema_in_prompt};
use super::chat::{Ask, CHUNK_CHARS, Frame, Reply, Stop, Wanted, sse};

/// Reads a Messages request.
pub(crate) fn ask(body: &Value) -> Ask {
    let system = match body.get("system") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    };
    let mut texts = Vec::new();
    let mut images = 0;
    let mut input_chars = system.chars().count();
    for message in body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (text, message_images) = match message.get("content") {
            Some(Value::String(text)) => (text.clone(), 0),
            Some(Value::Array(blocks)) => {
                let mut parts = Vec::new();
                let mut count = 0;
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => parts.push(
                            block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                        ),
                        Some("image") => count += 1,
                        _ => {}
                    }
                }
                (parts.join("\n\n"), count)
            }
            _ => (String::new(), 0),
        };
        images += message_images;
        input_chars += text.chars().count();
        texts.push(text);
    }
    let forced = body
        .get("tool_choice")
        .filter(|choice| choice.get("type").and_then(Value::as_str) == Some("tool"))
        .and_then(|choice| choice.get("name"))
        .and_then(Value::as_str);
    let tool = forced.and_then(|name| {
        body.get("tools")?
            .as_array()?
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
    });
    let format = body
        .get("output_config")
        .and_then(|config| config.get("format"))
        .filter(|format| format.get("type").and_then(Value::as_str) == Some("json_schema"));
    let wanted = if let Some(tool) = tool {
        Wanted::Tool {
            name: tool
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_owned(),
            schema: tool
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"})),
        }
    } else if let Some(format) = format {
        Wanted::Json(format.get("schema").cloned())
    } else if let Some(schema) = schema_in_prompt(&system) {
        Wanted::Json(Some(schema))
    } else {
        Wanted::Text
    };
    Ask {
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("stub")
            .to_owned(),
        key: request_key(Some(&system), texts.iter().map(String::as_str)),
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        wanted,
        max_tokens: body.get("max_tokens").and_then(Value::as_u64),
        input_chars,
        images,
        include_usage: false,
    }
}

fn stop_reason(ask: &Ask, reply: &Reply) -> &'static str {
    match (reply.stop, &ask.wanted) {
        (Stop::Refusal, _) => "refusal",
        (Stop::Length, _) => "max_tokens",
        (Stop::Done, Wanted::Tool { .. }) => "tool_use",
        (Stop::Done, _) => "end_turn",
    }
}

/// The forced tool's input: the reply's JSON, or the reply as `{"text"}`
/// when it is not an object.
fn tool_input(reply: &Reply) -> Value {
    match serde_json::from_str::<Value>(&reply.text) {
        Ok(value @ Value::Object(_)) => value,
        _ if reply.text.is_empty() => json!({}),
        _ => json!({"text": reply.text}),
    }
}

fn id(ask: &Ask) -> String {
    format!("msg_stub_{}", &ask.key[..12])
}

/// The answer of a Messages request that is not streamed.
pub(crate) fn message(ask: &Ask, reply: &Reply) -> Value {
    let content = match (&ask.wanted, reply.stop) {
        (_, Stop::Refusal) => json!([]),
        (Wanted::Tool { name, .. }, _) => {
            json!([{"type": "tool_use", "id": "toolu_stub", "name": name, "input": tool_input(reply)}])
        }
        _ if reply.text.is_empty() => json!([]),
        _ => json!([{"type": "text", "text": reply.text}]),
    };
    json!({
        "id": id(ask),
        "type": "message",
        "role": "assistant",
        "model": ask.model,
        "content": content,
        "stop_reason": stop_reason(ask, reply),
        "stop_sequence": null,
        "usage": {
            "input_tokens": reply.input_tokens,
            "output_tokens": reply.output_tokens,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
        },
    })
}

/// The frames of a streamed Messages answer. `malformed` replaces the content
/// with an event that is not JSON.
pub(crate) fn message_stream(ask: &Ask, reply: &Reply, malformed: bool) -> Vec<Frame> {
    let event = |name: &str, data: Value| Frame::now(sse(Some(name), &data.to_string()));
    let mut frames = vec![
        event(
            "message_start",
            json!({"type": "message_start", "message": {
                "id": id(ask), "type": "message", "role": "assistant", "model": ask.model,
                "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": reply.input_tokens, "output_tokens": 1,
                          "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
            }}),
        ),
        event("ping", json!({"type": "ping"})),
    ];
    if malformed {
        frames.push(Frame::now(sse(
            Some("content_block_delta"),
            "{\"type\": \"content_block_delta\", ",
        )));
    } else if reply.stop != Stop::Refusal && !reply.text.is_empty() {
        let (block, delta_type, field) = match &ask.wanted {
            Wanted::Tool { name, .. } => (
                json!({"type": "tool_use", "id": "toolu_stub", "name": name, "input": {}}),
                "input_json_delta",
                "partial_json",
            ),
            _ => (json!({"type": "text", "text": ""}), "text_delta", "text"),
        };
        frames.push(event(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": block}),
        ));
        let text = match &ask.wanted {
            Wanted::Tool { .. } => tool_input(reply).to_string(),
            _ => reply.text.clone(),
        };
        for piece in answer::chunks(&text, CHUNK_CHARS) {
            frames.push(event(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": delta_type, field: piece}}),
            ));
        }
        frames.push(event(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        ));
    }
    frames.push(event(
        "message_delta",
        json!({"type": "message_delta",
               "delta": {"stop_reason": stop_reason(ask, reply), "stop_sequence": null},
               "usage": {"output_tokens": reply.output_tokens}}),
    ));
    frames.push(event("message_stop", json!({"type": "message_stop"})));
    frames
}

/// The model list.
pub(crate) fn models(models: &[String]) -> Value {
    let data: Vec<Value> = models
        .iter()
        .map(|id| json!({"type": "model", "id": id, "display_name": id, "created_at": "2026-01-01T00:00:00Z"}))
        .collect();
    json!({
        "data": data,
        "has_more": false,
        "first_id": models.first(),
        "last_id": models.last(),
    })
}

/// An error body.
pub(crate) fn error(message: &str, kind: &str) -> Value {
    json!({"type": "error", "error": {"type": kind, "message": message}, "request_id": "req_stub"})
}
