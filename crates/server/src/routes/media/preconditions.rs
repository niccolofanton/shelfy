//! Conditional and range requests (RFC 9110 §13, §14) for the media route:
//! entity-tag lists, `If-Match`, `If-None-Match`, `If-Range` and a single
//! byte `Range`.
//!
//! Media responses carry a strong `ETag` and no `Last-Modified`, so the date
//! forms (`If-Modified-Since`, `If-Unmodified-Since`, a date in `If-Range`)
//! never match and are otherwise ignored, as RFC 9110 prescribes for a
//! resource without a modification date.

use axum::http::header::{IF_MATCH, IF_NONE_MATCH, IF_RANGE, RANGE};
use axum::http::{HeaderMap, HeaderName, Method};

/// What to answer, given the request's preconditions and range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 200 with the whole representation.
    Full,
    /// 206 with the bytes `start..=end`.
    Partial {
        /// First byte.
        start: u64,
        /// Last byte, inclusive.
        end: u64,
    },
    /// 304: the client's copy is current.
    NotModified,
    /// 412: an `If-Match` precondition failed.
    PreconditionFailed,
    /// 416: no byte of the range exists.
    RangeNotSatisfiable,
}

/// Evaluates the preconditions and the range of a `GET` or `HEAD` for a
/// representation of `size` bytes with the strong tag `etag` (quotes
/// included), in the order of RFC 9110 §13.2.2.
#[must_use]
pub fn evaluate(method: &Method, headers: &HeaderMap, etag: &str, size: u64) -> Outcome {
    if headers.contains_key(IF_MATCH) && !any_matches(headers, &IF_MATCH, etag, Comparison::Strong)
    {
        return Outcome::PreconditionFailed;
    }
    if headers.contains_key(IF_NONE_MATCH)
        && any_matches(headers, &IF_NONE_MATCH, etag, Comparison::Weak)
    {
        return Outcome::NotModified;
    }
    // Range is defined for GET only; HEAD describes the whole representation.
    if method != Method::GET {
        return Outcome::Full;
    }
    let Some(range) = single_value(headers, &RANGE) else {
        return Outcome::Full;
    };
    if let Some(if_range) = headers.get(IF_RANGE) {
        let current = if_range
            .to_str()
            .ok()
            .and_then(|v| parse_tag(v.trim()))
            .is_some_and(|tag| tag.matches(etag, Comparison::Strong));
        if !current {
            return Outcome::Full;
        }
    }
    byte_range(range, size)
}

/// Parses a `Range` value against a representation of `size` bytes.
///
/// One `bytes` range is served as 206. Other units, malformed values and
/// several ranges are ignored (200 with everything), which RFC 9110 allows; a
/// range starting past the end, or a zero-length suffix, is unsatisfiable.
#[must_use]
pub fn byte_range(value: &str, size: u64) -> Outcome {
    let value = value.trim();
    let Some(spec) = value
        .get(..6)
        .filter(|unit| unit.eq_ignore_ascii_case("bytes="))
        .map(|_| value[6..].trim())
    else {
        return Outcome::Full;
    };
    if spec.contains(',') {
        return Outcome::Full;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return Outcome::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        // A suffix: the last `n` bytes.
        let Some(n) = digits(last) else {
            return Outcome::Full;
        };
        if n == 0 || size == 0 {
            return Outcome::RangeNotSatisfiable;
        }
        return Outcome::Partial {
            start: size - n.min(size),
            end: size - 1,
        };
    }
    let Some(start) = digits(first) else {
        return Outcome::Full;
    };
    let end = if last.is_empty() {
        None
    } else {
        match digits(last) {
            Some(end) if end >= start => Some(end),
            _ => return Outcome::Full,
        }
    };
    if start >= size {
        return Outcome::RangeNotSatisfiable;
    }
    Outcome::Partial {
        start,
        end: end.map_or(size - 1, |end| end.min(size - 1)),
    }
}

/// A non-empty run of ASCII digits; values past `u64::MAX` saturate.
fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(text.parse().unwrap_or(u64::MAX))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Comparison {
    /// Both tags strong and equal (`If-Match`, `If-Range`).
    Strong,
    /// Equal opaque tags, weak or not (`If-None-Match`).
    Weak,
}

/// An entity tag as sent by a client: `"x"` or `W/"x"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EntityTag<'a> {
    weak: bool,
    /// The quoted part, without the quotes.
    opaque: &'a str,
}

impl EntityTag<'_> {
    /// Compares with our strong tag `etag` (quotes included).
    fn matches(self, etag: &str, comparison: Comparison) -> bool {
        let ours = etag.strip_prefix('"').and_then(|e| e.strip_suffix('"'));
        ours == Some(self.opaque) && (comparison == Comparison::Weak || !self.weak)
    }
}

/// Parses one entity tag at the start of `text`; returns it and the rest.
fn parse_tag_prefix(text: &str) -> Option<(EntityTag<'_>, &str)> {
    let (weak, rest) = match text.strip_prefix("W/") {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let body = rest.strip_prefix('"')?;
    let end = body.find('"')?;
    Some((
        EntityTag {
            weak,
            opaque: &body[..end],
        },
        &body[end + 1..],
    ))
}

/// Parses a value that must be exactly one entity tag.
fn parse_tag(text: &str) -> Option<EntityTag<'_>> {
    parse_tag_prefix(text).and_then(|(tag, rest)| rest.trim().is_empty().then_some(tag))
}

/// Whether any `name` header (`*` or a list of tags, possibly over several
/// lines) matches `etag`. A malformed member ends its line.
fn any_matches(headers: &HeaderMap, name: &HeaderName, etag: &str, comparison: Comparison) -> bool {
    headers.get_all(name).iter().any(|value| {
        let Ok(value) = value.to_str() else {
            return false;
        };
        if value.trim() == "*" {
            return true;
        }
        let mut rest = value;
        loop {
            rest = rest.trim_start_matches([' ', '\t', ',']);
            let Some((tag, after)) = parse_tag_prefix(rest) else {
                return false;
            };
            if tag.matches(etag, comparison) {
                return true;
            }
            rest = after;
        }
    })
}

/// The value of a header that must appear once; `None` when absent,
/// repeated or not visible ASCII.
fn single_value<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use proptest::prelude::*;

    use super::*;

    const ETAG: &str = "\"abc\"";

    fn headers(pairs: &[(HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(name.clone(), HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn get(pairs: &[(HeaderName, &str)]) -> Outcome {
        evaluate(&Method::GET, &headers(pairs), ETAG, 100)
    }

    #[test]
    fn ranges_follow_rfc_9110() {
        let cases: [(&str, Outcome); 22] = [
            ("bytes=0-0", Outcome::Partial { start: 0, end: 0 }),
            ("bytes=0-", Outcome::Partial { start: 0, end: 99 }),
            ("bytes=10-19", Outcome::Partial { start: 10, end: 19 }),
            ("bytes=90-200", Outcome::Partial { start: 90, end: 99 }),
            ("bytes=99-99", Outcome::Partial { start: 99, end: 99 }),
            ("bytes=-10", Outcome::Partial { start: 90, end: 99 }),
            ("bytes=-100", Outcome::Partial { start: 0, end: 99 }),
            ("bytes=-500", Outcome::Partial { start: 0, end: 99 }),
            ("BYTES=1-2", Outcome::Partial { start: 1, end: 2 }),
            (" bytes=5-6 ", Outcome::Partial { start: 5, end: 6 }),
            (
                "bytes=0-99999999999999999999999",
                Outcome::Partial { start: 0, end: 99 },
            ),
            ("bytes=100-", Outcome::RangeNotSatisfiable),
            ("bytes=100-200", Outcome::RangeNotSatisfiable),
            (
                "bytes=99999999999999999999999-",
                Outcome::RangeNotSatisfiable,
            ),
            ("bytes=-0", Outcome::RangeNotSatisfiable),
            ("bytes=5-4", Outcome::Full),
            ("bytes=0-1,5-6", Outcome::Full),
            ("bytes=a-b", Outcome::Full),
            ("bytes=-", Outcome::Full),
            ("bytes=1", Outcome::Full),
            ("items=0-1", Outcome::Full),
            ("", Outcome::Full),
        ];
        for (value, expected) in cases {
            assert_eq!(byte_range(value, 100), expected, "{value:?}");
        }
        assert_eq!(byte_range("bytes=0-", 0), Outcome::RangeNotSatisfiable);
        assert_eq!(byte_range("bytes=-1", 0), Outcome::RangeNotSatisfiable);
    }

    #[test]
    fn if_none_match_uses_the_weak_comparison() {
        for value in [
            "\"abc\"",
            "W/\"abc\"",
            "\"x\", \"abc\"",
            "\"x\",W/\"abc\"",
            "*",
        ] {
            assert_eq!(
                get(&[(IF_NONE_MATCH, value)]),
                Outcome::NotModified,
                "{value}"
            );
        }
        for value in ["\"abcd\"", "abc", "\"x\"", "\"ab\"c\"", ""] {
            assert_eq!(get(&[(IF_NONE_MATCH, value)]), Outcome::Full, "{value}");
        }
        // Several header lines form one list.
        assert_eq!(
            get(&[(IF_NONE_MATCH, "\"x\""), (IF_NONE_MATCH, "\"abc\"")]),
            Outcome::NotModified
        );
        // It applies to HEAD too, and wins over a range.
        assert_eq!(
            evaluate(&Method::HEAD, &headers(&[(IF_NONE_MATCH, ETAG)]), ETAG, 100),
            Outcome::NotModified
        );
        assert_eq!(
            get(&[(IF_NONE_MATCH, ETAG), (RANGE, "bytes=0-1")]),
            Outcome::NotModified
        );
    }

    #[test]
    fn if_match_uses_the_strong_comparison() {
        assert_eq!(get(&[(IF_MATCH, "\"abc\"")]), Outcome::Full);
        assert_eq!(get(&[(IF_MATCH, "*")]), Outcome::Full);
        assert_eq!(get(&[(IF_MATCH, "\"x\", \"abc\"")]), Outcome::Full);
        assert_eq!(get(&[(IF_MATCH, "W/\"abc\"")]), Outcome::PreconditionFailed);
        assert_eq!(get(&[(IF_MATCH, "\"x\"")]), Outcome::PreconditionFailed);
    }

    #[test]
    fn if_range_gates_the_range() {
        let range = (RANGE, "bytes=0-9");
        assert_eq!(
            get(&[range.clone(), (IF_RANGE, ETAG)]),
            Outcome::Partial { start: 0, end: 9 }
        );
        for stale in [
            "\"old\"",
            "W/\"abc\"",
            "Wed, 21 Oct 2015 07:28:00 GMT",
            "\"abc\" x",
        ] {
            assert_eq!(
                get(&[range.clone(), (IF_RANGE, stale)]),
                Outcome::Full,
                "{stale}"
            );
        }
    }

    #[test]
    fn range_is_for_get_only_and_once() {
        let range = [(RANGE, "bytes=0-9")];
        assert_eq!(
            evaluate(&Method::HEAD, &headers(&range), ETAG, 100),
            Outcome::Full
        );
        assert_eq!(
            get(&[(RANGE, "bytes=0-9"), (RANGE, "bytes=10-19")]),
            Outcome::Full
        );
    }

    proptest! {
        #[test]
        fn any_range_value_is_answered_within_bounds(value in "\\PC{0,40}", size in 0u64..10_000) {
            match byte_range(&value, size) {
                Outcome::Partial { start, end } => {
                    prop_assert!(start <= end && end < size);
                }
                Outcome::Full | Outcome::RangeNotSatisfiable => {}
                other => prop_assert!(false, "unexpected {other:?}"),
            }
        }

        #[test]
        fn valid_ranges_select_the_requested_bytes(
            size in 1u64..10_000,
            a in 0u64..12_000,
            b in 0u64..12_000,
        ) {
            let (start, end) = (a.min(b), a.max(b));
            let outcome = byte_range(&format!("bytes={start}-{end}"), size);
            if start < size {
                prop_assert_eq!(outcome, Outcome::Partial { start, end: end.min(size - 1) });
            } else {
                prop_assert_eq!(outcome, Outcome::RangeNotSatisfiable);
            }
        }
    }
}
