//! yt-dlp's errors mapped to a failure code (plan D15, §2.13).
//!
//! yt-dlp reports a failure on stderr as `ERROR: [extractor] id: message`.
//! [`classify`] reads those lines and returns one [`YtdlpFailure`]. Before
//! matching, each line loses its `[extractor] id:` prefix and every URL, so a
//! user name or a shortcode inside them (`a-suspended`, `private_account`)
//! cannot pick the code. The rules are tried in this order, and the first one
//! that matches any error line wins:
//!
//! 1. **`rate_limited`**: HTTP 429, "Too Many Requests", "rate limit
//!    exceeded", and X refusing a guest token.
//! 2. **HTTP status**: 5xx is `transient`; 404 and 410 are `not_found`; 403
//!    is `rate_limited` (the platform refuses this address).
//! 3. **`transient`**: network failures, timeouts, proxy errors and "Unable
//!    to extract" (an extractor that no longer matches the page).
//! 4. **`login_required`**: an age gate, a private post or account, a login
//!    wall. These are the patterns of the desktop's removed stage B
//!    (`NEEDS_LOGIN_RE` in `electron/downloader.ts` at `v1.0.2-beta.4`), plus
//!    the hint yt-dlp appends to every login error ("Use --cookies …",
//!    "… to provide account credentials") and X's "requires authentication"
//!    and "not authorized to view".
//! 5. **`not_found`**: deleted, suspended or missing posts, and posts without
//!    a video.
//! 6. **`unsupported`**: an URL no allowed extractor takes, no usable format,
//!    DRM, a geo restriction, a live stream that has not started.
//! 7. Anything else is `transient`, and so is a failure without an `ERROR:`
//!    line (a crash, a kill): the job's tries bound how often it is retried.
//!
//! Rules 1–3 come before rule 4, so a network error, a 429 or an "Unable to
//! extract" never becomes `login_required`. The patterns follow the messages
//! of yt-dlp 2026.02 (local) and the image's pinned 2026.08.19. SPIKE-9 met
//! no 429, challenge or login wall in 360 runs from the VPS, so the refusal
//! rules (403, the guest token) rest on yt-dlp's wording, not on a measured
//! refusal.
//!
//! Nothing here keeps the text: the code is all a caller gets (plan §3.7).

use std::sync::LazyLock;

use regex::Regex;

/// Why yt-dlp could not fetch a post's video.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum YtdlpFailure {
    /// An age gate, a private post or account, or a login wall. Never
    /// retried, anonymously or with cookies: on the web the extension takes
    /// over (D15), in place of the desktop's removed stage B.
    LoginRequired,
    /// The post, or its video, is gone: deleted, suspended, or never a video.
    NotFound,
    /// The platform refuses this address for now: 429, 403, no guest token.
    RateLimited,
    /// No route through yt-dlp: an URL no allowed extractor takes, no usable
    /// format, DRM, a geo restriction.
    Unsupported,
    /// Network, timeouts, an extractor that no longer matches the page, or
    /// an unknown failure: another try may succeed.
    Transient,
}

impl YtdlpFailure {
    /// Every failure, in a stable order.
    pub const ALL: [Self; 5] = [
        Self::LoginRequired,
        Self::NotFound,
        Self::RateLimited,
        Self::Unsupported,
        Self::Transient,
    ];

    /// The stable `snake_case` code (plan §2.9: codes, not prose).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::LoginRequired => "login_required",
            Self::NotFound => "not_found",
            Self::RateLimited => "rate_limited",
            Self::Unsupported => "unsupported",
            Self::Transient => "transient",
        }
    }

    /// Whether a later try may succeed: `rate_limited` (after a longer wait)
    /// and `transient`.
    #[must_use]
    pub const fn is_transient(self) -> bool {
        matches!(self, Self::RateLimited | Self::Transient)
    }
}

impl std::fmt::Display for YtdlpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// The rules, in order (see the module docs).
static RULES: LazyLock<Vec<(Regex, YtdlpFailure)>> = LazyLock::new(|| {
    let rule = |pattern: &str, failure| {
        let regex = Regex::new(&format!("(?i){pattern}")).expect("valid classifier pattern");
        (regex, failure)
    };
    vec![
        rule(
            r"\bHTTP ?Error 429\b|\btoo many requests\b|\brate[- ]?limit(?:ed\b| exceeded)|could not retrieve guest token",
            YtdlpFailure::RateLimited,
        ),
        rule(r"\bHTTP ?Error 5\d\d\b", YtdlpFailure::Transient),
        rule(r"\bHTTP ?Error 4(?:04|10)\b", YtdlpFailure::NotFound),
        rule(r"\bHTTP ?Error 403\b", YtdlpFailure::RateLimited),
        rule(
            concat!(
                r"unable to extract|unable to download|unable to connect|unable to read",
                r"|connection (?:refused|reset|aborted|closed)|remote end closed|broken pipe",
                r"|timed? ?out|temporary failure|name or service not known",
                r"|nodename nor servname|no route to host|network is unreachable",
                r"|failed to resolve|getaddrinfo|\bssl|certificate|eof occurred|incomplete ?read",
                r"|transporterror|proxyerror|tunnel connection failed",
                r"|did not get any data blocks|giving up after|empty json response",
                // The curl backend (curl_cffi) of the image's build.
                r"|failed to perform|curl: \(\d+\)|could not resolve",
            ),
            YtdlpFailure::Transient,
        ),
        rule(
            concat!(
                // Stage B's NEEDS_LOGIN_RE (desktop v1.0.2-beta.4), verbatim.
                r"age[-\s]?restrict|must be 18|login required|log ?in to|sign in to",
                r"|rerun .*--cookies|requires? .*log\s?in|is private",
                r"|private (?:account|video|profile|user)|only available .*registered users",
                r"|restricted video",
                // yt-dlp's login hint, and X's own wording.
                r"|use --cookies|--cookies-from-browser|provide account credentials",
                r"|for the authentication|requires authentication|authentication required",
                r"|not authorized to (?:view|see)|limits who can view",
            ),
            YtdlpFailure::LoginRequired,
        ),
        rule(
            concat!(
                r"no status found|(?:post|tweet|status|video|media|page|content|account|user)",
                r" (?:was |has been )?not found|does ?n[o'’]t exist|no longer (?:exists|available)",
                r"|has been (?:deleted|removed)|was deleted|deleted by|\bsuspended\b|violated",
                r"|there is no video|no video could be found|is not a video",
                r"|(?:post|tweet|video|content|media)(?: #\d+)? (?:is |was )?unavailable",
                r"|isn['’]t available",
            ),
            YtdlpFailure::NotFound,
        ),
        rule(
            concat!(
                r"unsupported url|no suitable extractor|no video formats found",
                r"|requested format is not available|is not a valid url|\bdrm\b",
                r"|geo[- ]?restrict|not available (?:from|in) your (?:location|country)",
                r"|in your country|has not yet started|not started yet|replay is disabled",
            ),
            YtdlpFailure::Unsupported,
        ),
    ]
});

/// The `[extractor] id: ` that starts an error message.
static SOURCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*\[[^\]]*\]\s*(?:[^:\s]+:\s+)?").expect("valid source pattern")
});

/// An URL, up to the next space.
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b[a-z][a-z0-9+.-]*://\S*").expect("valid URL pattern"));

/// Maps yt-dlp's stderr lines to a failure (see the module docs). Only the
/// `ERROR:` lines count: warnings never pick the code.
#[must_use]
pub fn classify<'a>(stderr: impl IntoIterator<Item = &'a str>) -> YtdlpFailure {
    let messages: Vec<String> = stderr
        .into_iter()
        .filter_map(|line| line.trim_start().strip_prefix("ERROR:"))
        .map(|message| {
            let message = SOURCE.replace(message, "");
            URL.replace_all(&message, " ").into_owned()
        })
        .collect();
    RULES
        .iter()
        .find(|(regex, _)| messages.iter().any(|message| regex.is_match(message)))
        .map_or(YtdlpFailure::Transient, |&(_, failure)| failure)
}
