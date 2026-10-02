//! Text rendering of a [`PlanReport`] and of the column mapping.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use shelfy_core::legacy::catalog::{self, Disposition, Presence};
use shelfy_core::legacy::{OpenMode, TableStatus};

use crate::report::{DuplicateSummary, FileClassCounts, PlanReport};

/// The human-readable report.
pub fn plan_text(r: &PlanReport) -> String {
    let mut o = String::new();
    let w = &mut o;
    line(
        w,
        &format!("{} · plan (dry run: nothing is written)", r.tool),
    );

    section(w, "Source");
    let s = &r.source;
    kv(w, "file", &format!("{} ({})", s.file_name, bytes(s.bytes)));
    let mode = match s.open_mode {
        OpenMode::Immutable => "immutable (no WAL: the desktop app is closed)",
        OpenMode::SharedReadOnly => "shared read-only (a WAL file is present)",
    };
    kv(
        w,
        "schema",
        &format!(
            "user_version {}, journal {}, opened {mode}",
            s.user_version, s.journal_mode
        ),
    );
    kv(
        w,
        "repairs pending",
        &if s.repairs_pending.is_empty() {
            "none".to_owned()
        } else {
            s.repairs_pending.join("; ")
        },
    );

    section(w, "Tables (rows → outcome)");
    for t in &r.tables {
        let outcomes = if t.outcomes.is_empty() {
            match t.status {
                TableStatus::AbsentOptional => "absent in this file".to_owned(),
                _ => "—".to_owned(),
            }
        } else {
            join_counts(&t.outcomes)
        };
        let target = match (&t.target, &t.dropped_reason) {
            (Some(target), _) => format!("→ {target}"),
            (None, Some(_)) => "dropped".to_owned(),
            (None, None) => "UNMAPPED".to_owned(),
        };
        let flag = if t.accounted { "" } else { "  [NOT ACCOUNTED]" };
        line(
            w,
            &format!("  {:<24}{:>8}  {outcomes}  {target}{flag}", t.table, t.rows),
        );
    }

    section(w, "Columns");
    let c = &r.coverage;
    kv(
        w,
        "coverage",
        &format!(
            "{} columns: {} present ({} mapped, {} dropped), {} absent (older file), {} unmapped",
            c.columns,
            c.present,
            c.mapped,
            c.dropped,
            c.absent_optional.len(),
            c.unmapped.len()
        ),
    );
    if !c.unmapped.is_empty() {
        kv(w, "unmapped", &c.unmapped.join(", "));
    }
    if !c.missing_required.is_empty() {
        kv(w, "missing", &c.missing_required.join(", "));
    }
    if !c.type_anomalies.is_empty() {
        let list: Vec<_> = c
            .type_anomalies
            .iter()
            .map(|a| format!("{} ({})", a.column, join_counts(&a.found)))
            .collect();
        kv(w, "type anomalies", &list.join("; "));
    }

    section(w, "Posts");
    let p = &r.posts;
    kv(w, "platforms", &join_counts(&p.by_platform));
    kv(w, "media types", &join_counts(&p.by_media_type));
    kv(w, "slides", &join_counts(&p.slides_by_kind));
    let ts = &p.posted_at;
    kv(
        w,
        "posted_at",
        &format!(
            "valid {} · empty {} · null {} · invalid {} (undated IG datable from shortcode: {})",
            ts.valid, ts.empty, ts.null, ts.invalid, ts.undated_ig_datable_from_shortcode
        ),
    );
    kv(w, "imported_at", &join_counts(&p.imported_at));
    kv(
        w,
        "AI",
        &format!(
            "status {} · with AI fields {} · ai_web_json {} · stuck 'analyzing' {}",
            join_counts(&p.ai.status),
            p.ai.with_ai_fields,
            p.ai.with_ai_web_json,
            p.ai.stuck_analyzing
        ),
    );
    kv(
        w,
        "user layer",
        &format!("notes {} · manual tags {}", p.user_notes, p.user_tags),
    );
    for (column, classes) in &p.json_arrays {
        kv(w, column, &join_counts(classes));
    }
    kv(
        w,
        "thumb_blur",
        &format!("{} (dropped)", join_counts(&p.thumb_blur)),
    );
    kv(
        w,
        "without slides",
        &format!(
            "{} (backfilled by desktop repair v1: {})",
            join_counts(&p.without_slides),
            p.without_slides_backfillable
        ),
    );
    kv(
        w,
        "repairs",
        &format!(
            "X //status URLs {} · media_count mismatches {}",
            p.x_status_urls_to_repair, p.media_count_mismatches
        ),
    );

    section(w, "Identity (§2.8)");
    for (platform, id) in &r.identity.by_platform {
        kv(
            w,
            platform,
            &format!(
                "{} rows → {} keys ({})",
                id.rows,
                id.distinct_keys,
                join_counts(&id.sources)
            ),
        );
    }
    let sc = &r.identity.ig_shortcode_check;
    if sc.checked > 0 {
        kv(
            w,
            "IG shortcodes",
            &format!(
                "{} checked: {} decode to the id's pk, {} differ, {} undecodable, {} without shortcode",
                sc.checked,
                sc.consistent,
                sc.inconsistent,
                sc.undecodable_shortcode,
                sc.no_shortcode
            ),
        );
    }
    if !r.identity.web_legacy_id_check.is_empty() {
        kv(
            w,
            "web legacy ids",
            &format!(
                "reproduced from {}",
                join_counts(&r.identity.web_legacy_id_check)
            ),
        );
    }
    kv(
        w,
        "unmappable",
        &if r.identity.unmappable.is_empty() {
            "0".to_owned()
        } else {
            join_counts(&r.identity.unmappable)
        },
    );

    section(w, "Duplicate groups");
    let d = &r.duplicates;
    duplicates(w, "instagram (same pk)", &d.instagram);
    duplicates(w, "web (http/https twins)", &d.web);
    duplicates(w, "other keys", &d.other);
    duplicates(w, "collections", &d.collections);

    section(w, "Tags");
    let t = &r.tags;
    kv(w, "post_tags", &join_counts(&t.post_tags));
    kv(
        w,
        "other",
        &format!(
            "unknown tiers {} · manual/AI collisions {} · aliases {} · clusters {} ({} memberships)",
            t.unknown_tiers,
            t.manual_ai_collisions,
            join_counts(&t.alias_status),
            t.clusters,
            t.cluster_memberships
        ),
    );

    section(w, "Sites and versions");
    let web = &r.web;
    kv(
        w,
        "sites",
        &format!(
            "{} ({} captured, {} placeholders) · {} older versions → {} web_captures rows · {} pages",
            web.sites, web.captured, web.placeholders, web.snapshots, web.web_captures, web.pages
        ),
    );
    kv(
        w,
        "assets",
        &format!(
            "{} (of which {} from older versions)",
            join_counts(&web.assets_by_role),
            web.snapshot_asset_refs
        ),
    );
    let f = &web.facets;
    kv(
        w,
        "post_facets",
        &format!(
            "{} rows on {} posts: {} rebuilt from ai_web_json, {} not derivable, {} extra on rebuild",
            f.rows, f.posts, f.derivable_rows, f.not_derivable_rows, f.extra_rows
        ),
    );

    section(w, "Local files");
    let files = &r.files;
    if files.checked {
        kv(
            w,
            "media root",
            &format!(
                "{} (desktop root {}detected)",
                files.media_root.as_deref().unwrap_or("?"),
                if files.legacy_root_detected {
                    ""
                } else {
                    "NOT "
                }
            ),
        );
    } else {
        kv(
            w,
            "media root",
            "not given: references counted, files not checked",
        );
    }
    line(
        w,
        &format!(
            "  {:<22}{:>8}{:>8}{:>9}{:>9}{:>9}{:>12}",
            "class", "refs", "files", "present", "missing", "outside", "bytes"
        ),
    );
    for (class, n) in &files.classes {
        file_row(w, class, n);
    }
    file_row(w, "total (distinct)", &files.totals);
    if files.checked {
        let u = &files.upload;
        kv(
            w,
            "upload",
            &format!(
                "{} files ({}) by default; {} video files ({}) only with --with-videos",
                u.files_default,
                bytes(u.bytes_default),
                u.files_videos,
                bytes(u.bytes_videos)
            ),
        );
        let cv = &files.covers;
        kv(
            w,
            "covers",
            &format!(
                "{} posts with a local cover; without: {} (IG cover URLs: {})",
                cv.with_local_cover,
                join_counts(&cv.without_local_cover),
                join_counts(&cv.ig_without_cover_url)
            ),
        );
        let or = &files.orphans;
        let dirs: BTreeMap<String, u64> = or.by_dir.iter().map(|(k, v)| (k.clone(), v.0)).collect();
        kv(
            w,
            "orphans",
            &format!(
                "{} files ({}) under assets/ referenced by no row: {} · {} ignored",
                or.files,
                bytes(or.bytes),
                join_counts(&dirs),
                or.ignored
            ),
        );
    }

    if !r.warnings.is_empty() {
        section(w, &format!("Warnings ({})", r.warnings.len()));
        for warning in &r.warnings {
            line(w, &format!("  - {warning}"));
        }
    }
    if !r.errors.is_empty() {
        section(w, &format!("Errors ({})", r.errors.len()));
        for error in &r.errors {
            line(w, &format!("  - {error}"));
        }
    }

    section(w, "Verdict (SPIKE-1)");
    let v = &r.verdict;
    line(
        w,
        &format!(
            "  {} · every row accounted for: {} · duplicate groups listed: {} · no unmapped column: {} · no errors: {}",
            if v.pass { "PASS" } else { "FAIL" },
            yes(v.every_row_accounted),
            yes(v.duplicate_groups_listed),
            yes(v.no_unmapped_column),
            yes(v.no_errors)
        ),
    );
    o
}

/// The desktop → web column mapping as a Markdown table. "Since" is `base`
/// for a column of the original `CREATE TABLE` and `added` for one a later
/// desktop migration added (an older file may lack it). Consecutive columns
/// of a table with the same presence and disposition share a row.
pub fn mapping_markdown() -> String {
    let mut o = String::new();
    let w = &mut o;
    line(w, "| Desktop column | Since | Web target | Rule / reason |");
    line(w, "|---|---|---|---|");
    for t in catalog::TABLES {
        let (target, rule) = match t.disposition {
            Disposition::Mapped { target, rule } => (format!("**{target}**"), rule),
            Disposition::Dropped { reason } => ("**dropped**".to_owned(), reason),
        };
        line(
            w,
            &format!(
                "| **`{}`** (table) | | {target} | {} |",
                t.name,
                rule.replace('|', "\\|")
            ),
        );
        for run in t
            .columns
            .chunk_by(|a, b| a.presence == b.presence && a.disposition == b.disposition)
        {
            let names: Vec<String> = run
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    if i == 0 {
                        format!("`{}.{}`", t.name, c.name)
                    } else {
                        format!("`{}`", c.name)
                    }
                })
                .collect();
            let since = match run[0].presence {
                Presence::Base => "base",
                Presence::Added { .. } => "added",
            };
            let (target, rule) = match run[0].disposition {
                Disposition::Mapped { target, rule } => (format!("`{target}`"), rule),
                Disposition::Dropped { reason } => ("dropped".to_owned(), reason),
            };
            line(
                w,
                &format!(
                    "| {} | {since} | {target} | {} |",
                    names.join(", "),
                    rule.replace('|', "\\|")
                ),
            );
        }
    }
    line(
        w,
        &format!(
            "| `sqlite_*` | | dropped | {} |",
            catalog::SQLITE_INTERNAL_REASON
        ),
    );
    o
}

fn duplicates(w: &mut String, label: &str, d: &DuplicateSummary) {
    kv(
        w,
        label,
        &format!(
            "{} groups, {} rows, {} merged into the kept row, {} with notes to concatenate",
            d.groups, d.rows_in_groups, d.rows_merged, d.notes_to_concatenate
        ),
    );
    for group in &d.listed {
        let members: Vec<String> = group
            .members
            .iter()
            .map(|m| {
                format!(
                    "{}{} [{}; files {}; ai {}; user {}]",
                    if m.kept { "keep " } else { "merge " },
                    m.legacy_id.as_deref().unwrap_or("·"),
                    m.source,
                    m.archived_files,
                    yes(m.has_ai),
                    yes(m.has_user_layer)
                )
            })
            .collect();
        line(
            w,
            &format!(
                "      {}: {}",
                group.key.as_deref().unwrap_or("(redacted)"),
                members.join(" | ")
            ),
        );
    }
}

fn file_row(w: &mut String, class: &str, n: &FileClassCounts) {
    line(
        w,
        &format!(
            "  {:<22}{:>8}{:>8}{:>9}{:>9}{:>9}{:>12}",
            class,
            n.refs,
            n.files,
            n.present,
            n.missing,
            n.outside_root,
            bytes(n.bytes_present)
        ),
    );
}

fn section(w: &mut String, title: &str) {
    line(w, "");
    line(w, title);
}

fn kv(w: &mut String, key: &str, value: &str) {
    line(w, &format!("  {key:<16} {value}"));
}

fn line(w: &mut String, text: &str) {
    let _ = writeln!(w, "{text}");
}

fn yes(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn join_counts(map: &BTreeMap<String, u64>) -> String {
    if map.is_empty() {
        return "none".to_owned();
    }
    map.iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Bytes in binary units.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(47_644_672), "45.4 MiB");
    }

    #[test]
    fn mapping_lists_every_column() {
        let md = mapping_markdown();
        // Column rows start with "| `table.column`"; grouped rows add
        // ", `column`" per extra column.
        let columns: usize = md
            .lines()
            .filter(|l| l.starts_with("| `") && !l.starts_with("| `sqlite_*`"))
            .map(|l| {
                let first = l.split(" | ").next().unwrap_or_default();
                first.matches('`').count() / 2
            })
            .sum();
        assert_eq!(columns, 121);
        for t in catalog::TABLES {
            for c in t.columns {
                assert!(
                    md.contains(&format!("`{}`", c.name))
                        || md.contains(&format!("`{}.{}`", t.name, c.name))
                );
            }
        }
    }
}
