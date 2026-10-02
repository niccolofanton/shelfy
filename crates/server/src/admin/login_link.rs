//! `admin login-link`: a one-time link for an existing account (E4), to sign
//! in or to re-authenticate.
//!
//! The way in while SMTP is optional, and the way to a first passkey: the
//! operator runs it next to the server and opens the printed URL in a
//! browser. A sign-in link (`--purpose login`, the default) is
//! `<public url>/login/magic#<token>`: the SPA's sign-in page redeems it
//! after a click. Without the SPA, `POST /api/v1/auth/magic-links/redeem`
//! with `{"token": "<token>"}` does the same (`deploy/README.md` shows it
//! with curl). A re-authentication link (`--purpose reauth`) is
//! `<public url>/login/reauth#<token>`: opened in the browser that is signed
//! in to the account, it confirms a sensitive action (removing a passkey,
//! later minting a token) through `POST /api/v1/auth/reauth/finish`.
//!
//! Either link works once and expires in 15 minutes. It is printed to stdout
//! only, never logged; the database keeps its SHA-256, and `audit_log`
//! records that a link was minted, with its purpose.

use std::io::Write;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Args, ValueEnum};
use shelfy_core::repo::RepoError;

use super::open_existing_control;
use crate::auth::AuthConfig;
use crate::auth::magic_link::{self, Via};
use crate::config::{DataDir, PublicUrl, PublicUrlArg};
use crate::control::magic_links::Purpose;
use crate::control::users::{self, Status};
use crate::ids::now_ms;
use crate::telemetry::redact::Redacted;

/// What a link is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum LinkPurpose {
    /// Sign in (the page `/login/magic`).
    #[default]
    Login,
    /// Re-authenticate the signed-in session before a sensitive action (the
    /// page `/login/reauth`).
    Reauth,
}

impl LinkPurpose {
    const fn purpose(self) -> Purpose {
        match self {
            Self::Login => Purpose::Login,
            Self::Reauth => Purpose::Reauth,
        }
    }
}

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

    /// What the link is for: `login` signs in; `reauth` confirms a sensitive
    /// action in a browser that is signed in already.
    #[arg(long, value_enum, default_value_t = LinkPurpose::Login)]
    pub purpose: LinkPurpose,
}

/// A minted link.
#[derive(Clone, Debug)]
pub struct LoginLink {
    /// `<public url>/login/magic#<token>` or `<public url>/login/reauth#<token>`;
    /// the only copy of the token.
    pub url: Redacted<String>,
    /// Expiry, unix ms.
    pub expires_at: i64,
}

/// Runs `admin login-link`.
///
/// # Errors
///
/// See [`create_link`].
pub fn run(data: &DataDir, args: &LoginLinkArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let ttl = AuthConfig::default().magic_link_ttl;
    let link = create_link(
        data,
        &args.public.public_url,
        &args.email,
        args.purpose,
        ttl,
    )?;
    let minutes = ttl.as_secs() / 60;
    match args.purpose {
        LinkPurpose::Login => writeln!(
            out,
            "one-time sign-in link, valid {minutes} minutes (it is shown only now):"
        )?,
        LinkPurpose::Reauth => writeln!(
            out,
            "one-time re-authentication link, valid {minutes} minutes; open it in the browser \
             that is signed in (it is shown only now):"
        )?,
    }
    writeln!(out, "{}", link.url.expose())?;
    Ok(())
}

/// Mints a sign-in link, valid `ttl`, for the active account with `email`.
///
/// # Errors
///
/// See [`create_link`].
pub fn create_login_link(
    data: &DataDir,
    public_url: &PublicUrl,
    email: &str,
    ttl: Duration,
) -> anyhow::Result<LoginLink> {
    create_link(data, public_url, email, LinkPurpose::Login, ttl)
}

/// Mints a link for `purpose`, valid `ttl`, for the active account with
/// `email`, and audit-logs it.
///
/// # Errors
///
/// No control database, a malformed email, no account with this email, or
/// an account that is not active.
pub fn create_link(
    data: &DataDir,
    public_url: &PublicUrl,
    email: &str,
    purpose: LinkPurpose,
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
            magic_link::mint(tx, &user.id, purpose.purpose(), ttl, Via::Cli, now)
        })
        .map_err(|err| match err {
            RepoError::NotFound => anyhow::anyhow!("no account uses this email"),
            RepoError::Conflict("status") => anyhow::anyhow!("the account is not active"),
            other => anyhow::Error::new(other),
        })
        .context("cannot create the link")?;
    let url = match purpose {
        LinkPurpose::Login => magic_link::link_url(public_url, &minted.token),
        LinkPurpose::Reauth => magic_link::reauth_link_url(public_url, &minted.token),
    };
    Ok(LoginLink {
        url,
        expires_at: minted.expires_at,
    })
}
