//! FTS5 query building for post search (plan §2.14).
//!
//! A free-text query becomes its content terms ([`super::terms`]) and then an
//! FTS5 expression `{columns}: ("t1"* OR "t2"* …)`: every term is a quoted
//! string (so FTS5 operators in user input stay literal) with a prefix match.
//!
//! The ranking constants live here so SPIKE-5 (T4) can tune them in one place;
//! the SQL that applies them is in `repo::posts`.

use std::sync::LazyLock;

use regex::Regex;

use super::terms::{DEFAULT_MIN_LEN, content_terms_or_raw};

/// Columns of `posts_fts`, in table order (the order of the bm25 weights).
pub const COLUMNS: [&str; 8] = [
    "tags",
    "keywords",
    "entities",
    "description",
    "note",
    "caption",
    "author",
    "web_text",
];

/// bm25 weight per column of [`COLUMNS`]. They mirror the desktop's CASE weights
/// (tag/keyword 5–6, description/note 4, caption 3.5). Tuned by SPIKE-5.
pub const BM25_WEIGHTS: [f64; 8] = [6.0, 5.0, 4.5, 4.0, 4.0, 3.5, 2.0, 2.0];

/// Score bonus per query term that equals one of the post's tags. bm25 is
/// lower-is-better, so bonuses are subtracted.
pub const EXACT_TAG_BOOST: f64 = 3.0;

/// Score bonus when the whole query appears as a phrase.
pub const PHRASE_BONUS: f64 = 1.5;

/// Relevance results are paged by offset inside the first this-many results.
pub const RELEVANCE_WINDOW: u32 = 1000;

static HAS_TOKEN_CHAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]").expect("token pattern is valid"));
static TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").expect("token pattern is valid"));

/// A free-text query prepared for FTS5.
#[derive(Clone, Debug, PartialEq)]
pub struct TextQuery {
    /// Lowercased content terms (also the exact-tag boost candidates).
    pub terms: Vec<String>,
    /// Prefix match on any term; `None` when no term holds an indexable character,
    /// in which case the query matches nothing.
    pub match_expr: Option<String>,
    /// The whole query as one phrase, for the phrase bonus; `None` for queries of
    /// fewer than two tokens, where it would just repeat the term match.
    pub phrase_expr: Option<String>,
}

impl TextQuery {
    /// Prepares `query`: content terms with the raw-query fallback (as the desktop
    /// search does), the prefix-match expression and the phrase expression.
    #[must_use]
    pub fn parse(query: &str) -> Self {
        let terms = content_terms_or_raw(query, DEFAULT_MIN_LEN);
        let units: Vec<String> = terms.iter().filter_map(|t| prefix_unit(t)).collect();
        let match_expr = any_of(&units);
        let trimmed = query.trim();
        let phrase_expr = (TOKEN.find_iter(trimmed).count() >= 2)
            .then(|| format!("{}: {}", column_filter(), quote(trimmed)));
        Self {
            terms,
            match_expr,
            phrase_expr,
        }
    }
}

/// FTS5 string literal: double quotes, inner quotes doubled.
#[must_use]
pub fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

/// One prefix-matched unit (`"text"*`), or `None` when `text` has no character
/// the tokenizer indexes (FTS5 rejects an empty prefix phrase).
#[must_use]
pub fn prefix_unit(text: &str) -> Option<String> {
    let text = text.trim();
    HAS_TOKEN_CHAR
        .is_match(text)
        .then(|| format!("{}*", quote(text)))
}

/// `{columns}: (u1 OR u2 …)`, or `None` without units.
#[must_use]
pub fn any_of(units: &[String]) -> Option<String> {
    if units.is_empty() {
        return None;
    }
    Some(format!("{}: ({})", column_filter(), units.join(" OR ")))
}

/// The bm25 call with the weights of [`BM25_WEIGHTS`].
#[must_use]
pub fn bm25_call() -> String {
    let weights: Vec<String> = BM25_WEIGHTS.iter().map(|w| format!("{w:.2}")).collect();
    format!("bm25(posts_fts, {})", weights.join(", "))
}

fn column_filter() -> String {
    format!("{{{}}}", COLUMNS.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_prefix_or_expression() {
        let q = TextQuery::parse("Cerco una lampada Arco");
        assert_eq!(q.terms, ["lampada", "arco"]);
        assert_eq!(
            q.match_expr.as_deref(),
            Some(
                "{tags keywords entities description note caption author web_text}: \
                 (\"lampada\"* OR \"arco\"*)"
            )
        );
        assert!(q.phrase_expr.is_some());
    }

    #[test]
    fn escapes_quotes_and_skips_untokenizable_terms() {
        assert_eq!(quote("say \"hi\""), "\"say \"\"hi\"\"\"");
        let q = TextQuery::parse("!!!");
        assert_eq!(q.terms, ["!!!"]);
        assert_eq!(q.match_expr, None);
        assert_eq!(q.phrase_expr, None);
    }

    #[test]
    fn bm25_call_lists_every_weight() {
        assert_eq!(
            bm25_call(),
            "bm25(posts_fts, 6.00, 5.00, 4.50, 4.00, 4.00, 3.50, 2.00, 2.00)"
        );
    }
}
