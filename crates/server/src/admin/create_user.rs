//! `admin create-user`: creates a regular member account for tests (E6).
//!
//! The instance stays owner-only (E4): there is no invite redemption route,
//! so this is the only way to get a second, non-owner account, such as the
//! live host's mock account (about 500 of the owner's posts, migrated in by
//! the lead) or a lane's own throwaway test user. It is idempotent for the
//! same email, refuses the owner's email and refuses an email another
//! account already uses, and never prints the email back, like
//! `create-owner`.
//!
//! The new member gets role `member`, the default 5 GiB quota (plan §4.2)
//! and an empty library, exactly as `create-owner` sets up the owner. Every
//! other admin command and every per-user route already take a member by id
//! or email (nothing here is owner-gated): `admin login-link`, `admin
//! migrate-token`, `admin synth` and `admin bench` work the same for the
//! returned id, and `GET /me`'s `admin` capability stays `false` for it.

use std::io::Write;

use clap::Args;
use serde_json::json;
use shelfy_core::repo::RepoError;

use super::open_existing_control;
use super::owner::create_library;
use crate::config::DataDir;
use crate::control::audit::{self, Entry};
use crate::control::users::{self, DEFAULT_MEMBER_QUOTA_BYTES, NewUser, Role};
use crate::ids::{new_ulid, now_ms};

/// Arguments of `admin create-user`.
#[derive(Debug, Args)]
pub struct CreateUserArgs {
    /// Email of the new member. Never printed back.
    #[arg(long, value_name = "EMAIL")]
    pub email: String,

    /// Display name, if any. Never printed back.
    #[arg(long, value_name = "NAME")]
    pub display_name: Option<String>,
}

/// What `create-user` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateUserOutcome {
    /// A new member with this id.
    Created(String),
    /// A member with this email already existed; nothing changed.
    AlreadyExists(String),
}

impl CreateUserOutcome {
    /// The member's user id.
    #[must_use]
    pub fn user_id(&self) -> &str {
        match self {
            Self::Created(id) | Self::AlreadyExists(id) => id,
        }
    }
}

/// Runs `admin create-user`.
///
/// # Errors
///
/// See [`create_user`].
pub fn run(data: &DataDir, args: &CreateUserArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    match create_user(data, &args.email, args.display_name.as_deref())? {
        CreateUserOutcome::Created(id) => writeln!(out, "created user {id}")?,
        CreateUserOutcome::AlreadyExists(id) => {
            writeln!(out, "user {id} already exists; nothing changed")?;
        }
    }
    Ok(())
}

/// Creates a member account with `email` (and `display_name`, if given) and
/// its empty library.
///
/// # Errors
///
/// The email is malformed, it is the owner's, another account already uses
/// it, or a database cannot be opened.
pub fn create_user(
    data: &DataDir,
    email: &str,
    display_name: Option<&str>,
) -> anyhow::Result<CreateUserOutcome> {
    let email = users::normalize_email(email).map_err(describe)?;
    let control = open_existing_control(data)?;
    let now = now_ms();
    let outcome = control
        .write(|tx| -> Result<_, RepoError> {
            if let Some(existing) = users::find_by_email(tx, &email)? {
                return if existing.role == Role::Owner {
                    Err(RepoError::Conflict("owner_email"))
                } else {
                    // Idempotent: a rerun with the same email changes
                    // nothing, as create-owner does.
                    Ok(CreateUserOutcome::AlreadyExists(existing.id))
                };
            }
            let id = new_ulid();
            let user = NewUser {
                id: &id,
                email: &email,
                display_name,
                role: Role::Member,
                quota_bytes: DEFAULT_MEMBER_QUOTA_BYTES,
            };
            users::insert(tx, &user, now)?;
            let meta = json!({ "via": "admin" });
            let entry = Entry {
                action: audit::USER_CREATE,
                actor_user_id: None,
                target: Some(&id),
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            Ok(CreateUserOutcome::Created(id))
        })
        .map_err(describe)?;
    // Also when the member already existed: a rerun repairs a missing
    // library, as create-owner's does.
    create_library(data, outcome.user_id())?;
    Ok(outcome)
}

fn describe(err: RepoError) -> anyhow::Error {
    match err {
        RepoError::Conflict("owner_email") => {
            anyhow::anyhow!("this is the owner's email; create-user is for member accounts only")
        }
        RepoError::Conflict(_) => anyhow::anyhow!("an account already uses this email"),
        RepoError::Invalid { field, reason } => anyhow::anyhow!("invalid {field}: {reason}"),
        other => anyhow::Error::new(other).context("cannot create the user"),
    }
}
