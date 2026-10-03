//! Pinterest hydration (L17, SPIKE-9's `pinResource` and `pinPidgets`):
//! `PinResource`, then the widget API. Never the pin page: 1 MB of HTML,
//! and it rate-limits fast (SPIKE-9 risk 5).
//!
//! | Route | Request | Data |
//! |---|---|---|
//! | PinResource | `GET www.pinterest.com/resource/PinResource/get/?data={"options":{"field_set_key":"unauth_react_main_pin","id":"<id>"}}` | every field, `created_at` included |
//! | pidgets | `GET widgets.pinterest.com/v3/pidgets/pins/info/?pin_ids=<id>` | every field but the date |
//!
//! A 404, `resource_response.error.http_status` 404 and a pidgets entry
//! with an `error` are gone. The video is `V_720P`, else the widest MP4
//! (a story pin's first video block when the pin has none of its own).

use axum::http::Method;
use serde_json::{Value, json};

use super::fetch::{Gate, JSON_CAP, Sent, Stop, headers};
use super::{Found, Target, Verdict, rfc2822_ms, str_at};
use crate::outbound::{HostGroup, Outbound, Signal};

/// The `PinResource` URL of a pin.
#[must_use]
pub fn resource_url(pin_id: &str) -> String {
    let data = json!({ "options": { "field_set_key": "unauth_react_main_pin", "id": pin_id } })
        .to_string();
    let encoded: String = url::form_urlencoded::byte_serialize(data.as_bytes()).collect();
    format!("https://www.pinterest.com/resource/PinResource/get/?data={encoded}")
}

/// The widget API URL of a pin.
#[must_use]
pub fn pidgets_url(pin_id: &str) -> String {
    format!("https://widgets.pinterest.com/v3/pidgets/pins/info/?pin_ids={pin_id}")
}

/// Hydrates a pin: `PinResource`, then pidgets.
///
/// # Errors
///
/// [`Stop`] when the breaker is open or the first block signal tripped it.
pub async fn hydrate(outbound: &Outbound, target: &Target) -> Result<Verdict, Stop> {
    let mut gate = Gate::open(outbound, HostGroup::PinterestWeb).await?;
    let resource_headers = headers(&[
        ("accept", "application/json, text/javascript, */*; q=0.01"),
        ("x-pinterest-pws-handler", "www/pin/[id].js"),
    ]);
    let resource = gate
        .send(
            Method::GET,
            &resource_url(&target.native_id),
            resource_headers,
            None,
            JSON_CAP,
        )
        .await;
    match resource {
        Sent::Reply(reply) => {
            let json = serde_json::from_str::<Value>(&reply.body).ok();
            let not_found = json
                .as_ref()
                .and_then(|j| j["resource_response"]["error"]["http_status"].as_u64())
                == Some(404);
            if matches!(reply.status, 404 | 410) || not_found {
                return Ok(done(gate, Verdict::Gone));
            }
            if reply.status == 200
                && let Some(pin) = json
                    .as_ref()
                    .map(|j| &j["resource_response"]["data"])
                    .filter(|pin| pin.is_object())
            {
                return Ok(done(gate, Verdict::Found(found(target, pin))));
            }
        }
        Sent::Blocked(block) => return Err(Stop::Blocked(block)),
        Sent::Failed(_) => {}
    }
    let pidgets = gate
        .send(
            Method::GET,
            &pidgets_url(&target.native_id),
            headers(&[]),
            None,
            JSON_CAP,
        )
        .await;
    let last = match pidgets {
        Sent::Reply(reply) => match reply.status {
            404 | 410 => return Ok(done(gate, Verdict::Gone)),
            200 => {
                let json = serde_json::from_str::<Value>(&reply.body).ok();
                match json.as_ref().map(|j| &j["data"][0]) {
                    Some(pin) if pin.get("error").is_some_and(|e| !e.is_null()) => {
                        return Ok(done(gate, Verdict::Gone));
                    }
                    Some(pin) if pin.is_object() => {
                        return Ok(done(gate, Verdict::Found(found(target, pin))));
                    }
                    _ => "parse_failed",
                }
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

/// A pin object (either route's) as an extension item.
fn found(target: &Target, pin: &Value) -> Found {
    let images = &pin["images"];
    let image = ["orig", "736x", "564x", "237x"]
        .iter()
        .find_map(|size| images[*size]["url"].as_str());
    let video = video_list(pin).and_then(best_video);
    let mut media = Vec::new();
    match (image, video.as_deref()) {
        (Some(image), Some(video)) => {
            media.push(json!({ "type": "video", "url": image, "videoUrl": video }));
        }
        (None, Some(video)) => media.push(json!({ "type": "video", "url": video })),
        (Some(image), None) => media.push(json!({ "type": "image", "url": image })),
        (None, None) => {}
    }
    let caption = ["description", "title", "grid_title"]
        .iter()
        .filter_map(|field| pin[*field].as_str())
        .map(str::trim)
        .find(|text| !text.is_empty())
        .unwrap_or_default();
    let username = str_at(&pin["pinner"], "username");
    Found {
        item: json!({
            "id": target.native_id,
            "postUrl": target.post_url,
            "profileUrl": username.map(|u| format!("https://www.pinterest.com/{u}/")),
            "authorUsername": username,
            "authorName": str_at(&pin["pinner"], "full_name"),
            "text": caption,
            "mediaType": if video.is_some() { "video" } else { "image" },
            "thumbnailUrl": image,
            "media": media,
        }),
        posted_at: pin["created_at"].as_str().and_then(rfc2822_ms),
    }
}

/// The pin's own video list, or the first one of its story pages.
fn video_list(pin: &Value) -> Option<&Value> {
    let has_url = |list: &Value| {
        list.as_object()
            .is_some_and(|l| l.values().any(|v| v["url"].is_string()))
    };
    let own = &pin["videos"]["video_list"];
    if has_url(own) {
        return Some(own);
    }
    pin["story_pin_data"]["pages"]
        .as_array()?
        .iter()
        .flat_map(|page| page["blocks"].as_array().into_iter().flatten())
        .map(|block| &block["video"]["video_list"])
        .find(|list| has_url(list))
}

/// `V_720P`, else the widest MP4.
fn best_video(list: &Value) -> Option<String> {
    let entries = list.as_object()?;
    let mp4 = |v: &&Value| {
        v["url"]
            .as_str()
            .is_some_and(|url| url.split('?').next().is_some_and(|p| p.ends_with(".mp4")))
    };
    if let Some(v720) = entries.get("V_720P").filter(mp4) {
        return v720["url"].as_str().map(str::to_owned);
    }
    entries
        .values()
        .filter(mp4)
        .max_by_key(|v| v["width"].as_u64().unwrap_or(0))
        .and_then(|v| v["url"].as_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            id: 1,
            key: "pin_987654321012345678".to_owned(),
            platform: shelfy_core::repo::Platform::Pinterest,
            native_id: "987654321012345678".to_owned(),
            shortcode: None,
            post_url: "https://www.pinterest.com/pin/987654321012345678/".to_owned(),
        }
    }

    #[test]
    fn a_video_pin_keeps_v720p_and_its_poster() {
        let pin = json!({
            "description": "  ",
            "title": "Living room",
            "created_at": "Thu, 01 Oct 2026 12:00:00 +0000",
            "pinner": { "username": "studio", "full_name": "Studio" },
            "images": { "orig": { "url": "https://i.pinimg.com/originals/a.jpg" } },
            "videos": { "video_list": {
                "V_HLSV4": { "url": "https://v1.pinimg.com/videos/a.m3u8", "width": 1080 },
                "V_720P": { "url": "https://v1.pinimg.com/videos/720p/a.mp4", "width": 720 },
                "V_EXP7": { "url": "https://v1.pinimg.com/videos/exp7/a.mp4", "width": 1080 }
            } }
        });
        let found = found(&target(), &pin);
        assert_eq!(found.item["text"], "Living room");
        assert_eq!(found.item["mediaType"], "video");
        assert_eq!(
            found.item["media"][0]["videoUrl"],
            "https://v1.pinimg.com/videos/720p/a.mp4"
        );
        assert_eq!(found.posted_at, Some(1_790_856_000_000));
        assert_eq!(
            resource_url("1"),
            "https://www.pinterest.com/resource/PinResource/get/?data=%7B%22options%22%3A%7B%22field_set_key%22%3A%22unauth_react_main_pin%22%2C%22id%22%3A%221%22%7D%7D"
        );
    }
}
