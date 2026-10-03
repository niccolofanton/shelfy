//! The golden sets of the AI catalog (`shared/golden/ai/catalog/`, written by
//! `scripts/golden/ai-catalog.ts`): the desktop's catalog requests and the
//! normalization of their answers, against `shelfy_core::ai`.

use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Map, Value};
use shelfy_core::ai::catalog::{self, CatalogKind};
use shelfy_core::ai::normalize::{self, clean_string_array};
use shelfy_core::ai::template::{self, Var};
use shelfy_core::repo::Platform;
use shelfy_core::repo::posts::{self, AiPatch, NewPost};
use shelfy_core::schema::{self, Kind};

use super::{AiFields, Alias, NOW_MS, check, layers};

fn kind(name: &str) -> CatalogKind {
    match name {
        "social" => CatalogKind::Social,
        "web" => CatalogKind::Web,
        other => panic!("unknown catalog kind {other}"),
    }
}

#[test]
fn templates_render_like_the_desktop() {
    check(
        "ai/catalog/templates",
        |(text, vars): (String, Map<String, Value>)| {
            let vars: Vec<(&str, Var<'_>)> = vars
                .iter()
                .map(|(name, value)| {
                    let var = match value {
                        Value::Bool(flag) => Var::Flag(*flag),
                        Value::String(text) => Var::Text(text),
                        other => panic!("variable {name}: {other}"),
                    };
                    (name.as_str(), var)
                })
                .collect();
            template::render(&text, &vars).unwrap()
        },
    );
}

#[test]
fn markers_are_stripped_like_the_desktop() {
    check("ai/catalog/markers", |(text,): (String,)| {
        catalog::strip_prompt_markers(&text)
    });
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CleanOptions {
    keep_case: Option<bool>,
    cap: Option<usize>,
}

#[test]
fn string_arrays_are_cleaned_like_the_desktop() {
    check(
        "ai/catalog/clean-strings",
        |(items, opts): (Vec<String>, CleanOptions)| {
            clean_string_array(
                items.iter().map(String::as_str),
                opts.keep_case.unwrap_or(false),
                opts.cap.unwrap_or(usize::MAX),
            )
        },
    );
}

#[test]
fn user_prompts_match_the_desktop() {
    check(
        "ai/catalog/user-prompt",
        |(text, hints, frames, kind_name): (Option<String>, Vec<String>, bool, String)| {
            catalog::user_prompt(kind(&kind_name), text.as_deref(), &hints, frames).unwrap()
        },
    );
}

#[test]
fn requests_match_the_desktop() {
    check(
        "ai/catalog/request",
        |(text, hints, frames, kind_name): (Option<String>, Vec<String>, bool, String)| {
            // Serialized as it is: a `Value` would sort the schema's keys.
            catalog::request(kind(&kind_name), text.as_deref(), &hints, frames).unwrap()
        },
    );
}

#[test]
fn answers_are_normalized_like_the_desktop() {
    check(
        "ai/catalog/normalize",
        |(answer, kind_name): (Value, String)| {
            normalize::catalog(kind(&kind_name), &answer).unwrap()
        },
    );
}

/// The desktop's `updateAiAnalysis` fields of an earlier analysis, as an
/// [`AiPatch`] (web-only columns untouched).
fn earlier(fields: AiFields) -> AiPatch {
    AiPatch {
        status: fields.status,
        provider: None,
        schema_version: None,
        error: None,
        model: fields.model,
        description: fields.description,
        tags: fields.tags,
        general_tags: fields.general_tags,
        specific_tags: fields.specific_tags,
        category: fields.category,
        content_type: fields.content_type,
        entities: fields.entities,
        keywords: fields.keywords,
        language: fields.language,
        save_reason: fields.save_reason,
        web: None,
        analyzed_at: fields.analyzed_at,
    }
}

#[test]
fn applied_answers_match_the_desktop() {
    type Args = (Vec<Alias>, Option<AiFields>, String, Value, String);
    check(
        "ai/catalog/apply",
        |(aliases, before, kind_name, answer, model): Args| {
            let mut conn = Connection::open_in_memory().unwrap();
            schema::migrate(&mut conn, Kind::Library).unwrap();
            for (alias, norm, form, status) in &aliases {
                conn.execute(
                    "INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status,
                                        created_at)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                    params![alias, norm, form, status],
                )
                .unwrap();
            }
            let post = NewPost::new("ig_1", Platform::Instagram, "1", "image", NOW_MS);
            let id = posts::insert(&conn, &post, NOW_MS).unwrap();
            if let Some(fields) = before {
                posts::update_ai(&conn, id, &earlier(fields), NOW_MS).unwrap();
            }
            let patch = normalize::catalog(kind(&kind_name), &answer)
                .unwrap()
                .into_patch("golden", &model);
            posts::update_ai(&conn, id, &patch, NOW_MS).unwrap();
            layers(&conn, id)
        },
    );
}
