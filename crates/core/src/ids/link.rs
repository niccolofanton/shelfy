//! Shared links (plan §2.17; P2 contract C7): the post a URL names.
//!
//! The Android share target, the bookmarklet and the iOS Shortcut all send
//! a URL to `POST /links`. [`classify`] checks it and names its post:
//!
//! | URL | [`Link`] | Key |
//! |---|---|---|
//! | Instagram `/p/<code>`, `/reel/<code>`, `/reels/<code>`, `/tv/<code>`, also after a user name, on `instagram.com` (`www.`, `m.`) or `instagr.am` | [`Link::Post`] | `ig_<pk>` |
//! | X `…/status/<id>` (or `statuses`) on `x.com` or `twitter.com` (`www.`, `mobile.`, `m.`) | [`Link::Post`] | `x_<id>` |
//! | Pinterest `/pin/<id>/` (or `/pin/<slug>--<id>/`) on [`PINTEREST_HOSTS`] and their subdomains | [`Link::Post`] | `pin_<id>` |
//! | Pinterest's short link `pin.it/<code>` | [`Link::PinterestShort`]: resolve it, then classify where it leads | — |
//! | any other http(s) URL, a platform's profile or board included | [`Link::Web`] | `web_<sha1:20>` (§2.8) |
//!
//! **Refused** ([`LinkError`]): an empty or over-long string (4,096 UTF-16
//! code units, as captured URLs), whitespace or control characters, a URL
//! that does not parse, a scheme other than http and https (`javascript:`,
//! `data:`, `file:`…), a user name or password, a port other than 80 and
//! 443, a host that is an address (IPv4 in any of its spellings, IPv6), a
//! name without a dot, with a trailing dot, or under `localhost`, and a
//! platform post URL whose id is not valid. The outbound client refuses the
//! same destinations (P2-04), so a web post never names a place the capture
//! (P4) could not fetch.
//!
//! **Canonical URLs.** A post's link is rebuilt on the platform's main host
//! over https, without the query and the fragment, where share sheets put
//! their tracking (`igsh`, `s`, `t`, `invite_code`…):
//! `https://www.instagram.com/<p|reel|tv>/<code>/`,
//! `https://x.com/<user>/status/<id>` (`/i/status/<id>` without a user),
//! `https://www.pinterest.com/pin/<id>/`. A web link keeps its URL without
//! the fragment and the §2.8 tracking parameters (`utm_*`, `gclid`,
//! `fbclid`, `ref`); its key hashes the same normalization ([`web::from_url`]).
//!
//! **Long Instagram codes.** A private post's code carries 28 more
//! characters after the code of its pk (yt-dlp's `_id_to_pk`): the key is
//! the pk of the code without them, so it matches the key a sync of the
//! same post gives (`<pk>_<owner>`). The link keeps the whole code.

use url::{Host, Url};

use super::ig::{MAX_SHORTCODE_LEN, MediaPk};
use super::{CanonicalId, Platform, is_ascii_digits, web, x};
use crate::ingest::hosts::{MAX_URL_LEN, PINTEREST_HOSTS, utf16_len};
use crate::search::terms::js_trim;

/// Pinterest's short-link host.
pub const PINTEREST_SHORT_HOST: &str = "pin.it";

/// Characters an Instagram private-post code carries after its pk's code.
const IG_PRIVATE_SUFFIX: usize = 28;

/// Instagram's hosts that serve post links.
const INSTAGRAM_HOSTS: [&str; 5] = [
    "instagram.com",
    "www.instagram.com",
    "m.instagram.com",
    "instagr.am",
    "www.instagr.am",
];

/// X's hosts that serve post links.
const X_HOSTS: [&str; 8] = [
    "x.com",
    "www.x.com",
    "mobile.x.com",
    "m.x.com",
    "twitter.com",
    "www.twitter.com",
    "mobile.twitter.com",
    "m.twitter.com",
];

/// Query parameters a web link drops (compared lowercase), besides every
/// `utm_*` one: the list of §2.8.
const TRACKING_PARAMS: [&str; 3] = ["gclid", "fbclid", "ref"];

/// What a shared URL names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// A post of Instagram, X or Pinterest.
    Post(PostLink),
    /// A Pinterest short link: resolve it (P2-04's client, allowlisted
    /// redirects), then [`classify`] where it leads.
    PinterestShort(Url),
    /// A website: a web post.
    Web(WebLink),
}

impl Link {
    /// The key of the post the link names, `None` for a short link.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        match self {
            Self::Post(post) => Some(post.id.key()),
            Self::Web(web) => Some(web.id.key()),
            Self::PinterestShort(_) => None,
        }
    }
}

/// A platform post named by a link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostLink {
    /// Its identity (§2.8).
    pub id: CanonicalId,
    /// The canonical link to it (module docs).
    pub url: String,
    /// Instagram's code, as the link gives it.
    pub shortcode: Option<String>,
}

/// A website named by a link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebLink {
    /// Its identity (§2.8).
    pub id: CanonicalId,
    /// The URL without its fragment and tracking parameters.
    pub url: String,
}

/// Why a URL is not a link Shelfy saves. The variants never carry the URL:
/// links are user data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// Nothing but whitespace.
    #[error("the URL is empty")]
    Empty,
    /// Longer than 4,096 UTF-16 code units.
    #[error("the URL is longer than 4096 characters")]
    TooLong,
    /// Whitespace or a control character inside, or not an absolute URL.
    #[error("not an absolute URL")]
    Malformed,
    /// Not http or https.
    #[error("only http and https links can be saved")]
    Scheme,
    /// A user name or password.
    #[error("the URL carries credentials")]
    Credentials,
    /// A port other than 80 and 443.
    #[error("the port is not allowed")]
    Port,
    /// An address, a name without a dot or with a trailing dot, or a local
    /// name.
    #[error("the host is not a public name")]
    Host,
    /// A platform's post URL whose id is not valid.
    #[error("the post id in the URL is not valid")]
    PostId,
}

impl LinkError {
    /// A stable code for logs and tests.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooLong => "too_long",
            Self::Malformed => "malformed",
            Self::Scheme => "scheme",
            Self::Credentials => "credentials",
            Self::Port => "port",
            Self::Host => "host",
            Self::PostId => "post_id",
        }
    }
}

/// What `raw` names (module docs). Surrounding whitespace is ignored.
///
/// # Errors
///
/// [`LinkError`] when the URL is not one Shelfy saves.
pub fn classify(raw: &str) -> Result<Link, LinkError> {
    let url = parse(raw)?;
    let host = url.host_str().ok_or(LinkError::Malformed)?.to_owned();
    let segments: Vec<&str> = url
        .path_segments()
        .map(|segments| segments.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    if INSTAGRAM_HOSTS.contains(&host.as_str()) {
        if let Some(post) = instagram(&segments)? {
            return Ok(Link::Post(post));
        }
    } else if X_HOSTS.contains(&host.as_str()) {
        if let Some(post) = twitter(&segments)? {
            return Ok(Link::Post(post));
        }
    } else if host == PINTEREST_SHORT_HOST {
        return match segments.as_slice() {
            [_code, ..] => Ok(Link::PinterestShort(url)),
            [] => Err(LinkError::PostId),
        };
    } else if is_pinterest_host(&host)
        && let Some(post) = pinterest(&segments)?
    {
        return Ok(Link::Post(post));
    }
    let cleaned = clean_web_url(url);
    let id = web::from_url(&cleaned).map_err(|_| LinkError::Malformed)?;
    Ok(Link::Web(WebLink { id, url: cleaned }))
}

/// Whether `host` is one of Pinterest's sites ([`PINTEREST_HOSTS`]) or a
/// subdomain of one.
#[must_use]
pub fn is_pinterest_host(host: &str) -> bool {
    PINTEREST_HOSTS.iter().any(|domain| {
        host.strip_suffix(domain)
            .is_some_and(|rest| rest.is_empty() || (rest.len() > 1 && rest.ends_with('.')))
    })
}

/// The checks every link passes (module docs).
fn parse(raw: &str) -> Result<Url, LinkError> {
    let raw = js_trim(raw);
    if raw.is_empty() {
        return Err(LinkError::Empty);
    }
    if utf16_len(raw) > MAX_URL_LEN {
        return Err(LinkError::TooLong);
    }
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(LinkError::Malformed);
    }
    let url = Url::parse(raw).map_err(|_| LinkError::Malformed)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(LinkError::Scheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(LinkError::Credentials);
    }
    if !matches!(url.port(), None | Some(80 | 443)) {
        return Err(LinkError::Port);
    }
    match url.host() {
        Some(Host::Domain(name)) if is_public_name(name) => Ok(url),
        Some(_) => Err(LinkError::Host),
        None => Err(LinkError::Malformed),
    }
}

/// A name with a dot, no trailing dot, outside `localhost`.
fn is_public_name(name: &str) -> bool {
    name.contains('.')
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name != "localhost"
        && !name.ends_with(".localhost")
}

/// An Instagram post path: `[user/]<p|reel|reels|tv>/<code>`. `Ok(None)`
/// for any other path (a profile, a story: a web link).
fn instagram(segments: &[&str]) -> Result<Option<PostLink>, LinkError> {
    let at = match segments {
        [kind, ..] if is_ig_kind(kind) => 0,
        [_, kind, ..] if is_ig_kind(kind) => 1,
        _ => return Ok(None),
    };
    let Some(code) = segments.get(at + 1) else {
        return Err(LinkError::PostId);
    };
    let valid = !code.is_empty()
        && code.len() <= MAX_SHORTCODE_LEN
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !valid {
        return Err(LinkError::PostId);
    }
    let own = if code.len() > IG_PRIVATE_SUFFIX {
        &code[..code.len() - IG_PRIVATE_SUFFIX]
    } else {
        code
    };
    let pk = MediaPk::from_shortcode(own).map_err(|_| LinkError::PostId)?;
    let kind = match segments[at] {
        "reel" | "reels" => "reel",
        "tv" => "tv",
        _ => "p",
    };
    Ok(Some(PostLink {
        id: pk.canonical(),
        url: format!("https://www.instagram.com/{kind}/{code}/"),
        shortcode: Some((*code).to_owned()),
    }))
}

fn is_ig_kind(segment: &str) -> bool {
    matches!(segment, "p" | "reel" | "reels" | "tv")
}

/// An X status path: `…/status/<id>` or `…/statuses/<id>`. `Ok(None)` for
/// any other path (a profile, a list: a web link).
fn twitter(segments: &[&str]) -> Result<Option<PostLink>, LinkError> {
    let Some(at) = segments
        .iter()
        .position(|s| *s == "status" || *s == "statuses")
    else {
        return Ok(None);
    };
    let Some(raw) = segments.get(at + 1) else {
        return Err(LinkError::PostId);
    };
    if !is_ascii_digits(raw) {
        return Err(LinkError::PostId);
    }
    let id = x::from_legacy(raw, None).map_err(|_| LinkError::PostId)?;
    // `/i/web/status/<id>` and `/i/status/<id>` name no user.
    let user = at
        .checked_sub(1)
        .map(|before| segments[before])
        .filter(|user| is_x_handle(user) && !segments[..at].contains(&"i"));
    let url = match user {
        Some(user) => format!("https://x.com/{user}/status/{}", id.native_id()),
        None => format!("https://x.com/i/status/{}", id.native_id()),
    };
    Ok(Some(PostLink {
        id,
        url,
        shortcode: None,
    }))
}

/// A user name X allows: 1–15 letters, digits and underscores; `i` is
/// X's own path, not a user.
fn is_x_handle(segment: &str) -> bool {
    segment != "i"
        && (1..=15).contains(&segment.len())
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A Pinterest pin path: `/pin/<id>/`, or `/pin/<slug>--<id>/`. `Ok(None)`
/// for any other path (a board, a profile: a web link).
fn pinterest(segments: &[&str]) -> Result<Option<PostLink>, LinkError> {
    let Some(at) = segments.iter().position(|s| *s == "pin") else {
        return Ok(None);
    };
    let Some(raw) = segments.get(at + 1) else {
        return Err(LinkError::PostId);
    };
    let digits = raw.rsplit_once("--").map_or(*raw, |(_, id)| id);
    if !is_ascii_digits(digits) || digits.len() > 64 || digits.bytes().all(|b| b == b'0') {
        return Err(LinkError::PostId);
    }
    let id = super::pinterest::from_legacy(digits, None).map_err(|_| LinkError::PostId)?;
    let url = format!("https://www.pinterest.com/pin/{}/", id.native_id());
    Ok(Some(PostLink {
        id,
        url,
        shortcode: None,
    }))
}

/// A web link without its fragment and tracking parameters.
fn clean_web_url(mut url: Url) -> String {
    url.set_fragment(None);
    if url.query().is_some() {
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let kept: Vec<&(String, String)> = pairs
            .iter()
            .filter(|(name, _)| !is_tracking_param(name))
            .collect();
        if kept.len() != pairs.len() {
            if kept.is_empty() {
                url.set_query(None);
            } else {
                url.query_pairs_mut()
                    .clear()
                    .extend_pairs(kept.iter().map(|(n, v)| (n.as_str(), v.as_str())));
            }
        }
    }
    url.into()
}

fn is_tracking_param(name: &str) -> bool {
    let lowered = name.to_lowercase();
    lowered.starts_with("utm_") || TRACKING_PARAMS.contains(&lowered.as_str())
}

/// The platform of a classified post link.
#[must_use]
pub fn platform_of(link: &Link) -> Option<Platform> {
    match link {
        Link::Post(post) => Some(post.id.platform()),
        Link::Web(_) => Some(Platform::Web),
        Link::PinterestShort(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(raw: &str) -> PostLink {
        match classify(raw) {
            Ok(Link::Post(post)) => post,
            other => panic!("{raw:?} is not a post link: {other:?}"),
        }
    }

    fn web(raw: &str) -> WebLink {
        match classify(raw) {
            Ok(Link::Web(web)) => web,
            other => panic!("{raw:?} is not a web link: {other:?}"),
        }
    }

    #[test]
    fn instagram_posts_reels_and_tv() {
        // `CuZLd-iMknW` is the code of pk 3141592653589793238 (ids::ig).
        for raw in [
            "https://www.instagram.com/p/CuZLd-iMknW/",
            "https://instagram.com/p/CuZLd-iMknW",
            "http://m.instagram.com/p/CuZLd-iMknW/?igsh=abc&utm_source=ig_web",
            "https://www.instagram.com/someone/p/CuZLd-iMknW/",
            "https://instagr.am/p/CuZLd-iMknW/#comments",
            " https://www.instagram.com/p/CuZLd-iMknW/\n",
        ] {
            let link = post(raw);
            assert_eq!(link.id.key(), "ig_3141592653589793238", "{raw}");
            assert_eq!(
                link.url, "https://www.instagram.com/p/CuZLd-iMknW/",
                "{raw}"
            );
            assert_eq!(link.shortcode.as_deref(), Some("CuZLd-iMknW"));
        }
        let reel = post("https://www.instagram.com/reels/CuZLd-iMknW/?igsh=x");
        assert_eq!(reel.url, "https://www.instagram.com/reel/CuZLd-iMknW/");
        assert_eq!(reel.id.key(), "ig_3141592653589793238");
        let tv = post("https://www.instagram.com/tv/CuZLd-iMknW");
        assert_eq!(tv.url, "https://www.instagram.com/tv/CuZLd-iMknW/");
    }

    #[test]
    fn a_private_instagram_code_keys_by_its_own_pk() {
        let suffix = "A".repeat(IG_PRIVATE_SUFFIX);
        let link = post(&format!("https://www.instagram.com/p/CuZLd-iMknW{suffix}/"));
        assert_eq!(link.id.key(), "ig_3141592653589793238");
        assert_eq!(
            link.url,
            format!("https://www.instagram.com/p/CuZLd-iMknW{suffix}/")
        );
    }

    #[test]
    fn x_statuses_on_every_host() {
        for raw in [
            "https://x.com/studio/status/1700000000000000001",
            "https://twitter.com/studio/status/1700000000000000001?s=20&t=abc",
            "https://mobile.twitter.com/studio/status/1700000000000000001",
            "https://mobile.x.com/studio/status/1700000000000000001/photo/1",
            "https://www.x.com/studio/statuses/1700000000000000001",
        ] {
            let link = post(raw);
            assert_eq!(link.id.key(), "x_1700000000000000001", "{raw}");
            assert_eq!(
                link.url, "https://x.com/studio/status/1700000000000000001",
                "{raw}"
            );
        }
        for raw in ["https://x.com/i/status/42", "https://x.com/i/web/status/42"] {
            assert_eq!(post(raw).url, "https://x.com/i/status/42", "{raw}");
        }
    }

    #[test]
    fn pinterest_pins_on_every_site() {
        for raw in [
            "https://www.pinterest.com/pin/987654321012345678/",
            "https://it.pinterest.com/pin/987654321012345678/?nic=1",
            "https://pinterest.co.uk/pin/987654321012345678",
            "https://www.pinterest.com/pin/modern-living-room--987654321012345678/",
        ] {
            let link = post(raw);
            assert_eq!(link.id.key(), "pin_987654321012345678", "{raw}");
            assert_eq!(
                link.url, "https://www.pinterest.com/pin/987654321012345678/",
                "{raw}"
            );
        }
        let short = classify("https://pin.it/1AbCdEfGh").unwrap();
        assert!(matches!(short, Link::PinterestShort(_)));
        assert_eq!(short.key(), None);
        assert_eq!(classify("https://pin.it/"), Err(LinkError::PostId));
    }

    #[test]
    fn other_urls_are_web_links_without_tracking() {
        let link = web("https://Example.com/a/b?utm_source=x&id=7&fbclid=y#top");
        assert_eq!(link.url, "https://example.com/a/b?id=7");
        let same = web("http://www.example.com/a/b/?id=7&gclid=1");
        assert_eq!(
            link.id, same.id,
            "http and https, www and tracking collapse"
        );
        assert!(link.id.key().starts_with("web_"));
        assert_eq!(
            web("https://example.com/?ref=hn").url,
            "https://example.com/"
        );
        // A platform's other pages are web links.
        for raw in [
            "https://www.instagram.com/someone/",
            "https://x.com/someone",
            "https://www.pinterest.com/someone/board/",
            "https://www.instagram.com/stories/someone/1/",
        ] {
            web(raw);
        }
    }

    #[test]
    fn hostile_and_unusable_urls_are_refused() {
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LEN));
        for (raw, error) in [
            ("", LinkError::Empty),
            ("   ", LinkError::Empty),
            (long.as_str(), LinkError::TooLong),
            ("example.com/a", LinkError::Malformed),
            ("https://exa mple.com/", LinkError::Malformed),
            ("https://example.com/a\tb", LinkError::Malformed),
            ("https://example.com/\u{0}", LinkError::Malformed),
            ("javascript:alert(1)", LinkError::Scheme),
            ("JAVASCRIPT:alert(document.cookie)", LinkError::Scheme),
            (
                "data:text/html,<script>alert(1)</script>",
                LinkError::Scheme,
            ),
            ("file:///etc/passwd", LinkError::Scheme),
            ("ftp://example.com/a", LinkError::Scheme),
            ("https://user:pw@example.com/", LinkError::Credentials),
            ("https://user@x.com/a/status/1", LinkError::Credentials),
            ("https://example.com:8443/", LinkError::Port),
            ("https://127.0.0.1/", LinkError::Host),
            ("http://2130706433/", LinkError::Host),
            ("http://0x7f.0.0.1/", LinkError::Host),
            ("http://[::1]/", LinkError::Host),
            ("http://169.254.169.254/latest/meta-data/", LinkError::Host),
            ("http://localhost/", LinkError::Host),
            ("http://shelfy.localhost/", LinkError::Host),
            ("http://intranet/", LinkError::Host),
            ("http://example.com./", LinkError::Host),
            ("https://www.instagram.com/p/", LinkError::PostId),
            ("https://www.instagram.com/p/bad*code/", LinkError::PostId),
            ("https://x.com/a/status/abc", LinkError::PostId),
            ("https://x.com/a/status/0", LinkError::PostId),
            ("https://x.com/a/status/", LinkError::PostId),
            ("https://www.pinterest.com/pin/", LinkError::PostId),
            (
                "https://www.pinterest.com/pin/not-a-pin/",
                LinkError::PostId,
            ),
        ] {
            assert_eq!(classify(raw), Err(error), "{raw:?}");
        }
        // Default ports are fine.
        assert!(classify("https://example.com:443/").is_ok());
        assert!(classify("http://example.com:80/").is_ok());
    }

    #[test]
    fn platforms_of_links() {
        assert_eq!(
            platform_of(&classify("https://x.com/a/status/1").unwrap()),
            Some(Platform::Twitter)
        );
        assert_eq!(
            platform_of(&classify("https://example.com/").unwrap()),
            Some(Platform::Web)
        );
        assert!(is_pinterest_host("www.pinterest.com"));
        assert!(is_pinterest_host("pinterest.com.mx"));
        assert!(!is_pinterest_host("notpinterest.com"));
        assert!(!is_pinterest_host("pinterest.com.evil.io"));
    }
}
