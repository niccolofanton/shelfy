//! `shelfy-migrate run` (plan §4.1 step 4): the desktop library to the
//! server, end to end.
//!
//! 1. **Read** a consistent snapshot of the library ([`crate::snapshot`],
//!    OI-8) and dry-run it ([`crate::plan`]). A plan that fails its criteria
//!    stops the run.
//! 2. **Bundle** it ([`crate::bundle`]) into the work directory.
//! 3. **Ask** the server which objects it lacks
//!    (`POST /migrations/missing-objects`) and **upload** only those with
//!    tus, then the database last. Unfinished uploads are recorded in
//!    `state.json` in the work directory; a re-run continues each one where
//!    it stopped, and skips the objects the server already has.
//! 4. **Install** (`POST /migrations`) and follow the install until it ends.
//! 5. **Reconcile**: desktop rows, bundle rows and installed rows side by
//!    side. On success the work files are removed, unless `--keep-work`.
//!
//! The desktop library and its files are only read. Progress goes to the log
//! writer (stderr in the CLI); nothing printed carries content, keys or
//! paths, except the orphan list the owner asks for (`--list-orphans`).

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use shelfy_core::legacy::LegacyDb;

use crate::bundle::{self, Bundle, BundleOptions, BundleSummary};
use crate::client::{self, ApiError, Client, ObjectRef};
use crate::files;
use crate::plan::{PlanOptions, plan_with_mapping};
use crate::report::PlanReport;
use crate::snapshot;

/// Bytes per tus `PATCH`: half the server's 16 MiB limit per chunk.
pub const CHUNK_BYTES: usize = 8 * 1024 * 1024;
/// File name of the resume state in the work directory.
pub const STATE_FILE: &str = "state.json";
/// Directory of the bundle in the work directory.
pub const BUNDLE_DIR: &str = "bundle";
/// `purpose` of an object upload.
pub const PURPOSE_OBJECT: &str = "migration-object";
/// `purpose` of the database upload.
pub const PURPOSE_DATABASE: &str = "migration-db";

/// Attempts per chunk when the connection breaks.
const CHUNK_ATTEMPTS: u32 = 5;

/// What `run` does.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The desktop library (`<userData>/shelfy.sqlite`).
    pub db: PathBuf,
    /// The desktop userData directory holding `assets/`.
    pub media_root: Option<PathBuf>,
    /// The server's public origin.
    pub server: String,
    /// A `migrate` token.
    pub token: String,
    /// Where the snapshot, the bundle and the resume state go.
    pub work_dir: PathBuf,
    /// Include kept videos.
    pub with_videos: bool,
    /// Merge into a non-empty web library (not supported by the server yet).
    pub merge: bool,
    /// Keep the work files after a successful install.
    pub keep_work: bool,
    /// Also list the orphan files under `assets/` (OI-11).
    pub list_orphans: bool,
    /// How often the install is polled.
    pub poll_interval: Duration,
}

/// Upload counts of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadCounts {
    /// Objects of the bundle.
    pub objects: u64,
    /// Of which the server lacked.
    pub missing: u64,
    pub uploaded: u64,
    pub uploaded_bytes: u64,
    /// Uploads continued from an earlier run.
    pub resumed: u64,
    pub database_bytes: u64,
}

/// How long each phase took, ms.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Durations {
    pub bundle_ms: u64,
    pub upload_ms: u64,
    pub install_ms: u64,
}

/// One line of the reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub what: String,
    /// Desktop rows, from the dry run.
    pub desktop: Option<u64>,
    /// Rows written to the bundle.
    pub bundle: u64,
    /// Rows of the installed library.
    pub installed: u64,
    pub matches: bool,
}

/// The result of a run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOutcome {
    pub summary: BundleSummary,
    pub upload: UploadCounts,
    pub migration_id: String,
    pub report: client::InstallReport,
    pub durations: Durations,
    pub reconciliation: Vec<Line>,
    /// Every line matches.
    pub matches: bool,
    /// Files under `assets/` that no row references (OI-11): not migrated.
    pub orphan_files: u64,
    pub orphan_bytes: u64,
    /// Those files, relative to `assets/` (`--list-orphans`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orphans: Option<Vec<String>>,
}

/// Resume state: unfinished uploads by object hash.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    uploads: BTreeMap<String, String>,
}

impl State {
    fn load(path: &Path) -> State {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> anyhow::Result<()> {
        let partial = path.with_extension("json.partial");
        fs::write(&partial, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("cannot write {}", partial.display()))?;
        fs::rename(&partial, path).with_context(|| format!("cannot write {}", path.display()))
    }
}

/// Runs the migration; progress goes to `log`.
///
/// # Errors
///
/// The library cannot be read or fails its dry run, the server cannot be
/// reached or refuses the bundle, or the install fails. The work directory
/// then keeps what a re-run continues from.
pub fn run(opts: &RunOptions, log: &mut dyn Write) -> anyhow::Result<RunOutcome> {
    let started = Instant::now();
    fs::create_dir_all(&opts.work_dir)
        .with_context(|| format!("cannot create {}", opts.work_dir.display()))?;
    let client = Client::new(&opts.server, &opts.token)?;

    // 1–2. Snapshot, dry run, bundle.
    let source = snapshot::prepare(&opts.db, &opts.work_dir)?;
    if source.snapshot {
        writeln!(
            log,
            "the library has a -wal file: read from a snapshot (close Shelfy for a final migration)"
        )?;
    }
    let legacy = LegacyDb::open(&source.path)
        .with_context(|| format!("cannot read the library at {}", opts.db.display()))?;
    let media_root = opts.media_root.clone().or_else(|| {
        let dir = opts.db.parent()?;
        dir.join("assets").is_dir().then(|| dir.to_path_buf())
    });
    let now = now_ms();
    let (plan, mapping) = plan_with_mapping(
        &legacy,
        &PlanOptions {
            media_root: media_root.clone(),
            redact: true,
            now_ms: now,
        },
    )
    .context("cannot read the library")?;
    if !plan.verdict.pass {
        anyhow::bail!(
            "the dry run fails ({}): run `shelfy-migrate plan` for details",
            plan.errors.join("; ")
        );
    }
    anyhow::ensure!(
        media_root.is_some(),
        "no media root: pass --media-root <userData directory>"
    );
    let bundle_dir = opts.work_dir.join(BUNDLE_DIR);
    fs::create_dir_all(&bundle_dir)?;
    let bundle = bundle::build(
        &legacy,
        &mapping,
        &bundle_dir,
        &BundleOptions {
            with_videos: opts.with_videos,
            snapshot: source.snapshot,
            now_ms: now,
        },
    )?;
    drop(legacy);
    let orphans = match (&media_root, opts.list_orphans) {
        (Some(root), true) => Some(files::orphan_paths(root, &mapping.files.referenced())),
        _ => None,
    };
    let bundle_ms = elapsed_ms(started);
    writeln!(
        log,
        "bundle: {} posts, {} objects ({}), database {}",
        bundle.summary.posts.written.values().sum::<u64>(),
        bundle.objects.len(),
        bytes(bundle.summary.objects.bytes),
        bytes(bundle.db_bytes)
    )?;

    // 3. Upload what the server lacks, then the database.
    let uploading = Instant::now();
    let state_path = opts.work_dir.join(STATE_FILE);
    let mut state = State::load(&state_path);
    let mut counts = UploadCounts {
        objects: bundle.objects.len() as u64,
        database_bytes: bundle.db_bytes,
        ..UploadCounts::default()
    };
    let refs: Vec<ObjectRef> = bundle
        .objects
        .iter()
        .map(|o| ObjectRef {
            sha256: o.sha256.clone(),
            ext: o.ext.to_owned(),
            bytes: o.bytes,
        })
        .collect();
    let missing: std::collections::HashSet<String> =
        client.missing_objects(&refs)?.into_iter().collect();
    counts.missing = missing.len() as u64;
    writeln!(
        log,
        "upload: the server lacks {} of {} objects",
        counts.missing, counts.objects
    )?;
    let total_missing_bytes: u64 = bundle
        .objects
        .iter()
        .filter(|o| missing.contains(&o.sha256))
        .map(|o| o.bytes)
        .sum();
    let mut last_report = Instant::now();
    for object in bundle
        .objects
        .iter()
        .filter(|o| missing.contains(&o.sha256))
    {
        let metadata = [
            ("purpose", PURPOSE_OBJECT),
            ("sha256", object.sha256.as_str()),
            ("ext", object.ext),
        ];
        upload(
            &client,
            &mut state,
            &state_path,
            &object.sha256,
            &object.path,
            object.bytes,
            &metadata,
            &mut counts,
        )?;
        counts.uploaded += 1;
        counts.uploaded_bytes += object.bytes;
        if last_report.elapsed() >= Duration::from_secs(5) || counts.uploaded == counts.missing {
            writeln!(
                log,
                "upload: {}/{} objects, {} of {}",
                counts.uploaded,
                counts.missing,
                bytes(counts.uploaded_bytes),
                bytes(total_missing_bytes)
            )?;
            last_report = Instant::now();
        }
    }
    let metadata = [
        ("purpose", PURPOSE_DATABASE),
        ("sha256", bundle.db_sha256.as_str()),
    ];
    let db_url = upload(
        &client,
        &mut state,
        &state_path,
        &bundle.db_sha256,
        &bundle.db_path,
        bundle.db_bytes,
        &metadata,
        &mut counts,
    )?;
    let db_upload_id = db_url
        .rsplit('/')
        .next()
        .filter(|id| !id.is_empty())
        .context("the server gave the database upload no id")?
        .to_owned();
    writeln!(log, "upload: database sent")?;
    let upload_ms = elapsed_ms(uploading);

    // 4. Install.
    let installing = Instant::now();
    let mut status = client.start_migration(&db_upload_id, opts.merge)?;
    writeln!(log, "install {}: {}", status.id, status.stage)?;
    let mut stage = status.stage.clone();
    while status.state == "running" {
        thread::sleep(opts.poll_interval);
        status = client.migration(&status.id)?;
        if status.stage != stage {
            writeln!(log, "install: {}", status.stage)?;
            stage = status.stage.clone();
        }
    }
    let install_ms = elapsed_ms(installing);
    if status.state != "succeeded" {
        let (code, detail) = status
            .error
            .clone()
            .map_or(("unknown".to_owned(), None), |f| (f.code, f.detail));
        anyhow::bail!(
            "the install failed: {code}{}",
            detail.map(|d| format!(" ({d})")).unwrap_or_default()
        );
    }
    let report = status
        .report
        .clone()
        .context("the server reported no reconciliation")?;

    // 5. Reconcile, then clean up.
    let reconciliation = reconcile(&plan, &bundle, &report);
    let matches = reconciliation.iter().all(|line| line.matches);
    if !opts.keep_work {
        clean_work_dir(&opts.work_dir, source.snapshot)?;
    }
    Ok(RunOutcome {
        summary: bundle.summary,
        upload: counts,
        migration_id: status.id,
        report,
        durations: Durations {
            bundle_ms,
            upload_ms,
            install_ms,
        },
        reconciliation,
        matches,
        orphan_files: plan.files.orphans.files,
        orphan_bytes: plan.files.orphans.bytes,
        orphans,
    })
}

/// Uploads the file at `path` (`length` bytes) with tus, continuing the
/// upload recorded for `key` in `state` when the server still has it.
/// Returns the upload's URL.
#[allow(clippy::too_many_arguments)]
fn upload(
    client: &Client,
    state: &mut State,
    state_path: &Path,
    key: &str,
    path: &Path,
    length: u64,
    metadata: &[(&str, &str)],
    counts: &mut UploadCounts,
) -> anyhow::Result<String> {
    let mut offset = 0;
    let mut url = None;
    if let Some(known) = state.uploads.get(key)
        && let Some(found) = client.upload_state(known)?
        && found.length == length
    {
        offset = found.offset;
        url = Some(known.clone());
        if offset > 0 {
            counts.resumed += 1;
        }
    }
    let url = match url {
        Some(url) => url,
        None => {
            let url = client.create_upload(length, metadata)?;
            state.uploads.insert(key.to_owned(), url.clone());
            state.save(state_path)?;
            url
        }
    };
    let mut file = File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut buffer = vec![0u8; CHUNK_BYTES];
    let mut failures = 0;
    while offset < length {
        file.seek(SeekFrom::Start(offset))?;
        let wanted =
            usize::try_from((length - offset).min(CHUNK_BYTES as u64)).unwrap_or(CHUNK_BYTES);
        read_exact_or_shorter(&mut file, &mut buffer[..wanted])
            .with_context(|| format!("{} changed while it was uploaded", path.display()))?;
        match client.append(&url, offset, &buffer[..wanted]) {
            Ok(next) => {
                offset = next;
                failures = 0;
            }
            Err(err) => {
                // A refusal is final, except an offset conflict; a broken
                // connection is retried from the offset the server has.
                let refused = err
                    .downcast_ref::<ApiError>()
                    .is_some_and(|e| e.status < 500 && e.status != 409);
                failures += 1;
                if refused || failures >= CHUNK_ATTEMPTS {
                    return Err(err);
                }
                thread::sleep(Duration::from_millis(500 * u64::from(failures)));
                match client.upload_state(&url)? {
                    Some(found) => offset = found.offset,
                    None => return Err(err.context("the server dropped the upload")),
                }
            }
        }
    }
    state.uploads.remove(key);
    state.save(state_path)?;
    Ok(url)
}

/// Fills `buffer`; a file that ends early changed since it was hashed.
fn read_exact_or_shorter(file: &mut File, buffer: &mut [u8]) -> io::Result<()> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the file is shorter than when it was hashed",
                ));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Desktop rows (from the dry run), bundle rows and installed rows.
fn reconcile(plan: &PlanReport, bundle: &Bundle, report: &client::InstallReport) -> Vec<Line> {
    let outcome = |table: &str, outcome: &str| {
        plan.tables
            .iter()
            .find(|t| t.table == table)
            .map(|t| t.outcomes.get(outcome).copied().unwrap_or(0))
    };
    let summary = &bundle.summary;
    let installed = &report.installed;
    let mut lines = Vec::new();
    let mut line = |what: &str, desktop: Option<u64>, bundle: u64, installed: u64| {
        let matches = bundle == installed && desktop.is_none_or(|d| d == bundle);
        lines.push(Line {
            what: what.to_owned(),
            desktop,
            bundle,
            installed,
            matches,
        });
    };
    let mut platforms: Vec<&String> = summary
        .posts
        .written
        .keys()
        .chain(installed.posts.keys())
        .collect();
    platforms.sort();
    platforms.dedup();
    for platform in platforms {
        let desktop = plan.posts.by_platform.get(platform).copied().unwrap_or(0);
        let written = summary.posts.written.get(platform).copied().unwrap_or(0);
        let read = summary.posts.read.get(platform).copied().unwrap_or(0);
        line(
            &format!("posts {platform}"),
            // The desktop rows of a platform minus those merged into another.
            Some(desktop - (read - written).min(desktop)),
            written,
            installed.posts.get(platform).copied().unwrap_or(0),
        );
    }
    line(
        "slides",
        outcome("post_media", "insert"),
        summary.rows.slides,
        installed.slides,
    );
    line(
        "collections",
        outcome("collections", "insert"),
        summary.rows.collections,
        installed.collections,
    );
    line(
        "memberships",
        outcome("post_collections", "insert"),
        summary.rows.memberships,
        installed.memberships,
    );
    line(
        "post_tags",
        outcome("post_tags", "insert"),
        summary.rows.post_tags,
        installed.post_tags,
    );
    line(
        "post_entities",
        outcome("post_entities", "insert"),
        summary.rows.post_entities,
        installed.post_entities,
    );
    line(
        "tag_aliases",
        outcome("tag_alias", "insert"),
        summary.rows.tag_aliases,
        installed.tag_aliases,
    );
    line(
        "tag_clusters",
        outcome("tag_cluster", "insert"),
        summary.rows.tag_clusters,
        installed.tag_clusters,
    );
    line(
        "web captures",
        Some(plan.web.web_captures),
        summary.rows.web_captures,
        installed.web_captures,
    );
    line(
        "media objects",
        None,
        summary.objects.count,
        installed.media_objects,
    );
    let plan_files = &plan.files.totals;
    lines.push(Line {
        what: "files present".to_owned(),
        desktop: Some(plan_files.present),
        bundle: summary.files.present,
        installed: summary.files.present,
        matches: plan_files.present == summary.files.present,
    });
    lines.push(Line {
        what: "files missing".to_owned(),
        desktop: Some(plan_files.missing),
        bundle: summary.files.missing,
        installed: summary.files.missing,
        matches: plan_files.missing == summary.files.missing,
    });
    lines
}

/// Removes what `run` wrote to the work directory, and the directory when
/// nothing else is left in it.
fn clean_work_dir(dir: &Path, snapshot: bool) -> anyhow::Result<()> {
    let bundle_dir = dir.join(BUNDLE_DIR);
    if bundle_dir.exists() {
        fs::remove_dir_all(&bundle_dir)
            .with_context(|| format!("cannot remove {}", bundle_dir.display()))?;
    }
    let mut leftovers = vec![dir.join(STATE_FILE)];
    if snapshot {
        leftovers.push(dir.join(snapshot::SNAPSHOT_FILE));
    }
    for file in leftovers {
        match fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("cannot remove {}", file.display())),
        }
    }
    // Only an empty directory goes: the work directory may be shared.
    let _ = fs::remove_dir(dir);
    Ok(())
}

/// The reconciliation as text, for the CLI.
#[must_use]
pub fn render(outcome: &RunOutcome) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let s = &outcome.summary;
    let r = &outcome.report;
    let u = &outcome.upload;
    let d = &outcome.durations;
    let _ = writeln!(
        out,
        "shelfy-migrate {} · run · install {}",
        env!("CARGO_PKG_VERSION"),
        outcome.migration_id
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Bundle    {} posts, {} merged duplicates · {} objects ({}) · files: {} present, {} missing{}",
        s.posts.written.values().sum::<u64>(),
        s.posts.merged,
        s.objects.count,
        bytes(s.objects.bytes),
        s.files.present,
        s.files.missing,
        if s.files.videos_excluded > 0 {
            format!(
                ", {} videos left out ({})",
                s.files.videos_excluded,
                bytes(s.files.videos_excluded_bytes)
            )
        } else {
            String::new()
        }
    );
    let _ = writeln!(
        out,
        "Upload    {} of {} objects missing on the server: {} uploaded ({}), {} resumed · database {}",
        u.missing,
        u.objects,
        u.uploaded,
        bytes(u.uploaded_bytes),
        u.resumed,
        bytes(u.database_bytes)
    );
    let _ = writeln!(
        out,
        "Install   {} objects stored ({} from uploads, {} already stored, {}) · g480 {} rendered, {} existing, {} failed, {} not renderable · ThumbHash {}",
        r.objects.total,
        r.objects.from_uploads,
        r.objects.already_stored,
        bytes(r.objects.bytes),
        r.renditions.rendered,
        r.renditions.existing,
        r.renditions.failed,
        r.renditions.not_renderable,
        r.renditions.thumbhashes
    );
    let a = &r.archive;
    let states: Vec<String> = a.by_state.iter().map(|(k, v)| format!("{k} {v}")).collect();
    let _ = writeln!(
        out,
        "Archive   posts by state: {} · covers to archive: IG valid {}, IG expired {}, IG no expiry {}, X {}, Pinterest {}, other {} · image slides pending {}",
        states.join(" · "),
        a.ig_cover_valid,
        a.ig_cover_expired,
        a.ig_cover_no_expiry,
        a.x_cover,
        a.pinterest_cover,
        a.other_cover,
        a.image_slides_pending
    );
    let _ = writeln!(
        out,
        "Orphans   {} files ({}) under assets/ that no row references: not migrated{}",
        outcome.orphan_files,
        bytes(outcome.orphan_bytes),
        if outcome.orphans.is_some() {
            " (listed below)"
        } else {
            " (--list-orphans lists them)"
        }
    );
    let _ = writeln!(
        out,
        "Time      bundle {} · upload {} · install {}",
        seconds(d.bundle_ms),
        seconds(d.upload_ms),
        seconds(d.install_ms)
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  {:<20} {:>9} {:>9} {:>9}",
        "reconciliation", "desktop", "bundle", "installed"
    );
    for line in &outcome.reconciliation {
        let _ = writeln!(
            out,
            "  {:<20} {:>9} {:>9} {:>9}{}",
            line.what,
            line.desktop
                .map_or_else(|| "-".to_owned(), |d| d.to_string()),
            line.bundle,
            line.installed,
            if line.matches { "" } else { "  MISMATCH" }
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Verdict   {}",
        if outcome.matches {
            "every count matches"
        } else {
            "MISMATCH: see the lines above"
        }
    );
    if let Some(orphans) = &outcome.orphans {
        let _ = writeln!(out);
        let _ = writeln!(out, "Orphan files, relative to assets/:");
        for path in orphans {
            let _ = writeln!(out, "  {path}");
        }
    }
    out
}

fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
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

fn seconds(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_times_read_well() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(seconds(1234), "1.2 s");
    }

    #[test]
    fn state_survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        assert!(State::load(&path).uploads.is_empty());
        let mut state = State::default();
        state
            .uploads
            .insert("abc".into(), "http://x.test/api/v1/uploads/U1".into());
        state.save(&path).unwrap();
        assert_eq!(State::load(&path).uploads, state.uploads);
    }

    #[test]
    fn cleanup_leaves_other_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        fs::create_dir_all(work.join(BUNDLE_DIR)).unwrap();
        fs::write(work.join(BUNDLE_DIR).join("library.sqlite"), b"x").unwrap();
        fs::write(work.join(STATE_FILE), b"{}").unwrap();
        fs::write(work.join(snapshot::SNAPSHOT_FILE), b"x").unwrap();
        fs::write(work.join("mine.txt"), b"x").unwrap();
        clean_work_dir(&work, true).unwrap();
        assert!(work.join("mine.txt").exists());
        assert!(!work.join(BUNDLE_DIR).exists());
        assert!(!work.join(STATE_FILE).exists());
        fs::remove_file(work.join("mine.txt")).unwrap();
        fs::create_dir_all(work.join(BUNDLE_DIR)).unwrap();
        clean_work_dir(&work, false).unwrap();
        assert!(!work.exists(), "an emptied work directory goes too");
    }
}
