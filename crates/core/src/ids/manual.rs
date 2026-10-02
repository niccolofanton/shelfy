//! Manual bookmark identity: a ULID (plan §2.8).
//!
//! The web app mints manual ids server-side. A desktop `manual:<uuid>` post
//! gets a new ULID (§4.2); the original id is kept for traceability by the
//! migration. The ULID is derived deterministically, so re-running a migration
//! yields the same key: its time part is the import time and its random part
//! the first 80 bits of `SHA-1("shelfy-manual:" + legacy id)`.

use sha1::{Digest, Sha1};

use super::{CanonicalId, IdError, Platform};

/// The prefix of desktop manual ids (`electron/bookmarks.ts`).
pub const LEGACY_PREFIX: &str = "manual:";

/// Crockford base32, the ULID alphabet.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Largest ULID time: 48 bits of milliseconds.
const MAX_ULID_TIME_MS: u64 = (1 << 48) - 1;

/// The web identity `m_<ulid>` of a desktop manual post, imported at
/// `imported_at_ms` (unix milliseconds).
pub fn from_legacy(legacy_id: &str, imported_at_ms: i64) -> Result<CanonicalId, IdError> {
    match legacy_id.strip_prefix(LEGACY_PREFIX) {
        Some(rest) if !rest.is_empty() => {}
        _ => return Err(IdError::InvalidManualId),
    }
    let time_ms = u64::try_from(imported_at_ms)
        .unwrap_or(0)
        .min(MAX_ULID_TIME_MS);
    let digest = Sha1::digest(format!("shelfy-manual:{legacy_id}").as_bytes());
    let random = digest
        .as_slice()
        .iter()
        .take(10)
        .fold(0u128, |acc, &b| (acc << 8) | u128::from(b));
    let ulid = ulid_from_parts(time_ms, random);
    Ok(CanonicalId::new(Platform::Manual, ulid.clone(), &ulid))
}

/// Encodes a ULID: 48 bits of time (ms) then 80 random bits, as 26 Crockford
/// base32 characters. Extra high bits of either part are ignored.
pub fn ulid_from_parts(time_ms: u64, random: u128) -> String {
    let time = u128::from(time_ms & MAX_ULID_TIME_MS);
    let random = random & ((1u128 << 80) - 1);
    let mut value = (time << 80) | random;
    let mut out = [0u8; 26];
    for slot in out.iter_mut().rev() {
        *slot = CROCKFORD[(value & 31) as usize];
        value >>= 5;
    }
    out.iter().map(|&b| char::from(b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(ulid: &str) -> (u64, u128) {
        let value = ulid.bytes().fold(0u128, |acc, b| {
            let digit = CROCKFORD.iter().position(|&c| c == b).unwrap();
            (acc << 5) | digit as u128
        });
        ((value >> 80) as u64, value & ((1u128 << 80) - 1))
    }

    #[test]
    fn encodes_the_reference_ulid() {
        // The seeded example of the reference implementation:
        // `ulid(1469918176385)` → `01ARYZ6S41TSV4RRFFQ69G5FAV`.
        let (time, random) = decode("01ARYZ6S41TSV4RRFFQ69G5FAV");
        assert_eq!(time, 1_469_918_176_385);
        assert_eq!(ulid_from_parts(time, random), "01ARYZ6S41TSV4RRFFQ69G5FAV");
        assert_eq!(ulid_from_parts(time, 0), "01ARYZ6S410000000000000000");
    }

    #[test]
    fn legacy_ids_map_deterministically() {
        let legacy = "manual:6f1c2a9e-0b7d-4c55-9e2a-4d8f3b1c0a77";
        let a = from_legacy(legacy, 1_700_000_000_000).unwrap();
        let b = from_legacy(legacy, 1_700_000_000_000).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.platform(), Platform::Manual);
        assert_eq!(a.native_id().len(), 26);
        assert_eq!(a.key(), format!("m_{}", a.native_id()));
        assert_eq!(decode(a.native_id()).0, 1_700_000_000_000);
        let other = from_legacy("manual:another", 1_700_000_000_000).unwrap();
        assert_ne!(a, other);
    }

    #[test]
    fn out_of_range_times_are_clamped() {
        let negative = from_legacy("manual:x", -5).unwrap();
        assert_eq!(decode(negative.native_id()).0, 0);
        let huge = from_legacy("manual:x", i64::MAX).unwrap();
        assert_eq!(decode(huge.native_id()).0, MAX_ULID_TIME_MS);
    }

    #[test]
    fn invalid_ids_are_rejected() {
        assert_eq!(from_legacy("", 0), Err(IdError::InvalidManualId));
        assert_eq!(from_legacy("manual:", 0), Err(IdError::InvalidManualId));
        assert_eq!(from_legacy("web:abc", 0), Err(IdError::InvalidManualId));
    }
}
