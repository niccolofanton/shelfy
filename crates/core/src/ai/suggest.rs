//! Suggestion prompts and real, live vocabulary intersection (P3-15).
use rusqlite::Connection;
use serde::Serialize;
use sha1::{Digest, Sha1};

use super::prompts::{self, PromptError, ResponseSchema, Task};
use super::template::Var;
use crate::repo::{RepoError, posts::SourceBucket};
use crate::search::{terms::js_trim, vocab::Vocabulary};

pub const MAX_TAGS: usize = 8;
pub const TTL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestRequest {
    pub system: String,
    pub user: String,
    pub schema: &'static ResponseSchema,
    pub temperature: f64,
    pub max_tokens: u32,
}

/// The same builder the desktop uses for the suggest task of shared/ai.
pub fn request(query: &str) -> Result<SuggestRequest, PromptError> {
    Ok(SuggestRequest {
        system: prompts::system_prompt(Task::Suggest, &[])?,
        user: prompts::user_prompt(Task::Suggest, &[("query", Var::Text(js_trim(query)))])?,
        schema: prompts::response_schema(Task::Suggest).expect("suggest has a schema"),
        temperature: prompts::spec(Task::Suggest).temperature,
        max_tokens: prompts::max_tokens(Task::Suggest, 0),
    })
}

/// Content-addressed vocabulary generation. Library writes to ai_cache must
/// not invalidate their own entries; this survives restart and cache writes,
/// but moves with live membership, forms, frequencies and accepted aliases.
pub fn cache_key(
    conn: &Connection,
    query: &str,
    source: Option<SourceBucket>,
) -> Result<Vec<u8>, RepoError> {
    let predicate = crate::search::vocab::source_predicate(source);
    let rows: Vec<(String, String, i64)> = conn
        .prepare(&format!(
            "SELECT t.tag_norm,t.tag_form,COUNT(DISTINCT t.post_id) FROM post_tags t
         JOIN posts p ON p.id=t.post_id WHERE p.deleted_at IS NULL AND {predicate}
         GROUP BY t.tag_norm,t.tag_form ORDER BY t.tag_norm,t.tag_form"
        ))?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let aliases: Vec<(String, String, String)> = conn
        .prepare(
            "SELECT alias_norm,canonical_norm,canonical_form FROM tag_alias
         WHERE status='accepted' ORDER BY alias_norm",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let query = js_trim(query)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let scope = match source {
        None => "all",
        Some(SourceBucket::Web) => "sites",
        Some(SourceBucket::Social) => "social",
    };
    let bytes = serde_json::to_vec(&(
        "suggest-v1",
        prompts::SCHEMA_VERSION,
        query,
        scope,
        rows,
        aliases,
    ))
    .expect("vocabulary serializes");
    Ok(Sha1::digest(bytes).to_vec())
}

/// Exact, accepted-alias, then indexed-vocabulary fuzzy substring matches.
/// Recheck even cached candidates against this snapshot to discard removed
/// tags and exclude posts outside the requested scope or in the trash.
pub fn intersect(
    conn: &Connection,
    source: Option<SourceBucket>,
    candidates: &[String],
) -> Result<Vec<String>, RepoError> {
    let vocab = Vocabulary::load_for_source(conn, source)?;
    let candidates: Vec<_> = candidates
        .iter()
        .take(64)
        .filter(|s| s.chars().count() <= 256)
        .cloned()
        .collect();
    let mut tags = vocab.intersect(conn, &candidates)?;
    tags.truncate(MAX_TAGS);
    Ok(tags)
}

/// Strict JSON from the suggest schema; malformed output has no chips.
pub fn parse(text: &str) -> Option<Vec<String>> {
    if text.len() > 65_536 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    let tags = object.get("tags")?.as_array()?;
    tags.iter().map(|t| t.as_str().map(str::to_owned)).collect()
}
