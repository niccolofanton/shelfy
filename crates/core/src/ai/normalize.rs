//! The normalization of catalog answers (plan §2.15; AI-08, AI-09): a model's
//! answer, checked against its response schema, becomes the post's AI layer.
//!
//! - [`catalog`] validates the whole answer first: one that does not match the
//!   schema is an [`OutputError`] and yields nothing, so nothing is half-written.
//! - The normalization is the desktop's (`normalizeCatalogOutput` in
//!   `shared/ai/catalog.ts`): two tiers of tags, deduplicated and capped by the
//!   manifest (general ≤ 3, specific ≤ 7, flat ≤ 10), trimmed texts, and for a
//!   website the closed-enum purpose and industry as the content type and the
//!   category. Golden: `shared/golden/ai/catalog/{clean-strings,normalize}.jsonl`.
//! - [`Catalog::into_patch`] is the [`AiPatch`] of a finished analysis: status
//!   `done`, provider, model, [`SCHEMA_VERSION`] and the tiers. Golden against
//!   the desktop's write of the same answer: `ai/catalog/apply.jsonl`.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use serde::Serialize;
use serde_json::Value;

use super::catalog::CatalogKind;
use super::prompts::{self, SCHEMA_VERSION, Task};
use crate::repo::posts::AiPatch;
use crate::search::terms::js_trim;

/// An answer that cannot become an AI layer. Its messages name the schema
/// path, never the answer's text (which can echo a caption).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
    /// The answer is not JSON.
    #[error("the answer is not JSON")]
    NotJson,
    /// The answer does not match the response schema.
    #[error("the answer does not match the {schema} schema at {path:?}: {message}")]
    SchemaInvalid {
        /// The schema's name.
        schema: &'static str,
        /// JSON pointer of the first mismatch in the answer (`""` for the root).
        path: String,
        /// What is wrong, with the answer's values masked.
        message: String,
    },
}

impl OutputError {
    /// The code stored in `ai_error` and reported to clients.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "schema_invalid"
    }
}

/// A normalized catalog answer, as the archive stores it. Serialized like the
/// desktop's `normalizeCatalogOutput` result without `modelUsed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    /// What the post shows, as given.
    pub description: String,
    /// General then specific tags, deduplicated (feeds `ai_tags_json`).
    pub tags: Vec<String>,
    /// Theme tags, lowercased.
    pub general_tags: Vec<String>,
    /// Detail tags, lowercased.
    pub specific_tags: Vec<String>,
    /// Names in their own casing.
    pub entities: Vec<String>,
    /// Search queries in their own casing.
    pub keywords: Vec<String>,
    /// Why the post is worth keeping.
    pub save_reason: String,
    /// The language of the caption or page.
    pub language: String,
    /// Websites: the purpose (`ai_content_type`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Websites: the industry (`ai_category`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

static VALIDATORS: LazyLock<HashMap<Task, jsonschema::Validator>> = LazyLock::new(|| {
    [Task::Catalog, Task::WebCatalog]
        .into_iter()
        .map(|task| {
            let schema = prompts::response_schema(task).expect("catalog tasks have a schema");
            let validator =
                jsonschema::validator_for(&schema.value).expect("the catalog schemas compile");
            (task, validator)
        })
        .collect()
});

/// Checks `answer` against the response schema of `kind`.
///
/// # Errors
///
/// [`OutputError::SchemaInvalid`] with the first mismatch.
pub fn validate(kind: CatalogKind, answer: &Value) -> Result<(), OutputError> {
    let task = kind.task();
    let validator = &VALIDATORS[&task];
    match validator.iter_errors(answer).next() {
        None => Ok(()),
        Some(error) => Err(OutputError::SchemaInvalid {
            schema: prompts::response_schema(task)
                .expect("catalog tasks have a schema")
                .name,
            path: error.instance_path().to_string(),
            message: error.masked().to_string(),
        }),
    }
}

/// Normalizes a string array like the desktop's `cleanStringArray`: trims,
/// drops empties, dedupes case-insensitively in order, lowercases unless
/// `keep_case`, and stops once it holds `cap` items (checked after each push,
/// so a `cap` of 0 still keeps the first item, as on the desktop).
#[must_use]
pub fn clean_string_array<'a>(
    items: impl IntoIterator<Item = &'a str>,
    keep_case: bool,
    cap: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for item in items {
        let trimmed = js_trim(item);
        if trimmed.is_empty() {
            continue;
        }
        let norm = if keep_case {
            trimmed.to_owned()
        } else {
            trimmed.to_lowercase()
        };
        if !seen.insert(norm.to_lowercase()) {
            continue;
        }
        out.push(norm);
        if out.len() >= cap {
            break;
        }
    }
    out
}

fn strings<'a>(answer: &'a Value, field: &str) -> Vec<&'a str> {
    answer[field]
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn text<'a>(answer: &'a Value, field: &str) -> &'a str {
    answer[field].as_str().unwrap_or_default()
}

/// Validates and normalizes a catalog answer.
///
/// # Errors
///
/// [`OutputError::SchemaInvalid`] when the answer does not match the schema of
/// `kind`; nothing is normalized then.
pub fn catalog(kind: CatalogKind, answer: &Value) -> Result<Catalog, OutputError> {
    validate(kind, answer)?;
    let caps = prompts::spec(kind.task())
        .caps
        .expect("catalog tasks have caps (prompts tests)");
    let general_tags = clean_string_array(strings(answer, "general_tags"), false, caps.general);
    let specific_tags = clean_string_array(strings(answer, "specific_tags"), false, caps.specific);
    let tags = clean_string_array(
        general_tags
            .iter()
            .chain(&specific_tags)
            .map(String::as_str),
        false,
        caps.tags,
    );
    let mut catalog = Catalog {
        description: text(answer, "description").to_owned(),
        tags,
        general_tags,
        specific_tags,
        entities: clean_string_array(strings(answer, "entities"), true, usize::MAX),
        keywords: clean_string_array(strings(answer, "search_keywords"), true, usize::MAX),
        save_reason: js_trim(text(answer, "save_reason")).to_owned(),
        language: js_trim(text(answer, "language")).to_owned(),
        content_type: None,
        category: None,
    };
    if kind == CatalogKind::Web {
        let some = |field| Some(js_trim(text(answer, field)).to_owned()).filter(|s| !s.is_empty());
        catalog.content_type = some("purpose");
        catalog.category = some("industry");
    }
    Ok(catalog)
}

/// [`catalog`] on the model's text.
///
/// # Errors
///
/// [`OutputError::NotJson`] when `text` does not parse; else as [`catalog`].
pub fn parse_catalog(kind: CatalogKind, text: &str) -> Result<Catalog, OutputError> {
    let answer: Value = serde_json::from_str(text).map_err(|_| OutputError::NotJson)?;
    catalog(kind, &answer)
}

impl Catalog {
    /// The AI patch of a finished analysis that produced this catalog: status
    /// `done` (which stamps `ai_analyzed_at`), the provider and model, the
    /// output schema version, no error, and every field, the tag tiers
    /// included. A social post leaves the category and content type untouched,
    /// as on the desktop.
    #[must_use]
    pub fn into_patch(self, provider: &str, model: &str) -> AiPatch {
        AiPatch {
            status: Some(Some("done".to_owned())),
            provider: Some(Some(provider.to_owned())),
            model: Some(Some(model.to_owned())),
            schema_version: Some(Some(SCHEMA_VERSION)),
            error: Some(None),
            description: Some(Some(self.description)),
            tags: Some(Some(self.tags)),
            general_tags: Some(self.general_tags),
            specific_tags: Some(self.specific_tags),
            category: self.category.map(Some),
            content_type: self.content_type.map(Some),
            entities: Some(Some(self.entities)),
            keywords: Some(Some(self.keywords)),
            language: Some(Some(self.language)),
            save_reason: Some(Some(self.save_reason)),
            analyzed_at: None,
            web: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn answer() -> Value {
        json!({
            "description": " A lamp. ",
            "general_tags": ["Design", "design", " Lighting ", "Interior", "Extra"],
            "specific_tags": ["lighting", "walnut", "brass", "", "desk lamp", "studio", "e27",
                              "dimmer", "x"],
            "entities": ["Studio Lumen", "studio lumen", " IKEA "],
            "search_keywords": ["Walnut Desk Lamp", "walnut desk lamp"],
            "save_reason": " Good detailing. ",
            "language": " en ",
        })
    }

    #[test]
    fn normalizes_like_the_desktop() {
        let catalog = catalog(CatalogKind::Social, &answer()).unwrap();
        assert_eq!(catalog.general_tags, ["design", "lighting", "interior"]);
        assert_eq!(
            catalog.specific_tags,
            [
                "lighting",
                "walnut",
                "brass",
                "desk lamp",
                "studio",
                "e27",
                "dimmer"
            ]
        );
        assert_eq!(
            catalog.tags,
            [
                "design",
                "lighting",
                "interior",
                "walnut",
                "brass",
                "desk lamp",
                "studio",
                "e27",
                "dimmer"
            ]
        );
        assert_eq!(catalog.entities, ["Studio Lumen", "IKEA"]);
        assert_eq!(catalog.keywords, ["Walnut Desk Lamp"]);
        assert_eq!(catalog.description, " A lamp. ");
        assert_eq!(catalog.save_reason, "Good detailing.");
        assert_eq!(catalog.language, "en");
        assert_eq!(catalog.content_type, None);
        assert_eq!(catalog.category, None);
    }

    #[test]
    fn websites_map_purpose_and_industry() {
        let mut web = answer();
        web["purpose"] = json!("saas");
        web["industry"] = json!("other");
        let catalog = catalog(CatalogKind::Web, &web).unwrap();
        assert_eq!(catalog.content_type.as_deref(), Some("saas"));
        assert_eq!(catalog.category.as_deref(), Some("other"));
    }

    #[test]
    fn answers_off_the_schema_are_typed_errors() {
        let cases: [(&str, Value); 6] = [
            ("missing field", {
                let mut a = answer();
                a.as_object_mut().unwrap().remove("entities");
                a
            }),
            ("wrong type", {
                let mut a = answer();
                a["general_tags"] = json!("design, lighting");
                a
            }),
            ("non-string item", {
                let mut a = answer();
                a["specific_tags"] = json!(["ok", 7]);
                a
            }),
            ("extra field", {
                let mut a = answer();
                a["mood"] = json!("calm");
                a
            }),
            ("not an object", json!(["design"])),
            ("null", Value::Null),
        ];
        for (why, case) in cases {
            let err = catalog(CatalogKind::Social, &case).unwrap_err();
            assert!(
                matches!(
                    err,
                    OutputError::SchemaInvalid {
                        schema: "video_catalog",
                        ..
                    }
                ),
                "{why}"
            );
            assert_eq!(err.code(), "schema_invalid");
        }
        // A social answer is not a website answer: purpose and industry are required there.
        assert!(catalog(CatalogKind::Web, &answer()).is_err());
        let mut web = answer();
        web["purpose"] = json!("blog");
        web["industry"] = json!("other");
        let err = catalog(CatalogKind::Web, &web).unwrap_err();
        let OutputError::SchemaInvalid { path, message, .. } = err else {
            panic!("expected a schema error");
        };
        assert_eq!(path, "/purpose");
        assert!(
            !message.contains("blog"),
            "the answer's text stays out: {message}"
        );
    }

    #[test]
    fn text_that_is_not_json_is_an_error() {
        assert_eq!(
            parse_catalog(CatalogKind::Social, "{\"description\":"),
            Err(OutputError::NotJson)
        );
        assert!(parse_catalog(CatalogKind::Social, &answer().to_string()).is_ok());
    }

    #[test]
    fn clean_string_array_matches_the_desktop() {
        let items = [" A ", "a", "", "  ", "B", "Ä", "ä", "c"];
        assert_eq!(
            clean_string_array(items, false, usize::MAX),
            ["a", "b", "ä", "c"]
        );
        assert_eq!(
            clean_string_array(items, true, usize::MAX),
            ["A", "B", "Ä", "c"]
        );
        assert_eq!(clean_string_array(items, false, 2), ["a", "b"]);
        assert_eq!(clean_string_array(items, false, 0), ["a"]);
    }

    #[test]
    fn the_patch_marks_a_finished_analysis() {
        let mut web = answer();
        web["purpose"] = json!("saas");
        web["industry"] = json!("fintech");
        let patch = catalog(CatalogKind::Web, &web)
            .unwrap()
            .into_patch("operator", "qwen3.8-27b");
        assert_eq!(patch.status, Some(Some("done".to_owned())));
        assert_eq!(patch.provider, Some(Some("operator".to_owned())));
        assert_eq!(patch.model, Some(Some("qwen3.8-27b".to_owned())));
        assert_eq!(patch.schema_version, Some(Some(2)));
        assert_eq!(patch.error, Some(None));
        assert_eq!(
            patch.general_tags.as_deref(),
            Some(
                &[
                    "design".to_owned(),
                    "lighting".to_owned(),
                    "interior".to_owned()
                ][..]
            )
        );
        assert_eq!(patch.content_type, Some(Some("saas".to_owned())));
        assert_eq!(patch.category, Some(Some("fintech".to_owned())));
        assert_eq!(patch.analyzed_at, None);
        let social = catalog(CatalogKind::Social, &answer())
            .unwrap()
            .into_patch("p", "m");
        assert_eq!(
            social.content_type, None,
            "a social analysis leaves the column alone"
        );
        assert_eq!(social.category, None);
    }
}
