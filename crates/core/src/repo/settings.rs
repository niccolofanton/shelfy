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
//!
//! Reading is lenient: a row whose value does not parse (written by another
//! version) reads as the default, and unknown keys are ignored. Writing is
//! strict: only the typed values above.

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};

use super::{RepoError, Result};

/// Key of the UI language.
pub const LANGUAGE: &str = "language";
/// Key of the asset types to archive.
pub const ARCHIVE_ASSET_TYPES: &str = "archiveAssetTypes";

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

/// Every setting, with defaults filled in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// The UI language; `None` until the user picks one.
    pub language: Option<Language>,
    /// The asset types to archive.
    pub archive_asset_types: ArchiveAssetTypes,
}

/// A change to the settings: `None` leaves a setting as it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingsChange {
    /// The new UI language.
    pub language: Option<Language>,
    /// The new asset types.
    pub archive_asset_types: Option<ArchiveAssetTypes>,
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
    let archive_asset_types = value(conn, ARCHIVE_ASSET_TYPES)?
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    Ok(Settings {
        language,
        archive_asset_types,
    })
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
