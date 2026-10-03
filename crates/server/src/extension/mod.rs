//! The browser extension's side of the server (plan §2.11 device tokens,
//! §2.16; P2-03, contracts C1–C3 and C8 in `docs/web-port/phases/P2.md`).
//!
//! | Part | Where |
//! |---|---|
//! | pairing: a 60-second code from the web app, exchanged by the extension for its token | [`pairing`], the routes `POST /me/tokens/pairing-code` and `POST /extension/pair` |
//! | its configuration: minimum version, kill switches, pacing, stop thresholds | [`flags`] (typed `feature_flags` keys and their defaults), `GET /extension/config` |
//! | presence and the version gate of every extension-token request | [`seen`] and [`admit`], called by the access gate ([`crate::auth::access`]) |
//! | presence: connected or not, version, last request; the `extension.status` event | [`presence`], `GET /extension/status` |
//! | tasks: uploads, refreshes and hydrations for the archive, their leases and the long poll (P2-14) | [`tasks`], `GET /ingest/tasks`, `POST /ingest/tasks/{id}/complete` |
//!
//! **Requests (C1).** The extension sends `Authorization: Bearer shx_…` (a
//! token of kind `extension`) and `X-Shelfy-Extension: <version>`, its
//! manifest version. Once the gate has verified the token, [`seen`] records
//! the request in [`presence`]; once the token passed the route's scope
//! (403 first otherwise), [`admit`] refuses it with 426
//! `extension_outdated` when the version is missing, malformed or older than
//! `minVersion` ([`flags`]), except on `GET /extension/config`, which tells
//! an outdated extension what to update to. Other kinds of tokens (the
//! Shortcut, the migration CLI) are not concerned.
//!
//! **Kill switches.** `shelfy-server admin flags set extension.<platform>.<mode>
//! false` (E4: no `/admin` page) turns a capture mode off; the server reads
//! the flags again at most [`ExtensionSettings::flags_ttl`] (30 s) after a
//! change, and the extension picks them up on its next config refresh. The
//! values are data only: no code is ever sent to the extension.

pub mod flags;
pub mod pairing;
pub mod presence;
pub mod tasks;

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use axum::http::{HeaderMap, HeaderName, Method};
use utoipa::IntoParams;

use crate::error::{ApiError, ErrorCode};
use crate::jobs::Clock;
use crate::state::AppState;

pub use flags::{ExtensionConfig, FlagCache};
pub use presence::Presence;
pub use tasks::TaskBoard;

/// `X-Shelfy-Extension`: the extension's version, on every request it makes
/// (C1).
pub const VERSION_HEADER: HeaderName = HeaderName::from_static("x-shelfy-extension");

/// The route that answers an outdated extension too: its configuration,
/// which names the minimum version.
pub const CONFIG_ROUTE: &str = "/api/v1/extension/config";

/// The request header of the extension's routes, for the OpenAPI document
/// (`params(ExtensionHeaders)`); the access gate reads it ([`seen`], [`admit`]).
#[derive(Clone, Debug, Default, IntoParams)]
#[into_params(parameter_in = Header)]
pub struct ExtensionHeaders {
    /// The extension's manifest version (`0.2.0`). An extension token's
    /// request without it, or below `minVersion`, gets 426
    /// `extension_outdated`, except `GET /extension/config`.
    #[param(rename = "X-Shelfy-Extension", nullable = false)]
    pub x_shelfy_extension: Option<String>,
}

/// Server-side timings of the extension support. The extension's own
/// configuration is [`ExtensionConfig`], from the flags.
#[derive(Clone, Debug)]
pub struct ExtensionSettings {
    /// How long the server serves the flags it read before reading them
    /// again: a change made with `admin flags` takes effect within this.
    pub flags_ttl: Duration,
    /// An extension that made no request for this long is disconnected.
    pub presence_timeout: Duration,
    /// Where pairing-code expiry and presence read the time: the wall clock,
    /// or in tests tokio's (paused) clock.
    pub clock: Clock,
}

/// Most a flag change takes to reach the server (P2-03 acceptance: 30 s).
pub const FLAGS_TTL: Duration = Duration::from_secs(30);
/// Silence after which an extension counts as disconnected (P2-03: 10 min).
pub const PRESENCE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

impl Default for ExtensionSettings {
    fn default() -> Self {
        Self {
            flags_ttl: FLAGS_TTL,
            presence_timeout: PRESENCE_TIMEOUT,
            clock: Clock::System,
        }
    }
}

/// The runtime state of the extension support, shared by every request.
pub struct ExtensionState {
    flags: FlagCache,
    presence: Presence,
    tasks: TaskBoard,
    clock: Clock,
}

impl ExtensionState {
    /// The state for `settings`.
    #[must_use]
    pub fn new(settings: &ExtensionSettings) -> Self {
        Self {
            flags: FlagCache::new(settings.flags_ttl),
            presence: Presence::new(settings.presence_timeout),
            tasks: TaskBoard::default(),
            clock: settings.clock,
        }
    }

    /// The current time of pairing codes and presence, unix ms.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// The flags, read from `feature_flags` at most every
    /// [`ExtensionSettings::flags_ttl`].
    #[must_use]
    pub fn flags(&self) -> &FlagCache {
        &self.flags
    }

    /// Which users' extensions are connected.
    #[must_use]
    pub fn presence(&self) -> &Presence {
        &self.presence
    }

    /// The tasks' leases and the wake-ups of waiting polls ([`tasks`]).
    #[must_use]
    pub fn tasks(&self) -> &TaskBoard {
        &self.tasks
    }
}

/// A version of the extension: 1 to 4 dot-separated numbers without
/// leading zeros, as Chrome's manifest `version` (`0.2.0`), optionally
/// followed by a semver pre-release (`-beta.1`) and build metadata (`+abc`).
/// Precedence is semver's: the numbers, missing ones counting as 0 (so `0.2`
/// equals `0.2.0`), then a pre-release sorts before its release. Build
/// metadata is ignored, and not kept.
#[derive(Clone, Copy)]
pub struct ExtensionVersion {
    parts: [u32; 4],
    len: u8,
    pre: [u8; Self::MAX_PRE_LEN],
    pre_len: u8,
}

impl ExtensionVersion {
    /// Longest text read as a version.
    const MAX_LEN: usize = 64;
    /// Longest pre-release kept (`beta.1`).
    const MAX_PRE_LEN: usize = 32;
    /// Most digits in one number.
    const MAX_DIGITS: usize = 9;

    /// Parses `text`; `None` unless it is a version as described above.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text.is_empty() || text.len() > Self::MAX_LEN {
            return None;
        }
        let (main, build) = match text.split_once('+') {
            Some((main, build)) => (main, Some(build)),
            None => (text, None),
        };
        if build.is_some_and(|build| !build.split('.').all(is_identifier)) {
            return None;
        }
        let (core, pre) = match main.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (main, None),
        };
        let mut parts = [0u32; 4];
        let mut len = 0usize;
        for part in core.split('.') {
            if len == parts.len() || !is_number(part) || part.len() > Self::MAX_DIGITS {
                return None;
            }
            parts[len] = part.parse().ok()?;
            len += 1;
        }
        let mut stored = [0u8; Self::MAX_PRE_LEN];
        let pre_len = match pre {
            None => 0,
            Some(pre) => {
                let valid = pre.len() <= Self::MAX_PRE_LEN
                    && pre.split('.').all(|id| {
                        is_identifier(id)
                            && (!id.bytes().all(|b| b.is_ascii_digit()) || is_number(id))
                    });
                if !valid {
                    return None;
                }
                stored[..pre.len()].copy_from_slice(pre.as_bytes());
                pre.len()
            }
        };
        Some(Self {
            parts,
            len: u8::try_from(len).ok()?,
            pre: stored,
            pre_len: u8::try_from(pre_len).ok()?,
        })
    }

    /// The pre-release (`beta.1`), if any.
    #[must_use]
    pub fn pre_release(&self) -> Option<&str> {
        (self.pre_len > 0)
            .then(|| std::str::from_utf8(&self.pre[..usize::from(self.pre_len)]).ok())
            .flatten()
    }

    /// The version of a request's [`VERSION_HEADER`], if it carries a valid
    /// one.
    #[must_use]
    pub fn of_request(headers: &HeaderMap) -> Option<Self> {
        let mut values = headers.get_all(VERSION_HEADER).iter();
        let value = values.next()?;
        if values.next().is_some() {
            return None;
        }
        Self::parse(value.to_str().ok()?.trim())
    }
}

/// A semver identifier: one or more ASCII letters, digits or `-`.
fn is_identifier(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A number without leading zeros.
fn is_number(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text.len() == 1 || !text.starts_with('0'))
}

/// Semver precedence of two pre-releases (§11.4): identifier by identifier,
/// numbers numerically and below words, words in ASCII order; then the one
/// with more identifiers is higher.
fn cmp_pre_release(a: &str, b: &str) -> Ordering {
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let numeric = |id: &str| id.bytes().all(|b| b.is_ascii_digit());
                let order = match (numeric(x), numeric(y)) {
                    // No leading zeros: the longer number is the larger.
                    (true, true) => x.len().cmp(&y.len()).then_with(|| x.cmp(y)),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => x.cmp(y),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

impl PartialEq for ExtensionVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for ExtensionVersion {}

impl Hash for ExtensionVersion {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.parts.hash(state);
        self.pre_release().hash(state);
    }
}

impl PartialOrd for ExtensionVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ExtensionVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.parts
            .cmp(&other.parts)
            .then_with(|| match (self.pre_release(), other.pre_release()) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => cmp_pre_release(a, b),
            })
    }
}

impl fmt::Display for ExtensionVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, part) in self.parts[..usize::from(self.len)].iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            write!(f, "{part}")?;
        }
        if let Some(pre) = self.pre_release() {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ExtensionVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ExtensionVersion({self})")
    }
}

/// 426 `extension_outdated`: the extension must update to `min` or later.
#[must_use]
pub fn outdated(min: ExtensionVersion) -> ApiError {
    ApiError::new(ErrorCode::ExtensionOutdated)
        .with_detail(format!("X-Shelfy-Extension must be {min} or newer"))
}

/// Whether `method path` (a route template) answers an outdated extension.
#[must_use]
pub fn answers_outdated(method: &Method, path: &str) -> bool {
    (*method == Method::GET || *method == Method::HEAD) && path == CONFIG_ROUTE
}

/// The access gate's first hook, for every request whose `extension` token
/// of `user_id` (token `token_id`) verified, whatever the route's scope:
/// records it in [`presence`] with the version its [`VERSION_HEADER`]
/// names, and announces a change of the user's `extension.status`.
pub fn seen(state: &AppState, user_id: &str, token_id: &str, headers: &HeaderMap) {
    let version = ExtensionVersion::of_request(headers);
    let extension = state.extension();
    if let Some(status) = extension
        .presence()
        .seen(user_id, token_id, version, extension.now_ms())
    {
        state.events().extension_status(user_id, &status);
    }
}

/// The access gate's second hook, once the `extension` token passed the
/// route's scope: refuses the request with 426 `extension_outdated` when its
/// [`VERSION_HEADER`] is missing, malformed or below `minVersion`, unless
/// the route is `GET /extension/config` ([`answers_outdated`]).
///
/// # Errors
///
/// 426 `extension_outdated`; the flags could not be read (a database
/// failure, on the first read only).
pub async fn admit(
    state: &AppState,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    if answers_outdated(method, path) {
        return Ok(());
    }
    let snapshot = state.extension().flags().get(state.control()).await?;
    match ExtensionVersion::of_request(headers) {
        Some(version) if version >= snapshot.min_version() => Ok(()),
        _ => Err(outdated(snapshot.min_version())),
    }
}

/// The token `token_id` of `user_id` was revoked: its extension no longer
/// counts as connected, and a change of the user's status is announced.
pub fn token_revoked(state: &AppState, user_id: &str, token_id: &str) {
    let extension = state.extension();
    if let Some(status) = extension
        .presence()
        .forget(user_id, token_id, extension.now_ms())
    {
        state.events().extension_status(user_id, &status);
    }
}

/// Part of the server's maintenance: extensions silent for
/// [`ExtensionSettings::presence_timeout`] become disconnected, and their
/// users hear `extension.status`; ended task leases are forgotten.
pub fn sweep(state: &AppState) {
    let extension = state.extension();
    for (user_id, status) in extension.presence().sweep(extension.now_ms()) {
        state.events().extension_status(&user_id, &status);
    }
    extension.tasks().sweep(state.jobs().clock().now_ms());
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn version(text: &str) -> ExtensionVersion {
        ExtensionVersion::parse(text).unwrap_or_else(|| panic!("{text} is a version"))
    }

    #[test]
    fn manifest_versions_parse_and_compare_numerically() {
        for text in [
            "0",
            "0.2.0",
            "1.2.3.4",
            "65535.0.10",
            "10.0",
            "65536",
            "0.3.0-beta.1",
        ] {
            assert_eq!(version(text).to_string(), text);
        }
        assert!(version("0.10.0") > version("0.9.9"));
        assert!(version("0.2.0") > version("0.1.65535"));
        assert!(version("1") > version("0.99.99.99"));
        assert_eq!(version("0.2"), version("0.2.0.0"));
        assert_eq!(version("0.2").cmp(&version("0.2.0")), Ordering::Equal);
        assert!(version("0.2.0.1") > version("0.2"));
        for bad in [
            "",
            ".",
            "1.",
            ".1",
            "1..2",
            "1.2.3.4.5",
            "01.2",
            "1.02",
            "-1",
            "+1",
            "v1.2",
            " 1.2",
            "1,2",
            "1.2.3.4444444444",
            "١.٢",
        ] {
            assert!(ExtensionVersion::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn semver_pre_releases_sort_before_their_release() {
        // The precedence example of semver §11.
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for pair in chain.windows(2) {
            assert!(version(pair[0]) < version(pair[1]), "{pair:?}");
        }
        assert!(version("0.2.0-beta") < version("0.2.0"));
        assert!(version("0.2.0-beta") > version("0.1.9"));
        // Build metadata is ignored, and dropped.
        assert_eq!(version("1.0.0+build.5"), version("1.0.0"));
        assert_eq!(version("1.0.0-rc.1+build.5").to_string(), "1.0.0-rc.1");
        assert_eq!(
            version("1.0.0-rc.1").pre_release(),
            Some("rc.1"),
            "kept for display"
        );
        for bad in [
            "1.0.0-",
            "1.0.0-beta..1",
            "1.0.0-01",
            "1.0.0+",
            "1.0.0+a+b",
            "1.0.0-be_ta",
            &format!("1.0.0-{}", "a".repeat(33)),
            &format!("1.0.0+{}", "a".repeat(60)),
        ] {
            assert!(ExtensionVersion::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn the_header_must_name_one_valid_version() {
        let with = |values: &[&str]| {
            let mut headers = HeaderMap::new();
            for value in values {
                headers.append(VERSION_HEADER, HeaderValue::from_str(value).unwrap());
            }
            ExtensionVersion::of_request(&headers)
        };
        assert_eq!(with(&["0.2.0"]), Some(version("0.2.0")));
        assert_eq!(with(&[" 0.3.1 "]), Some(version("0.3.1")));
        assert_eq!(with(&[]), None);
        assert_eq!(with(&["0.2.0", "0.3.0"]), None, "ambiguous");
        assert_eq!(with(&["latest"]), None);
    }

    #[test]
    fn only_the_config_route_answers_outdated_extensions() {
        assert!(answers_outdated(&Method::GET, CONFIG_ROUTE));
        assert!(answers_outdated(&Method::HEAD, CONFIG_ROUTE));
        assert!(!answers_outdated(&Method::POST, CONFIG_ROUTE));
        assert!(!answers_outdated(&Method::GET, "/api/v1/extension/config/"));
        assert!(!answers_outdated(&Method::POST, "/api/v1/posts/lookup"));
    }

    #[test]
    fn the_426_names_the_minimum() {
        let err = outdated(version("0.3.0"));
        assert_eq!(err.code(), ErrorCode::ExtensionOutdated);
        assert_eq!(err.status().as_u16(), 426);
        assert_eq!(
            err.problem().detail.as_deref(),
            Some("X-Shelfy-Extension must be 0.3.0 or newer")
        );
    }
}
