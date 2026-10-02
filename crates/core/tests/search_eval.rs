//! SPIKE-5 (plan §2.14, §9): the desktop's relevance evaluation
//! (`scripts/search-eval`) ported to the web core, and the gate the FTS5 search
//! must pass: mean nDCG@10 and mean MRR at least the desktop's minus 0.02.
//!
//! The cases need a real library, so the gate only runs when
//! `SHELFY_SEARCH_EVAL_DB` names a desktop library (`shelfy.sqlite`, opened
//! read-only). Without it the test is skipped and passes, as in CI.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `SHELFY_SEARCH_EVAL_DB` | desktop library to evaluate on |
//! | `SHELFY_SEARCH_EVAL_BASELINE` | desktop report measured on the same library (default `scripts/search-eval/last-report.json`) |
//! | `SHELFY_SEARCH_EVAL_REPORT` | optional path for a JSON report (aggregate numbers only) |
//!
//! The run mirrors `scripts/search-eval/run.ts`: the gold set of a case is
//! computed from the desktop library by raw SQL on a separate read-only
//! connection; the search under test is `repo::posts::list` with relevance
//! order (the builder behind `GET /search` and `GET /posts?q=`), first page of
//! 60, on a web library built from the same rows. The desktop's AI tag
//! retrieval metrics (`poolRelevance`, `poolNoise`, `keywordRelevance`), its
//! composite pass/fail score and its tag-only probe measure the AI views, not
//! the FTS search, and are not ported.
//!
//! Nothing from the library is printed or written: only counts and metrics.
//! How to run it: `docs/web-port/spikes/05-fts-relevance.md`.

#[path = "search_eval/baseline.rs"]
mod baseline;
#[path = "search_eval/cases.rs"]
mod cases;
#[path = "search_eval/corpus.rs"]
mod corpus;
#[path = "search_eval/metrics.rs"]
mod metrics;
#[path = "search_eval/oracle.rs"]
mod oracle;

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use baseline::{Baseline, BaselineCase};
use cases::{CASES, EvalCase, Kind};
use corpus::Corpus;
use metrics::{Metrics, NAMES};
use oracle::Oracle;
use serde_json::{Value, json};
use shelfy_core::repo::RepoError;
use shelfy_core::repo::posts::{self, Mode, PageRequest, PostFilter, Sort};

/// First page of the desktop harness (`RESULT_LIMIT`).
const PAGE: u32 = 60;
/// How far below the desktop the FTS search may fall (plan §2.14).
const TOLERANCE: f64 = 0.02;
/// `GET /search` p95 budget (plan §6.2).
const SEARCH_P95_BUDGET: Duration = Duration::from_millis(60);
/// Timed runs per case, after one warm-up run.
const TIMED_RUNS: usize = 20;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The ported cases are the desktop's: same ids, kinds, queries and terms.
#[test]
fn cases_match_the_desktop_set() {
    let path = repo_root().join("scripts/search-eval/cases.ts");
    let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}: {path:?}"));
    // Comment lines hold a commented-out example case.
    let code: Vec<&str> = source
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    let code = code.join("\n");
    let chunks: Vec<&str> = code.split("id: '").skip(1).collect();
    assert_eq!(chunks.len(), CASES.len(), "number of cases");
    for (chunk, case) in chunks.iter().zip(CASES) {
        assert!(chunk.starts_with(&format!("{}'", case.id)), "{}", case.id);
        let kind = format!("kind: '{}'", case.kind.as_str());
        assert!(chunk.contains(&kind), "{}: kind", case.id);
        let query = format!("query: '{}'", case.query);
        assert!(chunk.contains(&query), "{}: query", case.id);
        let lists = [
            ("goldTerms", Some(case.gold_terms)),
            ("rejectTags", Some(case.reject_tags)),
            ("tagProbeOverride", case.tag_probe_override),
        ];
        for (name, expected) in lists {
            assert_eq!(
                quoted_list(chunk, name),
                expected.map(<[&str]>::to_vec),
                "{}: {name}",
                case.id
            );
        }
        assert!(!chunk.contains("humanGold"), "{}: humanGold", case.id);
    }
}

/// Every case has a distinct id and gold terms.
#[test]
fn cases_are_well_formed() {
    let ids: HashSet<&str> = CASES.iter().map(|c| c.id).collect();
    assert_eq!(ids.len(), CASES.len());
    assert!(CASES.iter().all(|c| !c.gold_terms.is_empty()));
}

/// The single-quoted strings of `name: [ … ]` in `chunk`.
fn quoted_list<'a>(chunk: &'a str, name: &str) -> Option<Vec<&'a str>> {
    let start = chunk.find(&format!("{name}: ["))? + name.len() + 3;
    let end = start + chunk[start..].find(']')?;
    Some(chunk[start..end].split('\'').skip(1).step_by(2).collect())
}

/// The results of one case.
struct CaseRun {
    case: &'static EvalCase,
    gold: usize,
    total: u64,
    text: Metrics,
    /// The hybrid probe (top-2 gold tags + the query); `None` without gold tags.
    hybrid: Option<Metrics>,
    /// First page plus the count of matches, per timed run.
    times: Vec<Duration>,
}

#[test]
fn search_eval_gate() {
    let Some(library) = std::env::var_os("SHELFY_SEARCH_EVAL_DB").map(PathBuf::from) else {
        eprintln!(
            "search_eval: skipped, SHELFY_SEARCH_EVAL_DB is not set \
             (docs/web-port/spikes/05-fts-relevance.md)"
        );
        return;
    };
    let baseline_path = std::env::var_os("SHELFY_SEARCH_EVAL_BASELINE").map_or_else(
        || repo_root().join("scripts/search-eval/last-report.json"),
        PathBuf::from,
    );
    let baseline = Baseline::read(&baseline_path);
    let oracle = Oracle::open(&library).expect("open the desktop library for the oracle");
    let corpus = corpus::build(&library);

    let runs: Vec<CaseRun> = CASES
        .iter()
        .map(|c| run_case(c, &oracle, &corpus))
        .collect();
    println!("{}", render(&runs, &baseline, &corpus));
    if let Some(path) = std::env::var_os("SHELFY_SEARCH_EVAL_REPORT") {
        let report = serde_json::to_string_pretty(&json_report(&runs, &baseline, &corpus))
            .expect("the report serializes");
        std::fs::write(&path, report).expect("write the report");
    }

    // The desktop numbers only compare when they were measured on the same
    // rows: the gold sets come from the same SQL, so their sizes must match.
    let mismatched: Vec<&str> = runs
        .iter()
        .filter(|r| baseline_of(&baseline, r).is_none_or(|b| b.gold_posts != r.gold as u64))
        .map(|r| r.case.id)
        .collect();
    assert!(
        mismatched.is_empty(),
        "the baseline {} was measured on another library (gold sets differ or are missing for \
         {mismatched:?}); \
         run `pnpm run eval:search` on this library or point SHELFY_SEARCH_EVAL_DB at the copy \
         the report was measured on (scripts/search-eval/.scratch/shelfy.sqlite)",
        baseline_path.display()
    );

    let (fts, desktop) = group_means(&runs, &baseline, &Kind::ALL);
    for (name, ours, theirs) in [
        ("nDCG@10", fts.ndcg10, desktop.ndcg10),
        ("MRR", fts.mrr, desktop.mrr),
    ] {
        let (ours, theirs) = (ours.unwrap_or(0.0), theirs.unwrap_or(0.0));
        assert!(
            ours >= theirs - TOLERANCE,
            "mean {name} {ours:.3} is below the desktop's {theirs:.3} - {TOLERANCE}"
        );
    }

    let p95 = percentile(all_times(&runs), 0.95);
    if cfg!(debug_assertions) {
        println!("latency: debug build, the budget is checked with --release only");
    } else {
        assert!(
            p95 <= SEARCH_P95_BUDGET,
            "search p95 {p95:?} is over the {SEARCH_P95_BUDGET:?} budget"
        );
    }
}

fn baseline_of<'a>(baseline: &'a Baseline, run: &CaseRun) -> Option<&'a BaselineCase> {
    baseline.cases.get(run.case.id)
}

fn run_case(case: &'static EvalCase, oracle: &Oracle, corpus: &Corpus) -> CaseRun {
    let gold = oracle.gold_posts(case.gold_terms).expect("gold posts");
    let text_filter = PostFilter {
        q: Some(case.query.to_owned()),
        ..PostFilter::default()
    };
    let (ranked, total) = search(corpus, &text_filter);
    let text = Metrics::of(&ranked, &gold, PAGE as usize);

    // The desktop's P1 probe: the two strongest gold tags fused with the text.
    let probe: Vec<String> = oracle
        .gold_tags(&gold, 12)
        .expect("gold tags")
        .into_iter()
        .take(2)
        .collect();
    let hybrid = (!probe.is_empty()).then(|| {
        let filter = PostFilter {
            q: Some(case.query.to_owned()),
            tags: probe,
            tag_mode: Mode::Or,
            ..PostFilter::default()
        };
        let (ranked, _) = search(corpus, &filter);
        Metrics::of(&ranked, &gold, PAGE as usize)
    });

    let times = (0..=TIMED_RUNS)
        .map(|_| time_search(corpus, &text_filter))
        .skip(1)
        .collect();
    CaseRun {
        case,
        gold: gold.len(),
        total,
        text,
        hybrid,
        times,
    }
}

fn first_page() -> PageRequest {
    PageRequest {
        sort: Sort::Relevance,
        limit: PAGE,
        cursor: None,
    }
}

/// The first page as desktop ids, in rank order, and the number of matches.
fn search(corpus: &Corpus, filter: &PostFilter) -> (Vec<String>, u64) {
    corpus
        .db
        .read(|conn| {
            let page = posts::list(conn, filter, &first_page())?;
            let total = posts::count(conn, filter)?;
            let ids = page
                .items
                .iter()
                .map(|p| corpus.legacy_id[&p.key].clone())
                .collect();
            Ok::<_, RepoError>((ids, total))
        })
        .expect("search")
}

/// Time of the first page plus the count, on one read snapshot.
fn time_search(corpus: &Corpus, filter: &PostFilter) -> Duration {
    corpus
        .db
        .read(|conn| {
            let started = Instant::now();
            let page = posts::list(conn, filter, &first_page())?;
            let total = posts::count(conn, filter)?;
            let elapsed = started.elapsed();
            assert!(page.items.len() as u64 <= total);
            Ok::<_, RepoError>(elapsed)
        })
        .expect("timed search")
}

fn all_times(runs: &[CaseRun]) -> Vec<Duration> {
    runs.iter().flat_map(|r| r.times.iter().copied()).collect()
}

/// The `q`-quantile (nearest rank).
fn percentile(mut values: Vec<Duration>, q: f64) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }
    values.sort();
    let rank = ((values.len() as f64 * q).ceil() as usize).clamp(1, values.len());
    values[rank - 1]
}

/// Means of the FTS metrics and of the desktop's over the cases of `kinds`.
fn group_means(runs: &[CaseRun], baseline: &Baseline, kinds: &[Kind]) -> (Metrics, Metrics) {
    let in_group: Vec<&CaseRun> = runs
        .iter()
        .filter(|r| kinds.contains(&r.case.kind))
        .collect();
    let fts: Vec<Metrics> = in_group.iter().map(|r| r.text).collect();
    let desktop: Vec<Metrics> = in_group
        .iter()
        .filter_map(|r| baseline_of(baseline, r).map(|b| b.text))
        .collect();
    (metrics::mean(&fts), metrics::mean(&desktop))
}

/// The report groups: every kind, then all cases.
fn groups() -> Vec<(&'static str, Vec<Kind>)> {
    Kind::ALL
        .iter()
        .map(|k| (k.as_str(), vec![*k]))
        .chain([("all", Kind::ALL.to_vec())])
        .collect()
}

fn f3(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| format!("{v:.3}"))
}

fn pair(ours: Option<f64>, theirs: Option<f64>) -> String {
    let delta = match (ours, theirs) {
        (Some(a), Some(b)) => format!(" ({:+.3})", a - b),
        _ => String::new(),
    };
    format!("{}/{}{delta}", f3(ours), f3(theirs))
}

fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

/// The console report: corpus counts, per-case and per-group metrics (FTS /
/// desktop), the hybrid probe and the latency.
fn render(runs: &[CaseRun], baseline: &Baseline, corpus: &Corpus) -> String {
    let s = &corpus.stats;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "\nSPIKE-5 search eval: {} posts ({} with AI fields, {} accepted aliases, {} synthetic \
         keys, {} captions cut), built in {:.1} s; desktop report from {}",
        s.posts,
        s.with_ai,
        s.accepted_aliases,
        s.synthetic_keys,
        s.truncated_captions,
        s.build_seconds,
        baseline.ts
    );
    let _ = writeln!(
        out,
        "\n{:<11} {:<8} {:>5} {:>11} {:>22} {:>22} {:>12} {:>12}",
        "case",
        "kind",
        "gold",
        "total f/d",
        "nDCG@10 fts/dt (Δ)",
        "MRR fts/dt (Δ)",
        "p@10 f/d",
        "r@10 f/d"
    );
    for r in runs {
        let b = baseline_of(baseline, r);
        let dt = b.map(|b| b.text).unwrap_or_default();
        let dt_total = b
            .and_then(|b| b.total)
            .map_or_else(|| "-".to_owned(), |t| t.to_string());
        let _ = writeln!(
            out,
            "{:<11} {:<8} {:>5} {:>11} {:>22} {:>22} {:>12} {:>12}",
            r.case.id,
            r.case.kind.as_str(),
            r.gold,
            format!("{}/{dt_total}", r.total),
            pair(r.text.ndcg10, dt.ndcg10),
            pair(r.text.mrr, dt.mrr),
            format!("{}/{}", f3(r.text.p10), f3(dt.p10)),
            format!("{}/{}", f3(r.text.r10), f3(dt.r10)),
        );
    }
    let _ = writeln!(out, "\nmeans, fts/desktop:");
    let header: Vec<String> = NAMES.iter().map(|n| format!("{n:>16}")).collect();
    let _ = writeln!(out, "{:<8}{}", "group", header.join(""));
    for (name, kinds) in groups() {
        let (fts, dt) = group_means(runs, baseline, &kinds);
        let cells: Vec<String> = fts
            .values()
            .iter()
            .zip(dt.values())
            .map(|(a, b)| format!("{:>16}", format!("{}/{}", f3(*a), f3(b))))
            .collect();
        let _ = writeln!(out, "{name:<8}{}", cells.join(""));
    }
    let _ = writeln!(
        out,
        "\nhybrid probe (top-2 gold tags + query), fts/desktop:"
    );
    for r in runs {
        let theirs = baseline_of(baseline, r).and_then(|b| b.hybrid);
        if r.hybrid.is_none() && theirs.is_none() {
            continue;
        }
        let (ours, theirs) = (r.hybrid.unwrap_or_default(), theirs.unwrap_or_default());
        let _ = writeln!(
            out,
            "{:<11} nDCG@10 {}  MRR {}  p@10 {}",
            r.case.id,
            pair(ours.ndcg10, theirs.ndcg10),
            pair(ours.mrr, theirs.mrr),
            pair(ours.p10, theirs.p10)
        );
    }
    let times = all_times(runs);
    let _ = writeln!(
        out,
        "\nlatency of first page + count over {} runs ({} build): p50 {} ms, p95 {} ms, \
         max {} ms (budget p95 {} ms)",
        times.len(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        ms(percentile(times.clone(), 0.5)),
        ms(percentile(times.clone(), 0.95)),
        ms(percentile(times, 1.0)),
        SEARCH_P95_BUDGET.as_millis()
    );
    out
}

/// The run as JSON: corpus counts, per-case and per-group metrics, latency.
fn json_report(runs: &[CaseRun], baseline: &Baseline, corpus: &Corpus) -> Value {
    let metrics_json = |m: Metrics| -> Value {
        NAMES
            .iter()
            .zip(m.values())
            .map(|(n, v)| ((*n).to_owned(), v.into()))
            .collect::<serde_json::Map<_, _>>()
            .into()
    };
    let cases: Vec<Value> = runs
        .iter()
        .map(|r| {
            let b = baseline_of(baseline, r);
            json!({
                "id": r.case.id,
                "kind": r.case.kind.as_str(),
                "goldPosts": r.gold,
                "total": r.total,
                "fts": metrics_json(r.text),
                "ftsHybrid": r.hybrid.map(metrics_json),
                "desktopTotal": b.and_then(|b| b.total),
                "desktop": b.map(|b| metrics_json(b.text)),
                "desktopHybrid": b.and_then(|b| b.hybrid).map(metrics_json),
            })
        })
        .collect();
    let groups: Vec<Value> = groups()
        .into_iter()
        .map(|(name, kinds)| {
            let (fts, dt) = group_means(runs, baseline, &kinds);
            json!({ "group": name, "fts": metrics_json(fts), "desktop": metrics_json(dt) })
        })
        .collect();
    let times = all_times(runs);
    let s = &corpus.stats;
    json!({
        "baselineTs": baseline.ts,
        "posts": s.posts,
        "postsWithAi": s.with_ai,
        "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        "cases": cases,
        "groups": groups,
        "latencyMs": {
            "p50": percentile(times.clone(), 0.5).as_secs_f64() * 1000.0,
            "p95": percentile(times.clone(), 0.95).as_secs_f64() * 1000.0,
            "max": percentile(times, 1.0).as_secs_f64() * 1000.0,
        },
    })
}
