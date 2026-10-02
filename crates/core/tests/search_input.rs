//! User input never breaks FTS5 (P1-05): search text, concepts and tags are
//! quoted and filtered before they reach a `MATCH`, so no string makes the
//! list, its count, the ranking or a selection fail.
//!
//! The strategies mix FTS5's syntax (quotes, `*`, `^`, `:`, `+`, `-`,
//! parentheses, braces, `AND`/`OR`/`NOT`/`NEAR`, column filters), JSON and
//! LIKE punctuation, letters and digits of every script, combining marks,
//! and characters newer than SQLite's tokenizer tables.

mod support;

use proptest::prelude::*;
use rusqlite::Connection;
use shelfy_core::repo::posts::{self, Mode, PageRequest, PostFilter, Sort};
use shelfy_core::search::query::TextQuery;
use shelfy_core::selector::{self, Selector};
use support::{insert_all, library, synthetic_posts};

/// Pieces that are special somewhere between the API and FTS5.
const SYNTAX: &[&str] = &[
    "\"",
    "\"\"",
    "*",
    "^",
    ":",
    "+",
    "-",
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    "AND",
    "OR",
    "NOT",
    "NEAR",
    "NEAR(",
    ",",
    "caption:",
    "{tags note}:",
    "%",
    "_",
    "\\",
    "'",
    ";",
    "--",
    "\0",
    " ",
    "\t",
    "\n",
    "\u{feff}",
    "\u{200b}",
    "\u{0301}",
    "ß",
    "İ",
    "ǅ",
    "𝒜",
    "😀",
    "\u{1e94b}",
    "\u{1fbf5}",
    "\u{a7cb}",
    "\u{11f00}",
    "lampada",
    "3d",
    "r",
    "a",
    "fluidi",
];

fn piece() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(SYNTAX).prop_map(str::to_owned),
        "\\PC{0,6}",
        any::<char>().prop_map(String::from),
    ]
}

/// A user string: up to a dozen pieces, sometimes separated by spaces.
fn text() -> impl Strategy<Value = String> {
    prop::collection::vec((piece(), any::<bool>()), 0..12).prop_map(|parts| {
        parts
            .into_iter()
            .map(|(p, space)| if space { format!("{p} ") } else { p })
            .collect()
    })
}

fn mode() -> impl Strategy<Value = Mode> {
    prop_oneof![Just(Mode::Or), Just(Mode::And)]
}

/// A small library to search, built once per test process.
fn searched_library() -> Connection {
    let conn = library();
    insert_all(&conn, &synthetic_posts(120, 0x5EA7C4));
    conn
}

/// Runs every query that takes `filter`'s text, failing on any error.
fn run_everything(conn: &Connection, filter: &PostFilter) -> Result<(), TestCaseError> {
    let fail = |what: &str, e: &dyn std::fmt::Debug| {
        TestCaseError::fail(format!("{what} failed for {filter:?}: {e:?}"))
    };
    for sort in [Sort::Relevance, Sort::Newest, Sort::Oldest] {
        let page = PageRequest {
            sort,
            limit: 20,
            cursor: None,
        };
        posts::list(conn, filter, &page).map_err(|e| fail("list", &e))?;
    }
    let total = posts::count(conn, filter).map_err(|e| fail("count", &e))?;
    let ranked = posts::rank(conn, filter).map_err(|e| fail("rank", &e))?;
    prop_assert!(ranked.len() as u64 <= total, "the ranking is a subset");
    let page = posts::ranked_page(conn, filter, &ranked, 0, 50).map_err(|e| fail("page", &e))?;
    prop_assert!(page.items.len() <= ranked.len());
    let ids = posts::list_ids(conn, filter).map_err(|e| fail("list_ids", &e))?;
    prop_assert_eq!(ids.len() as u64, total);
    let selected = selector::count(conn, &Selector::filter(filter.clone()))
        .map_err(|e| fail("selector", &e))?;
    prop_assert_eq!(selected, total);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 384,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// The FTS5 expressions built from any text are valid queries.
    #[test]
    fn query_expressions_always_parse(q in text()) {
        thread_local! {
            static CONN: Connection = searched_library();
        }
        let parsed = TextQuery::parse(&q);
        CONN.with(|conn| -> Result<(), TestCaseError> {
            for (table, expr) in [
                ("posts_fts", &parsed.match_expr),
                ("posts_infix", &parsed.infix_expr),
                ("posts_fts", &parsed.phrase_expr),
            ] {
                let Some(expr) = expr else { continue };
                let sql = format!("SELECT count(*) FROM {table} WHERE {table} MATCH ?1");
                let result: rusqlite::Result<i64> = conn.query_row(&sql, [expr], |r| r.get(0));
                prop_assert!(result.is_ok(), "{table} MATCH {expr:?} for {q:?}: {result:?}");
            }
            Ok(())
        })?;
    }

    /// No search input makes a list, count, ranking or selection fail.
    #[test]
    fn search_input_never_fails(
        q in prop::option::of(text()),
        concepts in prop::collection::vec(text(), 0..4),
        tags in prop::collection::vec(text(), 0..3),
        tag_mode in mode(),
        concept_mode in mode(),
        tag in prop::option::of(text()),
        entity in prop::option::of(text()),
    ) {
        thread_local! {
            static CONN: Connection = searched_library();
        }
        let filter = PostFilter {
            q,
            concepts,
            concept_mode,
            tags,
            tag_mode,
            tag,
            entity,
            ..PostFilter::default()
        };
        CONN.with(|conn| run_everything(conn, &filter))?;
    }
}

/// Inputs that once looked risky, run every time.
#[test]
fn known_hard_inputs_run() {
    let conn = searched_library();
    let inputs = [
        "\"",
        "\"\"\"",
        "*",
        "lampada*",
        "\"lampada\"*",
        "NEAR(a b)",
        "a AND",
        "OR OR",
        "NOT",
        "caption:lampada",
        "{tags}: x",
        "-",
        "^lampada",
        "(",
        ")",
        "\0",
        "a\0b",
        "ab",
        "  ",
        "\u{feff}",
        "\u{0301}",
        "e\u{0301}",
        "😀😀😀",
        "%_%",
        "fluidi",
        "simulazionefluidi",
    ];
    for q in inputs {
        let filter = PostFilter {
            q: Some(q.to_owned()),
            concepts: vec![q.to_owned()],
            tags: vec![q.to_owned()],
            ..PostFilter::default()
        };
        run_everything(&conn, &filter).unwrap_or_else(|e| panic!("{q:?}: {e}"));
    }
}
