//! "Similar sites" (plan PG1, §1.3 deferred "semantic similarity"; P4-05):
//! the desktop's facet-overlap tab is ported as plain set arithmetic;
//! semantic similarity stays deferred.
//!
//! Ported byte for byte from the desktop's `electron/db.ts#similarWebReferences`:
//! the same `SIMILAR_WEIGHTS`, the same weighted-Jaccard score, the same
//! palette-proximity tie-break. A golden fixture
//! (`shared/golden/web/similar.jsonl`, built by `scripts/golden/web-sites.ts`)
//! checks the order, score and shared facets byte for byte
//! (`crates/core/tests/web_sites.rs`).
//!
//! # Facets, read live (PG6)
//!
//! The desktop keeps one row per `(post, facet, value)` in a `post_facets`
//! table, built when the AI analysis is written. The web schema has no such
//! table: a post's facets are its `ai_web_json.facets` (`{facet: [values]}`),
//! read with `json_each` at query time. Only JSON string values count (a
//! number, object or boolean is skipped): real facets are always string
//! arrays (the AI catalog never writes anything else), and this sidesteps the
//! desktop's incidental `String(v || '')` coercion at write time, which is
//! unspecified for the exotic inputs it would apply to (`0`, `false`, …).
//!
//! # Deterministic order
//!
//! The desktop groups `post_facets` rows into per-post `Map`s and `Set`s, so
//! its shared-value order and its tie-break on equal scores follow
//! incidental SQL row order, which the port does not try to reproduce (SQLite
//! does not specify one either). This port iterates candidate posts in `id`
//! order and, within a facet, in the target's own `ai_web_json` array order
//! (`json_each` preserves it), which is deterministic and, in practice, what
//! the desktop's insertion order already followed. The golden fixture keeps
//! at most one shared value per matching facet, so this never shows.

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

use super::color::{self, Lab};
use crate::repo::posts;
use crate::repo::{Result, json_value};

/// Largest `limit` accepted (plan P4-05 card: "`limit ≤ 40`").
pub const MAX_LIMIT: u32 = 40;

/// How many shared facet/value pairs are reported (the desktop's
/// `shared.length < 8`).
const MAX_SHARED: usize = 8;

/// The desktop's `SIMILAR_WEIGHTS`: facet name, weight, in the object's
/// declaration order (the order `shared`/`sharedFacets` follow when more than
/// one facet matches).
const SIMILAR_WEIGHTS: [(&str, f64); 13] = [
    ("style", 3.0),
    ("layout", 2.0),
    ("typography", 2.0),
    ("font", 2.0),
    ("siteType", 2.0),
    ("colorMood", 1.5),
    ("industry", 1.5),
    ("hero", 1.0),
    ("theme", 1.0),
    ("imagery", 1.0),
    ("tech", 1.0),
    ("motion", 1.0),
    ("fontClass", 1.0),
];

/// One shared facet/value pair of a similarity match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedFacet {
    /// The facet name.
    pub facet: String,
    /// The value both sites carry for it (lowercased: facet values are
    /// matched case-insensitively).
    pub value: String,
}

/// A site similar to the one [`for_site`] was asked about.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarSite {
    /// The other site's key.
    pub key: String,
    /// Weighted Jaccard over shared facets, plus the palette-proximity
    /// tie-break, rounded to 2 decimals (the desktop's `Math.round(x*100)/100`).
    pub score: f64,
    /// The shared values, in the order of [`SIMILAR_WEIGHTS`] (at most
    /// [`MAX_SHARED`]).
    pub shared: Vec<String>,
    /// The shared facet/value pairs behind `shared`.
    pub shared_facets: Vec<SharedFacet>,
}

/// Facets of one post: facet name to its lowercased values, from a single
/// read of every web post's `ai_web_json.facets` (see the module docs).
type FacetSets = HashMap<String, Vec<String>>;

/// The sites most similar to the site `key` (desktop `similarWebReferences`):
/// weighted Jaccard over `SIMILAR_WEIGHTS`, then a palette-proximity
/// tie-break, best first. `limit` is clamped to `1..=`[`MAX_LIMIT`].
///
/// Empty when `key` is not a post, the post has no facets yet (unanalyzed, or
/// analyzed before P3's web catalog landed), or no other site shares a
/// weighted facet with it. The caller tells "not found" from "no matches" by
/// resolving `key` itself first ([`crate::repo::posts::id_for_key`]).
///
/// Runs several queries: call it inside [`crate::db::UserDb::read`] (like
/// [`super::captures::get`]) so they share one snapshot.
///
/// # Errors
///
/// Database errors.
pub fn for_site(conn: &Connection, key: &str, limit: u32) -> Result<Vec<SimilarSite>> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let Some(target_id) = posts::id_for_key(conn, key)? else {
        return Ok(Vec::new());
    };
    let candidates = web_posts_with_facets(conn)?;
    let Some(mine) = candidates
        .iter()
        .find(|c| c.id == target_id)
        .map(|c| &c.facets)
    else {
        return Ok(Vec::new());
    };

    let mut scored: Vec<(usize, f64, Vec<SharedFacet>)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.id != target_id)
        .filter_map(|(idx, c)| {
            score_pair(mine, &c.facets).map(|(score, shared)| (idx, score, shared))
        })
        .collect();
    // Best first; ties keep the `id` order `web_posts_with_facets` returned
    // (a stable sort), since the desktop's own tie-break is SQL-incidental.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let window = usize::try_from(limit)
        .unwrap_or(usize::MAX)
        .saturating_mul(2)
        .max(limit as usize);
    scored.truncate(window);

    let my_swatches = tie_break_swatches(conn, target_id)?;
    let mut out: Vec<SimilarSite> = Vec::with_capacity(scored.len());
    for (idx, score, shared_facets) in scored {
        let candidate = &candidates[idx];
        let bonus = if my_swatches.is_empty() {
            0.0
        } else {
            palette_bonus(conn, &my_swatches, candidate.id)?
        };
        let shared = shared_facets.iter().map(|s| s.value.clone()).collect();
        out.push(SimilarSite {
            key: candidate.key.clone(),
            score: round2(score + bonus),
            shared,
            shared_facets,
        });
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(limit as usize);
    Ok(out)
}

/// The weighted-Jaccard score of `mine` against `other`, and their shared
/// facet/value pairs (at most [`MAX_SHARED`], in [`SIMILAR_WEIGHTS`] order).
/// `None` when nothing is shared (score would be `0.0`): the desktop only
/// keeps a candidate when `score > 0`.
fn score_pair(mine: &FacetSets, other: &FacetSets) -> Option<(f64, Vec<SharedFacet>)> {
    let mut score = 0.0;
    let mut shared = Vec::new();
    for (facet, weight) in SIMILAR_WEIGHTS {
        let (Some(a), Some(b)) = (mine.get(facet), other.get(facet)) else {
            continue;
        };
        if a.is_empty() || b.is_empty() {
            continue;
        }
        let mut inter = 0usize;
        for value in a {
            if b.contains(value) {
                inter += 1;
                if shared.len() < MAX_SHARED {
                    shared.push(SharedFacet {
                        facet: facet.to_owned(),
                        value: value.clone(),
                    });
                }
            }
        }
        let union = a.len() + b.len() - inter;
        if union > 0 {
            #[allow(clippy::cast_precision_loss)] // facet lists are tiny
            {
                score += weight * inter as f64 / union as f64;
            }
        }
    }
    (score > 0.0).then_some((score, shared))
}

/// `Math.round(x * 100) / 100` (JS half-up rounding): exact for our always
/// non-negative scores, where it agrees with `f64::round`'s half-away-from-zero.
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// One web post's id, key and facets.
struct Candidate {
    id: i64,
    key: String,
    facets: FacetSets,
}

/// Every non-trashed web post with its facets (see the module docs), in `id`
/// order. A post with no facets (or none that parse as JSON strings) still
/// appears, with an empty [`FacetSets`] (so [`for_site`] can tell "the post
/// exists, unanalyzed" from "the post does not exist").
fn web_posts_with_facets(conn: &Connection) -> Result<Vec<Candidate>> {
    let mut posts: Vec<Candidate> = conn
        .prepare_cached(
            "SELECT id, key FROM posts WHERE platform = 'web' AND deleted_at IS NULL ORDER BY id",
        )?
        .query_map([], |r| {
            Ok(Candidate {
                id: r.get(0)?,
                key: r.get(1)?,
                facets: FacetSets::new(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare_cached(
        "SELECT p.id, fj.key, v.value
         FROM posts p,
              json_each(CASE WHEN json_valid(p.ai_web_json) THEN p.ai_web_json END,
                        '$.facets') fj,
              json_each(fj.value) v
         WHERE p.platform = 'web' AND p.deleted_at IS NULL AND v.type = 'text'
         ORDER BY p.id",
    )?;
    let mut rows = stmt.query([])?;
    // `posts` is already `id`-ordered; a linear merge avoids a HashMap (and
    // its unspecified iteration order) for the id -> Candidate lookup.
    let mut at = 0usize;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        while posts.get(at).is_some_and(|c| c.id < id) {
            at += 1;
        }
        let Some(candidate) = posts.get_mut(at).filter(|c| c.id == id) else {
            continue; // defensive: the post vanished between the two queries
        };
        let facet: String = row.get(1)?;
        let value: String = row.get::<_, String>(2)?.trim().to_lowercase();
        if value.is_empty() {
            continue;
        }
        candidate.facets.entry(facet).or_default().push(value);
    }
    Ok(posts)
}

/// The target site's own tie-break swatches ([`color::tie_break_swatches`]),
/// from its current capture's palette; empty for a placeholder.
fn tie_break_swatches(conn: &Connection, post_id: i64) -> Result<Vec<Lab>> {
    Ok(palette_of(conn, post_id)?
        .as_ref()
        .map(color::tie_break_swatches)
        .unwrap_or_default())
}

/// The palette-proximity bonus of a candidate against the target's
/// `targets` (the desktop's tie-break): the average, over `targets`, of the
/// candidate's closest eligible-swatch distance, folded to `max(0, 1 -
/// avg/2)`. A candidate with no eligible swatch (no palette included) scores
/// every target as "infinitely far", so the bonus is `0.0` — matching
/// [`color::palette_distance`]'s `None` folded to [`f64::INFINITY`].
fn palette_bonus(conn: &Connection, targets: &[Lab], candidate_id: i64) -> Result<f64> {
    let Some(palette) = palette_of(conn, candidate_id)? else {
        return Ok(0.0);
    };
    #[allow(clippy::cast_precision_loss)] // at most TIE_BREAK_SWATCHES (3)
    let avg = targets
        .iter()
        .map(|&target| color::palette_distance(&palette, target).unwrap_or(f64::INFINITY))
        .sum::<f64>()
        / targets.len() as f64;
    Ok((1.0 - avg / 2.0).max(0.0))
}

/// The palette of a post's current capture; `None` for a placeholder or a
/// capture with no palette.
fn palette_of(conn: &Connection, post_id: i64) -> Result<Option<Value>> {
    let raw: Option<String> = conn
        .prepare_cached(
            "SELECT wc.palette_json FROM posts p
             LEFT JOIN web_captures wc ON wc.id = p.current_capture_id
             WHERE p.id = ?1",
        )?
        .query_row(params![post_id], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(json_value(raw.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Platform;
    use crate::repo::posts::{AiLayer, NewPost};
    use crate::schema::{self, Kind};
    use serde_json::json;

    const NOW: i64 = 1_790_899_200_000;

    fn library() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn, Kind::Library).unwrap();
        conn
    }

    fn web_post(conn: &Connection, key: &str, facets: Value) -> i64 {
        let mut post = NewPost::new(key, Platform::Web, key, "website", NOW);
        post.ai = Some(AiLayer {
            status: Some("done".into()),
            web: Some(json!({ "facets": facets })),
            ..AiLayer::default()
        });
        posts::insert(conn, &post, NOW).unwrap()
    }

    #[test]
    fn unknown_or_unanalyzed_sites_have_no_similar() {
        let conn = library();
        web_post(&conn, "web_a", json!({ "style": ["minimal"] }));
        assert_eq!(for_site(&conn, "web_missing", 10).unwrap(), Vec::new());

        let mut placeholder = NewPost::new("web_b", Platform::Web, "web_b", "website", NOW);
        placeholder.web_domain = Some("bare.example.test".into());
        posts::insert(&conn, &placeholder, NOW).unwrap();
        assert_eq!(for_site(&conn, "web_b", 10).unwrap(), Vec::new());
    }

    #[test]
    fn scores_and_shared_facets_follow_the_weighted_jaccard() {
        let conn = library();
        web_post(
            &conn,
            "web_target",
            json!({ "style": ["minimal", "bold"], "siteType": ["portfolio"] }),
        );
        // style: {minimal,bold} ∩ {minimal} = 1, union 2 -> 3.0 * 1/2 = 1.5
        web_post(&conn, "web_close", json!({ "style": ["Minimal"] }));
        // siteType: {portfolio} ∩ {portfolio} = 1, union 1 -> 2.0 * 1/1 = 2.0
        web_post(&conn, "web_closer", json!({ "siteType": ["Portfolio"] }));
        web_post(&conn, "web_unrelated", json!({ "tech": ["wordpress"] }));

        let out = for_site(&conn, "web_target", 10).unwrap();
        let keys: Vec<&str> = out.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(
            keys,
            ["web_closer", "web_close"],
            "web_unrelated shares nothing"
        );
        assert_eq!(out[0].score, 2.0);
        assert_eq!(out[0].shared, ["portfolio"]);
        assert_eq!(
            out[0].shared_facets,
            vec![SharedFacet {
                facet: "siteType".into(),
                value: "portfolio".into()
            }]
        );
        assert_eq!(out[1].score, 1.5);
    }

    #[test]
    fn non_string_and_blank_facet_entries_are_skipped() {
        let conn = library();
        web_post(&conn, "web_target", json!({ "style": ["minimal"] }));
        web_post(
            &conn,
            "web_odd",
            json!({ "style": ["minimal", 42, "", "  ", null, true] }),
        );
        let out = for_site(&conn, "web_target", 10).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].shared, ["minimal"]);
    }

    #[test]
    fn limit_is_clamped_and_applied_after_the_bonus_resort() {
        let conn = library();
        web_post(&conn, "web_target", json!({ "style": ["a", "b", "c"] }));
        let styles: [&[&str]; 5] = [&["a"], &["a", "b"], &["a", "b", "c"], &["a"], &["a"]];
        for (i, style) in styles.into_iter().enumerate() {
            web_post(&conn, &format!("web_{i}"), json!({ "style": style }));
        }
        let out = for_site(&conn, "web_target", 2).unwrap();
        assert_eq!(out.len(), 2);
        // The exact match (a,b,c) outranks the partial ones.
        assert_eq!(out[0].key, "web_2");
        assert_eq!(
            for_site(&conn, "web_target", 1000).unwrap().len(),
            5,
            "clamped to MAX_LIMIT, not 1000 candidates"
        );
    }

    #[test]
    fn palette_proximity_breaks_a_tie() {
        let conn = library();
        let target_id = web_post(&conn, "web_target", json!({ "style": ["minimal"] }));
        captures_with_palette(
            &conn,
            target_id,
            json!([{ "hex": "#000000", "role": "background" }]),
        );
        let near_id = web_post(&conn, "web_near", json!({ "style": ["minimal"] }));
        captures_with_palette(
            &conn,
            near_id,
            json!([{ "hex": "#010101", "role": "background" }]),
        );
        let far_id = web_post(&conn, "web_far", json!({ "style": ["minimal"] }));
        captures_with_palette(
            &conn,
            far_id,
            json!([{ "hex": "#ffffff", "role": "background" }]),
        );

        let out = for_site(&conn, "web_target", 10).unwrap();
        assert_eq!(
            out[0].key, "web_near",
            "equal Jaccard score, closer palette wins"
        );
        assert!(out[0].score > out[1].score);
    }

    fn captures_with_palette(conn: &Connection, post_id: i64, palette: Value) {
        use crate::web::captures::{self, CaptureStatus, NewCapture};
        let mut capture = NewCapture::new(NOW);
        capture.status = CaptureStatus::Done;
        capture.palette = Some(palette);
        captures::insert(conn, post_id, &capture, &[], NOW).unwrap();
    }
}
