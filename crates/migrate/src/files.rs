//! Local files referenced by a desktop library, checked read-only.
//!
//! The desktop stores absolute paths under its userData directory
//! (`<userData>/assets/<kind>/…`). The library file may have been copied
//! elsewhere, and its paths may come from another machine, so each path is
//! rebased: the desktop root is detected as the most common prefix before
//! `assets/`, and a path is looked up as `<media root>/assets/<rest>`.
//! Nothing is ever written; directories are only listed.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use shelfy_core::legacy::web::WebAssetRole;

use crate::report::{FileClassCounts, OrphanCounts, UploadEstimate};

/// Where a reference comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum FileClass {
    /// `posts.thumbnail_path`: the downloaded cover.
    Cover,
    /// `posts.preview_path`: the automatic 640 px cover.
    Preview,
    /// `posts.image_path`: the slide-0 image.
    Image,
    /// `posts.video_path`: the slide-0 video.
    Video,
    /// `post_media.local_path` of an image slide.
    SlideImage,
    /// `post_media.local_path` of a video slide.
    SlideVideo,
    /// `post_media.local_path` of a file slide (manual documents).
    SlideFile,
    /// `post_media.source_url` of a manual post: the original upload.
    ManualOriginal,
    /// A file of a captured site version.
    Web(WebAssetRole),
}

impl FileClass {
    pub fn name(self) -> String {
        match self {
            FileClass::Cover => "cover".to_owned(),
            FileClass::Preview => "preview".to_owned(),
            FileClass::Image => "image".to_owned(),
            FileClass::Video => "video".to_owned(),
            FileClass::SlideImage => "slide_image".to_owned(),
            FileClass::SlideVideo => "slide_video".to_owned(),
            FileClass::SlideFile => "slide_file".to_owned(),
            FileClass::ManualOriginal => "manual_original".to_owned(),
            FileClass::Web(role) => format!("web_{}", role.as_str()),
        }
    }

    /// Archived only with `--with-videos` (plan §4.2).
    pub fn is_video(self) -> bool {
        match self {
            FileClass::Video | FileClass::SlideVideo => true,
            FileClass::Web(role) => role.is_video(),
            _ => false,
        }
    }
}

/// A distinct referenced path and what was found on disk.
#[derive(Debug, Clone)]
struct PathEntry {
    raw: String,
    classes: Vec<FileClass>,
    state: FileState,
    /// Where the file was looked up, under the media root.
    full: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// Not looked up (no media root).
    Unchecked,
    Present {
        bytes: u64,
    },
    Missing,
    /// Not under the detected desktop root, or not a safe relative path.
    OutsideRoot,
}

/// The set of referenced files.
#[derive(Debug, Default)]
pub struct FileRefs {
    entries: Vec<PathEntry>,
    index: HashMap<String, usize>,
    refs_per_class: BTreeMap<FileClass, u64>,
    legacy_root: Option<String>,
}

/// Index of a distinct path in [`FileRefs`].
pub type PathId = usize;

impl FileRefs {
    /// Records one reference and returns the path's id.
    pub fn add(&mut self, class: FileClass, path: &str) -> PathId {
        *self.refs_per_class.entry(class).or_default() += 1;
        let id = match self.index.get(path) {
            Some(&id) => id,
            None => {
                let id = self.entries.len();
                self.entries.push(PathEntry {
                    raw: path.to_owned(),
                    classes: Vec::new(),
                    state: FileState::Unchecked,
                    full: None,
                });
                self.index.insert(path.to_owned(), id);
                id
            }
        };
        let classes = &mut self.entries[id].classes;
        if !classes.contains(&class) {
            classes.push(class);
        }
        id
    }

    pub fn state(&self, id: PathId) -> FileState {
        self.entries[id].state
    }

    /// The id of a referenced path, as stored in the library.
    pub fn find(&self, path: &str) -> Option<PathId> {
        self.index.get(path).copied()
    }

    /// Where the file of `id` is on this machine, once [`FileRefs::check`]
    /// found it.
    pub fn full_path(&self, id: PathId) -> Option<&Path> {
        match self.entries[id].state {
            FileState::Present { .. } => self.entries[id].full.as_deref(),
            _ => None,
        }
    }

    /// Every class that references the path `id`.
    pub fn classes(&self, id: PathId) -> &[FileClass] {
        &self.entries[id].classes
    }

    /// Number of distinct paths.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no path is referenced.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Detects the desktop root and looks every path up under `media_root`
    /// (metadata only). Returns the relative paths found, for the orphan scan.
    pub fn check(&mut self, media_root: &Path) -> HashSet<String> {
        self.legacy_root = detect_root(self.entries.iter().map(|e| e.raw.as_str()));
        let assets = media_root.join("assets");
        let mut referenced = HashSet::new();
        for entry in &mut self.entries {
            let Some(relative) = self
                .legacy_root
                .as_deref()
                .and_then(|root| relative_to_assets(&entry.raw, root))
            else {
                entry.state = FileState::OutsideRoot;
                continue;
            };
            let full = relative.iter().fold(assets.clone(), |p, c| p.join(c));
            entry.state = match fs::metadata(&full) {
                Ok(meta) if meta.is_file() => FileState::Present { bytes: meta.len() },
                _ => FileState::Missing,
            };
            entry.full = Some(full);
            referenced.insert(relative.join("/"));
        }
        referenced
    }

    pub fn legacy_root_detected(&self) -> bool {
        self.legacy_root.is_some()
    }

    /// The checked paths relative to `assets/` (`/`-separated): what the
    /// orphan scan treats as referenced.
    pub fn referenced(&self) -> HashSet<String> {
        let Some(root) = self.legacy_root.as_deref() else {
            return HashSet::new();
        };
        self.entries
            .iter()
            .filter_map(|e| relative_to_assets(&e.raw, root))
            .map(|parts| parts.join("/"))
            .collect()
    }

    /// Per-class and total counts.
    pub fn counts(&self) -> (BTreeMap<String, FileClassCounts>, FileClassCounts) {
        let mut classes: BTreeMap<FileClass, FileClassCounts> = BTreeMap::new();
        for (class, refs) in &self.refs_per_class {
            classes.entry(*class).or_default().refs = *refs;
        }
        let mut totals = FileClassCounts {
            refs: self.refs_per_class.values().sum(),
            ..FileClassCounts::default()
        };
        for entry in &self.entries {
            for class in &entry.classes {
                tally(classes.entry(*class).or_default(), entry.state);
            }
            tally(&mut totals, entry.state);
        }
        let named = classes.into_iter().map(|(c, n)| (c.name(), n)).collect();
        (named, totals)
    }

    /// What `run` would upload: present files, videos separately.
    pub fn upload_estimate(&self) -> UploadEstimate {
        let mut out = UploadEstimate::default();
        for entry in &self.entries {
            let FileState::Present { bytes } = entry.state else {
                continue;
            };
            if entry.classes.iter().all(|c| c.is_video()) {
                out.files_videos += 1;
                out.bytes_videos += bytes;
            } else {
                out.files_default += 1;
                out.bytes_default += bytes;
            }
        }
        out
    }
}

fn tally(counts: &mut FileClassCounts, state: FileState) {
    counts.files += 1;
    match state {
        FileState::Unchecked => {}
        FileState::Present { bytes } => {
            counts.present += 1;
            counts.bytes_present += bytes;
        }
        FileState::Missing => counts.missing += 1,
        FileState::OutsideRoot => counts.outside_root += 1,
    }
}

/// The most common prefix before an `assets` path segment (either separator).
fn detect_root<'a>(paths: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut candidates: HashMap<&str, u64> = HashMap::new();
    for path in paths {
        if let Some(root) = root_of(path) {
            *candidates.entry(root).or_default() += 1;
        }
    }
    candidates
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(root, _)| root.to_owned())
}

/// The part of `path` before its last `/assets/` (or `\assets\`) segment.
fn root_of(path: &str) -> Option<&str> {
    let slash = path.rfind("/assets/");
    let backslash = path.rfind("\\assets\\");
    let at = slash.max(backslash)?;
    (at > 0).then(|| &path[..at])
}

/// The components of `path` after `<root>/assets/`, when they are plain names.
fn relative_to_assets(path: &str, root: &str) -> Option<Vec<String>> {
    let rest = path.strip_prefix(root)?;
    let rest = rest
        .strip_prefix("/assets/")
        .or_else(|| rest.strip_prefix("\\assets\\"))?;
    let parts: Vec<String> = rest.split(['/', '\\']).map(str::to_owned).collect();
    let safe = parts.iter().all(|p| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && matches!(Path::new(p).components().next(), Some(Component::Normal(_)))
            && Path::new(p).components().count() == 1
    });
    safe.then_some(parts)
}

/// Lists `<media root>/assets` and counts the files no row references.
pub fn scan_orphans(media_root: &Path, referenced: &HashSet<String>) -> OrphanCounts {
    let mut out = OrphanCounts::default();
    walk_orphans(
        media_root,
        referenced,
        |relative, bytes| {
            out.files += 1;
            out.bytes += bytes;
            let top = if relative.len() > 1 {
                relative[0].clone()
            } else {
                ".".to_owned()
            };
            let slot = out.by_dir.entry(top).or_default();
            slot.0 += 1;
            slot.1 += bytes;
        },
        &mut out.ignored,
    );
    out
}

/// The files under `<media root>/assets` that no row references, as paths
/// relative to `assets/`, sorted (OI-11: so the owner can delete them on the
/// desktop).
pub fn orphan_paths(media_root: &Path, referenced: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut ignored = 0;
    walk_orphans(
        media_root,
        referenced,
        |relative, _| out.push(relative.join("/")),
        &mut ignored,
    );
    out.sort();
    out
}

fn walk_orphans(
    media_root: &Path,
    referenced: &HashSet<String>,
    mut found: impl FnMut(&[String], u64),
    ignored: &mut u64,
) {
    let assets = media_root.join("assets");
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(assets, Vec::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let mut relative = prefix.clone();
            relative.push(name.clone());
            if file_type.is_symlink() || is_os_metadata(&name) {
                *ignored += 1;
            } else if file_type.is_dir() {
                stack.push((entry.path(), relative));
            } else if file_type.is_file() && !referenced.contains(&relative.join("/")) {
                let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
                found(&relative, bytes);
            }
        }
    }
}

fn is_os_metadata(name: &str) -> bool {
    matches!(name, ".DS_Store" | "Thumbs.db" | "desktop.ini") || name.starts_with("._")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_and_relative_paths() {
        let mac = "/Users/x/Library/Application Support/Shelfy/assets/images/instagram-1-0.jpg";
        let win = "C:\\Users\\x\\AppData\\Roaming\\Shelfy\\assets\\web\\a.webp";
        assert_eq!(
            root_of(mac),
            Some("/Users/x/Library/Application Support/Shelfy")
        );
        assert_eq!(root_of(win), Some("C:\\Users\\x\\AppData\\Roaming\\Shelfy"));
        assert_eq!(root_of("/elsewhere/file.jpg"), None);
        assert_eq!(
            relative_to_assets(mac, "/Users/x/Library/Application Support/Shelfy"),
            Some(vec!["images".to_owned(), "instagram-1-0.jpg".to_owned()])
        );
        assert_eq!(
            relative_to_assets(win, "C:\\Users\\x\\AppData\\Roaming\\Shelfy"),
            Some(vec!["web".to_owned(), "a.webp".to_owned()])
        );
        assert_eq!(relative_to_assets("/r/assets/../secret", "/r"), None);
        assert_eq!(relative_to_assets("/r/assets//x", "/r"), None);
        assert_eq!(relative_to_assets("/other/assets/x", "/r"), None);
    }

    #[test]
    fn the_most_common_root_wins() {
        let paths = ["/a/assets/x", "/a/assets/y", "/b/assets/z"];
        assert_eq!(detect_root(paths.into_iter()), Some("/a".to_owned()));
        assert_eq!(detect_root(["/nowhere/x"].into_iter()), None);
    }

    #[test]
    fn checks_presence_and_orphans() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("assets/images")).unwrap();
        fs::create_dir_all(root.join("assets/videos")).unwrap();
        fs::write(root.join("assets/images/a.jpg"), b"12345").unwrap();
        fs::write(root.join("assets/images/orphan.jpg"), b"123").unwrap();
        fs::write(root.join("assets/videos/v.mp4"), b"1234567890").unwrap();
        fs::write(root.join("assets/.DS_Store"), b"x").unwrap();

        let mut refs = FileRefs::default();
        let a = refs.add(FileClass::Cover, "/old/home/assets/images/a.jpg");
        refs.add(FileClass::Image, "/old/home/assets/images/a.jpg");
        let v = refs.add(FileClass::Video, "/old/home/assets/videos/v.mp4");
        let gone = refs.add(FileClass::SlideImage, "/old/home/assets/images/gone.jpg");
        let outside = refs.add(FileClass::Cover, "/tmp/elsewhere.jpg");
        let referenced = refs.check(root);

        assert!(refs.legacy_root_detected());
        assert_eq!(refs.state(a), FileState::Present { bytes: 5 });
        assert_eq!(refs.state(v), FileState::Present { bytes: 10 });
        assert_eq!(refs.state(gone), FileState::Missing);
        assert_eq!(refs.state(outside), FileState::OutsideRoot);

        let (classes, totals) = refs.counts();
        assert_eq!(totals.refs, 5);
        assert_eq!(totals.files, 4);
        assert_eq!(
            (totals.present, totals.missing, totals.outside_root),
            (2, 1, 1)
        );
        assert_eq!(classes["cover"].refs, 2);
        assert_eq!(classes["image"].files, 1);

        let upload = refs.upload_estimate();
        assert_eq!((upload.files_default, upload.bytes_default), (1, 5));
        assert_eq!((upload.files_videos, upload.bytes_videos), (1, 10));

        let orphans = scan_orphans(root, &referenced);
        assert_eq!((orphans.files, orphans.bytes), (1, 3));
        assert_eq!(orphans.by_dir["images"], (1, 3));
        assert_eq!(orphans.ignored, 1);
    }
}
