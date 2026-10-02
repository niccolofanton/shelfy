//! The desktop settings that move to the web library (plan §4.2: "language
//! and asset preferences only"; OI-10).
//!
//! The desktop keeps them in the renderer's localStorage, which Chromium
//! stores in `<userData>/Local Storage/leveldb/` ([`crate::leveldb`]):
//!
//! | localStorage key | Web setting (`settings.key`) | Value |
//! |---|---|---|
//! | `app:language` | `language` | `"it"` or `"en"` |
//! | `download:assetTypes` | `archiveAssetTypes` | `{"thumbnail", "image", "video"}` booleans; a missing one is `true`, as on the desktop |
//!
//! A Chromium localStorage key is `_` + the page's origin + `\0` + the
//! script's key, and a key or value starts with an encoding byte: `1` for
//! Latin-1, `0` for UTF-16LE. The packaged app's origin is `file://`; a key
//! found there wins over the same key of another origin (a development
//! server's). Anything unreadable leaves the setting out: the web defaults
//! apply.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use shelfy_core::repo::settings::{ARCHIVE_ASSET_TYPES, ArchiveAssetTypes, LANGUAGE, Language};

use crate::leveldb;

/// Where the desktop's localStorage lives, under its userData directory.
pub const LOCAL_STORAGE_DIR: &str = "Local Storage/leveldb";
/// The packaged desktop app's origin.
pub const APP_ORIGIN: &str = "file://";

/// The desktop settings found, as the web stores them.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSettings {
    /// `found`, `missing` (no localStorage) or `unreadable`.
    pub source: String,
    /// The UI language.
    pub language: Option<Language>,
    /// The asset types to archive.
    pub archive_asset_types: Option<ArchiveAssetTypes>,
}

impl DesktopSettings {
    /// The `settings` rows of the bundle: key and JSON value, as the web
    /// writes them (`shelfy_core::repo::settings`).
    ///
    /// # Panics
    ///
    /// Never: the values always serialize.
    #[must_use]
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let mut rows = Vec::new();
        if let Some(language) = &self.language {
            rows.push((
                LANGUAGE,
                serde_json::to_string(language).expect("a language serializes"),
            ));
        }
        if let Some(types) = &self.archive_asset_types {
            rows.push((
                ARCHIVE_ASSET_TYPES,
                serde_json::to_string(types).expect("asset types serialize"),
            ));
        }
        rows
    }
}

/// The localStorage directory under the desktop userData directory `root`.
#[must_use]
pub fn local_storage(root: &Path) -> PathBuf {
    root.join(LOCAL_STORAGE_DIR)
}

/// Reads the desktop settings from the userData directory `root`, read-only.
#[must_use]
pub fn read(root: &Path) -> DesktopSettings {
    let dir = local_storage(root);
    if !dir.is_dir() {
        return DesktopSettings {
            source: "missing".to_owned(),
            ..DesktopSettings::default()
        };
    }
    let Ok(snapshot) = leveldb::read_dir(&dir) else {
        return DesktopSettings {
            source: "unreadable".to_owned(),
            ..DesktopSettings::default()
        };
    };
    let language = value_of(&snapshot, "app:language").and_then(|v| match v.trim() {
        "it" => Some(Language::It),
        "en" => Some(Language::En),
        _ => None,
    });
    let archive_asset_types = value_of(&snapshot, "download:assetTypes")
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| asset_types(&v));
    DesktopSettings {
        source: "found".to_owned(),
        language,
        archive_asset_types,
    }
}

/// The desktop's asset preferences, with the desktop's default (`true`) for
/// a missing type; `None` for anything but an object.
fn asset_types(value: &Value) -> Option<ArchiveAssetTypes> {
    let map = value.as_object()?;
    let get = |name: &str| map.get(name).and_then(Value::as_bool).unwrap_or(true);
    Some(ArchiveAssetTypes {
        thumbnail: get("thumbnail"),
        image: get("image"),
        video: get("video"),
    })
}

/// The value of the localStorage key `name`: the app origin's, else the
/// newest of any origin.
fn value_of(snapshot: &leveldb::Snapshot, name: &str) -> Option<String> {
    let mut best: Option<(bool, u64, String)> = None;
    for (key, value) in snapshot.iter() {
        let Some((origin, script_key)) = split_key(key) else {
            continue;
        };
        if decode(script_key).as_deref() != Some(name) {
            continue;
        }
        let Some(text) = decode(value) else {
            continue;
        };
        let app = origin == APP_ORIGIN.as_bytes();
        let sequence = snapshot.sequence(key).unwrap_or(0);
        let better = best
            .as_ref()
            .is_none_or(|(b_app, b_seq, _)| (app, sequence) > (*b_app, *b_seq));
        if better {
            best = Some((app, sequence, text));
        }
    }
    best.map(|(_, _, text)| text)
}

/// `_<origin>\0<script key>` → the origin and the encoded script key.
fn split_key(key: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = key.strip_prefix(b"_")?;
    let nul = rest.iter().position(|&b| b == 0)?;
    Some((&rest[..nul], &rest[nul + 1..]))
}

/// A Chromium localStorage string: `1` + Latin-1, or `0` + UTF-16LE.
fn decode(bytes: &[u8]) -> Option<String> {
    let (&encoding, rest) = bytes.split_first()?;
    match encoding {
        1 => Some(rest.iter().map(|&b| char::from(b)).collect()),
        0 => {
            if rest.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_le_bytes(pair))
                .collect();
            String::from_utf16(&units).ok()
        }
        _ => None,
    }
}

/// Builders of a desktop localStorage, for tests.
#[doc(hidden)]
pub mod fixture {
    use std::path::Path;

    use super::LOCAL_STORAGE_DIR;
    use crate::leveldb::fixture::{batch, log};

    /// A localStorage key of `origin`, Latin-1 encoded.
    #[must_use]
    pub fn key(origin: &str, name: &str) -> Vec<u8> {
        let mut out = vec![b'_'];
        out.extend_from_slice(origin.as_bytes());
        out.push(0);
        out.push(1);
        out.extend_from_slice(name.as_bytes());
        out
    }

    /// A value, UTF-16LE encoded as Chromium writes non-Latin-1 strings.
    #[must_use]
    pub fn utf16(value: &str) -> Vec<u8> {
        let mut out = vec![0];
        for unit in value.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    }

    /// A value, Latin-1 encoded.
    #[must_use]
    pub fn latin1(value: &str) -> Vec<u8> {
        let mut out = vec![1];
        out.extend_from_slice(value.as_bytes());
        out
    }

    /// Writes a localStorage with these `(key, value)` entries under the
    /// userData directory `root`.
    ///
    /// # Panics
    ///
    /// When the files cannot be written.
    pub fn write(root: &Path, entries: &[(Vec<u8>, Vec<u8>)]) {
        let dir = root.join(LOCAL_STORAGE_DIR);
        std::fs::create_dir_all(&dir).expect("localStorage directory");
        let ops: Vec<(&[u8], Option<&[u8]>)> = entries
            .iter()
            .map(|(k, v)| (k.as_slice(), Some(v.as_slice())))
            .collect();
        std::fs::write(dir.join("000003.log"), log(&[batch(1, &ops)])).expect("log");
        std::fs::write(dir.join("CURRENT"), b"MANIFEST-000001\n").expect("CURRENT");
        std::fs::write(dir.join("LOCK"), b"").expect("LOCK");
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{key, latin1, utf16, write};
    use super::*;

    #[test]
    fn the_language_and_asset_types_are_read_and_others_ignored() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            &[
                (key("file://", "app:language"), latin1("it")),
                (
                    key("file://", "download:assetTypes"),
                    utf16(r#"{"image":false,"video":false}"#),
                ),
                (key("http://localhost:5173", "app:language"), latin1("en")),
                (key("file://", "app:viewMode"), latin1("grid")),
                (b"VERSION".to_vec(), b"1".to_vec()),
            ],
        );
        let settings = read(dir.path());
        assert_eq!(settings.source, "found");
        assert_eq!(settings.language, Some(Language::It), "the app origin wins");
        assert_eq!(
            settings.archive_asset_types,
            Some(ArchiveAssetTypes {
                thumbnail: true,
                image: false,
                video: false
            })
        );
        assert_eq!(
            settings.rows(),
            [
                (LANGUAGE, r#""it""#.to_owned()),
                (
                    ARCHIVE_ASSET_TYPES,
                    r#"{"thumbnail":true,"image":false,"video":false}"#.to_owned()
                )
            ]
        );
    }

    #[test]
    fn missing_or_invalid_values_leave_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()).source, "missing");
        write(
            dir.path(),
            &[
                (key("http://localhost:5173", "app:language"), latin1("en")),
                (key("file://", "download:assetTypes"), latin1("[1,2]")),
            ],
        );
        let settings = read(dir.path());
        assert_eq!(
            settings.language,
            Some(Language::En),
            "another origin when the app has none"
        );
        assert_eq!(settings.archive_asset_types, None);
        write(
            dir.path(),
            &[(key("file://", "app:language"), latin1("fr"))],
        );
        assert_eq!(read(dir.path()).language, None);
        assert_eq!(decode(&[0, 0x41]), None, "odd UTF-16");
        assert_eq!(decode(&[2, b'a']), None, "unknown encoding");
    }
}
