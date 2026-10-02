//! Server-minted identifiers: ULIDs for users, request ids and, later, jobs,
//! uploads and exports (plan §2.6).

use std::time::{SystemTime, UNIX_EPOCH};

use shelfy_core::ids::manual::ulid_from_parts;

/// A new ULID: the current time in milliseconds and 80 random bits, as 26
/// Crockford base32 characters (sortable by creation time).
///
/// # Panics
///
/// When the operating system's random number generator fails, which leaves
/// nothing safe to continue with.
#[must_use]
pub fn new_ulid() -> String {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).expect("the OS random number generator failed");
    ulid_from_parts(now_ms_u64(), u128::from_be_bytes(random))
}

/// The current unix time in milliseconds.
#[must_use]
pub fn now_ms() -> i64 {
    i64::try_from(now_ms_u64()).unwrap_or(i64::MAX)
}

fn now_ms_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulids_are_unique_well_formed_user_ids() {
        let a = new_ulid();
        let b = new_ulid();
        assert_ne!(a, b);
        for id in [&a, &b] {
            assert_eq!(id.len(), 26);
            assert!(
                id.bytes()
                    .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
            );
        }
        // Sortable: the time prefix of a later id is never smaller.
        assert!(a[..10] <= b[..10]);
    }
}
