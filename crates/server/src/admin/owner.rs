//! `admin create-owner`: creates the instance owner (E4: the only account).
//!
//! The owner gets role `owner`, an unlimited quota and an empty library. The
//! command prints no invite or link: the owner signs in with a link from
//! `admin login-link` or, when email is configured, from the sign-in page.
//! Running it again with the same email changes nothing, so deploy scripts
//! can call it unconditionally.

use std::io::Write;

use anyhow::Context as _;
use clap::Args;
use serde_json::json;
use shelfy_core::db::{UserDb, UserDbConfig};
use shelfy_core::repo::RepoError;

use super::open_control;
use crate::config::{DataDir, create_private_dir};
use crate::control::audit::{self, Entry};
use crate::control::users::{self, NewUser, Role};
use crate::ids::{new_ulid, now_ms};

/// Arguments of `admin create-owner`.
#[derive(Debug, Args)]
pub struct CreateOwnerArgs {
    /// Email of the owner (E4: kept in the osn secrets). Never printed back.
    #[arg(
        long,
        env = "SHELFY_OWNER_EMAIL",
        hide_env_values = true,
        value_name = "EMAIL"
    )]
    pub email: String,
}

/// What `create-owner` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateOwnerOutcome {
    /// A new owner with this id.
    Created(String),
    /// The owner with this email already existed; nothing changed.
    AlreadyExists(String),
}

impl CreateOwnerOutcome {
    /// The owner's user id.
    #[must_use]
    pub fn user_id(&self) -> &str {
        match self {
            Self::Created(id) | Self::AlreadyExists(id) => id,
        }
    }
}

/// Runs `admin create-owner`.
///
/// # Errors
///
/// See [`create_owner`].
pub fn run(data: &DataDir, args: &CreateOwnerArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    match create_owner(data, &args.email)? {
        CreateOwnerOutcome::Created(id) => writeln!(out, "created owner {id}")?,
        CreateOwnerOutcome::AlreadyExists(id) => {
            writeln!(out, "owner {id} already exists; nothing changed")?;
        }
    }
    Ok(())
}

/// Creates the owner with `email` and its empty library, creating the data
/// layout and the control database if needed.
///
/// # Errors
///
/// The email is malformed, another owner exists, a member already uses the
/// email, or a database cannot be opened.
pub fn create_owner(data: &DataDir, email: &str) -> anyhow::Result<CreateOwnerOutcome> {
    let email = users::normalize_email(email).map_err(describe)?;
    let control = open_control(data)?;
    let now = now_ms();
    let outcome = control
        .write(|tx| -> Result<_, RepoError> {
            if let Some(owner) = users::find_owner(tx)? {
                return if *owner.email.expose() == email {
                    Ok(CreateOwnerOutcome::AlreadyExists(owner.id))
                } else {
                    Err(RepoError::Conflict("owner"))
                };
            }
            if users::find_by_email(tx, &email)?.is_some() {
                return Err(RepoError::Conflict("email"));
            }
            let id = new_ulid();
            let owner = NewUser {
                id: &id,
                email: &email,
                role: Role::Owner,
                quota_bytes: 0,
            };
            users::insert(tx, &owner, now)?;
            let meta = json!({ "via": "cli" });
            let entry = Entry {
                action: audit::OWNER_CREATE,
                actor_user_id: None,
                target: Some(&id),
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            Ok(CreateOwnerOutcome::Created(id))
        })
        .map_err(describe)?;
    // Also when the owner already existed: a rerun repairs a missing library.
    create_library(data, outcome.user_id())?;
    Ok(outcome)
}

/// Creates (or opens and migrates) the user's empty library, so a fresh
/// install has the whole layout of §2.5.
fn create_library(data: &DataDir, user_id: &str) -> anyhow::Result<()> {
    let path = data.library_db(user_id);
    if let Some(dir) = path.parent() {
        create_private_dir(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    UserDb::open(&path, &UserDbConfig::default())
        .with_context(|| format!("cannot create the library {}", path.display()))?;
    Ok(())
}

fn describe(err: RepoError) -> anyhow::Error {
    match err {
        RepoError::Conflict("owner") => anyhow::anyhow!(
            "this instance already has an owner with another email; it has a single owner (E4)"
        ),
        RepoError::Conflict(_) => anyhow::anyhow!("an account already uses this email"),
        RepoError::Invalid { field, reason } => anyhow::anyhow!("invalid {field}: {reason}"),
        other => anyhow::Error::new(other).context("cannot create the owner"),
    }
}
