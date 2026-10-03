//! Bounded, metadata-only imports of desktop and extension JSON exports.
pub mod collections;
pub mod normalize;
pub mod v1;

use crate::ingest::merge::{self, UpsertOptions};
use crate::repo::{self, RepoError};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// One rejected record; indices refer to the source array before folding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    /// Zero-based source index.
    pub index: u64,
    /// Stable rejection code.
    pub code: String,
}
/// Durable incremental report. Rejection details are capped while the total
/// is retained, so malicious arrays cannot grow the working set indefinitely.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Newly created canonical posts.
    pub imported: u64,
    /// Existing posts whose stored layers changed.
    pub updated: u64,
    /// Collections created.
    pub collections: u64,
    /// Memberships added.
    pub links: u64,
    /// Unchanged keys and folded copies.
    pub skipped: u64,
    /// Bounded per-record failures.
    pub rejected: Vec<Rejected>,
    /// Total rejected input records.
    pub rejected_count: u64,
}
impl Report {
    /// Records a failure without unbounded per-record state.
    pub fn reject(&mut self, index: u64, code: &str) {
        self.rejected_count += 1;
        if self.rejected.len() < 1000 {
            self.rejected.push(Rejected {
                index,
                code: code.into(),
            });
        }
    }
}
/// Applies one bounded batch. Fold field-presence last-wins before normalization
/// and overwrite-AI merging; canonical aliases share the same fold. Memberships
/// are united. The caller supplies its transaction and durable report.
pub fn apply(
    conn: &Connection,
    records: &[(u64, Value)],
    defs: &BTreeMap<String, collections::Definition>,
    report: &mut Report,
    now: i64,
) -> repo::Result<Vec<String>> {
    let mut folded: BTreeMap<String, Value> = BTreeMap::new();
    for (index, raw) in records {
        match normalize::post(raw, now) {
            Err(e) => report.reject(*index, e.code()),
            Ok(p) => {
                if let Some(previous) = folded.get_mut(&p.incoming.key) {
                    let old = previous.as_object_mut().ok_or(RepoError::NotFound)?;
                    let mut memberships = old
                        .get("collections")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if let Some(more) = raw.get("collections").and_then(Value::as_array) {
                        for key in more {
                            if !memberships.contains(key) {
                                memberships.push(key.clone());
                            }
                        }
                    }
                    for (key, value) in raw.as_object().ok_or(RepoError::NotFound)? {
                        // The desktop normalizer ignores invalid/null AI
                        // fields; they are absent, not explicit clears.
                        if key.starts_with("ai") && !normalize::ai_present(key, value) {
                            continue;
                        }
                        old.insert(key.clone(), value.clone());
                    }
                    if raw.get("caption").is_some() || raw.get("text").is_some() {
                        // IG extension caption and desktop text name the same
                        // normalized field. Last presence wins across aliases.
                        old.remove("caption");
                        old.insert(
                            "text".into(),
                            Value::String(p.incoming.caption.unwrap_or_default()),
                        );
                    }
                    if !memberships.is_empty() {
                        old.insert("collections".into(), Value::Array(memberships));
                    }
                    report.skipped += 1;
                } else {
                    folded.insert(p.incoming.key, raw.clone());
                }
            }
        }
    }
    let posts: Vec<_> = folded
        .values()
        .map(|v| {
            let mut raw = v.clone();
            let memberships = raw.as_object_mut().and_then(|o| o.remove("collections"));
            let mut p = normalize::post(&raw, now).map_err(|_| RepoError::Invalid {
                field: "post",
                reason: "invalid folded record",
            })?;
            // Each source list was checked already; their union can be larger
            // than the per-record list cap while staying within the batch cap.
            if let Some(memberships) = memberships {
                p.collections =
                    serde_json::from_value(memberships).map_err(|_| RepoError::Invalid {
                        field: "collections",
                        reason: "invalid folded memberships",
                    })?;
            }
            Ok(p)
        })
        .collect::<repo::Result<_>>()?;
    let mut inputs: Vec<_> = posts.iter().map(|p| p.incoming.clone()).collect();
    // Missing export dates must not become a fresh wall-clock timestamp on
    // each overwrite. Keep the stored date, NULL for a new undated analysis.
    for p in &mut inputs {
        if p.ai.analyzed_at.is_none()
            && p.ai.status.as_ref().and_then(Option::as_deref) == Some("done")
        {
            let at: Option<Option<i64>> = conn
                .query_row(
                    "SELECT ai_analyzed_at FROM posts WHERE key=?1",
                    [&p.key],
                    |r| r.get(0),
                )
                .optional()?;
            p.ai.analyzed_at = Some(at.flatten());
        }
    }
    let merged = merge::upsert_batch(conn, &inputs, UpsertOptions { overwrite_ai: true }, now)?;
    report.imported += merged.inserted as u64;
    report.updated += merged.changed as u64;
    report.skipped += (merged.merged - merged.changed) as u64;
    let mut changed = Vec::new();
    for (p, done) in posts.iter().zip(merged.posts) {
        let mut altered = done.changed;
        if done.inserted {
            repo::posts::update_user_content(conn, done.id, &p.user, now)?;
        }
        for key in &p.collections {
            let fallback;
            let def = if let Some(d) = defs.get(key) {
                d
            } else {
                fallback = collections::from_key(key);
                let Some(d) = fallback.as_ref() else {
                    continue;
                };
                d
            };
            let (id, created) = collections::ensure(conn, def, now)?;
            report.collections += u64::from(created);
            let links = repo::collections::add_posts(conn, &[done.id], &[id], now)?;
            report.links += links as u64;
            altered |= links > 0;
        }
        if altered {
            changed.push(p.incoming.key.clone());
        }
    }
    Ok(changed)
}
