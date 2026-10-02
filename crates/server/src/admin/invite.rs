//! `admin invite`: a one-time invite link for a new member (plan §2.11).
//!
//! Unused while E4 keeps the instance owner-only: the command works, but no
//! route accepts invites yet. The link is printed once; the database keeps
//! only the token's SHA-256.

use std::io::Write;

use anyhow::Context as _;
use clap::Args;
use serde_json::json;
use shelfy_core::repo::RepoError;

use super::open_existing_control;
use crate::config::{DataDir, PublicUrl, PublicUrlArg};
use crate::control::audit::{self, Entry};
use crate::control::invites::{self, NewInvite};
use crate::control::users::{self, Role};
use crate::ids::now_ms;
use crate::telemetry::redact::Redacted;
use crate::tokens::SecretToken;

/// Default validity of an invite (§2.11: 7 days).
pub const DEFAULT_TTL_DAYS: u32 = 7;

const DAY_MS: i64 = 86_400_000;

/// Arguments of `admin invite`.
#[derive(Debug, Args)]
pub struct InviteArgs {
    #[command(flatten)]
    pub public: PublicUrlArg,

    /// Only this email may accept the invite.
    #[arg(long, value_name = "EMAIL")]
    pub email: Option<String>,

    /// Days until the link expires.
    #[arg(
        long,
        value_name = "DAYS",
        default_value_t = DEFAULT_TTL_DAYS,
        value_parser = clap::value_parser!(u32).range(1..=30)
    )]
    pub ttl_days: u32,
}

/// A created invite.
#[derive(Clone, Debug)]
pub struct InviteLink {
    /// `<public url>/invite/<token>`; the only copy of the token.
    pub url: Redacted<String>,
    /// Expiry, unix ms.
    pub expires_at: i64,
}

/// Runs `admin invite`.
///
/// # Errors
///
/// See [`create_invite`].
pub fn run(data: &DataDir, args: &InviteArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let link = create_invite(
        data,
        &args.public.public_url,
        args.email.as_deref(),
        args.ttl_days,
    )?;
    writeln!(
        out,
        "one-time invite link, valid {} days (it is shown only now):",
        args.ttl_days
    )?;
    writeln!(out, "{}", link.url.expose())?;
    Ok(())
}

/// Creates an invite for a member, valid `ttl_days`, optionally locked to
/// `email`, on behalf of the owner.
///
/// # Errors
///
/// No control database or no owner yet, or a malformed email.
pub fn create_invite(
    data: &DataDir,
    public_url: &PublicUrl,
    email: Option<&str>,
    ttl_days: u32,
) -> anyhow::Result<InviteLink> {
    let email = email
        .map(users::normalize_email)
        .transpose()
        .map_err(|err| match err {
            RepoError::Invalid { field, reason } => anyhow::anyhow!("invalid {field}: {reason}"),
            other => anyhow::Error::new(other),
        })?;
    let control = open_existing_control(data)?;
    let token = SecretToken::generate();
    let hash = token.hash();
    let now = now_ms();
    let expires_at = now + i64::from(ttl_days) * DAY_MS;
    control
        .write(|tx| -> Result<_, RepoError> {
            let Some(owner) = users::find_owner(tx)? else {
                return Err(RepoError::NotFound);
            };
            let invite = NewInvite {
                token_hash: &hash,
                email: email.as_deref(),
                role: Role::Member,
                created_by: &owner.id,
                expires_at,
            };
            invites::insert(tx, &invite, now)?;
            let meta = json!({
                "via": "cli",
                "ttlDays": ttl_days,
                "emailLocked": email.is_some(),
            });
            let entry = Entry {
                action: audit::INVITE_CREATE,
                actor_user_id: None,
                target: None,
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            Ok(())
        })
        .map_err(|err| match err {
            RepoError::NotFound => anyhow::anyhow!("no owner yet: run create-owner first"),
            other => anyhow::Error::new(other),
        })
        .context("cannot create the invite")?;
    let url = public_url.join(&format!("/invite/{}", token.expose()));
    Ok(InviteLink {
        url: Redacted(url),
        expires_at,
    })
}
