//! Instagram hydration on the server (L17, SPIKE-9's `igPage` and
//! `igGraphql`): the post page, then the logged-out GraphQL query. The
//! extension's `hydrate_link` takes the posts the server finds gated, and
//! every post while the `instagram_web` breaker is open.
//!
//! | Route | Request | Data |
//! |---|---|---|
//! | post page | `GET /p/<code>/` as a document load | the inline JSON's `xig_polaris_media.if_not_gated_logged_out` |
//! | GraphQL | `POST /api/graphql`, `PolarisLoggedOutDesktopWWWPostRootContentQuery` with `{media_id}`, and the LSD token of the home page (one `GET /` per 10 minutes) | `data.xig_polaris_media.if_not_gated_logged_out` |
//!
//! A media object whose `if_not_gated_logged_out` is null is gated; a page
//! whose `pageID` is `httpErrorPage`, a 404 and a GraphQL answer without
//! the object are gone. The recipe (doc id, friendly name, `X-ASBD-ID`) is
//! yt-dlp 2026.08.19's; SPIKE-9 keeps it as data to update when it drifts.

use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::Duration;

use axum::http::Method;
use regex::Regex;
use serde_json::{Value, json};
use tokio::time::Instant;

use super::fetch::{self, Gate, JSON_CAP, PAGE_CAP, Sent, Stop};
use super::{Found, Target, Verdict, find_object, str_at};
use crate::outbound::{HostGroup, Outbound, Signal};

/// The web app id Instagram's pages send.
pub const APP_ID: &str = "936619743392459";
/// The logged-out post query (yt-dlp 2026.08.19).
pub const DOC_ID: &str = "27130156389949648";
/// Its friendly name.
pub const FRIENDLY_NAME: &str = "PolarisLoggedOutDesktopWWWPostRootContentQuery";
/// How long an LSD token is reused.
const LSD_TTL: Duration = Duration::from_secs(600);

/// The LSD token of the home page, and when it was read.
static LSD: Mutex<Option<(String, Instant)>> = Mutex::new(None);

static JSON_SCRIPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<script type="application/json"[^>]*>([\s\S]*?)</script>"#)
        .expect("valid pattern")
});
static LSD_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""LSD",\[\],\{"token":"([^"]+)""#).expect("valid pattern"));
static PAGE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""pageID":"([^"]+)""#).expect("valid pattern"));

/// One route's answer.
enum Route {
    /// A verdict.
    Verdict(Verdict),
    /// No verdict: try the next route.
    Next(&'static str),
}

/// Hydrates an Instagram post: the post page, then GraphQL.
///
/// # Errors
///
/// [`Stop`] when the breaker is open or the first block signal tripped it.
pub async fn hydrate(outbound: &Outbound, target: &Target) -> Result<Verdict, Stop> {
    let mut gate = Gate::open(outbound, HostGroup::InstagramWeb).await?;
    let code = target
        .shortcode
        .clone()
        .unwrap_or_else(|| shortcode_of(&target.native_id));
    if let Route::Verdict(verdict) = page(&mut gate, target, &code).await? {
        return Ok(finish(gate, verdict));
    }
    match graphql(&mut gate, target, &code).await? {
        Route::Verdict(verdict) => Ok(finish(gate, verdict)),
        Route::Next(last) => {
            gate.finish(Signal::Transient);
            Ok(Verdict::Failed(last))
        }
    }
}

/// Reports a verdict to the breaker.
fn finish(gate: Gate<'_>, verdict: Verdict) -> Verdict {
    gate.finish(match verdict {
        Verdict::Found(_) => Signal::Served,
        Verdict::Gone | Verdict::Gated => Signal::Answered,
        Verdict::Failed(_) => Signal::Transient,
    });
    verdict
}

/// The code of a pk, for a post saved without one.
fn shortcode_of(native_id: &str) -> String {
    shelfy_core::ids::ig::MediaPk::parse_decimal(native_id)
        .map(|pk| pk.to_shortcode())
        .unwrap_or_default()
}

/// The post page.
async fn page(gate: &mut Gate<'_>, target: &Target, code: &str) -> Result<Route, Stop> {
    let url = format!("https://www.instagram.com/p/{code}/");
    let reply = match gate
        .send(Method::GET, &url, fetch::document_headers(), None, PAGE_CAP)
        .await
    {
        Sent::Reply(reply) => reply,
        Sent::Blocked(block) => return Err(Stop::Blocked(block)),
        Sent::Failed(code) => return Ok(Route::Next(code)),
    };
    match reply.status {
        200 => {}
        404 | 410 => return Ok(Route::Verdict(Verdict::Gone)),
        _ => return Ok(Route::Next("http_error")),
    }
    for script in JSON_SCRIPT.captures_iter(&reply.body) {
        let text = &script[1];
        if !text.contains("xig_polaris_media") {
            continue;
        }
        let Ok(json) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        let Some(holder) = find_object(&json, |o| o.contains_key("xig_polaris_media")) else {
            continue;
        };
        let xig = &holder["xig_polaris_media"];
        if !xig.is_object() {
            continue;
        }
        return Ok(Route::Verdict(media_verdict(target, code, xig)));
    }
    if PAGE_ID
        .captures(&reply.body)
        .is_some_and(|m| &m[1] == "httpErrorPage")
    {
        return Ok(Route::Verdict(Verdict::Gone));
    }
    if let Some(block) = gate.check_body(&reply) {
        return Err(Stop::Blocked(block));
    }
    Ok(Route::Next("parse_failed"))
}

/// The logged-out GraphQL query, with the home page's LSD token.
async fn graphql(gate: &mut Gate<'_>, target: &Target, code: &str) -> Result<Route, Stop> {
    let Some(lsd) = lsd(gate).await? else {
        return Ok(Route::Next("no_lsd"));
    };
    let variables = json!({ "media_id": target.native_id }).to_string();
    let form = serde_html_encode(&[
        ("lsd", lsd.as_str()),
        ("fb_api_caller_class", "RelayModern"),
        ("fb_api_req_friendly_name", FRIENDLY_NAME),
        ("server_timestamps", "true"),
        ("variables", variables.as_str()),
        ("doc_id", DOC_ID),
    ]);
    let referer = format!("https://www.instagram.com/p/{code}/");
    let mut headers = fetch::headers(&[
        ("accept", "*/*"),
        ("content-type", "application/x-www-form-urlencoded"),
        ("x-ig-app-id", APP_ID),
        ("x-asbd-id", "359341"),
        ("x-ig-www-claim", "0"),
        ("x-fb-friendly-name", FRIENDLY_NAME),
        ("x-requested-with", "XMLHttpRequest"),
        ("origin", "https://www.instagram.com"),
        ("sec-fetch-dest", "empty"),
        ("sec-fetch-mode", "cors"),
        ("sec-fetch-site", "same-origin"),
    ]);
    if let (Ok(lsd), Ok(referer)) = (lsd.parse(), referer.parse()) {
        headers.insert("x-fb-lsd", lsd);
        headers.insert("referer", referer);
    }
    let reply = match gate
        .send(
            Method::POST,
            "https://www.instagram.com/api/graphql",
            headers,
            Some(form),
            JSON_CAP,
        )
        .await
    {
        Sent::Reply(reply) => reply,
        Sent::Blocked(block) => return Err(Stop::Blocked(block)),
        Sent::Failed(code) => return Ok(Route::Next(code)),
    };
    if reply.status != 200 {
        return Ok(Route::Next("http_error"));
    }
    let text = reply.body.trim_start_matches("for (;;);");
    let Ok(json) = serde_json::from_str::<Value>(text) else {
        if let Some(block) = gate.check_body(&reply) {
            return Err(Stop::Blocked(block));
        }
        return Ok(Route::Next("parse_failed"));
    };
    if json.get("data").is_none_or(Value::is_null)
        && json["errors"].as_array().is_some_and(|e| !e.is_empty())
    {
        forget_lsd();
        return Ok(Route::Next("graphql_error"));
    }
    let xig = &json["data"]["xig_polaris_media"];
    if !xig.is_object() {
        return Ok(Route::Verdict(Verdict::Gone));
    }
    Ok(Route::Verdict(media_verdict(target, code, xig)))
}

/// The LSD token: the cached one, or one read from the home page.
async fn lsd(gate: &mut Gate<'_>) -> Result<Option<String>, Stop> {
    {
        let cached = LSD.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((token, at)) = cached.as_ref()
            && at.elapsed() < LSD_TTL
        {
            return Ok(Some(token.clone()));
        }
    }
    let reply = match gate
        .send(
            Method::GET,
            "https://www.instagram.com/",
            fetch::document_headers(),
            None,
            PAGE_CAP,
        )
        .await
    {
        Sent::Reply(reply) => reply,
        Sent::Blocked(block) => return Err(Stop::Blocked(block)),
        Sent::Failed(_) => return Ok(None),
    };
    if reply.status != 200 {
        return Ok(None);
    }
    let Some(token) = LSD_TOKEN.captures(&reply.body).map(|m| m[1].to_owned()) else {
        if let Some(block) = gate.check_body(&reply) {
            return Err(Stop::Blocked(block));
        }
        return Ok(None);
    };
    *LSD.lock().unwrap_or_else(PoisonError::into_inner) = Some((token.clone(), Instant::now()));
    Ok(Some(token))
}

fn forget_lsd() {
    *LSD.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// `application/x-www-form-urlencoded` of `pairs`.
fn serde_html_encode(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// The verdict of an `xig_polaris_media` object.
fn media_verdict(target: &Target, code: &str, xig: &Value) -> Verdict {
    let media = &xig["if_not_gated_logged_out"];
    if !media.is_object() {
        return Verdict::Gated;
    }
    Verdict::Found(Found {
        item: item(target, code, media),
        posted_at: media["taken_at"].as_i64().map(|s| s.saturating_mul(1_000)),
    })
}

/// A logged-out media object as an extension item (`sanitize`'s input).
fn item(target: &Target, code: &str, media: &Value) -> Value {
    let carousel = media["media_type"].as_i64() == Some(8);
    let children: Vec<&Value> = if carousel {
        media["carousel_media"]
            .as_array()
            .map(|items| items.iter().collect())
            .unwrap_or_default()
    } else {
        vec![media]
    };
    let slides: Vec<Value> = children.iter().filter_map(|child| slide(child)).collect();
    let media_type = if carousel {
        "carousel"
    } else if media["media_type"].as_i64() == Some(2) {
        "video"
    } else {
        "image"
    };
    let user = if media["user"].is_object() {
        &media["user"]
    } else {
        &media["owner"]
    };
    let username = str_at(user, "username");
    json!({
        "id": target.native_id,
        "shortcode": code,
        "postUrl": target.post_url,
        "profileUrl": username.map(|u| format!("https://www.instagram.com/{u}/")),
        "authorUsername": username,
        "authorName": str_at(user, "full_name"),
        "text": media["caption"]["text"].as_str().unwrap_or_default(),
        "mediaType": media_type,
        "thumbnailUrl": slides.first().map(|s| s["url"].clone()),
        "media": slides,
    })
}

/// A slide: an image, or a video with its poster.
fn slide(media: &Value) -> Option<Value> {
    let image = media["image_versions2"]["candidates"][0]["url"]
        .as_str()
        .or_else(|| media["display_uri"].as_str())?;
    match media["video_versions"][0]["url"].as_str() {
        Some(video) => Some(json!({ "type": "video", "url": image, "videoUrl": video })),
        None => Some(json!({ "type": "image", "url": image })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            id: 1,
            key: "ig_3141592653589793238".to_owned(),
            platform: shelfy_core::repo::Platform::Instagram,
            native_id: "3141592653589793238".to_owned(),
            shortcode: Some("CuZLd-iMknW".to_owned()),
            post_url: "https://www.instagram.com/p/CuZLd-iMknW/".to_owned(),
        }
    }

    #[test]
    fn a_carousel_maps_to_slides_with_posters() {
        let media = json!({
            "media_type": 8,
            "taken_at": 1_790_000_000,
            "caption": { "text": "hello" },
            "user": { "username": "studio", "full_name": "Studio" },
            "carousel_media": [
                { "image_versions2": { "candidates": [{ "url": "https://scontent.cdninstagram.com/a.jpg?oe=6A000000" }] } },
                {
                    "image_versions2": { "candidates": [{ "url": "https://scontent.cdninstagram.com/b.jpg" }] },
                    "video_versions": [{ "url": "https://scontent.cdninstagram.com/b.mp4" }]
                }
            ]
        });
        let Verdict::Found(found) = media_verdict(
            &target(),
            "CuZLd-iMknW",
            &json!({ "if_not_gated_logged_out": media }),
        ) else {
            panic!("found");
        };
        assert_eq!(found.posted_at, Some(1_790_000_000_000));
        let item = found.item;
        assert_eq!(item["mediaType"], "carousel");
        assert_eq!(item["authorUsername"], "studio");
        assert_eq!(item["profileUrl"], "https://www.instagram.com/studio/");
        assert_eq!(item["media"][1]["type"], "video");
        assert_eq!(
            item["media"][1]["videoUrl"],
            "https://scontent.cdninstagram.com/b.mp4"
        );
        assert_eq!(
            item["thumbnailUrl"],
            "https://scontent.cdninstagram.com/a.jpg?oe=6A000000"
        );
    }

    #[test]
    fn a_null_logged_out_object_is_gated() {
        let verdict = media_verdict(
            &target(),
            "CuZLd-iMknW",
            &json!({ "if_not_gated_logged_out": null }),
        );
        assert!(matches!(verdict, Verdict::Gated));
    }
}
