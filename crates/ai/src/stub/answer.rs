//! What the stub answers: a canned answer for the request's key, a recorded
//! one, or a deterministic one (an instance of the request's schema, or a
//! short text).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::request::ChatRequest;

/// A request's key: the lowercase hex SHA-256 of its system prompt and, for
/// each message in order, its text parts joined with blank lines, each
/// preceded by a NUL byte. Images do not count. The JSON instruction that
/// [`crate::StructuredMode::JsonObject`] appends to the system prompt does not
/// count either, so one key serves every mode.
#[must_use]
pub fn request_key<'a>(
    system: Option<&str>,
    messages: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(strip_json_instruction(system.unwrap_or("")).as_bytes());
    for message in messages {
        hasher.update([0]);
        hasher.update(message.as_bytes());
    }
    hex(&hasher.finalize())
}

/// The key the stub computes for `request` (see [`request_key`]).
#[must_use]
pub fn key_of(request: &ChatRequest) -> String {
    let texts: Vec<String> = request
        .messages
        .iter()
        .map(crate::request::Message::text)
        .collect();
    request_key(request.system.as_deref(), texts.iter().map(String::as_str))
}

/// The lowercase hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `system` without the JSON instruction and what follows it.
pub(crate) fn strip_json_instruction(system: &str) -> &str {
    match system.find(crate::structured::JSON_INSTRUCTION) {
        Some(at) => system[..at].trim_end_matches('\n'),
        None => system,
    }
}

/// The schema after the JSON instruction in `system`, if any.
pub(crate) fn schema_in_prompt(system: &str) -> Option<Value> {
    let at = system.find(crate::structured::JSON_INSTRUCTION)?;
    let rest = &system[at + crate::structured::JSON_INSTRUCTION.len()..];
    serde_json::from_str(rest.trim()).ok()
}

/// A recorded answer, replayed for its key from a recordings directory as
/// `<key>.json`. Recordings of real runs stay outside the repository (P3
/// lane rule 4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recording {
    /// The answer's text (the JSON text for JSON answers).
    pub text: String,
}

/// Writes `recording` for `key` into `dir`.
///
/// # Errors
///
/// Any I/O error; a key that is not 64 hex digits is `InvalidInput`.
pub fn write_recording(dir: &Path, key: &str, recording: &Recording) -> io::Result<()> {
    let path = recording_path(dir, key)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad key"))?;
    std::fs::write(path, serde_json::to_vec_pretty(recording)?)
}

fn recording_path(dir: &Path, key: &str) -> Option<PathBuf> {
    (key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| dir.join(format!("{key}.json")))
}

/// The answers the stub knows by key.
#[derive(Debug, Default)]
pub(crate) struct Answers {
    pub(crate) canned: HashMap<String, String>,
    pub(crate) recordings: Option<PathBuf>,
}

impl Answers {
    /// The canned or recorded answer of `key`.
    pub(crate) fn known(&self, key: &str) -> Option<String> {
        if let Some(text) = self.canned.get(key) {
            return Some(text.clone());
        }
        let path = recording_path(self.recordings.as_deref()?, key)?;
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice::<Recording>(&bytes)
            .ok()
            .map(|recording| recording.text)
    }
}

/// The default text answer for `key`.
pub(crate) fn text_answer(key: &str) -> String {
    format!("Stub answer {}.", &key[..12.min(key.len())])
}

/// A deterministic instance of `schema`, seeded by `key`: required
/// properties (all of them for closed objects), the first enum value, two
/// array items unless the bounds say otherwise, strings that respect length
/// bounds and common formats.
pub fn schema_example(schema: &Value, key: &str) -> Value {
    Example {
        root: schema,
        seed: key,
    }
    .of(schema, 0)
}

struct Example<'a> {
    root: &'a Value,
    seed: &'a str,
}

impl Example<'_> {
    fn of(&self, schema: &Value, depth: u32) -> Value {
        if depth > 16 {
            return Value::Null;
        }
        let Some(object) = schema.as_object() else {
            // `true` or an empty schema: anything goes.
            return json!("stub");
        };
        if let Some(reference) = object.get("$ref").and_then(Value::as_str)
            && let Some(target) = self.resolve(reference)
        {
            return self.of(target, depth + 1);
        }
        if let Some(value) = object.get("const") {
            return value.clone();
        }
        if let Some(first) = object
            .get("enum")
            .and_then(Value::as_array)
            .and_then(|values| values.first())
        {
            return first.clone();
        }
        for combinator in ["anyOf", "oneOf", "allOf"] {
            if let Some(first) =
                object
                    .get(combinator)
                    .and_then(Value::as_array)
                    .and_then(|schemas| {
                        schemas.iter().find(|schema| {
                            schema.get("type").and_then(Value::as_str) != Some("null")
                        })
                    })
            {
                return self.of(first, depth + 1);
            }
        }
        let kind = match object.get("type") {
            Some(Value::String(kind)) => kind.as_str(),
            Some(Value::Array(kinds)) => kinds
                .iter()
                .filter_map(Value::as_str)
                .find(|kind| *kind != "null")
                .unwrap_or("null"),
            _ if object.contains_key("properties") => "object",
            _ if object.contains_key("items") => "array",
            _ => "string",
        };
        match kind {
            "object" => self.object(object, depth),
            "array" => self.array(object, depth),
            "integer" => json!(bound(object, 0.0).round() as i64),
            "number" => json!(bound(object, 0.5)),
            "boolean" => json!(false),
            "null" => Value::Null,
            _ => json!(self.string(object)),
        }
    }

    fn object(&self, object: &Map<String, Value>, depth: u32) -> Value {
        let empty = Map::new();
        let properties = object
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut out = Map::new();
        for (name, schema) in properties {
            out.insert(name.clone(), self.of(schema, depth + 1));
        }
        for name in required {
            out.entry(name.to_owned()).or_insert_with(|| json!("stub"));
        }
        Value::Object(out)
    }

    fn array(&self, object: &Map<String, Value>, depth: u32) -> Value {
        let min = object.get("minItems").and_then(Value::as_u64).unwrap_or(0);
        let max = object
            .get("maxItems")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let count = min.max(2).min(max);
        let items = object.get("items").cloned().unwrap_or(Value::Bool(true));
        let unique = object
            .get("uniqueItems")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        (0..count)
            .map(|n| {
                if let Some(values) = items
                    .get("enum")
                    .and_then(Value::as_array)
                    .filter(|v| !v.is_empty())
                {
                    return values[(n as usize) % values.len()].clone();
                }
                let value = self.of(&items, depth + 1);
                match value {
                    Value::String(text) if unique || n > 0 => json!(format!("{text}-{}", n + 1)),
                    other => other,
                }
            })
            .collect()
    }

    fn string(&self, object: &Map<String, Value>) -> String {
        let text = match object.get("format").and_then(Value::as_str) {
            Some("date-time") => "2026-01-01T00:00:00Z".to_owned(),
            Some("date") => "2026-01-01".to_owned(),
            Some("time") => "00:00:00Z".to_owned(),
            Some("email") => "stub@example.com".to_owned(),
            Some("uri" | "url") => "https://example.com/stub".to_owned(),
            Some("uuid") => "00000000-0000-4000-8000-000000000000".to_owned(),
            _ => format!("stub {}", &self.seed[..8.min(self.seed.len())]),
        };
        let min = object.get("minLength").and_then(Value::as_u64).unwrap_or(0);
        let max = object
            .get("maxLength")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let mut text: String = text
            .chars()
            .take(usize::try_from(max).unwrap_or(usize::MAX))
            .collect();
        while (text.chars().count() as u64) < min {
            text.push('x');
        }
        text
    }

    fn resolve(&self, reference: &str) -> Option<&Value> {
        let pointer = reference.strip_prefix('#')?;
        self.root.pointer(pointer)
    }
}

/// A number inside the schema's bounds, near `preferred`.
fn bound(object: &Map<String, Value>, preferred: f64) -> f64 {
    let min = object
        .get("minimum")
        .or_else(|| object.get("exclusiveMinimum"))
        .and_then(Value::as_f64);
    let max = object
        .get("maximum")
        .or_else(|| object.get("exclusiveMaximum"))
        .and_then(Value::as_f64);
    match (min, max) {
        (Some(min), Some(max)) => (min + max) / 2.0,
        (Some(min), None) => min + 1.0,
        (None, Some(max)) => max - 1.0,
        (None, None) => preferred,
    }
}

/// A deterministic unit vector of `dims` for `text`.
pub(crate) fn embedding(text: &str, dims: usize) -> Vec<f32> {
    let mut values = Vec::with_capacity(dims);
    let mut block = 0_u32;
    while values.len() < dims {
        let digest = Sha256::new()
            .chain_update(block.to_be_bytes())
            .chain_update(text.as_bytes())
            .finalize();
        for pair in digest.chunks(2) {
            if values.len() == dims {
                break;
            }
            let raw = u16::from_be_bytes([pair[0], pair[1]]);
            values.push(f32::from(raw) / f32::from(u16::MAX) * 2.0 - 1.0);
        }
        block += 1;
    }
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut values {
            *value /= norm;
        }
    }
    values
}

/// Rough token counts: four characters a token, at least one.
pub(crate) fn tokens(chars: usize) -> u64 {
    u64::try_from(chars.div_ceil(4).max(1)).unwrap_or(u64::MAX)
}

/// `text` in chunks of about `size` characters, on character boundaries.
pub(crate) fn chunks(text: &str, size: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(size.max(1))
        .map(|chunk| chunk.iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_match_their_schema() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "description": {"type": "string", "minLength": 20},
                "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 3, "uniqueItems": true},
                "purpose": {"type": "string", "enum": ["portfolio", "other"]},
                "score": {"type": "integer", "minimum": 1, "maximum": 5},
                "maybe": {"type": ["string", "null"]},
                "nested": {"$ref": "#/$defs/thing"}
            },
            "required": ["description", "tags", "purpose", "score", "maybe", "nested"],
            "$defs": {"thing": {"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]}}
        });
        let value = schema_example(&schema, "0123456789abcdef");
        assert!(jsonschema::is_valid(&schema, &value), "{value}");
        assert_eq!(value["purpose"], "portfolio");
        assert_eq!(value["tags"].as_array().unwrap().len(), 2);
        assert_eq!(schema_example(&schema, "0123456789abcdef"), value);
    }

    #[test]
    fn keys_ignore_the_json_instruction() {
        let plain = request_key(Some("Catalog."), ["caption"]);
        let instructed = format!(
            "Catalog.\n\n{}\n{{\"type\":\"object\"}}",
            crate::structured::JSON_INSTRUCTION
        );
        assert_eq!(request_key(Some(&instructed), ["caption"]), plain);
        assert_ne!(request_key(Some("Catalog."), ["caption", ""]), plain);
        assert_eq!(plain.len(), 64);
    }

    #[test]
    fn embeddings_are_deterministic_unit_vectors() {
        let vector = embedding("lamp", 24);
        assert_eq!(vector.len(), 24);
        let norm: f32 = vector.iter().map(|value| value * value).sum();
        assert!((norm - 1.0).abs() < 1e-4);
        assert_eq!(embedding("lamp", 24), vector);
        assert_ne!(embedding("chair", 24), vector);
    }
}

#[cfg(test)]
mod array_enum_regression {
    use super::*;
    #[test]
    fn closed_vocabulary_arrays_never_invent_suffixed_enum_values() {
        let schema = json!({"type":"array","items":{"type":"string","enum":["minimal","editorial"]},"minItems":1,"maxItems":3});
        let example = schema_example(&schema, "test");
        assert_eq!(example, json!(["minimal", "editorial"]));
        assert!(
            jsonschema::validator_for(&schema)
                .unwrap()
                .is_valid(&example)
        );
    }
}
