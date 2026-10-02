//! Passkeys (plan §2.11, D8; owner-only under E4) with `webauthn-rs`:
//! registration of a discoverable credential, username-less sign-in, and
//! re-authentication.
//!
//! **Relying party.** The RP ID is the host of `SHELFY_PUBLIC_URL`
//! (`refs.niccolofanton.dev` in production, `localhost` locally), and the one
//! accepted origin is the public URL itself, port included. A public URL that
//! cannot carry passkeys turns them off, with a warning at start: an IP
//! address (an RP ID is a domain name), or plain http to a host other than
//! localhost (browsers offer WebAuthn in secure contexts only). Then
//! `GET /auth/methods` says `passkeys: false` and the ceremonies answer 404.
//!
//! **Options sent to the browser.**
//!
//! | Ceremony | Options |
//! |---|---|
//! | registration | `residentKey: required` (discoverable, for username-less sign-in), `userVerification: required`, `attestation: none`, ES256 or RS256, `excludeCredentials` = the account's passkeys, `credProps`, `credProtect` (not enforced) |
//! | sign-in | no `allowCredentials`: the browser offers the passkeys it holds for the RP ID; `userVerification: required` |
//! | re-authentication | `allowCredentials` = the account's passkeys; `userVerification: required` |
//!
//! Every ceremony's `timeout` is [`super::AuthConfig::passkey_ceremony_ttl`]
//! (5 minutes). The user handle (`user.id`) is [`user_handle`]: opaque, and
//! no personal data. `user.name` and `user.displayName` are the account's
//! email, which the authenticator shows in its account picker.
//!
//! **Ceremonies.** A ceremony starts with options and a `ceremonyId`, and
//! ends with the browser's answer and that id. Its state lives in memory for
//! 5 minutes (a moka TTL cache; §2.11: no state serialization), and is used
//! once, whatever the outcome. A registration or re-authentication ceremony
//! finishes only in the session that started it; otherwise, and when the
//! ceremony is unknown, used or expired, 400 `challenge_expired`. Pending
//! ceremonies do not survive a restart: the client starts again.
//!
//! **Verification.** webauthn-rs checks the challenge, the origin, the RP ID
//! hash, user presence and verification, the algorithm, the signature and
//! the signature counter. The server adds:
//!
//! - the credential must be registered to an active account, and the user
//!   handle must name that account (sign-in needs one; re-authentication
//!   checks it when present, and the passkey must be the signed-in user's);
//! - the stored state the answer is checked against is read in the
//!   transaction that records the use, so two sign-ins cannot pass with the
//!   same counter value;
//! - a counter that did not grow refuses the sign-in (a cloned authenticator)
//!   and writes `passkey.clone_suspected` to the audit log. Synced passkeys
//!   always send 0 and skip the check.
//!
//! A refusal is 400 `passkey_invalid`, its detail a reason code.
//!
//! **Logs.** Credential ids, public keys, challenges, ceremony ids and
//! emails are never logged; a refusal logs its reason code. webauthn-rs logs
//! its state at debug and trace level, so [`crate::telemetry`] drops its
//! events below WARN whatever `RUST_LOG` says.

use std::sync::Arc;
use std::time::{Duration, Instant};

use moka::sync::Cache;
use rusqlite::Connection;
use serde_json::json;
use sha2::{Digest, Sha256};
use shelfy_core::db::DbError;
use shelfy_core::repo::RepoError;
use url::Url;
use webauthn_rs::prelude::{
    CredentialID, DiscoverableAuthentication, DiscoverableKey, Passkey, PasskeyRegistration,
    PublicKeyCredential, RegisterPublicKeyCredential, Uuid, Webauthn, WebauthnBuilder,
    WebauthnError,
};
use webauthn_rs_proto::{
    AllowCredentials, PublicKeyCredentialCreationOptions, PublicKeyCredentialRequestOptions,
    ResidentKeyRequirement,
};

use super::cookie;
use super::reauth;
use super::session::{SessionUser, SignInMethod, SignedIn, create_session};
use crate::config::PublicUrl;
use crate::control::audit::{self, Entry};
use crate::control::passkeys::{self as rows, LABEL_MAX_CHARS, NewPasskey, PasskeyRow};
use crate::control::sessions;
use crate::control::users::{self, Status};
use crate::error::{ApiError, ErrorCode};
use crate::ids::now_ms;
use crate::state::{AppState, blocking};
use crate::tokens::{SecretToken, TokenHash, hash_token, is_token_shaped};

/// The relying party's name, shown by some authenticators.
pub const RP_NAME: &str = "Shelfy";

/// Most ceremonies kept at once; past it, the cache evicts.
const MAX_CEREMONIES: u64 = 10_000;

/// Domain separation of [`user_handle`].
const USER_HANDLE_PREFIX: &[u8] = b"shelfy.passkey.user-handle\0";

/// The relying party and the ceremonies in flight.
pub struct Passkeys {
    webauthn: Option<Arc<Webauthn>>,
    ceremonies: Cache<TokenHash, Arc<Pending>>,
    ttl: Duration,
}

/// A ceremony and its deadline. The cache's time to live only bounds its
/// memory: its `remove` hands back an expired entry that was not evicted
/// yet, so [`Passkeys::take`] checks the deadline itself.
struct Pending {
    expires_at: Instant,
    ceremony: Ceremony,
}

/// The server's state of a ceremony.
enum Ceremony {
    /// A passkey being added to the account of a session.
    Registration {
        user_id: String,
        session: TokenHash,
        state: PasskeyRegistration,
    },
    /// A sign-in.
    SignIn(DiscoverableAuthentication),
    /// A re-authentication of a session.
    Reauth {
        user_id: String,
        session: TokenHash,
        state: DiscoverableAuthentication,
    },
}

impl Passkeys {
    /// The relying party of `public_url` (see the module docs), whose
    /// ceremonies last `ceremony_ttl`. Passkeys are off, with a warning, when
    /// the public URL cannot carry them.
    #[must_use]
    pub fn new(public_url: &PublicUrl, ceremony_ttl: Duration) -> Self {
        let webauthn = match relying_party(public_url, ceremony_ttl) {
            Ok(webauthn) => Some(Arc::new(webauthn)),
            Err(reason) => {
                tracing::warn!(public_url = %public_url, reason, "passkeys are off");
                None
            }
        };
        let ttl = ceremony_ttl.max(Duration::from_millis(1));
        let ceremonies = Cache::builder()
            .max_capacity(MAX_CEREMONIES)
            .time_to_live(ttl)
            .build();
        Self {
            webauthn,
            ceremonies,
            ttl,
        }
    }

    /// Whether passkeys work on this server (`GET /auth/methods`).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.webauthn.is_some()
    }

    /// The relying party, or 404 when passkeys are off.
    fn webauthn(&self) -> Result<Arc<Webauthn>, ApiError> {
        self.webauthn
            .clone()
            .ok_or_else(|| ApiError::not_found().with_detail("passkeys are off on this server"))
    }

    /// Keeps `ceremony` for the ceremony time; returns its id.
    fn begin(&self, ceremony: Ceremony) -> String {
        let id = SecretToken::generate();
        let pending = Pending {
            expires_at: Instant::now() + self.ttl,
            ceremony,
        };
        self.ceremonies.insert(id.hash(), Arc::new(pending));
        id.expose().to_owned()
    }

    /// Takes the ceremony `id` out, if it has not expired: a ceremony is used
    /// once.
    fn take(&self, id: &str) -> Option<Arc<Pending>> {
        if !is_token_shaped(id) {
            return None;
        }
        let pending = self.ceremonies.remove(&hash_token(id))?;
        (Instant::now() < pending.expires_at).then_some(pending)
    }
}

/// The relying party of `public_url`, or why there is none.
fn relying_party(public_url: &PublicUrl, ttl: Duration) -> Result<Webauthn, &'static str> {
    if !cookie::secure_cookies_work(public_url.as_str()) {
        return Err("browsers offer passkeys on https or localhost only");
    }
    let origin = Url::parse(public_url.as_str()).map_err(|_| "the public URL does not parse")?;
    let Some(rp_id) = origin.domain() else {
        return Err("an IP address cannot be a passkey RP ID");
    };
    WebauthnBuilder::new(rp_id, &origin)
        .and_then(|builder| builder.rp_name(RP_NAME).timeout(ttl).build())
        .map_err(|_| "webauthn-rs refused the relying party")
}

/// The WebAuthn user handle (`user.id`) of account `user_id`: the first 16
/// bytes of a SHA-256 over a fixed prefix and the account id. Stable and
/// opaque, with no personal data (WebAuthn §14.6.1). The server never looks
/// an account up by it: it checks that it names the credential's account.
#[must_use]
pub fn user_handle(user_id: &str) -> Uuid {
    let digest = Sha256::new()
        .chain_update(USER_HANDLE_PREFIX)
        .chain_update(user_id.as_bytes())
        .finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

/// A label as stored: trimmed, at most [`LABEL_MAX_CHARS`] characters,
/// without control characters; blank is none.
///
/// # Errors
///
/// 422 `validation_failed` on field `label`.
pub fn normalize_label(raw: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(label) = raw.map(str::trim).filter(|label| !label.is_empty()) else {
        return Ok(None);
    };
    if label.chars().count() > LABEL_MAX_CHARS {
        return Err(ApiError::invalid_field(
            "label",
            format!("must be at most {LABEL_MAX_CHARS} characters"),
        ));
    }
    if label.chars().any(char::is_control) {
        return Err(ApiError::invalid_field(
            "label",
            "must not contain control characters",
        ));
    }
    Ok(Some(label.to_owned()))
}

/// The reason code of a refusal by webauthn-rs, for the logs and the
/// problem's detail. Never the error's text, which can quote the input.
fn reason_of(err: &WebauthnError) -> &'static str {
    match err {
        WebauthnError::MismatchedChallenge => "challenge_mismatch",
        WebauthnError::InvalidClientDataType => "wrong_ceremony",
        WebauthnError::InvalidRPOrigin | WebauthnError::CredentialCrossOrigin => "origin_mismatch",
        WebauthnError::InvalidRPIDHash => "rp_id_mismatch",
        WebauthnError::UserNotPresent => "user_not_present",
        WebauthnError::UserNotVerified => "user_not_verified",
        WebauthnError::AuthenticationFailure => "bad_signature",
        WebauthnError::CredentialNotFound => "unknown_credential",
        WebauthnError::CredentialPossibleCompromise => "counter_not_increased",
        WebauthnError::CredentialBackupEligibilityInconsistent
        | WebauthnError::CredentialMayNotBeHardwareBound => "backup_state_inconsistent",
        WebauthnError::InvalidUserUniqueId => "bad_user_handle",
        WebauthnError::CredentialAlteredAlgFromRequest
        | WebauthnError::CredentialExcludedFromRequest => "credential_not_allowed",
        WebauthnError::COSEKeyInvalidAlgorithm | WebauthnError::CredentialInsecureCryptography => {
            "unsupported_key"
        }
        _ => "malformed",
    }
}

/// 400 `challenge_expired`.
fn expired() -> ApiError {
    tracing::info!("passkey ceremony unknown, finished or expired");
    ApiError::new(ErrorCode::ChallengeExpired)
}

/// 400 `passkey_invalid` for `reason`.
fn refused(reason: &'static str) -> ApiError {
    tracing::info!(reason, "passkey refused");
    ApiError::new(ErrorCode::PasskeyInvalid).with_detail(reason)
}

/// A 500 for webauthn-rs failing to build options, which only a bug causes.
fn options_failed(err: &WebauthnError) -> ApiError {
    ApiError::internal(anyhow::anyhow!(
        "passkey options not built: {}",
        reason_of(err)
    ))
}

/// The failure of a write transaction that checks a passkey.
enum TxError {
    /// A query failed.
    Repo(RepoError),
    /// A stored passkey does not parse.
    Corrupt,
    /// The session ended while the request ran.
    SessionGone,
}

impl From<DbError> for TxError {
    fn from(err: DbError) -> Self {
        Self::Repo(err.into())
    }
}

impl From<RepoError> for TxError {
    fn from(err: RepoError) -> Self {
        Self::Repo(err)
    }
}

impl From<TxError> for ApiError {
    fn from(err: TxError) -> Self {
        match err {
            TxError::Repo(err) => err.into(),
            TxError::Corrupt => {
                ApiError::internal(anyhow::anyhow!("a stored passkey cannot be read"))
            }
            TxError::SessionGone => ApiError::new(ErrorCode::Unauthorized),
        }
    }
}

/// Writes an audit row about passkey `id` of `user_id`.
fn audit_passkey(
    tx: &Connection,
    action: &str,
    actor: Option<&str>,
    user_id: &str,
    id: i64,
    now: i64,
) -> Result<(), RepoError> {
    let meta = json!({ "id": id });
    let entry = Entry {
        action,
        actor_user_id: actor,
        target: Some(user_id),
        meta: Some(&meta),
    };
    audit::record(tx, &entry, now)?;
    Ok(())
}

/// The passkeys of `user_id`, oldest first.
///
/// # Errors
///
/// The control database failed.
pub async fn list(state: &AppState, user_id: &str) -> Result<Vec<PasskeyRow>, ApiError> {
    let control = Arc::clone(state.control());
    let user_id = user_id.to_owned();
    blocking(move || control.read(|conn| rows::list(conn, &user_id))).await
}

/// Starts adding a passkey to the account of `user`'s session: the options
/// for `navigator.credentials.create()` and the ceremony id.
///
/// # Errors
///
/// 404 when passkeys are off; 401 when the account is gone.
pub async fn start_registration(
    state: &AppState,
    user: &SessionUser,
) -> Result<(String, PublicKeyCredentialCreationOptions), ApiError> {
    let passkeys = state.auth().passkeys();
    let webauthn = passkeys.webauthn()?;
    let control = Arc::clone(state.control());
    let user_id = user.id().to_owned();
    let (account, existing) = blocking(move || {
        control.read(|conn| {
            Ok::<_, RepoError>((users::get(conn, &user_id)?, rows::list(conn, &user_id)?))
        })
    })
    .await?;
    let Some(account) = account else {
        return Err(ApiError::new(ErrorCode::Unauthorized));
    };
    let exclude: Vec<CredentialID> = existing.into_iter().map(|row| row.cred_id.into()).collect();
    let name = account.email.expose();
    let (mut options, registration) = webauthn
        .start_passkey_registration(
            user_handle(&account.id),
            name,
            name,
            (!exclude.is_empty()).then_some(exclude),
        )
        .map_err(|err| options_failed(&err))?;
    // A discoverable credential, which username-less sign-in needs: the
    // passkey API asks for none. Only the browser reads this option.
    if let Some(selection) = options.public_key.authenticator_selection.as_mut() {
        selection.resident_key = Some(ResidentKeyRequirement::Required);
        selection.require_resident_key = true;
    }
    let id = passkeys.begin(Ceremony::Registration {
        user_id: account.id,
        session: *user.session().id_hash(),
        state: registration,
    });
    Ok((id, options.public_key))
}

/// Finishes adding a passkey: checks the browser's `credential` against the
/// ceremony `ceremony_id` of `user`'s session, then stores the passkey with
/// `label` (already normalized) and writes `passkey.create`.
///
/// # Errors
///
/// 400 `challenge_expired` or `passkey_invalid`; 409 `conflict` when the
/// credential is registered already; 404 when passkeys are off.
pub async fn finish_registration(
    state: &AppState,
    user: &SessionUser,
    ceremony_id: &str,
    credential: &RegisterPublicKeyCredential,
    label: Option<String>,
) -> Result<PasskeyRow, ApiError> {
    let passkeys = state.auth().passkeys();
    let webauthn = passkeys.webauthn()?;
    let Some(ceremony) = passkeys.take(ceremony_id) else {
        return Err(expired());
    };
    let Ceremony::Registration {
        user_id,
        session,
        state: registration,
    } = &ceremony.ceremony
    else {
        return Err(expired());
    };
    if user_id != user.id() || session != user.session().id_hash() {
        return Err(expired());
    }
    let passkey = webauthn
        .finish_passkey_registration(credential, registration)
        .map_err(|err| refused(reason_of(&err)))?;
    let passkey_json = serde_json::to_string(&passkey).map_err(|_| TxError::Corrupt)?;
    let cred_id = passkey.cred_id().to_vec();
    let control = Arc::clone(state.control());
    let user_id = user_id.clone();
    let now = now_ms();
    blocking(move || {
        control.write(|tx| {
            let new = NewPasskey {
                user_id: &user_id,
                cred_id: &cred_id,
                passkey_json: &passkey_json,
                label: label.as_deref(),
            };
            let id = rows::insert(tx, &new, now)?;
            audit_passkey(tx, audit::PASSKEY_CREATE, Some(&user_id), &user_id, id, now)?;
            Ok::<_, RepoError>(PasskeyRow {
                id,
                user_id,
                cred_id,
                passkey_json,
                label,
                created_at: now,
                last_used_at: None,
            })
        })
    })
    .await
}

/// Starts a username-less sign-in: the options for
/// `navigator.credentials.get()` and the ceremony id.
///
/// # Errors
///
/// 404 when passkeys are off.
pub fn start_sign_in(
    state: &AppState,
) -> Result<(String, PublicKeyCredentialRequestOptions), ApiError> {
    let passkeys = state.auth().passkeys();
    let webauthn = passkeys.webauthn()?;
    let (options, authentication) = webauthn
        .start_discoverable_authentication()
        .map_err(|err| options_failed(&err))?;
    // `options.mediation` (conditional) is dropped: the client picks the
    // mediation, a button's modal prompt or the autofill.
    let id = passkeys.begin(Ceremony::SignIn(authentication));
    Ok((id, options.public_key))
}

/// Finishes a sign-in: checks `credential` against the ceremony
/// `ceremony_id` and the passkey it names, records the use, and creates a
/// session replacing `replaced` (the session cookie the browser sent), with
/// `session.create` (`method: passkey`) in the audit log.
///
/// # Errors
///
/// 400 `challenge_expired` or `passkey_invalid`; 404 when passkeys are off;
/// the control database failed.
pub async fn finish_sign_in(
    state: &AppState,
    ceremony_id: &str,
    credential: PublicKeyCredential,
    replaced: Option<&str>,
    user_agent: Option<String>,
) -> Result<SignedIn, ApiError> {
    let passkeys = state.auth().passkeys();
    let webauthn = passkeys.webauthn()?;
    let Some(ceremony) = passkeys.take(ceremony_id) else {
        return Err(expired());
    };
    let Ceremony::SignIn(authentication) = &ceremony.ceremony else {
        return Err(expired());
    };
    let authentication = authentication.clone();
    precheck(state, &credential, None).await?;
    let replaced = replaced.filter(|t| is_token_shaped(t)).map(hash_token);
    let config = state.auth().config().clone();
    let control = Arc::clone(state.control());
    let now = now_ms();
    let outcome = blocking(move || {
        control.write(|tx| {
            let user_id = match verify_use(tx, &webauthn, &credential, authentication, None, now)? {
                Ok(user_id) => user_id,
                Err(reason) => return Ok(Err(reason)),
            };
            let token = create_session(
                tx,
                &config,
                &user_id,
                replaced.as_ref(),
                user_agent.as_deref(),
                SignInMethod::Passkey,
                now,
            )?;
            Ok::<_, TxError>(Ok(SignedIn { user_id, token }))
        })
    })
    .await?;
    let signed_in = outcome.map_err(refused)?;
    if let Some(old) = &replaced {
        state.auth().forget_session(old);
    }
    state.auth().forget_miss(&signed_in.token.hash());
    Ok(signed_in)
}

/// Starts re-authenticating `user`'s session with a passkey: the options
/// for `navigator.credentials.get()`, which name the account's passkeys, and
/// the ceremony id.
///
/// # Errors
///
/// 404 when passkeys are off or the account has none.
pub async fn start_reauth(
    state: &AppState,
    user: &SessionUser,
) -> Result<(String, PublicKeyCredentialRequestOptions), ApiError> {
    let passkeys = state.auth().passkeys();
    let webauthn = passkeys.webauthn()?;
    let existing = list(state, user.id()).await?;
    if existing.is_empty() {
        return Err(ApiError::not_found().with_detail("the account has no passkey"));
    }
    let (mut options, authentication) = webauthn
        .start_discoverable_authentication()
        .map_err(|err| options_failed(&err))?;
    // The finish checks the answer against the passkeys read then, not
    // against copies taken now; the list only guides the browser.
    options.public_key.allow_credentials = existing
        .into_iter()
        .map(|row| AllowCredentials {
            type_: "public-key".to_owned(),
            id: row.cred_id.into(),
            transports: None,
        })
        .collect();
    let id = passkeys.begin(Ceremony::Reauth {
        user_id: user.id().to_owned(),
        session: *user.session().id_hash(),
        state: authentication,
    });
    Ok((id, options.public_key))
}

/// Finishes re-authenticating `user`'s session: checks `credential` against
/// the ceremony `ceremony_id` of this session and one of the account's
/// passkeys, records the use, and moves the session's `reauth_at` to now
/// ([`reauth::mark`]).
///
/// # Errors
///
/// 400 `challenge_expired` or `passkey_invalid`; 401 when the session ended
/// meanwhile; 404 when passkeys are off.
pub async fn finish_reauth(
    state: &AppState,
    user: &SessionUser,
    ceremony_id: &str,
    credential: PublicKeyCredential,
) -> Result<(), ApiError> {
    let passkeys = state.auth().passkeys();
    let webauthn = passkeys.webauthn()?;
    let Some(ceremony) = passkeys.take(ceremony_id) else {
        return Err(expired());
    };
    let Ceremony::Reauth {
        user_id,
        session,
        state: authentication,
    } = &ceremony.ceremony
    else {
        return Err(expired());
    };
    if user_id != user.id() || session != user.session().id_hash() {
        return Err(expired());
    }
    let (user_id, session, authentication) = (user_id.clone(), *session, authentication.clone());
    precheck(state, &credential, Some(user_id.as_str())).await?;
    let control = Arc::clone(state.control());
    let now = now_ms();
    let outcome = blocking(move || {
        control.write(|tx| {
            if sessions::find(tx, &session)?.is_none() {
                return Err(TxError::SessionGone);
            }
            if let Err(reason) = verify_use(
                tx,
                &webauthn,
                &credential,
                authentication,
                Some(&user_id),
                now,
            )? {
                return Ok(Err(reason));
            }
            reauth::mark(tx, &session, &user_id, SignInMethod::Passkey, now)?;
            Ok(Ok(()))
        })
    })
    .await;
    state.auth().forget_session(&session);
    outcome?.map_err(refused)
}

/// Removes passkey `id` of `user_id` and writes `passkey.delete`.
///
/// # Errors
///
/// 404 when the account has no such passkey; the control database failed.
pub async fn remove(state: &AppState, user_id: &str, id: i64) -> Result<(), ApiError> {
    let control = Arc::clone(state.control());
    let user_id = user_id.to_owned();
    let now = now_ms();
    blocking(move || {
        control.write(|tx| {
            if !rows::delete(tx, &user_id, id)? {
                return Err(RepoError::NotFound);
            }
            audit_passkey(tx, audit::PASSKEY_DELETE, Some(&user_id), &user_id, id, now)
        })
    })
    .await
}

/// Refuses, on a reader, an answer naming no passkey (of `owner`, when
/// given) before the writer is taken.
async fn precheck(
    state: &AppState,
    credential: &PublicKeyCredential,
    owner: Option<&str>,
) -> Result<(), ApiError> {
    let control = Arc::clone(state.control());
    let cred_id = credential.get_credential_id().to_vec();
    let found =
        blocking(move || control.read(|conn| rows::find_by_credential(conn, &cred_id))).await?;
    match found {
        Some((row, _)) if owner.is_none_or(|owner| owner == row.user_id) => Ok(()),
        _ => Err(refused("unknown_credential")),
    }
}

/// Checks `credential` against the passkey it names, read in this write
/// transaction, and records the use: the passkey's new state (counter,
/// backup flags) and its last use. `owner` is the account the passkey must
/// belong to (re-authentication); without it (sign-in) the user handle is
/// required. Returns the passkey's account, or the reason of a refusal; a
/// counter that did not grow also writes `passkey.clone_suspected`.
fn verify_use(
    tx: &Connection,
    webauthn: &Webauthn,
    credential: &PublicKeyCredential,
    authentication: DiscoverableAuthentication,
    owner: Option<&str>,
    now: i64,
) -> Result<Result<String, &'static str>, TxError> {
    let Some((row, status)) = rows::find_by_credential(tx, credential.get_credential_id())? else {
        return Ok(Err("unknown_credential"));
    };
    if owner.is_some_and(|owner| owner != row.user_id) {
        return Ok(Err("unknown_credential"));
    }
    if status != Status::Active {
        return Ok(Err("account_inactive"));
    }
    match credential.get_user_unique_id() {
        Some(handle) if handle != user_handle(&row.user_id).as_bytes() => {
            return Ok(Err("user_handle_mismatch"));
        }
        None if owner.is_none() => return Ok(Err("bad_user_handle")),
        _ => {}
    }
    let mut passkey: Passkey =
        serde_json::from_str(&row.passkey_json).map_err(|_| TxError::Corrupt)?;
    let key = DiscoverableKey::from(&passkey);
    match webauthn.finish_discoverable_authentication(credential, authentication, &[key]) {
        Ok(result) => {
            let state = if passkey.update_credential(&result) == Some(true) {
                Some(serde_json::to_string(&passkey).map_err(|_| TxError::Corrupt)?)
            } else {
                None
            };
            rows::record_use(tx, row.id, state.as_deref(), now)?;
            Ok(Ok(row.user_id))
        }
        Err(WebauthnError::CredentialPossibleCompromise) => {
            audit_passkey(
                tx,
                audit::PASSKEY_CLONE_SUSPECTED,
                None,
                &row.user_id,
                row.id,
                now,
            )?;
            tracing::warn!(passkey = row.id, "passkey counter did not grow; refused");
            Ok(Err("counter_not_increased"))
        }
        Err(err) => Ok(Err(reason_of(&err))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passkeys(url: &str) -> Passkeys {
        Passkeys::new(&PublicUrl::parse(url).unwrap(), Duration::from_secs(300))
    }

    #[test]
    fn the_public_url_decides_whether_passkeys_work() {
        for on in [
            "https://refs.niccolofanton.dev",
            "http://localhost:18193",
            "http://app.localhost:5173",
        ] {
            assert!(passkeys(on).is_enabled(), "{on}");
        }
        for off in [
            "http://refs.example.test",
            "http://127.0.0.1:8080",
            "https://192.0.2.10",
            "http://[::1]:8080",
        ] {
            assert!(!passkeys(off).is_enabled(), "{off}");
        }
    }

    #[test]
    fn ceremonies_are_used_once() {
        let passkeys = passkeys("http://localhost:18193");
        let webauthn = passkeys.webauthn().unwrap();
        let (_, state) = webauthn.start_discoverable_authentication().unwrap();
        let id = passkeys.begin(Ceremony::SignIn(state));
        assert_eq!(id.len(), crate::tokens::TOKEN_LEN);
        assert!(passkeys.take(&id).is_some());
        assert!(passkeys.take(&id).is_none(), "used");
        assert!(passkeys.take("not a ceremony").is_none());
    }

    #[test]
    fn expired_ceremonies_are_refused_before_their_eviction() {
        let passkeys = Passkeys::new(
            &PublicUrl::parse("http://localhost:18193").unwrap(),
            Duration::from_millis(50),
        );
        let webauthn = passkeys.webauthn().unwrap();
        let (_, state) = webauthn.start_discoverable_authentication().unwrap();
        let id = passkeys.begin(Ceremony::SignIn(state));
        std::thread::sleep(Duration::from_millis(80));
        assert!(passkeys.take(&id).is_none(), "expired");
    }

    #[test]
    fn user_handles_are_opaque_and_stable() {
        let a = user_handle("01J9Z3B8K4QW6TFX0V7G2N5RCE");
        assert_eq!(a, user_handle("01J9Z3B8K4QW6TFX0V7G2N5RCE"));
        assert_ne!(a, user_handle("01J9Z3B8K4QW6TFX0V7G2N5RCF"));
        assert_eq!(a.as_bytes().len(), 16);
    }

    #[test]
    fn labels_are_trimmed_and_bounded() {
        assert_eq!(normalize_label(None).unwrap(), None);
        assert_eq!(normalize_label(Some("   ")).unwrap(), None);
        assert_eq!(
            normalize_label(Some("  iPhone ")).unwrap().as_deref(),
            Some("iPhone")
        );
        let long = "é".repeat(LABEL_MAX_CHARS);
        assert_eq!(normalize_label(Some(&long)).unwrap(), Some(long.clone()));
        for bad in [format!("{long}x"), "a\nb".to_owned()] {
            let err = normalize_label(Some(&bad)).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ValidationFailed, "{bad:?}");
        }
    }
}
