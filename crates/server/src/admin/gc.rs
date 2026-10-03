//! Aggregate-only operator collection. Dry-run opens existing databases
//! read-only and does not restamp objects, sweep files or correct usage.
use std::io::Write;
use std::sync::Arc;

use anyhow::{Context as _, ensure};
use clap::Args;
use rusqlite::{Connection, OpenFlags};
use shelfy_core::db::{UserDb, is_library_locked};
use shelfy_core::repo::RepoError;
use shelfy_media::refs;
use shelfy_media::store::MediaStore;

use crate::config::DataDir;
use crate::jobs::{Clock, gc};
use crate::quota::Quotas;

#[derive(Debug, Args)]
pub struct GcArgs {
    /// One existing active user; absent means every active user.
    #[arg(long)]
    pub user: Option<String>,
    /// Count eligible objects without changing any data or files.
    #[arg(long)]
    pub dry_run: bool,
}

/// Runs GC and prints objects/bytes only.
///
/// # Errors
///
/// Unknown user, locked library, database or filesystem failure.
pub fn run(data: &DataDir, args: &GcArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    ensure!(
        data.control_db().is_file(),
        "control database does not exist"
    );
    let control_read =
        Connection::open_with_flags(data.control_db(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let users = control_read
        .prepare(
            "SELECT id FROM users WHERE status='active' AND (?1 IS NULL OR id=?1) ORDER BY id",
        )?
        .query_map([args.user.as_deref()], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        args.user.is_none() || !users.is_empty(),
        "no such active user"
    );
    drop(control_read);
    let now = crate::ids::now_ms();
    let cutoff = now.saturating_sub(86_400_000);
    let mut report = gc::Report::default();
    let control = if args.dry_run {
        None
    } else {
        Some(Arc::new(super::open_existing_control(data)?))
    };
    let quotas = control.as_ref().map(|control| {
        Quotas::new(
            Default::default(),
            Arc::clone(control),
            Clock::default(),
            data.root().to_path_buf(),
        )
    });
    for user in users {
        let media = MediaStore::new(data.users_dir()).user(&user)?;
        let path = data.library_db(&user);
        if !path.is_file() {
            continue;
        }
        ensure!(
            !is_library_locked(&data.users_dir(), &user)?,
            "library is locked for maintenance"
        );
        if args.dry_run {
            let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let (objects, bytes) = refs::garbage_totals(&conn, cutoff)?;
            report.objects += objects;
            report.bytes += bytes;
            continue;
        }
        let db = UserDb::open(path, &Default::default()).context("cannot open library")?;
        let quotas = quotas.as_ref().expect("apply quotas");
        let control = control.as_ref().expect("apply control");
        db.write(|tx| refs::restamp(tx, now))?;
        loop {
            let chunk = gc::collect_chunk(&db, &media, quotas, control, &user, cutoff, None)?;
            report.objects += chunk.objects;
            report.bytes += chunk.bytes;
            if chunk.objects == 0 {
                break;
            }
        }
        db.write(|tx| -> Result<(), RepoError> {
            media
                .sweep_temp(gc::RETENTION)
                .map_err(shelfy_core::db::DbError::Io)?;
            gc::prune_taxonomy(tx, control, &user)?;
            let media_bytes = tx.query_row(
                "SELECT coalesce(sum(bytes),0) FROM media_objects",
                [],
                |r| r.get::<_, i64>(0),
            )?;
            let pages = tx.query_row("PRAGMA page_count", [], |r| r.get::<_, i64>(0))?;
            let size = tx.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0))?;
            quotas.record_count(&user, media_bytes, pages.saturating_mul(size))?;
            Ok(())
        })?;
    }
    writeln!(out, "objects={} bytes={}", report.objects, report.bytes)?;
    Ok(())
}
