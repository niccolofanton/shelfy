//! The media type allowlist (plan §2.5, §7.1): what the store accepts, the
//! fixed file extension and MIME type of each type, and magic-byte sniffing.
//!
//! The type of an object is always sniffed from its first bytes. A declared
//! type (a `Content-Type`, a file name) is never trusted: it decides nothing.
//! SVG, HTML and every other text format are absent on purpose, because they
//! cannot be sniffed reliably and could carry script.

/// How many leading bytes [`MediaKind::sniff`] looks at. Shorter input is fine:
/// a file shorter than this is sniffed from what it has.
pub const SNIFF_LEN: usize = 64;

/// A media type the store accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// JPEG (`image/jpeg`).
    Jpeg,
    /// PNG, including APNG (`image/png`).
    Png,
    /// GIF, including animated GIF (`image/gif`).
    Gif,
    /// WebP, including animated WebP (`image/webp`).
    Webp,
    /// AVIF (`image/avif`). Stored and served, but not decoded: it gets no
    /// rendition.
    Avif,
    /// MP4 (`video/mp4`).
    Mp4,
    /// QuickTime (`video/quicktime`).
    Mov,
    /// WebM (`video/webm`).
    Webm,
    /// PDF (`application/pdf`). Served as an attachment.
    Pdf,
}

impl MediaKind {
    /// Every kind, in a stable order.
    pub const ALL: [Self; 9] = [
        Self::Jpeg,
        Self::Png,
        Self::Gif,
        Self::Webp,
        Self::Avif,
        Self::Mp4,
        Self::Mov,
        Self::Webm,
        Self::Pdf,
    ];

    /// The file extension (lowercase, no dot): `media_objects.ext` and the
    /// suffix of the object's file name.
    #[must_use]
    pub const fn ext(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Webm => "webm",
            Self::Pdf => "pdf",
        }
    }

    /// The MIME type: `media_objects.mime` and the served `Content-Type`.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
            Self::Mp4 => "video/mp4",
            Self::Mov => "video/quicktime",
            Self::Webm => "video/webm",
            Self::Pdf => "application/pdf",
        }
    }

    /// The kind with this extension; `None` outside the allowlist. Only the
    /// canonical, lowercase extension matches (`jpg`, not `jpeg` or `JPG`).
    #[must_use]
    pub fn from_ext(ext: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.ext() == ext)
    }

    /// Whether it is a still or animated image.
    #[must_use]
    pub const fn is_image(self) -> bool {
        matches!(
            self,
            Self::Jpeg | Self::Png | Self::Gif | Self::Webp | Self::Avif
        )
    }

    /// Whether it is a video.
    #[must_use]
    pub const fn is_video(self) -> bool {
        matches!(self, Self::Mp4 | Self::Mov | Self::Webm)
    }

    /// Whether the image pipeline decodes it ([`crate::render`]).
    #[must_use]
    pub const fn is_renderable(self) -> bool {
        matches!(self, Self::Jpeg | Self::Png | Self::Gif | Self::Webp)
    }

    /// The kind whose magic bytes start `head`; `None` when nothing in the
    /// allowlist matches. Looks at the first [`SNIFF_LEN`] bytes at most.
    #[must_use]
    pub fn sniff(head: &[u8]) -> Option<Self> {
        let head = &head[..head.len().min(SNIFF_LEN)];
        if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if head.starts_with(b"%PDF-") {
            Some(Self::Pdf)
        } else if is_webm(head) {
            Some(Self::Webm)
        } else {
            iso_bmff(head)
        }
    }
}

/// Major brands of an MP4 video.
const MP4_BRANDS: [&[u8; 4]; 17] = [
    b"isom", b"iso2", b"iso3", b"iso4", b"iso5", b"iso6", b"iso7", b"iso8", b"iso9", b"mp41",
    b"mp42", b"avc1", b"dash", b"M4V ", b"mmp4", b"MSNV", b"f4v ",
];
/// Brands of an AVIF image or image sequence.
const AVIF_BRANDS: [&[u8; 4]; 2] = [b"avif", b"avis"];
/// Generic HEIF brands that an AVIF file may carry as its major brand.
const HEIF_BRANDS: [&[u8; 4]; 3] = [b"mif1", b"msf1", b"miaf"];

/// Classifies an ISO base media file (MP4, QuickTime, AVIF) by the brands of
/// its leading `ftyp` box. HEIC, 3GP and audio-only brands are refused.
fn iso_bmff(head: &[u8]) -> Option<MediaKind> {
    if head.len() < 12 || &head[4..8] != b"ftyp" {
        return None;
    }
    let box_len = usize::try_from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]))
        .unwrap_or(usize::MAX);
    let major: &[u8] = &head[8..12];
    let (compatible, _) = head
        .get(16..box_len.min(head.len()))
        .unwrap_or_default()
        .as_chunks::<4>();
    let is_one_of = |brand: &[u8], set: &[&[u8; 4]]| set.iter().any(|b| b.as_slice() == brand);
    if is_one_of(major, &AVIF_BRANDS)
        || (is_one_of(major, &HEIF_BRANDS) && compatible.iter().any(|b| is_one_of(b, &AVIF_BRANDS)))
    {
        Some(MediaKind::Avif)
    } else if major == b"qt  " {
        Some(MediaKind::Mov)
    } else if is_one_of(major, &MP4_BRANDS) {
        Some(MediaKind::Mp4)
    } else {
        None
    }
}

/// Whether `head` is an EBML (Matroska family) header whose `DocType` is
/// `webm`. Plain Matroska is refused.
fn is_webm(head: &[u8]) -> bool {
    if !head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return false;
    }
    // The DocType element (ID 0x4282) sits in the EBML header, within the
    // first few dozen bytes.
    let Some(at) = head[4..].windows(2).position(|w| w == [0x42, 0x82]) else {
        return false;
    };
    let rest = &head[4 + at + 2..];
    match ebml_vint(rest) {
        Some((len, used)) => rest.get(used..used.saturating_add(len)) == Some(b"webm".as_slice()),
        None => false,
    }
}

/// Reads an EBML variable-length integer: `(value, bytes used)`.
fn ebml_vint(bytes: &[u8]) -> Option<(usize, usize)> {
    let first = *bytes.first()?;
    let width = usize::try_from(first.leading_zeros()).ok()? + 1;
    if width > 8 {
        return None;
    }
    let mut value = u64::from(u32::from(first) & (0xFF >> width));
    for &byte in bytes.get(1..width)? {
        value = (value << 8) | u64::from(byte);
    }
    Some((usize::try_from(value).ok()?, width))
}

/// A set of [`MediaKind`]s, for example the types one ingest path accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KindSet(u16);

impl KindSet {
    /// No kind.
    pub const NONE: Self = Self(0);
    /// Every kind of the allowlist.
    pub const ALL: Self = Self::NONE
        .with(MediaKind::Jpeg)
        .with(MediaKind::Png)
        .with(MediaKind::Gif)
        .with(MediaKind::Webp)
        .with(MediaKind::Avif)
        .with(MediaKind::Mp4)
        .with(MediaKind::Mov)
        .with(MediaKind::Webm)
        .with(MediaKind::Pdf);
    /// The image kinds.
    pub const IMAGES: Self = Self::NONE
        .with(MediaKind::Jpeg)
        .with(MediaKind::Png)
        .with(MediaKind::Gif)
        .with(MediaKind::Webp)
        .with(MediaKind::Avif);
    /// The video kinds.
    pub const VIDEOS: Self = Self::NONE
        .with(MediaKind::Mp4)
        .with(MediaKind::Mov)
        .with(MediaKind::Webm);

    /// This set plus `kind`.
    #[must_use]
    pub const fn with(self, kind: MediaKind) -> Self {
        Self(self.0 | bit(kind))
    }

    /// Whether `kind` is in the set.
    #[must_use]
    pub const fn contains(self, kind: MediaKind) -> bool {
        self.0 & bit(kind) != 0
    }
}

const fn bit(kind: MediaKind) -> u16 {
    1 << (kind as u16)
}

#[cfg(test)]
mod tests {
    use proptest::collection::vec;
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn sniffing_any_bytes_is_safe(bytes in vec(any::<u8>(), 0..160)) {
            if let Some(kind) = MediaKind::sniff(&bytes) {
                // Only the head decides: more bytes after it change nothing.
                let mut longer = bytes[..bytes.len().min(SNIFF_LEN)].to_vec();
                longer.extend_from_slice(&[0xAB; 32]);
                prop_assert_eq!(MediaKind::sniff(&longer), Some(kind));
            }
        }

        #[test]
        fn hostile_container_headers_are_safe(
            len in any::<u32>(),
            brands in vec(any::<[u8; 4]>(), 0..12),
            vint in vec(any::<u8>(), 0..12),
        ) {
            let mut iso = len.to_be_bytes().to_vec();
            iso.extend_from_slice(b"ftyp");
            brands.iter().for_each(|b| iso.extend_from_slice(b));
            let _ = MediaKind::sniff(&iso);
            let mut ebml = vec![0x1A, 0x45, 0xDF, 0xA3, 0x42, 0x82];
            ebml.extend_from_slice(&vint);
            let _ = MediaKind::sniff(&ebml);
        }
    }

    fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let len = 16 + 4 * compatible.len();
        let mut b = u32::try_from(len).unwrap().to_be_bytes().to_vec();
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(major);
        b.extend_from_slice(&[0, 0, 0, 0]);
        for brand in compatible {
            b.extend_from_slice(*brand);
        }
        b.extend_from_slice(b"\0\0\0\x08free");
        b
    }

    fn webm(doc_type: &[u8]) -> Vec<u8> {
        let mut b = vec![0x1A, 0x45, 0xDF, 0xA3, 0x9F];
        b.extend_from_slice(&[0x42, 0x86, 0x81, 0x01]); // EBMLVersion 1
        b.extend_from_slice(&[0x42, 0xF7, 0x81, 0x01]); // EBMLReadVersion 1
        b.extend_from_slice(&[0x42, 0x82, 0x80 | u8::try_from(doc_type.len()).unwrap()]);
        b.extend_from_slice(doc_type);
        b
    }

    #[test]
    fn every_kind_is_sniffed_from_its_magic_bytes() {
        let cases: Vec<(Vec<u8>, MediaKind)> = vec![
            (b"\xFF\xD8\xFF\xE0\0\x10JFIF\0".to_vec(), MediaKind::Jpeg),
            (b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(), MediaKind::Png),
            (b"GIF89a\x01\0\x01\0".to_vec(), MediaKind::Gif),
            (b"GIF87a\x01\0\x01\0".to_vec(), MediaKind::Gif),
            (b"RIFF\x24\0\0\0WEBPVP8 ".to_vec(), MediaKind::Webp),
            (ftyp(b"avif", &[b"mif1", b"miaf"]), MediaKind::Avif),
            (ftyp(b"mif1", &[b"avif", b"miaf"]), MediaKind::Avif),
            (ftyp(b"avis", &[b"msf1"]), MediaKind::Avif),
            (
                ftyp(b"isom", &[b"isom", b"iso2", b"avc1", b"mp41"]),
                MediaKind::Mp4,
            ),
            (ftyp(b"mp42", &[b"mp42", b"isom"]), MediaKind::Mp4),
            (ftyp(b"dash", &[b"iso6", b"mp41"]), MediaKind::Mp4),
            (ftyp(b"qt  ", &[b"qt  "]), MediaKind::Mov),
            (webm(b"webm"), MediaKind::Webm),
            (b"%PDF-1.7\n".to_vec(), MediaKind::Pdf),
        ];
        for (bytes, kind) in cases {
            assert_eq!(MediaKind::sniff(&bytes), Some(kind), "{kind:?}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        let refused: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"\xFF\xD8".to_vec(),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
            b"<!DOCTYPE html><html>".to_vec(),
            b"<?xml version=\"1.0\"?>".to_vec(),
            b"RIFF\x24\0\0\0WAVEfmt ".to_vec(),
            b"BM\x36\0\0\0".to_vec(),
            b"II*\0\x08\0\0\0".to_vec(),
            b"PK\x03\x04".to_vec(),
            ftyp(b"heic", &[b"mif1", b"heic"]),
            ftyp(b"mif1", &[b"heic"]),
            ftyp(b"3gp5", &[b"3gp5", b"isom"]),
            ftyp(b"M4A ", &[b"M4A ", b"isom"]),
            webm(b"matroska"),
            b"\x1A\x45\xDF\xA3".to_vec(),
            b"%PD".to_vec(),
        ];
        for bytes in refused {
            assert_eq!(MediaKind::sniff(&bytes), None, "{bytes:?}");
        }
    }

    #[test]
    fn extensions_and_mime_types_round_trip() {
        for kind in MediaKind::ALL {
            assert_eq!(MediaKind::from_ext(kind.ext()), Some(kind));
            assert!(
                kind.ext()
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            );
            assert!(KindSet::ALL.contains(kind));
            assert_eq!(KindSet::IMAGES.contains(kind), kind.is_image());
            assert_eq!(KindSet::VIDEOS.contains(kind), kind.is_video());
            assert!(!KindSet::NONE.contains(kind));
            assert!(!kind.is_renderable() || kind.is_image());
        }
        for other in ["jpeg", "JPG", "svg", "html", "", "../jpg"] {
            assert_eq!(MediaKind::from_ext(other), None, "{other}");
        }
    }

    #[test]
    fn a_long_ftyp_box_is_read_only_as_far_as_the_head_goes() {
        let mut long = ftyp(b"mif1", &[b"miaf"; 20]);
        long[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(MediaKind::sniff(&long), None);
    }
}
