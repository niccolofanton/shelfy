//! The device flow (RFC 8628) that signs the migration CLI in (plan §2.11,
//! §4.1 step 2).
//!
//! 1. `shelfy-migrate login` calls `POST /auth/device/start` ([`start`]) and
//!    shows the user code (`BCDF-GHJK`) with the page that approves it,
//!    `<public url>/device` (or `/device#BCDF-GHJK`, which fills it in).
//! 2. The user, signed in to the web app, approves the code there:
//!    `POST /auth/device/approve` ([`approve`]), which needs a sign-in or a
//!    re-authentication from the last 5 minutes.
//! 3. Meanwhile the CLI polls `POST /auth/device/poll` ([`poll`]) with its
//!    device code, every `interval` seconds. Once the code is approved, the
//!    poll answers a `migrate` token valid 7 days ([`super::api_tokens`]),
//!    once.
//!
//! **State.** Flows live in memory, [`DeviceFlows`] (assumption G5: the
//! control schema has no table for them, and `pairing_codes.user_id` is NOT
//! NULL). A restart forgets them; the CLI then asks for a new code. A flow
//! lasts [`super::AuthConfig::device_code_ttl`] (10 minutes); at most
//! [`MAX_FLOWS`] are kept, the least recently used going first.
//!
//! **Secrets.** The device code is 256 random bits, so it cannot be guessed
//! and the poll needs no other limit. The user code is short enough to type:
//! 8 letters from 20 consonants (RFC 8628 §6.1, about 34.6 bits), shown as
//! `XXXX-XXXX`, read without regard to case, dashes or spaces. Approvals are
//! limited per user ([`super::AuthConfig::user_code_limit`]), so a signed-in
//! user cannot guess someone else's pending code. Both codes are held as
//! SHA-256 digests only and never logged. A flow delivers its token once,
//! then it is gone: a replayed device code or user code is refused.
//!
//! **Pacing.** A poll that comes sooner than the interval (with 20 % leeway)
//! answers `slow_down`, and the interval grows by 5 seconds (RFC 8628 §3.5)
//! up to [`MAX_INTERVAL`]. A device code takes at most
//! [`MAX_POLLS_PER_MINUTE`] polls a minute, `slow_down` answers included;
//! past that, polls answer 429 until the minute is over. That bound is per
//! device code because the poll is not counted by the sign-in limit per
//! client address ([`crate::rate_limit::UNCOUNTED_SIGN_IN_ROUTES`]): the
//! CLI and the browser that approves its code usually share an address.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use moka::policy::EvictionPolicy;
use moka::sync::Cache;
use serde_json::json;
use sha2::{Digest, Sha256};
use shelfy_core::repo::RepoError;

use super::api_tokens::{self, MIGRATE_LABEL, MIGRATE_TOKEN_TTL, Mint, Minted, Via};
use super::bearer::Scope;
use super::rate_limit::{self, RateLimiter};
use super::{SessionUser, millis};
use crate::control::api_tokens::TokenKind;
use crate::control::audit::{self, Entry};
use crate::control::users::{self, Status};
use crate::error::{ApiError, ErrorCode};
use crate::ids::now_ms;
use crate::state::{AppState, blocking};
use crate::telemetry::redact::Redacted;
use crate::tokens::{SecretToken, TokenHash, hash_token, is_token_shaped};

/// The letters of a user code: consonants only, so no word and no
/// look-alike digits (RFC 8628 §6.1).
pub const USER_CODE_ALPHABET: &[u8; 20] = b"BCDFGHJKLMNPQRSTVWXZ";
/// Letters in a user code.
pub const USER_CODE_LEN: usize = 8;
/// Most flows kept at once.
pub const MAX_FLOWS: u64 = 1_000;
/// What a `slow_down` adds to the interval (RFC 8628 §3.5).
pub const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);
/// The longest interval `slow_down` grows to.
pub const MAX_INTERVAL: Duration = Duration::from_secs(60);
/// The scope a device gets.
pub const DEVICE_SCOPE: Scope = Scope::Migrate;
/// Most polls of one device code in a minute. A CLI that keeps to the
/// interval polls 12 times a minute at most.
pub const MAX_POLLS_PER_MINUTE: u32 = 20;

/// The window of [`MAX_POLLS_PER_MINUTE`], unix ms.
const POLL_WINDOW_MS: i64 = 60_000;

/// Domain separation of the user-code digests.
const USER_CODE_PREFIX: &[u8] = b"shelfy.device.user-code\0";
/// Longest input read as a user code; anything longer is refused unread.
const MAX_USER_CODE_INPUT: usize = 32;

/// The digest a user code is kept as.
type CodeHash = [u8; 32];

/// The flows in progress (see the module docs).
pub struct DeviceFlows {
    /// By the digest of the device code.
    flows: Cache<TokenHash, Arc<Mutex<Flow>>>,
    /// The device-code digest of each user-code digest.
    user_codes: Cache<CodeHash, TokenHash>,
    ttl: Duration,
    interval: Duration,
}

/// One flow.
struct Flow {
    user_code: CodeHash,
    /// Unix ms; the cache's time to live only bounds memory.
    expires_at: i64,
    interval_ms: i64,
    last_poll_at: Option<i64>,
    /// The start of the current minute of polls, unix ms, and its polls.
    window_start: i64,
    window_polls: u32,
    state: FlowState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FlowState {
    /// Waiting for a signed-in user.
    Pending,
    /// Approved by this user; the next poll gets the token.
    Approved { user_id: String },
    /// A poll is minting the token.
    Delivering { user_id: String },
}

/// A flow just started: what the CLI shows and keeps.
#[derive(Debug)]
pub struct Started {
    /// The device code, which the CLI polls with. Its only copy.
    pub device_code: SecretToken,
    /// The user code, as shown: `XXXX-XXXX`.
    pub user_code: Redacted<String>,
    /// How long both codes stay valid.
    pub expires_in: Duration,
    /// How long the CLI waits between polls.
    pub interval: Duration,
}

/// What a poll found.
#[derive(Debug, PartialEq, Eq)]
pub enum Polled {
    /// Not approved yet: poll again after `interval`.
    Pending {
        /// The wait before the next poll.
        interval: Duration,
    },
    /// Polled too soon: wait `interval`, which grew, from now on.
    SlowDown {
        /// The wait before the next poll.
        interval: Duration,
    },
    /// Approved: mint the token, then [`DeviceFlows::delivered`], or
    /// [`DeviceFlows::release`] when minting failed.
    Approved(Delivery),
    /// Polled [`MAX_POLLS_PER_MINUTE`] times this minute already: refused
    /// for `retry_after`.
    Limited {
        /// The wait until the minute is over.
        retry_after: Duration,
    },
    /// Unknown, expired, or delivered already.
    Invalid,
}

/// An approved flow whose token a poll is minting.
#[derive(Debug, PartialEq, Eq)]
pub struct Delivery {
    device: TokenHash,
    /// Who approved it: the token acts for this user.
    pub user_id: String,
}

/// What an approval found.
#[derive(Debug, PartialEq, Eq)]
pub enum Approval {
    /// Approved now.
    Approved(ApprovedFlow),
    /// This user approved it already.
    Already,
    /// Unknown, expired, delivered, or approved by someone else.
    Invalid,
}

/// A flow this call approved; [`DeviceFlows::revoke_approval`] takes the
/// approval back.
#[derive(Debug, PartialEq, Eq)]
pub struct ApprovedFlow {
    device: TokenHash,
    user_id: String,
}

fn lock(flow: &Mutex<Flow>) -> MutexGuard<'_, Flow> {
    flow.lock().unwrap_or_else(PoisonError::into_inner)
}

impl DeviceFlows {
    /// Flows that last `ttl`, polled every `interval`.
    #[must_use]
    pub fn new(ttl: Duration, interval: Duration) -> Self {
        let ttl = ttl.max(Duration::from_millis(1));
        Self {
            flows: Cache::builder()
                .max_capacity(MAX_FLOWS)
                .time_to_live(ttl)
                .eviction_policy(EvictionPolicy::lru())
                .build(),
            user_codes: Cache::builder()
                .max_capacity(MAX_FLOWS)
                .time_to_live(ttl)
                .eviction_policy(EvictionPolicy::lru())
                .build(),
            ttl,
            interval,
        }
    }

    /// Starts a flow at `now` (unix ms).
    #[must_use]
    pub fn start(&self, now: i64) -> Started {
        let device_code = SecretToken::generate();
        let (user_code, user_code_hash) = loop {
            let code = generate_user_code();
            let hash = user_code_hash(&code);
            // Two pending flows never share a user code.
            if !self.user_codes.contains_key(&hash) {
                break (code, hash);
            }
        };
        let flow = Flow {
            user_code: user_code_hash,
            expires_at: now.saturating_add(millis(self.ttl)),
            interval_ms: millis(self.interval),
            last_poll_at: None,
            window_start: now,
            window_polls: 0,
            state: FlowState::Pending,
        };
        let device = device_code.hash();
        self.flows.insert(device, Arc::new(Mutex::new(flow)));
        self.user_codes.insert(user_code_hash, device);
        Started {
            device_code,
            user_code: Redacted(format_user_code(&user_code)),
            expires_in: self.ttl,
            interval: self.interval,
        }
    }

    /// Polls the flow of `device_code` at `now`.
    #[must_use]
    pub fn poll(&self, device_code: &str, now: i64) -> Polled {
        if !is_token_shaped(device_code) {
            return Polled::Invalid;
        }
        let device = hash_token(device_code);
        let Some(entry) = self.flows.get(&device) else {
            return Polled::Invalid;
        };
        let mut flow = lock(&entry);
        if now >= flow.expires_at {
            let user_code = flow.user_code;
            drop(flow);
            self.forget(&device, &user_code);
            return Polled::Invalid;
        }
        if now.saturating_sub(flow.window_start) >= POLL_WINDOW_MS {
            flow.window_start = now;
            flow.window_polls = 0;
        }
        if flow.window_polls >= MAX_POLLS_PER_MINUTE {
            return Polled::Limited {
                retry_after: duration(flow.window_start.saturating_add(POLL_WINDOW_MS) - now),
            };
        }
        flow.window_polls += 1;
        let too_soon = flow.last_poll_at.is_some_and(|last| {
            now.saturating_sub(last).saturating_mul(5) < flow.interval_ms.saturating_mul(4)
        });
        flow.last_poll_at = Some(now);
        if too_soon {
            flow.interval_ms = flow
                .interval_ms
                .saturating_add(millis(SLOW_DOWN_STEP))
                .min(millis(MAX_INTERVAL));
            return Polled::SlowDown {
                interval: duration(flow.interval_ms),
            };
        }
        match flow.state.clone() {
            FlowState::Pending | FlowState::Delivering { .. } => Polled::Pending {
                interval: duration(flow.interval_ms),
            },
            FlowState::Approved { user_id } => {
                flow.state = FlowState::Delivering {
                    user_id: user_id.clone(),
                };
                Polled::Approved(Delivery { device, user_id })
            }
        }
    }

    /// The token of `delivery` was handed out: the flow is over, and both of
    /// its codes stop working.
    pub fn delivered(&self, delivery: &Delivery) {
        if let Some(entry) = self.flows.get(&delivery.device) {
            let user_code = lock(&entry).user_code;
            self.forget(&delivery.device, &user_code);
        }
    }

    /// Minting the token of `delivery` failed: the next poll tries again.
    pub fn release(&self, delivery: &Delivery) {
        if let Some(entry) = self.flows.get(&delivery.device) {
            let mut flow = lock(&entry);
            if flow.state
                == (FlowState::Delivering {
                    user_id: delivery.user_id.clone(),
                })
            {
                flow.state = FlowState::Approved {
                    user_id: delivery.user_id.clone(),
                };
            }
        }
    }

    /// Approves the flow of `user_code` for `user_id` at `now`.
    #[must_use]
    pub fn approve(&self, user_code: &str, user_id: &str, now: i64) -> Approval {
        let Some(code) = normalize_user_code(user_code) else {
            return Approval::Invalid;
        };
        let code_hash = user_code_hash(&code);
        let Some(device) = self.user_codes.get(&code_hash) else {
            return Approval::Invalid;
        };
        let Some(entry) = self.flows.get(&device) else {
            return Approval::Invalid;
        };
        let mut flow = lock(&entry);
        if now >= flow.expires_at {
            drop(flow);
            self.forget(&device, &code_hash);
            return Approval::Invalid;
        }
        match &flow.state {
            FlowState::Pending => {
                flow.state = FlowState::Approved {
                    user_id: user_id.to_owned(),
                };
                Approval::Approved(ApprovedFlow {
                    device,
                    user_id: user_id.to_owned(),
                })
            }
            FlowState::Approved { user_id: by } | FlowState::Delivering { user_id: by }
                if by == user_id =>
            {
                Approval::Already
            }
            FlowState::Approved { .. } | FlowState::Delivering { .. } => Approval::Invalid,
        }
    }

    /// Takes back an approval that could not be recorded: the flow waits
    /// again, unless its token is being delivered already.
    pub fn revoke_approval(&self, approved: &ApprovedFlow) {
        if let Some(entry) = self.flows.get(&approved.device) {
            let mut flow = lock(&entry);
            if flow.state
                == (FlowState::Approved {
                    user_id: approved.user_id.clone(),
                })
            {
                flow.state = FlowState::Pending;
            }
        }
    }

    fn forget(&self, device: &TokenHash, user_code: &CodeHash) {
        self.flows.invalidate(device);
        self.user_codes.invalidate(user_code);
    }
}

/// `ms` milliseconds.
fn duration(ms: i64) -> Duration {
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

/// A fresh user code: [`USER_CODE_LEN`] letters of [`USER_CODE_ALPHABET`],
/// uniformly drawn.
fn generate_user_code() -> String {
    let alphabet = USER_CODE_ALPHABET.len();
    // The largest multiple of the alphabet's size below 256: bytes from it
    // up are drawn again, so every letter is equally likely.
    let limit = 256 - 256 % alphabet;
    let mut code = String::with_capacity(USER_CODE_LEN);
    let mut bytes = [0u8; 16];
    while code.len() < USER_CODE_LEN {
        getrandom::fill(&mut bytes).expect("the OS random number generator failed");
        for &byte in &bytes {
            if usize::from(byte) < limit && code.len() < USER_CODE_LEN {
                code.push(char::from(USER_CODE_ALPHABET[usize::from(byte) % alphabet]));
            }
        }
    }
    code
}

/// `code` (8 letters) as shown: `XXXX-XXXX`.
fn format_user_code(code: &str) -> String {
    let (head, tail) = code.split_at(USER_CODE_LEN / 2);
    format!("{head}-{tail}")
}

/// A typed user code as stored: case, dashes and spaces ignored; `None`
/// unless it is [`USER_CODE_LEN`] letters of the alphabet.
#[must_use]
pub fn normalize_user_code(input: &str) -> Option<String> {
    if input.len() > MAX_USER_CODE_INPUT {
        return None;
    }
    let code: String = input
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let valid =
        code.len() == USER_CODE_LEN && code.bytes().all(|b| USER_CODE_ALPHABET.contains(&b));
    valid.then_some(code)
}

fn user_code_hash(code: &str) -> CodeHash {
    Sha256::new()
        .chain_update(USER_CODE_PREFIX)
        .chain_update(code.as_bytes())
        .finalize()
        .into()
}

/// The answer of a poll ([`poll`]).
#[derive(Debug)]
pub enum PollOutcome {
    /// Not approved yet.
    Pending {
        /// The wait before the next poll.
        interval: Duration,
    },
    /// Polled too soon.
    SlowDown {
        /// The wait before the next poll, which grew.
        interval: Duration,
    },
    /// Polled too often this minute.
    Limited {
        /// The wait until polls are answered again.
        retry_after: Duration,
    },
    /// The token, minted for the approver.
    Approved(Minted),
}

/// 400 `invalid_device_code`.
fn invalid() -> ApiError {
    ApiError::new(ErrorCode::InvalidDeviceCode)
}

/// Starts a flow (`POST /auth/device/start`).
#[must_use]
pub fn start(state: &AppState) -> Started {
    state.auth().devices().start(now_ms())
}

/// Polls the flow of `device_code` (`POST /auth/device/poll`). Once it is
/// approved, mints the approver's `migrate` token and ends the flow.
///
/// The minting and the end of the flow run together on the blocking pool,
/// so they complete even when the CLI's connection drops meanwhile: the flow
/// never stays half delivered. A token whose answer was lost is never shown
/// again; the CLI starts over (its next poll is refused), and the lost token
/// stays in the account's list until it expires or is revoked.
///
/// # Errors
///
/// 400 `invalid_device_code` for an unknown, expired or used device code,
/// or when the approver's account is no longer active; the control database
/// failed (the next poll tries again).
pub async fn poll(state: &AppState, device_code: &str) -> Result<PollOutcome, ApiError> {
    let delivery = match state.auth().devices().poll(device_code, now_ms()) {
        Polled::Invalid => return Err(invalid()),
        Polled::Pending { interval } => return Ok(PollOutcome::Pending { interval }),
        Polled::SlowDown { interval } => return Ok(PollOutcome::SlowDown { interval }),
        Polled::Limited { retry_after } => return Ok(PollOutcome::Limited { retry_after }),
        Polled::Approved(delivery) => delivery,
    };
    let user_id = delivery.user_id.clone();
    let approver = delivery.user_id.clone();
    let worker = state.clone();
    let now = now_ms();
    let minted = blocking(move || {
        let minted = worker.control().write(|tx| {
            let active = users::get(tx, &user_id)?.is_some_and(|u| u.status == Status::Active);
            if !active {
                return Ok(None);
            }
            let mint = Mint {
                user_id: &user_id,
                kind: TokenKind::Migrate,
                scopes: &[DEVICE_SCOPE],
                label: Some(MIGRATE_LABEL),
                ttl: Some(MIGRATE_TOKEN_TTL),
                via: Via::Device,
                actor: Some(&user_id),
            };
            api_tokens::mint(tx, &mint, now).map(Some)
        });
        let flows = worker.auth().devices();
        match &minted {
            Ok(_) => flows.delivered(&delivery),
            Err(_) => flows.release(&delivery),
        }
        minted.map_err(ApiError::from)
    })
    .await?;
    let Some(minted) = minted else {
        return Err(invalid());
    };
    tracing::info!(user_id = %approver, "device signed in");
    Ok(PollOutcome::Approved(minted))
}

/// Approves the flow whose user code is `user_code` for `user`'s account
/// (`POST /auth/device/approve`), and writes `device.approve` to the audit
/// log. Approving a code again is a no-op.
///
/// # Errors
///
/// 429 `rate_limited` past [`super::AuthConfig::user_code_limit`];
/// 400 `invalid_device_code` for a code that is unknown, expired, used or
/// approved by another account; the control database failed.
pub async fn approve(
    state: &AppState,
    user: &SessionUser,
    user_code: &str,
) -> Result<(), ApiError> {
    let now = now_ms();
    hit(
        state.auth().user_code_limiter(),
        &rate_limit::key("device-approve", user.id()),
        now,
    )?;
    let flows = state.auth().devices();
    let approved = match flows.approve(user_code, user.id(), now) {
        Approval::Approved(approved) => approved,
        Approval::Already => return Ok(()),
        Approval::Invalid => return Err(invalid()),
    };
    let control = Arc::clone(state.control());
    let user_id = user.id().to_owned();
    let recorded = blocking(move || {
        control.write(|tx| {
            let meta = json!({ "scope": DEVICE_SCOPE.as_str() });
            let entry = Entry {
                action: audit::DEVICE_APPROVE,
                actor_user_id: Some(&user_id),
                target: Some(&user_id),
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            Ok::<_, RepoError>(())
        })
    })
    .await;
    if let Err(err) = recorded {
        flows.revoke_approval(&approved);
        return Err(err);
    }
    Ok(())
}

/// Counts a hit on `limiter`, or refuses with 429 `rate_limited`.
fn hit(limiter: &RateLimiter, key: &rate_limit::Key, now: i64) -> Result<(), ApiError> {
    limiter
        .hit(key, now)
        .map_err(|seconds| ApiError::new(ErrorCode::RateLimited).with_retry_after(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_790_899_200_000;
    const SECOND: i64 = 1_000;

    fn flows() -> DeviceFlows {
        DeviceFlows::new(Duration::from_secs(600), Duration::from_secs(5))
    }

    #[test]
    fn user_codes_are_typable_and_read_leniently() {
        for _ in 0..200 {
            let code = generate_user_code();
            assert_eq!(code.len(), USER_CODE_LEN);
            assert!(
                code.bytes().all(|b| USER_CODE_ALPHABET.contains(&b)),
                "{code}"
            );
            let shown = format_user_code(&code);
            assert_eq!(shown.len(), 9);
            assert_eq!(&shown[4..5], "-");
            assert_eq!(normalize_user_code(&shown).as_deref(), Some(code.as_str()));
            let typed = format!(" {} ", shown.to_ascii_lowercase().replace('-', " "));
            assert_eq!(normalize_user_code(&typed).as_deref(), Some(code.as_str()));
        }
        for bad in [
            "",
            "BCDF-GHJ",
            "BCDF-GHJKL",
            "BCDF-GHJA",
            "BCDF-GHJ1",
            &"B".repeat(40),
        ] {
            assert_eq!(normalize_user_code(bad), None, "{bad}");
        }
    }

    #[test]
    fn every_letter_is_drawn() {
        let mut seen = [false; 20];
        for _ in 0..200 {
            for b in generate_user_code().bytes() {
                let i = USER_CODE_ALPHABET.iter().position(|&a| a == b).unwrap();
                seen[i] = true;
            }
        }
        assert!(seen.iter().all(|s| *s), "{seen:?}");
    }

    #[test]
    fn a_flow_waits_for_approval_then_delivers_once() {
        let flows = flows();
        let started = flows.start(T0);
        assert_eq!(started.expires_in, Duration::from_secs(600));
        assert_eq!(started.interval, Duration::from_secs(5));
        let device = started.device_code.expose().to_owned();
        let code = started.user_code.expose().clone();
        assert!(!format!("{started:?}").contains(&device));
        assert!(!format!("{started:?}").contains(&code));

        assert_eq!(
            flows.poll(&device, T0 + 5 * SECOND),
            Polled::Pending {
                interval: Duration::from_secs(5)
            }
        );
        assert!(matches!(
            flows.approve(&code, "u1", T0 + 6 * SECOND),
            Approval::Approved(_)
        ));
        assert_eq!(
            flows.approve(&code, "u1", T0 + 7 * SECOND),
            Approval::Already
        );
        assert_eq!(
            flows.approve(&code, "u2", T0 + 7 * SECOND),
            Approval::Invalid
        );

        let Polled::Approved(delivery) = flows.poll(&device, T0 + 10 * SECOND) else {
            panic!("approved");
        };
        assert_eq!(delivery.user_id, "u1");
        // Another poll while the token is minted waits.
        assert!(matches!(
            flows.poll(&device, T0 + 15 * SECOND),
            Polled::Pending { .. }
        ));
        flows.delivered(&delivery);
        // Replays: the device code and the user code are spent.
        assert_eq!(flows.poll(&device, T0 + 20 * SECOND), Polled::Invalid);
        assert_eq!(
            flows.approve(&code, "u1", T0 + 20 * SECOND),
            Approval::Invalid
        );
    }

    #[test]
    fn a_failed_delivery_or_record_is_retried() {
        let flows = flows();
        let started = flows.start(T0);
        let device = started.device_code.expose().to_owned();
        let code = started.user_code.expose().clone();

        let Approval::Approved(approved) = flows.approve(&code, "u1", T0) else {
            panic!("approved");
        };
        flows.revoke_approval(&approved);
        assert!(matches!(flows.poll(&device, T0), Polled::Pending { .. }));
        assert!(matches!(
            flows.approve(&code, "u1", T0),
            Approval::Approved(_)
        ));

        let Polled::Approved(delivery) = flows.poll(&device, T0 + 5 * SECOND) else {
            panic!("approved");
        };
        flows.release(&delivery);
        assert!(matches!(
            flows.poll(&device, T0 + 10 * SECOND),
            Polled::Approved(_)
        ));
    }

    #[test]
    fn polling_too_soon_slows_the_device_down() {
        let flows = flows();
        let device = flows.start(T0).device_code.expose().to_owned();
        assert!(matches!(flows.poll(&device, T0), Polled::Pending { .. }));
        // 20 % early is fine; sooner is not.
        assert!(matches!(
            flows.poll(&device, T0 + 4 * SECOND),
            Polled::Pending { .. }
        ));
        assert_eq!(
            flows.poll(&device, T0 + 7 * SECOND),
            Polled::SlowDown {
                interval: Duration::from_secs(10)
            }
        );
        assert_eq!(
            flows.poll(&device, T0 + 17 * SECOND),
            Polled::Pending {
                interval: Duration::from_secs(10)
            }
        );
        // Ten more back to back: 15, 20 … 60 seconds, then no further.
        let mut at = T0 + 17 * SECOND;
        for _ in 0..10 {
            at += 1;
            let _ = flows.poll(&device, at);
        }
        assert_eq!(
            flows.poll(&device, at + 1),
            Polled::SlowDown {
                interval: MAX_INTERVAL
            }
        );
    }

    #[test]
    fn a_device_code_takes_at_most_20_polls_a_minute() {
        let flows = flows();
        let device = flows.start(T0).device_code.expose().to_owned();
        let other = flows.start(T0).device_code.expose().to_owned();
        for i in 0..20 {
            let polled = flows.poll(&device, T0 + i);
            assert!(
                matches!(polled, Polled::Pending { .. } | Polled::SlowDown { .. }),
                "poll {i}: {polled:?}"
            );
        }
        assert_eq!(
            flows.poll(&device, T0 + 30 * SECOND),
            Polled::Limited {
                retry_after: Duration::from_secs(30)
            }
        );
        // Other device codes have their own minute.
        assert!(matches!(
            flows.poll(&other, T0 + 30 * SECOND),
            Polled::Pending { .. }
        ));
        // A new minute, new polls.
        assert!(!matches!(
            flows.poll(&device, T0 + 60 * SECOND),
            Polled::Limited { .. }
        ));
    }

    #[test]
    fn codes_expire() {
        let flows = flows();
        let started = flows.start(T0);
        let device = started.device_code.expose().to_owned();
        let code = started.user_code.expose().clone();
        let end = T0 + 600 * SECOND;
        assert_eq!(flows.approve(&code, "u1", end), Approval::Invalid);
        assert_eq!(flows.poll(&device, end - 1), Polled::Invalid, "forgotten");

        let started = flows.start(T0);
        let device = started.device_code.expose().to_owned();
        assert_eq!(flows.poll(&device, end), Polled::Invalid);
    }

    #[test]
    fn malformed_and_unknown_codes_are_invalid() {
        let flows = flows();
        let _ = flows.start(T0);
        assert_eq!(flows.poll("short", T0), Polled::Invalid);
        let unknown = SecretToken::generate();
        assert_eq!(flows.poll(unknown.expose(), T0), Polled::Invalid);
        assert_eq!(flows.approve("BCDF-GHJK", "u1", T0), Approval::Invalid);
        assert_eq!(flows.approve("nonsense", "u1", T0), Approval::Invalid);
    }
}
