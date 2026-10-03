//! `uploads` (plan §2.6, §2.9): resumable tus uploads, and the purposes they
//! serve.
//!
//! A row tracks one upload of one user: its purpose, its declared `length`,
//! the bytes received so far (`upload_offset`), and `meta_json`
//! ([`UploadMeta`]). The bytes live in `<data>/work/uploads/` ([`file_path`]).
//! An upload goes through three states:
//!
//! | State | What it means | Kept until |
//! |---|---|---|
//! | unfinished | bytes are arriving; `HEAD` tells where to resume | `expires_at`: 24 h after creation |
//! | complete | every byte arrived and the purpose's checks passed; the server recorded the SHA-256 and the type it found | its purpose's [`UploadPurpose::keep_complete`] after completion |
//! | consumed | a consumer claimed it ([`claim`]): a second claim answers `upload_consumed` | [`CONSUMED_RETENTION`] after the claim; the consumer removes the bytes when it is done ([`discard`]) |
//!
//! **Purposes.** [`UploadPurpose`] is the registry: each purpose says who may
//! upload it, how large it may be, whether the client declares the SHA-256,
//! what the bytes must be, and how long a complete upload waits. The routes
//! ([`crate::routes::uploads`]) and the housekeeping
//! ([`crate::migrations::housekeeping`]) read everything from it, so a new
//! purpose is one line in the `purposes!` table below and nothing else. The
//! migration (T9, P1-19) uses `migration-object` and `migration-db`; the web
//! app (P4-08) `bookmark-original`, `bookmark-preview` and `import`; P2-14
//! adds the extension's `archive-object`.
//!
//! **Consumers.** Migration uploads are consumed by the install, which reads
//! them in place and deletes them ([`crate::migrations::install`]). Every
//! other purpose is consumed once, through [`claim`], [`release`] and
//! [`discard`] (and their async forms in [`crate::routes::uploads`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde::{Deserialize, Serialize};
use shelfy_core::repo::{RepoError, Result};
use shelfy_media::kind::KindSet;
use shelfy_media::store::IngestLimits;
use shelfy_media::{Digest, MediaKind};

use super::conflict_on_unique;
use crate::auth::bearer::{Scope, ScopeSet};

const MIB: u64 = 1024 * 1024;
const DAY: Duration = Duration::from_secs(86_400);
const WEEK: Duration = Duration::from_secs(7 * 86_400);

/// Largest migration object: a kept video (§2.12).
pub const MAX_OBJECT_BYTES: u64 = IngestLimits::VIDEO.max_bytes;
/// Largest migration database.
pub const MAX_DATABASE_BYTES: u64 = 4 * 1024 * MIB;
/// Largest bookmark original (§2.13: manual uploads up to 200 MiB).
pub const MAX_BOOKMARK_BYTES: u64 = IngestLimits::UPLOAD.max_bytes;
/// Largest bookmark preview, the image the client renders for a video or a
/// PDF (§1.2 #16).
pub const MAX_PREVIEW_BYTES: u64 = 2 * MIB;
/// The images a bookmark preview may be: those the server decodes, since it
/// checks the preview by decoding it (§2.13). AVIF is stored but not decoded.
pub const PREVIEW_KINDS: KindSet = KindSet::NONE
    .with(MediaKind::Jpeg)
    .with(MediaKind::Png)
    .with(MediaKind::Gif)
    .with(MediaKind::Webp);
/// How long a consumed upload's row stays, so that using it again answers
/// `upload_consumed` and not 404, and how long its bytes may wait for a
/// consumer that never called [`discard`] (a crash).
pub const CONSUMED_RETENTION: Duration = WEEK;
/// Longest kept file name of an upload, in bytes.
pub const MAX_FILENAME_BYTES: usize = 255;

/// Who may create uploads of a purpose and work on them afterwards (resume,
/// terminate): a signed-in session, API tokens with some scopes, or both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Uploaders {
    /// A signed-in session of the web app, which follows the CSRF rules.
    pub session: bool,
    /// API tokens holding one of these scopes.
    pub scopes: ScopeSet,
}

impl Uploaders {
    /// API tokens with one of `scopes`; no session.
    #[must_use]
    pub const fn tokens(scopes: ScopeSet) -> Self {
        Self {
            session: false,
            scopes,
        }
    }

    /// A signed-in session, or an API token with one of `scopes`.
    #[must_use]
    pub const fn session_or(scopes: ScopeSet) -> Self {
        Self {
            session: true,
            scopes,
        }
    }
}

/// The migration CLI: a `migrate` token.
const MIGRATE: Uploaders = Uploaders::tokens(ScopeSet::one(Scope::Migrate));
/// The web app: a session, or an `uploads` token.
const WEB: Uploaders = Uploaders::session_or(ScopeSet::one(Scope::Uploads));

/// The largest upload of a purpose: its `Upload-Length` at creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaxBytes {
    /// A fixed number of bytes.
    Fixed(u64),
    /// `SHELFY_IMPORT_MAX_GB` ([`crate::config::Config::import_max_bytes`]).
    ImportSetting,
}

/// Whether the client declares the content's SHA-256 (`sha256` in
/// `Upload-Metadata`, 64 lowercase hex digits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashRule {
    /// Required, and checked when the last byte arrives.
    Required,
    /// Optional (PG19: a browser cannot hash 200 MB cheaply): checked when
    /// declared; computed and recorded on completion either way.
    Optional,
}

/// What the bytes of an upload must be, checked from their first bytes once
/// the last one arrived. A refused upload is deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    /// A media type of the set that the client declares at creation (`ext`,
    /// the store's extension): the sniffed type must be the declared one.
    DeclaredMedia(KindSet),
    /// A media type of the set, sniffed (§7.1). What the client says about
    /// the type (`ext`, `filetype`) decides nothing.
    Media(KindSet),
    /// A SQLite database file.
    Sqlite,
    /// A JSON document (an object or an array, UTF-8, after an optional byte
    /// order mark and whitespace) or a zip archive (a local file header
    /// first).
    JsonOrZip,
}

/// Why [`Content::check`] refused the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentRefusal {
    /// They are not of the type the client declared (or declared none).
    NotDeclared,
    /// They are of no type the purpose takes.
    Unsupported,
}

/// What the bytes of a complete upload are, as its purpose's check found
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// A media type of the store's allowlist.
    Media(MediaKind),
    /// A SQLite database.
    Sqlite,
    /// A JSON document.
    Json,
    /// A zip archive.
    Zip,
}

impl Found {
    /// The type as [`UploadMeta::ext`] records it: the media type's extension,
    /// `json` or `zip`; nothing for a database.
    #[must_use]
    pub const fn ext(self) -> Option<&'static str> {
        match self {
            Self::Media(kind) => Some(kind.ext()),
            Self::Sqlite => None,
            Self::Json => Some("json"),
            Self::Zip => Some("zip"),
        }
    }
}

/// The first bytes of every SQLite database file.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
/// The first bytes of a zip archive that starts with a file.
const ZIP_MAGIC: &[u8; 4] = b"PK\x03\x04";
/// The UTF-8 byte order mark.
const UTF8_BOM: &[u8; 3] = b"\xEF\xBB\xBF";

impl Content {
    /// How many leading bytes [`Content::check`] looks at: enough for a JSON
    /// document's leading whitespace.
    pub const HEAD_LEN: usize = 1024;

    /// What bytes starting with `head` are, if this rule takes them;
    /// `declared` is the `ext` the client declared at creation.
    ///
    /// # Errors
    ///
    /// [`ContentRefusal`]: the bytes are not what the rule takes.
    pub fn check(
        self,
        declared: Option<&str>,
        head: &[u8],
    ) -> std::result::Result<Found, ContentRefusal> {
        match self {
            Self::DeclaredMedia(kinds) => {
                let declared = declared.and_then(MediaKind::from_ext);
                match MediaKind::sniff(head) {
                    Some(kind) if Some(kind) == declared && kinds.contains(kind) => {
                        Ok(Found::Media(kind))
                    }
                    _ => Err(ContentRefusal::NotDeclared),
                }
            }
            Self::Media(kinds) => match MediaKind::sniff(head) {
                Some(kind) if kinds.contains(kind) => Ok(Found::Media(kind)),
                _ => Err(ContentRefusal::Unsupported),
            },
            Self::Sqlite if head.starts_with(SQLITE_MAGIC) => Ok(Found::Sqlite),
            Self::Sqlite => Err(ContentRefusal::NotDeclared),
            Self::JsonOrZip => {
                if head.starts_with(ZIP_MAGIC) {
                    return Ok(Found::Zip);
                }
                let text = head.strip_prefix(UTF8_BOM.as_slice()).unwrap_or(head);
                let first = text
                    .iter()
                    .find(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
                match first {
                    Some(b'{' | b'[') => Ok(Found::Json),
                    _ => Err(ContentRefusal::Unsupported),
                }
            }
        }
    }
}

/// What an upload is for (`uploads.purpose`), and the rules that come with
/// it. The registry is the `purposes!` table: [`UploadPurpose::ALL`] and one
/// constant per purpose. Two purposes are equal when their names are.
#[derive(Clone, Copy, Debug)]
pub struct UploadPurpose {
    name: &'static str,
    /// Who may create uploads of this purpose and work on them.
    pub uploaders: Uploaders,
    /// The largest one.
    pub max_bytes: MaxBytes,
    /// Whether the client must declare the SHA-256.
    pub sha256: HashRule,
    /// What the bytes must be.
    pub content: Content,
    /// How long a complete upload waits for its consumer before the
    /// housekeeping deletes it.
    pub keep_complete: Duration,
    /// Whether the bytes count against the storage quota once used: a new
    /// upload that could not fit is refused at once
    /// ([`crate::routes::uploads`], P4-07). The consumer reserves the quota
    /// for real when it stores the bytes.
    pub quota: bool,
    /// Whether its live uploads count toward the user's staging cap
    /// ([`crate::routes::uploads::max_staged_bytes`]): every purpose but the
    /// migration's, whose install bounds them.
    pub staged: bool,
}

/// Registers the purposes, one line each: a constant on [`UploadPurpose`],
/// and its place in [`UploadPurpose::ALL`].
macro_rules! purposes {
    ($($(#[$doc:meta])* $konst:ident { $($field:ident: $value:expr),* $(,)? })*) => {
        impl UploadPurpose {
            $($(#[$doc])* pub const $konst: Self = Self { $($field: $value),* };)*

            /// Every purpose, in the order of registration.
            pub const ALL: &'static [Self] = &[$(Self::$konst),*];
        }
    };
}

/// The registry. Adding a purpose is one line here: the routes and the
/// housekeeping read everything else from it. A purpose that a consumer
/// claims ([`claim`]) keeps complete uploads a day; the migration's wait a
/// week, the life of a `migrate` token.
mod registry {
    use super::Content::{DeclaredMedia, JsonOrZip, Media, Sqlite};
    use super::HashRule::{Optional, Required};
    use super::MaxBytes::{Fixed, ImportSetting};
    use super::{
        DAY, KindSet, MAX_BOOKMARK_BYTES, MAX_DATABASE_BYTES, MAX_OBJECT_BYTES, MAX_PREVIEW_BYTES,
        MIGRATE, PREVIEW_KINDS, UploadPurpose, WEB, WEEK,
    };

    purposes! {
        /// One media object of a migration bundle (T9): a `migrate` token, the hash and the type declared.
        MIGRATION_OBJECT { name: "migration-object", uploaders: MIGRATE, max_bytes: Fixed(MAX_OBJECT_BYTES), sha256: Required, content: DeclaredMedia(KindSet::ALL), keep_complete: WEEK, quota: false, staged: false }
        /// The database of a migration bundle (T9).
        MIGRATION_DB { name: "migration-db", uploaders: MIGRATE, max_bytes: Fixed(MAX_DATABASE_BYTES), sha256: Required, content: Sqlite, keep_complete: WEEK, quota: false, staged: false }
        /// The original file of a manual bookmark (P4-18): any type of the store's allowlist, sniffed.
        BOOKMARK_ORIGINAL { name: "bookmark-original", uploaders: WEB, max_bytes: Fixed(MAX_BOOKMARK_BYTES), sha256: Optional, content: Media(KindSet::ALL), keep_complete: DAY, quota: true, staged: true }
        /// The client's preview image of a video or PDF bookmark (P4-18).
        BOOKMARK_PREVIEW { name: "bookmark-preview", uploaders: WEB, max_bytes: Fixed(MAX_PREVIEW_BYTES), sha256: Optional, content: Media(PREVIEW_KINDS), keep_complete: DAY, quota: true, staged: true }
        /// A file to import (P4-10, P4-19): a JSON export or a zip bundle.
        IMPORT { name: "import", uploaders: WEB, max_bytes: ImportSetting, sha256: Optional, content: JsonOrZip, keep_complete: DAY, quota: false, staged: true }
    }
}

impl UploadPurpose {
    /// The stored value, also the `purpose` of the tus `Upload-Metadata`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.name
    }

    /// The purpose named `value`; `None` for a name this build does not know.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.name == value)
    }

    /// The largest upload, in bytes, with `import_max_bytes` the configured
    /// `SHELFY_IMPORT_MAX_GB`.
    #[must_use]
    pub const fn byte_limit(self, import_max_bytes: u64) -> u64 {
        match self.max_bytes {
            MaxBytes::Fixed(bytes) => bytes,
            MaxBytes::ImportSetting => import_max_bytes,
        }
    }

    /// The scopes of every purpose's uploaders: the tokens the upload routes
    /// take ([`crate::routes::TOKEN_ROUTES`]).
    #[must_use]
    pub const fn token_scopes() -> ScopeSet {
        let mut scopes = ScopeSet::NONE;
        let mut i = 0;
        while i < Self::ALL.len() {
            scopes = scopes.union(Self::ALL[i].uploaders.scopes);
            i += 1;
        }
        scopes
    }

    /// Whether some purpose takes a session: then the upload routes do.
    #[must_use]
    pub const fn any_takes_sessions() -> bool {
        let mut i = 0;
        while i < Self::ALL.len() {
            if Self::ALL[i].uploaders.session {
                return true;
            }
            i += 1;
        }
        false
    }

    /// The longest wait of a complete upload: what an upload of a purpose
    /// this build does not know gets (a rollback after a newer build).
    #[must_use]
    pub fn longest_keep() -> Duration {
        Self::ALL
            .iter()
            .map(|p| p.keep_complete)
            .max()
            .unwrap_or(WEEK)
    }
}

impl PartialEq for UploadPurpose {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for UploadPurpose {}

impl std::hash::Hash for UploadPurpose {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

/// What the client declared at creation and what the server recorded since
/// (`uploads.meta_json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadMeta {
    /// SHA-256 of the whole content, lowercase hex: declared at creation, or
    /// recorded when the upload completes (a purpose with
    /// [`HashRule::Optional`]). Empty until then when not declared.
    #[serde(default)]
    pub sha256: String,
    /// The content's type: a media extension of the store's allowlist,
    /// declared at creation ([`Content::DeclaredMedia`]) or sniffed on
    /// completion, or `json` or `zip` for an import ([`Found::ext`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<String>,
    /// The file name the client gave (`filename` in `Upload-Metadata`),
    /// cleaned ([`clean_filename`]): untrusted, a label at most, never a
    /// path. Dropped when the consumer discards the bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// When a consumer claimed the upload ([`claim`]), unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<i64>,
}

/// `raw` as a label: the last path component, control characters dropped,
/// whitespace trimmed, at most [`MAX_FILENAME_BYTES`] bytes (cut on a
/// character boundary); `None` when nothing is left.
#[must_use]
pub fn clean_filename(raw: &str) -> Option<String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let mut name: String = base.chars().filter(|c| !c.is_control()).collect();
    if name.len() > MAX_FILENAME_BYTES {
        let mut cut = MAX_FILENAME_BYTES;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name.truncate(cut);
    }
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// An upload to create.
#[derive(Clone, Debug)]
pub struct NewUpload<'a> {
    /// Upload id (ULID).
    pub id: &'a str,
    /// The user it belongs to.
    pub user_id: &'a str,
    /// What it is for.
    pub purpose: UploadPurpose,
    /// Declared length in bytes.
    pub length: i64,
    /// What the client declared.
    pub meta: &'a UploadMeta,
    /// When an unfinished upload may be removed, unix ms.
    pub expires_at: i64,
}

/// A stored upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    /// Upload id (ULID).
    pub id: String,
    /// The user it belongs to.
    pub user_id: String,
    /// What it is for; `None` for a purpose this build does not know.
    pub purpose: Option<UploadPurpose>,
    /// Declared length in bytes.
    pub length: i64,
    /// Bytes received so far.
    pub offset: i64,
    /// What the client declared and the server recorded.
    pub meta: UploadMeta,
    /// Creation time, unix ms.
    pub created_at: i64,
    /// When it may be removed if unfinished, unix ms.
    pub expires_at: i64,
    /// When every byte arrived and was checked, unix ms.
    pub completed_at: Option<i64>,
}

impl Upload {
    /// Whether every byte arrived and was checked.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.completed_at.is_some()
    }

    /// Whether a consumer claimed it.
    #[must_use]
    pub fn is_consumed(&self) -> bool {
        self.meta.consumed_at.is_some()
    }

    /// When the housekeeping may delete the row and its bytes, unix ms:
    /// `expires_at` while unfinished; [`CONSUMED_RETENTION`] after a claim;
    /// otherwise the purpose's [`UploadPurpose::keep_complete`] after
    /// completion (the longest of the registry for an unknown purpose).
    #[must_use]
    pub fn stale_at(&self) -> i64 {
        let after = |at: i64, wait: Duration| at.saturating_add(crate::auth::millis(wait));
        match (self.completed_at, self.meta.consumed_at) {
            (_, Some(consumed)) => after(consumed, CONSUMED_RETENTION),
            (Some(completed), None) => after(
                completed,
                self.purpose
                    .map_or_else(UploadPurpose::longest_keep, |p| p.keep_complete),
            ),
            (None, None) => self.expires_at,
        }
    }

    /// Whether it can still be resumed (unfinished) or used (complete) at
    /// `now`. A consumed upload is not live.
    #[must_use]
    pub fn is_live(&self, now: i64) -> bool {
        !self.is_consumed() && now < self.stale_at()
    }

    /// What its bytes are, once complete.
    #[must_use]
    pub fn found(&self) -> Option<Found> {
        if !self.is_complete() {
            return None;
        }
        let ext = self.meta.ext.as_deref();
        match self.purpose?.content {
            Content::Sqlite => Some(Found::Sqlite),
            Content::DeclaredMedia(_) | Content::Media(_) => {
                ext.and_then(MediaKind::from_ext).map(Found::Media)
            }
            Content::JsonOrZip => match ext {
                Some("json") => Some(Found::Json),
                Some("zip") => Some(Found::Zip),
                _ => None,
            },
        }
    }

    /// The SHA-256 of its bytes, once complete.
    #[must_use]
    pub fn digest(&self) -> Option<Digest> {
        self.is_complete()
            .then(|| Digest::parse_hex(&self.meta.sha256))
            .flatten()
    }
}

/// Where the bytes of upload `id` are: `<id>.part` while it is in progress,
/// `<id>` once complete. `uploads_dir` is `<data>/work/uploads`.
#[must_use]
pub fn file_path(uploads_dir: &Path, id: &str, complete: bool) -> PathBuf {
    if complete {
        uploads_dir.join(id)
    } else {
        uploads_dir.join(format!("{id}.part"))
    }
}

/// Stores a new upload at offset 0.
///
/// # Errors
///
/// [`RepoError::Conflict`] when the id is taken; otherwise the insert failed.
pub fn insert(conn: &Connection, upload: &NewUpload<'_>, now: i64) -> Result<()> {
    let meta = serde_json::to_string(upload.meta).expect("upload metadata serializes");
    conn.execute(
        "INSERT INTO uploads (id, user_id, purpose, length, upload_offset, meta_json, created_at, \
         expires_at) VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)",
        params![
            upload.id,
            upload.user_id,
            upload.purpose.as_str(),
            upload.length,
            meta,
            now,
            upload.expires_at,
        ],
    )
    .map_err(|e| conflict_on_unique(e, "upload"))?;
    Ok(())
}

/// The upload `id` of `user_id`, if it exists. Another user's upload reads
/// as missing.
///
/// # Errors
///
/// The query failed.
pub fn get(conn: &Connection, user_id: &str, id: &str) -> Result<Option<Upload>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM uploads WHERE id = ?1 AND user_id = ?2"),
        params![id, user_id],
        from_row,
    )
    .optional()
    .map_err(RepoError::from)
}

/// Moves the offset of `id` from `from` to `to`. Returns false when the
/// offset was not `from` any more (another request moved it).
///
/// # Errors
///
/// The update failed.
pub fn advance(conn: &Connection, id: &str, from: i64, to: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE uploads SET upload_offset = ?3 WHERE id = ?1 AND upload_offset = ?2 \
         AND completed_at IS NULL",
        params![id, from, to],
    )?;
    Ok(changed == 1)
}

/// Marks `id` complete at `now`, with `meta`: what the client declared plus
/// what the server recorded (the SHA-256, the type it found). Returns
/// whether it was in progress.
///
/// # Errors
///
/// The update failed.
pub fn complete(conn: &Connection, id: &str, meta: &UploadMeta, now: i64) -> Result<bool> {
    let meta = serde_json::to_string(meta).expect("upload metadata serializes");
    let changed = conn.execute(
        "UPDATE uploads SET completed_at = ?2, upload_offset = length, meta_json = ?3 \
         WHERE id = ?1 AND completed_at IS NULL",
        params![id, now, meta],
    )?;
    Ok(changed == 1)
}

/// Deletes the rows `ids`; returns how many existed. The caller removes
/// their files.
///
/// # Errors
///
/// The delete failed.
pub fn delete(conn: &Connection, ids: &[String]) -> Result<usize> {
    let ids = serde_json::to_string(ids).expect("ids serialize");
    Ok(conn.execute(
        "DELETE FROM uploads WHERE id IN (SELECT value FROM json_each(?1))",
        [ids],
    )?)
}

/// What [`terminate`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminated {
    /// The row is gone; the caller removes the files.
    Deleted,
    /// No such upload of the user.
    Missing,
    /// A consumer claimed it: its bytes are the consumer's now.
    Consumed,
}

/// Deletes the upload `id` of `user_id` unless a consumer claimed it (tus
/// termination). In the caller's write transaction, so a claim cannot slip
/// between the check and the delete.
///
/// # Errors
///
/// A query failed.
pub fn terminate(conn: &Connection, user_id: &str, id: &str) -> Result<Terminated> {
    match get(conn, user_id, id)? {
        None => Ok(Terminated::Missing),
        Some(upload) if upload.is_consumed() => Ok(Terminated::Consumed),
        Some(upload) => {
            delete(conn, &[upload.id])?;
            Ok(Terminated::Deleted)
        }
    }
}

/// Unfinished uploads of `user_id` that have not expired at `now`.
///
/// # Errors
///
/// The query failed.
pub fn count_unfinished(conn: &Connection, user_id: &str, now: i64) -> Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM uploads WHERE user_id = ?1 AND completed_at IS NULL \
         AND expires_at > ?2",
        params![user_id, now],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// The declared bytes of `user_id`'s live, unconsumed uploads of purposes
/// that count toward the staging cap ([`UploadPurpose::staged`]), at `now`.
///
/// # Errors
///
/// The query failed.
pub fn staged_bytes(conn: &Connection, user_id: &str, now: i64) -> Result<u64> {
    let staged: Vec<&str> = UploadPurpose::ALL
        .iter()
        .filter(|p| p.staged)
        .map(|p| p.as_str())
        .collect();
    let staged = serde_json::to_string(&staged).expect("names serialize");
    let uploads = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM uploads
             WHERE user_id = ?1 AND purpose IN (SELECT value FROM json_each(?2))"
        ))?
        .query_map(params![user_id, staged], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(uploads
        .iter()
        .filter(|u| u.is_live(now))
        .map(|u| u64::try_from(u.length).unwrap_or(0))
        .sum())
}

/// Unfinished uploads of `user_id` past their expiry at `now`: the sweep's
/// work list.
///
/// # Errors
///
/// The query failed.
pub fn expired(conn: &Connection, user_id: &str, now: i64) -> Result<Vec<String>> {
    let ids = conn
        .prepare(
            "SELECT id FROM uploads WHERE user_id = ?1 AND completed_at IS NULL \
             AND expires_at <= ?2 ORDER BY id",
        )?
        .query_map(params![user_id, now], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Deletes the rows of every user's stale uploads at `now`
/// ([`Upload::stale_at`]: unfinished ones past their expiry, complete ones
/// past their purpose's wait, consumed ones past [`CONSUMED_RETENTION`]) and
/// returns their ids, for the caller to remove the files. Run it in a write
/// transaction, so that nothing claims a row between the check and the
/// delete.
///
/// # Errors
///
/// A query failed.
pub fn delete_stale(conn: &Connection, now: i64) -> Result<Vec<String>> {
    let stale: Vec<String> = conn
        .prepare(&format!("SELECT {COLUMNS} FROM uploads ORDER BY id"))?
        .query_map([], from_row)?
        .filter_map(|row| match row {
            Ok(upload) if upload.stale_at() <= now => Some(Ok(upload.id)),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<rusqlite::Result<_>>()?;
    if !stale.is_empty() {
        delete(conn, &stale)?;
    }
    Ok(stale)
}

/// Every upload id, of every user.
///
/// # Errors
///
/// The query failed.
pub fn all_ids(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let ids = conn
        .prepare("SELECT id FROM uploads")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// The complete uploads of `user_id` with `purpose` whose declared SHA-256 is
/// one of `hashes`: `(sha256, upload)` pairs, oldest upload first.
///
/// # Errors
///
/// The query failed.
pub fn complete_by_sha256(
    conn: &Connection,
    user_id: &str,
    purpose: UploadPurpose,
    hashes: &[String],
) -> Result<Vec<Upload>> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }
    let hashes = serde_json::to_string(hashes).expect("hashes serialize");
    let rows = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM uploads
             WHERE user_id = ?1 AND purpose = ?2 AND completed_at IS NOT NULL
               AND json_extract(meta_json, '$.sha256') IN (SELECT value FROM json_each(?3))
             ORDER BY created_at, id"
        ))?
        .query_map(params![user_id, purpose.as_str(), hashes], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Every complete upload of `user_id` with `purpose`, oldest first.
///
/// # Errors
///
/// The query failed.
pub fn complete_of(
    conn: &Connection,
    user_id: &str,
    purpose: UploadPurpose,
) -> Result<Vec<Upload>> {
    let rows = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM uploads
             WHERE user_id = ?1 AND purpose = ?2 AND completed_at IS NOT NULL
             ORDER BY created_at, id"
        ))?
        .query_map(params![user_id, purpose.as_str()], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Why [`claim`] took nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimRefusal {
    /// The upload is not a complete upload of the user with the purpose,
    /// within its wait: missing, another user's, unfinished, expired, or of
    /// another purpose.
    Unusable(String),
    /// The upload was claimed before (or appears twice in one claim).
    Consumed(String),
}

/// Claims the complete uploads `ids` of `user_id` with `purpose` for one
/// consumer, at `now`: all of them or none. Each becomes consumed, and a
/// second claim of it is refused. The bytes stay at [`file_path`]`(dir, id,
/// true)` until the consumer calls [`discard`] (it used them) or [`release`]
/// (it did not, after all). Run it in a write transaction.
///
/// Returns the uploads as they were before the claim, in the order of `ids`,
/// or the first refusal, with nothing changed.
///
/// # Errors
///
/// A query failed.
pub fn claim(
    conn: &Connection,
    user_id: &str,
    purpose: UploadPurpose,
    ids: &[String],
    now: i64,
) -> Result<std::result::Result<Vec<Upload>, ClaimRefusal>> {
    let mut claimed: Vec<Upload> = Vec::with_capacity(ids.len());
    for id in ids {
        if claimed.iter().any(|u| &u.id == id) {
            return Ok(Err(ClaimRefusal::Consumed(id.clone())));
        }
        let usable = match get(conn, user_id, id)? {
            Some(upload) if upload.is_consumed() => {
                return Ok(Err(ClaimRefusal::Consumed(id.clone())));
            }
            Some(upload)
                if upload.purpose == Some(purpose)
                    && upload.is_complete()
                    && upload.is_live(now) =>
            {
                upload
            }
            _ => return Ok(Err(ClaimRefusal::Unusable(id.clone()))),
        };
        claimed.push(usable);
    }
    for upload in &claimed {
        let meta = UploadMeta {
            consumed_at: Some(now),
            ..upload.meta.clone()
        };
        set_meta(conn, &upload.id, &meta)?;
    }
    Ok(Ok(claimed))
}

/// Gives back claimed uploads `ids` of `user_id` that their consumer did not
/// use: they can be claimed again. Returns how many were given back.
///
/// # Errors
///
/// A query failed.
pub fn release(conn: &Connection, user_id: &str, ids: &[String]) -> Result<usize> {
    let mut released = 0;
    for id in ids {
        if let Some(upload) = get(conn, user_id, id)?.filter(Upload::is_consumed) {
            let meta = UploadMeta {
                consumed_at: None,
                ..upload.meta
            };
            set_meta(conn, id, &meta)?;
            released += 1;
        }
    }
    Ok(released)
}

/// Records that the consumer of the claimed uploads `ids` of `user_id` is
/// done with their bytes: the client's file name is dropped, and the row
/// stays, so a reuse answers `upload_consumed` until the housekeeping
/// deletes it. Returns the ids that were claimed ones, whose files the
/// caller removes; the others are left alone.
///
/// # Errors
///
/// A query failed.
pub fn discard(conn: &Connection, user_id: &str, ids: &[String]) -> Result<Vec<String>> {
    let mut discarded = Vec::new();
    for id in ids {
        if let Some(upload) = get(conn, user_id, id)?.filter(Upload::is_consumed) {
            let meta = UploadMeta {
                filename: None,
                ..upload.meta
            };
            set_meta(conn, id, &meta)?;
            discarded.push(upload.id);
        }
    }
    Ok(discarded)
}

fn set_meta(conn: &Connection, id: &str, meta: &UploadMeta) -> Result<()> {
    let meta = serde_json::to_string(meta).expect("upload metadata serializes");
    conn.execute(
        "UPDATE uploads SET meta_json = ?2 WHERE id = ?1",
        params![id, meta],
    )?;
    Ok(())
}

const COLUMNS: &str =
    "id, user_id, purpose, length, upload_offset, meta_json, created_at, expires_at, completed_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Upload> {
    let meta: Option<String> = row.get(5)?;
    Ok(Upload {
        id: row.get(0)?,
        user_id: row.get(1)?,
        purpose: UploadPurpose::parse(&row.get::<_, String>(2)?),
        length: row.get(3)?,
        offset: row.get(4)?,
        meta: meta
            .and_then(|m| serde_json::from_str(&m).ok())
            .unwrap_or_default(),
        created_at: row.get(6)?,
        expires_at: row.get(7)?,
        completed_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::control::testing::{NOW, control_with_users};

    const SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const HOUR: i64 = 3_600_000;

    fn meta(ext: Option<&str>) -> UploadMeta {
        UploadMeta {
            sha256: SHA.into(),
            ext: ext.map(str::to_owned),
            ..UploadMeta::default()
        }
    }

    fn new<'a>(
        id: &'a str,
        user_id: &'a str,
        purpose: UploadPurpose,
        meta: &'a UploadMeta,
    ) -> NewUpload<'a> {
        NewUpload {
            id,
            user_id,
            purpose,
            length: 10,
            meta,
            expires_at: NOW + 24 * HOUR,
        }
    }

    #[test]
    fn an_upload_moves_forward_completes_and_is_found_by_hash() {
        let (db, owner, member) = control_with_users();
        let declared = meta(Some("jpg"));
        let upload = new("U1", &owner, UploadPurpose::MIGRATION_OBJECT, &declared);
        db.write(|tx| insert(tx, &upload, NOW)).unwrap();
        let get_as = |user: &str| db.read(|c| get(c, user, "U1")).unwrap();
        assert_eq!(get_as(&member), None, "another user's upload is missing");
        let upload = get_as(&owner).unwrap();
        assert_eq!(
            (upload.offset, upload.length, upload.is_complete()),
            (0, 10, false)
        );
        assert_eq!(upload.meta, declared);
        assert_eq!(upload.purpose, Some(UploadPurpose::MIGRATION_OBJECT));
        assert_eq!((upload.found(), upload.digest()), (None, None));

        assert!(db.write(|tx| advance(tx, "U1", 0, 6)).unwrap());
        assert!(
            !db.write(|tx| advance(tx, "U1", 0, 6)).unwrap(),
            "stale offset"
        );
        let hashes = vec![SHA.to_owned()];
        let found = |user: &str| {
            db.read(|c| complete_by_sha256(c, user, UploadPurpose::MIGRATION_OBJECT, &hashes))
                .unwrap()
        };
        assert!(found(&owner).is_empty(), "not complete yet");
        assert_eq!(db.read(|c| count_unfinished(c, &owner, NOW)).unwrap(), 1);

        assert!(db.write(|tx| complete(tx, "U1", &declared, NOW)).unwrap());
        assert!(!db.write(|tx| complete(tx, "U1", &declared, NOW)).unwrap());
        let done = get_as(&owner).unwrap();
        assert_eq!((done.offset, done.completed_at), (10, Some(NOW)));
        assert_eq!(done.found(), Some(Found::Media(MediaKind::Jpeg)));
        assert_eq!(done.digest().unwrap().to_string(), SHA);
        assert_eq!(found(&owner).len(), 1);
        assert!(found(&member).is_empty());
        assert_eq!(db.read(|c| count_unfinished(c, &owner, NOW)).unwrap(), 0);
        assert_eq!(db.write(|tx| delete(tx, &["U1".to_owned()])).unwrap(), 1);
    }

    #[test]
    fn the_registry_names_each_purpose_once_with_its_rules() {
        let names: HashSet<&str> = UploadPurpose::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(names.len(), UploadPurpose::ALL.len(), "names are unique");
        for purpose in UploadPurpose::ALL {
            assert_eq!(UploadPurpose::parse(purpose.as_str()), Some(*purpose));
            assert!(!purpose.uploaders.scopes.is_empty() || purpose.uploaders.session);
        }
        for unknown in ["bookmark", "Import", "", "migration"] {
            assert_eq!(UploadPurpose::parse(unknown), None, "{unknown}");
        }

        // The rules of each purpose. Migration needs `migrate`; bookmarks
        // and imports a session or `uploads`, and no declared hash (PG19).
        let migrate = Uploaders::tokens(ScopeSet::one(Scope::Migrate));
        let web = Uploaders::session_or(ScopeSet::one(Scope::Uploads));
        let gib = 1024 * MIB;
        let import_setting = 10 * gib;
        let (required, optional) = (HashRule::Required, HashRule::Optional);
        let expected = [
            (
                UploadPurpose::MIGRATION_OBJECT,
                migrate,
                300 * MIB,
                required,
                WEEK,
                false,
                false,
            ),
            (
                UploadPurpose::MIGRATION_DB,
                migrate,
                4 * gib,
                required,
                WEEK,
                false,
                false,
            ),
            (
                UploadPurpose::BOOKMARK_ORIGINAL,
                web,
                200 * MIB,
                optional,
                DAY,
                true,
                true,
            ),
            (
                UploadPurpose::BOOKMARK_PREVIEW,
                web,
                2 * MIB,
                optional,
                DAY,
                true,
                true,
            ),
            (
                UploadPurpose::IMPORT,
                web,
                import_setting,
                optional,
                DAY,
                false,
                true,
            ),
        ];
        for (purpose, uploaders, cap, sha256, keep, quota, staged) in expected {
            let name = purpose.as_str();
            assert_eq!(purpose.uploaders, uploaders, "{name}");
            assert_eq!(purpose.byte_limit(import_setting), cap, "{name}");
            assert_eq!(purpose.sha256, sha256, "{name}");
            assert_eq!(purpose.keep_complete, keep, "{name}");
            assert_eq!((purpose.quota, purpose.staged), (quota, staged), "{name}");
        }
        assert_eq!(
            UploadPurpose::MIGRATION_OBJECT.byte_limit(1),
            300 * MIB,
            "a fixed cap ignores the import setting"
        );
        assert_eq!(UploadPurpose::IMPORT.byte_limit(gib), gib);
        assert!(
            UploadPurpose::token_scopes().contains(Scope::Uploads)
                && UploadPurpose::token_scopes().contains(Scope::Migrate)
        );
        assert!(UploadPurpose::any_takes_sessions());
        assert_eq!(UploadPurpose::longest_keep(), WEEK);
    }

    #[test]
    fn content_rules_go_by_the_first_bytes() {
        let jpeg = b"\xFF\xD8\xFF\xE0\0\x10JFIF\0".as_slice();
        let pdf = b"%PDF-1.7\n".as_slice();
        let avif = b"\0\0\0\x1cftypavif\0\0\0\0mif1miafMA1B".as_slice();
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".as_slice();
        let html = b"<!DOCTYPE html><html>".as_slice();
        let zip = b"PK\x03\x04\x14\0\0\0".as_slice();

        let declared = Content::DeclaredMedia(KindSet::ALL);
        assert_eq!(
            declared.check(Some("jpg"), jpeg),
            Ok(Found::Media(MediaKind::Jpeg))
        );
        for (ext, head) in [(Some("png"), jpeg), (None, jpeg), (Some("svg"), svg)] {
            assert_eq!(declared.check(ext, head), Err(ContentRefusal::NotDeclared));
        }

        let original = UploadPurpose::BOOKMARK_ORIGINAL.content;
        assert_eq!(
            original.check(Some("png"), pdf),
            Ok(Found::Media(MediaKind::Pdf)),
            "a declared type decides nothing"
        );
        assert_eq!(
            original.check(None, avif),
            Ok(Found::Media(MediaKind::Avif))
        );
        let preview = UploadPurpose::BOOKMARK_PREVIEW.content;
        assert_eq!(preview.check(None, jpeg), Ok(Found::Media(MediaKind::Jpeg)));
        for head in [pdf, avif, svg, html, zip, b"".as_slice()] {
            assert_eq!(preview.check(None, head), Err(ContentRefusal::Unsupported));
        }
        for head in [svg, html, zip] {
            assert_eq!(original.check(None, head), Err(ContentRefusal::Unsupported));
        }

        let import = UploadPurpose::IMPORT.content;
        let mut padded = b"\xEF\xBB\xBF".to_vec();
        padded.extend_from_slice(&[b' '; 600]);
        padded.extend_from_slice(b"\r\n\t[{\"id\": 1}]");
        for head in [
            b"{\"posts\": []}".as_slice(),
            b"[]".as_slice(),
            b"\n  {".as_slice(),
            &padded,
        ] {
            assert_eq!(import.check(None, head), Ok(Found::Json));
        }
        assert_eq!(import.check(Some("json"), zip), Ok(Found::Zip));
        for head in [
            html,
            svg,
            jpeg,
            b"\"a string\"".as_slice(),
            b"   ".as_slice(),
            b"PK\x05\x06".as_slice(),
            b"\xFF\xFE{\0".as_slice(),
        ] {
            assert_eq!(import.check(None, head), Err(ContentRefusal::Unsupported));
        }

        let db = UploadPurpose::MIGRATION_DB.content;
        assert_eq!(
            db.check(None, b"SQLite format 3\0\x10\0"),
            Ok(Found::Sqlite)
        );
        assert_eq!(db.check(None, zip), Err(ContentRefusal::NotDeclared));
        assert_eq!(Found::Zip.ext(), Some("zip"));
        assert_eq!(Found::Sqlite.ext(), None);
    }

    #[test]
    fn file_names_are_labels_not_paths() {
        assert_eq!(
            clean_filename("IMG_0001.HEIC").as_deref(),
            Some("IMG_0001.HEIC")
        );
        assert_eq!(
            clean_filename("../../etc/passwd").as_deref(),
            Some("passwd")
        );
        assert_eq!(
            clean_filename("C:\\Users\\me\\scan.pdf").as_deref(),
            Some("scan.pdf")
        );
        assert_eq!(
            clean_filename(" a\u{0}b\u{7}\nc.jpg ").as_deref(),
            Some("abc.jpg")
        );
        assert_eq!(clean_filename("dir/"), None);
        assert_eq!(clean_filename("\u{1}\u{2}"), None);
        let long = "é".repeat(200);
        let cut = clean_filename(&long).unwrap();
        assert!(cut.len() <= MAX_FILENAME_BYTES && cut.chars().all(|c| c == 'é'));
    }

    #[test]
    fn a_complete_upload_is_claimed_once_all_or_none() {
        let (db, owner, member) = control_with_users();
        let declared = UploadMeta {
            filename: Some("photo.jpg".into()),
            ..meta(Some("jpg"))
        };
        for (id, user, purpose) in [
            ("A", &owner, UploadPurpose::BOOKMARK_ORIGINAL),
            ("B", &owner, UploadPurpose::BOOKMARK_ORIGINAL),
            ("P", &owner, UploadPurpose::BOOKMARK_PREVIEW),
            ("OPEN", &owner, UploadPurpose::BOOKMARK_ORIGINAL),
            ("M", &member, UploadPurpose::BOOKMARK_ORIGINAL),
        ] {
            db.write(|tx| insert(tx, &new(id, user, purpose, &declared), NOW))
                .unwrap();
            if id != "OPEN" {
                db.write(|tx| complete(tx, id, &declared, NOW)).unwrap();
            }
        }
        let ids = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let original = UploadPurpose::BOOKMARK_ORIGINAL;
        let claim_as = |user: &str, list: &[&str], at: i64| {
            db.write(|tx| claim(tx, user, original, &ids(list), at))
                .unwrap()
        };

        // Each unusable one refuses the whole claim, and changes nothing.
        for (list, refused) in [
            (&["A", "OPEN"][..], ClaimRefusal::Unusable("OPEN".into())),
            (&["A", "P"][..], ClaimRefusal::Unusable("P".into())),
            (&["A", "M"][..], ClaimRefusal::Unusable("M".into())),
            (&["A", "NONE"][..], ClaimRefusal::Unusable("NONE".into())),
            (&["A", "A"][..], ClaimRefusal::Consumed("A".into())),
        ] {
            assert_eq!(claim_as(&owner, list, NOW), Err(refused), "{list:?}");
        }
        let a = db.read(|c| get(c, &owner, "A")).unwrap().unwrap();
        assert!(!a.is_consumed(), "a refused claim changes nothing");
        // Past its day, a complete upload is unusable.
        assert_eq!(
            claim_as(&owner, &["A"], NOW + 24 * HOUR),
            Err(ClaimRefusal::Unusable("A".into()))
        );

        let claimed = claim_as(&owner, &["B", "A"], NOW + HOUR).unwrap();
        assert_eq!(
            claimed.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
            ["B", "A"]
        );
        assert_eq!(claimed[0].meta.filename.as_deref(), Some("photo.jpg"));
        assert_eq!(
            claim_as(&owner, &["A"], NOW + HOUR),
            Err(ClaimRefusal::Consumed("A".into()))
        );
        let a = db.read(|c| get(c, &owner, "A")).unwrap().unwrap();
        assert_eq!(a.meta.consumed_at, Some(NOW + HOUR));
        assert!(!a.is_live(NOW + HOUR));
        assert_eq!(
            db.write(|tx| terminate(tx, &owner, "A")).unwrap(),
            Terminated::Consumed
        );

        // Given back, it can be claimed again; discarded, it stays used.
        assert_eq!(
            db.write(|tx| release(tx, &owner, &ids(&["A", "P"])))
                .unwrap(),
            1
        );
        assert_eq!(claim_as(&owner, &["A"], NOW + HOUR).unwrap().len(), 1);
        assert_eq!(
            db.write(|tx| discard(tx, &owner, &ids(&["A", "B", "P"])))
                .unwrap(),
            ["A", "B"]
        );
        let a = db.read(|c| get(c, &owner, "A")).unwrap().unwrap();
        assert_eq!(a.meta.filename, None, "the file name goes with the bytes");
        assert!(a.is_consumed() && a.digest().is_some());
        assert_eq!(
            claim_as(&owner, &["A"], NOW + HOUR),
            Err(ClaimRefusal::Consumed("A".into()))
        );
        assert_eq!(
            db.write(|tx| release(tx, &member, &ids(&["A"]))).unwrap(),
            0,
            "only the owner's uploads"
        );
        assert_eq!(
            db.write(|tx| terminate(tx, &owner, "P")).unwrap(),
            Terminated::Deleted
        );
        assert_eq!(
            db.write(|tx| terminate(tx, &owner, "P")).unwrap(),
            Terminated::Missing
        );
    }

    #[test]
    fn each_state_and_purpose_has_its_own_expiry() {
        let (db, owner, member) = control_with_users();
        let declared = meta(None);
        let insert_as = |id: &str, user: &str, purpose: UploadPurpose, done: Option<i64>| {
            db.write(|tx| insert(tx, &new(id, user, purpose, &declared), NOW))
                .unwrap();
            if let Some(at) = done {
                db.write(|tx| complete(tx, id, &declared, at)).unwrap();
            }
        };
        insert_as("OPEN", &owner, UploadPurpose::IMPORT, None);
        insert_as("WEB", &owner, UploadPurpose::IMPORT, Some(NOW));
        insert_as("MIG", &member, UploadPurpose::MIGRATION_DB, Some(NOW));
        insert_as("USED", &owner, UploadPurpose::IMPORT, Some(NOW));
        db.write(|tx| claim(tx, &owner, UploadPurpose::IMPORT, &["USED".into()], NOW))
            .unwrap()
            .unwrap();
        db.write(|tx| {
            tx.execute(
                "INSERT INTO uploads (id, user_id, purpose, length, upload_offset, meta_json, \
                 created_at, expires_at, completed_at) \
                 VALUES ('NEW', ?1, 'from-a-newer-build', 1, 1, NULL, ?2, ?2, ?2)",
                params![owner, NOW],
            )
            .map_err(RepoError::from)
        })
        .unwrap();

        let stale_after = |hours: i64| {
            let mut ids = db.write(|tx| delete_stale(tx, NOW + hours * HOUR)).unwrap();
            ids.sort();
            ids
        };
        assert!(stale_after(23).is_empty());
        assert_eq!(
            db.read(|c| staged_bytes(c, &owner, NOW + 23 * HOUR))
                .unwrap(),
            20,
            "the open upload and the complete one; not the used one"
        );
        assert_eq!(stale_after(24), ["OPEN", "WEB"]);
        assert_eq!(db.read(|c| staged_bytes(c, &owner, NOW)).unwrap(), 0);
        assert!(stale_after(24 * 7 - 1).is_empty());
        assert_eq!(stale_after(24 * 7), ["MIG", "NEW", "USED"]);
        assert!(db.read(all_ids).unwrap().is_empty());
    }

    #[test]
    fn unfinished_uploads_expire() {
        let (db, owner, _) = control_with_users();
        let declared = meta(None);
        for (id, expires_at) in [("U1", NOW - 1), ("U2", NOW + 1)] {
            let upload = NewUpload {
                expires_at,
                ..new(id, &owner, UploadPurpose::MIGRATION_DB, &declared)
            };
            db.write(|tx| insert(tx, &upload, NOW)).unwrap();
        }
        assert_eq!(db.read(|c| expired(c, &owner, NOW)).unwrap(), ["U1"]);
        assert_eq!(db.read(|c| count_unfinished(c, &owner, NOW)).unwrap(), 1);
        let u1 = db.read(|c| get(c, &owner, "U1")).unwrap().unwrap();
        assert!(!u1.is_live(NOW));
        let u2 = db.read(|c| get(c, &owner, "U2")).unwrap().unwrap();
        assert!(u2.is_live(NOW));
    }

    #[test]
    fn files_are_named_by_id_and_state() {
        let dir = Path::new("/data/work/uploads");
        assert_eq!(file_path(dir, "U1", false), dir.join("U1.part"));
        assert_eq!(file_path(dir, "U1", true), dir.join("U1"));
    }
}
