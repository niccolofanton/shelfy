//! The desktop numbers: a `last-report.json` written by `pnpm run eval:search`.
//! Only aggregate fields are read (metrics, gold-set sizes, result counts).

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use super::metrics::{Metrics, NAMES};

/// The desktop results of one case.
#[derive(Debug)]
pub struct BaselineCase {
    pub gold_posts: u64,
    pub total: Option<u64>,
    /// The text search (`searchPostsHybrid([], query)`).
    pub text: Metrics,
    /// The hybrid probe (top-2 gold tags + the query), when the desktop ran it.
    pub hybrid: Option<Metrics>,
    /// Tag-only probe (top-5 gold tags or the case override).
    pub tags: Option<Metrics>,
}

/// A desktop report.
#[derive(Debug)]
pub struct Baseline {
    /// When the desktop harness ran.
    pub ts: String,
    pub cases: HashMap<String, BaselineCase>,
    /// The fingerprint of the library the report was measured on, when the
    /// report records it (the committed report of the synthetic library).
    pub library_digest: Option<String>,
}

impl Baseline {
    /// Reads a report; panics with the path on any error.
    pub fn read(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "cannot read the desktop baseline {}: {e}. Run `pnpm run eval:search` on the \
                 same library, or point SHELFY_SEARCH_EVAL_BASELINE at its report.",
                path.display()
            )
        });
        let json: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{} is not a search-eval report: {e}", path.display()));
        let results = json["results"]
            .as_array()
            .unwrap_or_else(|| panic!("{} has no results array", path.display()));
        let cases = results
            .iter()
            .map(|r| {
                let id = r["id"].as_str().expect("case id").to_owned();
                let m = &r["metrics"];
                let text = Metrics::from_values(NAMES.map(|name| m[name].as_f64()));
                let hybrid = m.get("hy_ndcg@10").map(|_| {
                    Metrics::from_values(NAMES.map(|name| m[format!("hy_{name}")].as_f64()))
                });
                let case = BaselineCase {
                    gold_posts: r["goldPostCount"].as_u64().expect("goldPostCount"),
                    total: r["sample"]["searchTotal"].as_u64(),
                    text,
                    hybrid,
                    tags: m.get("tag_ndcg@10").map(|_| {
                        Metrics::from_values(NAMES.map(|name| m[format!("tag_{name}")].as_f64()))
                    }),
                };
                (id, case)
            })
            .collect();
        Self {
            ts: json["ts"].as_str().unwrap_or("?").to_owned(),
            cases,
            library_digest: json["libraryDigest"].as_str().map(str::to_owned),
        }
    }
}
