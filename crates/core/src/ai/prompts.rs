//! The AI tasks of `shared/ai/manifest.json` (plan §2.15): each task's prompts,
//! rendered from their template files, its response schema and its sampling.
//!
//! The files are embedded with `include_str!`, so the binary always carries the
//! prompts it was built with. The desktop reads the same files through
//! `shared/ai/prompts.ts`; the golden sets under `shared/golden/ai/` keep the
//! two readers identical. `shared/ai/README.md` explains how to change them.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use super::template::{self, TemplateError, Var};

/// The version of the AI output schemas, which web results store in
/// `ai_schema_version`. It is the manifest's `schemaVersion` (a test ties the
/// two): an output-schema change bumps both.
pub const SCHEMA_VERSION: i64 = 2;

/// The embedded files of `shared/ai/`: `(file name, text)`. A test checks that
/// this is exactly the set the manifest names.
pub const FILES: &[(&str, &str)] = &[
    (
        "aliases.schema.json",
        include_str!("../../../../shared/ai/aliases.schema.json"),
    ),
    (
        "aliases.user.md",
        include_str!("../../../../shared/ai/aliases.user.md"),
    ),
    (
        "catalog.schema.json",
        include_str!("../../../../shared/ai/catalog.schema.json"),
    ),
    (
        "catalog.system.md",
        include_str!("../../../../shared/ai/catalog.system.md"),
    ),
    (
        "catalog.user.md",
        include_str!("../../../../shared/ai/catalog.user.md"),
    ),
    (
        "chat.system.md",
        include_str!("../../../../shared/ai/chat.system.md"),
    ),
    (
        "cluster_refine.schema.json",
        include_str!("../../../../shared/ai/cluster_refine.schema.json"),
    ),
    (
        "cluster_refine.system.md",
        include_str!("../../../../shared/ai/cluster_refine.system.md"),
    ),
    (
        "cluster_refine.user.md",
        include_str!("../../../../shared/ai/cluster_refine.user.md"),
    ),
    (
        "qc.schema.json",
        include_str!("../../../../shared/ai/qc.schema.json"),
    ),
    (
        "qc.system.md",
        include_str!("../../../../shared/ai/qc.system.md"),
    ),
    (
        "qc.user.md",
        include_str!("../../../../shared/ai/qc.user.md"),
    ),
    (
        "suggest.schema.json",
        include_str!("../../../../shared/ai/suggest.schema.json"),
    ),
    (
        "suggest.system.md",
        include_str!("../../../../shared/ai/suggest.system.md"),
    ),
    (
        "suggest.user.md",
        include_str!("../../../../shared/ai/suggest.user.md"),
    ),
    (
        "web_catalog.schema.json",
        include_str!("../../../../shared/ai/web_catalog.schema.json"),
    ),
    (
        "web_catalog.system.md",
        include_str!("../../../../shared/ai/web_catalog.system.md"),
    ),
    (
        "web_catalog.user.md",
        include_str!("../../../../shared/ai/web_catalog.user.md"),
    ),
];

/// `shared/ai/manifest.json`, as embedded.
pub const MANIFEST_JSON: &str = include_str!("../../../../shared/ai/manifest.json");

/// A task of the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Task {
    /// Social cataloging (AI-08).
    Catalog,
    /// Website cataloging (AI-09).
    WebCatalog,
    /// Screenshot quality check (AI-23).
    Qc,
    /// The search chat (AI-35).
    Chat,
    /// Suggestion chips (AI-41).
    Suggest,
    /// Cluster refinement (AI-30).
    ClusterRefine,
    /// Alias proposals (AI-32).
    Aliases,
}

impl Task {
    /// Every task, in manifest order.
    pub const ALL: [Self; 7] = [
        Self::Catalog,
        Self::WebCatalog,
        Self::Qc,
        Self::Chat,
        Self::Suggest,
        Self::ClusterRefine,
        Self::Aliases,
    ];

    /// The task's key in the manifest.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::WebCatalog => "web_catalog",
            Self::Qc => "qc",
            Self::Chat => "chat",
            Self::Suggest => "suggest",
            Self::ClusterRefine => "cluster_refine",
            Self::Aliases => "aliases",
        }
    }
}

/// The manifest.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    /// The output schema version ([`SCHEMA_VERSION`]).
    pub schema_version: i64,
    /// The tasks, by name.
    pub tasks: HashMap<String, TaskSpec>,
}

/// One task of the manifest.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskSpec {
    /// What the task is for.
    pub about: String,
    /// The system prompt's file.
    pub system: String,
    /// The user message's file, when the task has a fixed one.
    pub user: Option<String>,
    /// The response schema, for structured answers.
    pub schema: Option<SchemaRef>,
    /// Sampling temperature.
    pub temperature: f64,
    /// `max_tokens`.
    pub max_tokens: MaxTokens,
    /// Catalogs: the caption or page text is cut at this many UTF-16 units.
    pub caption_max: Option<usize>,
    /// Catalogs: at most this many vocabulary or tech-stack hints.
    pub hints_max: Option<usize>,
    /// Catalogs: the caps of the normalized tag lists.
    pub caps: Option<CatalogCaps>,
}

/// A task's response schema in the manifest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaRef {
    /// The schema's name for the provider (`json_schema.name`, the tool name).
    pub name: String,
    /// Its file.
    pub file: String,
}

/// A task's `max_tokens`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum MaxTokens {
    /// The same for any input.
    Fixed(u32),
    /// Growing with the input.
    PerItem(MaxTokensRule),
}

/// `min(max, base + per_item × items)`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaxTokensRule {
    /// Tokens for any input.
    pub base: u32,
    /// Tokens per input item.
    pub per_item: u32,
    /// The ceiling.
    pub max: u32,
}

/// The normalization caps of a catalog's tag lists.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogCaps {
    /// General (theme) tags.
    pub general: usize,
    /// Specific (detail) tags.
    pub specific: usize,
    /// The flat list, general then specific.
    pub tags: usize,
}

/// A response schema, provider-neutral: the fields of OpenAI's `json_schema`,
/// which the Anthropic adapter turns into a forced tool.
#[derive(Debug, Serialize)]
pub struct ResponseSchema {
    /// The schema's name.
    pub name: &'static str,
    /// Strict decoding: always asked for.
    pub strict: bool,
    /// The schema as compact JSON with the file's key order. Providers fill
    /// the answer's fields in this order, so send this rather than a
    /// [`Value`], whose map sorts the keys.
    pub schema: Box<RawValue>,
    /// The schema, parsed (for validation).
    #[serde(skip)]
    pub value: Value,
}

/// A prompt that cannot be built.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PromptError {
    /// The task has no user message of its own (the chat).
    #[error("shared/ai: task {0} has no user message")]
    NoUserMessage(&'static str),
    /// A template does not render with the variables given.
    #[error("shared/ai/{file}: {source}")]
    Template {
        /// The template file.
        file: String,
        /// What went wrong.
        source: TemplateError,
    },
}

static MANIFEST: LazyLock<Manifest> = LazyLock::new(|| {
    serde_json::from_str(MANIFEST_JSON).expect("shared/ai/manifest.json is valid (prompts tests)")
});

static SCHEMAS: LazyLock<HashMap<Task, ResponseSchema>> = LazyLock::new(|| {
    Task::ALL
        .into_iter()
        .filter_map(|task| {
            let schema = spec(task).schema.as_ref()?;
            let text = file(&schema.file).expect("the manifest names embedded files (tests)");
            let value = serde_json::from_str(text).expect("schema files are JSON (tests)");
            let compact = RawValue::from_string(compact_json(text))
                .expect("a compacted JSON document is JSON (tests)");
            Some((
                task,
                ResponseSchema {
                    name: schema.name.as_str(),
                    strict: true,
                    schema: compact,
                    value,
                },
            ))
        })
        .collect()
});

/// The manifest.
#[must_use]
pub fn manifest() -> &'static Manifest {
    &MANIFEST
}

/// A task of the manifest.
///
/// # Panics
///
/// When the manifest lacks the task, which the tests rule out.
#[must_use]
pub fn spec(task: Task) -> &'static TaskSpec {
    MANIFEST
        .tasks
        .get(task.name())
        .unwrap_or_else(|| panic!("shared/ai/manifest.json has no task {}", task.name()))
}

/// The text of an embedded file.
#[must_use]
pub fn file(name: &str) -> Option<&'static str> {
    FILES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, text)| *text)
}

fn render(file_name: &str, vars: &[(&str, Var<'_>)]) -> Result<String, PromptError> {
    let text = file(file_name).expect("the manifest names embedded files (tests)");
    template::render(text, vars).map_err(|source| PromptError::Template {
        file: file_name.to_owned(),
        source,
    })
}

/// The system prompt of a task, rendered with `vars`.
///
/// # Errors
///
/// [`PromptError::Template`] when the template uses a variable that `vars`
/// lacks (the tests render every template with its builder's variables).
pub fn system_prompt(task: Task, vars: &[(&str, Var<'_>)]) -> Result<String, PromptError> {
    render(&spec(task).system, vars)
}

/// The user message of a task, rendered with `vars`.
///
/// # Errors
///
/// [`PromptError::NoUserMessage`] for the chat; [`PromptError::Template`] as
/// for [`system_prompt`].
pub fn user_prompt(task: Task, vars: &[(&str, Var<'_>)]) -> Result<String, PromptError> {
    let user = spec(task)
        .user
        .as_deref()
        .ok_or(PromptError::NoUserMessage(task.name()))?;
    render(user, vars)
}

/// The response schema of a task; `None` for free text (the chat).
#[must_use]
pub fn response_schema(task: Task) -> Option<&'static ResponseSchema> {
    SCHEMAS.get(&task)
}

/// The `max_tokens` of a task for an input of `items` items (ignored unless
/// the task grows with its input).
#[must_use]
pub fn max_tokens(task: Task, items: usize) -> u32 {
    match &spec(task).max_tokens {
        MaxTokens::Fixed(n) => *n,
        MaxTokens::PerItem(rule) => {
            let items = u32::try_from(items).unwrap_or(u32::MAX);
            rule.base
                .saturating_add(rule.per_item.saturating_mul(items))
                .min(rule.max)
        }
    }
}

/// `text` without the whitespace between JSON tokens: the compact form that
/// `JSON.stringify` writes, keys in their original order. `text` must be JSON.
fn compact_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in text.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if !matches!(c, ' ' | '\t' | '\n' | '\r') {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    #[test]
    fn schema_version_is_the_manifests() {
        assert_eq!(SCHEMA_VERSION, 2);
        assert_eq!(manifest().schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn the_manifest_has_every_task_and_nothing_else() {
        let names: BTreeSet<&str> = manifest().tasks.keys().map(String::as_str).collect();
        let tasks: BTreeSet<&str> = Task::ALL.iter().map(|t| t.name()).collect();
        assert_eq!(names, tasks);
    }

    #[test]
    fn the_embedded_files_are_the_ones_the_manifest_names_and_the_directory_holds() {
        let embedded: BTreeSet<&str> = FILES.iter().map(|(name, _)| *name).collect();
        assert_eq!(embedded.len(), FILES.len(), "a file is embedded twice");
        let named: BTreeSet<&str> = manifest()
            .tasks
            .values()
            .flat_map(|t| {
                [
                    Some(&t.system),
                    t.user.as_ref(),
                    t.schema.as_ref().map(|s| &s.file),
                ]
                .into_iter()
                .flatten()
                .map(String::as_str)
            })
            .collect();
        assert_eq!(embedded, named);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shared/ai");
        let on_disk: BTreeSet<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| {
                (n.ends_with(".md") || n.ends_with(".json"))
                    && n != "manifest.json"
                    && n != "README.md"
            })
            .collect();
        let embedded: BTreeSet<String> = embedded.into_iter().map(str::to_owned).collect();
        assert_eq!(
            on_disk, embedded,
            "embed every prompt and schema of shared/ai/ in FILES"
        );
    }

    #[test]
    fn schemas_keep_their_key_order_and_meaning() {
        for task in Task::ALL {
            let Some(schema) = response_schema(task) else {
                assert_eq!(task, Task::Chat);
                continue;
            };
            let text = file(&spec(task).schema.as_ref().unwrap().file).unwrap();
            let reparsed: Value = serde_json::from_str(schema.schema.get()).unwrap();
            assert_eq!(reparsed, schema.value);
            assert_eq!(reparsed, serde_json::from_str::<Value>(text).unwrap());
            assert!(!schema.schema.get().contains('\n'));
            assert!(schema.strict);
        }
        let catalog = response_schema(Task::Catalog).unwrap().schema.get();
        assert!(catalog.starts_with(r#"{"type":"object","additionalProperties":false,"#));
        let order: Vec<usize> = ["description", "general_tags", "specific_tags", "language"]
            .iter()
            .map(|k| catalog.find(&format!("\"{k}\":{{")).unwrap())
            .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{catalog}");
    }

    #[test]
    fn compact_json_keeps_strings_intact() {
        assert_eq!(
            compact_json("{ \"a b\" : [ 1 ,\n \"x \\\" y\" ] }"),
            r#"{"a b":[1,"x \" y"]}"#
        );
    }

    #[test]
    fn max_tokens_follow_the_manifest() {
        assert_eq!(max_tokens(Task::Catalog, 0), 768);
        assert_eq!(max_tokens(Task::Qc, 9), 120);
        assert_eq!(max_tokens(Task::ClusterRefine, 3), 280);
        assert_eq!(max_tokens(Task::ClusterRefine, 1000), 2048);
        assert_eq!(max_tokens(Task::Aliases, 40), 1216);
        assert_eq!(max_tokens(Task::Aliases, usize::MAX), 2048);
    }

    #[test]
    fn llama_only_knobs_stay_out_of_the_shared_files() {
        let knobs = [
            "dry_multiplier",
            "dry_base",
            "dry_allowed_length",
            "dry_penalty_last_n",
            "cache_prompt",
            "chat_template_kwargs",
            "enable_thinking",
            "repeat_penalty",
            "n_predict",
        ];
        for (name, text) in FILES.iter().chain([&("manifest.json", MANIFEST_JSON)]) {
            for knob in knobs {
                assert!(!text.contains(knob), "{name} holds {knob}");
            }
        }
    }

    #[test]
    fn the_chat_has_no_user_message() {
        assert_eq!(
            user_prompt(Task::Chat, &[]),
            Err(PromptError::NoUserMessage("chat"))
        );
        assert!(response_schema(Task::Chat).is_none());
    }
}
