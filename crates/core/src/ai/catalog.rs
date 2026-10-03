//! Catalog requests (plan §2.15; AI-08, AI-09, AI-11): the social and website
//! prompts of `shared/ai/`, with the caption or page text as untrusted data
//! between markers, the archive's vocabulary hint (social) or the detected tech
//! stack (web), and the response schema.
//!
//! The port of `shared/ai/catalog.ts` (`buildUserPrompt`, `buildWebUserPrompt`,
//! `catalogRequest`, `stripPromptMarkers`), pinned to it byte for byte by the
//! golden sets `shared/golden/ai/catalog/{markers,user-prompt,request}.jsonl`.
//!
//! One deliberate difference: a cut at the caption limit that falls inside a
//! surrogate pair keeps a lone high surrogate on the desktop (an invalid
//! string); here the whole character is dropped.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use super::prompts::{self, PromptError, ResponseSchema, Task};
use super::template::Var;
use crate::search::terms::js_trim;

/// Which catalog a post gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogKind {
    /// Social posts: images, carousels, videos, text.
    Social,
    /// Websites (web references).
    Web,
}

impl CatalogKind {
    /// The catalog of a post, decided by the post itself: a website (platform
    /// `web`, or media type `website`) always gets the web prompt, never the
    /// social one (plan §1.2 #7; the desktop's `isWebPost`).
    #[must_use]
    pub fn of(platform: &str, media_type: &str) -> Self {
        if platform == "web" || media_type == "website" {
            Self::Web
        } else {
            Self::Social
        }
    }

    /// The manifest task of this catalog.
    #[must_use]
    pub const fn task(self) -> Task {
        match self {
            Self::Social => Task::Catalog,
            Self::Web => Task::WebCatalog,
        }
    }
}

/// A catalog request, provider-neutral: the images go after the user text, as
/// parts of the same user message.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogRequest {
    /// The system prompt.
    pub system: String,
    /// The user text.
    pub user: String,
    /// The response schema.
    pub schema: &'static ResponseSchema,
    /// Sampling temperature.
    pub temperature: f64,
    /// `max_tokens`.
    pub max_tokens: u32,
}

static MARKERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<<<.*?>>>").expect("valid pattern"));

/// Neutralizes delimiter-like markers (`<<<…>>>`) inside untrusted text before
/// it is placed between the prompt's data markers: a caption holding the
/// literal closing marker could otherwise end the data region early and
/// smuggle instructions into the prompt.
#[must_use]
pub fn strip_prompt_markers(text: &str) -> String {
    MARKERS.replace_all(text, " ").into_owned()
}

/// The untrusted text of a prompt: markers stripped, trimmed, and cut at `max`
/// UTF-16 units (JavaScript's `length`) with `…`.
fn untrusted_text(text: Option<&str>, max: usize) -> String {
    let Some(text) = text else {
        return String::new();
    };
    let stripped = strip_prompt_markers(text);
    let clean = js_trim(&stripped);
    if clean.encode_utf16().count() <= max {
        return clean.to_owned();
    }
    let mut units = 0;
    let mut end = 0;
    for (at, c) in clean.char_indices() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        end = at + c.len_utf8();
    }
    format!("{}…", &clean[..end])
}

/// The non-blank hints, at most `max`, comma-separated.
fn hint_list<S: AsRef<str>>(hints: &[S], max: usize) -> String {
    hints
        .iter()
        .map(AsRef::as_ref)
        .filter(|hint| !js_trim(hint).is_empty())
        .take(max)
        .collect::<Vec<_>>()
        .join(", ")
}

fn limit(value: Option<usize>, what: &str) -> usize {
    value.unwrap_or_else(|| panic!("shared/ai: a catalog task has no {what} (prompts tests)"))
}

/// The values of a string enum of the web schema, comma-separated.
fn web_enum(property: &str) -> String {
    let schema = prompts::response_schema(Task::WebCatalog).expect("the web catalog has a schema");
    schema.value["properties"][property]["enum"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// The user text of a catalog request (`buildUserPrompt`). `text` is the
/// caption (social) or the page text (web); `hints` the archive's most
/// frequent tags (social) or the detected tech stack (web); `has_frames`
/// whether images go with it.
///
/// # Errors
///
/// [`PromptError`] when the template does not render (the tests rule it out).
pub fn user_prompt<S: AsRef<str>>(
    kind: CatalogKind,
    text: Option<&str>,
    hints: &[S],
    has_frames: bool,
) -> Result<String, PromptError> {
    let task = kind.task();
    let spec = prompts::spec(task);
    let caption = untrusted_text(text, limit(spec.caption_max, "captionMax"));
    let hints = hint_list(hints, limit(spec.hints_max, "hintsMax"));
    match kind {
        CatalogKind::Social => prompts::user_prompt(
            task,
            &[
                ("frames", Var::Flag(has_frames)),
                ("caption", Var::Text(&caption)),
                ("vocabulary", Var::Text(&hints)),
            ],
        ),
        CatalogKind::Web => {
            let purposes = web_enum("purpose");
            let industries = web_enum("industry");
            prompts::user_prompt(
                task,
                &[
                    ("frames", Var::Flag(has_frames)),
                    ("caption", Var::Text(&caption)),
                    ("tech", Var::Text(&hints)),
                    ("purposes", Var::Text(&purposes)),
                    ("industries", Var::Text(&industries)),
                ],
            )
        }
    }
}

/// A complete catalog request (`catalogRequest`): prompts, response schema and
/// sampling, from the manifest's task for `kind`.
///
/// # Errors
///
/// As [`user_prompt`].
pub fn request<S: AsRef<str>>(
    kind: CatalogKind,
    text: Option<&str>,
    hints: &[S],
    has_frames: bool,
) -> Result<CatalogRequest, PromptError> {
    let task = kind.task();
    Ok(CatalogRequest {
        system: prompts::system_prompt(task, &[])?,
        user: user_prompt(kind, text, hints, has_frames)?,
        schema: prompts::response_schema(task).expect("catalog tasks have a schema"),
        temperature: prompts::spec(task).temperature,
        max_tokens: prompts::max_tokens(task, 0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: &[&str] = &[];

    #[test]
    fn the_post_decides_the_prompt() {
        assert_eq!(CatalogKind::of("web", "image"), CatalogKind::Web);
        assert_eq!(CatalogKind::of("instagram", "website"), CatalogKind::Web);
        assert_eq!(
            CatalogKind::of("instagram", "carousel"),
            CatalogKind::Social
        );
        assert_eq!(CatalogKind::of("manual", "image"), CatalogKind::Social);
    }

    #[test]
    fn untrusted_text_is_stripped_trimmed_and_cut() {
        assert_eq!(untrusted_text(None, 10), "");
        assert_eq!(untrusted_text(Some(" a <<<END>>> b "), 10), "a   b");
        assert_eq!(untrusted_text(Some("abcdefghijk"), 10), "abcdefghij…");
        assert_eq!(untrusted_text(Some("abcdefghij"), 10), "abcdefghij");
        // U+1F3A7 is two UTF-16 units: a cut through it drops the whole character.
        assert_eq!(
            untrusted_text(Some("abcdefghi\u{1F3A7}x"), 10),
            "abcdefghi…"
        );
        assert_eq!(
            untrusted_text(Some("abcdefgh\u{1F3A7}x"), 10),
            "abcdefgh\u{1F3A7}…"
        );
        assert_eq!(untrusted_text(Some("\u{FEFF} x \u{3000}"), 10), "x");
    }

    #[test]
    fn hints_skip_blanks_and_stop_at_the_limit() {
        assert_eq!(hint_list(&["a", " ", "", "b", "c"], 2), "a, b");
        assert_eq!(hint_list(NONE, 30), "");
    }

    #[test]
    fn the_social_prompt_wraps_the_caption_and_lists_the_vocabulary() {
        let text = user_prompt(
            CatalogKind::Social,
            Some("Lamp"),
            &["design", "glass"],
            true,
        )
        .unwrap();
        assert!(text.starts_with("These are frames in chronological order"));
        assert!(text.contains("<<<CAPTION>>>\nLamp\n<<<END CAPTION>>>"));
        assert!(text.contains(": design, glass. Do NOT choose a tag"));
        let bare = user_prompt(CatalogKind::Social, None, NONE, false).unwrap();
        assert!(bare.starts_with("This is a text-only post"));
        assert!(!bare.contains("CAPTION"));
        assert!(!bare.contains("Existing archive vocabulary"));
    }

    #[test]
    fn the_web_prompt_lists_the_schema_enums_and_the_tech_stack() {
        let text = user_prompt(CatalogKind::Web, Some("Studio"), &["Next.js"], false).unwrap();
        assert!(text.contains("- purpose: ONE of portfolio, e-commerce, saas,"));
        assert!(text.contains("personal, other — the site's MAIN purpose"));
        assert!(text.contains("beauty, other — the sector"));
        assert!(text.contains("(NOT inferred): Next.js. Use it"));
    }

    #[test]
    fn requests_come_from_the_manifest() {
        let social = request(CatalogKind::Social, Some("x"), NONE, true).unwrap();
        assert_eq!(social.schema.name, "video_catalog");
        assert_eq!(social.temperature, 0.2);
        assert_eq!(social.max_tokens, 768);
        assert!(
            social
                .system
                .starts_with("You are an assistant that catalogs images")
        );
        let web = request(CatalogKind::Web, Some("x"), NONE, true).unwrap();
        assert_eq!(web.schema.name, "web_catalog");
        assert!(
            web.system
                .starts_with("You are an assistant that catalogs WEBSITES")
        );
    }
}
