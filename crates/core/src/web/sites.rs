//! The Websites view (plan §1.2 #6, §2.14, WEB-45; P4-05): listing sites
//! (captured or placeholders) with free text, facets and a colour filter,
//! their faceted counts.
//!
//! # Facets, read live (PG6)
//!
//! The desktop keeps a `post_facets` table, one row per `(post, facet,
//! value)`, built when the AI analysis is written. The web schema has none:
//! a post's facets are its `ai_web_json.facets` (`{facet: [values]}`, plan
//! §2.7), read with SQLite's `json_each` at query time — "no library
//! migration" (PG6). Only JSON string values count; see
//! [`super::similar`]'s module docs for why.
//!
//! # Paging
//!
//! A site's capture time is `posts.sort_ts` (a capture's `insert` mirrors it
//! from `captured_at`, [`super::captures`]), so the default listing
//! (`sort: recent`) is a true keyset scan on `(sort_ts, id)`, which
//! `posts_platform(platform, sort_ts DESC, id DESC)` indexes — no 500 cap,
//! unlike the desktop's `queryWebReferences` (§1.2 #6, WEB-45). `sort: name`
//! is the same kind of keyset, on `lower(coalesce(author_name, web_domain,
//! ''))`.
//!
//! `sort: color` and the `color` filter need the OKLab distance of
//! [`super::color`], which SQL cannot index or evaluate inline without a
//! custom function (and registering one on every pooled connection was out
//! of scope for this card). Whenever `color` parses, [`list`] instead
//! fetches every post matching the other filters, filters and orders them in
//! Rust, and pages the result by offset ([`Cursor::Filtered`]) — recomputed
//! fresh on every request, like [`crate::repo::posts::rank`] does for
//! relevance search. This is the plan-consistent choice where the card does
//! not pick one (an *assumption*, P4 lane rule 8).
//!
//! An unparseable `color` (not `#rrggbb`) is treated as absent, as the
//! desktop's `hexToLab(garbage) === null` does: no filter, and `sort: color`
//! falls back to `recent` (mirrors [`crate::repo::posts::Sort::Relevance`]
//! falling back to `Newest` without search text).

use std::collections::BTreeMap;
use std::fmt;

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, Row, params_from_iter};
use serde::Serialize;
use serde_json::Value;

use super::color::{self, Lab};
use crate::repo::posts::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use crate::repo::{ObjectRef, RepoError, Result, object_columns, object_ref_at};
use crate::search::query::TextQuery;

/// At most this many OR'd values per facet (the desktop's `.slice(0, 30)`).
pub const MAX_FACET_VALUES: usize = 30;

/// Sort order of [`list`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SiteSort {
    /// Most recently captured first (`sort_ts` descending); the default.
    #[default]
    Recent,
    /// `lower(coalesce(author_name, web_domain))` ascending.
    Name,
    /// Closest to `color` first. Falls back to [`Self::Recent`] without a
    /// `color` that parses.
    Color,
}

/// Filters and sort of a sites listing (the desktop's `WebQuery`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteQuery {
    /// Free text: the P1-05 FTS builder restricted to web posts, plus a
    /// domain prefix match (see [`text_filter`]).
    pub q: Option<String>,
    /// Selected facet values, AND across facets, OR within one
    /// (case-insensitive; each list capped to [`MAX_FACET_VALUES`]). A
    /// facet whose values are all blank is dropped, as if not given.
    pub facets: BTreeMap<String, Vec<String>>,
    /// `#rrggbb`; sites with an eligible swatch (background, surface or
    /// accent) within [`color::MAX_MATCH_DISTANCE`] of it.
    pub color: Option<String>,
    /// Sort order.
    pub sort: SiteSort,
}

/// A position in a sites listing. Its text form ([`fmt::Display`],
/// [`Cursor::parse`]) is the opaque cursor the API hands out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cursor {
    /// After this site in `sort: recent` order.
    Recent {
        /// `sort_ts` of the last site of the previous page.
        sort_ts: i64,
        /// Its internal id.
        id: i64,
    },
    /// After this site in `sort: name` order.
    Name {
        /// `lower(coalesce(author_name, web_domain, ''))` of the last site.
        name: String,
        /// Its internal id.
        id: i64,
    },
    /// `color` engaged (`sort: recent`, `name` or `color`): this many sites
    /// of the snapshot already returned (see the module docs).
    Filtered {
        /// Sites already returned.
        offset: u32,
    },
}

impl Cursor {
    /// Parses the text form.
    ///
    /// # Errors
    ///
    /// [`RepoError::InvalidCursor`] for anything [`Cursor`]'s [`fmt::Display`]
    /// did not produce.
    pub fn parse(text: &str) -> Result<Self> {
        let mut parts = text.split('.');
        let kind = parts.next();
        let rest: Vec<&str> = parts.collect();
        let int = |s: &str| s.parse::<i64>().map_err(|_| RepoError::InvalidCursor);
        match (kind, rest.as_slice()) {
            (Some("r"), [ts, id]) => Ok(Self::Recent {
                sort_ts: int(ts)?,
                id: int(id)?,
            }),
            (Some("n"), [name, id]) => Ok(Self::Name {
                name: decode_cursor_text(name).ok_or(RepoError::InvalidCursor)?,
                id: int(id)?,
            }),
            (Some("f"), [offset]) => Ok(Self::Filtered {
                offset: offset.parse().map_err(|_| RepoError::InvalidCursor)?,
            }),
            _ => Err(RepoError::InvalidCursor),
        }
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Recent { sort_ts, id } => write!(f, "r.{sort_ts}.{id}"),
            Self::Name { name, id } => write!(f, "n.{}.{id}", encode_cursor_text(name)),
            Self::Filtered { offset } => write!(f, "f.{offset}"),
        }
    }
}

/// Hex-encodes arbitrary text for a `.`-joined cursor (a site name may
/// contain any character, `.` included). No external dependency: plain hex,
/// not base64.
fn encode_cursor_text(text: &str) -> String {
    text.bytes()
        .fold(String::with_capacity(text.len() * 2), |mut out, b| {
            out.push_str(&format!("{b:02x}"));
            out
        })
}

/// The inverse of [`encode_cursor_text`]; `None` for anything else.
fn decode_cursor_text(hex: &str) -> Option<String> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

/// Which page of a listing to return.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRequest {
    /// Page size, clamped to `1..=`[`MAX_PAGE_SIZE`].
    pub limit: u32,
    /// Where the previous page ended; `None` for the first page.
    pub cursor: Option<Cursor>,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            limit: DEFAULT_PAGE_SIZE,
            cursor: None,
        }
    }
}

/// One page of [`list`].
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    /// The results.
    pub items: Vec<T>,
    /// Cursor of the next page; `None` on the last one.
    #[serde(serialize_with = "serialize_cursor")]
    pub next_cursor: Option<Cursor>,
}

fn serialize_cursor<S: serde::Serializer>(
    c: &Option<Cursor>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match c {
        Some(c) => s.collect_str(c),
        None => s.serialize_none(),
    }
}

/// A site as a listing shows it: captured or a placeholder (P4-04).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteSummary {
    /// Internal row id, for paging; never serialized (sites are addressed by
    /// key).
    #[serde(skip)]
    pub id: i64,
    /// Public id (plan §2.8).
    pub key: String,
    /// The site's URL: the current capture's final URL, else the requested
    /// one, else the URL a bare placeholder was created from.
    pub url: Option<String>,
    /// Domain (`posts.web_domain`).
    pub domain: Option<String>,
    /// Display title (`posts.author_name`: the capture's title, else the
    /// domain — see [`super::captures`]'s module docs).
    pub title: Option<String>,
    /// Capture time (`posts.sort_ts`); the import time for a placeholder.
    pub captured_at: i64,
    /// Number of versions ([`super::captures::list`]).
    pub version_count: i64,
    /// The cover (the current version's hero, else its first page's image),
    /// from a stored object only (§1.2 #10).
    pub hero: Option<ObjectRef>,
    /// The current version's favicon, from a stored object only.
    pub favicon: Option<ObjectRef>,
    /// The current version's palette (JSON as stored; `None` for a
    /// placeholder).
    pub palette: Option<Value>,
    /// The current version's fonts (JSON as stored).
    pub fonts: Option<Value>,
    /// The current version's detected technologies (JSON as stored).
    pub tech: Option<Value>,
    /// Facets (`ai_web_json.facets`), string values only, blanks dropped; a
    /// facet with none left is dropped. Empty until P3's web catalog writes
    /// it (card *Assumption*).
    pub facets: BTreeMap<String, Vec<String>>,
    /// AI lifecycle status.
    pub ai_status: Option<String>,
    /// Archive progress: `pending` (a placeholder) or `done`.
    pub archive_state: String,
}

/// Web posts, captured or placeholders (plan §1.2 #6, WEB-45; P4-05).
///
/// Runs several queries when `query.color` parses: call it inside
/// [`crate::db::UserDb::read`] so they share one snapshot.
///
/// # Errors
///
/// [`RepoError::InvalidCursor`] for a cursor from another sort or filter;
/// database errors otherwise.
pub fn list(conn: &Connection, query: &SiteQuery, page: &PageRequest) -> Result<Page<SiteSummary>> {
    let limit = page.limit.clamp(1, MAX_PAGE_SIZE);
    let target = query.color.as_deref().and_then(color::hex_to_lab);
    let sort = if query.sort == SiteSort::Color && target.is_none() {
        SiteSort::Recent
    } else {
        query.sort
    };
    if let Some(target) = target {
        list_filtered(conn, query, target, sort, limit, page.cursor.as_ref())
    } else {
        list_keyset(conn, query, sort, limit, page.cursor.as_ref())
    }
}

/// Facet value counts (the desktop's `getWebFacetCounts`): a facet's own
/// selection is ignored when counting that facet, so its OR alternatives
/// stay visible, while every *other* active filter (the rest of `facets`,
/// `q`, `color`) still restricts the count.
///
/// # Errors
///
/// Database errors.
pub fn facet_counts(
    conn: &Connection,
    query: &SiteQuery,
) -> Result<BTreeMap<String, Vec<FacetCount>>> {
    let any_filter = non_blank(query.q.as_deref()).is_some()
        || query.color.is_some()
        || !query.facets.is_empty();
    let mut out: BTreeMap<String, Vec<FacetCount>> = BTreeMap::new();
    if !any_filter {
        for row in count_facets(conn, None, None)? {
            out.entry(row.facet).or_default().push(FacetCount {
                value: row.value,
                count: row.count,
            });
        }
    } else {
        let all_ids = candidate_ids(conn, query, None)?;
        for row in count_facets(conn, Some(&all_ids), None)? {
            if query.facets.contains_key(&row.facet) {
                continue; // recomputed below, ignoring this facet's own selection
            }
            out.entry(row.facet).or_default().push(FacetCount {
                value: row.value,
                count: row.count,
            });
        }
        for facet in query.facets.keys() {
            let ids = candidate_ids(conn, query, Some(facet.as_str()))?;
            for row in count_facets(conn, Some(&ids), Some(facet.as_str()))? {
                out.entry(row.facet).or_default().push(FacetCount {
                    value: row.value,
                    count: row.count,
                });
            }
        }
    }
    for values in out.values_mut() {
        values.sort_by_key(|a| std::cmp::Reverse(a.count));
    }
    Ok(out)
}

/// One facet value and how many matching sites carry it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetCount {
    /// Display form: the desktop picks one raw casing per lowercased group
    /// (`MAX(value)`, so the lexicographically greatest — ported as is, the
    /// choice itself does not matter to callers, only that it is stable).
    pub value: String,
    /// Sites carrying it, among the restricted set.
    pub count: i64,
}

// ── True keyset paging (no `color`) ─────────────────────────────────────────

fn list_keyset(
    conn: &Connection,
    query: &SiteQuery,
    sort: SiteSort,
    limit: u32,
    cursor: Option<&Cursor>,
) -> Result<Page<SiteSummary>> {
    let (mut where_sql, mut params) = where_clause(query, None);
    let newest_first;
    match (sort, cursor) {
        (SiteSort::Name, None) => newest_first = false,
        (SiteSort::Name, Some(Cursor::Name { name, id })) => {
            newest_first = false;
            where_sql.push_str(&format!(
                " AND ({NAME_EXPR} > ? OR ({NAME_EXPR} = ? AND p.id > ?))"
            ));
            params.push(SqlValue::Text(name.clone()));
            params.push(SqlValue::Text(name.clone()));
            params.push(SqlValue::Integer(*id));
        }
        (_, None) => newest_first = true,
        (_, Some(Cursor::Recent { sort_ts, id })) => {
            newest_first = true;
            // `sort_ts <= x` (not only the OR) lets SQLite seek the index,
            // as `repo::posts::list_keyset` does.
            where_sql.push_str(" AND (p.sort_ts <= ? AND (p.sort_ts < ? OR p.id < ?))");
            params.push(SqlValue::Integer(*sort_ts));
            params.push(SqlValue::Integer(*sort_ts));
            params.push(SqlValue::Integer(*id));
        }
        (_, Some(_)) => return Err(RepoError::InvalidCursor),
    }
    let order = if newest_first {
        "p.sort_ts DESC, p.id DESC".to_owned()
    } else {
        format!("{NAME_EXPR} ASC, p.id ASC")
    };
    let sql = format!(
        "SELECT {} FROM {SUMMARY_FROM} WHERE {where_sql} ORDER BY {order} LIMIT ?",
        summary_columns()
    );
    params.push(SqlValue::Integer(i64::from(limit) + 1));
    let mut items = query_summaries(conn, &sql, &params)?;
    let next_cursor = if items.len() > limit as usize {
        items.truncate(limit as usize);
        items.last().map(|s| {
            if newest_first {
                Cursor::Recent {
                    sort_ts: s.captured_at,
                    id: s.id,
                }
            } else {
                Cursor::Name {
                    name: name_key(s),
                    id: s.id,
                }
            }
        })
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

/// `lower(coalesce(author_name, web_domain, ''))`, computed in Rust from an
/// already-fetched row (matches [`NAME_EXPR`]).
fn name_key(s: &SiteSummary) -> String {
    s.title
        .clone()
        .or_else(|| s.domain.clone())
        .unwrap_or_default()
        .to_lowercase()
}

const NAME_EXPR: &str = "lower(coalesce(p.author_name, p.web_domain, ''))";

// ── `color` engaged: materialize, filter and page in Rust ──────────────────

/// A candidate of the `color`-engaged path: just enough to filter and order
/// it without paying for the hero/favicon joins and the version-count
/// subquery of every other matching site (only the final page's rows are
/// worth that cost — see [`fetch_by_ids`]).
struct Candidate {
    id: i64,
    sort_ts: i64,
    name: String,
    distance: f64,
}

fn list_filtered(
    conn: &Connection,
    query: &SiteQuery,
    target: Lab,
    sort: SiteSort,
    limit: u32,
    cursor: Option<&Cursor>,
) -> Result<Page<SiteSummary>> {
    let offset = match cursor {
        None => 0,
        Some(Cursor::Filtered { offset }) => *offset,
        Some(_) => return Err(RepoError::InvalidCursor),
    };
    let (where_sql, params) = where_clause(query, None);
    let sql = format!(
        "SELECT p.id, p.sort_ts, {NAME_EXPR}, wc.palette_json
         FROM posts p LEFT JOIN web_captures wc ON wc.id = p.current_capture_id
         WHERE {where_sql}"
    );
    let mut matching: Vec<Candidate> = conn
        .prepare_cached(&sql)?
        .query_map(params_from_iter(params.iter()), |r| {
            let palette: Option<String> = r.get(3)?;
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                palette,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|(id, sort_ts, name, palette)| {
            let distance = palette
                .as_deref()
                .and_then(|p| serde_json::from_str::<Value>(p).ok())
                .and_then(|p| color::palette_distance(&p, target))?;
            (distance <= color::MAX_MATCH_DISTANCE).then_some(Candidate {
                id,
                sort_ts,
                name,
                distance,
            })
        })
        .collect();
    match sort {
        SiteSort::Color => {
            matching.sort_by(|a, b| a.distance.total_cmp(&b.distance).then(a.id.cmp(&b.id)))
        }
        SiteSort::Name => matching.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id))),
        SiteSort::Recent => {
            matching.sort_by(|a, b| b.sort_ts.cmp(&a.sort_ts).then(b.id.cmp(&a.id)));
        }
    }
    let start = (offset as usize).min(matching.len());
    let end = start.saturating_add(limit as usize).min(matching.len());
    let next_cursor = (end < matching.len()).then_some(Cursor::Filtered {
        offset: offset + u32::try_from(end - start).unwrap_or(limit),
    });
    let page_ids: Vec<i64> = matching[start..end].iter().map(|c| c.id).collect();
    let mut by_id = fetch_by_ids(conn, &page_ids)?;
    let items = page_ids.iter().filter_map(|id| by_id.remove(id)).collect();
    Ok(Page { items, next_cursor })
}

/// The full summary of each of `ids` (hero, favicon, version count, …); an id
/// without a post (should not happen: it came from the same snapshot) is
/// simply absent from the map.
fn fetch_by_ids(
    conn: &Connection,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, SiteSummary>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let sql = format!(
        "SELECT {} FROM {SUMMARY_FROM} WHERE p.id IN (SELECT value FROM json_each(?))",
        summary_columns()
    );
    let ids_json = serde_json::to_string(ids).expect("integers serialize");
    Ok(query_summaries(conn, &sql, &[SqlValue::Text(ids_json)])?
        .into_iter()
        .map(|s| (s.id, s))
        .collect())
}

// ── The shared WHERE clause: platform, trash, facets, `q` ───────────────────

/// Builds the WHERE condition (ANDed clauses) matching `query`'s facets and
/// text, always restricted to live web posts. `skip_facet`, when given, omits
/// that one facet's own constraint (ignoring its own selection when counting
/// it, [`facet_counts`]). The `color` filter is applied separately, in Rust.
fn where_clause(query: &SiteQuery, skip_facet: Option<&str>) -> (String, Vec<SqlValue>) {
    let mut clauses = vec![
        "p.platform = 'web'".to_owned(),
        "p.deleted_at IS NULL".to_owned(),
    ];
    let mut params = Vec::new();
    for (facet, values) in &query.facets {
        if skip_facet == Some(facet.as_str()) {
            continue;
        }
        let cleaned: Vec<String> = values
            .iter()
            .map(|v| v.trim().to_lowercase())
            .filter(|v| !v.is_empty())
            .take(MAX_FACET_VALUES)
            .collect();
        if cleaned.is_empty() {
            continue; // as if this facet were not given at all
        }
        let placeholders = vec!["?"; cleaned.len()].join(", ");
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(p.ai_web_json) THEN p.ai_web_json END,
                                             '$.facets') fj, json_each(fj.value) v
                     WHERE fj.key = ? AND v.type = 'text' AND lower(v.value) IN ({placeholders}))"
        ));
        params.push(SqlValue::Text(facet.clone()));
        params.extend(cleaned.into_iter().map(SqlValue::Text));
    }
    if let Some(q) = non_blank(query.q.as_deref()) {
        let (clause, text_params) = text_filter(q);
        clauses.push(clause);
        params.extend(text_params);
    }
    (clauses.join(" AND "), params)
}

/// `q`'s matching clause: the P1-05 FTS builder (prefix and infix matches,
/// restricted to web posts by the caller's own `p.platform = 'web'` clause),
/// OR a domain prefix match on the raw, case-folded query (so "stripe" finds
/// `stripe.com` even though a 2-character FTS prefix needs an indexable
/// token and `%`/`_` are stripped, not escaped, as the desktop's `WEB_SEARCH_COLS`
/// LIKE search already does for its tokens). A query with no indexable
/// character and no domain-prefix candidate matches nothing (like
/// `repo::posts`'s `text_block`), not everything.
fn text_filter(q: &str) -> (String, Vec<SqlValue>) {
    let mut ors = Vec::new();
    let mut params = Vec::new();
    let parsed = TextQuery::parse(q);
    if let Some(m) = parsed.match_expr {
        ors.push("p.id IN (SELECT rowid FROM posts_fts WHERE posts_fts MATCH ?)".to_owned());
        params.push(SqlValue::Text(m));
    }
    if let Some(i) = parsed.infix_expr {
        ors.push("p.id IN (SELECT rowid FROM posts_infix WHERE posts_infix MATCH ?)".to_owned());
        params.push(SqlValue::Text(i));
    }
    let prefix: String = q
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '%' | '_'))
        .collect();
    if !prefix.is_empty() {
        ors.push("p.web_domain LIKE ?".to_owned());
        params.push(SqlValue::Text(format!("{prefix}%")));
    }
    if ors.is_empty() {
        ("0".to_owned(), Vec::new())
    } else {
        (format!("({})", ors.join(" OR ")), params)
    }
}

fn non_blank(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// Ids of the web posts matching `query` (its facets and `q` via
/// [`where_clause`], its `color` in Rust): the restricted set
/// [`facet_counts`] counts within.
fn candidate_ids(
    conn: &Connection,
    query: &SiteQuery,
    skip_facet: Option<&str>,
) -> Result<Vec<i64>> {
    let (where_sql, params) = where_clause(query, skip_facet);
    let target = query.color.as_deref().and_then(color::hex_to_lab);
    if let Some(target) = target {
        let sql = format!(
            "SELECT p.id, wc.palette_json FROM posts p
             LEFT JOIN web_captures wc ON wc.id = p.current_capture_id
             WHERE {where_sql}"
        );
        let rows: Vec<(i64, Option<String>)> = conn
            .prepare_cached(&sql)?
            .query_map(params_from_iter(params.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .filter(|(_, palette)| {
                palette
                    .as_deref()
                    .and_then(|p| serde_json::from_str::<Value>(p).ok())
                    .and_then(|p| color::palette_distance(&p, target))
                    .is_some_and(|d| d <= color::MAX_MATCH_DISTANCE)
            })
            .map(|(id, _)| id)
            .collect())
    } else {
        let sql = format!("SELECT p.id FROM posts p WHERE {where_sql}");
        Ok(conn
            .prepare_cached(&sql)?
            .query_map(params_from_iter(params.iter()), |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
}

/// One `(facet, value, count)` row, grouped case-insensitively (the
/// desktop's `countFor`).
struct FacetCountRow {
    facet: String,
    value: String,
    count: i64,
}

/// Facet counts restricted to `ids` (`None` for every web post), optionally
/// to one facet name (`facet_counts`'s per-selected-facet recompute).
fn count_facets(
    conn: &Connection,
    ids: Option<&[i64]>,
    only_facet: Option<&str>,
) -> Result<Vec<FacetCountRow>> {
    if ids.is_some_and(<[i64]>::is_empty) {
        return Ok(Vec::new());
    }
    let mut clauses = vec![
        "p.platform = 'web'".to_owned(),
        "p.deleted_at IS NULL".to_owned(),
        "v.type = 'text'".to_owned(),
    ];
    let mut params = Vec::new();
    if let Some(ids) = ids {
        clauses.push("p.id IN (SELECT value FROM json_each(?))".to_owned());
        params.push(SqlValue::Text(
            serde_json::to_string(ids).expect("integers serialize"),
        ));
    }
    if let Some(facet) = only_facet {
        clauses.push("fj.key = ?".to_owned());
        params.push(SqlValue::Text(facet.to_owned()));
    }
    let sql = format!(
        "SELECT fj.key AS facet, MAX(v.value) AS value, COUNT(DISTINCT p.id) AS n
         FROM posts p,
              json_each(CASE WHEN json_valid(p.ai_web_json) THEN p.ai_web_json END, '$.facets') fj,
              json_each(fj.value) v
         WHERE {}
         GROUP BY fj.key, LOWER(v.value)",
        clauses.join(" AND ")
    );
    Ok(conn
        .prepare_cached(&sql)?
        .query_map(params_from_iter(params.iter()), |r| {
            Ok(FacetCountRow {
                facet: r.get(0)?,
                value: r.get(1)?,
                count: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

// ── Reading a summary row ───────────────────────────────────────────────────

const SUMMARY_FROM: &str = "posts p
    LEFT JOIN web_captures wc ON wc.id = p.current_capture_id
    LEFT JOIN media_objects o ON o.id = p.cover_object
    LEFT JOIN media_objects f ON f.id = wc.favicon_object";

/// Columns [`row_to_summary`] reads, before the two 8-wide object groups.
const SUMMARY_SCALARS: usize = 10;

fn summary_columns() -> String {
    format!(
        "p.id, p.key, COALESCE(p.web_final_url, p.web_url), p.web_domain, p.author_name,
         p.sort_ts, p.ai_status, p.ai_web_json, p.archive_state,
         (SELECT count(*) FROM web_captures c WHERE c.post_id = p.id),
         wc.palette_json, wc.fonts_json, wc.tech_json, {}, {}",
        object_columns("o"),
        object_columns("f")
    )
}

fn row_to_summary(row: &Row<'_>) -> rusqlite::Result<SiteSummary> {
    let json = |raw: Option<String>| raw.as_deref().and_then(|s| serde_json::from_str(s).ok());
    Ok(SiteSummary {
        id: row.get(0)?,
        key: row.get(1)?,
        url: row.get(2)?,
        domain: row.get(3)?,
        title: row.get(4)?,
        captured_at: row.get(5)?,
        ai_status: row.get(6)?,
        facets: display_facets(row.get::<_, Option<String>>(7)?.as_deref()),
        archive_state: row.get(8)?,
        version_count: row.get(9)?,
        palette: json(row.get(10)?),
        fonts: json(row.get(11)?),
        tech: json(row.get(12)?),
        hero: object_ref_at(row, SUMMARY_SCALARS + 3)?,
        favicon: object_ref_at(row, SUMMARY_SCALARS + 3 + 8)?,
    })
}

/// A string array of `ai_web_json.facets`'s value, blanks dropped; the facet
/// is omitted when nothing is left.
fn display_facets(ai_web_json: Option<&str>) -> BTreeMap<String, Vec<String>> {
    let Some(facets) = ai_web_json
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|v| v.get("facets").and_then(Value::as_object).cloned())
    else {
        return BTreeMap::new();
    };
    facets
        .into_iter()
        .filter_map(|(facet, values)| {
            let values: Vec<String> = values
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
            (!values.is_empty()).then_some((facet, values))
        })
        .collect()
}

fn query_summaries(conn: &Connection, sql: &str, params: &[SqlValue]) -> Result<Vec<SiteSummary>> {
    Ok(conn
        .prepare_cached(sql)?
        .query_map(params_from_iter(params.iter()), row_to_summary)?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Platform;
    use crate::repo::posts::{self, AiLayer, NewPost};
    use crate::schema::{self, Kind};
    use crate::web::captures::{self, CaptureStatus, NewCapture};
    use serde_json::json;

    const NOW: i64 = 1_790_899_200_000;
    const DAY: i64 = 86_400_000;

    fn library() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn, Kind::Library).unwrap();
        conn
    }

    /// A placeholder web post with the given domain/title and capture time.
    fn placeholder(
        conn: &Connection,
        key: &str,
        domain: &str,
        title: Option<&str>,
        at: i64,
    ) -> i64 {
        let mut post = NewPost::new(key, Platform::Web, key, "website", at);
        post.posted_at = Some(at);
        post.web_url = Some(format!("https://{domain}/"));
        post.web_domain = Some(domain.to_owned());
        post.web_final_url = post.web_url.clone();
        post.author_username = Some(domain.to_owned());
        post.author_name = title.map(str::to_owned).or_else(|| Some(domain.to_owned()));
        post.post_url = post.web_url.clone();
        posts::insert(conn, &post, at).unwrap()
    }

    /// Gives `post_id` a current capture with this palette and facets, at
    /// capture time `at` (mirrors `sort_ts`).
    fn capture(
        conn: &Connection,
        post_id: i64,
        at: i64,
        palette: Option<Value>,
        facets: Option<Value>,
    ) {
        let mut c = NewCapture::new(at);
        c.status = CaptureStatus::Done;
        c.palette = palette;
        captures::insert(conn, post_id, &c, &[], at).unwrap();
        if let Some(facets) = facets {
            posts::set_ai(
                conn,
                post_id,
                &AiLayer {
                    status: Some("done".into()),
                    web: Some(json!({ "facets": facets })),
                    ..AiLayer::default()
                },
                at,
            )
            .unwrap();
        }
    }

    fn keys(page: &Page<SiteSummary>) -> Vec<String> {
        page.items.iter().map(|s| s.key.clone()).collect()
    }

    // ── Recent paging ───────────────────────────────────────────────────────

    #[test]
    fn recent_pages_newest_first_with_no_cap() {
        let conn = library();
        for i in 0..5 {
            placeholder(
                &conn,
                &format!("web_{i}"),
                &format!("site{i}.test"),
                None,
                NOW + i * DAY,
            );
        }
        let page1 = list(
            &conn,
            &SiteQuery::default(),
            &PageRequest {
                limit: 2,
                cursor: None,
            },
        )
        .unwrap();
        assert_eq!(keys(&page1), ["web_4", "web_3"]);
        let cursor = page1.next_cursor.clone().unwrap();
        assert_eq!(
            cursor.to_string(),
            Cursor::parse(&cursor.to_string()).unwrap().to_string()
        );

        let page2 = list(
            &conn,
            &SiteQuery::default(),
            &PageRequest {
                limit: 2,
                cursor: page1.next_cursor,
            },
        )
        .unwrap();
        assert_eq!(keys(&page2), ["web_2", "web_1"]);

        let page3 = list(
            &conn,
            &SiteQuery::default(),
            &PageRequest {
                limit: 2,
                cursor: page2.next_cursor,
            },
        )
        .unwrap();
        assert_eq!(keys(&page3), ["web_0"]);
        assert_eq!(page3.next_cursor, None);
    }

    #[test]
    fn recent_cursor_ties_break_on_id() {
        let conn = library();
        // Three sites share one capture time.
        for i in 0..3 {
            placeholder(
                &conn,
                &format!("web_{i}"),
                &format!("tie{i}.test"),
                None,
                NOW,
            );
        }
        let page1 = list(
            &conn,
            &SiteQuery::default(),
            &PageRequest {
                limit: 1,
                cursor: None,
            },
        )
        .unwrap();
        let page2 = list(
            &conn,
            &SiteQuery::default(),
            &PageRequest {
                limit: 1,
                cursor: page1.next_cursor,
            },
        )
        .unwrap();
        assert_ne!(page1.items[0].key, page2.items[0].key);
    }

    #[test]
    fn name_sort_is_case_insensitive_and_falls_back_to_the_domain() {
        let conn = library();
        placeholder(&conn, "web_b", "banana.test", Some("Banana Studio"), NOW);
        placeholder(&conn, "web_a", "apple.test", None, NOW); // no title: falls back to the domain
        placeholder(&conn, "web_c", "cherry.test", Some("apricot kit"), NOW); // lowercase 'a' ties 'Apple'
        let page = list(
            &conn,
            &SiteQuery {
                sort: SiteSort::Name,
                ..Default::default()
            },
            &PageRequest {
                limit: 10,
                cursor: None,
            },
        )
        .unwrap();
        // apple.test, "apricot kit", "Banana Studio": case-folded ascending.
        assert_eq!(keys(&page), ["web_a", "web_c", "web_b"]);
    }

    // ── Facets ──────────────────────────────────────────────────────────────

    #[test]
    fn facets_and_across_facets_or_within_one_case_insensitively() {
        let conn = library();
        let a = placeholder(&conn, "web_a", "a.test", None, NOW);
        capture(
            &conn,
            a,
            NOW,
            None,
            Some(json!({"style": ["Minimal", "Bold"], "siteType": ["portfolio"]})),
        );
        let b = placeholder(&conn, "web_b", "b.test", None, NOW);
        capture(&conn, b, NOW, None, Some(json!({"style": ["minimal"]})));
        let c = placeholder(&conn, "web_c", "c.test", None, NOW);
        capture(
            &conn,
            c,
            NOW,
            None,
            Some(json!({"style": ["bold"], "siteType": ["blog"]})),
        );

        let query = |facets: &[(&str, &[&str])]| SiteQuery {
            facets: facets
                .iter()
                .map(|(f, vs)| {
                    (
                        (*f).to_owned(),
                        vs.iter().map(|v| (*v).to_owned()).collect(),
                    )
                })
                .collect(),
            ..Default::default()
        };
        let run = |q: &SiteQuery| {
            let mut ks = keys(
                &list(
                    &conn,
                    q,
                    &PageRequest {
                        limit: 10,
                        cursor: None,
                    },
                )
                .unwrap(),
            );
            ks.sort();
            ks
        };
        // OR within `style`: minimal OR bold matches a, b, c.
        assert_eq!(
            run(&query(&[("style", &["minimal", "bold"])])),
            ["web_a", "web_b", "web_c"]
        );
        // AND across facets: style=bold AND siteType=portfolio -> only a.
        assert_eq!(
            run(&query(&[
                ("style", &["bold"]),
                ("siteType", &["portfolio"])
            ])),
            ["web_a"]
        );
        // Case-insensitive against the stored casing.
        assert_eq!(run(&query(&[("style", &["MINIMAL"])])), ["web_a", "web_b"]);
        // A facet with no usable values is dropped, matching everything.
        assert_eq!(
            run(&query(&[("style", &["  "])])),
            ["web_a", "web_b", "web_c"]
        );
    }

    #[test]
    fn a_malformed_ai_web_json_is_skipped_not_fatal() {
        let conn = library();
        let ok = placeholder(&conn, "web_ok", "ok.test", None, NOW);
        capture(&conn, ok, NOW, None, Some(json!({"style": ["minimal"]})));
        let bad = placeholder(&conn, "web_bad", "bad.test", None, NOW);
        conn.execute(
            "UPDATE posts SET ai_web_json = 'not json' WHERE id = ?1",
            [bad],
        )
        .unwrap();
        let query = SiteQuery {
            facets: BTreeMap::from([("style".to_owned(), vec!["minimal".to_owned()])]),
            ..Default::default()
        };
        let page = list(
            &conn,
            &query,
            &PageRequest {
                limit: 10,
                cursor: None,
            },
        )
        .unwrap();
        assert_eq!(keys(&page), ["web_ok"]);
        let counts = facet_counts(&conn, &SiteQuery::default()).unwrap();
        assert_eq!(
            counts["style"],
            vec![FacetCount {
                value: "minimal".into(),
                count: 1
            }]
        );
    }

    #[test]
    fn non_string_facet_array_entries_are_skipped() {
        let conn = library();
        let id = placeholder(&conn, "web_a", "a.test", None, NOW);
        capture(
            &conn,
            id,
            NOW,
            None,
            Some(json!({"style": ["minimal", 42, null, true, ""]})),
        );
        let page = list(
            &conn,
            &SiteQuery::default(),
            &PageRequest {
                limit: 10,
                cursor: None,
            },
        )
        .unwrap();
        assert_eq!(page.items[0].facets["style"], vec!["minimal".to_owned()]);
    }

    // ── Faceted counts ──────────────────────────────────────────────────────

    #[test]
    fn facet_counts_ignore_a_facets_own_selection() {
        let conn = library();
        let a = placeholder(&conn, "web_a", "a.test", None, NOW);
        capture(
            &conn,
            a,
            NOW,
            None,
            Some(json!({"style": ["minimal"], "siteType": ["portfolio"]})),
        );
        let b = placeholder(&conn, "web_b", "b.test", None, NOW);
        capture(
            &conn,
            b,
            NOW,
            None,
            Some(json!({"style": ["bold"], "siteType": ["portfolio"]})),
        );
        let c = placeholder(&conn, "web_c", "c.test", None, NOW);
        capture(
            &conn,
            c,
            NOW,
            None,
            Some(json!({"style": ["bold"], "siteType": ["blog"]})),
        );

        // Unfiltered: both style values visible with their full counts.
        let unfiltered = facet_counts(&conn, &SiteQuery::default()).unwrap();
        let mut style = unfiltered["style"].clone();
        style.sort_by(|a, b| a.value.cmp(&b.value));
        assert_eq!(
            style,
            vec![
                FacetCount {
                    value: "bold".into(),
                    count: 2
                },
                FacetCount {
                    value: "minimal".into(),
                    count: 1
                },
            ]
        );

        // Selecting style=bold: siteType counts restrict to {b, c} (portfolio:1, blog:1),
        // but style's OWN alternatives ignore the style selection (minimal:1 stays visible).
        let query = SiteQuery {
            facets: BTreeMap::from([("style".to_owned(), vec!["bold".to_owned()])]),
            ..Default::default()
        };
        let filtered = facet_counts(&conn, &query).unwrap();
        let mut style = filtered["style"].clone();
        style.sort_by(|a, b| a.value.cmp(&b.value));
        assert_eq!(
            style,
            vec![
                FacetCount {
                    value: "bold".into(),
                    count: 2
                },
                FacetCount {
                    value: "minimal".into(),
                    count: 1
                },
            ],
            "style's own selection must not shrink its own counts"
        );
        let mut site_type = filtered["siteType"].clone();
        site_type.sort_by(|a, b| a.value.cmp(&b.value));
        assert_eq!(
            site_type,
            vec![
                FacetCount {
                    value: "blog".into(),
                    count: 1
                },
                FacetCount {
                    value: "portfolio".into(),
                    count: 1
                },
            ],
            "siteType IS restricted by the style=bold selection"
        );
    }

    // ── Colour filter and sort ──────────────────────────────────────────────

    #[test]
    fn color_filters_by_distance_and_an_invalid_hex_is_ignored() {
        let conn = library();
        let near = placeholder(&conn, "web_near", "near.test", None, NOW);
        capture(
            &conn,
            near,
            NOW,
            Some(json!([{ "hex": "#000000", "role": "background" }])),
            None,
        );
        let far = placeholder(&conn, "web_far", "far.test", None, NOW + DAY);
        capture(
            &conn,
            far,
            NOW + DAY,
            Some(json!([{ "hex": "#ffffff", "role": "background" }])),
            None,
        );

        let query = SiteQuery {
            color: Some("#010101".to_owned()),
            ..Default::default()
        };
        let page = list(
            &conn,
            &query,
            &PageRequest {
                limit: 10,
                cursor: None,
            },
        )
        .unwrap();
        assert_eq!(keys(&page), ["web_near"]);

        // An unparseable color is treated as absent: no filter, recent order.
        let garbage = SiteQuery {
            color: Some("not-a-color".to_owned()),
            ..Default::default()
        };
        let page = list(
            &conn,
            &garbage,
            &PageRequest {
                limit: 10,
                cursor: None,
            },
        )
        .unwrap();
        assert_eq!(keys(&page), ["web_far", "web_near"]);
    }

    #[test]
    fn color_sort_orders_by_distance_but_other_sorts_keep_their_order_when_filtered() {
        let conn = library();
        let mid = placeholder(&conn, "web_mid", "mid.test", None, NOW);
        capture(
            &conn,
            mid,
            NOW,
            Some(json!([{ "hex": "#050505", "role": "background" }])),
            None,
        );
        let closest = placeholder(&conn, "web_closest", "closest.test", None, NOW - DAY);
        capture(
            &conn,
            closest,
            NOW - DAY,
            Some(json!([{ "hex": "#000000", "role": "background" }])),
            None,
        );

        let by_color = SiteQuery {
            color: Some("#000000".to_owned()),
            sort: SiteSort::Color,
            ..Default::default()
        };
        assert_eq!(
            keys(
                &list(
                    &conn,
                    &by_color,
                    &PageRequest {
                        limit: 10,
                        cursor: None
                    }
                )
                .unwrap()
            ),
            ["web_closest", "web_mid"],
            "sort=color orders by distance"
        );

        let by_recent = SiteQuery {
            color: Some("#000000".to_owned()),
            sort: SiteSort::Recent,
            ..Default::default()
        };
        assert_eq!(
            keys(
                &list(
                    &conn,
                    &by_recent,
                    &PageRequest {
                        limit: 10,
                        cursor: None
                    }
                )
                .unwrap()
            ),
            ["web_mid", "web_closest"],
            "sort=recent keeps recency order even though color filters both in"
        );
    }

    #[test]
    fn color_sort_without_a_color_falls_back_to_recent() {
        let conn = library();
        placeholder(&conn, "web_old", "old.test", None, NOW - DAY);
        placeholder(&conn, "web_new", "new.test", None, NOW);
        let query = SiteQuery {
            sort: SiteSort::Color,
            ..Default::default()
        };
        let page = list(
            &conn,
            &query,
            &PageRequest {
                limit: 10,
                cursor: None,
            },
        )
        .unwrap();
        assert_eq!(keys(&page), ["web_new", "web_old"]);
    }

    #[test]
    fn filtered_paging_offsets_a_recomputed_snapshot() {
        let conn = library();
        for i in 0..4 {
            let id = placeholder(
                &conn,
                &format!("web_{i}"),
                &format!("c{i}.test"),
                None,
                NOW + i * DAY,
            );
            capture(
                &conn,
                id,
                NOW + i * DAY,
                Some(json!([{ "hex": "#000000", "role": "background" }])),
                None,
            );
        }
        let query = SiteQuery {
            color: Some("#000000".to_owned()),
            ..Default::default()
        };
        let page1 = list(
            &conn,
            &query,
            &PageRequest {
                limit: 2,
                cursor: None,
            },
        )
        .unwrap();
        assert_eq!(keys(&page1), ["web_3", "web_2"]);
        let page2 = list(
            &conn,
            &query,
            &PageRequest {
                limit: 2,
                cursor: page1.next_cursor,
            },
        )
        .unwrap();
        assert_eq!(keys(&page2), ["web_1", "web_0"]);
        assert_eq!(page2.next_cursor, None);
    }

    // ── `q` (plain Rust behavior, not golden: see the module docs) ──────────

    #[test]
    fn q_matches_fts_text_or_a_domain_prefix() {
        let conn = library();
        let a = placeholder(&conn, "web_a", "unrelated.test", Some("Atelier Lumen"), NOW);
        capture(&conn, a, NOW, None, None);
        placeholder(&conn, "web_b", "studiolumen.example", None, NOW);

        let by_text = SiteQuery {
            q: Some("Lumen".to_owned()),
            ..Default::default()
        };
        let mut found = keys(
            &list(
                &conn,
                &by_text,
                &PageRequest {
                    limit: 10,
                    cursor: None,
                },
            )
            .unwrap(),
        );
        found.sort();
        assert_eq!(
            found,
            ["web_a", "web_b"],
            "the title and the domain both index 'lumen'"
        );

        let by_domain_prefix = SiteQuery {
            q: Some("studiolu".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            keys(
                &list(
                    &conn,
                    &by_domain_prefix,
                    &PageRequest {
                        limit: 10,
                        cursor: None
                    }
                )
                .unwrap()
            ),
            ["web_b"],
            "too short to be an FTS prefix term, but a domain prefix"
        );
    }

    #[test]
    fn q_without_any_indexable_character_matches_nothing() {
        let conn = library();
        placeholder(&conn, "web_a", "a.test", None, NOW);
        let query = SiteQuery {
            q: Some("!!!".to_owned()),
            ..Default::default()
        };
        let page = list(
            &conn,
            &query,
            &PageRequest {
                limit: 10,
                cursor: None,
            },
        )
        .unwrap();
        assert_eq!(page.items, Vec::new());
    }
}
