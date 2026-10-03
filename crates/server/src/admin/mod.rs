//! `shelfy-server admin …`: operator commands (plan §2.11, §3.5).
//!
//! They run next to the server (`docker exec`, or the host timers) on the same
//! data directory; SQLite's WAL mode and busy timeout make that safe.
//!
//! Output rules: results go to stdout, diagnostics to stderr, nothing to the
//! logs. Secrets are never printed, with exceptions by design: `invite` and
//! `login-link` (`--purpose login|reauth`) print their one-time link, and
//! `migrate-token` its API token, the only copy of the secret (the database
//! keeps its SHA-256).
//!
//! Backups and restores (plan §3.5): `snapshot` copies the databases for
//! restic, `verify` checks restored copies against the live data, `user
//! lock | unlock | restore-db` restores one user's library and
//! `install-snapshots` restores a whole host.
//!
//! Test libraries and budgets (P1-05): `synth` fills a user's empty library
//! with synthetic posts and media, `bench` times the read routes on it.
//! `create-user` (E6) adds the member account itself, such as the live
//! host's mock account; every one of the above already takes a member by id
//! or email, not the owner specifically.
//!
//! This file is the command dispatch: a new command adds its module and one
//! line here.

pub mod bench;
pub mod create_user;
pub mod install;
pub mod invite;
pub mod login_link;
pub mod migrate_token;
pub mod owner;
pub mod snapshot;
pub mod synth;
pub mod user;
pub mod verify;

use std::io::Write;

use anyhow::Context as _;
use clap::{Args, Subcommand};
use shelfy_core::db::{ControlDb, ControlDbConfig};

use crate::config::{DataDir, DataDirArg};
use crate::telemetry;

/// Arguments of `shelfy-server admin`.
#[derive(Debug, Args)]
pub struct AdminArgs {
    #[command(flatten)]
    pub data: DataDirArg,

    /// The command.
    #[command(subcommand)]
    pub command: AdminCommand,
}

/// The operator commands.
#[derive(Debug, Subcommand)]
pub enum AdminCommand {
    /// Create the owner account (idempotent for the same email).
    CreateOwner(owner::CreateOwnerArgs),
    /// Create a member account for tests (idempotent for the same email),
    /// such as the live host's E6 mock account. The instance stays
    /// owner-only (E4): there is no invite redemption route, so this is the
    /// only way to get a second account.
    CreateUser(create_user::CreateUserArgs),
    /// Create a one-time invite link for a new member. Unused while the
    /// instance is owner-only (E4).
    Invite(invite::InviteArgs),
    /// Print a one-time link (15 minutes) for an existing account: a sign-in
    /// link, the way in while email is not configured (E4), or with
    /// `--purpose reauth` a re-authentication link.
    LoginLink(login_link::LoginLinkArgs),
    /// Copy the control database and the user libraries with SQLite's online
    /// backup, while the server runs.
    Snapshot(snapshot::SnapshotArgs),
    /// Print a `migrate`-scoped API token (7 days) for `shelfy-migrate run`.
    MigrateToken(migrate_token::MigrateTokenArgs),
    /// Check restored database copies: integrity, row counts against the live
    /// databases, media references.
    Verify(verify::VerifyArgs),
    /// Lock, unlock or restore one user's library.
    User(user::UserArgs),
    /// Install the database copies of a snapshot directory as the live
    /// databases (full restore, server stopped).
    InstallSnapshots(install::InstallArgs),
    /// Fill a user's empty library with synthetic posts and media (for
    /// benchmarks and tests; run it with the server stopped).
    Synth(synth::SynthArgs),
    /// Time the read routes on a user's library against the §6.2 budgets
    /// (aggregates only).
    Bench(bench::BenchArgs),
}

/// Runs an admin command, writing its output to stdout.
///
/// # Errors
///
/// The command failed; the message says why.
pub fn run(args: AdminArgs) -> anyhow::Result<()> {
    telemetry::init_for_cli()?;
    let data = DataDir::new(args.data.data_dir).context("SHELFY_DATA_DIR is unusable")?;
    let mut out = std::io::stdout().lock();
    match args.command {
        AdminCommand::CreateOwner(args) => owner::run(&data, &args, &mut out),
        AdminCommand::CreateUser(args) => create_user::run(&data, &args, &mut out),
        AdminCommand::Invite(args) => invite::run(&data, &args, &mut out),
        AdminCommand::LoginLink(args) => login_link::run(&data, &args, &mut out),
        AdminCommand::Snapshot(args) => snapshot::run(&data, &args, &mut out),
        AdminCommand::MigrateToken(args) => migrate_token::run(&data, &args, &mut out),
        AdminCommand::Verify(args) => verify::run(&data, &args, &mut out),
        AdminCommand::User(args) => user::run(&data, &args, &mut out),
        AdminCommand::InstallSnapshots(args) => install::run(&data, &args, &mut out),
        AdminCommand::Synth(args) => synth::run(&data, &args, &mut out),
        AdminCommand::Bench(args) => bench::run(&data, &args, &mut out),
    }?;
    out.flush()?;
    Ok(())
}

/// Opens the control database of an existing installation. Unlike the server,
/// a command never creates one in a mistyped data directory.
fn open_existing_control(data: &DataDir) -> anyhow::Result<ControlDb> {
    let path = data.control_db();
    if !path.is_file() {
        anyhow::bail!(
            "no control database at {}: check SHELFY_DATA_DIR, or run create-owner first",
            path.display()
        );
    }
    open_control(data)
}

/// Opens (creating it if needed) the control database with one reader.
fn open_control(data: &DataDir) -> anyhow::Result<ControlDb> {
    data.create_layout()
        .with_context(|| format!("cannot create the layout of {}", data.root().display()))?;
    let path = data.control_db();
    let config = ControlDbConfig {
        readers: 1,
        ..ControlDbConfig::default()
    };
    ControlDb::open(&path, &config).with_context(|| format!("cannot open {}", path.display()))
}
