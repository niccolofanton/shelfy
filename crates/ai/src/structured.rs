//! Structured output (plan §2.15): how a provider is asked for JSON, and how
//! the answer is checked.
//!
//! Every JSON answer is validated against its schema with `jsonschema`,
//! whatever the mode: a server that claims strictness may still drift. An
//! answer that is not JSON, or does not match, gets one repair call that
//! quotes the validation error; if the repair fails too, the call ends with
//! [`crate::ErrorKind::SchemaInvalid`]. An answer cut by the token cap gets no
//! repair: the same cap would cut it again.
//!
//! The mode comes from the provider's preset or the operator's settings
//! ([`crate::ProviderConfig::structured`]) and can be overridden per call
//! ([`crate::CallOptions::structured`]).

use serde::Serialize;
use serde_json::Value;

use crate::request::JsonOutput;

/// How a provider is asked for JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredMode {
    /// A schema the server enforces: `response_format` of type `json_schema`
    /// with `strict` (OpenAI-compatible), `output_config.format` (Anthropic).
    JsonSchema,
    /// The schema in the system prompt, and `response_format` of type
    /// `json_object` on OpenAI-compatible servers (Anthropic has no JSON mode:
    /// the prompt alone).
    JsonObject,
    /// Anthropic only: one forced tool whose `input_schema` is the schema; the
    /// answer is the tool's input.
    Tool,
}

/// What the system prompt gains in [`StructuredMode::JsonObject`]: this
/// sentence, a newline and the compact schema. The stub finds the schema
/// after it.
pub const JSON_INSTRUCTION: &str = "Answer with one JSON object that matches this JSON schema:";

/// `system` with the JSON instruction and `schema` appended.
pub(crate) fn with_json_instruction(system: Option<&str>, schema: &str) -> String {
    let instruction = format!("{JSON_INSTRUCTION}\n{schema}");
    match system {
        Some(system) if !system.is_empty() => format!("{system}\n\n{instruction}"),
        _ => instruction,
    }
}

/// Why an answer failed its schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Invalid {
    /// For the repair call: the full validation error (it may quote the
    /// answer, which goes back to the same provider).
    pub(crate) for_model: String,
    /// For logs: the JSON pointer and the failed keyword, no value.
    pub(crate) for_log: String,
}

/// Parses and validates `text` against `output`.
pub(crate) fn validate(output: &JsonOutput, text: &str) -> Result<Value, Invalid> {
    let trimmed = strip_code_fence(text.trim());
    if trimmed.is_empty() {
        return Err(Invalid {
            for_model: "the answer was empty".into(),
            for_log: "the answer is empty".into(),
        });
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|error| Invalid {
        for_model: format!("it is not valid JSON ({error})"),
        for_log: format!(
            "the answer is not JSON (line {}, column {})",
            error.line(),
            error.column()
        ),
    })?;
    let errors: Vec<_> = output
        .validator()
        .iter_errors(&value)
        .take(MAX_QUOTED_ERRORS)
        .collect();
    let Some(first) = errors.first() else {
        return Ok(value);
    };
    let at = |error: &jsonschema::ValidationError<'_>| {
        let path = error.instance_path().to_string();
        if path.is_empty() {
            "/".to_owned()
        } else {
            path
        }
    };
    let for_model = errors
        .iter()
        .map(|error| format!("at {}: {error}", at(error)))
        .collect::<Vec<_>>()
        .join("; ");
    Err(Invalid {
        for_model,
        for_log: format!(
            "the answer does not match the schema at {}: {}",
            at(first),
            first.masked()
        ),
    })
}

/// How many validation errors the repair call quotes.
const MAX_QUOTED_ERRORS: usize = 3;

/// Drops a Markdown code fence around a JSON answer (prompt-only modes).
pub(crate) fn strip_code_fence(text: &str) -> &str {
    let Some(inner) = text.strip_prefix("```") else {
        return text;
    };
    let inner = inner.strip_prefix("json").unwrap_or(inner);
    inner.strip_suffix("```").map_or(text, str::trim)
}

/// The user turn of the repair call.
pub(crate) fn repair_prompt(invalid: &Invalid) -> String {
    format!(
        "Your previous answer does not match the required JSON schema: {}. \
         Answer again with only the corrected JSON object.",
        invalid.for_model
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn output() -> JsonOutput {
        JsonOutput::new(
            "catalog",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "description": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["description", "tags"]
            }),
        )
        .unwrap()
    }

    #[test]
    fn valid_answers_parse() {
        let value = validate(
            &output(),
            r#" {"description": "a lamp", "tags": ["glass"]} "#,
        )
        .unwrap();
        assert_eq!(value["tags"][0], "glass");
        let fenced = "```json\n{\"description\": \"x\", \"tags\": []}\n```";
        assert!(validate(&output(), fenced).is_ok());
    }

    #[test]
    fn invalid_answers_name_the_path_without_the_value_in_logs() {
        let invalid = validate(
            &output(),
            r#"{"description": "secret caption", "tags": [7]}"#,
        )
        .unwrap_err();
        assert!(invalid.for_model.contains("/tags/0"), "{invalid:?}");
        assert!(invalid.for_log.contains("/tags/0"), "{invalid:?}");
        assert!(!invalid.for_log.contains("secret caption"));
        assert!(!invalid.for_log.contains('7'), "{invalid:?}");

        let missing = validate(&output(), r#"{"description": "x"}"#).unwrap_err();
        assert!(missing.for_model.contains("tags"), "{missing:?}");

        let not_json = validate(&output(), "the lamp is nice").unwrap_err();
        assert!(not_json.for_model.contains("not valid JSON"));
        assert!(!not_json.for_log.contains("lamp"));

        assert_eq!(
            validate(&output(), "  ").unwrap_err().for_log,
            "the answer is empty"
        );
    }

    #[test]
    fn the_instruction_appends_the_compact_schema() {
        let schema = json!({"type": "object"});
        assert_eq!(
            with_json_instruction(Some("Catalog the post."), &schema.to_string()),
            format!("Catalog the post.\n\n{JSON_INSTRUCTION}\n{{\"type\":\"object\"}}")
        );
        assert_eq!(
            with_json_instruction(None, &schema.to_string()),
            format!("{JSON_INSTRUCTION}\n{{\"type\":\"object\"}}")
        );
    }

    #[test]
    fn the_repair_prompt_quotes_the_error() {
        let invalid = validate(&output(), "{}").unwrap_err();
        let prompt = repair_prompt(&invalid);
        assert!(prompt.contains("does not match the required JSON schema"));
        assert!(prompt.contains("description"), "{prompt}");
    }
}
