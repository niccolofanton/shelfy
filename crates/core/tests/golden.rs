//! Golden fixtures (plan §6.1): the desktop's TypeScript functions run on fixed
//! inputs by `scripts/golden/`, their outputs stored in `shared/golden/*.jsonl`,
//! and the Rust ports must produce the same JSON, byte for byte.
//!
//! Every fixture file needs a check here; `every_golden_file_has_a_check`
//! fails for a file without one. How to regenerate the files and add a
//! function: `scripts/golden/README.md`.

mod golden_merge;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use shelfy_core::search::terms::{SHORT_CONTENT_TERMS, STOPWORDS, extract_content_terms};

/// Golden sets with a check in this file; `<dir>/` stands for every file in
/// that directory.
const CHECKED: &[&str] = &["extract-content-terms", "merge/"];

#[derive(Deserialize)]
struct Header {
    golden: String,
    source: String,
    format: u32,
}

#[derive(Deserialize)]
struct Case<'a> {
    id: String,
    #[serde(borrow)]
    args: &'a RawValue,
    #[serde(borrow)]
    output: &'a RawValue,
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shared/golden")
}

fn read(name: &str) -> String {
    let path = golden_dir().join(format!("{name}.jsonl"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Parses a golden file: the header, then one case per line.
fn parse<'a>(name: &str, text: &'a str) -> Vec<Case<'a>> {
    let mut lines = text.lines();
    let header: Header = serde_json::from_str(lines.next().expect("header line")).unwrap();
    assert_eq!(header.golden, name);
    assert_eq!(header.format, 1, "{name}: unsupported format");
    assert!(!header.source.is_empty());
    lines
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{name}: {e}: {line}")))
        .collect()
}

/// Runs `port` on every case and compares its JSON with the recorded output.
fn check<A: DeserializeOwned, R: serde::Serialize>(name: &str, port: impl Fn(A) -> R) {
    let text = read(name);
    let cases = parse(name, &text);
    assert!(!cases.is_empty(), "{name}: no cases");
    let mut failures = Vec::new();
    for case in &cases {
        let args: A = serde_json::from_str(case.args.get())
            .unwrap_or_else(|e| panic!("{name}/{}: bad args: {e}", case.id));
        let actual = serde_json::to_string(&port(args)).unwrap();
        if actual != case.output.get() {
            failures.push(format!(
                "  {}\n    desktop: {}\n    rust:    {actual}",
                case.id,
                case.output.get()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{name}: {} of {} cases differ from the desktop:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

/// The golden sets under `dir`, as `<subdir>/<name>` without `.jsonl`.
fn golden_files(dir: &Path, prefix: &str, found: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            golden_files(&path, &format!("{prefix}{name}/"), found);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            found.push(format!("{prefix}{name}"));
        }
    }
}

#[test]
fn every_golden_file_has_a_check() {
    let mut found = Vec::new();
    golden_files(&golden_dir(), "", &mut found);
    found.sort();
    let covers = |check: &str, name: &str| {
        check == name || (check.ends_with('/') && name.starts_with(check))
    };
    for name in &found {
        assert!(
            CHECKED.iter().any(|check| covers(check, name)),
            "{name}.jsonl has no check in golden.rs"
        );
    }
    for check in CHECKED {
        assert!(
            found.iter().any(|name| covers(check, name)),
            "{check}: no golden file"
        );
    }
}

#[test]
fn merge_matches_the_desktop() {
    let (steps, failures) = golden_merge::check_dir(&golden_dir().join("merge"));
    assert!(
        failures.is_empty(),
        "merge: {} of {steps} steps differ from the desktop:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[derive(Deserialize)]
struct TermOptions {
    #[serde(rename = "minLen")]
    min_len: Option<usize>,
}

#[test]
fn extract_content_terms_matches_the_desktop() {
    check(
        "extract-content-terms",
        |(query, opts): (String, TermOptions)| {
            extract_content_terms(&query, opts.min_len.unwrap_or(3))
        },
    );
}

#[test]
fn word_lists_match_the_desktop() {
    // The golden cases list every desktop stopword and short term; the outputs
    // prove Rust drops or keeps each of them, and this proves Rust has no extra.
    let text = read("extract-content-terms");
    let words = |id: &str| -> BTreeSet<String> {
        let case = parse("extract-content-terms", &text)
            .into_iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("case {id} missing"));
        let (query, _): (String, serde_json::Value) =
            serde_json::from_str(case.args.get()).unwrap();
        query.split(' ').map(str::to_owned).collect()
    };
    let rust =
        |list: &[&str]| -> BTreeSet<String> { list.iter().map(|w| (*w).to_owned()).collect() };
    assert_eq!(rust(STOPWORDS), words("stopwords-all"));
    assert_eq!(rust(SHORT_CONTENT_TERMS), words("short-terms-all"));
}
