//! The extension's typed flags and its configuration (P2-03; contract C3,
//! plan §2.16 "Server control", P2-G1, P2-G7).
//!
//! Every flag is a key of `feature_flags` with a type, bounds and a default
//! ([`FLAGS`]). `GET /extension/config` answers the defaults with the stored
//! overrides ([`ExtensionConfig`]); the operator changes them with
//! `shelfy-server admin flags` (E4), which validates every value against
//! this registry. A stored value that does not fit its flag (a hand-edited
//! row) is ignored with a warning, so the config always answers.
//!
//! | Key | Type | Default | What |
//! |---|---|---|---|
//! | `extension.minVersion` | version | `0.2.0` | oldest extension the API serves; older ones get 426 everywhere but the config |
//! | `extension.<platform>.passive` | bool | `true` | kill switch: passive capture (`instagram`, `twitter`, `pinterest`) |
//! | `extension.instagram.replay` | bool | `true` | kill switch: the Instagram REST replay |
//! | `extension.<platform>.scroll` | bool | `true` | kill switch: the scroll |
//! | `extension.<platform>.stopAfterKnown` | 1–10,000 | IG 10, X 20, Pinterest 25 | incremental stop: known items in a row, checked at page boundaries (P2-G1) |
//! | `extension.instagram.replayGapMs` | 250–60,000 | 700 | pause between two replay pages |
//! | `extension.instagram.replayMaxPages` | 1–1,000 | 100 | replay pages per run |
//! | `extension.<platform>.scrollSettleMs` | 250–60,000 | IG 650, X 750, Pinterest 650 | wait after each scroll step |
//! | `extension.maxSteps` | 1–100,000 | 16,000 | steps per run |
//! | `extension.maxRunMs` | 60,000–14,400,000 | 1,800,000 | length of a run |
//! | `extension.taskPollMinutes` | 1–1,440 | 5 | how often the extension polls its tasks |
//! | `extension.refreshPerSession` | 0–10,000 | 200 | Instagram refreshes per IG-tab session |
//!
//! The defaults are the fixed pacing of the P2 lane rules (rule 4); making
//! it more aggressive is a lead step with the owner's agreement. The bounds
//! only catch typos.
//!
//! **Freshness.** [`FlagCache`] reads the table at most every
//! [`super::ExtensionSettings::flags_ttl`] (30 s): `admin flags` runs in
//! another process, so the server sees its change within that delay.

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use shelfy_core::db::ControlDb;
use shelfy_core::repo::Platform;
use tokio::time::Instant;
use utoipa::ToSchema;

use super::ExtensionVersion;
use crate::conditional::ETag;
use crate::control::flags::{self as rows, FlagRow};
use crate::error::ApiError;
use crate::state::blocking;

/// The keys of [`FLAGS`].
pub mod keys {
    /// Oldest extension version the API serves.
    pub const MIN_VERSION: &str = "extension.minVersion";
    /// Instagram: passive capture.
    pub const INSTAGRAM_PASSIVE: &str = "extension.instagram.passive";
    /// Instagram: the REST replay.
    pub const INSTAGRAM_REPLAY: &str = "extension.instagram.replay";
    /// Instagram: the scroll.
    pub const INSTAGRAM_SCROLL: &str = "extension.instagram.scroll";
    /// Instagram: the incremental stop.
    pub const INSTAGRAM_STOP_AFTER_KNOWN: &str = "extension.instagram.stopAfterKnown";
    /// Instagram: the pause between replay pages.
    pub const INSTAGRAM_REPLAY_GAP_MS: &str = "extension.instagram.replayGapMs";
    /// Instagram: replay pages per run.
    pub const INSTAGRAM_REPLAY_MAX_PAGES: &str = "extension.instagram.replayMaxPages";
    /// Instagram: the wait after a scroll step.
    pub const INSTAGRAM_SCROLL_SETTLE_MS: &str = "extension.instagram.scrollSettleMs";
    /// X: passive capture.
    pub const TWITTER_PASSIVE: &str = "extension.twitter.passive";
    /// X: the scroll.
    pub const TWITTER_SCROLL: &str = "extension.twitter.scroll";
    /// X: the incremental stop.
    pub const TWITTER_STOP_AFTER_KNOWN: &str = "extension.twitter.stopAfterKnown";
    /// X: the wait after a scroll step.
    pub const TWITTER_SCROLL_SETTLE_MS: &str = "extension.twitter.scrollSettleMs";
    /// Pinterest: passive capture.
    pub const PINTEREST_PASSIVE: &str = "extension.pinterest.passive";
    /// Pinterest: the scroll.
    pub const PINTEREST_SCROLL: &str = "extension.pinterest.scroll";
    /// Pinterest: the incremental stop.
    pub const PINTEREST_STOP_AFTER_KNOWN: &str = "extension.pinterest.stopAfterKnown";
    /// Pinterest: the wait after a scroll step.
    pub const PINTEREST_SCROLL_SETTLE_MS: &str = "extension.pinterest.scrollSettleMs";
    /// Steps per run.
    pub const MAX_STEPS: &str = "extension.maxSteps";
    /// Length of a run, ms.
    pub const MAX_RUN_MS: &str = "extension.maxRunMs";
    /// Minutes between two task polls.
    pub const TASK_POLL_MINUTES: &str = "extension.taskPollMinutes";
    /// Instagram refreshes per IG-tab session.
    pub const REFRESH_PER_SESSION: &str = "extension.refreshPerSession";
}

/// The values a flag takes, and its default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagType {
    /// `true` or `false`.
    Bool {
        /// The default.
        default: bool,
    },
    /// An integer from `min` to `max`.
    Int {
        /// The default.
        default: i64,
        /// The smallest value.
        min: i64,
        /// The largest value.
        max: i64,
    },
    /// An extension version ([`ExtensionVersion`]), as a JSON string.
    Version {
        /// The default.
        default: &'static str,
    },
}

/// One typed flag.
#[derive(Clone, Copy, Debug)]
pub struct Flag {
    /// The key in `feature_flags`.
    pub key: &'static str,
    /// Its values and default.
    pub kind: FlagType,
    /// What it does, for `admin flags list`.
    pub help: &'static str,
}

const fn switch(key: &'static str, help: &'static str) -> Flag {
    Flag {
        key,
        kind: FlagType::Bool { default: true },
        help,
    }
}

const fn int(key: &'static str, default: i64, min: i64, max: i64, help: &'static str) -> Flag {
    Flag {
        key,
        kind: FlagType::Int { default, min, max },
        help,
    }
}

/// Bounds of a wait between two steps, ms.
const SETTLE: (i64, i64) = (250, 60_000);
/// Bounds of a stop threshold.
const STOP: (i64, i64) = (1, 10_000);

/// Every flag, in the order `admin flags list` shows them.
pub const FLAGS: &[Flag] = &[
    Flag {
        key: keys::MIN_VERSION,
        kind: FlagType::Version { default: "0.2.0" },
        help: "oldest extension version the API serves (426 below it, but on the config)",
    },
    switch(keys::INSTAGRAM_PASSIVE, "Instagram passive capture"),
    switch(keys::INSTAGRAM_REPLAY, "the Instagram REST replay"),
    switch(keys::INSTAGRAM_SCROLL, "the Instagram scroll"),
    int(
        keys::INSTAGRAM_STOP_AFTER_KNOWN,
        10,
        STOP.0,
        STOP.1,
        "Instagram incremental stop: known items in a row",
    ),
    int(
        keys::INSTAGRAM_REPLAY_GAP_MS,
        700,
        SETTLE.0,
        SETTLE.1,
        "pause between two Instagram replay pages, ms",
    ),
    int(
        keys::INSTAGRAM_REPLAY_MAX_PAGES,
        100,
        1,
        1_000,
        "Instagram replay pages per run",
    ),
    int(
        keys::INSTAGRAM_SCROLL_SETTLE_MS,
        650,
        SETTLE.0,
        SETTLE.1,
        "wait after an Instagram scroll step, ms",
    ),
    switch(keys::TWITTER_PASSIVE, "X passive capture"),
    switch(keys::TWITTER_SCROLL, "the X scroll"),
    int(
        keys::TWITTER_STOP_AFTER_KNOWN,
        20,
        STOP.0,
        STOP.1,
        "X incremental stop: known items in a row",
    ),
    int(
        keys::TWITTER_SCROLL_SETTLE_MS,
        750,
        SETTLE.0,
        SETTLE.1,
        "wait after an X scroll step, ms",
    ),
    switch(keys::PINTEREST_PASSIVE, "Pinterest passive capture"),
    switch(keys::PINTEREST_SCROLL, "the Pinterest scroll"),
    int(
        keys::PINTEREST_STOP_AFTER_KNOWN,
        25,
        STOP.0,
        STOP.1,
        "Pinterest incremental stop: known items in a row",
    ),
    int(
        keys::PINTEREST_SCROLL_SETTLE_MS,
        650,
        SETTLE.0,
        SETTLE.1,
        "wait after a Pinterest scroll step, ms",
    ),
    int(keys::MAX_STEPS, 16_000, 1, 100_000, "steps per sync run"),
    int(
        keys::MAX_RUN_MS,
        1_800_000,
        60_000,
        14_400_000,
        "length of a sync run, ms",
    ),
    int(
        keys::TASK_POLL_MINUTES,
        5,
        1,
        1_440,
        "minutes between two task polls",
    ),
    int(
        keys::REFRESH_PER_SESSION,
        200,
        0,
        10_000,
        "Instagram refreshes per IG-tab session",
    ),
];

/// A flag's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagValue {
    /// Of a [`FlagType::Bool`] flag.
    Bool(bool),
    /// Of a [`FlagType::Int`] flag.
    Int(i64),
    /// Of a [`FlagType::Version`] flag.
    Version(ExtensionVersion),
}

impl FlagValue {
    /// The value as stored in `feature_flags.value_json`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::Bool(value) => Value::Bool(*value),
            Self::Int(value) => Value::from(*value),
            Self::Version(version) => Value::String(version.to_string()),
        }
    }
}

impl Flag {
    /// The flag with `key`, if any.
    #[must_use]
    pub fn find(key: &str) -> Option<&'static Self> {
        FLAGS.iter().find(|flag| flag.key == key)
    }

    /// The default value.
    #[must_use]
    pub fn default_value(&self) -> FlagValue {
        match self.kind {
            FlagType::Bool { default } => FlagValue::Bool(default),
            FlagType::Int { default, .. } => FlagValue::Int(default),
            FlagType::Version { default } => FlagValue::Version(
                ExtensionVersion::parse(default).expect("the registry's versions are valid"),
            ),
        }
    }

    /// Reads `value` as this flag's value.
    ///
    /// # Errors
    ///
    /// Why `value` does not fit: the wrong JSON type, an integer out of
    /// bounds, a malformed version.
    pub fn parse(&self, value: &Value) -> Result<FlagValue, String> {
        match (self.kind, value) {
            (FlagType::Bool { .. }, Value::Bool(value)) => Ok(FlagValue::Bool(*value)),
            (FlagType::Bool { .. }, _) => Err("must be true or false".to_owned()),
            (FlagType::Int { min, max, .. }, Value::Number(number)) => number
                .as_i64()
                .filter(|n| (min..=max).contains(n))
                .map(FlagValue::Int)
                .ok_or_else(|| format!("must be an integer from {min} to {max}")),
            (FlagType::Int { min, max, .. }, _) => {
                Err(format!("must be an integer from {min} to {max}"))
            }
            (FlagType::Version { .. }, Value::String(text)) => ExtensionVersion::parse(text)
                .map(FlagValue::Version)
                .ok_or_else(|| VERSION_HINT.to_owned()),
            (FlagType::Version { .. }, _) => Err(VERSION_HINT.to_owned()),
        }
    }

    /// A short name of the type, for `admin flags list`.
    #[must_use]
    pub fn type_name(&self) -> String {
        match self.kind {
            FlagType::Bool { .. } => "bool".to_owned(),
            FlagType::Int { min, max, .. } => format!("{min}..={max}"),
            FlagType::Version { .. } => "version".to_owned(),
        }
    }
}

const VERSION_HINT: &str = "must be an extension version such as \"0.2.0\" or \"0.3.0-beta.1\"";

/// The effective value of every flag: the defaults, with the stored values
/// that fit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flags {
    values: BTreeMap<&'static str, FlagValue>,
    /// The keys whose value comes from `feature_flags`.
    overridden: Vec<&'static str>,
}

impl Default for Flags {
    fn default() -> Self {
        Self {
            values: FLAGS
                .iter()
                .map(|flag| (flag.key, flag.default_value()))
                .collect(),
            overridden: Vec::new(),
        }
    }
}

impl Flags {
    /// The defaults, with the stored `rows` that are known flags and fit
    /// their type. Other rows are skipped: an unknown key with a debug line,
    /// a value that does not fit with a warning (never its value).
    #[must_use]
    pub fn from_rows(rows: &[FlagRow]) -> Self {
        let mut flags = Self::default();
        for row in rows {
            let Some(flag) = Flag::find(&row.key) else {
                if row.key.starts_with("extension.") {
                    tracing::warn!(key = %row.key, "unknown extension flag ignored");
                }
                continue;
            };
            let parsed = serde_json::from_str::<Value>(&row.value_json)
                .map_err(|_| "is not JSON".to_owned())
                .and_then(|value| flag.parse(&value));
            match parsed {
                Ok(value) => {
                    flags.values.insert(flag.key, value);
                    flags.overridden.push(flag.key);
                }
                Err(reason) => {
                    tracing::warn!(key = flag.key, %reason, "stored flag ignored: its default applies");
                }
            }
        }
        flags
    }

    /// The effective value of `key`, if it is a flag.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<FlagValue> {
        self.values.get(key).copied()
    }

    /// Whether `key`'s value comes from `feature_flags`.
    #[must_use]
    pub fn is_overridden(&self, key: &str) -> bool {
        self.overridden.contains(&key)
    }

    fn bool(&self, key: &str) -> bool {
        match self.get(key) {
            Some(FlagValue::Bool(value)) => value,
            other => unreachable_flag(key, other, false),
        }
    }

    fn int<T: TryFrom<i64> + Default>(&self, key: &str) -> T {
        match self.get(key) {
            Some(FlagValue::Int(value)) => T::try_from(value).unwrap_or_default(),
            other => unreachable_flag(key, other, T::default()),
        }
    }

    fn version(&self, key: &str) -> ExtensionVersion {
        match self.get(key) {
            Some(FlagValue::Version(version)) => version,
            other => unreachable_flag(
                key,
                other,
                ExtensionVersion::parse("0").expect("a valid version"),
            ),
        }
    }

    /// The oldest extension version the API serves.
    #[must_use]
    pub fn min_version(&self) -> ExtensionVersion {
        self.version(keys::MIN_VERSION)
    }

    /// The extension's configuration (C3).
    #[must_use]
    pub fn config(&self) -> ExtensionConfig {
        ExtensionConfig {
            min_version: self.min_version().to_string(),
            platforms: ExtensionPlatforms {
                instagram: ExtensionInstagram {
                    passive: self.bool(keys::INSTAGRAM_PASSIVE),
                    replay: self.bool(keys::INSTAGRAM_REPLAY),
                    scroll: self.bool(keys::INSTAGRAM_SCROLL),
                    stop_after_known: self.int(keys::INSTAGRAM_STOP_AFTER_KNOWN),
                    replay_gap_ms: self.int(keys::INSTAGRAM_REPLAY_GAP_MS),
                    replay_max_pages: self.int(keys::INSTAGRAM_REPLAY_MAX_PAGES),
                    scroll_settle_ms: self.int(keys::INSTAGRAM_SCROLL_SETTLE_MS),
                },
                twitter: ExtensionPlatform {
                    passive: self.bool(keys::TWITTER_PASSIVE),
                    scroll: self.bool(keys::TWITTER_SCROLL),
                    stop_after_known: self.int(keys::TWITTER_STOP_AFTER_KNOWN),
                    scroll_settle_ms: self.int(keys::TWITTER_SCROLL_SETTLE_MS),
                },
                pinterest: ExtensionPlatform {
                    passive: self.bool(keys::PINTEREST_PASSIVE),
                    scroll: self.bool(keys::PINTEREST_SCROLL),
                    stop_after_known: self.int(keys::PINTEREST_STOP_AFTER_KNOWN),
                    scroll_settle_ms: self.int(keys::PINTEREST_SCROLL_SETTLE_MS),
                },
            },
            max_steps: self.int(keys::MAX_STEPS),
            max_run_ms: self.int(keys::MAX_RUN_MS),
            task_poll_minutes: self.int(keys::TASK_POLL_MINUTES),
            refresh_per_session: self.int(keys::REFRESH_PER_SESSION),
        }
    }
}

/// A key the registry lacks, or of another type: a bug that the tests of
/// this module catch (every flag must reach the config). Logged, and
/// answered with `fallback` instead of a panic in a request.
fn unreachable_flag<T>(key: &str, found: Option<FlagValue>, fallback: T) -> T {
    tracing::error!(key, found = ?found, "flag missing from the registry or of another type");
    fallback
}

/// What `GET /api/v1/extension/config` answers: the extension's minimum
/// version, kill switches, pacing and stop thresholds (contract C3). Data
/// only; the extension refreshes it, and applies a kill switch within one
/// refresh.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionConfig {
    /// The oldest extension version the API serves: below it, every other
    /// request answers 426 `extension_outdated`.
    pub min_version: String,
    /// Per platform: kill switches, pacing, stop thresholds.
    pub platforms: ExtensionPlatforms,
    /// Most steps (scrolls, replay pages) of one sync run.
    pub max_steps: u32,
    /// Longest sync run, ms.
    pub max_run_ms: u64,
    /// Minutes between two polls of `GET /ingest/tasks`.
    pub task_poll_minutes: u32,
    /// Most Instagram posts refreshed per IG-tab session.
    pub refresh_per_session: u32,
}

/// The per-platform part of [`ExtensionConfig`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExtensionPlatforms {
    /// Instagram.
    pub instagram: ExtensionInstagram,
    /// X.
    pub twitter: ExtensionPlatform,
    /// Pinterest.
    pub pinterest: ExtensionPlatform,
}

/// Instagram's switches and pacing: X and Pinterest's, plus the REST
/// replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInstagram {
    /// Passive capture is on (kill switch).
    pub passive: bool,
    /// The REST replay is on (kill switch).
    pub replay: bool,
    /// The scroll is on (kill switch).
    pub scroll: bool,
    /// An incremental run stops at the first page boundary where this many
    /// known items came in a row.
    pub stop_after_known: u32,
    /// Pause between two replay pages, ms.
    pub replay_gap_ms: u32,
    /// Most replay pages per run.
    pub replay_max_pages: u32,
    /// Wait after each scroll step, ms.
    pub scroll_settle_ms: u32,
}

/// A platform's switches and pacing (X, Pinterest).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPlatform {
    /// Passive capture is on (kill switch).
    pub passive: bool,
    /// The scroll is on (kill switch).
    pub scroll: bool,
    /// An incremental run stops at the first page boundary where this many
    /// known items came in a row.
    pub stop_after_known: u32,
    /// Wait after each scroll step, ms.
    pub scroll_settle_ms: u32,
}

/// A capture mode that a kill switch covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    /// Items read while the user browses a saved listing.
    Passive,
    /// The Instagram REST replay.
    Replay,
    /// The scroll of a sync run.
    Scroll,
}

impl ExtensionConfig {
    /// Whether `mode` is on for `platform`. Only Instagram has a replay;
    /// the web and manual bookmarks have no extension capture at all.
    #[must_use]
    pub fn allows(&self, platform: Platform, mode: CaptureMode) -> bool {
        let platforms = &self.platforms;
        match (platform, mode) {
            (Platform::Instagram, CaptureMode::Passive) => platforms.instagram.passive,
            (Platform::Instagram, CaptureMode::Replay) => platforms.instagram.replay,
            (Platform::Instagram, CaptureMode::Scroll) => platforms.instagram.scroll,
            (Platform::Twitter, CaptureMode::Passive) => platforms.twitter.passive,
            (Platform::Twitter, CaptureMode::Scroll) => platforms.twitter.scroll,
            (Platform::Pinterest, CaptureMode::Passive) => platforms.pinterest.passive,
            (Platform::Pinterest, CaptureMode::Scroll) => platforms.pinterest.scroll,
            (Platform::Twitter | Platform::Pinterest, CaptureMode::Replay)
            | (Platform::Web | Platform::Manual, _) => false,
        }
    }
}

/// The flags as read at one time, with what the routes derive from them.
#[derive(Debug)]
pub struct Snapshot {
    flags: Flags,
    config: ExtensionConfig,
    min_version: ExtensionVersion,
    etag: ETag,
}

impl Snapshot {
    /// The snapshot of `flags`.
    #[must_use]
    pub fn new(flags: Flags) -> Self {
        let config = flags.config();
        Self {
            min_version: flags.min_version(),
            etag: ETag::for_document("extension.config", &config),
            config,
            flags,
        }
    }

    /// The effective flags.
    #[must_use]
    pub fn flags(&self) -> &Flags {
        &self.flags
    }

    /// The extension's configuration (C3).
    #[must_use]
    pub fn config(&self) -> &ExtensionConfig {
        &self.config
    }

    /// The oldest extension version the API serves.
    #[must_use]
    pub fn min_version(&self) -> ExtensionVersion {
        self.min_version
    }

    /// The ETag of [`Snapshot::config`]: it changes with any value.
    #[must_use]
    pub fn etag(&self) -> &ETag {
        &self.etag
    }
}

/// The flags, read from `feature_flags` at most once per time to live.
pub struct FlagCache {
    ttl: Duration,
    current: RwLock<Option<(Instant, Arc<Snapshot>)>>,
    reload: tokio::sync::Mutex<()>,
}

impl FlagCache {
    /// A cache that serves what it read for `ttl`.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            current: RwLock::new(None),
            reload: tokio::sync::Mutex::new(()),
        }
    }

    fn fresh(&self, now: Instant) -> Option<Arc<Snapshot>> {
        let current = self.current.read().unwrap_or_else(PoisonError::into_inner);
        current
            .as_ref()
            .filter(|(at, _)| now.saturating_duration_since(*at) < self.ttl)
            .map(|(_, snapshot)| Arc::clone(snapshot))
    }

    fn stale(&self) -> Option<Arc<Snapshot>> {
        let current = self.current.read().unwrap_or_else(PoisonError::into_inner);
        current.as_ref().map(|(_, snapshot)| Arc::clone(snapshot))
    }

    /// The flags: the cached snapshot while it is younger than the time to
    /// live, otherwise a new read of `feature_flags` (one at a time; the
    /// requests that wait for it share its result).
    ///
    /// # Errors
    ///
    /// The control database failed and no earlier snapshot exists. With an
    /// earlier one, it is served again and the failure logged.
    pub async fn get(&self, control: &Arc<ControlDb>) -> Result<Arc<Snapshot>, ApiError> {
        if let Some(snapshot) = self.fresh(Instant::now()) {
            return Ok(snapshot);
        }
        let _reloading = self.reload.lock().await;
        if let Some(snapshot) = self.fresh(Instant::now()) {
            return Ok(snapshot);
        }
        let control = Arc::clone(control);
        match blocking(move || control.read(rows::list)).await {
            Ok(rows) => {
                let snapshot = Arc::new(Snapshot::new(Flags::from_rows(&rows)));
                *self.current.write().unwrap_or_else(PoisonError::into_inner) =
                    Some((Instant::now(), Arc::clone(&snapshot)));
                Ok(snapshot)
            }
            Err(err) => match self.stale() {
                Some(snapshot) => {
                    tracing::warn!(error = %err, "reading the flags failed: serving the previous ones");
                    Ok(snapshot)
                }
                None => Err(err),
            },
        }
    }

    /// Forgets the cached snapshot: the next [`FlagCache::get`] reads the
    /// table.
    pub fn invalidate(&self) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn row(key: &str, value: &str) -> FlagRow {
        FlagRow {
            key: key.to_owned(),
            value_json: value.to_owned(),
            updated_at: 1,
        }
    }

    #[test]
    fn the_defaults_are_the_contract() {
        let config = Flags::default().config();
        assert_eq!(
            serde_json::to_value(&config).unwrap(),
            json!({
                "minVersion": "0.2.0",
                "platforms": {
                    "instagram": {
                        "passive": true, "replay": true, "scroll": true, "stopAfterKnown": 10,
                        "replayGapMs": 700, "replayMaxPages": 100, "scrollSettleMs": 650
                    },
                    "twitter": {
                        "passive": true, "scroll": true, "stopAfterKnown": 20, "scrollSettleMs": 750
                    },
                    "pinterest": {
                        "passive": true, "scroll": true, "stopAfterKnown": 25, "scrollSettleMs": 650
                    }
                },
                "maxSteps": 16000, "maxRunMs": 1_800_000, "taskPollMinutes": 5,
                "refreshPerSession": 200
            })
        );
    }

    #[test]
    fn every_flag_is_unique_has_a_valid_default_and_reaches_the_config() {
        let mut seen = std::collections::BTreeSet::new();
        for flag in FLAGS {
            assert!(seen.insert(flag.key), "{} twice", flag.key);
            assert!(flag.key.starts_with("extension."), "{}", flag.key);
            assert_eq!(
                flag.parse(&flag.default_value().to_json()),
                Ok(flag.default_value()),
                "{}",
                flag.key
            );
            if let FlagType::Int { default, min, max } = flag.kind {
                assert!((min..=max).contains(&default), "{}", flag.key);
                assert!(min >= 0 && max <= i64::from(u32::MAX), "{}", flag.key);
            }
            // Overriding the flag changes the config: every flag is used.
            let changed = match flag.kind {
                FlagType::Bool { default } => json!(!default),
                FlagType::Int { default, max, .. } => json!(if default < max {
                    default + 1
                } else {
                    default - 1
                }),
                FlagType::Version { .. } => json!("9.9.9"),
            };
            let flags = Flags::from_rows(&[row(flag.key, &changed.to_string())]);
            assert!(flags.is_overridden(flag.key));
            assert_ne!(
                flags.config(),
                Flags::default().config(),
                "{} does not reach the config",
                flag.key
            );
        }
        assert_eq!(seen.len(), 20);
    }

    #[test]
    fn values_are_checked_against_their_flag() {
        let switch = Flag::find(keys::TWITTER_SCROLL).unwrap();
        assert_eq!(switch.parse(&json!(false)), Ok(FlagValue::Bool(false)));
        for bad in [json!("false"), json!(0), json!(null), json!([true])] {
            assert!(switch.parse(&bad).is_err(), "{bad}");
        }
        let gap = Flag::find(keys::INSTAGRAM_REPLAY_GAP_MS).unwrap();
        assert_eq!(gap.parse(&json!(1_000)), Ok(FlagValue::Int(1_000)));
        for bad in [
            json!(249),
            json!(60_001),
            json!(700.5),
            json!("700"),
            json!(-1),
        ] {
            assert_eq!(
                gap.parse(&bad),
                Err("must be an integer from 250 to 60000".to_owned()),
                "{bad}"
            );
        }
        let min = Flag::find(keys::MIN_VERSION).unwrap();
        assert_eq!(
            min.parse(&json!("0.3.1")),
            Ok(FlagValue::Version(
                ExtensionVersion::parse("0.3.1").unwrap()
            ))
        );
        for bad in [json!("0.3.1-"), json!(3), json!(""), json!("v1")] {
            assert!(min.parse(&bad).is_err(), "{bad}");
        }
        assert!(Flag::find("extension.instagram.nope").is_none());
        assert!(
            Flag::find("extension.twitter.replay").is_none(),
            "X has no replay"
        );
    }

    #[test]
    fn stored_values_win_over_defaults_unless_they_do_not_fit() {
        let flags = Flags::from_rows(&[
            row(keys::INSTAGRAM_REPLAY, "false"),
            row(keys::TWITTER_STOP_AFTER_KNOWN, "40"),
            row(keys::MIN_VERSION, "\"0.3.0\""),
            // Ignored: unknown, the wrong type, out of bounds, not JSON.
            row("extension.instagram.turbo", "true"),
            row("capture", "true"),
            row(keys::PINTEREST_PASSIVE, "\"no\""),
            row(keys::MAX_STEPS, "0"),
            row(keys::INSTAGRAM_SCROLL, "fals"),
        ]);
        let config = flags.config();
        assert!(!config.platforms.instagram.replay);
        assert_eq!(config.platforms.twitter.stop_after_known, 40);
        assert_eq!(config.min_version, "0.3.0");
        assert_eq!(flags.min_version(), ExtensionVersion::parse("0.3").unwrap());
        assert!(config.platforms.pinterest.passive);
        assert_eq!(config.max_steps, 16_000);
        assert!(config.platforms.instagram.scroll);
        assert!(flags.is_overridden(keys::INSTAGRAM_REPLAY));
        assert!(!flags.is_overridden(keys::PINTEREST_PASSIVE));
        assert!(!flags.is_overridden(keys::MAX_STEPS));
    }

    #[test]
    fn kill_switches_answer_per_platform_and_mode() {
        let flags = Flags::from_rows(&[
            row(keys::INSTAGRAM_REPLAY, "false"),
            row(keys::PINTEREST_PASSIVE, "false"),
        ]);
        let config = flags.config();
        assert!(!config.allows(Platform::Instagram, CaptureMode::Replay));
        assert!(config.allows(Platform::Instagram, CaptureMode::Passive));
        assert!(config.allows(Platform::Instagram, CaptureMode::Scroll));
        assert!(!config.allows(Platform::Pinterest, CaptureMode::Passive));
        assert!(config.allows(Platform::Pinterest, CaptureMode::Scroll));
        assert!(config.allows(Platform::Twitter, CaptureMode::Passive));
        assert!(!config.allows(Platform::Twitter, CaptureMode::Replay));
        assert!(!config.allows(Platform::Web, CaptureMode::Passive));
        assert!(!config.allows(Platform::Manual, CaptureMode::Scroll));
    }

    #[test]
    fn the_etag_follows_the_values() {
        let defaults = Snapshot::new(Flags::default());
        assert_eq!(defaults.etag(), Snapshot::new(Flags::default()).etag());
        let changed = Snapshot::new(Flags::from_rows(&[row(keys::TWITTER_SCROLL, "false")]));
        assert_ne!(defaults.etag(), changed.etag());
        // A stored value equal to the default changes nothing.
        let same = Snapshot::new(Flags::from_rows(&[row(keys::TWITTER_SCROLL, "true")]));
        assert_eq!(defaults.etag(), same.etag());
    }
}
