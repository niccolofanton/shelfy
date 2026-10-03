//! The stub's OpenAI-compatible endpoints: chat completions (with llama.cpp's
//! `timings`), embeddings and the model list.

use serde_json::{Value, json};

use super::answer::{self, request_key, schema_in_prompt};
use super::chat::{Ask, CHUNK_CHARS, Frame, REFUSAL, Reply, Stop, Wanted, sse};

/// Reads a chat completion request.
pub(crate) fn ask(body: &Value) -> Ask {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut system = Vec::new();
    let mut texts = Vec::new();
    let mut images = 0;
    let mut input_chars = 0;
    for message in &messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let (text, message_images) = content(message.get("content").unwrap_or(&Value::Null));
        images += message_images;
        input_chars += text.chars().count();
        if role == "system" || role == "developer" {
            system.push(text);
        } else {
            texts.push(text);
        }
    }
    let system = system.join("\n\n");
    let format = body.get("response_format");
    let wanted = match format
        .and_then(|format| format.get("type"))
        .and_then(Value::as_str)
    {
        Some("json_schema") => Wanted::Json(
            format
                .and_then(|format| format.get("json_schema"))
                .and_then(|spec| spec.get("schema"))
                .cloned(),
        ),
        Some("json_object") => Wanted::Json(schema_in_prompt(&system)),
        _ => match schema_in_prompt(&system) {
            Some(schema) => Wanted::Json(Some(schema)),
            None => Wanted::Text,
        },
    };
    let max_tokens = body
        .get("max_completion_tokens")
        .or_else(|| body.get("max_tokens"))
        .and_then(Value::as_u64);
    Ask {
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("stub")
            .to_owned(),
        key: request_key(Some(&system), texts.iter().map(String::as_str)),
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        wanted,
        max_tokens,
        input_chars,
        images,
        include_usage: body
            .get("stream_options")
            .and_then(|options| options.get("include_usage"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// A message content's text (parts joined with blank lines) and image count.
fn content(content: &Value) -> (String, usize) {
    match content {
        Value::String(text) => (text.clone(), 0),
        Value::Array(parts) => {
            let mut texts = Vec::new();
            let mut images = 0;
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => texts.push(
                        part.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    ),
                    Some("image_url") => images += 1,
                    _ => {}
                }
            }
            (texts.join("\n\n"), images)
        }
        _ => (String::new(), 0),
    }
}

fn finish(stop: Stop) -> &'static str {
    match stop {
        Stop::Done | Stop::Refusal => "stop",
        Stop::Length => "length",
    }
}

fn usage(reply: &Reply) -> Value {
    json!({
        "prompt_tokens": reply.input_tokens,
        "completion_tokens": reply.output_tokens,
        "total_tokens": reply.input_tokens + reply.output_tokens,
    })
}

fn timings(reply: &Reply) -> Value {
    json!({
        "cache_n": 0,
        "prompt_n": reply.input_tokens,
        "prompt_ms": 1.0,
        "predicted_n": reply.output_tokens,
        "predicted_ms": 1.0,
    })
}

fn id(ask: &Ask) -> String {
    format!("chatcmpl-stub-{}", &ask.key[..12])
}

/// The answer of a chat completion that is not streamed.
pub(crate) fn completion(ask: &Ask, reply: &Reply) -> Value {
    let message = if reply.stop == Stop::Refusal {
        json!({"role": "assistant", "content": null, "refusal": REFUSAL})
    } else {
        json!({"role": "assistant", "content": reply.text, "refusal": null})
    };
    json!({
        "id": id(ask),
        "object": "chat.completion",
        "created": 0,
        "model": ask.model,
        "choices": [{"index": 0, "message": message, "finish_reason": finish(reply.stop)}],
        "usage": usage(reply),
        "timings": timings(reply),
    })
}

/// The frames of a streamed chat completion. `malformed` replaces the
/// content with a chunk that is not JSON.
pub(crate) fn completion_stream(ask: &Ask, reply: &Reply, malformed: bool) -> Vec<Frame> {
    let chunk = |delta: Value, finish: Option<&str>| {
        json!({
            "id": id(ask),
            "object": "chat.completion.chunk",
            "created": 0,
            "model": ask.model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    };
    let mut frames = vec![Frame::now(sse(
        None,
        &chunk(json!({"role": "assistant", "content": ""}), None).to_string(),
    ))];
    if malformed {
        frames.push(Frame::now(sse(
            None,
            "{\"choices\": [{\"delta\": {\"content\": ",
        )));
    } else if reply.stop == Stop::Refusal {
        frames.push(Frame::now(sse(
            None,
            &chunk(json!({"refusal": REFUSAL}), None).to_string(),
        )));
    } else {
        for piece in answer::chunks(&reply.text, CHUNK_CHARS) {
            frames.push(Frame::now(sse(
                None,
                &chunk(json!({"content": piece}), None).to_string(),
            )));
        }
    }
    let mut last = chunk(json!({}), Some(finish(reply.stop)));
    last["timings"] = timings(reply);
    frames.push(Frame::now(sse(None, &last.to_string())));
    if ask.include_usage {
        let usage = json!({
            "id": id(ask),
            "object": "chat.completion.chunk",
            "created": 0,
            "model": ask.model,
            "choices": [],
            "usage": usage(reply),
        });
        frames.push(Frame::now(sse(None, &usage.to_string())));
    }
    frames.push(Frame::now(sse(None, "[DONE]")));
    frames
}

/// The answer of an embeddings request.
pub(crate) fn embeddings(body: &Value, dims: usize) -> Result<Value, &'static str> {
    let inputs: Vec<String> = match body.get("input") {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or("input must hold strings")?,
        _ => return Err("input is required"),
    };
    let dims = body
        .get("dimensions")
        .and_then(Value::as_u64)
        .and_then(|dims| usize::try_from(dims).ok())
        .unwrap_or(dims);
    let tokens: u64 = inputs
        .iter()
        .map(|text| answer::tokens(text.chars().count()))
        .sum();
    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(index, text)| json!({"object": "embedding", "index": index, "embedding": answer::embedding(text, dims)}))
        .collect();
    Ok(json!({
        "object": "list",
        "data": data,
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "usage": {"prompt_tokens": tokens, "total_tokens": tokens},
    }))
}

/// The model list.
pub(crate) fn models(models: &[String]) -> Value {
    let data: Vec<Value> = models
        .iter()
        .map(|id| json!({"id": id, "object": "model", "created": 0, "owned_by": "shelfy-ai-stub"}))
        .collect();
    json!({"object": "list", "data": data})
}

/// An error body.
pub(crate) fn error(message: &str, kind: &str, code: &str) -> Value {
    json!({"error": {"message": message, "type": kind, "param": null, "code": code}})
}
