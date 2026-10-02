//! Rank-aware retrieval metrics: the port of `scripts/search-eval/order-metrics.ts`
//! plus the set metrics of `run.ts` (`searchPrecision`, `searchRecall`).
//!
//! `ranked` is the result list in the order the search returned it, `gold` the
//! relevant ids. Duplicates in `ranked` count once (the first wins), and a
//! metric is `None` where the desktop reports `null` (undefined, e.g. an empty
//! gold set).

use std::collections::HashSet;

/// The first occurrence of every id, in order.
pub fn dedupe(ranked: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    ranked
        .iter()
        .map(String::as_str)
        .filter(|id| seen.insert(*id))
        .collect()
}

/// Share of the first `k` results that are relevant. The denominator is
/// `min(k, results)`, so a list shorter than `k` is not penalized. `None` for
/// an empty list.
pub fn precision_at_k(ranked: &[String], gold: &HashSet<String>, k: usize) -> Option<f64> {
    let top: Vec<&str> = dedupe(ranked).into_iter().take(k).collect();
    if top.is_empty() {
        return None;
    }
    let hits = top.iter().filter(|id| gold.contains(**id)).count();
    Some(hits as f64 / top.len() as f64)
}

/// Share of the gold set found in the first `k` results; the denominator is
/// `min(|gold|, k)`. `None` for an empty gold set.
pub fn recall_at_k(ranked: &[String], gold: &HashSet<String>, k: usize) -> Option<f64> {
    if gold.is_empty() {
        return None;
    }
    let hits = dedupe(ranked)
        .into_iter()
        .take(k)
        .filter(|id| gold.contains(*id))
        .count();
    Some(hits as f64 / gold.len().min(k) as f64)
}

/// Reciprocal rank (1-based) of the first relevant result; 0 when none is
/// relevant. `None` for an empty gold set.
pub fn mrr(ranked: &[String], gold: &HashSet<String>) -> Option<f64> {
    if gold.is_empty() {
        return None;
    }
    Some(
        dedupe(ranked)
            .iter()
            .position(|id| gold.contains(*id))
            .map_or(0.0, |i| 1.0 / (i + 1) as f64),
    )
}

/// nDCG@k with binary relevance and a `log2(rank + 1)` discount, normalized by
/// the ideal ranking (every available gold result first). `None` for an empty
/// gold set.
pub fn ndcg_at_k(ranked: &[String], gold: &HashSet<String>, k: usize) -> Option<f64> {
    if gold.is_empty() {
        return None;
    }
    let dcg: f64 = dedupe(ranked)
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, id)| gold.contains(**id))
        .map(|(i, _)| 1.0 / ((i + 2) as f64).log2())
        .sum();
    let idcg: f64 = (0..gold.len().min(k))
        .map(|i| 1.0 / ((i + 2) as f64).log2())
        .sum();
    (idcg > 0.0).then(|| dcg / idcg)
}

/// The metrics of one result list (`orderMetrics` plus the set metrics).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    pub ndcg10: Option<f64>,
    pub mrr: Option<f64>,
    pub p5: Option<f64>,
    pub p10: Option<f64>,
    pub r5: Option<f64>,
    pub r10: Option<f64>,
    /// Share of the returned page that is relevant (`searchPrecision`).
    pub precision: Option<f64>,
    /// Share of the gold set on the returned page, out of `min(|gold|, page)`
    /// (`searchRecall`).
    pub recall: Option<f64>,
}

/// Names of the [`Metrics`] fields, in report order, as `last-report.json`
/// spells them.
pub const NAMES: [&str; 8] = [
    "ndcg@10",
    "mrr",
    "p@5",
    "p@10",
    "r@5",
    "r@10",
    "searchPrecision",
    "searchRecall",
];

impl Metrics {
    /// Scores `ranked`, the first page of a search of `page_size` results.
    pub fn of(ranked: &[String], gold: &HashSet<String>, page_size: usize) -> Self {
        let hits = ranked.iter().filter(|id| gold.contains(*id)).count();
        Self {
            ndcg10: ndcg_at_k(ranked, gold, 10),
            mrr: mrr(ranked, gold),
            p5: precision_at_k(ranked, gold, 5),
            p10: precision_at_k(ranked, gold, 10),
            r5: recall_at_k(ranked, gold, 5),
            r10: recall_at_k(ranked, gold, 10),
            precision: (!ranked.is_empty()).then(|| hits as f64 / ranked.len() as f64),
            recall: (!gold.is_empty()).then(|| hits as f64 / gold.len().min(page_size) as f64),
        }
    }

    /// The values in [`NAMES`] order.
    pub fn values(&self) -> [Option<f64>; 8] {
        [
            self.ndcg10,
            self.mrr,
            self.p5,
            self.p10,
            self.r5,
            self.r10,
            self.precision,
            self.recall,
        ]
    }

    /// Builds metrics from [`NAMES`]-ordered values.
    pub fn from_values(v: [Option<f64>; 8]) -> Self {
        Self {
            ndcg10: v[0],
            mrr: v[1],
            p5: v[2],
            p10: v[3],
            r5: v[4],
            r10: v[5],
            precision: v[6],
            recall: v[7],
        }
    }
}

/// Per-metric mean over `rows`, skipping undefined values (`None` when every
/// value is undefined).
pub fn mean(rows: &[Metrics]) -> Metrics {
    let mut out = [None; 8];
    for (i, slot) in out.iter_mut().enumerate() {
        let defined: Vec<f64> = rows.iter().filter_map(|m| m.values()[i]).collect();
        if !defined.is_empty() {
            *slot = Some(defined.iter().sum::<f64>() / defined.len() as f64);
        }
    }
    Metrics::from_values(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-12)
    }

    #[test]
    fn duplicates_count_once() {
        let ranked = ids(&["a", "b", "a", "c"]);
        assert_eq!(dedupe(&ranked), ["a", "b", "c"]);
        let gold = set(&["a"]);
        assert!(close(precision_at_k(&ranked, &gold, 3), 1.0 / 3.0));
    }

    #[test]
    fn precision_and_recall_use_the_desktop_denominators() {
        let ranked = ids(&["x", "a", "y"]);
        let gold = set(&["a", "b", "c", "d", "e", "f"]);
        // A list shorter than k is not penalized: 1 hit out of 3 results.
        assert!(close(precision_at_k(&ranked, &gold, 5), 1.0 / 3.0));
        // Recall is out of min(|gold|, k).
        assert!(close(recall_at_k(&ranked, &gold, 5), 1.0 / 5.0));
        assert!(close(recall_at_k(&ranked, &set(&["a", "z"]), 10), 0.5));
        assert_eq!(precision_at_k(&[], &gold, 5), None);
        assert_eq!(recall_at_k(&ranked, &HashSet::new(), 5), None);
    }

    #[test]
    fn mrr_is_the_reciprocal_rank_of_the_first_hit() {
        let gold = set(&["c"]);
        assert!(close(mrr(&ids(&["a", "b", "c"]), &gold), 1.0 / 3.0));
        assert!(close(mrr(&ids(&["a"]), &gold), 0.0));
        assert_eq!(mrr(&ids(&["a"]), &HashSet::new()), None);
    }

    #[test]
    fn ndcg_matches_hand_computed_values() {
        let gold = set(&["a", "b"]);
        // Ideal order.
        assert!(close(ndcg_at_k(&ids(&["a", "b", "x"]), &gold, 10), 1.0));
        // Hits at ranks 2 and 3: (1/log2 3 + 1/log2 4) / (1 + 1/log2 3).
        let expected = (1.0 / 3f64.log2() + 0.5) / (1.0 + 1.0 / 3f64.log2());
        assert!(close(
            ndcg_at_k(&ids(&["x", "a", "b"]), &gold, 10),
            expected
        ));
        // Hits beyond k do not count.
        assert!(close(ndcg_at_k(&ids(&["x", "y", "a"]), &gold, 2), 0.0));
        assert_eq!(ndcg_at_k(&ids(&["a"]), &HashSet::new(), 10), None);
    }

    #[test]
    fn set_metrics_follow_run_ts() {
        let ranked = ids(&["a", "x", "b", "y"]);
        let gold = set(&["a", "b", "c"]);
        let m = Metrics::of(&ranked, &gold, 60);
        assert!(close(m.precision, 0.5));
        assert!(close(m.recall, 2.0 / 3.0));
        let none = Metrics::of(&[], &gold, 60);
        assert_eq!(none.precision, None);
        assert!(close(none.recall, 0.0));
        assert!(close(none.mrr, 0.0));
        assert_eq!(none.p5, None);
    }

    #[test]
    fn mean_skips_undefined_values() {
        let a = Metrics {
            mrr: Some(1.0),
            ndcg10: None,
            ..Metrics::default()
        };
        let b = Metrics {
            mrr: Some(0.5),
            ndcg10: Some(0.25),
            ..Metrics::default()
        };
        let m = mean(&[a, b]);
        assert!(close(m.mrr, 0.75));
        assert!(close(m.ndcg10, 0.25));
        assert_eq!(m.p5, None);
    }
}
