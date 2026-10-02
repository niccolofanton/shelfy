//! `admin login-link`: a one-time sign-in link for an existing account (E4;
//! pulled forward from P1-13).
//!
//! The way in while SMTP is optional: the operator runs it next to the server
//! and opens the printed URL, `<public url>/login/magic#<token>`, in a
//! browser; the SPA's sign-in page redeems it after a click. Without the SPA,
//! `POST /api/v1/auth/magic-links/redeem` with `{"token": "<token>"}` does the
//! same (`deploy/README.md` shows it with curl). The link works once and
//! expires in 15 minutes. It is printed to stdout only, never logged; the
//! database keeps its SHA-256, and `audit_log` records that a link was
//! minted.
//!
//! P1-13 adds `--purpose reauth`.

use std::io::Write;
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use shelfy_core::repo::RepoError;

use super::open_existing_control;
use crate::auth::AuthConfig;
use crate::auth::magic_link::{self, Via};
use crate::config::{DataDir, PublicUrl, PublicUrlArg};
use crate::control::magic_links::Purpose;
use crate::control::users::{self, Status};
use crate::ids::now_ms;
use crate::telemetry::redact::Redacted;

/// Arguments of `admin login-link`.
#[derive(Debug, Args)]
pub struct LoginLinkArgs {
    #[command(flatten)]
    pub public: PublicUrlArg,

    /// Email of the account (E4: the owner's, kept in the osn secrets). Never
    /// printed back.
    #[arg(
        long,
        env = "SHELFY_OWNER_EMAIL",
        hide_env_values = true,
        value_name = "EMAIL"
    )]
    pub email: String,
}

/// A minted sign-in link.
#[derive(Clone, Debug)]
pub struct LoginLink {
    /// `<public url>/login/magic#<token>`; the only copy of the token.
    pub url: Redacted<String>,
    /// Expiry, unix ms.
    pub expires_at: i64,
}

/// Runs `admin login-link`.
///
/// # Errors
///
/// See [`create_login_link`].
pub fn run(data: &DataDir, args: &LoginLinkArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let ttl = AuthConfig::default().magic_link_ttl;
    let link = create_login_link(data, &args.public.public_url, &args.email, ttl)?;
    writeln!(
        out,
        "one-time sign-in link, valid {} minutes (it is shown only now):",
        ttl.as_secs() / 60
    )?;
    writeln!(out, "{}", link.url.expose())?;
    Ok(())
}

/// Mints a sign-in link, valid `ttl`, for the active account with `email`.
///
/// # Errors
///
/// No control database, a malformed email, no account with this email, or
/// an account that is not active.
pub fn create_login_link(
    data: &DataDir,
    public_url: &PublicUrl,
    email: &str,
    ttl: Duration,
) -> anyhow::Result<LoginLink> {
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
            magic_link::mint(tx, &user.id, Purpose::Login, ttl, Via::Cli, now)
        })
        .map_err(|err| match err {
            RepoError::NotFound => anyhow::anyhow!("no account uses this email"),
            RepoError::Conflict("status") => anyhow::anyhow!("the account is not active"),
            other => anyhow::Error::new(other),
        })
        .context("cannot create the sign-in link")?;
    Ok(LoginLink {
        url: magic_link::link_url(public_url, &minted.token),
        expires_at: minted.expires_at,
    })
}
