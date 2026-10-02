//! `admin user lock | unlock | restore-db`: restoring one user's library to
//! a point in time (plan §3.5, runbook "One user, point in time").
//!
//! 1. `admin user lock <id>`: the user's requests get 423 `user_locked`, and
//!    the server releases the library at their next request or within its
//!    maintenance interval (30 s). Jobs, the upgrade sweep and `admin
//!    snapshot` leave a locked library alone.
//! 2. `restic restore <snapshot> --include …/users/<id>.sqlite --target …`.
//! 3. `admin user restore-db <id> <file>`: checks the file (integrity,
//!    foreign keys, schema), waits until no process holds the library open,
//!    keeps the current one as `users/<id>/library.pre-restore-<time>.sqlite`
//!    and swaps the restored copy in atomically ([`install_file`]).
//! 4. `admin user unlock <id>`: the next request opens the restored library.
//!
//! Objects the restored library references but the store lost are
//! re-archived from live URLs or by the extension (P2–P4); `admin verify`
//! lists them.

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
use crate::control::users;
use crate::ids::now_ms;

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
            writeln!(
                out,
                "unlock the user when done: shelfy-server admin user unlock {}",
                args.user
            )?;
        }
    }
    Ok(())
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
/// `file`, waiting up to `wait` for the server to release it.
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
    let installed = install_file(file, &live, Kind::Library, &keep, wait)?;
    let meta = json!({
        "via": "cli",
        "bytes": installed.bytes,
        "keptPrevious": installed.kept.is_some(),
    });
    record(data, audit::LIBRARY_RESTORE, user_id, &meta)?;
    Ok(installed)
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
