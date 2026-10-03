//! Offline, manifest-bound recovery of existing Instagram covers from local
//! files. Dry-run opens SQLite read-only and never stages media. Apply uses
//! the archive store/quota contract; stop the API first and restart afterwards
//! so its in-memory quota ledger and library caches cannot outlive this write.
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, ensure};
use clap::Args;
use rusqlite::{Connection, OpenFlags, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use shelfy_core::ingest::archive::ArchivePolicy;
use shelfy_core::repo::RepoError;
use shelfy_media::refs::Origin;
use shelfy_media::render;
use shelfy_media::store::{IngestLimits, MediaStore};
use shelfy_media::{Digest, MediaKind};

use crate::config::{Config, DataDir};
use crate::events::model::ChangeReason;
use crate::jobs::archive::{ArchiveArgs, select::Slot, store};
use crate::library::{self, Change};
use crate::quota::{self, DEFAULT_MEDIA_BUDGET_GB, QuotaConfig};
use crate::state::{AppState, blocking};

const MAX_ENTRIES: usize = 1_000;
const MANIFEST_BYTES: u64 = 1_048_576;
const IMAGE_BYTES: u64 = 15 * 1_048_576;
const BUNDLE_BYTES: u64 = 256 * 1_048_576;

/// A private bundle: manifest and relative image files. Defaults to dry-run.
#[derive(Debug, Args)]
pub struct BackfillArgs {
    /// Private JSON manifest; never print its contents or commit it.
    #[arg(long)]
    pub manifest: PathBuf,
    /// Write the validated covers. Requires an offline API acknowledgement.
    #[arg(long, requires = "server_stopped")]
    pub apply: bool,
    /// The API is stopped; restart it after applying to retire its caches.
    #[arg(long, requires = "apply")]
    pub server_stopped: bool,
    /// Match the instance's media budget (GiB; 0 means unlimited).
    #[arg(long, env = "SHELFY_MEDIA_BUDGET_GB", default_value_t = DEFAULT_MEDIA_BUDGET_GB)]
    pub media_budget_gb: u64,
    #[command(flatten)]
    pub archive: ArchiveArgs,
}

/// Identity and byte-level evidence; contains private owner data.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    version: u32,
    user_id: String,
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    post_key: String,
    native_id: String,
    shortcode: String,
    media_type: String,
    file: PathBuf,
    ext: String,
    sha256: String,
    bytes: u64,
}

/// Aggregates only, suitable for the operator's report.
#[derive(Default, Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub entries: usize,
    pub eligible: usize,
    pub skipped_existing: usize,
    pub skipped_trashed: usize,
    pub input_bytes: u64,
    pub stored: usize,
    pub added_bytes: u64,
}

struct Plan {
    user_id: String,
    entries: Vec<(Entry, Vec<u8>)>,
    report: Report,
}

enum Eligibility {
    Empty,
    Existing,
    Trashed,
}

async fn offline_blocking<T, F>(task: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|_| anyhow::anyhow!("offline task failed"))?
}

fn readonly(path: &Path) -> anyhow::Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| anyhow::anyhow!("cannot open existing database read-only"))
}

fn eligible(conn: &Connection, entry: &Entry) -> Result<Eligibility, RepoError> {
    let found = conn.query_row(
        "SELECT native_id, shortcode, media_type, platform, deleted_at IS NOT NULL,
                cover_object IS NOT NULL OR EXISTS(SELECT 1 FROM post_media m
                  WHERE m.post_id=p.id AND (m.object_id IS NOT NULL OR m.video_object_id IS NOT NULL))
         FROM posts p WHERE key=?1",
        [&entry.post_key],
        |r| Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,
                 r.get::<_,String>(3)?,r.get::<_,bool>(4)?,r.get::<_,bool>(5)?)),
    ).optional()?;
    let Some((native, shortcode, kind, platform, trashed, existing)) = found else {
        return Err(RepoError::Invalid {
            field: "manifest",
            reason: "unknown post",
        });
    };
    if native != entry.native_id
        || shortcode.as_deref() != Some(entry.shortcode.as_str())
        || kind != entry.media_type
        || platform != "instagram"
    {
        return Err(RepoError::Invalid {
            field: "manifest",
            reason: "post identity mismatch",
        });
    }
    Ok(if trashed {
        Eligibility::Trashed
    } else if existing {
        Eligibility::Existing
    } else {
        Eligibility::Empty
    })
}

fn image_bytes(base: &Path, entry: &Entry) -> anyhow::Result<Vec<u8>> {
    ensure!(
        !entry.file.as_os_str().is_empty()
            && entry
                .file
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "invalid relative image file"
    );
    let path = base.join(&entry.file);
    let meta =
        fs::symlink_metadata(&path).map_err(|_| anyhow::anyhow!("image file unavailable"))?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "image must be a regular file"
    );
    let canonical =
        fs::canonicalize(&path).map_err(|_| anyhow::anyhow!("image file unavailable"))?;
    ensure!(canonical.starts_with(base), "image leaves private bundle");
    ensure!(
        entry.bytes > 0 && entry.bytes <= IMAGE_BYTES && meta.len() == entry.bytes,
        "image byte count mismatch or over cap"
    );
    let mut bytes = Vec::new();
    File::open(&canonical)
        .map_err(|_| anyhow::anyhow!("image file unavailable"))?
        .take(IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("image read failed"))?;
    ensure!(
        bytes.len() as u64 == entry.bytes,
        "image changed during read"
    );
    ensure!(
        Digest::parse_hex(&entry.sha256) == Some(Digest::of(&bytes)),
        "image hash mismatch"
    );
    let kind = MediaKind::sniff(&bytes).ok_or_else(|| anyhow::anyhow!("image header invalid"))?;
    ensure!(
        kind.is_renderable() && MediaKind::from_ext(&entry.ext) == Some(kind),
        "image header/type mismatch"
    );
    // Fully decode during validation: an undecodable header must not be
    // attached as a nominally analyzable cover by store::prepare's fallback.
    render::render_bytes(&bytes, render::RenderSpec::G480)
        .map_err(|_| anyhow::anyhow!("image cannot be decoded within render limits"))?;
    Ok(bytes)
}

fn plan(data: &DataDir, path: &Path) -> anyhow::Result<Plan> {
    let manifest_meta =
        fs::symlink_metadata(path).map_err(|_| anyhow::anyhow!("manifest unavailable"))?;
    ensure!(
        manifest_meta.is_file() && manifest_meta.len() <= MANIFEST_BYTES,
        "invalid manifest file or over cap"
    );
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| anyhow::anyhow!("manifest unavailable"))?
        .take(MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("manifest read failed"))?;
    ensure!(bytes.len() as u64 <= MANIFEST_BYTES, "manifest is over cap");
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid manifest JSON"))?;
    ensure!(
        manifest.version == 1
            && !manifest.entries.is_empty()
            && manifest.entries.len() <= MAX_ENTRIES,
        "invalid manifest version or population"
    );
    ensure!(
        manifest
            .entries
            .iter()
            .try_fold(0_u64, |sum, e| sum.checked_add(e.bytes))
            .is_some_and(|bytes| bytes <= BUNDLE_BYTES),
        "private bundle is over cap"
    );
    let base = fs::canonicalize(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )
    .map_err(|_| anyhow::anyhow!("manifest directory unavailable"))?;
    let control = readonly(&data.control_db())?;
    let owners = control
        .prepare("SELECT id FROM users WHERE role='owner' AND status='active'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        owners.len() == 1 && owners.first() == Some(&manifest.user_id),
        "manifest must name the unique active owner"
    );
    let db = readonly(&data.library_db(&manifest.user_id))?;
    let mut seen = HashSet::new();
    let mut report = Report {
        entries: manifest.entries.len(),
        ..Report::default()
    };
    let mut entries = Vec::new();
    for (index, entry) in manifest.entries.into_iter().enumerate() {
        ensure!(
            seen.insert(entry.post_key.clone()),
            "duplicate manifest identity at entry {}",
            index + 1
        );
        let bytes =
            image_bytes(&base, &entry).map_err(|e| anyhow::anyhow!("entry {}: {e}", index + 1))?;
        report.input_bytes += entry.bytes;
        match eligible(&db, &entry)
            .map_err(|_| anyhow::anyhow!("entry {}: post identity validation failed", index + 1))?
        {
            Eligibility::Empty => report.eligible += 1,
            Eligibility::Existing => report.skipped_existing += 1,
            Eligibility::Trashed => report.skipped_trashed += 1,
        }
        entries.push((entry, bytes));
    }
    Ok(Plan {
        user_id: manifest.user_id,
        entries,
        report,
    })
}

/// Validate a private manifest without writing any database or media file.
///
/// # Errors
/// Invalid identities/files or unavailable existing databases; errors omit
/// private keys, hashes, paths and captions.
pub fn inspect_manifest(data: &DataDir, path: &Path) -> anyhow::Result<Report> {
    Ok(plan(data, path)?.report)
}

/// Apply to an offline instance, using the same store/quota transaction as
/// archive uploads. Each committed entry is independently retryable.
///
/// # Errors
/// Validation, quotas or stores failed; entries already committed remain
/// safe to replay and the command never creates or overwrites a post.
pub async fn apply_manifest(state: &AppState, path: &Path) -> anyhow::Result<Report> {
    let data = state.config().data_dir.clone();
    let path = path.to_owned();
    let plan = offline_blocking(move || plan(&data, &path)).await?;
    let mut report = Report {
        entries: plan.report.entries,
        input_bytes: plan.report.input_bytes,
        ..Report::default()
    };
    for (index, (entry, bytes)) in plan.entries.into_iter().enumerate() {
        let entry = std::sync::Arc::new(entry);
        let db = state
            .user_db(&plan.user_id)
            .await
            .map_err(|_| anyhow::anyhow!("library unavailable"))?;
        let e = entry.clone();
        let current = blocking(move || db.read(|conn| eligible(conn, &e)))
            .await
            .map_err(|_| anyhow::anyhow!("entry {}: identity recheck failed", index + 1))?;
        match current {
            Eligibility::Existing => {
                report.skipped_existing += 1;
                continue;
            }
            Eligibility::Trashed => {
                report.skipped_trashed += 1;
                continue;
            }
            Eligibility::Empty => report.eligible += 1,
        }
        // Preparation may re-encode a poster: reserve the enforced archive
        // cap, not only the source length, before staging any bytes.
        let reservation = quota::reserve(state, &plan.user_id, IMAGE_BYTES)
            .await
            .map_err(|_| anyhow::anyhow!("entry {}: quota reservation refused", index + 1))?;
        let media = MediaStore::new(state.config().data_dir.users_dir())
            .user(&plan.user_id)
            .map_err(|_| anyhow::anyhow!("media store unavailable"))?;
        let db = state
            .user_db(&plan.user_id)
            .await
            .map_err(|_| anyhow::anyhow!("library unavailable"))?;
        let e = entry.clone();
        let media_for_prepare = media.clone();
        let (target, prepared) = offline_blocking(move || {
            let target = db
                .read(|conn| store::target_of(conn, &e.post_key, Slot::Cover))?
                .ok_or_else(|| anyhow::anyhow!("target unavailable"))?;
            let staged = media_for_prepare
                .ingest(std::io::Cursor::new(bytes), IngestLimits::ARCHIVE_IMAGE)?;
            let prepared = store::prepare(&media_for_prepare, staged, &target)?;
            Ok::<_, anyhow::Error>((target, prepared))
        })
        .await
        .map_err(|_| anyhow::anyhow!("entry {}: preparation failed", index + 1))?;
        let modes = state.config().archive.modes;
        let now = crate::ids::now_ms();
        let written = library::write(state, &plan.user_id, ChangeReason::Archive, move |tx| {
            if !matches!(eligible(tx, &entry)?, Eligibility::Empty) {
                return Ok(Change {
                    value: None,
                    keys: Some(vec![]),
                });
            }
            // Re-resolve after validation to prevent a row id being reused.
            let current =
                store::target_of(tx, &entry.post_key, Slot::Cover)?.ok_or(RepoError::NotFound)?;
            if current.post_id != target.post_id {
                return Err(RepoError::NotFound);
            }
            let policy = ArchivePolicy::read(tx, modes)?;
            let result = store::commit(
                tx,
                &store::StoreContext {
                    media: &media,
                    origin: Origin::Migration,
                    policy: &policy,
                    now,
                },
                &current,
                prepared,
                reservation,
            )?;
            Ok(Change {
                value: Some(result),
                keys: Some(vec![entry.post_key.clone()]),
            })
        })
        .await
        .map_err(|_| anyhow::anyhow!("entry {}: store transaction failed", index + 1))?;
        match written.value {
            Some(store::Committed::Stored { added_bytes, .. }) => {
                report.stored += 1;
                report.added_bytes += added_bytes;
            }
            Some(store::Committed::Unwanted) | None => report.skipped_existing += 1,
        }
    }
    Ok(report)
}

/// CLI entry point; dry-run deliberately avoids AppState (migrations, quota
/// notices and directory creation are writes).
pub fn run(data: &DataDir, args: &BackfillArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let report = if args.apply {
        if !args.server_stopped {
            bail!("apply requires the API to be stopped");
        }
        // Validate everything before opening any writable database.
        inspect_manifest(data, &args.manifest)?;
        let mut config = Config::with_data_dir(data.clone());
        config.archive = crate::jobs::archive::ArchiveConfig::from_args(&args.archive);
        config.quota = QuotaConfig::from_gib(args.media_budget_gb)
            .ok_or_else(|| anyhow::anyhow!("invalid media budget"))?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let state = offline_blocking(move || AppState::open(config))
                .await
                .map_err(|_| anyhow::anyhow!("cannot open offline instance"))?;
            apply_manifest(&state, &args.manifest).await
        })?
    } else {
        inspect_manifest(data, &args.manifest)?
    };
    writeln!(
        out,
        "mode={} {}",
        if args.apply { "apply" } else { "dry-run" },
        serde_json::to_string(&report)?
    )?;
    Ok(())
}
