//! Offline chat protocol and deterministic fallback; no provider calls or prose.
use crate::repo::RepoError;
use crate::search::{
    terms,
    vocab::{Vocabulary, normalize, normalize_spaces, source_predicate},
};
use regex::Regex;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::LazyLock;

pub const PER_TIER_CAP: usize = 15;
pub const MAX_KEYWORDS: usize = 6;

fn body<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let from = text.find(open)? + open.len();
    let tail = &text[from..];
    Some(&tail[..tail.find(close).unwrap_or(tail.len())])
}
static TAG_SEPARATOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[,\n]+|[\t-\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]{2,}").unwrap()
});
/// Desktop's raw allowlisted parser; the composed protocol applies tier caps.
#[must_use]
pub fn parse_tag_block(
    text: &str,
    open: &str,
    close: &str,
    vocab: &HashSet<String>,
) -> Vec<String> {
    let Some(body) = body(text, open, close) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    TAG_SEPARATOR
        .split(body)
        .map(|s| normalize(terms::js_trim(s).trim_start_matches('#')))
        .filter(|t| !t.is_empty() && vocab.contains(t) && seen.insert(t.clone()))
        .collect()
}
/// Free keyword phrases: six at most, four words and 2..40 UTF-16 units each.
#[must_use]
pub fn parse_keyword_block(text: &str, open: &str, close: &str) -> Vec<String> {
    let Some(body) = body(text, open, close) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    body.split([',', '\n'])
        .map(|s| normalize_spaces(terms::js_trim(s).trim_start_matches('#')).to_lowercase())
        .filter(|s| {
            (2..=40).contains(&s.encode_utf16().count())
                && s.split(' ').count() <= 4
                && seen.insert(s.clone())
        })
        .take(MAX_KEYWORDS)
        .collect()
}
#[must_use]
pub fn deterministic_keywords(message: &str) -> Vec<String> {
    terms::extract_content_terms(message, 3)
        .into_iter()
        .take(MAX_KEYWORDS)
        .collect()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagGroups {
    pub broad: Vec<String>,
    pub specific: Vec<String>,
}
/// Parses the desktop GENERAL/SPECIFIC (or legacy TAGS) blocks against the
/// offered vocabulary; overlap and active tags are
/// excluded globally, and caps apply after filtering.
#[must_use]
pub fn parse_tags(
    text: &str,
    broad: &HashSet<String>,
    specific: &HashSet<String>,
    active: &HashSet<String>,
) -> TagGroups {
    let mut seen = active
        .iter()
        .map(|tag| normalize(tag))
        .collect::<HashSet<_>>();
    let vocab = broad.union(specific).map(|tag| normalize(tag)).collect();
    let mut broad = parse_tag_block(text, "[[GENERAL]]", "[[/GENERAL]]", &vocab);
    let specific = parse_tag_block(text, "[[SPECIFIC]]", "[[/SPECIFIC]]", &vocab);
    if broad.is_empty() && specific.is_empty() {
        broad = parse_tag_block(text, "[[TAGS]]", "[[/TAGS]]", &vocab);
    }
    let broad = broad
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .take(PER_TIER_CAP)
        .collect();
    let specific = specific
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .take(PER_TIER_CAP)
        .collect();
    TagGroups { broad, specific }
}
/// Message substring, active-tag cooccurrence, then lexical archive matches.
pub fn deterministic_tag_matches(
    conn: &Connection,
    vocab: &Vocabulary,
    message: &str,
    active: &[String],
    broad_vocab: &[String],
) -> Result<TagGroups, RepoError> {
    let mut seen: HashSet<String> = active
        .iter()
        .map(|t| normalize(t))
        .filter(|t| !t.is_empty())
        .collect();
    let mut out = TagGroups::default();
    let msg = message.to_lowercase();
    let push = |list: &mut Vec<String>, seen: &mut HashSet<String>, tag: &str| {
        let t = normalize(tag);
        if list.len() < PER_TIER_CAP && !t.is_empty() && seen.insert(t.clone()) {
            list.push(t);
        }
    };
    for tag in broad_vocab {
        let t = normalize(tag);
        if t.encode_utf16().count() >= 2 && msg.contains(&t) {
            push(&mut out.broad, &mut seen, &t);
        }
    }
    let mut active_seen = HashSet::new();
    for tag in active {
        let tag = normalize(tag);
        if tag.is_empty() || !active_seen.insert(tag.clone()) {
            continue;
        }
        let predicate = source_predicate(vocab.source());
        let rows = conn
            .prepare(&format!(
                "SELECT b.tag_norm
                 FROM post_tags a
                 JOIN post_tags b ON b.post_id=a.post_id
                 JOIN posts p ON p.id=a.post_id
                 WHERE a.tag_norm=?1 AND b.tag_norm<>?1 AND p.deleted_at IS NULL AND {predicate}
                 GROUP BY b.tag_norm
                 ORDER BY COUNT(DISTINCT a.post_id) DESC,b.tag_norm DESC
                 LIMIT 8"
            ))?
            .query_map(params![tag], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for related in rows {
            push(&mut out.broad, &mut seen, vocab.display(&related));
        }
    }
    for tag in vocab.lexical(message, PER_TIER_CAP) {
        push(&mut out.specific, &mut seen, &tag);
    }
    Ok(out)
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplyCode {
    Suggestions,
    NoMatches,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Fallback {
    pub tags: TagGroups,
    pub keywords: Vec<String>,
    pub reply_code: ReplyCode,
}
pub fn fallback(
    conn: &Connection,
    vocab: &Vocabulary,
    message: &str,
    active: &[String],
) -> Result<Fallback, RepoError> {
    let tags = deterministic_tag_matches(conn, vocab, message, active, &vocab.broad())?;
    let keywords = deterministic_keywords(message);
    let reply_code = if tags.broad.is_empty() && tags.specific.is_empty() && keywords.is_empty() {
        ReplyCode::NoMatches
    } else {
        ReplyCode::Suggestions
    };
    Ok(Fallback {
        tags,
        keywords,
        reply_code,
    })
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Turn {
    pub role: String,
    pub content: String,
}
/// Valid user/assistant turns, newest eight, then oldest-first removal until
/// their UTF-16 content fits the 6k-token estimate (four units per token).
#[must_use]
pub fn truncate_history(turns: &[Turn]) -> Vec<Turn> {
    let mut out: Vec<_> = turns
        .iter()
        .filter(|t| matches!(t.role.as_str(), "user" | "assistant"))
        .rev()
        .take(8)
        .cloned()
        .collect();
    out.reverse();
    let mut size: usize = out.iter().map(|t| t.content.encode_utf16().count()).sum();
    while size > 24_000 && out.len() > 1 {
        size -= out.remove(0).content.encode_utf16().count();
    }
    if size > 24_000 {
        // Keep the latest message's end without a split surrogate.
        let mut suffix = String::new();
        let mut units = 0;
        for ch in out[0].content.chars().rev() {
            let n = ch.len_utf16();
            if units + n > 24_000 {
                break;
            }
            units += n;
            suffix.push(ch);
        }
        out[0].content = suffix.chars().rev().collect();
    }
    out
}
