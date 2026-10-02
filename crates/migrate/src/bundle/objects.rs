//! The bundle's objects: every present file the rows reference, hashed once
//! and typed by its magic bytes, deduplicated by content (plan D3, §4.1).
//!
//! The files stay where they are: the bundle records each object's digest,
//! type and size, plus the path to read it from when it is uploaded. Types
//! come from `shelfy-media`'s allowlist, so the bundle names an object
//! exactly as the server's store does; a file of another type cannot be
//! stored and counts as unsupported.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use shelfy_media::kind::SNIFF_LEN;
use shelfy_media::{Digest, MediaKind};

use crate::files::{FileRefs, FileState, PathId};

/// What an object is (`media_objects.role`, plan §2.7), by precedence: an
/// object used several ways takes the first role of this list that applies.
/// Hero images are `Screenshot` and the capture video preview is `Preview`
/// (OI-2): the plan's role list has no `hero` or `video_preview`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Screenshot,
    Band,
    Filmstrip,
    Section,
    Footer,
    Og,
    Favicon,
    Poster,
    Image,
    Preview,
    Video,
    File,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Screenshot => "screenshot",
            Role::Band => "band",
            Role::Filmstrip => "filmstrip",
            Role::Section => "section",
            Role::Footer => "footer",
            Role::Og => "og",
            Role::Favicon => "favicon",
            Role::Poster => "poster",
            Role::Image => "image",
            Role::Preview => "preview",
            Role::Video => "video",
            Role::File => "file",
        }
    }

    /// The role of a manual upload's original: by its type.
    pub fn of_original(kind: MediaKind) -> Role {
        if kind.is_image() {
            Role::Image
        } else if kind.is_video() {
            Role::Video
        } else {
            Role::File
        }
    }
}

/// One distinct object.
#[derive(Debug, Clone)]
pub struct Object {
    pub digest: Digest,
    pub kind: MediaKind,
    pub bytes: u64,
    /// The file to upload it from.
    pub path: PathBuf,
    /// Its role, once a row uses it.
    pub role: Option<Role>,
}

/// What became of a referenced path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    /// Hashed: index into [`ObjectTable::objects`].
    Object(usize),
    /// Not on disk, or not checked.
    Missing,
    /// Referenced only as a video, without `--with-videos`.
    Excluded,
    /// A type outside the store's allowlist.
    Unsupported,
    /// Present but unreadable.
    Unreadable,
}

/// Counts of the hashing pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HashCounts {
    pub hashed: u64,
    pub hashed_bytes: u64,
    pub excluded: u64,
    pub excluded_bytes: u64,
    pub unsupported: u64,
    pub unreadable: u64,
}

/// The objects of a bundle and how each referenced path resolved.
#[derive(Debug, Default)]
pub struct ObjectTable {
    pub objects: Vec<Object>,
    resolved: Vec<Resolved>,
    by_digest: HashMap<Digest, usize>,
    pub counts: HashCounts,
}

impl ObjectTable {
    /// Hashes every present file of `files` that the bundle carries: all of
    /// them with `with_videos`, else all but those referenced only as videos.
    /// Each file is read once.
    pub fn hash(files: &FileRefs, with_videos: bool) -> ObjectTable {
        let mut table = ObjectTable::default();
        for id in 0..files.len() {
            let resolved = match (files.state(id), files.full_path(id)) {
                (FileState::Present { bytes }, Some(path)) => {
                    let video_only = files.classes(id).iter().all(|c| c.is_video());
                    if video_only && !with_videos {
                        table.counts.excluded += 1;
                        table.counts.excluded_bytes += bytes;
                        Resolved::Excluded
                    } else {
                        table.add(path)
                    }
                }
                _ => Resolved::Missing,
            };
            table.resolved.push(resolved);
        }
        table
    }

    fn add(&mut self, path: &Path) -> Resolved {
        match hash_file(path) {
            Ok((digest, Some(kind), bytes)) => {
                self.counts.hashed += 1;
                self.counts.hashed_bytes += bytes;
                let index = *self.by_digest.entry(digest).or_insert_with(|| {
                    self.objects.push(Object {
                        digest,
                        kind,
                        bytes,
                        path: path.to_path_buf(),
                        role: None,
                    });
                    self.objects.len() - 1
                });
                Resolved::Object(index)
            }
            Ok((_, None, _)) => {
                self.counts.unsupported += 1;
                Resolved::Unsupported
            }
            Err(_) => {
                self.counts.unreadable += 1;
                Resolved::Unreadable
            }
        }
    }

    /// How the path `id` resolved.
    pub fn resolved(&self, id: PathId) -> Resolved {
        self.resolved.get(id).copied().unwrap_or(Resolved::Missing)
    }

    /// The object of the stored path `raw`, giving it `role` unless it has a
    /// role of higher precedence.
    pub fn use_path(&mut self, files: &FileRefs, raw: &str, role: Role) -> Option<usize> {
        let id = files.find(raw)?;
        let Resolved::Object(index) = self.resolved(id) else {
            return None;
        };
        let object = &mut self.objects[index];
        object.role = Some(object.role.map_or(role, |r| r.min(role)));
        Some(index)
    }

    /// The object of the stored path `raw` with the role its type gives it
    /// (manual originals).
    pub fn use_original(&mut self, files: &FileRefs, raw: &str) -> Option<usize> {
        let id = files.find(raw)?;
        let Resolved::Object(index) = self.resolved(id) else {
            return None;
        };
        let role = Role::of_original(self.objects[index].kind);
        self.use_path(files, raw, role)
    }

    /// The type of the object of the stored path `raw`, without using it.
    pub fn kind_of(&self, files: &FileRefs, raw: &str) -> Option<MediaKind> {
        let id = files.find(raw)?;
        match self.resolved(id) {
            Resolved::Object(index) => Some(self.objects[index].kind),
            _ => None,
        }
    }

    /// Whether the stored path `raw` names a referenced file that is not on
    /// disk.
    pub fn is_missing(&self, files: &FileRefs, raw: &str) -> bool {
        files
            .find(raw)
            .is_some_and(|id| self.resolved(id) == Resolved::Missing)
    }
}

/// The SHA-256, the type (from the first bytes) and the size of a file.
fn hash_file(path: &Path) -> io::Result<(Digest, Option<MediaKind>, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut head = Vec::with_capacity(SNIFF_LEN);
    let mut buffer = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let n = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if head.len() < SNIFF_LEN {
            let wanted = (SNIFF_LEN - head.len()).min(n);
            head.extend_from_slice(&buffer[..wanted]);
        }
        hasher.update(&buffer[..n]);
        size += n as u64;
    }
    let digest = Digest::from_bytes(hasher.finalize().into());
    let kind = if size == 0 {
        None
    } else {
        MediaKind::sniff(&head)
    };
    Ok((digest, kind, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::FileClass;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn files_are_hashed_once_deduplicated_and_typed() {
        let dir = tempfile::tempdir().unwrap();
        let assets = dir.path().join("assets/images");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(assets.join("a.png"), PNG).unwrap();
        std::fs::write(assets.join("same.png"), PNG).unwrap();
        std::fs::write(assets.join("note.txt"), b"plain text").unwrap();
        std::fs::write(assets.join("v.mp4"), b"\0\0\0\x18ftypisom\0\0\0\0isom").unwrap();

        let mut refs = FileRefs::default();
        let a = refs.add(FileClass::Cover, "/old/assets/images/a.png");
        let same = refs.add(FileClass::SlideImage, "/old/assets/images/same.png");
        let text = refs.add(FileClass::Image, "/old/assets/images/note.txt");
        let video = refs.add(FileClass::Video, "/old/assets/images/v.mp4");
        let gone = refs.add(FileClass::Cover, "/old/assets/images/gone.png");
        refs.check(dir.path());

        let mut table = ObjectTable::hash(&refs, false);
        assert_eq!(table.objects.len(), 1, "a.png and same.png are one object");
        assert_eq!(table.resolved(a), table.resolved(same));
        assert_eq!(table.resolved(text), Resolved::Unsupported);
        assert_eq!(table.resolved(video), Resolved::Excluded);
        assert_eq!(table.resolved(gone), Resolved::Missing);
        assert_eq!(table.objects[0].kind, MediaKind::Png);
        assert_eq!(table.objects[0].digest, Digest::of(PNG));
        assert_eq!(table.counts.hashed, 2);
        assert_eq!(table.counts.excluded, 1);

        // The role of higher precedence wins, whatever the order of use.
        let index = table
            .use_path(&refs, "/old/assets/images/same.png", Role::Image)
            .unwrap();
        table.use_path(&refs, "/old/assets/images/a.png", Role::Screenshot);
        table.use_path(&refs, "/old/assets/images/a.png", Role::Preview);
        assert_eq!(table.objects[index].role, Some(Role::Screenshot));
        assert!(table.is_missing(&refs, "/old/assets/images/gone.png"));
        assert!(!table.is_missing(&refs, "/old/assets/images/a.png"));

        let with_videos = ObjectTable::hash(&refs, true);
        assert_eq!(with_videos.objects.len(), 2);
        let Resolved::Object(v) = with_videos.resolved(video) else {
            panic!("the video is hashed with --with-videos");
        };
        assert_eq!(with_videos.objects[v].kind, MediaKind::Mp4);
    }
}
