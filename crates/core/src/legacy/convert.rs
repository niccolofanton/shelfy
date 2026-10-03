//! Value conversions from the desktop representation (plan §4.2).

use serde::Serialize;

/// The desktop `posts.timestamp` (ISO 8601 text with `''` and NULL
/// sentinels), classified for the `posted_at` mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timestamp {
    /// A valid ISO 8601 date-time, in unix milliseconds.
    Valid(i64),
    /// `''`: the legacy "no date" sentinel.
    Empty,
    /// NULL.
    Null,
    /// Anything else: maps to NULL.
    Invalid,
}

/// Classifies a desktop `posts.timestamp`.
pub fn classify_timestamp(value: Option<&str>) -> Timestamp {
    match value {
        None => Timestamp::Null,
        Some(s) if s.trim().is_empty() => Timestamp::Empty,
        Some(s) => parse_iso8601_ms(s.trim()).map_or(Timestamp::Invalid, Timestamp::Valid),
    }
}

/// Parses an ISO 8601 / RFC 3339 date-time into unix milliseconds.
///
/// Accepted: `YYYY-MM-DD` (UTC midnight, as JavaScript reads it) and
/// `YYYY-MM-DD[T ]HH:MM[:SS[.fraction]]` followed by `Z` or `±HH:MM`/`±HHMM`.
/// The desktop writes `Date#toISOString()` output
/// (`2023-09-14T09:56:58.007Z`). A date-time without a zone is rejected:
/// JavaScript would read it in the desktop's local time zone, which is unknown
/// here.
pub fn parse_iso8601_ms(value: &str) -> Option<i64> {
    let b = value.as_bytes();
    let year = digits(b, 0, 4)?;
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return None;
    }
    let month = digits(b, 5, 2)?;
    let day = digits(b, 8, 2)?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if b.len() == 10 {
        return Some(days * 86_400_000);
    }
    if !matches!(b.get(10), Some(b'T' | b't' | b' ')) {
        return None;
    }
    let hour = digits(b, 11, 2)?;
    if b.get(13) != Some(&b':') {
        return None;
    }
    let minute = digits(b, 14, 2)?;
    let mut i = 16;
    let mut second = 0;
    let mut millis = 0;
    if b.get(i) == Some(&b':') {
        second = digits(b, i + 1, 2)?;
        i += 3;
        if b.get(i) == Some(&b'.') {
            i += 1;
            let start = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == start {
                return None;
            }
            // Milliseconds: the first three fraction digits, truncated.
            let frac = &b[start..i.min(start + 3)];
            millis = frac
                .iter()
                .fold(0i64, |acc, d| acc * 10 + i64::from(d - b'0'))
                * 10i64.pow(3 - frac.len() as u32);
        }
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let offset_minutes = match b.get(i) {
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(&sign @ (b'+' | b'-')) => {
            let oh = digits(b, i + 1, 2)?;
            let (om, end) = if b.get(i + 3) == Some(&b':') {
                (digits(b, i + 4, 2)?, i + 6)
            } else {
                (digits(b, i + 3, 2)?, i + 5)
            };
            if end != b.len() || oh > 23 || om > 59 {
                return None;
            }
            let total = oh * 60 + om;
            if sign == b'+' { total } else { -total }
        }
        _ => return None,
    };
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_minutes * 60;
    Some(seconds * 1_000 + millis)
}

/// `count` ASCII digits at `at`, as a number.
fn digits(b: &[u8], at: usize, count: usize) -> Option<i64> {
    let slice = b.get(at..at + count)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(
        slice
            .iter()
            .fold(0i64, |acc, d| acc * 10 + i64::from(d - b'0')),
    )
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A desktop epoch column (`imported_at`, `created_at`, …: unix seconds from
/// SQLite `unixepoch()`), classified for the seconds → ms mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EpochClass {
    Null,
    /// Unix seconds between 2000 and 2100: × 1000.
    Seconds,
    /// Already milliseconds between 2000 and 2100: kept.
    Milliseconds,
    /// Anything else.
    Implausible,
}

const SECONDS_2000: i64 = 946_684_800;
const SECONDS_2100: i64 = 4_102_444_800;

/// Classifies an epoch value written in seconds by the desktop.
pub fn classify_epoch_seconds(value: Option<i64>) -> EpochClass {
    match value {
        None => EpochClass::Null,
        Some(v) if (SECONDS_2000..SECONDS_2100).contains(&v) => EpochClass::Seconds,
        Some(v) if (SECONDS_2000 * 1_000..SECONDS_2100 * 1_000).contains(&v) => {
            EpochClass::Milliseconds
        }
        Some(_) => EpochClass::Implausible,
    }
}

/// Converts a desktop epoch (seconds, or already milliseconds) to ms.
pub fn epoch_to_ms(value: Option<i64>) -> Option<i64> {
    match classify_epoch_seconds(value) {
        EpochClass::Seconds => value.map(|v| v * 1_000),
        EpochClass::Milliseconds => value,
        EpochClass::Null | EpochClass::Implausible => None,
    }
}

/// A JSON-array-of-strings column (`ai_tags`, `user_tags`, …), classified the
/// way the desktop's `parseTags` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonArrayClass {
    Null,
    /// `[]` or `''`.
    Empty,
    /// A non-empty array whose items are all strings.
    Strings,
    /// An array with non-string items (the desktop keeps them as-is).
    Mixed,
    /// Not JSON, or not an array: the desktop reads `[]`.
    Invalid,
}

/// Classifies a JSON-array-of-strings column.
pub fn classify_json_array(value: Option<&str>) -> JsonArrayClass {
    let Some(raw) = value else {
        return JsonArrayClass::Null;
    };
    if raw.is_empty() {
        return JsonArrayClass::Empty;
    }
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Array(items)) if items.is_empty() => JsonArrayClass::Empty,
        Ok(serde_json::Value::Array(items)) if items.iter().all(|v| v.is_string()) => {
            JsonArrayClass::Strings
        }
        Ok(serde_json::Value::Array(_)) => JsonArrayClass::Mixed,
        _ => JsonArrayClass::Invalid,
    }
}

/// The non-empty strings of a JSON array column, trimmed (the desktop's
/// `parseTags` + trim). Invalid JSON reads as empty.
pub fn json_string_array(value: Option<&str>) -> Vec<String> {
    let Some(raw) = value.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// The expiry of a signed Instagram/Facebook CDN URL; it lives with ingest
/// ([`crate::ingest::hosts`]) and is re-exported here for the legacy reader.
pub use crate::ingest::hosts::cdn_url_expiry_ms;

/// A string that is a local filesystem path, as the desktop stores them
/// (absolute POSIX or Windows paths), rather than a URL.
pub fn is_local_path(value: &str) -> bool {
    let b = value.as_bytes();
    value.starts_with('/')
        || value.starts_with("\\\\")
        || (b.len() > 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'\\' | b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn iso_timestamps_parse_to_ms() {
        // Values checked with JavaScript `Date.parse`.
        let cases = [
            ("2011-08-24T21:07:01.721Z", 1_314_220_021_721),
            ("2023-09-14T09:56:58.007Z", 1_694_685_418_007),
            ("2024-09-27T13:35:38.519Z", 1_727_444_138_519),
            ("1970-01-01T00:00:00.000Z", 0),
            ("2010-01-01T00:00:00Z", 1_262_304_000_000),
            ("2101-01-01T00:00:00.000Z", 4_133_980_800_000),
            ("2024-02-29T12:00:00+02:00", 1_709_200_800_000),
            ("2024-02-29T12:00:00-0130", 1_709_213_400_000),
            ("2024-02-29T12:00Z", 1_709_208_000_000),
            ("2024-02-29T12:00:00.5Z", 1_709_208_000_500),
            ("2024-02-29T12:00:00.123456Z", 1_709_208_000_123),
            ("2024-02-29 12:00:00Z", 1_709_208_000_000),
            ("2024-02-29", 1_709_164_800_000),
        ];
        for (input, ms) in cases {
            assert_eq!(parse_iso8601_ms(input), Some(ms), "{input}");
        }
    }

    #[test]
    fn invalid_timestamps_are_rejected() {
        for input in [
            "",
            "2023-02-29T00:00:00Z",
            "2023-13-01T00:00:00Z",
            "2023-01-01T24:00:00Z",
            "2023-01-01T00:00:00",
            "2023-01-01T00:00:00.Z",
            "2023-01-01T00:00:00Zx",
            "Thu, 01 Jan 2023 00:00:00 GMT",
            "1700000000",
            "2023-1-01",
        ] {
            assert_eq!(parse_iso8601_ms(input), None, "{input}");
        }
    }

    #[test]
    fn timestamp_classes() {
        assert_eq!(classify_timestamp(None), Timestamp::Null);
        assert_eq!(classify_timestamp(Some("")), Timestamp::Empty);
        assert_eq!(classify_timestamp(Some("nope")), Timestamp::Invalid);
        assert_eq!(
            classify_timestamp(Some("1970-01-01T00:00:01.000Z")),
            Timestamp::Valid(1_000)
        );
    }

    #[test]
    fn epochs() {
        assert_eq!(classify_epoch_seconds(None), EpochClass::Null);
        assert_eq!(
            classify_epoch_seconds(Some(1_700_000_000)),
            EpochClass::Seconds
        );
        assert_eq!(
            classify_epoch_seconds(Some(1_700_000_000_000)),
            EpochClass::Milliseconds
        );
        assert_eq!(classify_epoch_seconds(Some(5)), EpochClass::Implausible);
        assert_eq!(epoch_to_ms(Some(1_700_000_000)), Some(1_700_000_000_000));
        assert_eq!(
            epoch_to_ms(Some(1_700_000_000_000)),
            Some(1_700_000_000_000)
        );
        assert_eq!(epoch_to_ms(Some(5)), None);
    }

    #[test]
    fn json_arrays() {
        assert_eq!(classify_json_array(None), JsonArrayClass::Null);
        assert_eq!(classify_json_array(Some("")), JsonArrayClass::Empty);
        assert_eq!(classify_json_array(Some("[]")), JsonArrayClass::Empty);
        assert_eq!(
            classify_json_array(Some(r#"["a","b"]"#)),
            JsonArrayClass::Strings
        );
        assert_eq!(
            classify_json_array(Some(r#"["a",1]"#)),
            JsonArrayClass::Mixed
        );
        assert_eq!(
            classify_json_array(Some(r#"{"a":1}"#)),
            JsonArrayClass::Invalid
        );
        assert_eq!(classify_json_array(Some("[oops")), JsonArrayClass::Invalid);
        assert_eq!(json_string_array(Some(r#"[" a ","",1,"b"]"#)), ["a", "b"]);
        assert!(json_string_array(Some("nope")).is_empty());
    }

    #[test]
    fn cdn_expiry_is_reexported() {
        let url =
            "https://scontent.cdninstagram.com/v/t51/x.jpg?stp=dst&_nc_ht=x&oe=65A1B2C3&_nc_sid=1";
        assert_eq!(cdn_url_expiry_ms(url), Some(0x65A1_B2C3 * 1_000));
    }

    #[test]
    fn local_paths() {
        assert!(is_local_path(
            "/Users/someone/Library/Application Support/Shelfy/assets/web/a.webp"
        ));
        assert!(is_local_path(
            "C:\\Users\\someone\\AppData\\Roaming\\Shelfy\\assets\\x.jpg"
        ));
        assert!(is_local_path("\\\\server\\share\\x.jpg"));
        assert!(!is_local_path("https://example.com/a.png"));
        assert!(!is_local_path("data:image/png;base64,AAAA"));
        assert!(!is_local_path(""));
    }

    proptest! {
        #[test]
        fn iso_strings_round_trip(ms in 0i64..4_102_444_800_000) {
            let secs = ms.div_euclid(1_000);
            let days = secs.div_euclid(86_400);
            let rem = secs.rem_euclid(86_400);
            let (y, m, d) = civil_from_days(days);
            let iso = format!(
                "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
                rem / 3_600,
                rem % 3_600 / 60,
                rem % 60,
                ms.rem_euclid(1_000)
            );
            prop_assert_eq!(parse_iso8601_ms(&iso), Some(ms));
        }
    }

    /// Inverse of `days_from_civil` (test oracle).
    fn civil_from_days(z: i64) -> (i64, i64, i64) {
        let z = z + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (if m <= 2 { y + 1 } else { y }, m, d)
    }
}
