//! FTS5 query building and the relevance model of post search (plan §2.14).
//!
//! A free-text query becomes its content terms ([`super::terms`]). A post
//! matches when any term prefix-matches one of its indexed columns: the FTS5
//! expression `{columns}: ("t1"* OR "t2"* …)`, where every term is a quoted
//! string (so FTS5 operators in user input stay literal). A one-letter term
//! matches whole tokens only ([`MIN_PREFIX_CHARS`]).
//!
//! # Relevance (tuned by SPIKE-5)
//!
//! The score ports the desktop's ranking (`buildPostFilter` in
//! `electron/db.ts`) onto FTS5 signals. Lower is better, as for `bm25()`:
//!
//! ```text
//! score = Σ_term weight(term) × ( tf(prefix match) + EXACT_TOKEN_WEIGHT × tf(exact match) )
//!         − Σ_tag EXACT_TAG_BOOST × weight(tag)   (boost terms that are one of the post's tags)
//!         − PHRASE_BONUS                          (the whole query appears as a phrase)
//! ```
//!
//! - `tf(x)` is the term-frequency part of FTS5's bm25 for the single-phrase
//!   query `x`: `bm25(x) / idf(x)`, with the column weights of [`BM25_WEIGHTS`]
//!   (negative, like bm25). Dividing out FTS5's own IDF ([`fts5_idf`]) leaves the
//!   column-weighted, length-normalized frequency of the term in the post.
//! - `weight(term)` is the desktop's term weight ([`term_weight`]): `ln(N / df)`
//!   clamped to `[1, 3]`, with `N` the indexed posts and `df` those the term
//!   prefix-matches. bm25's unbounded IDF let one rare query word (an Italian
//!   filler such as "realizzati") outrank the posts that match the topic; the
//!   clamp keeps the number of distinct terms matched dominant.
//! - The exact arm counts the term again where it is a whole token: the
//!   desktop ranks whole-word hits above substring hits (for "accessori", a
//!   post that says "accessori" above one that only says "accessories").
//! - `weight(tag)` is the same clamp over the posts that carry the tag (the
//!   desktop's `6 × idf(tag)`).
//!
//! The constants live here so they can be tuned in one place; the SQL that
//! applies them is in `repo::posts`. The SPIKE-5 note
//! (`docs/web-port/spikes/05-fts-relevance.md`) records how they were chosen.

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
/// (tag/keyword 5–6, description/note 4, caption 3.5). SPIKE-5 kept them: its
/// libraries have almost no AI fields, so they cannot rank the columns.
pub const BM25_WEIGHTS: [f64; 8] = [6.0, 5.0, 4.5, 4.0, 4.0, 3.5, 2.0, 2.0];

/// Smallest weight of a query term (the desktop's IDF clamp).
pub const TERM_WEIGHT_MIN: f64 = 1.0;

/// Largest weight of a query term (the desktop's IDF clamp).
pub const TERM_WEIGHT_MAX: f64 = 3.0;

/// Weight of a term's whole-token matches on top of its prefix matches.
pub const EXACT_TOKEN_WEIGHT: f64 = 1.0;

/// Score bonus per boost term (query term, concept or hybrid tag) that is one
/// of the post's tags, times the tag's [`term_weight`]. Scores are
/// lower-is-better, so bonuses are subtracted.
pub const EXACT_TAG_BOOST: f64 = 3.0;

/// Score bonus when the whole query appears as a phrase.
pub const PHRASE_BONUS: f64 = 1.5;

/// Relevance results are paged by offset inside the first this-many results.
pub const RELEVANCE_WINDOW: u32 = 1000;

/// Shortest last token, in characters, that is matched as a prefix. A
/// one-letter term (the whitelisted "r", or a query of stopwords such as "a",
/// kept raw) matches whole tokens only: as a prefix it would match every word
/// with that initial, and `posts_fts` has no prefix index below 2 characters
/// (`prefix='2 3'`), so the scan read most of the index (~45 ms at 18k posts).
pub const MIN_PREFIX_CHARS: usize = 2;

static TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").expect("token pattern is valid"));

/// A free-text query prepared for FTS5.
#[derive(Clone, Debug, PartialEq)]
pub struct TextQuery {
    /// Lowercased content terms (the scored terms and the exact-tag boost
    /// candidates).
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

/// One matched unit: a prefix match (`"text"*`), or a whole-token match
/// (`"text"`) when the last token of `text` is shorter than
/// [`MIN_PREFIX_CHARS`]. `None` when `text` has no character the tokenizer
/// indexes (FTS5 rejects an empty prefix phrase).
#[must_use]
pub fn prefix_unit(text: &str) -> Option<String> {
    let text = text.trim();
    let last = TOKEN.find_iter(text).last()?;
    Some(if last.as_str().chars().count() < MIN_PREFIX_CHARS {
        quote(text)
    } else {
        format!("{}*", quote(text))
    })
}

/// `{columns}: (u1 OR u2 …)`, or `None` without units.
#[must_use]
pub fn any_of(units: &[String]) -> Option<String> {
    if units.is_empty() {
        return None;
    }
    Some(format!("{}: ({})", column_filter(), units.join(" OR ")))
}

/// The scored matches of one term: its [`prefix_unit`] match and, when that
/// is a prefix, the whole-token match `{columns}: "term"`. `None` without an
/// indexable character.
#[must_use]
pub fn term_matches(term: &str) -> Option<(String, Option<String>)> {
    let unit = prefix_unit(term)?;
    let exact = quote(term.trim());
    let columns = column_filter();
    let exact = (unit != exact).then(|| format!("{columns}: {exact}"));
    Some((format!("{columns}: {unit}"), exact))
}

/// The weight of a term that `df` of the `rows` indexed posts match:
/// `ln(rows / df)` clamped to [`TERM_WEIGHT_MIN`]..=[`TERM_WEIGHT_MAX`]
/// (the desktop's `termIdfWeights`).
#[must_use]
pub fn term_weight(rows: u64, df: u64) -> f64 {
    let ratio = rows as f64 / df.max(1) as f64;
    ratio.ln().clamp(TERM_WEIGHT_MIN, TERM_WEIGHT_MAX)
}

/// The IDF FTS5's bm25 gives a phrase that `df` of `rows` rows match:
/// `ln((rows - df + 0.5) / (df + 0.5))`, at least `1e-6` (`fts5_aux.c`).
#[must_use]
pub fn fts5_idf(rows: u64, df: u64) -> f64 {
    let (rows, df) = (rows as f64, df as f64);
    let idf = ((rows - df + 0.5) / (df + 0.5)).ln();
    if idf <= 0.0 { 1e-6 } else { idf }
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
        assert_eq!(term_matches("!!!"), None);
    }

    #[test]
    fn term_matches_are_prefix_and_exact() {
        let cols = "{tags keywords entities description note caption author web_text}";
        assert_eq!(
            term_matches(" say \"hi\" "),
            Some((
                format!("{cols}: \"say \"\"hi\"\"\"*"),
                Some(format!("{cols}: \"say \"\"hi\"\"\""))
            ))
        );
        // Two letters are a prefix (the shortest prefix index).
        assert_eq!(
            term_matches("3d"),
            Some((format!("{cols}: \"3d\"*"), Some(format!("{cols}: \"3d\""))))
        );
    }

    #[test]
    fn one_letter_terms_match_whole_tokens() {
        let cols = "{tags keywords entities description note caption author web_text}";
        assert_eq!(term_matches("r"), Some((format!("{cols}: \"r\""), None)));
        assert_eq!(prefix_unit(" a "), Some("\"a\"".to_owned()));
        // Only the last token of a phrase is a prefix.
        assert_eq!(prefix_unit("il e"), Some("\"il e\"".to_owned()));
        assert_eq!(prefix_unit("e il"), Some("\"e il\"*".to_owned()));
        let q = TextQuery::parse("a");
        assert_eq!(q.match_expr, Some(format!("{cols}: (\"a\")")));
    }

    #[test]
    fn term_weight_is_the_clamped_desktop_idf() {
        // ln(6000 / 3000) = 0.69 → the floor.
        assert!((term_weight(6000, 3000) - TERM_WEIGHT_MIN).abs() < 1e-12);
        // ln(6000 / 600) = 2.30, inside the range.
        assert!((term_weight(6000, 600) - 10f64.ln()).abs() < 1e-12);
        // ln(6000 / 1) = 8.7 → the cap.
        assert!((term_weight(6000, 1) - TERM_WEIGHT_MAX).abs() < 1e-12);
        // Degenerate inputs stay in range.
        assert!((term_weight(0, 0) - TERM_WEIGHT_MIN).abs() < 1e-12);
    }

    #[test]
    fn fts5_idf_matches_sqlite() {
        assert!((fts5_idf(100, 10) - (90.5f64 / 10.5).ln()).abs() < 1e-12);
        // A phrase in more than half of the rows gets FTS5's floor.
        assert!((fts5_idf(100, 60) - 1e-6).abs() < 1e-18);
    }

    #[test]
    fn bm25_call_lists_every_weight() {
        assert_eq!(
            bm25_call(),
            "bm25(posts_fts, 6.00, 5.00, 4.50, 4.00, 4.00, 3.50, 2.00, 2.00)"
        );
    }
}
