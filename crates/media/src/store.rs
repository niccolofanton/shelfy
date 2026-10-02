//! The per-user content-addressed store (plan D3, §2.5).
//!
//! # Layout
//!
//! ```text
//! users/<user_id>/media/<aa>/<sha256>.<ext>        stored objects (masters)
//! users/<user_id>/media/<aa>/<sha256>.g480.webp    their renditions
//! users/<user_id>/media/.tmp/                      files being written
//! ```
//!
//! `<aa>` is the first two hex digits of the digest, so no directory grows past
//! a few hundred entries. Names come only from a [`Digest`] and the allowlists
//! ([`ObjectName`]), so no input can address a path outside the user's
//! directory. Every user has their own store: the same bytes saved by two users
//! are two files, and deleting a user is removing their directory.
//!
//! # Writing
//!
//! An object is streamed into a temporary file under `.tmp/` (the same file
//! system as its final place) by an [`ObjectWriter`], which hashes the bytes,
//! sniffs the type from the first ones and enforces an [`IngestLimits`]. The
//! result is a [`StagedObject`]; [`StagedObject::publish`] fsyncs it, renames
//! it to `<aa>/<sha256>.<ext>` and fsyncs the directory. A reader therefore
//! sees either no file or the complete one, and a crash leaves at most a stale
//! temporary file ([`UserMedia::sweep_temp`] removes those). Publishing bytes
//! that are already stored keeps the existing file: content addressing dedupes
//! for free.
//!
//! # Consistency with the database
//!
//! `media_objects` rows ([`crate::refs`]) describe the files. Two rules keep
//! the two in step, also when the garbage collector (P4) runs concurrently:
//!
//! 1. **Ingest** publishes the object and writes its renditions *inside* the
//!    user-DB write transaction that records its row:
//!    [`crate::refs::publish_and_record`] and [`crate::refs::record_rendition`].
//! 2. **GC** deletes rows and removes their files inside one write transaction
//!    too: [`crate::refs::collect_garbage`].
//!
//! The user DB's writer lock then orders them: a file is never removed after a
//! newer ingest recorded it again. If a commit fails after the files changed,
//! the leftovers heal themselves: an orphan file is overwritten or dedupes on
//! the next publish, and a row whose file is gone is unreferenced and goes at
//! the next GC.

use std::fs::{self, File};
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;
use tokio::io::{AsyncRead, AsyncReadExt as _};

use crate::digest::Digest;
use crate::kind::{KindSet, MediaKind, SNIFF_LEN};
use crate::name::{ObjectName, Rendition};

/// Directory of a user's store, inside `users/<user_id>/`.
pub const MEDIA_DIR: &str = "media";
/// Directory of the files being written, inside the store.
pub const TEMP_DIR: &str = ".tmp";

const MIB: u64 = 1024 * 1024;
/// Read size of the ingest loops.
const CHUNK: usize = 64 * 1024;
/// How much [`UserMedia::ingest_async`] collects before one blocking write.
const ASYNC_BATCH: usize = 1024 * 1024;

/// What one ingest path accepts: a size cap and the allowed types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IngestLimits {
    /// Largest accepted object, in bytes.
    pub max_bytes: u64,
    /// The types accepted.
    pub accept: KindSet,
}

impl IngestLimits {
    /// Images archived from a platform CDN (§2.13: 15 MB per fetch).
    pub const ARCHIVE_IMAGE: Self = Self {
        max_bytes: 15 * MIB,
        accept: KindSet::IMAGES,
    };

    /// Manual uploads (§2.13: originals up to 200 MiB).
    pub const UPLOAD: Self = Self {
        max_bytes: 200 * MIB,
        accept: KindSet::ALL,
    };

    /// On-demand and kept videos (§2.12: up to 300 MB).
    pub const VIDEO: Self = Self {
        max_bytes: 300 * MIB,
        accept: KindSet::VIDEOS,
    };
}

/// Why an ingest was refused.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// The content is larger than [`IngestLimits::max_bytes`].
    #[error("the content is larger than {limit} bytes")]
    TooLarge {
        /// The limit that was exceeded.
        limit: u64,
    },
    /// The content is empty.
    #[error("the content is empty")]
    Empty,
    /// The first bytes match no type of the allowlist.
    #[error("the content is not a supported media type")]
    UnknownType,
    /// The type is in the allowlist but not accepted by this ingest path.
    #[error("{} is not accepted here", .0.mime())]
    NotAccepted(MediaKind),
    /// Reading the source or writing the file failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A user id that cannot name a directory safely.
#[derive(Debug, thiserror::Error)]
#[error("invalid user id")]
pub struct InvalidUserId;

/// The stores of every user, under the data directory's `users/`.
#[derive(Clone, Debug)]
pub struct MediaStore {
    users_dir: PathBuf,
}

impl MediaStore {
    /// The stores under `users_dir` (`/data/shelfy/users`).
    #[must_use]
    pub fn new(users_dir: impl Into<PathBuf>) -> Self {
        Self {
            users_dir: users_dir.into(),
        }
    }

    /// The store of `user_id`. Nothing is created until something is written.
    ///
    /// # Errors
    ///
    /// [`InvalidUserId`] unless `user_id` is 1–64 ASCII letters and digits (a
    /// ULID qualifies), the rule `shelfy_core::db::UserDbCache` applies too.
    pub fn user(&self, user_id: &str) -> Result<UserMedia, InvalidUserId> {
        let valid =
            (1..=64).contains(&user_id.len()) && user_id.bytes().all(|b| b.is_ascii_alphanumeric());
        if !valid {
            return Err(InvalidUserId);
        }
        Ok(UserMedia {
            root: self.users_dir.join(user_id).join(MEDIA_DIR),
        })
    }
}

/// One user's store: `users/<user_id>/media/`.
#[derive(Clone, Debug)]
pub struct UserMedia {
    root: PathBuf,
}

impl UserMedia {
    /// The store's directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of a stored object or rendition.
    #[must_use]
    pub fn path(&self, name: &ObjectName) -> PathBuf {
        self.root.join(name.digest.shard()).join(name.to_string())
    }

    /// Path of the stored object `digest` of type `kind`.
    #[must_use]
    pub fn object_path(&self, digest: &Digest, kind: MediaKind) -> PathBuf {
        self.path(&ObjectName::original(*digest, kind))
    }

    /// Path of a rendition of the stored object `digest`.
    #[must_use]
    pub fn rendition_path(&self, digest: &Digest, rendition: Rendition) -> PathBuf {
        self.path(&ObjectName::rendition(*digest, rendition))
    }

    /// Whether the object is stored. Blocking (one `stat`).
    #[must_use]
    pub fn contains(&self, digest: &Digest, kind: MediaKind) -> bool {
        fs::metadata(self.object_path(digest, kind)).is_ok_and(|m| m.is_file())
    }

    /// A writer for a new object. Blocking.
    ///
    /// # Errors
    ///
    /// The temporary file cannot be created.
    pub fn writer(&self, limits: IngestLimits) -> io::Result<ObjectWriter> {
        Ok(ObjectWriter {
            file: self.temp_file()?,
            hasher: Sha256::new(),
            size: 0,
            head: Vec::with_capacity(SNIFF_LEN),
            kind: None,
            limits,
            root: self.root.clone(),
        })
    }

    /// Streams `reader` into a staged object. Blocking.
    ///
    /// # Errors
    ///
    /// [`IngestError`]: the content is refused, or reading or writing failed.
    /// Nothing is left behind on error.
    pub fn ingest(
        &self,
        mut reader: impl Read,
        limits: IngestLimits,
    ) -> Result<StagedObject, IngestError> {
        let mut writer = self.writer(limits)?;
        let mut chunk = vec![0u8; CHUNK];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => writer.write_chunk(&chunk[..n])?,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        writer.finish()
    }

    /// Streams an async `reader` (an HTTP body, a CDN response) into a staged
    /// object. File work runs on tokio's blocking pool, in batches of 1 MiB.
    ///
    /// # Errors
    ///
    /// Like [`UserMedia::ingest`].
    pub async fn ingest_async<R>(
        &self,
        mut reader: R,
        limits: IngestLimits,
    ) -> Result<StagedObject, IngestError>
    where
        R: AsyncRead + Unpin,
    {
        let store = self.clone();
        let mut writer = blocking(move || store.writer(limits)).await??;
        let mut batch = Vec::with_capacity(ASYNC_BATCH);
        let mut chunk = vec![0u8; CHUNK];
        let mut sniffed = false;
        loop {
            let read = reader.read(&mut chunk).await?;
            batch.extend_from_slice(&chunk[..read]);
            // The first write goes out as soon as the type can be sniffed, so a
            // refused type fails before the rest is downloaded.
            let flush = batch.len() >= ASYNC_BATCH
                || (!sniffed && batch.len() >= SNIFF_LEN)
                || (read == 0 && !batch.is_empty());
            if flush {
                sniffed = true;
                (writer, batch) = blocking(move || {
                    writer.write_chunk(&batch)?;
                    batch.clear();
                    Ok::<_, IngestError>((writer, batch))
                })
                .await??;
            }
            if read == 0 {
                return writer.finish();
            }
        }
    }

    /// Writes a rendition of the object `digest` atomically, replacing an
    /// older one. Blocking.
    ///
    /// # Errors
    ///
    /// The file system refused.
    pub fn store_rendition(
        &self,
        digest: &Digest,
        rendition: Rendition,
        bytes: &[u8],
    ) -> io::Result<()> {
        let mut file = self.temp_file()?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        let shard = self.root.join(digest.shard());
        create_dir_durable(&shard)?;
        file.persist(self.rendition_path(digest, rendition))
            .map_err(|e| e.error)?;
        sync_dir(&shard)
    }

    /// Removes a stored object and every rendition of it; missing files are
    /// fine. Returns whether a file was removed. Blocking.
    ///
    /// The database row goes first, in the same transaction (see the module
    /// documentation).
    ///
    /// # Errors
    ///
    /// The file system refused.
    pub fn remove(&self, digest: &Digest, kind: MediaKind) -> io::Result<bool> {
        let mut removed = false;
        let files = std::iter::once(self.object_path(digest, kind))
            .chain(Rendition::ALL.map(|r| self.rendition_path(digest, r)));
        for file in files {
            match fs::remove_file(&file) {
                Ok(()) => removed = true,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        if removed {
            sync_dir(&self.root.join(digest.shard()))?;
        }
        Ok(removed)
    }

    /// Removes temporary files untouched for `older_than`: the leftovers of
    /// writes interrupted by a crash. Returns how many were removed. Blocking.
    ///
    /// # Errors
    ///
    /// The file system refused.
    pub fn sweep_temp(&self, older_than: Duration) -> io::Result<usize> {
        let dir = self.root.join(TEMP_DIR);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let Some(cutoff) = SystemTime::now().checked_sub(older_than) else {
            return Ok(0);
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry?;
            let meta = entry.metadata()?;
            if !meta.is_file() || meta.modified()? > cutoff {
                continue;
            }
            match fs::remove_file(entry.path()) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(removed)
    }

    fn temp_file(&self) -> io::Result<NamedTempFile> {
        let dir = self.root.join(TEMP_DIR);
        create_dir_durable(&dir)?;
        let mut builder = tempfile::Builder::new();
        builder.prefix("obj-").suffix(".part");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            builder.permissions(fs::Permissions::from_mode(0o640));
        }
        builder.tempfile_in(dir)
    }
}

/// Writes one new object: hashes, sniffs and caps it while it streams into a
/// temporary file. Dropping it before [`ObjectWriter::finish`] removes the file.
#[derive(Debug)]
pub struct ObjectWriter {
    file: NamedTempFile,
    hasher: Sha256,
    size: u64,
    head: Vec<u8>,
    kind: Option<MediaKind>,
    limits: IngestLimits,
    root: PathBuf,
}

impl ObjectWriter {
    /// Appends `chunk`. Blocking.
    ///
    /// The type is decided as soon as [`SNIFF_LEN`] bytes have arrived, so a
    /// refused type fails early instead of after the whole download.
    ///
    /// # Errors
    ///
    /// [`IngestError::TooLarge`], [`IngestError::UnknownType`],
    /// [`IngestError::NotAccepted`], or a write error. After an error the
    /// writer must be dropped.
    pub fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), IngestError> {
        let size = self.size.saturating_add(chunk.len() as u64);
        if size > self.limits.max_bytes {
            return Err(IngestError::TooLarge {
                limit: self.limits.max_bytes,
            });
        }
        if self.kind.is_none() {
            let wanted = (SNIFF_LEN - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..wanted]);
            if self.head.len() == SNIFF_LEN {
                self.kind = Some(self.classify()?);
            }
        }
        self.hasher.update(chunk);
        self.file.write_all(chunk)?;
        self.size = size;
        Ok(())
    }

    /// Bytes written so far.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Ends the object.
    ///
    /// # Errors
    ///
    /// [`IngestError::Empty`], or the type of a short object is refused.
    pub fn finish(self) -> Result<StagedObject, IngestError> {
        if self.size == 0 {
            return Err(IngestError::Empty);
        }
        let kind = match self.kind {
            Some(kind) => kind,
            None => self.classify()?,
        };
        Ok(StagedObject {
            digest: Digest::from_hasher(self.hasher),
            kind,
            size: self.size,
            file: self.file,
            root: self.root,
        })
    }

    fn classify(&self) -> Result<MediaKind, IngestError> {
        let kind = MediaKind::sniff(&self.head).ok_or(IngestError::UnknownType)?;
        if self.limits.accept.contains(kind) {
            Ok(kind)
        } else {
            Err(IngestError::NotAccepted(kind))
        }
    }
}

/// A complete object in a temporary file, not yet in the store. Dropping it
/// removes the file.
#[derive(Debug)]
pub struct StagedObject {
    digest: Digest,
    kind: MediaKind,
    size: u64,
    file: NamedTempFile,
    root: PathBuf,
}

impl StagedObject {
    /// The digest of the content.
    #[must_use]
    pub fn digest(&self) -> Digest {
        self.digest
    }

    /// The sniffed type.
    #[must_use]
    pub fn kind(&self) -> MediaKind {
        self.kind
    }

    /// Size in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The name it will have in the store.
    #[must_use]
    pub fn name(&self) -> ObjectName {
        ObjectName::original(self.digest, self.kind)
    }

    /// The temporary file, for example to render the image before deciding to
    /// keep it.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// Moves the object into the store: fsync, rename, fsync the directory.
    /// When the same content is already stored, the existing file stays and
    /// the temporary one is removed. Blocking.
    ///
    /// # Errors
    ///
    /// The file system refused; the temporary file is removed.
    pub fn publish(self) -> io::Result<StoredObject> {
        let shard = self.root.join(self.digest.shard());
        let dest = shard.join(self.name().to_string());
        let stored = StoredObject {
            digest: self.digest,
            kind: self.kind,
            size: self.size,
            deduplicated: true,
        };
        // A file of another size under this name is damaged: replace it.
        if fs::symlink_metadata(&dest).is_ok_and(|m| m.is_file() && m.len() == self.size) {
            return Ok(stored);
        }
        self.file.as_file().sync_all()?;
        create_dir_durable(&shard)?;
        self.file.persist(&dest).map_err(|e| e.error)?;
        sync_dir(&shard)?;
        Ok(StoredObject {
            deduplicated: false,
            ..stored
        })
    }
}

/// An object in the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredObject {
    /// The digest of the content.
    pub digest: Digest,
    /// Its type.
    pub kind: MediaKind,
    /// Size in bytes.
    pub size: u64,
    /// Whether the content was already stored (nothing was written).
    pub deduplicated: bool,
}

impl StoredObject {
    /// Its name in the store.
    #[must_use]
    pub fn name(&self) -> ObjectName {
        ObjectName::original(self.digest, self.kind)
    }
}

/// Creates `dir` and its missing parents with mode 0750 (§2.5), fsyncing the
/// parent of each directory it creates so the new entry survives a crash.
fn create_dir_durable(dir: &Path) -> io::Result<()> {
    if dir.as_os_str().is_empty() || dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        create_dir_durable(parent)?;
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o750);
    }
    match builder.create(dir) {
        Ok(()) => match dir.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => sync_dir(parent),
            _ => Ok(()),
        },
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// Makes the entries of `dir` durable (a rename, a new file).
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        // Windows has no directory fsync; NTFS journals the rename.
        let _ = dir;
        Ok(())
    }
}

/// Runs blocking file work on tokio's blocking pool.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> io::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| io::Error::other(format!("blocking file task failed: {e}")))
}
