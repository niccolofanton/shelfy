//! `admin migrate-token`: a `migrate`-scoped API token for `shelfy-migrate`
//! (T9).
//!
//! The migration CLI authenticates with a bearer token (§2.11). Its own
//! sign-in is the device flow (P1-17's `POST /auth/device/*`, which P1-19's
//! `shelfy-migrate login` drives); the operator can still mint the token
//! here and hand it to `shelfy-migrate run --token` (or
//! `SHELFY_MIGRATE_TOKEN`). Both mint with [`crate::auth::api_tokens::mint`].
//! The token:
//!
//! - has the `migrate` scope only: the migration routes accept it, every
//!   other route refuses it;
//! - expires 7 days after it is minted (`api_tokens.expires_at`, §2.11);
//! - is printed on stdout and nowhere else: the database keeps its SHA-256,
//!   and `audit_log` records only that a token was minted.

use std::io::Write;
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use shelfy_core::repo::RepoError;

use super::open_existing_control;
use crate::auth::api_tokens::{self, MIGRATE_LABEL, Mint, Via};
use crate::auth::bearer::Scope;
use crate::config::DataDir;
use crate::control::api_tokens::TokenKind;
use crate::control::users::{self, Status};
use crate::ids::now_ms;
use crate::telemetry::redact::Redacted;

/// How long a migration token works (§2.11: 7 days).
pub use crate::auth::api_tokens::MIGRATE_TOKEN_TTL;

/// Arguments of `admin migrate-token`.
#[derive(Debug, Args)]
pub struct MigrateTokenArgs {
    /// Email of the account whose library the migration fills (E4: the
    /// owner's). Never printed back.
    #[arg(
        long,
        env = "SHELFY_OWNER_EMAIL",
        hide_env_values = true,
        value_name = "EMAIL"
    )]
    pub email: String,
}

/// A minted migration token.
#[derive(Clone, Debug)]
pub struct MigrateToken {
    /// The token id (`api_tokens.id`).
    pub id: String,
    /// `shx_…`: the only copy of the value.
    pub token: Redacted<String>,
    /// Expiry, unix ms.
    pub expires_at: i64,
}

/// Runs `admin migrate-token`: the token alone on stdout, so a script can
/// capture it; the note about its lifetime on stderr.
///
/// # Errors
///
/// See [`create_migrate_token`].
pub fn run(data: &DataDir, args: &MigrateTokenArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let minted = create_migrate_token(data, &args.email, MIGRATE_TOKEN_TTL)?;
    eprintln!(
        "migration token {} for shelfy-migrate, valid {} days (it is shown only now)",
        minted.id,
        MIGRATE_TOKEN_TTL.as_secs() / 86_400
    );
    writeln!(out, "{}", minted.token.expose())?;
    Ok(())
}

/// Mints a `migrate` token, valid `ttl`, for the active account with `email`.
///
/// # Errors
///
/// No control database, a malformed email, no account with this email, or an
/// account that is not active.
pub fn create_migrate_token(
    data: &DataDir,
    email: &str,
    ttl: Duration,
) -> anyhow::Result<MigrateToken> {
    let email = users::normalize_email(email).map_err(|err| match err {
        RepoError::Invalid { field, reason } => anyhow::anyhow!("invalid {field}: {reason}"),
        other => anyhow::Error::new(other),
    })?;
    let control = open_existing_control(data)?;
    let now = now_ms();
    let minted = control
        .write(|tx| {
            let Some(user) = users::find_by_email(tx, &email)? else {
                return Err(RepoError::NotFound);
            };
            if user.status != Status::Active {
                return Err(RepoError::Conflict("status"));
            }
            let mint = Mint {
                user_id: &user.id,
                kind: TokenKind::Migrate,
                scopes: &[Scope::Migrate],
                label: Some(MIGRATE_LABEL),
                ttl: Some(ttl),
                via: Via::Cli,
                actor: None,
            };
            api_tokens::mint(tx, &mint, now)
        })
        .map_err(|err| match err {
            RepoError::NotFound => anyhow::anyhow!("no account uses this email"),
            RepoError::Conflict("status") => anyhow::anyhow!("the account is not active"),
            other => anyhow::Error::new(other),
        })
        .context("cannot create the migration token")?;
    Ok(MigrateToken {
        id: minted.row.id,
        token: minted.token,
        expires_at: minted.row.expires_at.unwrap_or(now),
    })
}
