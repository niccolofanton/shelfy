//! Artifact validation on pinned directory descriptors, before any CAS write.
use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

use rusqlite::Connection;
use serde_json::Value;
use shelfy_core::repo::RepoError;
use shelfy_core::web::{
    AssetRole,
    captures::{self, CaptureStatus, NewAsset, NewCapture},
};
use shelfy_media::refs::{self, ObjectMeta, Origin, Role};
use shelfy_media::render::{self, RenderSpec};
use shelfy_media::store::{IngestLimits, StagedObject, UserMedia};
use shelfy_media::{MediaKind, Rendition};

use super::{
    Options,
    protocol::{self, Asset},
};
use crate::jobs::JobError;
use crate::quota;

pub struct Validated {
    pub manifest: Value,
    files: Vec<ValidatedFile>,
    pub bytes: u64,
    pub blocked: bool,
}
struct ValidatedFile {
    asset: Asset,
    page: i64,
    role: AssetRole,
    bytes: Vec<u8>,
    kind: MediaKind,
}
pub struct Prepared {
    manifest: Value,
    files: Vec<PreparedFile>,
    blocked: bool,
    opts: Options,
}
struct PreparedFile {
    asset: Asset,
    page: i64,
    role: AssetRole,
    staged: StagedObject,
    grid: Option<Vec<u8>>,
    thumbhash: Option<Vec<u8>>,
    width: Option<u32>,
    height: Option<u32>,
}

fn directory(path: &Path) -> Result<File, JobError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| protocol::invalid())
}

/// Open relative to the directory that was checked, never via a replaceable path.
#[allow(unsafe_code)]
fn open_at(dir: &File, name: &str, cap: u64) -> Result<Vec<u8>, JobError> {
    let name = CString::new(name).map_err(|_| protocol::invalid())?;
    // SAFETY: dir is a live owned descriptor, name is a NUL-terminated single
    // component; openat returns a fresh fd which File takes over only on success.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(protocol::invalid());
    }
    // SAFETY: fd was freshly opened above and is owned by this function.
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file.metadata().map_err(|_| protocol::invalid())?;
    if !meta.is_file() || meta.nlink() != 1 || meta.len() == 0 || meta.len() > cap {
        return Err(protocol::invalid());
    }
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| protocol::invalid())?;
    if bytes.len() as u64 != meta.len() {
        return Err(protocol::invalid());
    }
    Ok(bytes)
}

fn name_ok(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    (1..=64).contains(&stem.len())
        && stem
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && matches!(ext, "webp" | "png" | "jpg" | "mp4")
}

fn role(asset: &str, page: usize) -> Result<AssetRole, JobError> {
    let prefix = format!("p{page}-");
    let value = asset.strip_prefix(&prefix).ok_or_else(protocol::invalid)?;
    Ok(match value {
        "hero" => AssetRole::Hero,
        "screenshot" => AssetRole::Screenshot,
        "footer" => AssetRole::Footer,
        "video" => AssetRole::Video,
        "preview" => AssetRole::VideoPreview,
        "poster" => AssetRole::VideoPoster,
        s if s
            .strip_prefix("band")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            AssetRole::Band
        }
        s if s
            .strip_prefix("sec")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            AssetRole::Section
        }
        s if s
            .strip_prefix("frame")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            AssetRole::Filmstrip
        }
        _ => return Err(protocol::invalid()),
    })
}

fn complete_image(bytes: &[u8], kind: MediaKind) -> bool {
    match kind {
        MediaKind::Webp => {
            bytes.len() >= 12
                && u32::from_le_bytes(bytes[4..8].try_into().expect("four bytes")) as usize + 8
                    == bytes.len()
        }
        MediaKind::Png => {
            let mut at = 8usize;
            while let Some(header) = bytes.get(at..at.saturating_add(8)) {
                let len = u32::from_be_bytes(header[..4].try_into().expect("four bytes")) as usize;
                let Some(end) = at.checked_add(12).and_then(|v| v.checked_add(len)) else {
                    return false;
                };
                if end > bytes.len() {
                    return false;
                }
                if &header[4..8] == b"IEND" {
                    return len == 0 && end == bytes.len();
                }
                at = end;
            }
            false
        }
        MediaKind::Jpeg => complete_jpeg(bytes),
        _ => false,
    }
}

fn complete_jpeg(bytes: &[u8]) -> bool {
    let mut at = 2usize;
    let mut scan = false;
    while at < bytes.len() {
        if scan && bytes[at] != 0xff {
            at += 1;
            continue;
        }
        if bytes[at] != 0xff {
            return false;
        }
        while bytes.get(at) == Some(&0xff) {
            at += 1;
        }
        let Some(&marker) = bytes.get(at) else {
            return false;
        };
        at += 1;
        if scan && (marker == 0 || (0xd0..=0xd7).contains(&marker)) {
            continue;
        }
        if marker == 0xd9 {
            return at == bytes.len();
        }
        if marker == 0 || marker == 0xd8 {
            return false;
        }
        if marker == 1 {
            continue;
        }
        let Some(len) = bytes.get(at..at.saturating_add(2)) else {
            return false;
        };
        let len = u16::from_be_bytes(len.try_into().expect("two bytes")) as usize;
        if len < 2 || at.checked_add(len).is_none_or(|n| n > bytes.len()) {
            return false;
        }
        at += len;
        scan = marker == 0xda;
    }
    false
}

fn complete_mp4(bytes: &[u8]) -> bool {
    let (mut offset, mut ftyp, mut moov, mut mdat) = (0usize, false, false, false);
    while offset < bytes.len() {
        let Some(header) = bytes.get(offset..offset.saturating_add(8)) else {
            return false;
        };
        let size = u32::from_be_bytes(header[..4].try_into().expect("four bytes"));
        let (size, head) = if size == 1 {
            let Some(v) = bytes.get(offset + 8..offset + 16) else {
                return false;
            };
            let Ok(n) = usize::try_from(u64::from_be_bytes(v.try_into().expect("eight bytes")))
            else {
                return false;
            };
            (n, 16)
        } else if size == 0 {
            (bytes.len() - offset, 8)
        } else {
            (size as usize, 8)
        };
        if size < head || offset.checked_add(size).is_none_or(|n| n > bytes.len()) {
            return false;
        }
        match &header[4..8] {
            b"ftyp" => ftyp = true,
            b"moov" => moov = true,
            b"mdat" => mdat = true,
            _ => {}
        }
        offset += size;
    }
    ftyp && moov && mdat
}

pub fn validate(
    dir: &Path,
    requested_url: &str,
    opts: Options,
    blocked: bool,
) -> Result<Validated, JobError> {
    let dir = directory(dir)?;
    let manifest = protocol::manifest(
        &open_at(&dir, "manifest.json", protocol::MANIFEST_BYTES)?,
        requested_url,
        opts.max_pages,
    )?;
    let mut assets = Vec::new();
    for (index, page) in manifest["pages"]
        .as_array()
        .ok_or_else(protocol::invalid)?
        .iter()
        .enumerate()
    {
        let list: Vec<Asset> =
            serde_json::from_value(page["assets"].clone()).map_err(|_| protocol::invalid())?;
        if list.len() > 128 {
            return Err(protocol::invalid());
        }
        for asset in list {
            let role = role(&asset.role, index)?;
            assets.push((index as i64, role, asset));
        }
    }
    for (field, role) in [("og", AssetRole::Og), ("favicon", AssetRole::Favicon)] {
        if !manifest[field].is_null() {
            let object = &manifest[field];
            let asset = Asset {
                role: field.into(),
                seq: Some(0),
                file: object["file"]
                    .as_str()
                    .ok_or_else(protocol::invalid)?
                    .into(),
                w: object["w"].as_f64().ok_or_else(protocol::invalid)?,
                h: object["h"].as_f64().ok_or_else(protocol::invalid)?,
                top: None,
                css_height: None,
            };
            assets.push((captures::SITE_LEVEL, role, asset));
        }
    }
    if assets.is_empty() || (blocked && manifest["og"].is_null()) {
        return Err(JobError::permanent(if blocked {
            "capture_blocked"
        } else {
            "capture_empty"
        }));
    }
    let mut names = HashSet::new();
    let mut slots = HashSet::new();
    let mut files = Vec::new();
    let mut total = 0_u64;
    for (page, role, mut asset) in assets {
        let page = if matches!(
            role,
            AssetRole::Video | AssetRole::VideoPreview | AssetRole::VideoPoster
        ) {
            // Site-level video slots retain the source page as their sequence.
            asset.seq = Some(page);
            captures::SITE_LEVEL
        } else {
            page
        };
        if !name_ok(&asset.file)
            || !names.insert(asset.file.clone())
            || !slots.insert((page, role, asset.seq.unwrap_or(0)))
            || asset.seq.unwrap_or(0) < 0
        {
            return Err(protocol::invalid());
        }
        let bytes = open_at(
            &dir,
            &asset.file,
            if role.is_video() {
                protocol::VIDEO_BYTES
            } else {
                protocol::IMAGE_BYTES
            },
        )?;
        total += bytes.len() as u64;
        if total > protocol::SITE_BYTES {
            return Err(protocol::invalid());
        }
        let kind = MediaKind::sniff(&bytes).ok_or_else(protocol::invalid)?;
        let ext = asset.file.rsplit_once('.').ok_or_else(protocol::invalid)?.1;
        if MediaKind::from_ext(ext) != Some(kind)
            || (role.is_video() && (kind != MediaKind::Mp4 || !complete_mp4(&bytes)))
            || (!role.is_video() && !complete_image(&bytes, kind))
        {
            return Err(protocol::invalid());
        }
        if !role.is_video() {
            render::render_bytes(&bytes, RenderSpec::G480).map_err(|_| protocol::invalid())?;
        }
        files.push(ValidatedFile {
            asset,
            page,
            role,
            bytes,
            kind,
        });
    }
    if !files.iter().any(|f| {
        Some(f.asset.role.as_str()) == manifest["cover"]["role"].as_str() && !f.role.is_video()
    }) {
        return Err(protocol::invalid());
    }
    Ok(Validated {
        manifest,
        files,
        bytes: total,
        blocked,
    })
}

pub fn prepare(
    media: &UserMedia,
    validated: Validated,
    opts: Options,
) -> Result<Prepared, JobError> {
    let mut files = Vec::new();
    for file in validated.files {
        let staged = media
            .ingest(
                std::io::Cursor::new(&file.bytes),
                IngestLimits {
                    max_bytes: if file.role.is_video() {
                        protocol::VIDEO_BYTES
                    } else {
                        protocol::IMAGE_BYTES
                    },
                    ..if file.role.is_video() {
                        IngestLimits::VIDEO
                    } else {
                        IngestLimits::ARCHIVE_IMAGE
                    }
                },
            )
            .map_err(|_| protocol::invalid())?;
        let hero = file.asset.role == validated.manifest["cover"]["role"].as_str().unwrap_or("")
            || matches!(file.role, AssetRole::Hero | AssetRole::Screenshot);
        let rendered = if hero && file.kind.is_renderable() {
            Some(
                render::render_bytes(&file.bytes, RenderSpec::G480)
                    .map_err(|_| protocol::invalid())?,
            )
        } else {
            None
        };
        files.push(PreparedFile {
            asset: file.asset,
            page: file.page,
            role: file.role,
            staged,
            grid: rendered.as_ref().map(|r| r.webp.clone()),
            thumbhash: rendered.as_ref().map(|r| r.thumbhash.clone()),
            width: rendered.as_ref().map(|r| r.source_width),
            height: rendered.as_ref().map(|r| r.source_height),
        });
    }
    Ok(Prepared {
        manifest: validated.manifest,
        files,
        blocked: validated.blocked,
        opts,
    })
}

pub fn commit(
    conn: &Connection,
    media: &UserMedia,
    post_id: i64,
    job_id: i64,
    prepared: Prepared,
    now: i64,
) -> Result<(i64, u64), RepoError> {
    let mut assets = Vec::new();
    let mut cover = None;
    let mut thumbhash = None;
    let mut favicon = None;
    let mut added = 0;
    for file in prepared.files {
        added += quota::new_bytes(conn, &[(file.staged.digest(), file.staged.size())])?;
        let role = match file.role {
            AssetRole::Hero | AssetRole::Screenshot => Role::Screenshot,
            AssetRole::Band => Role::Band,
            AssetRole::Section => Role::Section,
            AssetRole::Footer => Role::Footer,
            AssetRole::Filmstrip | AssetRole::VideoPreview => Role::Filmstrip,
            AssetRole::VideoPoster => Role::Poster,
            AssetRole::Video => Role::Video,
            AssetRole::Og => Role::Og,
            AssetRole::Favicon => Role::Favicon,
        };
        let meta = ObjectMeta {
            width: file.width,
            height: file.height,
            ..ObjectMeta::new(role, Origin::Capture)
        };
        let renditions = file
            .grid
            .as_deref()
            .map(|g| vec![(Rendition::G480, g)])
            .unwrap_or_default();
        let (id, _) = refs::publish_and_record(conn, media, file.staged, &renditions, &meta, now)?;
        if file.asset.role == prepared.manifest["cover"]["role"].as_str().unwrap_or("")
            || (prepared.blocked && file.role == AssetRole::Og)
        {
            cover = Some(id);
            thumbhash = file.thumbhash;
        }
        if file.role == AssetRole::Favicon {
            favicon = Some(id);
        }
        assets.push(NewAsset {
            page_index: file.page,
            role: file.role,
            seq: file.asset.seq.unwrap_or(0),
            object_id: id,
            css_top: file.asset.top.map(|n| n.clamp(0.0, 30_000.0) as i64),
            css_height: file.asset.css_height.map(|n| n.clamp(0.0, 30_000.0) as i64),
        });
    }
    let m = prepared.manifest;
    let mut capture = NewCapture::new(now);
    capture.requested_url = m["url"].as_str().map(str::to_owned);
    capture.final_url = m["finalUrl"].as_str().map(str::to_owned);
    capture.status = if prepared.blocked {
        CaptureStatus::Blocked
    } else {
        CaptureStatus::Done
    };
    capture.partial = prepared.blocked || m["partial"].as_bool().unwrap_or(false);
    capture.engine = m["engine"].as_str().map(str::to_owned);
    capture.viewport = Some(format!(
        "{}x{}",
        m["viewport"]["width"], m["viewport"]["height"]
    ));
    capture.title = m["title"].as_str().map(str::to_owned);
    capture.palette = Some(m["palette"].clone());
    capture.fonts = Some(m["typography"]["fonts"].clone());
    capture.tech = Some(m["tech"].clone());
    capture.awards = Some(m["awards"].clone());
    capture.traits = Some(m["traits"].clone());
    capture.meta = Some(
        serde_json::json!({"description":m["description"],"capture":{"singlePage":prepared.opts.single_page,"maxPages":prepared.opts.max_pages,"jobId":job_id},"metadata":m["webMeta"],"timeline":m["timeline"]}),
    );
    capture.pages = m["pages"].as_array().cloned().unwrap_or_default();
    for page in &mut capture.pages {
        if let Some(object) = page.as_object_mut() {
            object.remove("assets");
        }
    }
    capture.hero_object = cover;
    capture.favicon_object = favicon;
    let id = captures::insert(conn, post_id, &capture, &assets, now)?;
    if let (Some(object), Some(hash)) = (cover, thumbhash) {
        refs::set_cover_thumbhash(conn, object, &hash, now)?;
    }
    Ok((id, added))
}
