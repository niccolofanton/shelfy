//! Platform hosts (plan §2.16): the hosts a captured item's URLs may point
//! at, per platform, and the expiry that Instagram and Facebook sign into
//! their CDN URLs.
//!
//! | Platform  | Hosts |
//! |-----------|-------|
//! | Instagram | `www.instagram.com`, `*.cdninstagram.com`, `*.fbcdn.net` |
//! | X         | `x.com`, `twitter.com`, `pbs.twimg.com`, `video.twimg.com` |
//! | Pinterest | `*.<host>` for each of [`PINTEREST_HOSTS`], `*.pinimg.com` |
//!
//! `*.example.com` stands for the domain and every subdomain of it, as in a
//! Chrome match pattern.
//!
//! [`PINTEREST_HOSTS`] mirrors `PINTEREST_HOSTS` in
//! `extension/src/shared/hosts.ts`, which the extension's manifest is built
//! from (its `CDN_MATCHES` are the media hosts above). The golden set
//! `hosts` (`scripts/golden/hosts.ts`, `shared/golden/hosts.jsonl`) holds
//! the extension's list, and `crates/core/tests/golden.rs` checks this one
//! against it, so the two cannot drift apart.

use url::Url;

use crate::repo::Platform;

/// The Pinterest sites, one per supported ccTLD; their subdomains (`www.`,
/// `it.`, …) are allowed too. The same hosts, in the same order, as the
/// extension's list.
pub const PINTEREST_HOSTS: &[&str] = &[
    "pinterest.com",
    "pinterest.it",
    "pinterest.de",
    "pinterest.fr",
    "pinterest.es",
    "pinterest.co.uk",
    "pinterest.ca",
    "pinterest.com.au",
    "pinterest.jp",
    "pinterest.com.mx",
    "pinterest.at",
    "pinterest.ch",
    "pinterest.pt",
    "pinterest.se",
    "pinterest.dk",
    "pinterest.nz",
    "pinterest.ie",
    "pinterest.ph",
    "pinterest.cl",
    "pinterest.co.kr",
    "pinterest.ru",
];

/// Longest URL accepted, in UTF-16 code units, as the desktop counts it
/// (`MAX_URL_LEN` in `src/lib/browserSanitize.ts`).
pub const MAX_URL_LEN: usize = 4_096;

/// One allowlist entry.
#[derive(Clone, Copy)]
enum Allowed {
    /// Exactly this host.
    Host(&'static str),
    /// This domain or any subdomain of it.
    Domain(&'static str),
}

impl Allowed {
    fn matches(self, host: &str) -> bool {
        match self {
            Allowed::Host(allowed) => host == allowed,
            Allowed::Domain(domain) => host
                .strip_suffix(domain)
                .is_some_and(|rest| rest.is_empty() || (rest.len() > 1 && rest.ends_with('.'))),
        }
    }
}

const INSTAGRAM: &[Allowed] = &[
    Allowed::Host("www.instagram.com"),
    Allowed::Domain("cdninstagram.com"),
    Allowed::Domain("fbcdn.net"),
];

const TWITTER: &[Allowed] = &[
    Allowed::Host("x.com"),
    Allowed::Host("twitter.com"),
    Allowed::Host("pbs.twimg.com"),
    Allowed::Host("video.twimg.com"),
];

/// Pinterest's image and video CDN.
const PINIMG: Allowed = Allowed::Domain("pinimg.com");

/// Whether `host` (lowercase, as [`Url::host_str`] gives it) is on the
/// allowlist of `platform`. Web and manual posts have no allowlist.
#[must_use]
pub fn allows_host(platform: Platform, host: &str) -> bool {
    match platform {
        Platform::Instagram => INSTAGRAM.iter().any(|a| a.matches(host)),
        Platform::Twitter => TWITTER.iter().any(|a| a.matches(host)),
        Platform::Pinterest => {
            PINIMG.matches(host)
                || PINTEREST_HOSTS
                    .iter()
                    .any(|&domain| Allowed::Domain(domain).matches(host))
        }
        Platform::Web | Platform::Manual => false,
    }
}

/// Length of `value` in UTF-16 code units, the unit of JavaScript's
/// `String#length`.
pub(crate) fn utf16_len(value: &str) -> usize {
    // A UTF-8 string has at least as many bytes as UTF-16 code units.
    if value.len() <= MAX_URL_LEN {
        return value.encode_utf16().count();
    }
    value.chars().map(char::len_utf16).sum()
}

/// Parses a URL that an item of `platform` may carry, or `None`.
///
/// The desktop's rule (`isHttpUrl`): a string of at most [`MAX_URL_LEN`]
/// UTF-16 code units that parses as an `http:` or `https:` URL. The port
/// adds:
///
/// - the host is on the platform's allowlist ([`allows_host`]);
/// - no user name or password, and the scheme's default port;
/// - no whitespace or control character anywhere: the URL parser would drop
///   them silently, but the string is stored as given.
#[must_use]
pub fn parse_allowed(platform: Platform, raw: &str) -> Option<Url> {
    if raw.is_empty()
        || utf16_len(raw) > MAX_URL_LEN
        || raw.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    let allowed = matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url
            .host_str()
            .is_some_and(|host| allows_host(platform, host));
    allowed.then_some(url)
}

/// The expiry of a signed Instagram/Facebook CDN URL: its `oe` query
/// parameter (unix seconds in hex, at most 12 digits), in ms. Any other URL
/// has none.
#[must_use]
pub fn cdn_url_expiry_ms(url: &str) -> Option<i64> {
    let query = url.split_once('?')?.1;
    let query = query.split('#').next().unwrap_or(query);
    let oe = query.split('&').find_map(|pair| pair.strip_prefix("oe="))?;
    if oe.is_empty() || oe.len() > 12 || !oe.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    i64::from_str_radix(oe, 16).ok().map(|s| s * 1_000)
}

/// File extensions of the other video files and of streaming manifests.
const OTHER_VIDEO_EXTENSIONS: [&str; 5] = [".m4v", ".mov", ".webm", ".m3u8", ".mpd"];

/// What a media URL's path says it points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoFile {
    /// A progressive MP4: a direct video URL (`post_media.video_url`).
    Mp4,
    /// Another video file or a streaming manifest (HLS, DASH): a video, but
    /// not one the server keeps as a direct URL.
    Other,
}

/// Whether `url`'s path names a video file (by its extension, case
/// insensitive), and which kind; `None` for anything else, an image or a
/// poster included.
#[must_use]
pub fn video_file(url: &Url) -> Option<VideoFile> {
    let path = url.path().to_ascii_lowercase();
    if path.ends_with(".mp4") {
        Some(VideoFile::Mp4)
    } else if OTHER_VIDEO_EXTENSIONS.iter().any(|ext| path.ends_with(ext)) {
        Some(VideoFile::Other)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlists_follow_the_plan() {
        let ig = |host: &str| allows_host(Platform::Instagram, host);
        assert!(ig("www.instagram.com"));
        assert!(ig("scontent.cdninstagram.com"));
        assert!(ig("scontent-mxp1-1.cdninstagram.com"));
        assert!(ig("instagram.fmxp1-1.fna.fbcdn.net"));
        assert!(ig("cdninstagram.com"));
        assert!(!ig("instagram.com"), "only www.instagram.com");
        assert!(!ig("l.instagram.com"));
        assert!(!ig("evilcdninstagram.com"));
        assert!(!ig("cdninstagram.com.evil.io"));
        assert!(!ig(".cdninstagram.com"));
        assert!(!ig("pbs.twimg.com"));

        let x = |host: &str| allows_host(Platform::Twitter, host);
        for host in ["x.com", "twitter.com", "pbs.twimg.com", "video.twimg.com"] {
            assert!(x(host), "{host}");
        }
        assert!(!x("mobile.twitter.com"));
        assert!(!x("abs.twimg.com"));
        assert!(!x("www.instagram.com"));

        let pin = |host: &str| allows_host(Platform::Pinterest, host);
        for host in PINTEREST_HOSTS {
            assert!(pin(host), "{host}");
            assert!(pin(&format!("www.{host}")), "www.{host}");
            assert!(pin(&format!("it.{host}")), "it.{host}");
        }
        assert!(pin("i.pinimg.com"));
        assert!(pin("v1.pinimg.com"));
        assert!(!pin("pinterest.com.evil.io"));
        assert!(!pin("pinterest.xyz"));
        assert!(!pin("notpinterest.com"));

        assert!(!allows_host(Platform::Web, "www.instagram.com"));
        assert!(!allows_host(Platform::Manual, "x.com"));
    }

    #[test]
    fn urls_must_be_plain_https_or_http_on_the_allowlist() {
        let ok = |platform, url| parse_allowed(platform, url).is_some();
        assert!(ok(
            Platform::Instagram,
            "https://scontent.cdninstagram.com/v/t51/1.jpg?oe=65A1B2C3"
        ));
        assert!(ok(Platform::Twitter, "http://pbs.twimg.com/media/a.jpg"));
        assert!(ok(Platform::Twitter, "HTTPS://PBS.TWIMG.COM/media/A.jpg"));
        assert!(ok(
            Platform::Twitter,
            "https://pbs.twimg.com:443/media/a.jpg"
        ));
        assert!(ok(
            Platform::Pinterest,
            "https://i.pinimg.com/originals/a.jpg"
        ));
        for bad in [
            "",
            "ftp://pbs.twimg.com/media/a.jpg",
            "javascript:alert(1)",
            "data:image/png;base64,AAAA",
            "pbs.twimg.com/media/a.jpg",
            "https://evil.example/a.jpg",
            "https://pbs.twimg.com.evil.io/a.jpg",
            "https://user:pass@pbs.twimg.com/media/a.jpg",
            "https://user@pbs.twimg.com/media/a.jpg",
            "https://pbs.twimg.com:8443/media/a.jpg",
            "https://127.0.0.1/a.jpg",
            "https://[::1]/a.jpg",
            " https://pbs.twimg.com/media/a.jpg",
            "https://pbs.twimg.com/media/a.jpg\n",
            "https://pbs.twimg.com/me\tdia/a.jpg",
            "https://pbs.twimg.com/media/a\u{a0}b.jpg",
            "https://pbs.twimg.com/media/\u{0}.jpg",
        ] {
            assert!(!ok(Platform::Twitter, bad), "{bad:?}");
        }
        let long = format!("https://pbs.twimg.com/media/{}", "a".repeat(MAX_URL_LEN));
        assert!(!ok(Platform::Twitter, &long));
        let exact = format!(
            "https://pbs.twimg.com/media/{}",
            "a".repeat(MAX_URL_LEN - "https://pbs.twimg.com/media/".len())
        );
        assert_eq!(utf16_len(&exact), MAX_URL_LEN);
        assert!(ok(Platform::Twitter, &exact));
        // Web and manual posts carry no captured URLs.
        assert!(!ok(Platform::Web, "https://pbs.twimg.com/media/a.jpg"));
    }

    #[test]
    fn utf16_lengths() {
        assert_eq!(utf16_len(""), 0);
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_len("é"), 1);
        assert_eq!(utf16_len("😀"), 2);
        let long = "😀".repeat(3_000);
        assert_eq!(utf16_len(&long), 6_000);
    }

    #[test]
    fn cdn_expiry() {
        let url =
            "https://scontent.cdninstagram.com/v/t51/x.jpg?stp=dst&_nc_ht=x&oe=65A1B2C3&_nc_sid=1";
        assert_eq!(cdn_url_expiry_ms(url), Some(0x65A1_B2C3 * 1_000));
        assert_eq!(
            cdn_url_expiry_ms("https://x.test/a.jpg?oe=1#oe=2"),
            Some(1_000)
        );
        assert_eq!(cdn_url_expiry_ms("https://pbs.twimg.com/media/x.jpg"), None);
        assert_eq!(cdn_url_expiry_ms("https://x/y?oe=zz"), None);
        assert_eq!(cdn_url_expiry_ms("https://x/y?oe="), None);
        assert_eq!(cdn_url_expiry_ms("https://x/y?oe=-1"), None);
        assert_eq!(cdn_url_expiry_ms("https://x/y?oe=+1"), None);
        assert_eq!(cdn_url_expiry_ms("https://x/y?oe=1234567890abc"), None);
        assert_eq!(cdn_url_expiry_ms("https://x/y?noe=1"), None);
    }

    #[test]
    fn video_files_by_extension() {
        let kind = |url: &str| video_file(&Url::parse(url).unwrap());
        assert_eq!(
            kind("https://v1.pinimg.com/videos/mc/720p/a/b/c/x.mp4"),
            Some(VideoFile::Mp4)
        );
        assert_eq!(
            kind("https://video.twimg.com/ext_tw_video/1/pu/vid/720x1280/x.MP4?tag=12"),
            Some(VideoFile::Mp4)
        );
        assert_eq!(
            kind("https://v1.pinimg.com/videos/mc/hls/a/b/c/x.m3u8"),
            Some(VideoFile::Other)
        );
        assert_eq!(kind("https://x.test/a.mpd"), Some(VideoFile::Other));
        assert_eq!(kind("https://x.test/a.webm"), Some(VideoFile::Other));
        assert_eq!(kind("https://pbs.twimg.com/media/a.jpg"), None);
        assert_eq!(kind("https://x.test/a.jpg?name=x.mp4"), None);
        assert_eq!(kind("https://x.test/mp4"), None);
    }
}
