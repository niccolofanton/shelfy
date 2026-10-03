//! Deterministic merge suggestions and transactional source-of-truth renames.
use crate::repo::{RepoError, Result, tags};
use crate::search::index;
use crate::search::terms::js_trim;
use rusqlite::{Connection, Transaction, params};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub canonical: String,
    pub variants: Vec<String>,
    pub total_count: u64,
}
fn folded(s: &str) -> String {
    js_trim(
        &s.nfd()
            .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
            .collect::<String>(),
    )
    .to_lowercase()
}
fn near(a: &[u16], b: &[u16]) -> bool {
    if a.len().abs_diff(b.len()) > 2 {
        return false;
    }
    // Only the five diagonals that can finish within distance two matter.
    let mut prev: Vec<usize> = (0..=b.len()).map(|j| j.min(3)).collect();
    let mut curr = vec![3; b.len() + 1];
    for (i, &ac) in a.iter().enumerate() {
        curr.fill(3);
        let row = i + 1;
        curr[0] = row.min(3);
        let mut min = curr[0];
        for j in row.saturating_sub(2).max(1)..=b.len().min(row + 2) {
            curr[j] = (prev[j - 1] + usize::from(ac != b[j - 1]))
                .min(prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(3);
            min = min.min(curr[j]);
        }
        if min > 2 {
            return false;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()] <= 2
}
fn root(parents: &mut [usize], i: usize) -> usize {
    let mut r = i;
    while parents[r] != r {
        r = parents[r];
    }
    let mut cursor = i;
    while parents[cursor] != cursor {
        let next = parents[cursor];
        parents[cursor] = r;
        cursor = next;
    }
    r
}
/// Length bands and UTF-16 edit distance match the desktop's union-find rule.
pub fn suggestions(conn: &Connection) -> Result<Vec<Suggestion>> {
    // Suggestions use canonical norms, so display-form queries are unnecessary.
    let vocab:Vec<(String,u64)>=conn.prepare_cached("SELECT t.tag_norm,COUNT(DISTINCT t.post_id) FROM post_tags t JOIN posts p ON p.id=t.post_id WHERE p.deleted_at IS NULL GROUP BY t.tag_norm ORDER BY t.tag_norm")?.query_map([],|r|Ok((r.get(0)?,r.get::<_,i64>(1)? as u64)))?.collect::<rusqlite::Result<_>>()?;
    let keys: Vec<Vec<u16>> = vocab
        .iter()
        .map(|t| folded(&t.0).encode_utf16().collect())
        .collect();
    let mut parents: Vec<_> = (0..vocab.len()).collect();
    let mut exact: HashMap<&[u16], usize> = HashMap::new();
    let mut bands: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, key) in keys.iter().enumerate() {
        if let Some(&first) = exact.get(key.as_slice()) {
            let a = root(&mut parents, first);
            let b = root(&mut parents, i);
            parents[a] = b;
        } else {
            exact.insert(key, i);
        }
        bands.entry(key.len() / 2).or_default().push(i);
    }
    for (i, key) in keys.iter().enumerate() {
        let band = key.len() / 2;
        for b in band.saturating_sub(1)..=band + 1 {
            if let Some(bucket) = bands.get(&b) {
                for &j in bucket {
                    if j <= i || root(&mut parents, i) == root(&mut parents, j) {
                        continue;
                    }
                    if near(key, &keys[j]) {
                        let a = root(&mut parents, i);
                        let b = root(&mut parents, j);
                        parents[a] = b;
                    }
                }
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = vec![];
    let mut group_index = HashMap::new();
    for i in 0..vocab.len() {
        let r = root(&mut parents, i);
        let idx = *group_index.entry(r).or_insert_with(|| {
            groups.push(vec![]);
            groups.len() - 1
        });
        groups[idx].push(i);
    }
    let mut out = vec![];
    for mut group in groups {
        if group.len() < 2 {
            continue;
        }
        group.sort_by_key(|&i| std::cmp::Reverse(vocab[i].1));
        out.push(Suggestion {
            canonical: vocab[group[0]].0.clone(),
            variants: group[1..].iter().map(|&i| vocab[i].0.clone()).collect(),
            total_count: group.iter().map(|&i| vocab[i].1).sum(),
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.total_count));
    Ok(out)
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Merged {
    pub updated: usize,
    #[serde(skip)]
    pub keys: Vec<String>,
}
fn invalid(field: &'static str, reason: &'static str) -> RepoError {
    RepoError::Invalid { field, reason }
}
fn rewrite(
    conn: &Connection,
    raw: Option<&str>,
    sources: &HashSet<String>,
    target: &str,
    target_norm: &str,
) -> Result<Option<(String, Vec<String>)>> {
    let Some(mut value) = raw.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
    else {
        return Ok(None);
    };
    let Some(items) = value.as_array_mut() else {
        return Ok(None);
    };
    let mut seen = HashSet::new();
    let mut changed = false;
    let mut out = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        if let Some(s) = item.as_str() {
            let original = super::norm(s);
            let key = tags::resolve_alias(conn, &original)?.norm;
            let replace = sources.contains(&key);
            let form = if replace { target } else { s };
            let key = if replace { target_norm.to_owned() } else { key };
            changed |= replace && s != target;
            if !seen.insert(key) {
                changed = true;
                continue;
            }
            out.push(serde_json::Value::String(form.to_owned()));
        } else {
            out.push(item);
        }
    }
    if !changed {
        return Ok(None);
    }
    let strings = out
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    *items = out;
    Ok(Some((
        serde_json::to_string(&value).expect("JSON values serialize"),
        strings,
    )))
}
/// Both JSON layers, derived rows, cluster membership and both indexes commit
/// together. Renaming a canonical tag also redirects aliases that refer to it.
/// Specific tier wins when source tags and the destination collide.
pub fn merge(tx: &Transaction<'_>, sources: &[String], target: &str, now: i64) -> Result<Merged> {
    let target = js_trim(target);
    if target.is_empty() {
        return Err(invalid("target", "must not be blank"));
    }
    if target.len() > 1024 {
        return Err(invalid("target", "too long"));
    }
    if sources.is_empty() || sources.len() > 500 {
        return Err(invalid("sources", "expected 1 to 500 tags"));
    }
    let destination = tags::resolve_alias(tx, &super::norm(target))?;
    let target_form = if destination.norm != super::norm(target) {
        destination.form.as_str()
    } else {
        target
    };
    let mut source_keys = BTreeSet::new();
    let mut ordered = vec![];
    for s in sources {
        if s.len() > 1024 {
            return Err(invalid("sources", "tag too long"));
        }
        let key = tags::resolve_alias(tx, &super::norm(s))?.norm;
        if !key.is_empty() && source_keys.insert(key.clone()) {
            ordered.push(key);
        }
    }
    if source_keys.is_empty() {
        return Err(invalid("sources", "must contain a nonblank tag"));
    }
    let json = serde_json::to_string(&source_keys).expect("tag strings serialize");
    let rows:Vec<(i64,String,Option<String>,Option<String>)>=tx.prepare_cached("SELECT id,key,ai_tags_json,user_tags_json FROM posts WHERE EXISTS(SELECT 1 FROM post_tags t WHERE t.post_id=posts.id AND t.tag_norm IN (SELECT value FROM json_each(?1))) ORDER BY id")?.query_map([&json],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    let set = source_keys.iter().cloned().collect();
    let mut changed_keys = vec![];
    for (id, key, ai, user) in rows {
        let ai = rewrite(tx, ai.as_deref(), &set, target_form, &destination.norm)?;
        let user = rewrite(tx, user.as_deref(), &set, target_form, &destination.norm)?;
        if ai.is_none() && user.is_none() {
            continue;
        }
        if let Some((raw, strings)) = ai {
            let tiers: Vec<(String, Option<String>)> = tx
                .prepare_cached(
                    "SELECT tag_norm,tier FROM post_tags WHERE post_id=?1 AND source='ai'",
                )?
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut general = vec![];
            let mut specific = vec![];
            for (norm, tier) in tiers {
                let key = if source_keys.contains(&norm) {
                    destination.norm.clone()
                } else {
                    norm
                };
                match tier.as_deref() {
                    Some("specific") => specific.push(key),
                    Some("general") => general.push(key),
                    _ => {}
                }
            }
            general.retain(|key| !specific.contains(key));
            tags::sync_ai_tags(tx, id, &strings, Some(&general), Some(&specific))?;
            tx.execute(
                "UPDATE posts SET ai_tags_json=?1,updated_at=?2 WHERE id=?3",
                params![raw, now, id],
            )?;
        }
        if let Some((raw, strings)) = user {
            tags::sync_manual_tags(tx, id, &strings)?;
            tx.execute(
                "UPDATE posts SET user_tags_json=?1,updated_at=?2 WHERE id=?3",
                params![raw, now, id],
            )?;
        }
        index::reindex_post(tx, id)?;
        changed_keys.push(key);
    }
    let mut inherited = None;
    for key in &ordered {
        if inherited.is_none() {
            inherited = tx
                .query_row(
                    "SELECT cluster_id FROM tag_cluster_membership WHERE tag_norm=?1",
                    [key],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?;
        }
    }
    // A destination already in a cluster retains its membership.
    tx.execute("DELETE FROM tag_cluster_membership WHERE tag_norm IN (SELECT value FROM json_each(?1)) AND tag_norm<>?2",params![json,destination.norm])?;
    if let Some(id) = inherited {
        tx.execute(
            "INSERT OR IGNORE INTO tag_cluster_membership(cluster_id,tag_norm) VALUES (?1,?2)",
            params![id, destination.norm],
        )?;
    }
    for key in &ordered {
        if key != &destination.norm {
            tx.execute(
                "UPDATE tag_alias SET canonical_norm=?1,canonical_form=?2 WHERE canonical_norm=?3",
                params![destination.norm, target_form, key],
            )?;
        }
    }
    Ok(Merged {
        updated: changed_keys.len(),
        keys: changed_keys,
    })
}
use rusqlite::OptionalExtension;
pub fn rename(tx: &Transaction<'_>, from: &str, to: &str, now: i64) -> Result<Merged> {
    merge(tx, &[from.to_owned()], to, now)
}
