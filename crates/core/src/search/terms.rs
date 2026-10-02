//! Content-term extraction for search queries: the port of the desktop's
//! `extractContentTerms` and `contentTermsOrRaw` (`electron/db.ts`).
//!
//! The behavior is pinned to the desktop by the golden fixture
//! `shared/golden/extract-content-terms.jsonl` (see `scripts/golden/README.md`):
//! lowercase the query, split it on every run of characters that are neither a
//! Unicode letter (`\p{L}`) nor a number (`\p{N}`), drop stopwords and terms
//! shorter than `min_len` UTF-16 code units (unless whitelisted), and dedupe in
//! order of first appearance.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

/// Minimum term length of the desktop default (`minLen = 3`).
pub const DEFAULT_MIN_LEN: usize = 3;

/// Italian and English stopwords plus query boilerplate (`SEARCH_STOPWORDS` in
/// `electron/db.ts`, same order; the desktop lists `avere` twice).
#[rustfmt::skip]
pub const STOPWORDS: &[&str] = &[
    // Italian articles, prepositions, conjunctions, pronouns, common verbs.
    "di", "a", "da", "in", "con", "su", "per", "tra", "fra", "e", "ed", "o", "od", "ma", "se",
    "né", "ne", "il", "lo", "la", "i", "gli", "le", "un", "uno", "una", "del", "dello", "della",
    "dei", "degli", "delle", "dal", "dallo", "dalla", "dai", "dagli", "dalle", "al", "allo",
    "alla", "ai", "agli", "alle", "nel", "nello", "nella", "nei", "negli", "nelle", "sul",
    "sullo", "sulla", "sui", "sugli", "sulle", "col", "coi", "che", "chi", "cui", "come",
    "dove", "quando", "quale", "quali", "quanto", "più", "meno", "molto", "poco", "tanto",
    "tutto", "tutti", "tutte", "ad", "è", "sono", "sia", "essere", "avere", "fare", "questo",
    "questa", "questi", "queste", "quello", "quella", "quelli", "quelle", "mio", "mia", "miei",
    "mie", "non", "anche", "già", "ancora", "poi", "qui", "qua", "lì", "là",
    // Italian query boilerplate.
    "devo", "voglio", "vorrei", "cerco", "cerca", "cercare", "cercando", "trovo", "trova",
    "trovare", "trovando", "mostra", "mostrami", "dammi", "vedere", "reference", "references",
    "esempio", "esempi", "tipo", "tipi", "qualche", "alcuni", "alcune", "relativo", "relativa",
    "relativi", "relative", "riguardo", "riguardante", "circa", "simile", "simili", "qualcosa",
    "cosa", "cose", "roba",
    // English articles, prepositions, conjunctions.
    "the", "an", "of", "for", "to", "with", "and", "or", "but", "on", "at", "by", "as", "is",
    "are", "be", "this", "that", "these", "those", "it", "its",
    // English query boilerplate.
    "find", "search", "show", "give", "me", "my", "want", "wants", "need", "needs", "like",
    "some", "any", "few", "example", "examples", "sample", "samples", "about", "related",
    "please", "looking", "look", "get", "something", "thing", "things", "stuff", "kind", "sort",
];

/// Short terms that are content despite their length (`SHORT_CONTENT_TERMS`).
pub const SHORT_CONTENT_TERMS: &[&str] = &[
    "3d", "2d", "ai", "ar", "vr", "xr", "ux", "ui", "cg", "r", "go",
];

static STOPWORD_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| STOPWORDS.iter().copied().collect());

static SEPARATORS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]+").expect("separator pattern is valid"));

/// The content terms of `query`, in order of first appearance, deduplicated.
///
/// Mirrors `extractContentTerms(query, { minLen })`. Lengths count UTF-16 code
/// units, as JavaScript's `String.length` does.
#[must_use]
pub fn extract_content_terms(query: &str, min_len: usize) -> Vec<String> {
    let lowered = query.to_lowercase();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for token in SEPARATORS.split(&lowered) {
        // The desktop trims each token; a token holds only letters and numbers,
        // so trimming whitespace never changes it.
        if token.is_empty() || STOPWORD_SET.contains(token) {
            continue;
        }
        let long_enough = token.encode_utf16().count() >= min_len;
        if !long_enough && !SHORT_CONTENT_TERMS.contains(&token) {
            continue;
        }
        if seen.insert(token) {
            out.push(token.to_owned());
        }
    }
    out
}

/// [`extract_content_terms`] with the desktop's fallback (`contentTermsOrRaw`):
/// when no content term survives, the trimmed, lowercased query is the only
/// term. Empty only for a blank query.
#[must_use]
pub fn content_terms_or_raw(query: &str, min_len: usize) -> Vec<String> {
    let terms = extract_content_terms(query, min_len);
    if !terms.is_empty() {
        return terms;
    }
    let raw = js_trim(query).to_lowercase();
    if raw.is_empty() {
        Vec::new()
    } else {
        vec![raw]
    }
}

/// `String.prototype.trim`: strips JavaScript whitespace and line terminators.
/// It differs from `str::trim` on U+FEFF (JS whitespace, not Unicode
/// `White_Space`) and on U+0085 (the other way round).
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_content_terms_in_order() {
        assert_eq!(
            extract_content_terms("Cerco references di AirPods Max 3D per la UX", 3),
            ["airpods", "max", "3d", "ux"]
        );
    }

    #[test]
    fn dedupes_and_drops_short_terms() {
        assert_eq!(extract_content_terms("ok ok go Go xy", 3), ["go"]);
    }

    #[test]
    fn falls_back_to_the_raw_query() {
        assert_eq!(content_terms_or_raw("  Il La  ", 3), ["il la"]);
        assert!(content_terms_or_raw(" \u{FEFF} ", 3).is_empty());
    }

    #[test]
    fn counts_utf16_units() {
        // U+1D49C is one letter but two UTF-16 units: "𝒜𝒜" has length 4 in JS.
        assert_eq!(
            extract_content_terms("\u{1D49C}\u{1D49C}", 3),
            ["\u{1D49C}\u{1D49C}"]
        );
        assert!(extract_content_terms("\u{1D49C}", 3).is_empty());
    }

    #[test]
    fn stopword_list_has_no_unexpected_duplicates() {
        // The desktop list repeats "avere"; the port keeps it once.
        assert_eq!(STOPWORD_SET.len(), STOPWORDS.len());
    }
}
