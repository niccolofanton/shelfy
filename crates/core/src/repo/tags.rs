//! The derived tag and entity rows (`post_tags`, `post_entities`), ported from
//! the desktop's `normalizeTagRows`, `resolveAlias` and the tag sync in
//! `applyAiAnalysis` / `updateUserContent` (DATA-29, DATA-30).
//!
//! The JSON columns (`ai_tags_json`, `user_tags_json`, `ai_entities_json`) are
//! the source of truth; the rows are rebuilt from them in the same transaction.
//! Unlike the desktop, an AI tag and a manual tag with the same name are two
//! rows (`source` is in the primary key), so clearing one layer never drops the
//! other (plan §1.2 #3).
//!
//! This is the minimum the repositories need; the tag features (aliases,
//! clusters, renames) arrive with `core::tags` in the AI phase.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use super::Result;
use crate::search::terms::js_trim;

/// A tag or entity: `norm` is the lowercased form used for matching, `form`
/// the display form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NormForm {
    pub norm: String,
    pub form: String,
}

/// Trims each item, skips blanks, lowercases into `norm` and keeps the first
/// form of each norm (`normalizeTagRows`). Diacritics are preserved.
pub(crate) fn normalize_rows(items: &[String]) -> Vec<NormForm> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let form = js_trim(item);
        if form.is_empty() {
            continue;
        }
        let norm = form.to_lowercase();
        if seen.insert(norm.clone()) {
            out.push(NormForm {
                norm,
                form: form.to_owned(),
            });
        }
    }
    out
}

/// Follows accepted aliases from `norm` to its canonical tag (`resolveAlias`):
/// the identity when `norm` is not an alias; chains are followed with a loop
/// guard, although the alias table should never hold one.
pub(crate) fn resolve_alias(conn: &Connection, norm: &str) -> Result<NormForm> {
    let mut stmt = conn.prepare_cached(
        "SELECT canonical_norm, canonical_form FROM tag_alias
         WHERE alias_norm = ?1 AND status = 'accepted'",
    )?;
    let mut lookup = |key: &str| {
        stmt.query_row([key], |r| {
            Ok(NormForm {
                norm: r.get(0)?,
                form: r.get(1)?,
            })
        })
        .optional()
    };
    let key = js_trim(norm).to_lowercase();
    if key.is_empty() {
        return Ok(NormForm {
            norm: String::new(),
            form: String::new(),
        });
    }
    let Some(mut hit) = lookup(&key)? else {
        return Ok(NormForm {
            form: key.clone(),
            norm: key,
        });
    };
    let mut seen = HashSet::from([key]);
    while let Some(next) = lookup(&hit.norm)? {
        if !seen.insert(hit.norm.clone()) {
            break;
        }
        hit = next;
    }
    Ok(hit)
}

/// Canonical rows for `items`: normalized, alias-resolved, deduped on the
/// canonical norm. The form is the canonical one when an alias remapped the tag.
fn canonical_rows(conn: &Connection, items: &[String]) -> Result<Vec<NormForm>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for NormForm { norm, form } in normalize_rows(items) {
        let canonical = resolve_alias(conn, &norm)?;
        if canonical.norm.is_empty() || !seen.insert(canonical.norm.clone()) {
            continue;
        }
        let form = if canonical.norm == norm || canonical.form.is_empty() {
            form
        } else {
            canonical.form
        };
        out.push(NormForm {
            norm: canonical.norm,
            form,
        });
    }
    Ok(out)
}

/// Rebuilds the AI tag rows of a post. Tiers come from `general` and `specific`
/// (`specific` wins); without either list every row has no tier.
pub(crate) fn sync_ai_tags(
    conn: &Connection,
    post_id: i64,
    tags: &[String],
    general: Option<&[String]>,
    specific: Option<&[String]>,
) -> Result<()> {
    conn.prepare_cached("DELETE FROM post_tags WHERE post_id = ?1 AND source = 'ai'")?
        .execute([post_id])?;
    let have_tiers = general.is_some() || specific.is_some();
    let mut tier_by_norm: HashMap<String, &'static str> = HashMap::new();
    for (list, tier) in [(general, "general"), (specific, "specific")] {
        for tag in list.unwrap_or_default() {
            let norm = resolve_alias(conn, tag)?.norm;
            if !norm.is_empty() {
                tier_by_norm.insert(norm, tier);
            }
        }
    }
    let mut insert = conn.prepare_cached(
        "INSERT INTO post_tags (post_id, tag_norm, tag_form, source, tier) VALUES (?1, ?2, ?3, 'ai', ?4)",
    )?;
    for row in canonical_rows(conn, tags)? {
        let tier = if have_tiers {
            tier_by_norm.get(&row.norm).copied()
        } else {
            None
        };
        insert.execute(params![post_id, row.norm, row.form, tier])?;
    }
    Ok(())
}

/// Rebuilds the manual tag rows of a post.
pub(crate) fn sync_manual_tags(conn: &Connection, post_id: i64, tags: &[String]) -> Result<()> {
    conn.prepare_cached("DELETE FROM post_tags WHERE post_id = ?1 AND source = 'manual'")?
        .execute([post_id])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO post_tags (post_id, tag_norm, tag_form, source) VALUES (?1, ?2, ?3, 'manual')",
    )?;
    for row in canonical_rows(conn, tags)? {
        insert.execute(params![post_id, row.norm, row.form])?;
    }
    Ok(())
}

/// Rebuilds the entity rows of a post (no alias resolution, as on the desktop).
pub(crate) fn sync_entities(conn: &Connection, post_id: i64, entities: &[String]) -> Result<()> {
    conn.prepare_cached("DELETE FROM post_entities WHERE post_id = ?1")?
        .execute([post_id])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO post_entities (post_id, ent_norm, ent_form) VALUES (?1, ?2, ?3)",
    )?;
    for row in normalize_rows(entities) {
        insert.execute(params![post_id, row.norm, row.form])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_keeps_first_form_per_norm() {
        let items = ["  Città ", "città", "", "UX", " ux"].map(String::from);
        let rows = normalize_rows(&items);
        assert_eq!(
            rows,
            [
                NormForm {
                    norm: "città".into(),
                    form: "Città".into()
                },
                NormForm {
                    norm: "ux".into(),
                    form: "UX".into()
                },
            ]
        );
    }
}
