//! Object file names (plan §2.5): `<sha256>.<ext>` for a stored object and
//! `<sha256>.<rendition>.webp` for a rendition. They are both the file names on
//! disk and the last segment of the `/media/…` URLs.
//!
//! A name is built only from a [`Digest`] and the allowlists, so a parsed name
//! can never point outside its shard directory: path traversal is impossible
//! by construction.

use std::fmt;

use crate::digest::Digest;
use crate::kind::MediaKind;

/// A derived image stored next to its source object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rendition {
    /// The grid rendition: WebP, at most 480 px on the long side (plan D4).
    G480,
}

impl Rendition {
    /// Every rendition.
    pub const ALL: [Self; 1] = [Self::G480];

    /// The name segment between the digest and the extension.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::G480 => "g480",
        }
    }

    /// The type of the rendition file.
    #[must_use]
    pub const fn kind(self) -> MediaKind {
        match self {
            Self::G480 => MediaKind::Webp,
        }
    }

    /// Its bit in `media_objects.variants`.
    #[must_use]
    pub const fn bit(self) -> i64 {
        match self {
            Self::G480 => 1,
        }
    }
}

/// The renditions an object has: the `media_objects.variants` bitmask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Variants(i64);

impl Variants {
    /// No rendition.
    pub const NONE: Self = Self(0);

    /// The set stored in a `media_objects.variants` value.
    #[must_use]
    pub const fn from_bits(bits: i64) -> Self {
        Self(bits)
    }

    /// The stored value.
    #[must_use]
    pub const fn bits(self) -> i64 {
        self.0
    }

    /// This set plus `rendition`.
    #[must_use]
    pub const fn with(self, rendition: Rendition) -> Self {
        Self(self.0 | rendition.bit())
    }

    /// Whether `rendition` is in the set.
    #[must_use]
    pub const fn contains(self, rendition: Rendition) -> bool {
        self.0 & rendition.bit() != 0
    }
}

/// What a file name designates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variant {
    /// The stored object itself (the master).
    Original(MediaKind),
    /// A rendition of the object.
    Rendition(Rendition),
}

/// The file name of a stored object or of one of its renditions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectName {
    /// The digest of the stored object (also for a rendition).
    pub digest: Digest,
    /// The object itself or one of its renditions.
    pub variant: Variant,
}

impl ObjectName {
    /// The name of a stored object.
    #[must_use]
    pub const fn original(digest: Digest, kind: MediaKind) -> Self {
        Self {
            digest,
            variant: Variant::Original(kind),
        }
    }

    /// The name of a rendition of a stored object.
    #[must_use]
    pub const fn rendition(digest: Digest, rendition: Rendition) -> Self {
        Self {
            digest,
            variant: Variant::Rendition(rendition),
        }
    }

    /// Parses `<64 lowercase hex>.<ext>` or `<64 lowercase hex>.<rendition>.webp`.
    /// Anything else, including other spellings of a valid name, is `None`.
    #[must_use]
    pub fn parse(file_name: &str) -> Option<Self> {
        let (hex, rest) = file_name.split_at_checked(Digest::HEX_LEN)?;
        let digest = Digest::parse_hex(hex)?;
        let rest = rest.strip_prefix('.')?;
        if let Some(kind) = MediaKind::from_ext(rest) {
            return Some(Self::original(digest, kind));
        }
        let (suffix, ext) = rest.split_once('.')?;
        Rendition::ALL
            .into_iter()
            .find(|r| r.suffix() == suffix && r.kind().ext() == ext)
            .map(|r| Self::rendition(digest, r))
    }

    /// The type of the file: the object's kind, or the rendition's.
    #[must_use]
    pub const fn kind(&self) -> MediaKind {
        match self.variant {
            Variant::Original(kind) => kind,
            Variant::Rendition(rendition) => rendition.kind(),
        }
    }
}

impl fmt::Display for ObjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.variant {
            Variant::Original(kind) => write!(f, "{}.{}", self.digest, kind.ext()),
            Variant::Rendition(r) => write!(f, "{}.{}.{}", self.digest, r.suffix(), r.kind().ext()),
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const HEX: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    proptest! {
        #[test]
        fn every_name_round_trips(
            bytes in any::<[u8; 32]>(),
            kind in 0..MediaKind::ALL.len(),
            rendition in any::<bool>(),
        ) {
            let digest = Digest::from_bytes(bytes);
            let name = if rendition {
                ObjectName::rendition(digest, Rendition::G480)
            } else {
                ObjectName::original(digest, MediaKind::ALL[kind])
            };
            let text = name.to_string();
            prop_assert!(text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.'));
            prop_assert_eq!(ObjectName::parse(&text), Some(name));
        }

        #[test]
        fn parsing_any_text_is_safe(text in "\\PC{0,90}") {
            if let Some(name) = ObjectName::parse(&text) {
                prop_assert_eq!(name.to_string(), text);
            }
        }
    }

    #[test]
    fn names_round_trip() {
        let digest = Digest::parse_hex(HEX).unwrap();
        for name in [
            ObjectName::original(digest, MediaKind::Jpeg),
            ObjectName::original(digest, MediaKind::Webp),
            ObjectName::original(digest, MediaKind::Mp4),
            ObjectName::rendition(digest, Rendition::G480),
        ] {
            assert_eq!(ObjectName::parse(&name.to_string()), Some(name));
        }
        assert_eq!(
            ObjectName::rendition(digest, Rendition::G480).to_string(),
            format!("{HEX}.g480.webp")
        );
        assert_eq!(
            ObjectName::parse(&format!("{HEX}.g480.webp"))
                .unwrap()
                .kind(),
            MediaKind::Webp
        );
    }

    #[test]
    fn other_spellings_and_traversal_are_refused() {
        for bad in [
            String::new(),
            HEX.to_owned(),
            format!("{HEX}."),
            format!("{HEX}.jpeg"),
            format!("{HEX}.JPG"),
            format!("{HEX}.svg"),
            format!("{HEX}.html"),
            format!("{HEX}.g480.jpg"),
            format!("{HEX}.g480"),
            format!("{HEX}.g960.webp"),
            format!("{HEX}.jpg/"),
            format!("{HEX}.jpg.webp"),
            format!("{}.jpg", HEX.to_uppercase()),
            format!("{}.jpg", &HEX[1..]),
            format!("../{HEX}.jpg"),
            format!("{}/../{}.jpg", &HEX[..2], &HEX[..61]),
            "ééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééé.jpg".to_owned(),
        ] {
            assert_eq!(ObjectName::parse(&bad), None, "{bad:?}");
        }
    }

    #[test]
    fn variants_are_a_bitmask() {
        let none = Variants::NONE;
        assert!(!none.contains(Rendition::G480));
        let g480 = none.with(Rendition::G480);
        assert!(g480.contains(Rendition::G480));
        assert_eq!(g480.bits(), 1);
        assert_eq!(Variants::from_bits(3).with(Rendition::G480).bits(), 3);
    }
}
