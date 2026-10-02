//! Sign-in and re-authentication links (plan §2.11, E4).
//!
//! - **Minted** by [`mint`] for an existing, active account only: there is no
//!   sign-up. Either by email (`POST /api/v1/auth/magic-links`, through
//!   [`send_in_background`]; `POST /api/v1/auth/reauth/start`, through
//!   [`send_reauth_in_background`]) or by the operator (`admin login-link`,
//!   `--purpose login|reauth`). Every minted link is audit-logged.
//! - **Single use and short-lived:** 15 minutes; [`redeem`] uses the link and
//!   creates the session in one transaction, after a read-only check, so an
//!   unknown token never takes the database's writer. A re-authentication
//!   link is used by [`super::reauth::with_link`] instead, and only by the
//!   signed-in account it was minted for.
//! - **No account enumeration:** the email request answers 202 at once and
//!   does the lookup, the minting and the sending in the background, so the
//!   response is the same, in content and timing, whether or not the address
//!   has an account. The rate limits count every address alike, and the email
//!   goes to the account's stored address, never to the string typed in.
//! - **URL:** `<public url>/login/magic#<token>`, the SPA's sign-in page, or
//!   `<public url>/login/reauth#<token>` for a re-authentication link. The
//!   token sits in the fragment, which browsers never send to a server, so it
//!   reaches no proxy or server log. The page posts it after a click: to
//!   `POST /api/v1/auth/magic-links/redeem`, or, signed in, to
//!   `POST /api/v1/auth/reauth/finish`. Nothing redeems on `GET`, so link
//!   scanners and prefetchers cannot use a link up.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::json;
use shelfy_core::repo::RepoError;
use tracing::Instrument as _;

use super::millis;
use super::session::{SignInMethod, SignedIn, create_session};
use crate::config::PublicUrl;
use crate::control::audit::{self, Entry};
use crate::control::magic_links::{self, NewMagicLink, Purpose};
use crate::control::users::{self, Status};
use crate::error::ApiError;
use crate::ids::now_ms;
use crate::mail::Email;
use crate::state::{AppState, blocking};
use crate::telemetry::redact::Redacted;
use crate::tokens::{SecretToken, hash_token, is_token_shaped};

/// The SPA page a sign-in link opens; the token follows in the fragment.
pub const LINK_PAGE: &str = "/login/magic";

/// The SPA page a re-authentication link opens; the token follows in the
/// fragment.
pub const REAUTH_LINK_PAGE: &str = "/login/reauth";

/// The URL of the sign-in link carrying `token`:
/// `<public url>/login/magic#<token>`.
#[must_use]
pub fn link_url(public_url: &PublicUrl, token: &SecretToken) -> Redacted<String> {
    Redacted(format!("{}#{}", public_url.join(LINK_PAGE), token.expose()))
}

/// The URL of the re-authentication link carrying `token`:
/// `<public url>/login/reauth#<token>`.
#[must_use]
pub fn reauth_link_url(public_url: &PublicUrl, token: &SecretToken) -> Redacted<String> {
    Redacted(format!(
        "{}#{}",
        public_url.join(REAUTH_LINK_PAGE),
        token.expose()
    ))
}

/// Who asked for a link, for the audit log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// `POST /api/v1/auth/magic-links`.
    Email,
    /// `shelfy-server admin login-link`.
    Cli,
}

impl Via {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Cli => "cli",
        }
    }
}

/// A minted link: the token (its only copy) and its expiry (unix ms).
#[derive(Debug)]
pub struct Minted {
    /// The token.
    pub token: SecretToken,
    /// Expiry, unix ms.
    pub expires_at: i64,
}

/// Mints a link for `user_id` inside the caller's write transaction, after
/// pruning expired links, and writes `magic_link.create` to the audit log.
///
/// # Errors
///
/// A query failed.
pub fn mint(
    tx: &Connection,
    user_id: &str,
    purpose: Purpose,
    ttl: Duration,
    via: Via,
    now: i64,
) -> Result<Minted, RepoError> {
    magic_links::prune(tx, now)?;
    let token = SecretToken::generate();
    let token_hash = token.hash();
    let expires_at = now.saturating_add(millis(ttl));
    let link = NewMagicLink {
        token_hash: &token_hash,
        user_id,
        purpose,
        expires_at,
    };
    magic_links::insert(tx, &link)?;
    let meta = json!({ "via": via.as_str(), "purpose": purpose.as_str() });
    let entry = Entry {
        action: audit::MAGIC_LINK_CREATE,
        actor_user_id: None,
        target: Some(user_id),
        meta: Some(&meta),
    };
    audit::record(tx, &entry, now)?;
    Ok(Minted { token, expires_at })
}

/// Who an emailed link is for.
enum Recipient {
    /// A sign-in link for the account with this address (normalized), if any.
    Address(String),
    /// A re-authentication link for this account.
    Account(String),
}

/// Emails a sign-in link to the active account with `email` (normalized),
/// if there is one, in the background; returns at once. Does nothing when
/// email is off, and drops the request (with a warning) when
/// [`super::AuthConfig::mail_concurrency`] emails are already in flight. The
/// task stops at shutdown.
pub fn send_in_background(state: &AppState, email: String) {
    spawn_email(state, Recipient::Address(email));
}

/// Emails a re-authentication link to the active account `user_id`, in the
/// background; returns at once. Like [`send_in_background`] otherwise.
pub fn send_reauth_in_background(state: &AppState, user_id: String) {
    spawn_email(state, Recipient::Account(user_id));
}

fn spawn_email(state: &AppState, recipient: Recipient) {
    if !state.mailer().is_enabled() {
        return;
    }
    let Ok(slot) = Arc::clone(state.auth().mail_slots()).try_acquire_owned() else {
        tracing::warn!("sign-in email dropped: too many in flight");
        return;
    };
    let state = state.clone();
    // Stays in the request span, so its log lines carry the request id.
    let span = tracing::Span::current();
    tokio::spawn(
        async move {
            let _slot = slot;
            let shutdown = state.shutdown_token().clone();
            tokio::select! {
                () = shutdown.cancelled() => {}
                () = send_link(&state, recipient) => {}
            }
        }
        .instrument(span),
    );
}

async fn send_link(state: &AppState, recipient: Recipient) {
    let control = Arc::clone(state.control());
    let ttl = state.auth().config().magic_link_ttl;
    let now = now_ms();
    let purpose = match recipient {
        Recipient::Address(_) => Purpose::Login,
        Recipient::Account(_) => Purpose::Reauth,
    };
    let minted = blocking(move || {
        control.write(|tx| {
            let user = match &recipient {
                Recipient::Address(email) => users::find_by_email(tx, email)?,
                Recipient::Account(user_id) => users::get(tx, user_id)?,
            };
            let Some(user) = user.filter(|user| user.status == Status::Active) else {
                return Ok(None);
            };
            let minted = mint(tx, &user.id, purpose, ttl, Via::Email, now)?;
            // The account's own address: the lookup ignores case.
            Ok::<_, RepoError>(Some((minted, user.email.into_inner())))
        })
    })
    .await;
    let (token, email) = match minted {
        Ok(Some((minted, email))) => (minted.token, email),
        Ok(None) => {
            tracing::info!("sign-in link requested for no active account; nothing sent");
            return;
        }
        Err(err) => {
            tracing::warn!(error = %err, "sign-in link not minted");
            return;
        }
    };
    let public_url = &state.config().public_url;
    let message = if purpose == Purpose::Reauth {
        reauth_email(email, reauth_link_url(public_url, &token).expose(), ttl)
    } else {
        sign_in_email(email, link_url(public_url, &token).expose(), ttl)
    };
    let mailer = state.mailer();
    match mailer.send(message).await {
        Ok(()) => tracing::info!(transport = mailer.kind().as_str(), "sign-in email sent"),
        Err(err) => tracing::warn!(
            error = %err,
            transport = mailer.kind().as_str(),
            "sign-in email not sent"
        ),
    }
}

/// The sign-in email. English only: the owner's instance (E4).
fn sign_in_email(to: String, url: &str, ttl: Duration) -> Email {
    let minutes = ttl.as_secs() / 60;
    Email {
        to: Redacted(to),
        subject: "Your Shelfy sign-in link".to_owned(),
        text: Redacted(format!(
            "Open this link to sign in to Shelfy:\n\n{url}\n\nIt works once and expires in \
             {minutes} minutes. If you did not ask for it, ignore this email: nobody can sign \
             in without the link.\n"
        )),
    }
}

/// The re-authentication email: a signed-in user confirms who they are
/// before a sensitive action. English only, like [`sign_in_email`].
fn reauth_email(to: String, url: &str, ttl: Duration) -> Email {
    let minutes = ttl.as_secs() / 60;
    Email {
        to: Redacted(to),
        subject: "Confirm it is you on Shelfy".to_owned(),
        text: Redacted(format!(
            "Open this link in the browser where you are signed in to Shelfy, to confirm it is \
             you:\n\n{url}\n\nIt works once and expires in {minutes} minutes. If you did not \
             ask for it, ignore this email.\n"
        )),
    }
}

/// Redeems the sign-in link `token`: in one transaction, uses the link and
/// creates a session, replacing `replaced` (the session cookie the browser
/// sent, if any). `None` when the link is unknown, used, expired or not a
/// sign-in link, or its account is not active; that is found on a reader,
/// so only a usable link takes the writer.
///
/// # Errors
///
/// The control database failed.
pub async fn redeem(
    state: &AppState,
    token: &str,
    replaced: Option<&str>,
    user_agent: Option<String>,
) -> Result<Option<SignedIn>, ApiError> {
    if !is_token_shaped(token) {
        return Ok(None);
    }
    let link_hash = hash_token(token);
    let replaced = replaced.filter(|t| is_token_shaped(t)).map(hash_token);
    let config = state.auth().config().clone();
    let now = now_ms();
    let reader = Arc::clone(state.control());
    let usable = blocking(move || {
        reader.read(|conn| magic_links::is_redeemable(conn, &link_hash, Purpose::Login, now))
    })
    .await?;
    if !usable {
        return Ok(None);
    }
    let control = Arc::clone(state.control());
    let redeemed = blocking(move || {
        control.write(|tx| {
            let Some(user_id) = magic_links::consume(tx, &link_hash, Purpose::Login, now)? else {
                return Ok(None);
            };
            let token = create_session(
                tx,
                &config,
                &user_id,
                replaced.as_ref(),
                user_agent.as_deref(),
                SignInMethod::MagicLink,
                now,
            )?;
            Ok::<_, RepoError>(Some(SignedIn { user_id, token }))
        })
    })
    .await?;
    if let Some(old) = &replaced {
        state.auth().forget_session(old);
    }
    if let Some(redeemed) = &redeemed {
        state.auth().forget_miss(&redeemed.token.hash());
    }
    Ok(redeemed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_point_at_the_public_url() {
        let public = PublicUrl::parse("https://refs.example.test").unwrap();
        let token = SecretToken::generate();
        let url = link_url(&public, &token);
        assert_eq!(
            url.expose(),
            &format!("https://refs.example.test/login/magic#{}", token.expose())
        );
        assert!(!format!("{url:?}").contains(token.expose()));
    }

    #[test]
    fn reauth_links_open_their_own_page() {
        let public = PublicUrl::parse("http://localhost:18193").unwrap();
        let token = SecretToken::generate();
        let url = reauth_link_url(&public, &token);
        assert_eq!(
            url.expose(),
            &format!("http://localhost:18193/login/reauth#{}", token.expose())
        );
        assert!(!format!("{url:?}").contains(token.expose()));
        let email = reauth_email(
            "owner@example.test".into(),
            "http://localhost:18193/login/reauth#abc",
            Duration::from_secs(900),
        );
        assert_eq!(email.subject, "Confirm it is you on Shelfy");
        let text = email.text.expose();
        assert!(text.contains("\nhttp://localhost:18193/login/reauth#abc\n"));
        assert!(text.contains("expires in 15 minutes"));
    }

    #[test]
    fn the_email_carries_the_link_and_its_lifetime() {
        let email = sign_in_email(
            "owner@example.test".into(),
            "https://refs.example.test/login/magic#abc",
            Duration::from_secs(900),
        );
        assert_eq!(email.to.expose(), "owner@example.test");
        let text = email.text.expose();
        assert!(text.contains("\nhttps://refs.example.test/login/magic#abc\n"));
        assert!(text.contains("expires in 15 minutes"));
    }
}
