//! Proposed cluster persistence and review; accepted memberships are protected.
use super::{Status, norm, vocabulary};
use crate::repo::{RepoError, Result};
use crate::search::terms::js_trim;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefinedGroup {
    pub label: String,
    pub tags: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Cluster {
    pub id: i64,
    pub label: String,
    pub status: Status,
    pub top_tag: String,
    pub tags: Vec<String>,
    pub post_count: u64,
    pub run_id: i64,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SavedRun {
    pub run_id: i64,
    pub count: usize,
}
/// Strict JSON first, then balanced object recovery as the desktop does.
pub fn parse_refine_response(content: &Value) -> Value {
    let Some(text) = content.as_str().filter(|s| !js_trim(s).is_empty()) else {
        return json!({"groups":[],"outliers":[]});
    };
    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
        return parsed;
    }
    let mut stack = Vec::new();
    let mut groups = Vec::new();
    for (i, c) in text.char_indices() {
        if c == '{' {
            stack.push(i);
        } else if c == '}'
            && let Some(start) = stack.pop()
            && let Ok(g) = serde_json::from_str::<Value>(&text[start..i + 1])
            && g.get("name").is_some_and(Value::is_string)
            && g.get("tags").is_some_and(Value::is_array)
        {
            groups.push(g);
        }
    }
    json!({"groups":groups,"outliers":[]})
}
pub fn validate_refined_groups(input_tags: &[String], parsed: &Value) -> Vec<RefinedGroup> {
    let allowed: HashSet<_> = input_tags.iter().map(|s| s.to_lowercase()).collect();
    let mut used = HashSet::new();
    let mut out = Vec::new();
    for group in parsed
        .get("groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(label) = group
            .get("name")
            .and_then(Value::as_str)
            .map(js_trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let mut tags = Vec::new();
        for tag in group
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let t = norm(tag);
            if !t.is_empty() && allowed.contains(&t) && used.insert(t.clone()) {
                tags.push(t);
            }
        }
        // Desktop consumes valid singleton tags before dropping that group.
        if tags.len() >= 2 {
            out.push(RefinedGroup {
                label: label.to_owned(),
                tags,
            });
        }
    }
    out
}
/// New proposals replace old ones inside the caller's transaction. IDs never
/// recycle, so a delayed accept/delete cannot review another run's replacement.
/// The private sequence uses settings (ignored by the public preference reader),
/// avoiding a schema migration for this internal monotonic counter.
pub fn save_run(
    tx: &Transaction<'_>,
    groups: &[RefinedGroup],
    run_id: i64,
    now: i64,
) -> Result<SavedRun> {
    clear_proposals(tx, now)?;
    append_proposals(tx, groups, run_id, now)
}

/// Starts a new proposal generation without disturbing accepted memberships.
pub fn clear_proposals(tx: &Transaction<'_>, now: i64) -> Result<()> {
    // Remember legacy/manual proposal IDs before deleting them, even when this
    // is the first run to initialize the private sequence.
    let next = next_id(tx)?;
    tx.execute(
        "INSERT INTO settings (key,value_json,updated_at) VALUES ('taxonomy.cluster.next-id',?1,?2)
        ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at
        WHERE settings.value_json<>excluded.value_json",
        params![next.to_string(), now],
    )?;
    tx.execute("DELETE FROM tag_cluster WHERE status='proposed'", [])?;
    Ok(())
}

fn next_id(tx: &Transaction<'_>) -> Result<i64> {
    let max: i64 = tx.query_row("SELECT COALESCE(MAX(id),0) FROM tag_cluster", [], |r| {
        r.get(0)
    })?;
    let counter: Option<String> = tx
        .query_row(
            "SELECT value_json FROM settings WHERE key='taxonomy.cluster.next-id'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let next = counter
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1)
        .max(max.saturating_add(1));
    Ok(next)
}

/// Appends one completed chunk. Existing memberships, including proposals from
/// earlier chunks, are protected; the ID sequence never recycles after cancel.
pub fn append_proposals(
    tx: &Transaction<'_>,
    groups: &[RefinedGroup],
    run_id: i64,
    now: i64,
) -> Result<SavedRun> {
    let mut next = next_id(tx)?;
    let mut assigned: HashSet<String> = tx
        .prepare_cached("SELECT tag_norm FROM tag_cluster_membership")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut count = 0;
    for group in groups {
        let label = js_trim(&group.label);
        let mut seen = HashSet::new();
        let tags: Vec<_> = group
            .tags
            .iter()
            .map(|s| norm(s))
            .filter(|s| !s.is_empty() && !assigned.contains(s) && seen.insert(s.clone()))
            .collect();
        if label.is_empty() || tags.len() < 2 {
            continue;
        }
        if next == i64::MAX {
            return Err(RepoError::Conflict("cluster id exhausted"));
        }
        tx.execute(
            "INSERT INTO tag_cluster (id,label,label_norm,status,run_id,created_at,updated_at)
            VALUES (?1,?2,?3,'proposed',?4,?5,?5)",
            params![next, label, label.to_lowercase(), run_id, now],
        )?;
        for t in tags {
            tx.execute(
                "INSERT INTO tag_cluster_membership (tag_norm,cluster_id) VALUES (?1,?2)",
                params![t, next],
            )?;
            assigned.insert(t);
        }
        count += 1;
        next += 1;
    }
    tx.execute(
        "INSERT INTO settings (key,value_json,updated_at) VALUES ('taxonomy.cluster.next-id',?1,?2)
        ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at
        WHERE settings.value_json<>excluded.value_json",
        params![next.to_string(), now],
    )?;
    Ok(SavedRun { run_id, count })
}
pub fn list(conn: &Connection, limit: usize) -> Result<Vec<Cluster>> {
    let vocab = vocabulary(conn)?;
    let tags: std::collections::HashMap<_, _> =
        vocab.into_iter().map(|v| (v.norm.clone(), v)).collect();
    let rows = conn
        .prepare_cached(
            "SELECT id,label,status,COALESCE(run_id,0) FROM tag_cluster
        WHERE status IN ('proposed','accepted') ORDER BY id",
        )?
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for (id, label, status, run_id) in rows {
        let mut norms = conn
            .prepare_cached(
                "SELECT tag_norm FROM tag_cluster_membership WHERE cluster_id=?1 ORDER BY tag_norm",
            )?
            .query_map([id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if norms.is_empty() {
            continue;
        }
        norms.sort_by_key(|n| std::cmp::Reverse(tags.get(n).map_or(0, |v| v.count)));
        let post_count:i64=conn.query_row("SELECT COUNT(DISTINCT t.post_id) FROM post_tags t JOIN posts p ON p.id=t.post_id
            JOIN tag_cluster_membership m ON m.tag_norm=t.tag_norm WHERE m.cluster_id=?1 AND p.deleted_at IS NULL",[id],|r|r.get(0))?;
        let forms = norms
            .into_iter()
            .map(|n| tags.get(&n).map_or(n.clone(), |v| v.form.clone()))
            .collect();
        out.push(Cluster {
            id,
            top_tag: label.clone(),
            label,
            status: if status == "accepted" {
                Status::Accepted
            } else {
                Status::Proposed
            },
            tags: forms,
            post_count: post_count as u64,
            run_id,
        });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.post_count));
    out.truncate(limit);
    Ok(out)
}
fn exists(conn: &Connection, id: i64) -> Result<()> {
    conn.query_row("SELECT id FROM tag_cluster WHERE id=?1", [id], |r| {
        r.get::<_, i64>(0)
    })
    .optional()?
    .ok_or(RepoError::NotFound)?;
    Ok(())
}
pub fn review(
    tx: &Transaction<'_>,
    id: i64,
    accept: bool,
    label: Option<&str>,
    now: i64,
) -> Result<()> {
    exists(tx, id)?;
    if let Some(label) = label {
        let label = js_trim(label);
        if label.is_empty() || label.chars().count() > 200 {
            return Err(RepoError::Invalid {
                field: "label",
                reason: "must have 1–200 characters",
            });
        }
        tx.execute(
            "UPDATE tag_cluster SET label=?1,label_norm=?2,updated_at=?3 WHERE id=?4 AND label<>?1",
            params![label, label.to_lowercase(), now, id],
        )?;
    }
    if accept {
        tx.execute("UPDATE tag_cluster SET status='accepted',updated_at=?1 WHERE id=?2 AND status='proposed'",params![now,id])?;
    }
    Ok(())
}
pub fn dismiss(tx: &Transaction<'_>, id: i64) -> Result<()> {
    exists(tx, id)?;
    tx.execute("DELETE FROM tag_cluster WHERE id=?1", [id])?;
    Ok(())
}
pub fn remove_tag(tx: &Transaction<'_>, id: i64, tag: &str) -> Result<usize> {
    exists(tx, id)?;
    Ok(tx.execute(
        "DELETE FROM tag_cluster_membership WHERE cluster_id=?1 AND tag_norm=?2",
        params![id, norm(tag)],
    )?)
}
