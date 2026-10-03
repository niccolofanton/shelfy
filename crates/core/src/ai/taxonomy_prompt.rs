//! Desktop-compatible cluster refinement and alias requests. Taxonomy content
//! remains library data; provider calls belong to the server's AI service.
use serde::Serialize;

use super::prompts::{self, PromptError, ResponseSchema, Task};
use super::template::Var;
use crate::tags::{VocabTag, graph::CandidateGroup};

/// The text and structured-output contract of one taxonomy call.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyRequest {
    pub system: String,
    pub user: String,
    pub schema: &'static ResponseSchema,
    pub temperature: f64,
    pub max_tokens: u32,
}

/// One raw co-occurrence group, with the same neighbor context as the desktop.
pub fn refine(group: &CandidateGroup) -> Result<TaxonomyRequest, PromptError> {
    let lines = group
        .tags
        .iter()
        .map(|tag| {
            let neighbors = group.neighbors.get(tag);
            match neighbors.filter(|n| !n.is_empty()) {
                Some(neighbors) => format!("- {tag} ({})", neighbors.join(", ")),
                None => format!("- {tag}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    request(
        Task::ClusterRefine,
        &[("tags", Var::Text(&lines))],
        group.tags.len(),
    )
}

/// One batch and its canonical allowlist, preserving display forms verbatim.
pub fn aliases(
    batch: &[VocabTag],
    vocabulary: &[VocabTag],
) -> Result<TaxonomyRequest, PromptError> {
    let names = |tags: &[VocabTag]| {
        tags.iter()
            .map(|tag| {
                if tag.form.is_empty() {
                    tag.norm.as_str()
                } else {
                    tag.form.as_str()
                }
            })
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let candidates = names(batch);
    let vocabulary = names(vocabulary);
    request(
        Task::Aliases,
        &[
            ("candidates", Var::Text(&candidates)),
            ("vocabulary", Var::Text(&vocabulary)),
        ],
        batch.len(),
    )
}

fn request(
    task: Task,
    vars: &[(&str, Var<'_>)],
    items: usize,
) -> Result<TaxonomyRequest, PromptError> {
    Ok(TaxonomyRequest {
        system: prompts::system_prompt(task, &[])?,
        user: prompts::user_prompt(task, vars)?,
        schema: prompts::response_schema(task).expect("taxonomy tasks have response schemas"),
        temperature: prompts::spec(task).temperature,
        max_tokens: prompts::max_tokens(task, items),
    })
}
