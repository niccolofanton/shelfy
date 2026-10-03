//! Alias proposals and transactional review. Accepted mappings resolve at every
//! later manual/AI write via repo::tags; no global or cross-library map cache.
use super::{Status, VocabTag, norm, vocabulary};
use crate::repo::{RepoError, Result};
use crate::search::index;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AliasPair {
    pub alias_norm: String,
    pub alias_form: String,
    pub canonical_norm: String,
    pub canonical_form: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alias {
    pub alias_norm: String,
    pub alias_form: String,
    pub canonical_norm: String,
    pub canonical_form: String,
    pub status: Status,
    pub count: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Accepted {
    pub accepted: usize,
    pub rewritten: usize,
    #[serde(skip)]
    pub post_ids: Vec<i64>,
}
/// Allowlist, no self maps/chains, first valid mapping per alias wins.
pub fn validate_pairs(batch: &[VocabTag], vocab: &[VocabTag], parsed: &Value) -> Vec<AliasPair> {
    let forms = |tags: &[VocabTag]| {
        tags.iter()
            .map(|t| {
                (
                    norm(&t.norm),
                    if t.form.is_empty() {
                        t.norm.clone()
                    } else {
                        t.form.clone()
                    },
                )
            })
            .filter(|(n, _)| !n.is_empty())
            .collect::<HashMap<_, _>>()
    };
    let candidates = forms(batch);
    let canon = forms(vocab);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for pair in parsed
        .get("aliases")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let a = norm(pair.get("alias").and_then(Value::as_str).unwrap_or(""));
        let c = norm(pair.get("canonical").and_then(Value::as_str).unwrap_or(""));
        if a == c
            || !candidates.contains_key(&a)
            || !canon.contains_key(&c)
            || candidates.contains_key(&c)
            || !seen.insert(a.clone())
        {
            continue;
        }
        out.push(AliasPair {
            alias_form: candidates[&a].clone(),
            canonical_form: canon[&c].clone(),
            alias_norm: a,
            canonical_norm: c,
        });
    }
    out
}
pub fn unaliased_tags(conn: &Connection, limit: usize) -> Result<Vec<VocabTag>> {
    candidates(conn, limit.min(400), true)
}
pub fn canonical_vocab(conn: &Connection, limit: usize) -> Result<Vec<VocabTag>> {
    candidates(conn, limit.min(300), false)
}
fn candidates(conn: &Connection, limit: usize, unaliased: bool) -> Result<Vec<VocabTag>> {
    let rows = conn
        .prepare_cached("SELECT alias_norm,canonical_norm FROM tag_alias")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut excluded = HashSet::new();
    for (a, c) in rows {
        excluded.insert(a);
        if unaliased {
            excluded.insert(c);
        }
    }
    Ok(vocabulary(conn)?
        .into_iter()
        .filter(|t| !excluded.contains(&t.norm))
        .take(limit)
        .collect())
}
/// Save proposed pairs only: accepted mappings and previously reviewed data
/// cannot be overwritten by a delayed model response. No post rows change.
pub fn save_proposals(tx: &Transaction<'_>, pairs: &[AliasPair], now: i64) -> Result<usize> {
    let mut added = 0;
    let mut seen = HashSet::new();
    for pair in pairs {
        let a = norm(&pair.alias_norm);
        let c = norm(&pair.canonical_norm);
        if a.is_empty() || c.is_empty() || a == c || !seen.insert(a.clone()) {
            continue;
        }
        let root = crate::repo::tags::resolve_alias(tx, &c)?;
        if root.norm == a {
            continue;
        }
        let form = if root.norm == c {
            let form = crate::search::terms::js_trim(&pair.canonical_form);
            if form.is_empty() {
                c.clone()
            } else {
                form.to_owned()
            }
        } else {
            root.form
        };
        // Do not replace earlier proposals: review from another tab still
        // refers to exactly the canonical shown to that user.
        added+=tx.execute("INSERT OR IGNORE INTO tag_alias (alias_norm,canonical_norm,canonical_form,status,created_at)
            VALUES (?1,?2,?3,'proposed',?4)",params![a,root.norm,form,now])?;
    }
    Ok(added)
}
pub fn list(conn: &Connection, status: Option<Status>) -> Result<Vec<Alias>> {
    let tags: HashMap<_, _> = vocabulary(conn)?
        .into_iter()
        .map(|v| (v.norm.clone(), v))
        .collect();
    let mut rows = conn
        .prepare_cached(
            "SELECT alias_norm,canonical_norm,canonical_form,status FROM tag_alias
        WHERE (?1 IS NULL OR status=?1) ORDER BY alias_norm",
        )?
        .query_map([status.map(Status::as_str)], |r| {
            let a: String = r.get(0)?;
            let tag = tags.get(&a);
            let status: String = r.get(3)?;
            Ok(Alias {
                alias_form: tag.map_or(a.clone(), |t| t.form.clone()),
                count: tag.map_or(0, |t| t.count),
                alias_norm: a,
                canonical_norm: r.get(1)?,
                canonical_form: r.get(2)?,
                status: if status == "accepted" {
                    Status::Accepted
                } else {
                    Status::Proposed
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.sort_by_key(|a| std::cmp::Reverse(a.count));
    Ok(rows)
}
/// Accept and rewrite both tag sources, preserving the stronger AI tier on a
/// collision. Original JSON tags remain intact, exactly as on the desktop.
pub fn accept(tx: &Transaction<'_>, alias: &str) -> Result<Accepted> {
    let alias = norm(alias);
    let row = tx
        .query_row(
            "SELECT canonical_norm,canonical_form,status FROM tag_alias WHERE alias_norm=?1",
            [&alias],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(RepoError::NotFound)?;
    if row.2 == "accepted" {
        return Ok(Accepted::default());
    }
    let root = crate::repo::tags::resolve_alias(tx, &row.0)?;
    if root.norm == alias {
        return Err(RepoError::Conflict("alias cycle"));
    }
    let form = if root.norm == row.0 { row.1 } else { root.form };
    let ids = tx
        .prepare_cached(
            "SELECT DISTINCT post_id FROM post_tags WHERE tag_norm=?1 ORDER BY post_id",
        )?
        .query_map([&alias], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    tx.execute("UPDATE tag_alias SET canonical_norm=?1,canonical_form=?2,status='accepted' WHERE alias_norm=?3",params![root.norm,form,alias])?;
    // Flatten existing accepted inbound aliases too, keeping the no-chain invariant.
    tx.execute("UPDATE tag_alias SET canonical_norm=?1,canonical_form=?2 WHERE canonical_norm=?3 AND status='accepted' AND alias_norm<>?3",params![root.norm,form,alias])?;
    let rewritten = tx.execute(
        "INSERT INTO post_tags (post_id,tag_norm,tag_form,source,tier)
        SELECT post_id,?1,?2,source,tier FROM post_tags WHERE tag_norm=?3
        ON CONFLICT(post_id,tag_norm,source) DO UPDATE SET tag_form=excluded.tag_form,
        tier=CASE WHEN post_tags.tier='specific' OR excluded.tier='specific' THEN 'specific'
          WHEN post_tags.tier='general' OR excluded.tier='general' THEN 'general' ELSE NULL END",
        params![root.norm, form, alias],
    )?;
    tx.execute("DELETE FROM post_tags WHERE tag_norm=?1", [&alias])?;
    // Move cluster membership if possible; collisions preserve canonical's owner.
    tx.execute(
        "UPDATE OR IGNORE tag_cluster_membership SET tag_norm=?1 WHERE tag_norm=?2",
        params![root.norm, alias],
    )?;
    tx.execute(
        "DELETE FROM tag_cluster_membership WHERE tag_norm=?1",
        [&alias],
    )?;
    for id in &ids {
        index::reindex_post(tx, *id)?;
    }
    Ok(Accepted {
        accepted: 1,
        rewritten,
        post_ids: ids,
    })
}
pub fn accept_all(tx: &Transaction<'_>) -> Result<Accepted> {
    let aliases = tx
        .prepare_cached(
            "SELECT alias_norm FROM tag_alias WHERE status='proposed' ORDER BY alias_norm",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut result = Accepted::default();
    for a in aliases {
        let r = accept(tx, &a)?;
        result.accepted += r.accepted;
        result.rewritten += r.rewritten;
        result.post_ids.extend(r.post_ids);
    }
    result.post_ids.sort_unstable();
    result.post_ids.dedup();
    Ok(result)
}
pub fn dismiss(tx: &Transaction<'_>, alias: &str) -> Result<()> {
    let alias = norm(alias);
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM tag_alias WHERE alias_norm=?1",
            [&alias],
            |r| r.get(0),
        )
        .optional()?;
    match status.as_deref() {
        None => Err(RepoError::NotFound),
        Some("accepted") => Err(RepoError::Conflict("alias already accepted")),
        _ => {
            tx.execute("DELETE FROM tag_alias WHERE alias_norm=?1", [alias])?;
            Ok(())
        }
    }
}
