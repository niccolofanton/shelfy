//! Captured site versions → `web_captures` + `web_capture_assets` (plan
//! §2.7, §4.2; SPIKE-1 S1-7, OI-2, OI-3).
//!
//! A version's files become CAS objects referenced from
//! `web_capture_assets`; its JSON keeps the text and the probes and loses
//! every local path, which would leak the desktop's user name and mean
//! nothing on the server. The capture settings of `meta.capture` fill the
//! columns the desktop has no column for (OI-3): `status` is `done` (the
//! desktop keeps only finished captures), `partial` says whether pages were
//! skipped, `engine` and `viewport` come from the settings. The palette,
//! fonts, tech and awards are copied verbatim ([`SiteJson`]); a value that is
//! not JSON is written as `NULL` and counted (F12).

use serde_json::{Map, Value};
use shelfy_core::legacy::convert::is_local_path;
use shelfy_core::legacy::web::{WebAssetRole, capture_files};

use super::objects::{ObjectTable, Role};
use crate::files::FileRefs;

/// `web_captures.status` of a migrated version.
pub const STATUS_DONE: &str = "done";
/// `web_capture_assets.page_index` of a site-level asset (og image, favicon,
/// scroll video), which belongs to no page.
pub const SITE_LEVEL: i64 = -1;

/// One version of a site, mapped.
#[derive(Debug, Clone, PartialEq)]
pub struct Capture {
    pub pages: usize,
    pub partial: bool,
    pub engine: Option<String>,
    pub viewport: Option<String>,
    pub title: Option<String>,
    /// `pages_json` without file paths.
    pub pages_json: Option<String>,
    /// `meta_json` without file paths and without `traits`.
    pub meta_json: Option<String>,
    pub traits_json: Option<String>,
    /// `palette_json`, `fonts_json`, `tech_json`, `awards_json`: verbatim.
    pub palette_json: Option<String>,
    pub fonts_json: Option<String>,
    pub tech_json: Option<String>,
    pub awards_json: Option<String>,
    /// How many of those four were not JSON, and are `None`.
    pub site_json_invalid: u64,
    /// Object indexes into the [`ObjectTable`].
    pub hero: Option<usize>,
    pub favicon: Option<usize>,
    pub assets: Vec<Asset>,
}

/// One file of a version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub page_index: i64,
    pub role: &'static str,
    pub seq: i64,
    pub object: usize,
    pub css_top: Option<i64>,
    pub css_height: Option<i64>,
}

/// The desktop's `web_palette_json`, `web_fonts_json`, `web_tech_json` and
/// `web_awards_json` of a version (a `posts` or a `web_snapshots` row).
#[derive(Debug, Clone, Copy, Default)]
pub struct SiteJson<'a> {
    pub palette: Option<&'a str>,
    pub fonts: Option<&'a str>,
    pub tech: Option<&'a str>,
    pub awards: Option<&'a str>,
}

impl<'a> SiteJson<'a> {
    /// The four values, in column order.
    fn values(self) -> [Option<&'a str>; 4] {
        [self.palette, self.fonts, self.tech, self.awards]
    }

    /// How many of the four values are not JSON (written as `NULL`).
    #[must_use]
    pub fn invalid(self) -> u64 {
        self.values()
            .into_iter()
            .filter(|raw| site_json_value(*raw).is_err())
            .count() as u64
    }
}

/// A site JSON column, verbatim: `Ok(None)` when blank or JSON `null`,
/// `Err` when it is not JSON.
fn site_json_value(raw: Option<&str>) -> Result<Option<String>, ()> {
    let Some(raw) = raw.filter(|r| !r.trim().is_empty()) else {
        return Ok(None);
    };
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Null) => Ok(None),
        Ok(_) => Ok(Some(raw.to_owned())),
        Err(_) => Err(()),
    }
}

/// The object role of a capture file (OI-2).
pub fn object_role(role: WebAssetRole) -> Role {
    match role {
        WebAssetRole::Screenshot | WebAssetRole::Hero => Role::Screenshot,
        WebAssetRole::Band => Role::Band,
        WebAssetRole::Filmstrip => Role::Filmstrip,
        WebAssetRole::Section => Role::Section,
        WebAssetRole::Footer => Role::Footer,
        WebAssetRole::Og => Role::Og,
        WebAssetRole::Favicon => Role::Favicon,
        WebAssetRole::Video => Role::Video,
        WebAssetRole::VideoPreview => Role::Preview,
        WebAssetRole::VideoPoster => Role::Poster,
    }
}

/// Maps a version. `None` for a placeholder: a site without pages has no
/// capture.
pub fn map_capture(
    pages_json: Option<&str>,
    meta_json: Option<&str>,
    title: Option<&str>,
    site: SiteJson<'_>,
    files: &FileRefs,
    objects: &mut ObjectTable,
) -> Option<Capture> {
    let found = capture_files(pages_json, meta_json);
    if found.pages == 0 {
        return None;
    }
    let pages: Vec<Value> = pages_json
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|v| match v {
            Value::Array(items) => Some(items),
            _ => None,
        })
        .unwrap_or_default();
    let meta: Option<Map<String, Value>> = meta_json
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|v| match v {
            Value::Object(map) => Some(map),
            _ => None,
        });

    let site_json = site.values().map(site_json_value);
    let site_json_invalid = site_json.iter().filter(|v| v.is_err()).count() as u64;
    let [palette, fonts, tech, awards] = site_json;
    let mut capture = Capture {
        pages: found.pages,
        partial: false,
        engine: None,
        viewport: None,
        title: title
            .map(str::to_owned)
            .or_else(|| meta.as_ref().and_then(|m| str_field(m, "title"))),
        pages_json: Some(Value::Array(pages.iter().map(strip_page).collect()).to_string()),
        meta_json: None,
        traits_json: None,
        palette_json: palette.unwrap_or_default(),
        fonts_json: fonts.unwrap_or_default(),
        tech_json: tech.unwrap_or_default(),
        awards_json: awards.unwrap_or_default(),
        site_json_invalid,
        hero: None,
        favicon: None,
        assets: Vec::new(),
    };

    if let Some(mut meta) = meta {
        if let Some(Value::Object(settings)) = meta.get("capture") {
            capture.partial = settings
                .get("skipped")
                .and_then(Value::as_array)
                .is_some_and(|skipped| !skipped.is_empty());
            capture.engine = str_field(settings, "engine");
            capture.viewport = settings.get("viewport").and_then(viewport);
        }
        capture.traits_json = meta
            .remove("traits")
            .filter(|t| !t.is_null())
            .map(|t| t.to_string());
        strip_meta(&mut meta);
        capture.meta_json = Some(Value::Object(meta).to_string());
    }

    for asset in &found.refs {
        let Some(object) = objects.use_path(files, &asset.path, object_role(asset.role)) else {
            continue;
        };
        let page = asset.page_index;
        let (css_top, css_height) = page
            .and_then(|i| pages.get(i))
            .map_or((None, None), |p| css_box(p, asset.role, asset.seq));
        capture.assets.push(Asset {
            page_index: page.map_or(SITE_LEVEL, |i| i64::try_from(i).unwrap_or(i64::MAX)),
            role: asset.role.as_str(),
            seq: i64::try_from(asset.seq).unwrap_or(i64::MAX),
            object,
            css_top,
            css_height,
        });
    }
    // The hero of the first page that has one, else its screenshot.
    let first = |role: WebAssetRole| {
        capture
            .assets
            .iter()
            .filter(|a| a.role == role.as_str() && a.page_index >= 0)
            .min_by_key(|a| a.page_index)
            .map(|a| a.object)
    };
    capture.hero = first(WebAssetRole::Hero).or_else(|| first(WebAssetRole::Screenshot));
    capture.favicon = capture
        .assets
        .iter()
        .find(|a| a.role == WebAssetRole::Favicon.as_str())
        .map(|a| a.object);
    Some(capture)
}

fn str_field(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}

/// `{width, height}` → `"1440x900"`.
fn viewport(value: &Value) -> Option<String> {
    let width = value.get("width")?.as_f64()?;
    let height = value.get("height")?.as_f64()?;
    Some(format!("{width}x{height}"))
}

/// The position of a band or section on its page, in CSS pixels.
fn css_box(page: &Value, role: WebAssetRole, seq: usize) -> (Option<i64>, Option<i64>) {
    let list = match role {
        WebAssetRole::Band | WebAssetRole::Filmstrip => "chunks",
        WebAssetRole::Section => "sections",
        _ => return (None, None),
    };
    let Some(item) = page
        .get(list)
        .and_then(Value::as_array)
        .and_then(|l| l.get(seq))
    else {
        return (None, None);
    };
    let number = |key: &str| {
        item.get(key)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .map(|n| n.round() as i64)
    };
    (number("top"), number("cssHeight"))
}

/// A page without its file paths.
fn strip_page(page: &Value) -> Value {
    let mut page = page.clone();
    let Some(map) = page.as_object_mut() else {
        return page;
    };
    remove_path(map, "screenshotPath");
    for key in ["hero", "footer"] {
        if let Some(Value::Object(inner)) = map.get_mut(key) {
            remove_path(inner, "path");
        }
    }
    if let Some(Value::Array(chunks)) = map.get_mut("chunks") {
        for chunk in chunks.iter_mut().filter_map(Value::as_object_mut) {
            remove_path(chunk, "screenshotPath");
        }
    }
    if let Some(Value::Array(sections)) = map.get_mut("sections") {
        for section in sections.iter_mut().filter_map(Value::as_object_mut) {
            remove_path(section, "path");
        }
    }
    page
}

/// Site metadata without its file paths.
fn strip_meta(meta: &mut Map<String, Value>) {
    remove_path(meta, "ogImagePath");
    remove_path(meta, "favicon");
    if let Some(Value::Object(video)) = meta.get_mut("video") {
        for key in ["path", "preview", "poster"] {
            remove_path(video, key);
        }
    }
}

/// Removes `key` when it holds a local path (URLs and other values stay).
fn remove_path(map: &mut Map<String, Value>, key: &str) {
    if map
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(is_local_path)
    {
        map.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_leave_the_json_and_settings_fill_the_columns() {
        let pages = r#"[{"url":"https://e.test/","title":"Home","screenshotPath":"/u/assets/web/s.webp",
            "hero":{"path":"/u/assets/web/s.webp","width":2880},
            "chunks":[{"screenshotPath":"/u/assets/web/c0.webp","top":900,"cssHeight":1200.4}],
            "sections":[{"kind":"hero","path":"/u/assets/web/x.webp","top":0,"cssHeight":800}],
            "footer":{"path":"/u/assets/web/f.webp"},"contentText":"Selected work"}]"#;
        let meta = r#"{"title":"Studio","ogImage":"https://e.test/og.png","ogImagePath":"/u/assets/web/og.webp",
            "favicon":"/u/assets/web/fav.webp","traits":{"scroll":"smooth"},
            "video":{"path":"/u/assets/web/v.mp4","preview":"/u/assets/web/p.mp4","poster":null},
            "capture":{"engine":"playwright","viewport":{"width":1440,"height":900,"scale":2},"skipped":["https://e.test/x"]}}"#;
        let dir = tempfile::tempdir().unwrap();
        let web = dir.path().join("assets/web");
        std::fs::create_dir_all(&web).unwrap();
        for (name, bytes) in [
            ("s.webp", &b"RIFF\x24\0\0\0WEBPVP8 s"[..]),
            ("c0.webp", &b"RIFF\x24\0\0\0WEBPVP8 c"[..]),
            ("fav.webp", &b"RIFF\x24\0\0\0WEBPVP8 f"[..]),
        ] {
            std::fs::write(web.join(name), bytes).unwrap();
        }
        let mut files = FileRefs::default();
        for r in capture_files(Some(pages), Some(meta)).refs {
            files.add(crate::files::FileClass::Web(r.role), &r.path);
        }
        files.check(dir.path());
        let mut objects = ObjectTable::hash(&files, false);

        let capture = map_capture(
            Some(pages),
            Some(meta),
            None,
            SiteJson::default(),
            &files,
            &mut objects,
        )
        .unwrap();
        assert_eq!(capture.pages, 1);
        assert!(capture.partial);
        assert_eq!(capture.engine.as_deref(), Some("playwright"));
        assert_eq!(capture.viewport.as_deref(), Some("1440x900"));
        assert_eq!(capture.title.as_deref(), Some("Studio"));
        assert_eq!(
            capture.traits_json.as_deref(),
            Some(r#"{"scroll":"smooth"}"#)
        );
        for json in [&capture.pages_json, &capture.meta_json] {
            let json = json.as_deref().unwrap();
            assert!(!json.contains("/u/assets"), "{json}");
        }
        assert!(capture.meta_json.as_deref().unwrap().contains("og.png"));
        assert!(
            capture
                .pages_json
                .as_deref()
                .unwrap()
                .contains("Selected work")
        );

        // Present files only: the screenshot (also the hero), one band, the favicon.
        let roles: Vec<(i64, &str, i64)> = capture
            .assets
            .iter()
            .map(|a| (a.page_index, a.role, a.seq))
            .collect();
        assert_eq!(
            roles,
            [
                (0, "screenshot", 0),
                (0, "hero", 0),
                (0, "band", 0),
                (SITE_LEVEL, "favicon", 0)
            ]
        );
        let band = &capture.assets[2];
        assert_eq!((band.css_top, band.css_height), (Some(900), Some(1200)));
        let hero = capture.hero.unwrap();
        assert_eq!(objects.objects[hero].role, Some(Role::Screenshot));
        assert_eq!(
            objects.objects[capture.favicon.unwrap()].role,
            Some(Role::Favicon)
        );
    }

    #[test]
    fn a_site_without_pages_is_a_placeholder() {
        let files = FileRefs::default();
        let mut objects = ObjectTable::default();
        assert_eq!(
            map_capture(
                Some("[]"),
                Some("{}"),
                None,
                SiteJson::default(),
                &files,
                &mut objects
            ),
            None
        );
        assert_eq!(
            map_capture(None, None, None, SiteJson::default(), &files, &mut objects),
            None
        );
    }

    #[test]
    fn palette_fonts_tech_and_awards_are_copied_verbatim() {
        let pages = r#"[{"url":"https://e.test/","title":"Home"}]"#;
        let files = FileRefs::default();
        let mut objects = ObjectTable::default();
        // Spacing kept: verbatim, not re-serialized.
        let palette = r##"[ {"hex":"#0A0A0A","weight":0.6} ]"##;
        let site = SiteJson {
            palette: Some(palette),
            fonts: Some(r#"[{"family":"Inter"}]"#),
            tech: Some(r#"["Next.js"]"#),
            awards: Some(r#"[{"name":"Awwwards SOTD"}]"#),
        };
        assert_eq!(site.invalid(), 0);
        let capture = map_capture(Some(pages), None, None, site, &files, &mut objects).unwrap();
        assert_eq!(capture.palette_json.as_deref(), Some(palette));
        assert_eq!(
            capture.fonts_json.as_deref(),
            Some(r#"[{"family":"Inter"}]"#)
        );
        assert_eq!(capture.tech_json.as_deref(), Some(r#"["Next.js"]"#));
        assert_eq!(
            capture.awards_json.as_deref(),
            Some(r#"[{"name":"Awwwards SOTD"}]"#)
        );
        assert_eq!(capture.site_json_invalid, 0);

        // Not JSON → NULL and counted; blank and `null` → NULL, not counted.
        let site = SiteJson {
            palette: Some("[#0A0A0A"),
            fonts: Some("  "),
            tech: Some("null"),
            awards: Some("{\"x\":"),
        };
        assert_eq!(site.invalid(), 2);
        let capture = map_capture(Some(pages), None, None, site, &files, &mut objects).unwrap();
        assert_eq!(
            (
                &capture.palette_json,
                &capture.fonts_json,
                &capture.tech_json,
                &capture.awards_json
            ),
            (&None, &None, &None, &None)
        );
        assert_eq!(capture.site_json_invalid, 2);
    }
}
