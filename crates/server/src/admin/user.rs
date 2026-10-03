//! `admin user lock | unlock | restore-db | limits`: restoring one user's
//! library to a point in time (plan §3.5, runbook "One user, point in
//! time"), and a user's limits (P4-07: the storage quota and the daily
//! captures; E4 has no admin pages, so the CLI sets them).
//!
//! `restore-db` also cancels the user's queued `purge` and `bulk` jobs
//! ([`cancel_library_jobs`]): they were asked of the library it replaced.
//!
//! 1. `admin user lock <id>`: the user's requests get 423 `user_locked`, and
//!    the server releases the library at their next request or within its
//!    maintenance interval (30 s). Jobs, the upgrade sweep and `admin
//!    snapshot` leave a locked library alone.
//! 2. `restic restore <snapshot> --include …/users/<id>.sqlite --target …`.
//! 3. `admin user restore-db <id> <file>`: checks the file (integrity,
//!    foreign keys, schema), waits until no process holds the library open,
//!    keeps the current one as `users/<id>/library.pre-restore-<time>.sqlite`
//!    and swaps the restored copy in atomically ([`install_file`]). A handle
//!    the server still holds cannot reopen the library while it is locked.
//! 4. `admin user unlock <id>`: the next request opens the restored library.
//!
//! Objects the restored library references but the store lost are
//! re-archived from live URLs or by the extension (P2–P4); `admin verify`
//! lists them.
//!
//! `admin user limits <id> [--quota-gb N] [--capture-daily N]` prints the
//! limits in force after setting those given (0 means unlimited for both),
//! and audits a change as `user.limits`. A running server reads the quota
//! at its next reservation ([`crate::quota`]).

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Args, Subcommand};
use serde_json::json;
use shelfy_core::db::{is_library_locked, is_valid_user_id, lock_library, unlock_library};
use shelfy_core::repo::RepoError;
use shelfy_core::schema::Kind;

use super::install::{Installed, install_file, kept_path, restore_stamp};
use super::open_existing_control;
use super::verify::check_copy;
use crate::config::DataDir;
use crate::control::audit::{self, Entry};
use crate::control::users::{self, Limits};
use crate::ids::now_ms;
use crate::quota::GIB;

/// Default of `restore-db --wait-secs`: the server's maintenance interval is
/// 30 s, plus requests and jobs that still hold the library.
pub const DEFAULT_RESTORE_WAIT_SECS: u64 = 120;

/// Arguments of `admin user`.
#[derive(Debug, Args)]
pub struct UserArgs {
    /// The command.
    #[command(subcommand)]
    pub command: UserCommand,
}

/// The `admin user` commands.
#[derive(Debug, Subcommand)]
pub enum UserCommand {
    /// Lock a user's library for maintenance: their requests get 423
    /// `user_locked`, and the server releases the library.
    Lock(LockArgs),
    /// Unlock a user's library.
    Unlock(UnlockArgs),
    /// Replace a locked user's library with a restored copy, keeping the
    /// current one next to it.
    RestoreDb(RestoreDbArgs),
    /// Print a user's limits (storage quota, daily captures), after setting
    /// those given.
    Limits(LimitsArgs),
}

/// Arguments of `admin user lock`.
#[derive(Debug, Args)]
pub struct LockArgs {
    /// The user's id.
    #[arg(value_name = "USER_ID")]
    pub user: String,

    /// Why, for whoever finds the lock (stored in the lock file).
    #[arg(long, value_name = "TEXT", default_value = "maintenance")]
    pub reason: String,
}

/// Arguments of `admin user unlock`.
#[derive(Debug, Args)]
pub struct UnlockArgs {
    /// The user's id.
    #[arg(value_name = "USER_ID")]
    pub user: String,
}

/// Arguments of `admin user restore-db`.
#[derive(Debug, Args)]
pub struct RestoreDbArgs {
    /// The user's id.
    #[arg(value_name = "USER_ID")]
    pub user: String,

    /// The restored library: `users/<user_id>.sqlite` from a restore of
    /// `backup-staging/db`.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,

    /// How long to wait for the server to release the library.
    #[arg(long, value_name = "SECONDS", default_value_t = DEFAULT_RESTORE_WAIT_SECS)]
    pub wait_secs: u64,
}

/// Arguments of `admin user limits`.
#[derive(Debug, Args)]
pub struct LimitsArgs {
    /// The user's id.
    #[arg(value_name = "USER_ID")]
    pub user: String,

    /// The storage quota in GiB, media plus database (decimals allowed:
    /// `0.5`); 0 means unlimited.
    #[arg(long = "quota-gb", value_name = "GIB", value_parser = parse_quota_gb)]
    pub quota_bytes: Option<i64>,

    /// Site captures a day (UTC); 0 means unlimited.
    #[arg(long = "capture-daily", value_name = "N")]
    pub capture_daily: Option<u32>,
}

/// The largest `--quota-gb`: 1 PiB.
pub const MAX_QUOTA_GIB: f64 = 1_048_576.0;

/// Parses `--quota-gb` into bytes.
fn parse_quota_gb(raw: &str) -> Result<i64, String> {
    let gib: f64 = raw
        .trim()
        .parse()
        .map_err(|_| format!("{raw:?} is not a number of GiB"))?;
    if !gib.is_finite() || !(0.0..=MAX_QUOTA_GIB).contains(&gib) {
        return Err(format!("the quota must be 0 to {MAX_QUOTA_GIB} GiB"));
    }
    // At most 2^20 GiB of 2^30 bytes: 2^50 bytes, well within an i64.
    Ok((gib * GIB as f64).round() as i64)
}

/// The limits of a user before and after `admin user limits`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitsChange {
    /// Before.
    pub before: Limits,
    /// In force now.
    pub after: Limits,
}

/// Runs `admin user …`.
///
/// # Errors
///
/// See [`lock`], [`unlock`] and [`restore_db`].
pub fn run(data: &DataDir, args: &UserArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    match &args.command {
        UserCommand::Lock(args) => {
            if lock(data, &args.user, &args.reason)? {
                writeln!(
                    out,
                    "locked user {}: their requests get 423 user_locked until \"admin user \
                     unlock {}\"",
                    args.user, args.user
                )?;
            } else {
                writeln!(out, "user {} was already locked", args.user)?;
            }
        }
        UserCommand::Unlock(args) => {
            if unlock(data, &args.user)? {
                writeln!(out, "unlocked user {}", args.user)?;
            } else {
                writeln!(out, "user {} was not locked", args.user)?;
            }
        }
        UserCommand::RestoreDb(args) => {
            let installed = restore_db(
                data,
                &args.user,
                &args.file,
                Duration::from_secs(args.wait_secs),
            )?;
            // The swap is done: say so before anything else can fail.
            writeln!(
                out,
                "restored user {}'s library from {} ({} bytes)",
                args.user,
                args.file.display(),
                installed.bytes
            )?;
            if let Some(kept) = &installed.kept {
                writeln!(out, "the previous library is kept as {}", kept.display())?;
            }
            out.flush()?;
            let cancelled = cancel_library_jobs(data, &args.user);
            match &cancelled {
                Ok(n) => writeln!(
                    out,
                    "cancelled {n} queued purge and bulk jobs of the previous library"
                )?,
                Err(_) => writeln!(
                    out,
                    "could not cancel the user's queued purge and bulk jobs: cancel them \
                     before the unlock"
                )?,
            }
            writeln!(
                out,
                "unlock the user when done: shelfy-server admin user unlock {}",
                args.user
            )?;
            out.flush()?;
            record_restore(data, &args.user, &installed).context(
                "the library is restored (see above), but its audit row could not be written",
            )?;
            cancelled.context(
                "the library is restored (see above), but its queued purge and bulk jobs \
                 could not be cancelled",
            )?;
        }
        UserCommand::Limits(args) => {
            let capture_daily = args.capture_daily.map(i64::from);
            let change = limits(data, &args.user, args.quota_bytes, capture_daily)?;
            if args.quota_bytes.is_some() || capture_daily.is_some() {
                writeln!(out, "updated the limits of user {}", args.user)?;
            } else {
                writeln!(out, "limits of user {}", args.user)?;
            }
            writeln!(out, "quota: {}", describe_quota(change.after.quota_bytes))?;
            writeln!(
                out,
                "captures a day: {}",
                describe_count(change.after.capture_daily_limit)
            )?;
        }
    }
    Ok(())
}

/// Sets the limits of `user_id` that are given (bytes; captures a day) and
/// returns them before and after. With a change, the audit log gets
/// `user.limits`; without one, nothing is written.
///
/// # Errors
///
/// An invalid id, no control database, no such user, a negative limit, or
/// the control database failed.
pub fn limits(
    data: &DataDir,
    user_id: &str,
    quota_bytes: Option<i64>,
    capture_daily_limit: Option<i64>,
) -> anyhow::Result<LimitsChange> {
    if !is_valid_user_id(user_id) {
        anyhow::bail!("invalid user id {user_id:?}: expected 1-64 ASCII letters and digits");
    }
    let control = open_existing_control(data)?;
    let now = now_ms();
    let change = control
        .write(|tx| -> Result<Option<LimitsChange>, RepoError> {
            let Some(before) = users::limits(tx, user_id)? else {
                return Ok(None);
            };
            if quota_bytes.is_none() && capture_daily_limit.is_none() {
                return Ok(Some(LimitsChange {
                    before,
                    after: before,
                }));
            }
            let after = users::set_limits(tx, user_id, quota_bytes, capture_daily_limit)?
                .ok_or(RepoError::NotFound)?;
            let meta = json!({
                "via": "cli",
                "quotaBytes": after.quota_bytes,
                "captureDailyLimit": after.capture_daily_limit,
                "previous": {
                    "quotaBytes": before.quota_bytes,
                    "captureDailyLimit": before.capture_daily_limit,
                },
            });
            let entry = Entry {
                action: audit::USER_LIMITS,
                actor_user_id: None,
                target: Some(user_id),
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            Ok(Some(LimitsChange { before, after }))
        })
        .context("cannot update the limits")?;
    change.ok_or_else(|| anyhow::anyhow!("no user {user_id} in this data directory"))
}

/// A quota for people: `unlimited`, or GiB and bytes.
fn describe_quota(bytes: i64) -> String {
    if bytes <= 0 {
        return "unlimited".to_owned();
    }
    let whole = u64::try_from(bytes).unwrap_or(0);
    if whole % GIB == 0 {
        format!("{} GiB ({bytes} bytes)", whole / GIB)
    } else {
        let value = bytes as f64 / GIB as f64;
        format!("{value:.3} GiB ({bytes} bytes)")
    }
}

/// A daily count for people: `unlimited`, or the number.
fn describe_count(n: i64) -> String {
    if n <= 0 {
        "unlimited".to_owned()
    } else {
        n.to_string()
    }
}

/// Locks `user_id`'s library; returns `false` when it was already locked.
///
/// # Errors
///
/// An invalid id, no control database, no such user, or the file system
/// refused.
pub fn lock(data: &DataDir, user_id: &str, reason: &str) -> anyhow::Result<bool> {
    check_user(data, user_id)?;
    let now = now_ms();
    let note = json!({ "lockedAt": now, "reason": reason }).to_string();
    let locked = lock_library(&data.users_dir(), user_id, &note)
        .with_context(|| format!("cannot lock user {user_id}"))?;
    if locked {
        record(data, audit::USER_LOCK, user_id, &json!({ "via": "cli" }))?;
    }
    Ok(locked)
}

/// Unlocks `user_id`'s library; returns `false` when it was not locked. The
/// user need not exist any more (a stale lock of a deleted account).
///
/// # Errors
///
/// An invalid id, or the file system refused.
pub fn unlock(data: &DataDir, user_id: &str) -> anyhow::Result<bool> {
    if !is_valid_user_id(user_id) {
        anyhow::bail!("invalid user id {user_id:?}: expected 1-64 ASCII letters and digits");
    }
    let unlocked = unlock_library(&data.users_dir(), user_id)
        .with_context(|| format!("cannot unlock user {user_id}"))?;
    if unlocked && data.control_db().is_file() {
        record(data, audit::USER_UNLOCK, user_id, &json!({ "via": "cli" }))?;
    }
    Ok(unlocked)
}

/// Replaces the library of the locked user `user_id` with the copy at
/// `file`, waiting up to `wait` for the server to release it. The caller
/// then records it in the audit log ([`record_restore`]), after reporting
/// the swap: a failed audit write must not hide a restore that happened.
///
/// # Errors
///
/// An invalid id, no such user, a user that is not locked, a copy that fails
/// its checks, a library still open after `wait`, or an I/O error. The live
/// library is unchanged on error.
pub fn restore_db(
    data: &DataDir,
    user_id: &str,
    file: &std::path::Path,
    wait: Duration,
) -> anyhow::Result<Installed> {
    check_user(data, user_id)?;
    if !is_library_locked(&data.users_dir(), user_id)? {
        anyhow::bail!(
            "user {user_id} is not locked: run \"shelfy-server admin user lock {user_id}\" first"
        );
    }
    if !file.is_file() {
        anyhow::bail!("no file at {}", file.display());
    }
    let check = check_copy(file, Kind::Library)?;
    if !check.problems.is_empty() {
        anyhow::bail!(
            "{} cannot be restored: {}",
            file.display(),
            check.problems.join("; ")
        );
    }
    let live = data.library_db(user_id);
    if let Some(dir) = live.parent() {
        crate::config::create_private_dir(dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let keep = kept_path(&live, &restore_stamp(now_ms()));
    install_file(file, &live, Kind::Library, &keep, wait)
}

/// After a [`restore_db`]: cancels `user_id`'s queued and running `purge`
/// and `bulk` jobs, which were asked of the library the restore replaced. A
/// purge queued during the lock (the nightly one, or an emptying asked just
/// before) would otherwise delete the trash the restore brought back, and a
/// bulk job would redo changes on it (P1-11 review). Returns how many. The
/// server's scheduler finds them cancelled when it next tries them.
///
/// # Errors
///
/// The control database cannot be written.
pub fn cancel_library_jobs(data: &DataDir, user_id: &str) -> anyhow::Result<u64> {
    let control = open_existing_control(data)?;
    let now = now_ms();
    let cancelled = control
        .write(|tx| -> Result<_, RepoError> {
            let mut cancelled = 0;
            for kind in [crate::jobs::purge::KIND, crate::jobs::bulk::KIND] {
                cancelled += crate::control::jobs::cancel_kind(tx, user_id, kind, now)?.len();
            }
            Ok(cancelled)
        })
        .context("cannot cancel the user's jobs")?;
    Ok(u64::try_from(cancelled).unwrap_or(u64::MAX))
}

/// Writes the audit row of a [`restore_db`] of `user_id`'s library.
///
/// # Errors
///
/// The control database cannot be written.
pub fn record_restore(data: &DataDir, user_id: &str, installed: &Installed) -> anyhow::Result<()> {
    let meta = json!({
        "via": "cli",
        "bytes": installed.bytes,
        "keptPrevious": installed.kept.is_some(),
    });
    record(data, audit::LIBRARY_RESTORE, user_id, &meta)
}

/// Checks the id and that the user exists.
fn check_user(data: &DataDir, user_id: &str) -> anyhow::Result<()> {
    if !is_valid_user_id(user_id) {
        anyhow::bail!("invalid user id {user_id:?}: expected 1-64 ASCII letters and digits");
    }
    let control = open_existing_control(data)?;
    let id = user_id.to_owned();
    let user = control
        .read(|conn| users::get(conn, &id))
        .context("cannot read the users")?;
    if user.is_none() {
        anyhow::bail!("no user {user_id} in this data directory");
    }
    Ok(())
}

/// Writes an audit row for the CLI.
fn record(
    data: &DataDir,
    action: &str,
    user_id: &str,
    meta: &serde_json::Value,
) -> anyhow::Result<()> {
    let control = open_existing_control(data)?;
    let now = now_ms();
    control
        .write(|tx| -> Result<_, RepoError> {
            let entry = Entry {
                action,
                actor_user_id: None,
                target: Some(user_id),
                meta: Some(meta),
            };
            audit::record(tx, &entry, now)
        })
        .context("cannot write the audit log")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotas_are_read_in_gib() {
        assert_eq!(parse_quota_gb("5"), Ok(5 << 30));
        assert_eq!(parse_quota_gb(" 0.5 "), Ok(1 << 29));
        assert_eq!(parse_quota_gb("0"), Ok(0));
        assert_eq!(parse_quota_gb("1048576"), Ok(1 << 50));
        for bad in ["", "-1", "five", "NaN", "inf", "1048577"] {
            assert!(parse_quota_gb(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn limits_are_described_for_people() {
        assert_eq!(describe_quota(0), "unlimited");
        assert_eq!(describe_quota(5 << 30), "5 GiB (5368709120 bytes)");
        assert_eq!(describe_quota(1 << 29), "0.500 GiB (536870912 bytes)");
        assert_eq!(describe_count(0), "unlimited");
        assert_eq!(describe_count(20), "20");
    }
}
