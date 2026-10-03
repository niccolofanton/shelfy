//! Aggregate-only diagnostics for a user's durable catalog queue.
use crate::config::DataDir;
use anyhow::Context as _;
use clap::Args;
use shelfy_core::ai::queue;
use shelfy_core::db::UserDb;
use std::io::Write;

/// Arguments of `admin ai-status`.
#[derive(Debug, Args)]
pub struct AiStatusArgs {
    /// User ID whose queue to inspect.
    #[arg(long)]
    pub user: String,
}
/// Reads library state and the drain lease, printing no library content.
pub fn run(data: &DataDir, args: &AiStatusArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let user = super::synth::resolve_user(data, Some(&args.user), None)?;
    let control = super::open_existing_control(data)?;
    let now = crate::ids::now_ms();
    let live: bool = control.read(|conn| Ok::<_, anyhow::Error>(conn.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE user_id=?1 AND kind='ai.drain' AND state='running' AND lease_until>?2)", rusqlite::params![user,now], |r| r.get(0))?)).context("cannot read AI drain lease")?;
    let db = UserDb::open(data.library_db(&user), &Default::default())?;
    let (counts, oldest, errors, provider) = db.read(|conn| {
        Ok::<_, shelfy_core::repo::RepoError>((
            queue::state_counts(conn)?,
            queue::oldest_due_pending(conn, now)?,
            queue::errors_by_code(conn)?,
            queue::provider_status(conn)?,
        ))
    })?;
    writeln!(
        out,
        "unanalyzed={} pending={} analyzing={} done={} errors={}",
        counts.unanalyzed, counts.pending, counts.analyzing, counts.done, counts.error
    )?;
    writeln!(
        out,
        "oldest_due_age_ms={} due_pending={} orphaned_analyzing={}",
        oldest.map_or(0, |v| v.0),
        oldest.map_or(0, |v| v.1),
        if live { 0 } else { counts.analyzing }
    )?;
    writeln!(
        out,
        "provider_last_observed={} waiting_for_offline={}",
        provider.as_deref().unwrap_or("unknown"),
        if provider.as_deref() == Some("offline") {
            counts.pending
        } else {
            0
        }
    )?;
    for (code, n) in errors {
        writeln!(out, "error.{code}={n}")?;
    }
    Ok(())
}
