//! X hydration (L17, SPIKE-9's `xSyndication` and `xOembed`): the embed
//! widget's `tweet-result`, then oEmbed.
//!
//! | Route | Request | Data |
//! |---|---|---|
//! | `tweet-result` | `GET cdn.syndication.twimg.com/tweet-result?id=<id>&lang=en&token=<t>` | text, author, date, `mediaDetails` with the video variants |
//! | oEmbed | `GET publish.x.com/oembed?url=<post>&omit_script=true&dnt=true` | text, author and date; no media |
//!
//! `tweet-result`'s token is the widget's: `((id / 1e15) · π)` written in
//! base 36 as JavaScript writes numbers ([`radix36`]), without its zeros
//! and its point. A 404, an empty answer and a tombstone are gone; a
//! tombstone about age or sensitive content is gated (no extension path on
//! X: the post fails). oEmbed's text keeps the media link as text
//! (`pic.x.com/…`): such a post keeps its placeholder type and waits for its
//! media (a later sync or share), otherwise it is a text post.

use std::sync::LazyLock;

use axum::http::Method;
use regex::Regex;
use serde_json::{Value, json};

use super::fetch::{Gate, JSON_CAP, Sent, Stop};
use super::{Found, Target, Verdict, decode_entities, str_at};
use crate::outbound::{HostGroup, Outbound, Signal};

/// The longest side a video variant may have on its short side (SPIKE-9:
/// the best MP4 at 1080p or less).
const MAX_SHORT_SIDE: u64 = 1_080;

static VARIANT_SIZE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/(\d{2,5})x(\d{2,5})/").expect("valid pattern"));
static PARAGRAPH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<p\b[^>]*>(.*?)</p>").expect("valid pattern"));
static LINK_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<a\b[^>]*>([^<]*)</a>").expect("valid pattern"));
static BREAK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>").expect("valid pattern"));
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").expect("valid pattern"));

/// The `tweet-result` URL of a tweet.
#[must_use]
pub fn syndication_url(tweet_id: &str) -> String {
    format!(
        "https://cdn.syndication.twimg.com/tweet-result?id={tweet_id}&lang=en&token={}",
        token(tweet_id)
    )
}

/// The oEmbed URL of a post link.
#[must_use]
pub fn oembed_url(post_url: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(post_url.as_bytes()).collect();
    format!("https://publish.x.com/oembed?url={encoded}&omit_script=true&dnt=true")
}

/// The widget's token: `((id / 1e15) * Math.PI).toString(36)` without
/// zeros and points.
#[must_use]
pub fn token(tweet_id: &str) -> String {
    let id: f64 = tweet_id.parse().unwrap_or(0.0);
    radix36((id / 1e15) * std::f64::consts::PI)
        .chars()
        .filter(|c| *c != '0' && *c != '.')
        .collect()
}

/// `value.toString(36)` as V8 writes it (`DoubleToRadixCString`): the
/// integer digits, then the fraction digits up to the double's precision,
/// rounded to even. For finite, non-negative values below 2⁵³.
#[must_use]
pub fn radix36(value: f64) -> String {
    const RADIX: f64 = 36.0;
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if !value.is_finite() || value < 0.0 {
        return String::new();
    }
    let mut integer = value.floor();
    let mut fraction = value - integer;
    let next = f64::from_bits(value.to_bits() + 1);
    let mut delta = (0.5 * (next - value)).max(f64::from_bits(1));
    let mut fraction_digits: Vec<u8> = Vec::new();
    if fraction >= delta {
        loop {
            fraction *= RADIX;
            delta *= RADIX;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let digit = fraction as usize;
            fraction_digits.push(DIGITS[digit]);
            #[allow(clippy::cast_precision_loss)]
            {
                fraction -= digit as f64;
            }
            if (fraction > 0.5 || (fraction == 0.5 && digit & 1 == 1)) && fraction + delta > 1.0 {
                // Round up, carrying over the digits already written.
                loop {
                    let Some(last) = fraction_digits.pop() else {
                        integer += 1.0;
                        break;
                    };
                    let digit = DIGITS.iter().position(|d| *d == last).unwrap_or(0);
                    if digit + 1 < 36 {
                        fraction_digits.push(DIGITS[digit + 1]);
                        break;
                    }
                }
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    let mut integer_digits = Vec::new();
    loop {
        let remainder = integer % RADIX;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        integer_digits.push(DIGITS[remainder as usize]);
        integer = (integer - remainder) / RADIX;
        if integer <= 0.0 {
            break;
        }
    }
    integer_digits.reverse();
    let mut out = String::from_utf8(integer_digits).unwrap_or_default();
    if !fraction_digits.is_empty() {
        out.push('.');
        out.push_str(std::str::from_utf8(&fraction_digits).unwrap_or_default());
    }
    out
}

/// Hydrates an X post: `tweet-result`, then oEmbed.
///
/// # Errors
///
/// [`Stop`] when the breaker is open or the first block signal tripped it.
pub async fn hydrate(outbound: &Outbound, target: &Target) -> Result<Verdict, Stop> {
    let mut gate = Gate::open(outbound, HostGroup::XWeb).await?;
    let syndication = gate
        .send(
            Method::GET,
            &syndication_url(&target.native_id),
            super::fetch::headers(&[]),
            None,
            JSON_CAP,
        )
        .await;
    match syndication {
        Sent::Reply(reply) => match reply.status {
            404 | 410 => return Ok(done(gate, Verdict::Gone)),
            200 => {
                if let Ok(json) = serde_json::from_str::<Value>(&reply.body) {
                    return Ok(done(gate, syndication_verdict(target, &json)));
                }
            }
            _ => {}
        },
        Sent::Blocked(block) => return Err(Stop::Blocked(block)),
        Sent::Failed(_) => {}
    }
    let oembed = gate
        .send(
            Method::GET,
            &oembed_url(&target.post_url),
            super::fetch::headers(&[]),
            None,
            JSON_CAP,
        )
        .await;
    let last = match oembed {
        Sent::Reply(reply) => match reply.status {
            404 | 410 => return Ok(done(gate, Verdict::Gone)),
            200 => {
                if let Some(verdict) = serde_json::from_str::<Value>(&reply.body)
                    .ok()
                    .and_then(|json| oembed_verdict(target, &json))
                {
                    return Ok(done(gate, verdict));
                }
                "parse_failed"
            }
            _ => "http_error",
        },
        Sent::Blocked(block) => return Err(Stop::Blocked(block)),
        Sent::Failed(code) => code,
    };
    gate.finish(Signal::Transient);
    Ok(Verdict::Failed(last))
}

fn done(gate: Gate<'_>, verdict: Verdict) -> Verdict {
    gate.finish(match verdict {
        Verdict::Found(_) => Signal::Served,
        Verdict::Gone | Verdict::Gated => Signal::Answered,
        Verdict::Failed(_) => Signal::Transient,
    });
    verdict
}

/// The verdict of a `tweet-result` answer.
fn syndication_verdict(target: &Target, json: &Value) -> Verdict {
    let Some(tweet) = json.as_object() else {
        return Verdict::Gone;
    };
    if tweet.is_empty() {
        return Verdict::Gone;
    }
    if json["__typename"] == "TweetTombstone" {
        let text = json["tombstone"].to_string().to_lowercase();
        return if ["age", "adult", "sensitive"]
            .iter()
            .any(|w| text.contains(w))
        {
            Verdict::Gated
        } else {
            Verdict::Gone
        };
    }
    let handle = str_at(&json["user"], "screen_name");
    let details: Vec<&Value> = json["mediaDetails"]
        .as_array()
        .map(|items| items.iter().collect())
        .unwrap_or_default();
    let mut media: Vec<Value> = details
        .iter()
        .filter_map(|m| {
            let url = m["media_url_https"].as_str()?;
            if is_video(m) {
                Some(json!({
                    "type": "video",
                    "url": url,
                    "videoUrl": best_variant(&m["video_info"]["variants"]),
                }))
            } else {
                Some(json!({ "type": "image", "url": url }))
            }
        })
        .collect();
    if media.is_empty()
        && let Some(photos) = json["photos"].as_array()
    {
        media = photos
            .iter()
            .filter_map(|p| p["url"].as_str())
            .map(|url| json!({ "type": "image", "url": url }))
            .collect();
    }
    let media_type = match details.first() {
        Some(first) if is_video(first) => "video",
        _ if media.len() > 1 => "images",
        _ if media.len() == 1 => "image",
        _ => "text",
    };
    let posted_at = json["created_at"]
        .as_str()
        .and_then(shelfy_core::legacy::convert::parse_iso8601_ms);
    Verdict::Found(Found {
        item: json!({
            "id": target.native_id,
            "postUrl": post_url(target, handle),
            "profileUrl": handle.map(|h| format!("https://x.com/{h}")),
            "authorUsername": handle,
            "authorName": str_at(&json["user"], "name"),
            "text": json["text"].as_str().unwrap_or_default(),
            "mediaType": media_type,
            "thumbnailUrl": media.first().map(|m| m["url"].clone()),
            "media": media,
        }),
        posted_at,
    })
}

/// `https://x.com/<handle>/status/<id>` once the author is known.
fn post_url(target: &Target, handle: Option<&str>) -> String {
    match handle {
        Some(handle) if !handle.is_empty() => {
            format!("https://x.com/{handle}/status/{}", target.native_id)
        }
        _ => target.post_url.clone(),
    }
}

fn is_video(media: &Value) -> bool {
    matches!(media["type"].as_str(), Some("video" | "animated_gif"))
}

/// The best MP4 variant whose short side is at most 1080 px (or the best
/// one when none says its size), by bitrate.
fn best_variant(variants: &Value) -> Option<String> {
    let mp4: Vec<(u64, Option<u64>, &str)> = variants
        .as_array()?
        .iter()
        .filter_map(|v| {
            let kind = v["content_type"].as_str().or(v["type"].as_str())?;
            let url = v["url"].as_str().or(v["src"].as_str())?;
            kind.contains("mp4").then(|| {
                let short = VARIANT_SIZE.captures(url).and_then(|m| {
                    let w: u64 = m[1].parse().ok()?;
                    let h: u64 = m[2].parse().ok()?;
                    Some(w.min(h))
                });
                (v["bitrate"].as_u64().unwrap_or(0), short, url)
            })
        })
        .collect();
    let bounded: Vec<_> = mp4
        .iter()
        .filter(|(_, short, _)| short.is_none_or(|s| s <= MAX_SHORT_SIDE))
        .collect();
    let pool = if bounded.is_empty() {
        mp4.iter().collect()
    } else {
        bounded
    };
    pool.into_iter()
        .max_by_key(|(bitrate, _, _)| *bitrate)
        .map(|(_, _, url)| (*url).to_owned())
}

/// The verdict of an oEmbed answer, `None` when it has no text.
fn oembed_verdict(target: &Target, json: &Value) -> Option<Verdict> {
    let html = json["html"].as_str()?;
    let paragraph = PARAGRAPH.captures(html)?.get(1)?.as_str();
    let text = decode_entities(&TAG.replace_all(&BREAK.replace_all(paragraph, "\n"), ""));
    let handle = json["author_url"]
        .as_str()
        .and_then(|url| url::Url::parse(url).ok())
        .and_then(|url| {
            url.path_segments()?
                .find(|s| !s.is_empty())
                .map(str::to_owned)
        });
    let posted_at = LINK_TEXT
        .captures_iter(html)
        .last()
        .and_then(|m| super::month_day_year_ms(&m[1]));
    // The media stay unknown: a post that has some keeps its placeholder.
    let media_type = if html.contains("pic.x.com") || html.contains("pic.twitter.com") {
        "image"
    } else {
        "text"
    };
    Some(Verdict::Found(Found {
        item: json!({
            "id": target.native_id,
            "postUrl": post_url(target, handle.as_deref()),
            "profileUrl": handle.as_ref().map(|h| format!("https://x.com/{h}")),
            "authorUsername": handle,
            "authorName": json["author_name"].as_str(),
            "text": text,
            "mediaType": media_type,
            "media": [],
        }),
        posted_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Computed with Node 24: `((Number(id) / 1e15) * Math.PI).toString(36)`.
    #[test]
    fn tokens_match_the_widget() {
        for (id, written, token_) in [
            ("20", "0.000000006dq1a2xwd93", "6dq1a2xwd93"),
            ("1700000000000000001", "44c.pgxmyurn", "44cpgxmyurn"),
            ("1234567890123456789", "2zq.ic77uqyk", "2zqic77uqyk"),
            ("1972912262446055526", "4s6.34bo42xu", "4s634bo42xu"),
            ("1", "0.000000000bhi2ay3f28n", "bhi2ay3f28n"),
            ("999999999999999999", "2f9.lc2ug9mm", "2f9lc2ug9mm"),
            ("18446744073709551615", "18ps.5lqos4lc", "18ps5lqos4lc"),
            ("1000000000000000", "3.53i5ab8p5f", "353i5ab8p5f"),
        ] {
            let value: f64 = id.parse().unwrap();
            assert_eq!(
                radix36((value / 1e15) * std::f64::consts::PI),
                written,
                "{id}"
            );
            assert_eq!(token(id), token_, "{id}");
        }
        assert_eq!(radix36(0.0), "0");
        assert_eq!(radix36(36.0), "10");
    }

    fn target() -> Target {
        Target {
            id: 1,
            key: "x_1700000000000000001".to_owned(),
            platform: shelfy_core::repo::Platform::Twitter,
            native_id: "1700000000000000001".to_owned(),
            shortcode: None,
            post_url: "https://x.com/i/status/1700000000000000001".to_owned(),
        }
    }

    #[test]
    fn the_best_variant_stays_at_or_below_1080p() {
        let variants = json!([
            { "content_type": "application/x-mpegURL", "url": "https://video.twimg.com/a.m3u8" },
            { "content_type": "video/mp4", "bitrate": 832_000, "url": "https://video.twimg.com/v/480x852/a.mp4" },
            { "content_type": "video/mp4", "bitrate": 2_176_000, "url": "https://video.twimg.com/v/720x1280/b.mp4" },
            { "content_type": "video/mp4", "bitrate": 10_368_000, "url": "https://video.twimg.com/v/1440x2560/c.mp4" }
        ]);
        assert_eq!(
            best_variant(&variants).as_deref(),
            Some("https://video.twimg.com/v/720x1280/b.mp4")
        );
    }

    #[test]
    fn tombstones_and_empty_answers() {
        assert!(matches!(
            syndication_verdict(&target(), &json!({})),
            Verdict::Gone
        ));
        let deleted = json!({ "__typename": "TweetTombstone", "tombstone": { "text": { "text": "This Post was deleted by the Post author." } } });
        assert!(matches!(
            syndication_verdict(&target(), &deleted),
            Verdict::Gone
        ));
        let adult = json!({ "__typename": "TweetTombstone", "tombstone": { "text": { "text": "Age-restricted adult content." } } });
        assert!(matches!(
            syndication_verdict(&target(), &adult),
            Verdict::Gated
        ));
    }

    #[test]
    fn oembed_gives_text_author_and_date() {
        let json = json!({
            "author_url": "https://twitter.com/studio",
            "author_name": "Studio",
            "html": "<blockquote class=\"twitter-tweet\"><p lang=\"en\" dir=\"ltr\">Hello &amp; welcome<br>line two</p>&mdash; Studio (@studio) <a href=\"https://twitter.com/studio/status/1?ref_src=twsrc\">October 1, 2026</a></blockquote>"
        });
        let Some(Verdict::Found(found)) = oembed_verdict(&target(), &json) else {
            panic!("found");
        };
        assert_eq!(found.item["text"], "Hello & welcome\nline two");
        assert_eq!(found.item["authorUsername"], "studio");
        assert_eq!(found.item["mediaType"], "text");
        assert_eq!(
            found.item["postUrl"],
            "https://x.com/studio/status/1700000000000000001"
        );
        assert_eq!(found.posted_at, Some(1_790_812_800_000));
    }
}
