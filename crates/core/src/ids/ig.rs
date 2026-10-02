//! Instagram identity: the media `pk` (plan §2.8).
//!
//! Instagram exposes the same media under three identifiers, and the desktop
//! app stored whichever one its parser saw first in `posts.id`
//! (`electron/webview-injected.ts`, `electron/ig-parser.ts`,
//! `electron/webview-select.ts`):
//!
//! - REST `item.id`: `<pk>_<owner pk>` ([`LegacyIdForm::Composite`]);
//! - GraphQL `node.id` or REST `pk`: the decimal pk ([`LegacyIdForm::Pk`]);
//! - DOM fallback: the shortcode ([`LegacyIdForm::Shortcode`]).
//!
//! The shortcode is the pk written in base 64 with the alphabet
//! `A–Z a–z 0–9 - _`. Long shortcodes (private posts) can exceed 64 bits, so
//! the pk is kept as a decimal string of any length.

use super::{CanonicalId, IdError, Platform, is_ascii_digits, trim_leading_zeros};

/// The shortcode alphabet: digit value = index.
pub const SHORTCODE_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Longest shortcode accepted, as on the desktop (real ones are ≤ ~12
/// characters for public posts; the cap bounds the decoding cost).
pub const MAX_SHORTCODE_LEN: usize = 64;

/// The Instagram id epoch (2011-08-24T21:07:01.721Z) in unix milliseconds.
pub const IG_EPOCH_MS: i64 = 1_314_220_021_721;

/// 2010-01-01T00:00:00Z: dates derived from a pk before this are rejected.
const MIN_PLAUSIBLE_MS: i64 = 1_262_304_000_000;
/// 2101-01-01T00:00:00Z: dates derived from a pk from this on are rejected.
const MAX_PLAUSIBLE_MS: i64 = 4_133_980_800_000;

/// An Instagram media pk in canonical decimal form: ASCII digits, no leading
/// zeros, never zero.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MediaPk(String);

impl MediaPk {
    /// Parses a decimal pk. Leading zeros are dropped.
    pub fn parse_decimal(value: &str) -> Result<MediaPk, IdError> {
        if value.is_empty() {
            return Err(IdError::Empty);
        }
        if !is_ascii_digits(value) {
            return Err(IdError::InvalidMediaId);
        }
        let digits = trim_leading_zeros(value);
        if digits == "0" {
            return Err(IdError::ZeroMediaPk);
        }
        Ok(MediaPk(digits.to_owned()))
    }

    /// Decodes a shortcode into its pk.
    pub fn from_shortcode(shortcode: &str) -> Result<MediaPk, IdError> {
        let value = decode_raw(shortcode)?;
        if value.is_zero() {
            return Err(IdError::ZeroMediaPk);
        }
        Ok(MediaPk(value.to_decimal()))
    }

    /// The decimal pk: the `posts.native_id` value.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The shortcode of this pk (no leading `A`, the zero digit).
    pub fn to_shortcode(&self) -> String {
        let mut value = BigUint::from_decimal(&self.0);
        let mut out = Vec::new();
        while !value.is_zero() {
            let digit = value.div_rem_small(64);
            out.push(SHORTCODE_ALPHABET[digit as usize]);
        }
        if out.is_empty() {
            out.push(SHORTCODE_ALPHABET[0]);
        }
        out.reverse();
        // The alphabet is ASCII.
        out.into_iter().map(char::from).collect()
    }

    /// The creation time embedded in the pk (its top bits are milliseconds
    /// since [`IG_EPOCH_MS`]), when it is a plausible date.
    pub fn created_at_ms(&self) -> Option<i64> {
        let value: u128 = self.0.parse().ok()?;
        created_at_from_u128(value)
    }

    /// The canonical identity `ig_<pk>`.
    pub fn canonical(&self) -> CanonicalId {
        CanonicalId::new(Platform::Instagram, self.0.clone(), &self.0)
    }
}

/// Which identifier the desktop stored in `posts.id` for an Instagram post.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyIdForm {
    /// REST `item.id`: `<pk>_<owner pk>`.
    Composite,
    /// GraphQL `node.id` or REST `pk`: the decimal pk.
    Pk,
    /// DOM or parser fallback: the shortcode.
    Shortcode,
}

impl LegacyIdForm {
    pub fn as_str(self) -> &'static str {
        match self {
            LegacyIdForm::Composite => "composite",
            LegacyIdForm::Pk => "pk",
            LegacyIdForm::Shortcode => "shortcode",
        }
    }
}

/// A decoded desktop Instagram id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyIgId {
    pub pk: MediaPk,
    pub form: LegacyIdForm,
}

/// Decodes a desktop `posts.id` of an Instagram post into its pk.
///
/// `shortcode` is the row's `posts.shortcode`. Every desktop fallback that
/// keys a post by its shortcode also stores it as the shortcode, so
/// `id == shortcode` identifies that form even for an all-digit shortcode.
pub fn parse_legacy_id(id: &str, shortcode: Option<&str>) -> Result<LegacyIgId, IdError> {
    if id.is_empty() {
        return Err(IdError::Empty);
    }
    let shortcode = shortcode.filter(|s| !s.is_empty());
    if shortcode == Some(id) {
        return Ok(LegacyIgId {
            pk: MediaPk::from_shortcode(id)?,
            form: LegacyIdForm::Shortcode,
        });
    }
    if let Some((pk, _owner)) = split_composite(id) {
        return Ok(LegacyIgId {
            pk: MediaPk::parse_decimal(pk)?,
            form: LegacyIdForm::Composite,
        });
    }
    if is_ascii_digits(id) {
        return Ok(LegacyIgId {
            pk: MediaPk::parse_decimal(id)?,
            form: LegacyIdForm::Pk,
        });
    }
    match MediaPk::from_shortcode(id) {
        Ok(pk) => Ok(LegacyIgId {
            pk,
            form: LegacyIdForm::Shortcode,
        }),
        Err(IdError::InvalidShortcode) => Err(IdError::InvalidMediaId),
        Err(err) => Err(err),
    }
}

/// Splits a REST media id `<pk>_<owner pk>` (both decimal, non-empty).
pub fn split_composite(id: &str) -> Option<(&str, &str)> {
    let (pk, owner) = id.split_once('_')?;
    (is_ascii_digits(pk) && is_ascii_digits(owner)).then_some((pk, owner))
}

/// Port of the desktop `igDateFromShortcode` (`electron/ig-parser.ts`): the
/// creation time embedded in a shortcode, in unix milliseconds, or `None`
/// when the shortcode is invalid or the date implausible.
pub fn date_from_shortcode(shortcode: &str) -> Option<i64> {
    let value = decode_raw(shortcode).ok()?;
    // A value beyond u128 decodes to a year far past 2100: the desktop rejects it too.
    created_at_from_u128(value.to_u128()?)
}

fn created_at_from_u128(value: u128) -> Option<i64> {
    let epoch = u128::try_from(IG_EPOCH_MS).ok()?;
    let ms = i64::try_from((value >> 23) + epoch).ok()?;
    (MIN_PLAUSIBLE_MS..MAX_PLAUSIBLE_MS)
        .contains(&ms)
        .then_some(ms)
}

/// Decodes a shortcode to its value, zero included.
fn decode_raw(shortcode: &str) -> Result<BigUint, IdError> {
    if shortcode.is_empty() {
        return Err(IdError::Empty);
    }
    if shortcode.len() > MAX_SHORTCODE_LEN {
        return Err(IdError::ShortcodeTooLong {
            max: MAX_SHORTCODE_LEN,
        });
    }
    let mut value = BigUint::default();
    for byte in shortcode.bytes() {
        let digit = alphabet_index(byte).ok_or(IdError::InvalidShortcode)?;
        value.mul_add_small(64, digit);
    }
    Ok(value)
}

fn alphabet_index(byte: u8) -> Option<u32> {
    let index = match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'-' => 62,
        b'_' => 63,
        _ => return None,
    };
    Some(u32::from(index))
}

/// A minimal arbitrary-precision unsigned integer: little-endian limbs in
/// base 10^9, so decimal conversion is cheap. Only what the shortcode codec
/// needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BigUint {
    limbs: Vec<u32>,
}

const LIMB_BASE: u64 = 1_000_000_000;

impl BigUint {
    fn is_zero(&self) -> bool {
        self.limbs.iter().all(|&l| l == 0)
    }

    /// `self = self * mul + add`, with `mul` and `add` below 2^32.
    fn mul_add_small(&mut self, mul: u32, add: u32) {
        let mut carry = u64::from(add);
        for limb in &mut self.limbs {
            let v = u64::from(*limb) * u64::from(mul) + carry;
            *limb = (v % LIMB_BASE) as u32;
            carry = v / LIMB_BASE;
        }
        while carry > 0 {
            self.limbs.push((carry % LIMB_BASE) as u32);
            carry /= LIMB_BASE;
        }
    }

    /// `self /= div`, returning the remainder. `div` must be non-zero.
    fn div_rem_small(&mut self, div: u32) -> u32 {
        let div = u64::from(div);
        let mut rem = 0u64;
        for limb in self.limbs.iter_mut().rev() {
            let cur = rem * LIMB_BASE + u64::from(*limb);
            *limb = (cur / div) as u32;
            rem = cur % div;
        }
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
        rem as u32
    }

    /// Parses ASCII digits (already validated).
    fn from_decimal(digits: &str) -> BigUint {
        let bytes = digits.as_bytes();
        let mut limbs = Vec::with_capacity(bytes.len() / 9 + 1);
        let mut end = bytes.len();
        while end > 0 {
            let start = end.saturating_sub(9);
            let limb = bytes[start..end]
                .iter()
                .fold(0u32, |acc, b| acc * 10 + u32::from(b - b'0'));
            limbs.push(limb);
            end = start;
        }
        let mut value = BigUint { limbs };
        while value.limbs.last() == Some(&0) {
            value.limbs.pop();
        }
        value
    }

    fn to_decimal(&self) -> String {
        let mut limbs = self.limbs.iter().rev().skip_while(|&&l| l == 0);
        let Some(first) = limbs.next() else {
            return "0".to_owned();
        };
        let mut out = first.to_string();
        for limb in limbs {
            out.push_str(&format!("{limb:09}"));
        }
        out
    }

    fn to_u128(&self) -> Option<u128> {
        let mut value: u128 = 0;
        for &limb in self.limbs.iter().rev() {
            value = value
                .checked_mul(u128::from(LIMB_BASE as u32))?
                .checked_add(u128::from(limb))?;
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// pk ↔ shortcode pairs computed independently with JavaScript BigInt and
    /// the alphabet of `electron/ig-parser.ts`.
    const PAIRS: &[(&str, &str)] = &[
        ("1", "B"),
        ("63", "_"),
        ("64", "BA"),
        ("4095", "__"),
        ("3141592653589793238", "CuZLd-iMknW"),
        ("2718281828459045235", "CW5RuvAs2Fz"),
        ("18446744073709551615", "P__________"),
        ("18446744073709551616", "QAAAAAAAAAA"),
        (
            "1234567890123456789012345678901234567890",
            "OgySB1wNvzuKy8X5bOPwrS",
        ),
        ("3191575067010950169", "CxKwJ0fLmQZ"),
        ("3466375131966345763", "DAbCdEfGhIj"),
        (
            "1739233337758648896837673315139121987130790619854852263782220902908559295",
            "-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_",
        ),
    ];

    /// `igDateFromShortcode` outputs, produced by running the desktop function.
    const DATES: &[(&str, Option<i64>)] = &[
        ("B", Some(1_314_220_021_721)),           // 2011-08-24T21:07:01.721Z
        ("BA", Some(1_314_220_021_721)),          // 2011-08-24T21:07:01.721Z
        ("CxKwJ0fLmQZ", Some(1_694_685_418_007)), // 2023-09-14T09:56:58.007Z
        ("C1a2b3c4d5E", Some(1_703_815_517_074)), // 2023-12-29T02:05:17.074Z
        ("_____", Some(1_314_220_021_848)),       // 2011-08-24T21:07:01.848Z
        ("Cz-_aZ09", Some(1_314_221_496_151)),    // 2011-08-24T21:31:36.151Z
        ("DAbCdEfGhIj", Some(1_727_444_138_519)), // 2024-09-27T13:35:38.519Z
        ("AAAAB", Some(1_314_220_021_721)),       // 2011-08-24T21:07:01.721Z
        ("Bx_Y", Some(1_314_220_021_721)),        // 2011-08-24T21:07:01.721Z
        ("Cxxxxxxxxxx", Some(1_695_994_892_476)), // 2023-09-29T13:41:32.476Z
        ("A", Some(1_314_220_021_721)),           // zero decodes to the epoch, as on the desktop
        ("", None),
        ("abc$", None),
        ("__________________", None), // year far past 2100
    ];

    #[test]
    fn known_pairs_encode_and_decode() {
        for &(pk, shortcode) in PAIRS {
            let parsed = MediaPk::parse_decimal(pk).unwrap();
            assert_eq!(parsed.to_shortcode(), shortcode, "encode {pk}");
            assert_eq!(
                MediaPk::from_shortcode(shortcode).unwrap(),
                parsed,
                "decode {shortcode}"
            );
        }
    }

    #[test]
    fn dates_match_the_desktop() {
        for &(shortcode, expected) in DATES {
            assert_eq!(date_from_shortcode(shortcode), expected, "{shortcode:?}");
        }
        let pk = MediaPk::from_shortcode("CxKwJ0fLmQZ").unwrap();
        assert_eq!(pk.created_at_ms(), Some(1_694_685_418_007));
    }

    #[test]
    fn legacy_ids_decode_to_the_same_pk() {
        let composite =
            parse_legacy_id("3191575067010950169_25025320", Some("CxKwJ0fLmQZ")).unwrap();
        assert_eq!(composite.form, LegacyIdForm::Composite);
        let pk = parse_legacy_id("3191575067010950169", Some("CxKwJ0fLmQZ")).unwrap();
        assert_eq!(pk.form, LegacyIdForm::Pk);
        let shortcode = parse_legacy_id("CxKwJ0fLmQZ", Some("CxKwJ0fLmQZ")).unwrap();
        assert_eq!(shortcode.form, LegacyIdForm::Shortcode);
        let bare_shortcode = parse_legacy_id("CxKwJ0fLmQZ", None).unwrap();
        assert_eq!(bare_shortcode.form, LegacyIdForm::Shortcode);
        for decoded in [&pk, &shortcode, &bare_shortcode] {
            assert_eq!(decoded.pk, composite.pk);
        }
        assert_eq!(composite.pk.canonical().key(), "ig_3191575067010950169");
        assert_eq!(composite.pk.canonical().native_id(), "3191575067010950169");
    }

    #[test]
    fn an_all_digit_shortcode_is_read_as_a_shortcode_when_the_row_says_so() {
        let decoded = parse_legacy_id("12345678901", Some("12345678901")).unwrap();
        assert_eq!(decoded.form, LegacyIdForm::Shortcode);
        assert_ne!(decoded.pk.as_str(), "12345678901");
        let as_pk = parse_legacy_id("12345678901", Some("CxKwJ0fLmQZ")).unwrap();
        assert_eq!(as_pk.form, LegacyIdForm::Pk);
    }

    #[test]
    fn invalid_ids_are_rejected() {
        assert_eq!(parse_legacy_id("", None), Err(IdError::Empty));
        assert_eq!(
            parse_legacy_id("abc def", None),
            Err(IdError::InvalidMediaId)
        );
        assert_eq!(parse_legacy_id("0_123", None), Err(IdError::ZeroMediaPk));
        assert_eq!(parse_legacy_id("000", None), Err(IdError::ZeroMediaPk));
        assert_eq!(parse_legacy_id("AAAA", None), Err(IdError::ZeroMediaPk));
        let long = "B".repeat(MAX_SHORTCODE_LEN + 1);
        assert_eq!(
            parse_legacy_id(&long, None),
            Err(IdError::ShortcodeTooLong {
                max: MAX_SHORTCODE_LEN
            })
        );
        assert_eq!(MediaPk::parse_decimal("12a"), Err(IdError::InvalidMediaId));
    }

    #[test]
    fn composite_split_needs_two_numeric_halves() {
        assert_eq!(split_composite("1_2"), Some(("1", "2")));
        assert_eq!(split_composite("1_"), None);
        assert_eq!(split_composite("_2"), None);
        assert_eq!(split_composite("1_2_3"), None);
        assert_eq!(split_composite("a_2"), None);
    }

    #[test]
    fn leading_zeros_are_canonicalized() {
        assert_eq!(MediaPk::parse_decimal("00064").unwrap().as_str(), "64");
        assert_eq!(MediaPk::from_shortcode("AABA").unwrap().as_str(), "64");
    }

    proptest! {
        #[test]
        fn u64_pks_round_trip(n in 1u64..) {
            let pk = MediaPk::parse_decimal(&n.to_string()).unwrap();
            let shortcode = pk.to_shortcode();
            prop_assert_eq!(MediaPk::from_shortcode(&shortcode).unwrap(), pk.clone());
            prop_assert_eq!(pk.created_at_ms().is_some(), date_from_shortcode(&shortcode).is_some());
        }

        #[test]
        fn u128_pks_round_trip(n in 1u128..) {
            let pk = MediaPk::parse_decimal(&n.to_string()).unwrap();
            prop_assert_eq!(MediaPk::from_shortcode(&pk.to_shortcode()).unwrap(), pk);
        }

        #[test]
        fn long_decimal_pks_round_trip(digits in "[1-9][0-9]{0,90}") {
            let pk = MediaPk::parse_decimal(&digits).unwrap();
            prop_assert_eq!(pk.as_str(), digits.as_str());
            prop_assert_eq!(MediaPk::from_shortcode(&pk.to_shortcode()).unwrap(), pk);
        }

        #[test]
        fn shortcodes_round_trip(shortcode in "[B-Za-z0-9_-][A-Za-z0-9_-]{0,40}") {
            let pk = MediaPk::from_shortcode(&shortcode).unwrap();
            prop_assert_eq!(pk.to_shortcode(), shortcode);
        }

        #[test]
        fn composite_ids_decode_to_their_pk(pk in 1u64.., owner in 0u64..) {
            let decoded = parse_legacy_id(&format!("{pk}_{owner}"), None).unwrap();
            prop_assert_eq!(decoded.form, LegacyIdForm::Composite);
            prop_assert_eq!(decoded.pk.as_str(), pk.to_string());
        }

        #[test]
        fn every_form_of_one_post_maps_to_one_key(n in 1u64.., owner in 0u64..) {
            let pk = MediaPk::parse_decimal(&n.to_string()).unwrap();
            let shortcode = pk.to_shortcode();
            let keys: Vec<String> = [
                parse_legacy_id(&format!("{n}_{owner}"), Some(&shortcode)),
                parse_legacy_id(&n.to_string(), Some(&shortcode)),
                parse_legacy_id(&shortcode, Some(&shortcode)),
            ]
            .into_iter()
            .map(|r| r.unwrap().pk.canonical().key().to_owned())
            .collect();
            let expected = format!("ig_{n}");
            prop_assert!(keys.iter().all(|k| *k == expected));
        }
    }
}
