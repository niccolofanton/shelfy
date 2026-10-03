//! Settings (plan §2.7 `settings`, §4.2): the preferences the desktop kept in
//! localStorage, persisted per library as one JSON value per key.
//!
//! Only these keys exist; the account API reads and writes them
//! (`GET,PUT /me/settings`), and the migration maps the desktop's language
//! and asset preferences onto them (plan §4.2: "language and asset
//! preferences only").
//!
//! | Key | Value | Default | Desktop source |
//! |---|---|---|---|
//! | [`LANGUAGE`] | `"it"` or `"en"` | none: the client picks | localStorage `app:language` |
//! | [`ARCHIVE_ASSET_TYPES`] | `{"thumbnail": bool, "image": bool, "video": bool}` | all `true` | localStorage `download:assetTypes` |
//! | [`AI_PROVIDERS`] | the user's BYOK providers (no keys) | `[]` | P3 (new) |
//! | [`AI_ROUTING`] | task → provider id overrides | `{}` | P3 (new) |
//! | [`AI_CONCURRENCY`] | BYOK calls in flight, 1–8 | `4` | P3 (new) |
//! | [`AI_SUGGESTIONS`] | suggestion chips on | `true` | P3 (new) |
//! | [`AI_VISION_QC`] | vision screenshot QC on | `false` | P3 (new) |
//! | [`AI_AUTO_ANALYZE_WEBSITES`] | catalog websites on capture | `false` | P3 (new) |
//! | [`AI_DICTATION_INTERIM`] | live interim dictation text | `false` | P3 (new) |
//!
//! Reading is lenient: a row whose value does not parse (written by another
//! version) reads as the default, and unknown keys are ignored. Writing is
//! strict: only the typed values above.
//!
//! The AI keys (P3) hold the settings the web app shows and the service reads
//! ([`AiSettings`]). BYOK provider keys never live here: they are sealed in
//! the control database (P3-02). The operator provider is defined by the
//! server's environment, not by these settings.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};

use super::{RepoError, Result};

/// Key of the UI language.
pub const LANGUAGE: &str = "language";
/// Key of the asset types to archive.
pub const ARCHIVE_ASSET_TYPES: &str = "archiveAssetTypes";
/// Key of the user's BYOK providers (descriptors only, never keys).
pub const AI_PROVIDERS: &str = "aiProviders";
/// Key of the per-task provider overrides.
pub const AI_ROUTING: &str = "aiRouting";
/// Key of the BYOK concurrency (calls in flight per user).
pub const AI_CONCURRENCY: &str = "aiConcurrency";
/// Key of the suggestion-chips toggle.
pub const AI_SUGGESTIONS: &str = "aiSuggestions";
/// Key of the vision screenshot-QC toggle.
pub const AI_VISION_QC: &str = "aiVisionQc";
/// Key of the catalog-websites-on-capture toggle.
pub const AI_AUTO_ANALYZE_WEBSITES: &str = "aiAutoAnalyzeWebsites";
/// Key of the live interim dictation toggle.
pub const AI_DICTATION_INTERIM: &str = "aiDictationInterim";

/// Default of [`AI_CONCURRENCY`].
pub const AI_CONCURRENCY_DEFAULT: u8 = 4;
/// Smallest accepted [`AI_CONCURRENCY`].
pub const AI_CONCURRENCY_MIN: u8 = 1;
/// Largest accepted [`AI_CONCURRENCY`].
pub const AI_CONCURRENCY_MAX: u8 = 8;

/// A UI language of the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// Italian.
    It,
    /// English.
    En,
}

/// Which assets of a post are archived ("Asset types to download" on the
/// desktop). The keys are the desktop's, so the migration copies them as is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveAssetTypes {
    /// Covers and video posters.
    pub thumbnail: bool,
    /// Image slides.
    pub image: bool,
    /// Videos.
    pub video: bool,
}

impl Default for ArchiveAssetTypes {
    fn default() -> Self {
        Self {
            thumbnail: true,
            image: true,
            video: true,
        }
    }
}

/// The protocol a BYOK provider speaks (the non-secret descriptor; P3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProviderKind {
    /// An OpenAI-compatible server (OpenAI, Gemini, OpenRouter, …).
    OpenaiCompatible,
    /// The Anthropic Messages API.
    Anthropic,
}

/// The model ids a provider uses per task (the non-secret descriptor; P3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiModels {
    /// The cataloging and QC model (vision).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    /// The chat model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    /// The suggestion model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggest: Option<String>,
    /// The embedding model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed: Option<String>,
    /// The transcription model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

/// Optional prices in US dollars per million tokens (no keys).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiPrices {
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
}

/// The consent applying to this exact provider endpoint and credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderConsent {
    pub version: String,
    pub accepted_at: i64,
}

/// A probe outcome stores a stable error kind, never remote text or answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProbeResult {
    pub ok: bool,
    pub skipped: bool,
    pub error: Option<String>,
}

/// Synthetic onboarding checks; library content is never sent by the probe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderTest {
    pub tested_at: i64,
    pub models: AiProbeResult,
    pub text: AiProbeResult,
    pub vision: AiProbeResult,
    pub schema: AiProbeResult,
}

/// One BYOK provider as the user configured it, without its key (P3). The
/// key is sealed in the control database; the service looks it up there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProvider {
    /// A stable id the user chose.
    pub id: String,
    /// The protocol.
    pub kind: AiProviderKind,
    /// A display name.
    pub label: String,
    /// The base URL, used verbatim.
    pub base_url: String,
    /// The models per task.
    #[serde(default)]
    pub models: AiModels,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prices: Option<AiPrices>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<AiProviderConsent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<AiProviderTest>,
}

/// Per-task provider overrides: a task name (`catalog`, `chat`, …) to a
/// provider id (`operator` or a BYOK id). A task not named uses the default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AiRouting(pub BTreeMap<String, String>);

impl AiRouting {
    /// The provider id chosen for `task`, if any.
    #[must_use]
    pub fn for_task(&self, task: &str) -> Option<&str> {
        self.0.get(task).map(String::as_str)
    }
}

/// The AI settings (P3), with defaults filled in.
#[derive(Clone, Debug, PartialEq)]
pub struct AiSettings {
    /// The user's BYOK providers (no keys).
    pub providers: Vec<AiProvider>,
    /// Per-task provider overrides.
    pub routing: AiRouting,
    /// BYOK calls in flight for this user (1–8).
    pub concurrency: u8,
    /// Whether suggestion chips are on.
    pub suggestions: bool,
    /// Whether vision screenshot QC is on.
    pub vision_qc: bool,
    /// Whether websites are catalogued automatically on capture.
    pub auto_analyze_websites: bool,
    /// Whether live interim dictation text is shown.
    pub dictation_interim: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            routing: AiRouting::default(),
            concurrency: AI_CONCURRENCY_DEFAULT,
            suggestions: true,
            vision_qc: false,
            auto_analyze_websites: false,
            dictation_interim: false,
        }
    }
}

/// Every setting, with defaults filled in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// The UI language; `None` until the user picks one.
    pub language: Option<Language>,
    /// The asset types to archive.
    pub archive_asset_types: ArchiveAssetTypes,
    /// The AI settings (P3).
    pub ai: AiSettings,
}

/// A change to the settings: `None` leaves a setting as it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsChange {
    /// The new UI language.
    pub language: Option<Language>,
    /// The new asset types.
    pub archive_asset_types: Option<ArchiveAssetTypes>,
    /// The new BYOK providers.
    pub ai_providers: Option<Vec<AiProvider>>,
    /// The new per-task routing.
    pub ai_routing: Option<AiRouting>,
    /// The new BYOK concurrency (clamped to 1–8).
    pub ai_concurrency: Option<u8>,
    /// The new suggestion-chips toggle.
    pub ai_suggestions: Option<bool>,
    /// The new vision-QC toggle.
    pub ai_vision_qc: Option<bool>,
    /// The new catalog-websites toggle.
    pub ai_auto_analyze_websites: Option<bool>,
    /// The new interim-dictation toggle.
    pub ai_dictation_interim: Option<bool>,
}

/// The stored JSON of `key`, if any.
fn value(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT value_json FROM settings WHERE key = ?1",
        [key],
        |row| row.get(0),
    )
    .optional()
    .map_err(RepoError::from)
}

/// The settings, defaults filled in.
///
/// # Errors
///
/// The query failed.
pub fn read(conn: &Connection) -> Result<Settings> {
    let language = value(conn, LANGUAGE)?.and_then(|json| serde_json::from_str(&json).ok());
    let archive_asset_types = stored_archive_asset_types(conn)?.unwrap_or_default();
    Ok(Settings {
        language,
        archive_asset_types,
        ai: read_ai(conn)?,
    })
}

/// The AI settings, defaults filled in and values that do not parse ignored.
fn read_ai(conn: &Connection) -> Result<AiSettings> {
    let parsed = |key| -> Result<Option<serde_json::Value>> {
        Ok(value(conn, key)?.and_then(|json| serde_json::from_str(&json).ok()))
    };
    let d = AiSettings::default();
    let bool_of = |v: Option<serde_json::Value>, default: bool| {
        v.and_then(|v| v.as_bool()).unwrap_or(default)
    };
    let concurrency = parsed(AI_CONCURRENCY)?
        .and_then(|v| v.as_u64())
        .and_then(|n| u8::try_from(n).ok())
        .map(|n| n.clamp(AI_CONCURRENCY_MIN, AI_CONCURRENCY_MAX))
        .unwrap_or(d.concurrency);
    Ok(AiSettings {
        providers: parsed(AI_PROVIDERS)?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(d.providers),
        routing: parsed(AI_ROUTING)?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(d.routing),
        concurrency,
        suggestions: bool_of(parsed(AI_SUGGESTIONS)?, d.suggestions),
        vision_qc: bool_of(parsed(AI_VISION_QC)?, d.vision_qc),
        auto_analyze_websites: bool_of(parsed(AI_AUTO_ANALYZE_WEBSITES)?, d.auto_analyze_websites),
        dictation_interim: bool_of(parsed(AI_DICTATION_INTERIM)?, d.dictation_interim),
    })
}

/// The asset types the library has stored, if any (and they parse): `None`
/// when the default applies. The migration uses it to tell the library's own
/// choice from the default, because a library's settings win over the
/// desktop's.
///
/// # Errors
///
/// The query failed.
pub fn stored_archive_asset_types(conn: &Connection) -> Result<Option<ArchiveAssetTypes>> {
    Ok(value(conn, ARCHIVE_ASSET_TYPES)?.and_then(|json| serde_json::from_str(&json).ok()))
}

/// Stores `value` under `key` at `now`.
fn put(conn: &Connection, key: &str, value: &impl Serialize, now: i64) -> Result<()> {
    let json = serde_json::to_string(value).expect("settings always serialize");
    conn.execute(
        "INSERT INTO settings (key, value_json, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, \
         updated_at = excluded.updated_at WHERE value_json IS NOT excluded.value_json",
        params![key, json, now],
    )?;
    Ok(())
}

/// Applies `change` at `now` and returns the settings after it. A setting
/// that already has the value is not written again.
///
/// # Errors
///
/// A query failed.
pub fn update(conn: &Connection, change: &SettingsChange, now: i64) -> Result<Settings> {
    if let Some(language) = change.language {
        put(conn, LANGUAGE, &language, now)?;
    }
    if let Some(types) = change.archive_asset_types {
        put(conn, ARCHIVE_ASSET_TYPES, &types, now)?;
    }
    if let Some(providers) = &change.ai_providers {
        put(conn, AI_PROVIDERS, providers, now)?;
    }
    if let Some(routing) = &change.ai_routing {
        put(conn, AI_ROUTING, routing, now)?;
    }
    if let Some(concurrency) = change.ai_concurrency {
        put(
            conn,
            AI_CONCURRENCY,
            &concurrency.clamp(AI_CONCURRENCY_MIN, AI_CONCURRENCY_MAX),
            now,
        )?;
    }
    if let Some(on) = change.ai_suggestions {
        put(conn, AI_SUGGESTIONS, &on, now)?;
    }
    if let Some(on) = change.ai_vision_qc {
        put(conn, AI_VISION_QC, &on, now)?;
    }
    if let Some(on) = change.ai_auto_analyze_websites {
        put(conn, AI_AUTO_ANALYZE_WEBSITES, &on, now)?;
    }
    if let Some(on) = change.ai_dictation_interim {
        put(conn, AI_DICTATION_INTERIM, &on, now)?;
    }
    read(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{self, Kind};

    fn library() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn, Kind::Library).unwrap();
        conn
    }

    #[test]
    fn defaults_until_set_then_the_stored_values() {
        let conn = library();
        assert_eq!(read(&conn).unwrap(), Settings::default());
        assert_eq!(
            Settings::default().archive_asset_types,
            ArchiveAssetTypes {
                thumbnail: true,
                image: true,
                video: true
            }
        );

        let types = ArchiveAssetTypes {
            thumbnail: true,
            image: false,
            video: false,
        };
        let change = SettingsChange {
            language: Some(Language::En),
            archive_asset_types: Some(types),
            ..SettingsChange::default()
        };
        let after = update(&conn, &change, 1_000).unwrap();
        assert_eq!(after.language, Some(Language::En));
        assert_eq!(after.archive_asset_types, types);
        assert_eq!(read(&conn).unwrap(), after);

        // The stored form is the desktop's.
        let stored: String = conn
            .query_row(
                "SELECT value_json FROM settings WHERE key = 'archiveAssetTypes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, r#"{"thumbnail":true,"image":false,"video":false}"#);

        // A change of one setting keeps the other; an unchanged value is not
        // written again.
        let only_language = SettingsChange {
            language: Some(Language::It),
            archive_asset_types: None,
            ..SettingsChange::default()
        };
        let after = update(&conn, &only_language, 2_000).unwrap();
        assert_eq!(after.archive_asset_types, types);
        update(&conn, &only_language, 3_000).unwrap();
        let updated_at: i64 = conn
            .query_row(
                "SELECT updated_at FROM settings WHERE key = 'language'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(updated_at, 2_000);
    }

    #[test]
    fn ai_settings_round_trip_and_clamp() {
        let conn = library();
        assert_eq!(read(&conn).unwrap().ai, AiSettings::default());

        let providers = vec![AiProvider {
            prices: None,
            consent: None,
            test: None,
            id: "openai".into(),
            kind: AiProviderKind::OpenaiCompatible,
            label: "OpenAI".into(),
            base_url: "https://api.openai.com/v1".into(),
            models: AiModels {
                catalog: Some("gpt-x".into()),
                ..AiModels::default()
            },
        }];
        let mut routing = BTreeMap::new();
        routing.insert("catalog".to_owned(), "openai".to_owned());
        let change = SettingsChange {
            ai_providers: Some(providers.clone()),
            ai_routing: Some(AiRouting(routing)),
            ai_concurrency: Some(50), // clamped to 8
            ai_suggestions: Some(false),
            ai_vision_qc: Some(true),
            ..SettingsChange::default()
        };
        let after = update(&conn, &change, 1_000).unwrap();
        assert_eq!(after.ai.providers, providers);
        assert_eq!(after.ai.routing.for_task("catalog"), Some("openai"));
        assert_eq!(after.ai.concurrency, 8);
        assert!(!after.ai.suggestions);
        assert!(after.ai.vision_qc);
        // Untouched toggles keep their defaults.
        assert!(!after.ai.auto_analyze_websites);
        assert_eq!(read(&conn).unwrap(), after);
    }

    #[test]
    fn values_that_do_not_parse_read_as_defaults() {
        let conn = library();
        conn.execute_batch(
            "INSERT INTO settings VALUES ('language', '\"fr\"', 1);
             INSERT INTO settings VALUES ('archiveAssetTypes', '{\"thumbnail\":false}', 1);
             INSERT INTO settings VALUES ('theme', '\"dark\"', 1);",
        )
        .unwrap();
        assert_eq!(read(&conn).unwrap(), Settings::default());
    }
}
