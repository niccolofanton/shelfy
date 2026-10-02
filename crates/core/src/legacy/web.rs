//! Local files referenced inside the JSON of a captured site version.
//!
//! The desktop stores absolute file paths inside `web_pages_json` and
//! `web_meta_json` (on `posts` for the current version and on
//! `web_snapshots` for older ones). Capture v2 (`electron/weborchestrator.ts`)
//! writes, per page, `screenshotPath`, `hero.path`, `chunks[].screenshotPath`,
//! `sections[].path` and `footer.path`, and, per site, `ogImagePath`,
//! `favicon` and `video.{path, preview, poster}`; capture v1 only the page
//! `screenshotPath` and `chunks`. In the web schema each becomes a CAS object
//! referenced from `web_capture_assets` (plan §2.7).

use serde::Serialize;
use serde_json::Value;

use super::convert::is_local_path;

/// The role of a capture file; the strings are `media_objects.role` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebAssetRole {
    /// `pages[].screenshotPath`: the page's single image (v2: the untouched
    /// hero; page 0 may use the og image when the hero failed QC).
    Screenshot,
    /// `pages[].hero.path`: the first viewport at 2×.
    Hero,
    /// `pages[].chunks[].screenshotPath`: vertical bands of a tall page.
    Band,
    /// `pages[].chunks[]` of a scroll-jacked page (`jacked: true`).
    Filmstrip,
    /// `pages[].sections[].path`.
    Section,
    /// `pages[].footer.path`.
    Footer,
    /// `meta.ogImagePath`: the og image, downloaded.
    Og,
    /// `meta.favicon`: the favicon, downloaded.
    Favicon,
    /// `meta.video.path`: the scroll video.
    Video,
    /// `meta.video.preview`.
    VideoPreview,
    /// `meta.video.poster`.
    VideoPoster,
}

impl WebAssetRole {
    pub const ALL: [WebAssetRole; 11] = [
        WebAssetRole::Screenshot,
        WebAssetRole::Hero,
        WebAssetRole::Band,
        WebAssetRole::Filmstrip,
        WebAssetRole::Section,
        WebAssetRole::Footer,
        WebAssetRole::Og,
        WebAssetRole::Favicon,
        WebAssetRole::Video,
        WebAssetRole::VideoPreview,
        WebAssetRole::VideoPoster,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WebAssetRole::Screenshot => "screenshot",
            WebAssetRole::Hero => "hero",
            WebAssetRole::Band => "band",
            WebAssetRole::Filmstrip => "filmstrip",
            WebAssetRole::Section => "section",
            WebAssetRole::Footer => "footer",
            WebAssetRole::Og => "og",
            WebAssetRole::Favicon => "favicon",
            WebAssetRole::Video => "video",
            WebAssetRole::VideoPreview => "video_preview",
            WebAssetRole::VideoPoster => "video_poster",
        }
    }

    /// True for the scroll video, which is archived only with `--with-videos`.
    pub fn is_video(self) -> bool {
        self == WebAssetRole::Video
    }
}

/// One file referenced by a captured site version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAssetRef {
    pub role: WebAssetRole,
    /// The page, for page-level roles.
    pub page_index: Option<usize>,
    /// Position within the role on that page (or site).
    pub seq: usize,
    pub path: String,
}

/// The files of one captured site version.
#[derive(Debug, Clone, Default)]
pub struct WebCaptureFiles {
    pub refs: Vec<WebAssetRef>,
    /// Number of pages in `web_pages_json`.
    pub pages: usize,
    /// `web_pages_json` is present but not a JSON array.
    pub pages_json_invalid: bool,
    /// `web_meta_json` is present but not a JSON object.
    pub meta_json_invalid: bool,
}

/// Extracts the file references of a captured site version. Values that are
/// not local paths (URLs, empty strings) are ignored.
pub fn capture_files(pages_json: Option<&str>, meta_json: Option<&str>) -> WebCaptureFiles {
    let mut out = WebCaptureFiles::default();
    match parse(pages_json) {
        Parsed::Absent => {}
        Parsed::Invalid => out.pages_json_invalid = true,
        Parsed::Value(Value::Array(pages)) => {
            out.pages = pages.len();
            for (index, page) in pages.iter().enumerate() {
                page_files(&mut out.refs, index, page);
            }
        }
        Parsed::Value(_) => out.pages_json_invalid = true,
    }
    match parse(meta_json) {
        Parsed::Absent => {}
        Parsed::Value(meta @ Value::Object(_)) => meta_files(&mut out.refs, &meta),
        Parsed::Value(Value::Null) => {}
        Parsed::Invalid | Parsed::Value(_) => out.meta_json_invalid = true,
    }
    out
}

enum Parsed {
    Absent,
    Invalid,
    Value(Value),
}

fn parse(raw: Option<&str>) -> Parsed {
    match raw {
        None => Parsed::Absent,
        Some(s) if s.trim().is_empty() => Parsed::Absent,
        Some(s) => serde_json::from_str(s).map_or(Parsed::Invalid, Parsed::Value),
    }
}

fn page_files(refs: &mut Vec<WebAssetRef>, index: usize, page: &Value) {
    let mut push = |role, seq, value: Option<&Value>| {
        if let Some(path) = value.and_then(Value::as_str).filter(|p| is_local_path(p)) {
            refs.push(WebAssetRef {
                role,
                page_index: Some(index),
                seq,
                path: path.to_owned(),
            });
        }
    };
    push(WebAssetRole::Screenshot, 0, page.get("screenshotPath"));
    push(
        WebAssetRole::Hero,
        0,
        page.get("hero").and_then(|h| h.get("path")),
    );
    push(
        WebAssetRole::Footer,
        0,
        page.get("footer").and_then(|f| f.get("path")),
    );
    let jacked = page.get("jacked").and_then(Value::as_bool).unwrap_or(false);
    let chunk_role = if jacked {
        WebAssetRole::Filmstrip
    } else {
        WebAssetRole::Band
    };
    for (seq, chunk) in array(page.get("chunks")).iter().enumerate() {
        push(chunk_role, seq, chunk.get("screenshotPath"));
    }
    for (seq, section) in array(page.get("sections")).iter().enumerate() {
        push(WebAssetRole::Section, seq, section.get("path"));
    }
}

fn meta_files(refs: &mut Vec<WebAssetRef>, meta: &Value) {
    let mut push = |role, value: Option<&Value>| {
        if let Some(path) = value.and_then(Value::as_str).filter(|p| is_local_path(p)) {
            refs.push(WebAssetRef {
                role,
                page_index: None,
                seq: 0,
                path: path.to_owned(),
            });
        }
    };
    push(WebAssetRole::Og, meta.get("ogImagePath"));
    push(WebAssetRole::Favicon, meta.get("favicon"));
    let video = meta.get("video");
    push(WebAssetRole::Video, video.and_then(|v| v.get("path")));
    push(
        WebAssetRole::VideoPreview,
        video.and_then(|v| v.get("preview")),
    );
    push(
        WebAssetRole::VideoPoster,
        video.and_then(|v| v.get("poster")),
    );
}

fn array(value: Option<&Value>) -> &[Value] {
    match value {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

/// The facet rows the desktop derives from `ai_web_json.facets`
/// (`applyAiAnalysis` in `electron/db.ts`): for each facet, every value
/// stringified, trimmed, non-empty, cut to 120 UTF-16 units; duplicates
/// collapse. `None` when the JSON is absent or invalid.
pub fn derived_facets(ai_web_json: Option<&str>) -> Option<Vec<(String, String)>> {
    let raw = ai_web_json.filter(|s| !s.trim().is_empty())?;
    let value: Value = serde_json::from_str(raw).ok()?;
    let mut out = Vec::new();
    let Some(Value::Object(facets)) = value.get("facets") else {
        return Some(out);
    };
    for (facet, values) in facets {
        for v in array(Some(values)) {
            // `String(v || '')`
            let text = if js_truthy(v) {
                js_to_string(v)
            } else {
                String::new()
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value = truncate_utf16(trimmed, 120);
            let row = (facet.clone(), value);
            if !out.contains(&row) {
                out.push(row);
            }
        }
    }
    Some(out)
}

fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => false,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// JavaScript `String(v)` for JSON values (numbers: integral values print
/// without a fraction, as in JavaScript).
fn js_to_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e21 => format!("{f:.0}"),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_to_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

/// The longest prefix of `s` that fits in `max` UTF-16 code units.
fn truncate_utf16(s: &str, max: usize) -> String {
    let mut units = 0;
    let mut end = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        end = i + c.len_utf8();
    }
    s[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGES_V2: &str = r#"[
      {"url":"https://example.com/","screenshotPath":"/u/a/assets/web/1-example.com-hero.webp",
       "hero":{"path":"/u/a/assets/web/1-example.com-hero.webp","width":2880,"height":1800},
       "chunks":[{"screenshotPath":"/u/a/assets/web/1-c0.webp"},{"screenshotPath":"/u/a/assets/web/1-c1.webp"}],
       "footer":{"path":"/u/a/assets/web/1-f.webp"},
       "sections":[{"kind":"hero","path":"/u/a/assets/web/1-s0.webp"}],
       "jacked":false,
       "meta":{"ogImage":"https://example.com/og.png"}},
      {"url":"https://example.com/work","screenshotPath":"","chunks":[{"screenshotPath":"/u/a/assets/web/2-c0.webp"}],"jacked":true,"footer":null}
    ]"#;

    const META_V2: &str = r#"{"schema":2,"ogImage":"https://example.com/og.png",
      "ogImagePath":"/u/a/assets/web/og.webp","favicon":"/u/a/assets/web/fav.webp",
      "video":{"path":"/u/a/assets/web/v.mp4","preview":"/u/a/assets/web/v-prev.mp4","poster":null},
      "organization":{"logo":"https://example.com/logo.png"}}"#;

    #[test]
    fn extracts_every_v2_file() {
        let files = capture_files(Some(PAGES_V2), Some(META_V2));
        assert_eq!(files.pages, 2);
        assert!(!files.pages_json_invalid && !files.meta_json_invalid);
        let roles: Vec<_> = files
            .refs
            .iter()
            .map(|r| (r.role, r.page_index, r.seq))
            .collect();
        assert_eq!(
            roles,
            [
                (WebAssetRole::Screenshot, Some(0), 0),
                (WebAssetRole::Hero, Some(0), 0),
                (WebAssetRole::Footer, Some(0), 0),
                (WebAssetRole::Band, Some(0), 0),
                (WebAssetRole::Band, Some(0), 1),
                (WebAssetRole::Section, Some(0), 0),
                (WebAssetRole::Filmstrip, Some(1), 0),
                (WebAssetRole::Og, None, 0),
                (WebAssetRole::Favicon, None, 0),
                (WebAssetRole::Video, None, 0),
                (WebAssetRole::VideoPreview, None, 0),
            ]
        );
    }

    #[test]
    fn v1_pages_and_invalid_json() {
        let v1 = r#"[{"url":"https://e.com","screenshotPath":"C:\\Users\\x\\AppData\\Roaming\\Shelfy\\assets\\web\\a.png","chunks":[]}]"#;
        let files = capture_files(Some(v1), None);
        assert_eq!(files.refs.len(), 1);
        assert_eq!(files.refs[0].role, WebAssetRole::Screenshot);
        let bad = capture_files(Some("{not json"), Some("[1]"));
        assert!(bad.pages_json_invalid && bad.meta_json_invalid);
        assert!(bad.refs.is_empty());
        let empty = capture_files(Some(""), Some("null"));
        assert!(!empty.pages_json_invalid && !empty.meta_json_invalid);
    }

    #[test]
    fn facets_follow_the_desktop_rules() {
        let ai = r#"{"facets":{"style":[" Minimal ","minimal","Minimal",""],"tech":["React",3,0,true,false,null,2.0,["a",null,1],{}],"x":"not an array"}}"#;
        let facets = derived_facets(Some(ai)).unwrap();
        let pair = |f: &str, v: &str| (f.to_owned(), v.to_owned());
        assert_eq!(
            facets,
            [
                pair("style", "Minimal"),
                pair("style", "minimal"),
                pair("tech", "React"),
                pair("tech", "3"),
                pair("tech", "true"),
                pair("tech", "2"),
                pair("tech", "a,,1"),
                pair("tech", "[object Object]"),
            ]
        );
        assert_eq!(derived_facets(Some(r#"{"schema":2}"#)), Some(vec![]));
        assert_eq!(derived_facets(Some("nope")), None);
        assert_eq!(derived_facets(None), None);
    }

    #[test]
    fn utf16_truncation() {
        assert_eq!(truncate_utf16("abc", 2), "ab");
        assert_eq!(truncate_utf16("a😀b", 2), "a");
        assert_eq!(truncate_utf16("a😀b", 3), "a😀");
        assert_eq!(truncate_utf16(&"é".repeat(130), 120).chars().count(), 120);
    }
}
