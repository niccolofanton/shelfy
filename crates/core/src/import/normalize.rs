//! Desktop-compatible IG/X normalization, with explicit support for desktop
//! Pinterest, web placeholders and manual notes. No imported path is opened.
use crate::ids;
use crate::ingest::{merge::IncomingPost, sanitize};
use crate::repo::Platform;
use crate::repo::posts::UserContentPatch;
use serde::Serialize;
use serde_json::Value;

/// Stable record rejection, reported with the input index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    BadItem,
    BadId,
}
impl Reject {
    /// Wire code.
    pub fn code(self) -> &'static str {
        match self {
            Self::BadItem => "bad_item",
            Self::BadId => "bad_id",
        }
    }
}
/// One accepted post and its new-post-only user layer.
#[derive(Clone, Debug)]
pub struct Post {
    /// Merge input; every object id is deliberately absent.
    pub incoming: IncomingPost,
    /// Applied only when the canonical key is new.
    pub user: UserContentPatch,
    /// Membership keys (`x:<id>` / `n:<name>`).
    pub collections: Vec<String>,
}
/// A media item in the desktop normalizer's serialized order.
#[derive(Serialize)]
pub struct Media {
    #[serde(rename = "type")]
    kind: String,
    url: String,
}
/// Ordered desktop result used for byte-exact golden checks.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Desktop {
    id: String,
    platform: String,
    shortcode: String,
    post_url: String,
    profile_url: String,
    author_username: String,
    author_name: String,
    text: String,
    thumbnail_url: String,
    media_type: String,
    media: Vec<Media>,
    timestamp: String,
    #[serde(flatten)]
    ai: DesktopAi,
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopAi {
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_general_tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_specific_tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_entities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_keywords: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_save_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_analyzed_at: Option<i64>,
}
fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_owned()
}
fn opt(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}
fn strings(v: &Value, key: &str) -> Option<Vec<String>> {
    v.get(key)?
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect()
}
fn ai(v: &Value) -> DesktopAi {
    DesktopAi {
        ai_description: opt(v, "aiDescription"),
        ai_tags: strings(v, "aiTags"),
        ai_general_tags: strings(v, "aiGeneralTags"),
        ai_specific_tags: strings(v, "aiSpecificTags"),
        ai_category: opt(v, "aiCategory"),
        ai_content_type: opt(v, "aiContentType"),
        ai_entities: strings(v, "aiEntities"),
        ai_keywords: strings(v, "aiKeywords"),
        ai_language: opt(v, "aiLanguage"),
        ai_save_reason: opt(v, "aiSaveReason"),
        ai_status: opt(v, "aiStatus"),
        ai_model: opt(v, "aiModel"),
        ai_analyzed_at: v.get("aiAnalyzedAt").and_then(Value::as_i64),
    }
}
/// Faithful IG/X normalization on accepted, typed input. Pinterest/web/manual
/// use their explicit platforms instead of the desktop's default-to-X quirk.
pub fn desktop(v: &Value, platform: Platform) -> Desktop {
    let ig = platform == Platform::Instagram;
    let shortcode = if ig {
        text(v, "shortcode")
    } else {
        String::new()
    };
    let mut id = text(v, "id");
    if id.is_empty() && ig {
        id.clone_from(&shortcode);
    }
    let author_username = text(v, "authorUsername");
    let media_type = opt(v, "mediaType")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| if ig { "image" } else { "text" }.into());
    let thumbnail_url = text(v, "thumbnailUrl");
    let mut media = Vec::new();
    if let Some(entries) = v
        .get("media")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    {
        for m in entries {
            let url = opt(m, "url")
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| text(m, "thumbnailUrl"));
            if !url.is_empty() {
                media.push(Media {
                    kind: if text(m, "type") == "video" {
                        "video"
                    } else {
                        "image"
                    }
                    .into(),
                    url,
                });
            }
        }
    } else if !thumbnail_url.is_empty() && (ig || media_type != "text") {
        media.push(Media {
            kind: if media_type == "video" {
                "video"
            } else {
                "image"
            }
            .into(),
            url: thumbnail_url.clone(),
        });
    }
    let mut post_url = text(v, "postUrl");
    if post_url.is_empty() {
        if ig && !shortcode.is_empty() {
            post_url = format!("https://www.instagram.com/p/{shortcode}/");
        } else if platform == Platform::Twitter && !id.is_empty() {
            post_url = format!(
                "https://x.com/{}/status/{id}",
                if author_username.is_empty() {
                    "i"
                } else {
                    &author_username
                }
            );
        }
    }
    let mut profile_url = text(v, "profileUrl");
    if profile_url.is_empty() && !author_username.is_empty() {
        profile_url = if ig {
            format!("https://www.instagram.com/{author_username}/")
        } else if platform == Platform::Twitter {
            format!("https://x.com/{author_username}")
        } else {
            String::new()
        };
    }
    let timestamp = opt(v, "timestamp")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if ig {
                ids::ig::date_from_shortcode(&shortcode).map_or_else(String::new, iso)
            } else {
                String::new()
            }
        });
    Desktop {
        id,
        platform: platform.as_str().into(),
        shortcode,
        post_url,
        profile_url,
        author_username,
        author_name: text(v, "authorName"),
        text: if ig {
            opt(v, "caption")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| text(v, "text"))
        } else {
            text(v, "text")
        },
        thumbnail_url,
        media_type,
        media,
        timestamp,
        ai: ai(v),
    }
}
fn iso(ms: i64) -> String {
    // Gregorian civil date from days since Unix epoch (400-year eras).
    let days = ms.div_euclid(86_400_000) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let t = ms.rem_euclid(86_400_000);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        t / 3_600_000,
        t / 60_000 % 60,
        t / 1000 % 60,
        t % 1000
    )
}
/// Validates the shape, bounded strings/lists, platform and canonical identity.
/// Manual IDs use their source date (or epoch), never the job's varying time,
/// making the canonical mapping stable on separate reimports.
pub fn post(v: &Value, now: i64) -> Result<Post, Reject> {
    let obj = v.as_object().ok_or(Reject::BadItem)?;
    let platform = match obj.get("platform").and_then(Value::as_str) {
        Some(p) => p.parse().map_err(|_| Reject::BadItem)?,
        None if obj
            .get("shortcode")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()) =>
        {
            Platform::Instagram
        }
        None if obj.contains_key("text") && obj.contains_key("authorUsername") => Platform::Twitter,
        _ => return Err(Reject::BadItem),
    };
    for (key, value) in obj {
        if let Value::String(s) = value {
            let cap = if matches!(
                key.as_str(),
                "text" | "caption" | "note" | "userNote" | "aiDescription"
            ) {
                20_000
            } else {
                4096
            };
            if s.chars().count() > cap {
                return Err(Reject::BadItem);
            }
        }
        if let Value::Array(items) = value {
            if items.len() > 500 {
                return Err(Reject::BadItem);
            }
            if key != "media"
                && items
                    .iter()
                    .any(|i| !i.is_string() || i.as_str().is_some_and(|s| s.chars().count() > 4096))
            {
                return Err(Reject::BadItem);
            }
        }
    }
    let d = desktop(v, platform);
    let normalized = serde_json::to_value(&d).map_err(|_| Reject::BadItem)?;
    let mut incoming = if matches!(
        platform,
        Platform::Instagram | Platform::Twitter | Platform::Pinterest
    ) {
        let batch =
            sanitize::sanitize_batch(platform, &[normalized], now).map_err(|_| Reject::BadItem)?;
        batch.posts.into_iter().next().ok_or(Reject::BadId)?
    } else {
        let source_url = opt(v, "webUrl")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| d.post_url.clone());
        if !source_url.is_empty() || platform == Platform::Web {
            let url = url::Url::parse(&source_url).map_err(|_| Reject::BadItem)?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(Reject::BadItem);
            }
        }
        let id = if platform == Platform::Web {
            ids::web::from_url(&source_url)
        } else {
            ids::manual::from_legacy(
                &d.id,
                crate::legacy::convert::parse_iso8601_ms(&d.timestamp).unwrap_or(0),
            )
        }
        .map_err(|_| Reject::BadId)?;
        let mut p = IncomingPost::new(id.key(), platform, id.native_id(), "text");
        p.caption = Some(d.text.clone());
        p.post_url = (!source_url.is_empty()).then_some(source_url.clone());
        p.archive_state = Some("link_only".into());
        if platform == Platform::Web {
            p.web_url = Some(source_url.clone());
            p.web_domain = crate::web::captures::domain_of(&source_url);
        }
        p
    };
    let a = d.ai;
    incoming.ai.description = a.ai_description.map(Some);
    incoming.ai.tags = a.ai_tags.map(Some);
    incoming.ai.general_tags = a.ai_general_tags.map(Some);
    incoming.ai.specific_tags = a.ai_specific_tags.map(Some);
    incoming.ai.category = a.ai_category.map(Some);
    incoming.ai.content_type = a.ai_content_type.map(Some);
    incoming.ai.entities = a.ai_entities.map(Some);
    incoming.ai.keywords = a.ai_keywords.map(Some);
    incoming.ai.language = a.ai_language.map(Some);
    incoming.ai.save_reason = a.ai_save_reason.map(Some);
    incoming.ai.status = a.ai_status.map(Some);
    incoming.ai.model = a.ai_model.map(Some);
    incoming.ai.analyzed_at = a.ai_analyzed_at.map(Some);
    Ok(Post {
        incoming,
        user: UserContentPatch {
            note: opt(v, "note").or_else(|| opt(v, "userNote")).map(Some),
            tags: strings(v, "manualTags").or_else(|| strings(v, "userTags")),
        },
        collections: strings(v, "collections").unwrap_or_default(),
    })
}

/// Whether the desktop normalizer treats this field as present. Invalid/null
/// AI fields are omitted, so they must not erase an earlier valid copy.
pub(crate) fn ai_present(key: &str, value: &Value) -> bool {
    match key {
        "aiTags" | "aiGeneralTags" | "aiSpecificTags" | "aiEntities" | "aiKeywords" => {
            value.is_array()
        }
        "aiAnalyzedAt" => value.is_i64(),
        "aiDescription" | "aiCategory" | "aiContentType" | "aiLanguage" | "aiSaveReason"
        | "aiStatus" | "aiModel" => value.is_string(),
        _ => false,
    }
}
