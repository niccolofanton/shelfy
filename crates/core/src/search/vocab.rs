//! Offline chat retrieval: generation-cached tag vocabulary and indexed pools.
//! Counts use distinct live posts, so a manual/AI overlap contributes once.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use rusqlite::{Connection, params};
use serde::Serialize;

use super::{query, terms};
use crate::db::UserDb;
use crate::generation::GenerationCache;
use crate::repo::{RepoError, tags};

pub const BROAD_LIMIT: usize = 150;
pub const SPECIFIC_LIMIT: usize = 60;

#[derive(Clone, Debug)]
struct Tag {
    norm: String,
    form: String,
    count: usize,
    general: usize,
    specific: bool,
}

/// One immutable library snapshot. Load inside the caller's read transaction.
#[derive(Clone, Debug, Default)]
pub struct Vocabulary {
    tags: Vec<Tag>,
    keyword_tokens: HashMap<String, usize>,
    keyword_total: usize,
    posts: usize,
}

/// Bounded per-user cache. Read generation before opening the snapshot.
pub struct VocabCache(GenerationCache<Arc<Vocabulary>>);
impl Default for VocabCache {
    fn default() -> Self {
        Self(GenerationCache::new(64, Duration::from_secs(300)))
    }
}
impl VocabCache {
    /// Returns the current snapshot; database errors leave no cached value.
    pub fn get(&self, user: &str, db: &UserDb) -> Result<Arc<Vocabulary>, RepoError> {
        let generation = db.generation();
        let view = *b"chat-vocab-v1___";
        if let Some(value) = self.0.get(user, generation, &view) {
            return Ok(value);
        }
        let value = Arc::new(db.read(Vocabulary::load)?);
        self.0.insert(user, generation, view, value.clone());
        Ok(value)
    }
}

/// A tag ranked by weighted mass and lift within the query's indexed posts.
#[derive(Clone, Debug)]
pub struct DistinctTag {
    pub tag: String,
    pub lift: f64,
    pub score: f64,
}

/// Complete prompt pools. Tags remain display forms, keywords remain phrases.
#[derive(Clone, Debug, Serialize)]
pub struct Pools {
    pub broad: Vec<String>,
    pub specific: Vec<String>,
    pub keywords: Vec<String>,
}

impl Vocabulary {
    /// Builds counts, display forms, tiers and keyword statistics in one snapshot.
    pub fn load(conn: &Connection) -> Result<Self, RepoError> {
        let mut tags = Vec::new();
        let mut stmt = conn.prepare("SELECT t.tag_norm, COUNT(DISTINCT t.post_id), COUNT(DISTINCT CASE WHEN t.tier='general' THEN t.post_id END), MAX(t.tier='specific')
                 FROM post_tags t
                 JOIN posts p ON p.id=t.post_id
                 WHERE p.deleted_at IS NULL
                 GROUP BY t.tag_norm")?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)? as usize,
                r.get::<_, u32>(2)? as usize,
                r.get::<_, Option<bool>>(3)?.unwrap_or(false),
            ))
        })? {
            let (norm, count, general, specific) = row?;
            tags.push(Tag {
                form: norm.clone(),
                norm,
                count,
                general,
                specific,
            });
        }
        let forms: HashMap<String,String> = conn.prepare("SELECT tag_norm, tag_form
                 FROM (SELECT t.tag_norm,t.tag_form,COUNT(DISTINCT t.post_id) n, ROW_NUMBER() OVER (PARTITION BY t.tag_norm
                 ORDER BY COUNT(DISTINCT t.post_id) DESC,t.tag_form) rank
                 FROM post_tags t
                 JOIN posts p ON p.id=t.post_id
                 WHERE p.deleted_at IS NULL
                 GROUP BY t.tag_norm,t.tag_form)
                 WHERE rank=1")?.query_map([], |r| Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        for tag in &mut tags {
            if let Some(form) = forms.get(&tag.norm) {
                tag.form.clone_from(form);
            }
        }
        let mut keyword_tokens = HashMap::new();
        let rows = conn.prepare("SELECT ai_keywords_json FROM posts WHERE deleted_at IS NULL AND ai_keywords_json IS NOT NULL")?.query_map([], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for raw in rows {
            for phrase in keyword_phrases(&raw) {
                for token in keyword_tokens_of(&phrase) {
                    *keyword_tokens.entry(token).or_default() += 1;
                }
            }
        }
        let keyword_total = keyword_tokens.values().sum();
        let posts = conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE deleted_at IS NULL",
            [],
            |r| Ok(r.get::<_, u32>(0)? as usize),
        )?;
        Ok(Self {
            tags,
            keyword_tokens,
            keyword_total,
            posts,
        })
    }

    pub(crate) fn display<'a>(&'a self, norm: &'a str) -> &'a str {
        self.tags
            .iter()
            .find(|t| t.norm == norm)
            .map_or(norm, |t| t.form.as_str())
    }

    /// Top general-tier tags, falling back to all tiers for older libraries.
    #[must_use]
    pub fn broad(&self) -> Vec<String> {
        let general = self.tags.iter().any(|t| t.general > 0);
        let mut pick: Vec<_> = self
            .tags
            .iter()
            .filter(|t| !general || t.general > 0)
            .collect();
        pick.sort_by(|a, b| {
            (if general { b.general } else { b.count })
                .cmp(&(if general { a.general } else { a.count }))
                .then_with(|| a.norm.cmp(&b.norm))
        });
        pick.into_iter()
            .take(BROAD_LIMIT)
            .map(|t| t.form.clone())
            .collect()
    }

    /// Lexical matches scan the cached names, never the post corpus.
    #[must_use]
    pub fn lexical(&self, text: &str, limit: usize) -> Vec<String> {
        let words = terms::content_terms_or_raw(text, 3);
        let mut pick: Vec<_> = self
            .tags
            .iter()
            .filter(|t| words.iter().any(|word| t.norm.contains(word)))
            .collect();
        pick.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.norm.cmp(&b.norm)));
        pick.into_iter()
            .take(limit)
            .map(|t| t.form.clone())
            .collect()
    }

    /// Canonical exact/accepted-alias matches, then the first lexical match.
    pub fn intersect(
        &self,
        conn: &Connection,
        candidates: &[String],
    ) -> Result<Vec<String>, RepoError> {
        let real: HashSet<_> = self.tags.iter().map(|t| t.norm.as_str()).collect();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for raw in candidates {
            let norm = normalize(raw);
            if norm.is_empty() {
                continue;
            }
            let canonical = tags::resolve_alias(conn, &norm)?;
            if real.contains(canonical.norm.as_str()) {
                if seen.insert(canonical.norm) {
                    out.push(terms::js_trim(&canonical.form).to_owned());
                }
                continue;
            }
            for form in self.lexical(&norm, 5) {
                let canonical = tags::resolve_alias(conn, &normalize(&form))?;
                if real.contains(canonical.norm.as_str()) {
                    if seen.insert(canonical.norm) {
                        out.push(terms::js_trim(&canonical.form).to_owned());
                    }
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Distinctive tags on indexed matching posts. Rare-term singleton and
    /// mass/lift gates mirror the desktop, without its full-corpus LIKE scans.
    pub fn distinctive(
        &self,
        conn: &Connection,
        text: &str,
        limit: usize,
    ) -> Result<Vec<DistinctTag>, RepoError> {
        let mut weights: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
        let mut max_idf: f64 = 0.0;
        for term in terms::content_terms_or_raw(text, 3) {
            let ids = indexed_matches(conn, &term, 4000)?;
            let df = indexed_count(conn, &term)?;
            if df == 0 {
                continue;
            }
            let idf = ((self.posts as f64 + 1.0) / (df as f64 + 1.0)).ln();
            max_idf = max_idf.max(idf);
            for id in ids {
                let w = weights.entry(id).or_default();
                w.0 += idf;
                w.1 = w.1.max(idf);
            }
        }
        if weights.is_empty() {
            return Ok(Vec::new());
        }
        let ids =
            serde_json::to_string(&weights.keys().collect::<Vec<_>>()).expect("ids serialize");
        let pairs = conn.prepare("SELECT DISTINCT post_id,tag_norm FROM post_tags WHERE post_id IN (SELECT value FROM json_each(?1)) ORDER BY tag_norm,post_id")?.query_map([ids], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut mass: HashMap<String, (usize, f64, f64)> = HashMap::new();
        for (id, tag) in pairs {
            let m = mass.entry(tag).or_default();
            m.0 += 1;
            m.1 += weights[&id].0;
            m.2 = m.2.max(weights[&id].1);
        }
        let mut out = Vec::new();
        for tag in &self.tags {
            let Some(&(n, weight, best)) = mass.get(&tag.norm) else {
                continue;
            };
            let lift = n as f64 / tag.count as f64;
            if lift < 0.12 || (n >= 2 && lift < 0.2) || (n < 2 && best < max_idf * 0.9) {
                continue;
            }
            out.push(DistinctTag {
                tag: tag.form.clone(),
                lift,
                score: weight * lift.powi(3),
            });
        }
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| b.lift.total_cmp(&a.lift))
                .then_with(|| normalize(&a.tag).cmp(&normalize(&b.tag)))
        });
        out.truncate(limit);
        Ok(out)
    }

    pub fn expand(
        &self,
        conn: &Connection,
        text: &str,
        limit: usize,
    ) -> Result<Vec<String>, RepoError> {
        if terms::js_trim(text).is_empty() {
            return Ok(Vec::new());
        }
        let mut candidates: Vec<_> = self
            .distinctive(conn, text, limit)?
            .into_iter()
            .map(|t| t.tag)
            .collect();
        candidates.extend(terms::extract_content_terms(text, 3));
        let mut out = self.intersect(conn, &candidates)?;
        out.truncate(limit);
        Ok(out)
    }

    /// Chat-search pool assembly, including query expansion after the primary
    /// harvest. Active aliases resolve before exclusions are applied.
    pub fn pools(
        &self,
        conn: &Connection,
        text: &str,
        active: &[String],
    ) -> Result<Pools, RepoError> {
        let broad = self.broad();
        let mut seen: HashSet<_> = broad.iter().map(|t| normalize(t)).collect();
        for tag in active {
            seen.insert(tags::resolve_alias(conn, &normalize(tag))?.norm);
        }
        let mut specific = self.specific(conn, text, &seen)?;
        seen.extend(specific.iter().map(|t| normalize(t)));
        for tag in self.expand(conn, text, SPECIFIC_LIMIT)? {
            if specific.len() >= SPECIFIC_LIMIT {
                break;
            }
            if seen.insert(normalize(&tag)) {
                specific.push(tag);
            }
        }
        let keywords = self.keywords(conn, text, 12)?;
        Ok(Pools {
            broad,
            specific,
            keywords,
        })
    }

    /// Specific-tier hits first, then distinctive tags and lift-gated names.
    /// Callers pass both the broad pool and active tags in `exclude`.
    pub fn specific(
        &self,
        conn: &Connection,
        text: &str,
        exclude: &HashSet<String>,
    ) -> Result<Vec<String>, RepoError> {
        let distinct = self.distinctive(conn, text, SPECIFIC_LIMIT)?;
        let specific: HashSet<_> = self
            .tags
            .iter()
            .filter(|t| t.specific)
            .map(|t| t.norm.as_str())
            .collect();
        let lift: HashMap<_, _> = distinct
            .iter()
            .map(|t| (normalize(&t.tag), t.lift))
            .collect();
        let mut candidates = Vec::new();
        candidates.extend(
            distinct
                .iter()
                .filter(|t| specific.contains(normalize(&t.tag).as_str()))
                .map(|t| t.tag.clone()),
        );
        candidates.extend(distinct.iter().map(|t| t.tag.clone()));
        candidates.extend(self.lexical(text, SPECIFIC_LIMIT).into_iter().filter(|t| {
            distinct.is_empty() || lift.get(&normalize(t)).is_some_and(|v| *v >= 0.12)
        }));
        let mut seen = exclude.clone();
        Ok(candidates
            .into_iter()
            .filter(|t| seen.insert(normalize(t)))
            .take(SPECIFIC_LIMIT)
            .collect())
    }

    /// Keyword phrase retrieval from at most 600 indexed matching posts.
    pub fn keywords(
        &self,
        conn: &Connection,
        text: &str,
        limit: usize,
    ) -> Result<Vec<String>, RepoError> {
        let mut ids = HashSet::new();
        for term in terms::content_terms_or_raw(text, 3) {
            ids.extend(indexed_matches(conn, &term, 600)?);
        }
        let raw = serde_json::to_string(&ids).expect("ids serialize");
        let rows = conn.prepare("SELECT ai_keywords_json FROM posts WHERE id IN (SELECT value FROM json_each(?1)) AND ai_keywords_json IS NOT NULL ORDER BY id LIMIT 600")?.query_map([raw], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut phrases: BTreeMap<String, (String, usize)> = BTreeMap::new();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for raw in rows {
            for phrase in keyword_phrases(&raw) {
                let norm = phrase.to_lowercase();
                let item = phrases.entry(norm).or_insert((phrase.clone(), 0));
                item.1 += 1;
                for token in keyword_tokens_of(&phrase) {
                    *counts.entry(token).or_default() += 1;
                }
            }
        }
        let words = terms::content_terms_or_raw(text, 3);
        let mut scored = Vec::new();
        for (norm, (form, _)) in phrases {
            let toks = keyword_tokens_of(&norm);
            if toks.is_empty() {
                continue;
            }
            let mut coverage = 0.0;
            let mut distinct = 0.0;
            for token in &toks {
                let global = self.keyword_tokens.get(token).copied().unwrap_or(0);
                if words.iter().any(|q| token.contains(q) || q.contains(token)) {
                    coverage += ((self.keyword_total as f64 + 1.0) / (global as f64 + 1.0)).ln();
                }
                let n = counts.get(token).copied().unwrap_or(0);
                if n >= 2 {
                    distinct += n as f64 * (n as f64 / global.max(1) as f64).powf(3.2);
                }
            }
            let score = (coverage + 0.2 * distinct) / (toks.len() as f64).sqrt();
            if score > 0.0 {
                scored.push((norm, form, score));
            }
        }
        scored.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        let mut out: Vec<String> = Vec::new();
        for (norm, form, _) in scored {
            if out.len() >= limit {
                break;
            }
            let norm = normalize_spaces(&norm);
            if !out
                .iter()
                .any(|p| normalize_spaces(&p.to_lowercase()).contains(&norm))
            {
                out.push(form);
            }
        }
        Ok(out)
    }
}

pub(crate) fn normalize(text: &str) -> String {
    terms::js_trim(text).to_lowercase()
}
static SPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"[\t-\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]+",
    )
    .unwrap()
});
pub(crate) fn normalize_spaces(text: &str) -> String {
    SPACE.replace_all(text, " ").trim().to_owned()
}
static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").unwrap());
fn keyword_tokens_of(text: &str) -> HashSet<String> {
    TOKEN
        .find_iter(&text.to_lowercase())
        .map(|m| m.as_str().to_owned())
        .filter(|t| {
            t.encode_utf16().count() >= 3 || terms::SHORT_CONTENT_TERMS.contains(&t.as_str())
        })
        .collect()
}
fn keyword_phrases(raw: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str())
        .map(terms::js_trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn match_sql(term: &str) -> Option<(String, String)> {
    let prefix = query::prefix_unit(term)?;
    // The infix index includes authors/entities as well: retain its established
    // search contract rather than maintain a second substring corpus.
    let infix = query::infix_unit(term);
    Some((
        format!("{{tags keywords description note caption}}: ({prefix})"),
        infix.unwrap_or_else(|| query::quote("\u{1}")),
    ))
}
fn indexed_matches(conn: &Connection, term: &str, limit: usize) -> rusqlite::Result<Vec<i64>> {
    let Some((prefix, infix)) = match_sql(term) else {
        return Ok(Vec::new());
    };
    conn.prepare_cached(
        "SELECT p.id
                 FROM posts p
                 WHERE p.id IN (SELECT rowid
                 FROM posts_fts
                 WHERE posts_fts MATCH ?1 UNION SELECT rowid
                 FROM posts_infix
                 WHERE posts_infix MATCH ?2)
                 ORDER BY COALESCE(p.posted_at,p.imported_at) DESC,p.id ASC
                 LIMIT ?3",
    )?
    .query_map(
        params![prefix, infix, i64::try_from(limit).unwrap_or(i64::MAX)],
        |r| r.get(0),
    )?
    .collect()
}
fn indexed_count(conn: &Connection, term: &str) -> rusqlite::Result<usize> {
    let Some((prefix, infix)) = match_sql(term) else {
        return Ok(0);
    };
    conn.query_row("SELECT COUNT(*) FROM (SELECT rowid FROM posts_fts WHERE posts_fts MATCH ?1 UNION SELECT rowid FROM posts_infix WHERE posts_infix MATCH ?2)",params![prefix,infix],|r| Ok(r.get::<_,u32>(0)? as usize))
}
