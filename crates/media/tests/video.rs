//! The video tools (P4-06): yt-dlp's argv, URL rules and error codes, the
//! contained runs (environment, niceness, slots, size cap, time limit,
//! cancellation), and ffmpeg's operations.
//!
//! The runs use the fake binaries in `tests/fixtures/bin/`: no test touches
//! the network. The real tools run only when asked for:
//!
//! - `SHELFY_TEST_FFMPEG=/path/to/ffmpeg`: remux, inspect, poster and
//!   keyframes on synthetic videos that ffmpeg itself generates;
//! - `SHELFY_TEST_YTDLP=/path/to/yt-dlp`: the real yt-dlp takes the whole
//!   argv and sends its first request to a local fake proxy, which refuses
//!   it. Nothing leaves the machine.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::num::NonZeroU32;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use proptest::prelude::*;
use shelfy_media::render::RenderSpec;
use shelfy_media::video::ffmpeg::MAX_KEYFRAMES;
use shelfy_media::video::{
    DownloadRequest, Egress, PostUrl, PostUrlError, Progress, ProxyUrl, ToolPaths, ToolStatus,
    VideoError, VideoPlatform, VideoTools, VideoToolsConfig, YtdlpFailure, index_first,
    sweep_scratch,
};
use shelfy_media::{Digest, MediaKind};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// The fake binaries, copied into a private directory per test, with the
/// scratch directory next to them.
struct Fakes {
    dir: TempDir,
}

impl Fakes {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bin");
        for name in ["yt-dlp", "ffmpeg"] {
            let to = bin.join(name);
            fs::copy(fixtures.join(name), &to).unwrap();
            fs::set_permissions(&to, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { dir }
    }

    fn bin(&self, name: &str) -> PathBuf {
        self.dir.path().join("bin").join(name)
    }

    fn scratch(&self) -> PathBuf {
        self.dir.path().join("scratch")
    }

    /// A file the fakes wrote under their `state/` directory.
    fn state(&self, name: &str) -> PathBuf {
        self.dir.path().join("bin").join("state").join(name)
    }

    /// Files in `state/` whose names start with `prefix`.
    fn state_files(&self, prefix: &str) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(self.dir.path().join("bin").join("state")) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
            .map(|entry| entry.path())
            .collect();
        files.sort();
        files
    }

    fn config(&self) -> VideoToolsConfig {
        let mut config = VideoToolsConfig::new(
            ToolPaths {
                ytdlp: self.bin("yt-dlp"),
                ffmpeg: self.bin("ffmpeg"),
            },
            self.scratch(),
            Some(proxy()),
        );
        config.kill_grace = Duration::from_millis(400);
        config
    }

    fn tools(&self) -> VideoTools {
        VideoTools::new(self.config())
    }

    /// What is left in the scratch directory.
    fn leftovers(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.scratch()) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// The most tool runs the fakes saw alive at once.
    fn max_concurrency(&self) -> usize {
        fs::read_to_string(self.state("concurrency.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .max()
            .unwrap_or(0)
    }
}

fn proxy() -> Egress {
    Egress::Proxy(ProxyUrl::parse("http://127.0.0.1:9").unwrap())
}

fn post(platform: VideoPlatform, url: &str) -> PostUrl {
    PostUrl::parse(platform, url).expect("a valid post URL")
}

/// A request the fake yt-dlp answers with `scenario`.
fn scenario(name: &str) -> DownloadRequest {
    DownloadRequest::new(post(
        VideoPlatform::Twitter,
        &format!("https://x.com/i/status/{name}"),
    ))
}

async fn download(
    tools: &VideoTools,
    request: &DownloadRequest,
) -> Result<Vec<Progress>, VideoError> {
    let mut seen = Vec::new();
    let file = tools
        .download(request, &CancellationToken::new(), &mut |progress| {
            seen.push(progress)
        })
        .await?;
    drop(file);
    Ok(seen)
}

/// An input the fake ffmpeg answers with `marker` (SLOW, HANG, FAIL, OKAY).
fn fake_input(dir: &Path, marker: &str) -> PathBuf {
    let path = dir.join(format!("input-{marker}.mp4"));
    let mut bytes = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2".to_vec();
    bytes.extend_from_slice(marker.as_bytes());
    bytes.extend_from_slice(&[0u8; 1000]);
    fs::write(&path, bytes).unwrap();
    path
}

fn pid_alive(pid: &str) -> bool {
    // A zombie waiting for its reaper is as good as gone.
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", pid.trim()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&output.stdout);
    output.status.success() && !stat.trim().is_empty() && !stat.trim().starts_with('Z')
}

/// Waits up to 5 s for `pid` to be gone.
fn wait_gone(pid: &str) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

// ─── Post URLs and the argv ─────────────────────────────────────────────────

#[test]
fn post_urls_must_be_https_on_the_platforms_post_hosts() {
    use VideoPlatform::{Instagram, Pinterest, Twitter};
    let ok = [
        (Instagram, "https://www.instagram.com/reel/C1a2B3c4D5e/"),
        (
            Instagram,
            "https://instagram.com/p/C1a2B3c4D5e/?img_index=2",
        ),
        (Instagram, "https://m.instagram.com/tv/C1a2B3c4D5e"),
        (Twitter, "https://x.com/someone/status/1790000000000000001"),
        (Twitter, "https://mobile.twitter.com/someone/status/1"),
        (Twitter, "https://X.COM/someone/status/1"),
        (Twitter, "https://x.com:443/someone/status/1"),
        (Pinterest, "https://www.pinterest.com/pin/123456789/"),
        (Pinterest, "  https://pinterest.com/pin/1/  "),
    ];
    for (platform, url) in ok {
        assert!(PostUrl::parse(platform, url).is_ok(), "{url} was refused");
    }
    let refused = [
        (
            Twitter,
            "http://x.com/someone/status/1",
            PostUrlError::NotHttps,
        ),
        (Twitter, "file:///etc/passwd", PostUrlError::NotHttps),
        (Twitter, "javascript:alert(1)", PostUrlError::NotHttps),
        (Twitter, "ftp://x.com/a", PostUrlError::NotHttps),
        (Twitter, "-o/tmp/x", PostUrlError::Invalid),
        (Twitter, "--exec=touch /tmp/pwned", PostUrlError::Invalid),
        (Twitter, "x.com/someone/status/1", PostUrlError::Invalid),
        (
            Twitter,
            "https://user:pw@x.com/someone/status/1",
            PostUrlError::Credentials,
        ),
        (
            Twitter,
            "https://user@x.com/someone/status/1",
            PostUrlError::Credentials,
        ),
        (
            Twitter,
            "https://x.com:8443/someone/status/1",
            PostUrlError::Port,
        ),
        (
            Twitter,
            "https://x.com./someone/status/1",
            PostUrlError::Host,
        ),
        (
            Twitter,
            "https://evil.x.com/someone/status/1",
            PostUrlError::Host,
        ),
        (
            Twitter,
            "https://x.com.evil.test/status/1",
            PostUrlError::Host,
        ),
        (
            Twitter,
            "https://127.0.0.1/someone/status/1",
            PostUrlError::Host,
        ),
        (
            Twitter,
            "https://[::1]/someone/status/1",
            PostUrlError::Host,
        ),
        (
            Twitter,
            "https://www.instagram.com/reel/C1a2B3c4D5e/",
            PostUrlError::Host,
        ),
        (
            Instagram,
            "https://x.com/someone/status/1",
            PostUrlError::Host,
        ),
        (
            Pinterest,
            "https://it.pinterest.com/pin/1/",
            PostUrlError::Host,
        ),
        (Pinterest, "https://pin.it/abc", PostUrlError::Host),
    ];
    for (platform, url, error) in refused {
        assert_eq!(PostUrl::parse(platform, url), Err(error), "{url}");
    }
    let long = format!(
        "https://x.com/a/status/1?q={}",
        "a".repeat(PostUrl::MAX_LEN)
    );
    assert_eq!(PostUrl::parse(Twitter, &long), Err(PostUrlError::TooLong));
}

#[test]
fn post_urls_are_normalized_and_never_logged() {
    let url = post(VideoPlatform::Twitter, "https://X.com//status/1790#frag");
    // The desktop's fix for records saved without an author, and no fragment.
    assert_eq!(url.as_str(), "https://x.com/i/status/1790");
    let debug = format!("{url:?}");
    assert!(!debug.contains("x.com"), "{debug}");
    assert!(debug.contains("Twitter"), "{debug}");
    // The fix is X's alone.
    let pin = post(VideoPlatform::Pinterest, "https://pinterest.com//status/1");
    assert_eq!(pin.as_str(), "https://pinterest.com//status/1");
}

#[test]
fn platforms_parse_from_their_stored_names() {
    for platform in VideoPlatform::ALL {
        assert_eq!(
            platform.as_str().parse::<VideoPlatform>().unwrap(),
            platform
        );
    }
    assert!("web".parse::<VideoPlatform>().is_err());
    assert!("Twitter".parse::<VideoPlatform>().is_err());
}

#[test]
fn the_argv_is_the_desktops_anonymous_one_plus_the_plans() {
    let request = DownloadRequest::new(post(
        VideoPlatform::Twitter,
        "https://x.com/someone/status/1790000000000000001",
    ));
    let argv = request.argv(
        Path::new("/usr/bin/ffmpeg"),
        &Egress::Proxy(ProxyUrl::parse("http://shelfy-egress:4750").unwrap()),
        "/scratch/.ytdlp-x/video.%(ext)s".as_ref(),
    );
    let expected = [
        "--ignore-config",
        "--no-cookies",
        "--no-cookies-from-browser",
        "--no-cache-dir",
        "--no-plugin-dirs",
        "--no-update",
        "--use-extractors",
        "twitter",
        "--newline",
        "--progress-template",
        "download:shelfy-progress %(progress.status)s %(progress.downloaded_bytes)s \
         %(progress.total_bytes)s %(progress.total_bytes_estimate)s",
        "--sleep-requests",
        "1",
        // SPIKE-9's format (L17): https, H.264, at most 1080p.
        "-S",
        "proto:https,vcodec:h264,res:1080",
        "-f",
        "bv*+ba/b",
        "--merge-output-format",
        "mp4",
        "--ffmpeg-location",
        "/usr/bin/ffmpeg",
        "--proxy",
        "http://shelfy-egress:4750",
        "--no-playlist",
        "-o",
        "/scratch/.ytdlp-x/video.%(ext)s",
        "--",
        "https://x.com/someone/status/1790000000000000001",
    ];
    assert_eq!(argv, expected.map(OsString::from));

    // The third video of a post is playlist item 3; `direct` egress is
    // yt-dlp's empty proxy.
    let slide = DownloadRequest::video_slide(
        post(
            VideoPlatform::Instagram,
            "https://www.instagram.com/p/C1a2B3c4D5e/",
        ),
        NonZeroU32::new(3).unwrap(),
    );
    assert_eq!(slide.playlist_item(), NonZeroU32::new(3));
    let argv = slide.argv(
        Path::new("/usr/bin/ffmpeg"),
        &Egress::Direct,
        "out".as_ref(),
    );
    let text: Vec<&str> = argv.iter().map(|arg| arg.to_str().unwrap()).collect();
    let at = |flag: &str| text.iter().position(|arg| *arg == flag).unwrap();
    assert_eq!(text[at("--use-extractors") + 1], "Instagram");
    assert_eq!(text[at("--proxy") + 1], "");
    assert_eq!(
        &text[at("--yes-playlist")..at("--yes-playlist") + 3],
        ["--yes-playlist", "--playlist-items", "3"]
    );
    assert!(!text.contains(&"--no-playlist"));
    let pin = DownloadRequest::new(post(
        VideoPlatform::Pinterest,
        "https://pinterest.com/pin/1/",
    ));
    let argv = pin.argv(Path::new("/f"), &Egress::Direct, "out".as_ref());
    assert!(argv.contains(&OsString::from("Pinterest")));
}

/// Options that would hand yt-dlp an account credential, read a file of
/// options, or run a command.
const FORBIDDEN: [&str; 12] = [
    "--cookies",
    "--cookies-from-browser",
    "--username",
    "-u",
    "--password",
    "-p",
    "--netrc",
    "--netrc-cmd",
    "--add-headers",
    "--config-locations",
    "--batch-file",
    "--exec",
];

fn platform_and_url() -> impl Strategy<Value = (VideoPlatform, String)> {
    let platform = prop::sample::select(VideoPlatform::ALL.to_vec());
    (platform, "[a-zA-Z0-9_-]{1,20}", "[a-zA-Z0-9_=&%-]{0,30}").prop_flat_map(
        |(platform, path, query)| {
            let hosts = platform.post_hosts().to_vec();
            prop::sample::select(hosts).prop_map(move |host| {
                (
                    platform,
                    format!("https://{host}/--cookies/{path}?{query}--cookies=x"),
                )
            })
        },
    )
}

proptest! {
    /// "No argv ever contains `--cookies`": for every platform, host, slide
    /// and egress, and URLs that try to look like options.
    #[test]
    fn no_argv_ever_carries_cookies_or_credentials(
        (platform, raw) in platform_and_url(),
        slide in prop::option::of(1u32..20),
        direct in any::<bool>(),
    ) {
        let url = PostUrl::parse(platform, &raw).unwrap();
        let request = match slide.and_then(NonZeroU32::new) {
            Some(ordinal) => DownloadRequest::video_slide(url.clone(), ordinal),
            None => DownloadRequest::new(url.clone()),
        };
        let egress = if direct { Egress::Direct } else { proxy() };
        let argv = request.argv(Path::new("/usr/bin/ffmpeg"), &egress, "/s/video.%(ext)s".as_ref());
        let text: Vec<&str> = argv.iter().map(|arg| arg.to_str().unwrap()).collect();
        // The URL is the last argument, right after the only `--`.
        prop_assert_eq!(text.iter().filter(|arg| **arg == "--").count(), 1);
        prop_assert_eq!(text[text.len() - 2], "--");
        prop_assert_eq!(text[text.len() - 1], url.as_str());
        for arg in &text[..text.len() - 1] {
            for flag in FORBIDDEN {
                prop_assert!(*arg != flag && !arg.starts_with(&format!("{flag}=")), "{arg}");
            }
            prop_assert!(!arg.starts_with("--cookies"), "{arg}");
        }
        for flag in ["--ignore-config", "--no-cookies", "--no-cookies-from-browser",
                     "--no-cache-dir", "--no-plugin-dirs"] {
            prop_assert!(text.contains(&flag), "{flag} is missing");
        }
        prop_assert!(text.contains(&"--proxy"));
    }
}

// ─── The classifier ─────────────────────────────────────────────────────────

/// The login hint yt-dlp appends to every login error.
const HINT: &str = "Use --cookies, --cookies-from-browser, --username and --password, \
    --netrc-cmd, or --netrc (twitter) to provide account credentials. See  \
    https://github.com/yt-dlp/yt-dlp/wiki/FAQ#how-do-i-pass-cookies-to-yt-dlp  for how to \
    manually pass cookies";

fn classify_one(line: &str) -> YtdlpFailure {
    shelfy_media::video::classify([line])
}

#[test]
fn login_walls_age_gates_and_private_posts_are_login_required() {
    let lines = [
        // Instagram's logged-out answer from a datacenter.
        format!("ERROR: [Instagram] C1a2B3c4D5e: Requested content is not available, rate-limit reached or login required. {HINT}"),
        // Instagram's age gate.
        format!("ERROR: [Instagram] C1a2B3c4D5e: Restricted Video: You must be 18 years old or over to see this video. {HINT}"),
        // A private Instagram account.
        "ERROR: [Instagram] C1a2B3c4D5e: This content is only available for registered users who follow this account. Use --cookies-from-browser or --cookies for the authentication. See  https://github.com/yt-dlp/yt-dlp/wiki/FAQ  for how to manually pass cookies".into(),
        format!("ERROR: [Instagram] C1a2B3c4D5e: Instagram sent an empty media response. Check if this post is accessible in your browser without being logged-in. If it is not, then u{}", &HINT[1..]),
        // X's age gate, protected posts and API refusals.
        format!("ERROR: [twitter] 1790000000000000001: NSFW tweet requires authentication. {HINT}"),
        format!("ERROR: [twitter] 1790000000000000001: You are not authorized to view this protected tweet. {HINT}"),
        format!("ERROR: [twitter] 1790000000000000001: Sorry, you are not authorized to see this status. {HINT}"),
        "ERROR: [twitter] 1790000000000000001: Twitter API says: Age-restricted adult content. This content might not be appropriate for people under 18 years old. To view this media, you’ll need to log in to X".into(),
        "ERROR: [twitter] 1790000000000000001: Twitter API says: You’re unable to view this Post because this account owner limits who can view their Posts".into(),
        // Stage B's wording on other extractors.
        "ERROR: [Pinterest] 123: This video is private".into(),
        "ERROR: [youtube] abc: Sign in to confirm your age. This video may be inappropriate for some users.".into(),
        "ERROR: [vimeo] 1: This video is age-restricted".into(),
    ];
    for line in &lines {
        assert_eq!(classify_one(line), YtdlpFailure::LoginRequired, "{line}");
    }
}

#[test]
fn network_errors_429_and_unable_to_extract_never_become_login_required() {
    let transient = [
        // Real messages of yt-dlp 2026.02 against local failures.
        "ERROR: [generic] Unable to download webpage: HTTPConnection(host='127.0.0.1', port=28549): Failed to establish a new connection: [Errno 61] Connection refused (caused by TransportError(\"HTTPConnection(host='127.0.0.1', port=28549): Failed to establish a new connection: [Errno 61] Connection refused\"))",
        "ERROR: [twitter] 1234567890123456789: Unable to download JSON metadata: ('Unable to connect to proxy', OSError('Tunnel connection failed: 403 Forbidden')) (caused by ProxyError(\"('Unable to connect to proxy', OSError('Tunnel connection failed: 403 Forbidden'))\")); please report this issue on  https://github.com/yt-dlp/yt-dlp/issues?q= , filling out the appropriate issue template. Confirm you are on the latest version using  yt-dlp -U",
        // The image's yt-dlp 2026.08.19 (curl backend) behind a refusing proxy.
        "ERROR: [Instagram] C1a2B3c4D5e: Unable to download webpage: Failed to perform, curl: (7) CONNECT tunnel failed, response 403. See https://curl.se/libcurl/c/libcurl-errors.html first for more details. (caused by ProxyError('Failed to perform, curl: (7) CONNECT tunnel failed, response 403. See https://curl.se/libcurl/c/libcurl-errors.html first for more details.')); please report this issue on  https://github.com/yt-dlp/yt-dlp/issues?q= , filling out the appropriate issue template. Confirm you are on the latest version using  yt-dlp -U",
        "ERROR: [Instagram] C1a2B3c4D5e: Unable to download webpage: Failed to perform, curl: (6) Could not resolve host: www.instagram.com",
        "ERROR: [generic] Unable to download webpage: HTTP Error 500: Internal Server Error (caused by <HTTPError 500: Internal Server Error>)",
        "ERROR: [Instagram] C1a2B3c4D5e: Unable to extract shared data; please report this issue on  https://github.com/yt-dlp/yt-dlp/issues?q= , filling out the appropriate issue template. Confirm you are on the latest version using  yt-dlp -U",
        "ERROR: [twitter] 1: Unable to download webpage: The read operation timed out (caused by TransportError('The read operation timed out'))",
        "ERROR: [Pinterest] 1: Unable to download webpage: [Errno -3] Temporary failure in name resolution (caused by TransportError('[Errno -3] Temporary failure in name resolution'))",
        "ERROR: [twitter] 1: Unable to download JSON metadata: HTTP Error 503: Service Unavailable",
        "ERROR: unable to download video data: <urlopen error [Errno 104] Connection reset by peer>",
        "ERROR: Did not get any data blocks",
        // Transient rules come first, even next to login words.
        "ERROR: [Instagram] C1a2B3c4D5e: Unable to extract video url (login required?); please report this issue",
        "ERROR: [twitter] 1: Unable to download JSON metadata: The read operation timed out. Use --cookies, --cookies-from-browser",
    ];
    for line in transient {
        assert_eq!(classify_one(line), YtdlpFailure::Transient, "{line}");
    }
    let rate_limited = [
        "ERROR: [generic] Unable to download webpage: HTTP Error 429: Too Many Requests (caused by <HTTPError 429: Too Many Requests>)",
        "ERROR: [twitter] 1: Error(s) while querying API: Rate limit exceeded",
        "ERROR: [twitter] 1: Could not retrieve guest token",
        "ERROR: [generic] Unable to download webpage: HTTP Error 403: Forbidden (caused by <HTTPError 403: Forbidden>)",
        "ERROR: unable to download video data: HTTP Error 403: Forbidden",
        // 429 with a login hint is still a rate limit.
        "ERROR: [Instagram] C1a2B3c4D5e: HTTP Error 429: Too Many Requests. Use --cookies to log in",
    ];
    for line in rate_limited {
        assert_eq!(classify_one(line), YtdlpFailure::RateLimited, "{line}");
    }
}

#[test]
fn missing_posts_and_unsupported_urls_have_their_own_codes() {
    let not_found = [
        "ERROR: [generic] Unable to download webpage: HTTP Error 404: Not Found (caused by <HTTPError 404: Not Found>)",
        "ERROR: [twitter] 1: Unable to download JSON metadata: HTTP Error 410: Gone",
        "ERROR: [twitter] 1: Error(s) while querying API: _Missing: No status found with that ID.",
        "ERROR: [twitter] 1: Twitter API says: This Post was deleted by the Post author",
        "ERROR: [twitter] 1: Twitter API says: This Post is from a suspended account",
        "ERROR: [twitter] 1: Twitter API says: This Post violated the X Rules",
        "ERROR: [twitter] 1: Suspended",
        "ERROR: [twitter] 1: Requested tweet is unavailable",
        "ERROR: [twitter] 1: Video #2 is unavailable",
        "ERROR: [twitter] 1: Media #2 is not a video",
        "ERROR: [twitter] 1: No video could be found in this tweet",
        "ERROR: [Instagram] C1a2B3c4D5e: There is no video in this post",
        "ERROR: [Pinterest] 1: Sorry, this content isn’t available right now",
    ];
    for line in not_found {
        assert_eq!(classify_one(line), YtdlpFailure::NotFound, "{line}");
    }
    let unsupported = [
        "ERROR: No suitable extractor found for URL https://unsupported.invalid/i/status/1",
        "ERROR: Unsupported URL: https://x.com/home",
        // A user name in the URL cannot pick the code.
        "ERROR: Unsupported URL: https://x.com/suspended_or_deleted/likes",
        "ERROR: [Pinterest] 1: No video formats found!; please report this issue on  https://github.com/yt-dlp/yt-dlp/issues?q= , filling out the appropriate issue template.",
        "ERROR: [twitter] 1: Requested format is not available. Use --list-formats for a list of available formats",
        "ERROR: [twitter] 1: This video is not available from your location due to geo restriction",
        "ERROR: [twitter:broadcast] 1: This live broadcast has not yet started",
        "ERROR: [x] 1: This video is DRM protected",
    ];
    for line in unsupported {
        assert_eq!(classify_one(line), YtdlpFailure::Unsupported, "{line}");
    }
}

#[test]
fn only_error_lines_pick_the_code() {
    use shelfy_media::video::classify;
    // A crash or a kill: no ERROR line, whatever the warnings say.
    let warnings = [
        "WARNING: [Instagram] Main webpage is locked behind the login page. Retrying with embed webpage (some metadata might be missing).",
        "WARNING: login required for comments",
        "Traceback (most recent call last):",
        "KeyError: 'video'",
    ];
    assert_eq!(classify(warnings), YtdlpFailure::Transient);
    assert_eq!(classify(Vec::<&str>::new()), YtdlpFailure::Transient);
    // Across several error lines, the rules' order decides.
    let lines = [
        "WARNING: [twitter] something",
        "ERROR: [twitter] 1: NSFW tweet requires authentication. Use --cookies",
        "ERROR: [twitter] 2: Unable to download JSON metadata: HTTP Error 429: Too Many Requests",
    ];
    assert_eq!(classify(lines), YtdlpFailure::RateLimited);
    // An unknown error is transient: the job's tries bound it.
    assert_eq!(
        classify(["ERROR: Postprocessing: Conversion failed!"]),
        YtdlpFailure::Transient
    );
    // A shortcode that spells a word does not pick the code.
    assert_eq!(
        classify(["ERROR: [Instagram] a-suspended: Unable to extract data"]),
        YtdlpFailure::Transient
    );
    for failure in YtdlpFailure::ALL {
        assert_eq!(
            failure.is_transient(),
            matches!(failure, YtdlpFailure::RateLimited | YtdlpFailure::Transient)
        );
    }
}

// ─── yt-dlp runs (fake binary) ──────────────────────────────────────────────

#[tokio::test]
async fn a_download_runs_its_argv_in_a_clean_low_priority_process() {
    let fakes = Fakes::new();
    let tools = fakes.tools();
    let request = scenario("ok");
    let mut seen = Vec::new();
    let file = tools
        .download(&request, &CancellationToken::new(), &mut |p| seen.push(p))
        .await
        .expect("the download");
    assert_eq!(file.kind(), MediaKind::Mp4);
    assert_eq!(file.bytes(), 4120);
    assert_eq!(file.path().file_name().unwrap(), "video.mp4");
    let run_dir = file.path().parent().unwrap().to_owned();
    assert!(run_dir.starts_with(fakes.scratch()));
    assert!(
        run_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".ytdlp-"),
        "{}",
        run_dir.display()
    );

    // Exactly the argv of `DownloadRequest::argv`.
    let argv: Vec<String> = fs::read_to_string(run_dir.join("argv.txt"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let mut template = run_dir.clone().into_os_string();
    template.push("/video.%(ext)s");
    let expected: Vec<String> = request
        .argv(&fakes.bin("ffmpeg"), &proxy(), &template)
        .iter()
        .map(|arg| arg.to_str().unwrap().to_owned())
        .collect();
    assert_eq!(argv, expected);

    // Nothing of this process's environment, only the allowlist.
    let env = fs::read_to_string(run_dir.join("env.txt")).unwrap();
    let names: Vec<&str> = env
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .collect();
    let allowed = [
        "PATH",
        "LANG",
        "PYTHONUTF8",
        "PYTHONDONTWRITEBYTECODE",
        "PYTHONNOUSERSITE",
        "HOME",
        "TMPDIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        // Set by the shell running the fake itself.
        "PWD",
        "SHLVL",
        "_",
        "OLDPWD",
    ];
    for name in &names {
        assert!(allowed.contains(name), "{name} leaked into the child");
    }
    let home = format!("HOME={}", run_dir.display());
    assert!(env.lines().any(|line| line == home), "{env}");
    assert!(
        env.lines()
            .any(|line| line == "HTTPS_PROXY=http://127.0.0.1:9")
    );
    let cwd = fs::read_to_string(run_dir.join("cwd.txt")).unwrap();
    assert_eq!(
        fs::canonicalize(cwd.trim()).unwrap(),
        fs::canonicalize(&run_dir).unwrap()
    );
    // §2.3: nice -n 10.
    let nice: i32 = fs::read_to_string(run_dir.join("nice.txt"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(nice >= 10, "the child ran at nice {nice}");

    // Progress, from the template's lines.
    assert_eq!(
        seen.last(),
        Some(&Progress {
            downloaded_bytes: 4120,
            total_bytes: Some(4120)
        })
    );

    // Dropping the result removes its directory.
    drop(file);
    assert!(!run_dir.exists());
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

#[tokio::test]
async fn progress_adds_up_the_video_and_audio_parts() {
    let fakes = Fakes::new();
    let seen = download(&fakes.tools(), &scenario("merged")).await.unwrap();
    assert_eq!(
        seen.last(),
        Some(&Progress {
            downloaded_bytes: 1100,
            total_bytes: Some(1100)
        })
    );
    // Bytes never go back.
    assert!(
        seen.windows(2)
            .all(|w| w[0].downloaded_bytes <= w[1].downloaded_bytes)
    );
    assert!(
        seen.iter()
            .all(|p| p.fraction().is_none_or(|f| (0.0..=1.0).contains(&f)))
    );
}

#[tokio::test]
async fn webm_output_is_accepted_as_a_video() {
    let fakes = Fakes::new();
    let file = fakes
        .tools()
        .download(&scenario("webm"), &CancellationToken::new(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(file.kind(), MediaKind::Webm);
}

#[tokio::test]
async fn yt_dlp_failures_carry_their_code_and_leave_nothing() {
    let fakes = Fakes::new();
    let tools = fakes.tools();
    let err = download(&tools, &scenario("login")).await.unwrap_err();
    assert!(
        matches!(err, VideoError::Ytdlp(YtdlpFailure::LoginRequired)),
        "{err:?}"
    );
    assert_eq!(err.code(), "login_required");
    assert!(!err.is_transient());
    let err = download(&tools, &scenario("ratelimited"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, VideoError::Ytdlp(YtdlpFailure::RateLimited)),
        "{err:?}"
    );
    assert!(err.is_transient());
    let err = download(&tools, &scenario("unknown")).await.unwrap_err();
    assert!(
        matches!(err, VideoError::Ytdlp(YtdlpFailure::Unsupported)),
        "{err:?}"
    );
    // No error shows the URL.
    assert!(!format!("{err} {err:?}").contains("x.com"));
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

#[tokio::test]
async fn outputs_that_are_not_exactly_one_video_are_refused() {
    let fakes = Fakes::new();
    let tools = fakes.tools();
    for name in ["noout", "notvideo", "two", "symlink"] {
        let err = download(&tools, &scenario(name)).await.unwrap_err();
        assert!(matches!(err, VideoError::InvalidOutput), "{name}: {err:?}");
    }
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
    // The symlink's target is untouched.
    assert!(fakes.state("elsewhere.mp4").exists());
}

#[tokio::test]
async fn a_download_past_the_cap_is_stopped_and_removed() {
    let fakes = Fakes::new();
    let mut config = fakes.config();
    config.max_video_bytes = 2 * 1024 * 1024;
    let tools = VideoTools::new(config);
    let started = Instant::now();
    let err = download(&tools, &scenario("big")).await.unwrap_err();
    assert!(
        matches!(err, VideoError::TooLarge { limit } if limit == 2 * 1024 * 1024),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
    let pid = fs::read_to_string(fakes.state("ytdlp.pid")).unwrap();
    assert!(wait_gone(&pid));
}

#[tokio::test]
async fn downloaded_bytes_past_the_cap_stop_the_run() {
    let fakes = Fakes::new();
    let mut config = fakes.config();
    config.max_video_bytes = 2 * 1024 * 1024;
    let mut seen = Vec::new();
    let err = VideoTools::new(config)
        .download(&scenario("grow"), &CancellationToken::new(), &mut |p| {
            seen.push(p);
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VideoError::TooLarge { .. }), "{err:?}");
    // Stopped by the progress lines, before the directory held anything.
    assert!(
        seen.iter()
            .all(|p| p.downloaded_bytes <= 2 * 1024 * 1024 && p.total_bytes.is_none())
    );
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

#[tokio::test]
async fn an_announced_total_past_the_cap_stops_the_run_at_once() {
    let fakes = Fakes::new();
    let started = Instant::now();
    let err = download(&fakes.tools(), &scenario("announce"))
        .await
        .unwrap_err();
    assert!(matches!(err, VideoError::TooLarge { .. }), "{err:?}");
    // The fake would sleep 30 s.
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

#[tokio::test]
async fn cancel_sends_sigterm_and_removes_partial_files() {
    let fakes = Fakes::new();
    // A long grace: if SIGTERM did not end the run, the test would wait it out.
    let mut config = fakes.config();
    config.kill_grace = Duration::from_secs(10);
    let tools = VideoTools::new(config);
    let cancel = CancellationToken::new();
    let request = scenario("slow");
    let started = Arc::new(Mutex::new(None));
    let seen = started.clone();
    let canceller = cancel.clone();
    let mut on_progress = move |_: Progress| {
        // The partial file exists by now; cancel.
        seen.lock().unwrap().get_or_insert_with(Instant::now);
        canceller.cancel();
    };
    let err = tools
        .download(&request, &cancel, &mut on_progress)
        .await
        .unwrap_err();
    assert!(matches!(err, VideoError::Cancelled), "{err:?}");
    let cancelled_at = started.lock().unwrap().expect("progress was reported");
    // SIGTERM ended it, well before the grace.
    assert!(cancelled_at.elapsed() < Duration::from_secs(5));
    assert_eq!(fakes.state_files("term.").len(), 1, "SIGTERM came first");
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

#[tokio::test]
async fn a_child_that_ignores_sigterm_is_killed_after_the_grace() {
    let fakes = Fakes::new();
    let tools = fakes.tools();
    let cancel = CancellationToken::new();
    let canceller = cancel.clone();
    let mut cancelled_at = None;
    let mut on_progress = |_: Progress| {
        cancelled_at.get_or_insert_with(Instant::now);
        canceller.cancel();
    };
    let err = tools
        .download(&scenario("stubborn"), &cancel, &mut on_progress)
        .await
        .unwrap_err();
    assert!(matches!(err, VideoError::Cancelled), "{err:?}");
    let waited = cancelled_at.unwrap().elapsed();
    assert!(
        waited >= Duration::from_millis(400),
        "SIGKILL came after {waited:?}"
    );
    // The whole group went, the grandchild too.
    let pid = fs::read_to_string(fakes.state("ytdlp.pid")).unwrap();
    let grandchild = fs::read_to_string(fakes.state("grandchild.pid")).unwrap();
    assert!(wait_gone(&pid), "yt-dlp is still running");
    assert!(wait_gone(&grandchild), "its child is still running");
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

#[tokio::test]
async fn dropping_a_download_kills_its_process_group() {
    let fakes = Fakes::new();
    let tools = fakes.tools();
    let (started, mut is_started) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        let mut on_progress = |_: Progress| {
            started.send_replace(true);
        };
        tools
            .download(
                &scenario("stubborn"),
                &CancellationToken::new(),
                &mut on_progress,
            )
            .await
            .map(|_| ())
    });
    is_started.wait_for(|started| *started).await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let pid = fs::read_to_string(fakes.state("ytdlp.pid")).unwrap();
    let grandchild = fs::read_to_string(fakes.state("grandchild.pid")).unwrap();
    assert!(wait_gone(&pid), "yt-dlp is still running");
    assert!(wait_gone(&grandchild), "its child is still running");
}

#[tokio::test]
async fn a_run_past_its_time_limit_is_stopped() {
    let fakes = Fakes::new();
    let mut config = fakes.config();
    config.download_timeout = Duration::from_millis(300);
    let err = download(&VideoTools::new(config), &scenario("hang"))
        .await
        .unwrap_err();
    assert!(matches!(err, VideoError::TimedOut), "{err:?}");
    assert!(err.is_transient());
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

#[tokio::test]
async fn without_egress_yt_dlp_does_not_run() {
    let fakes = Fakes::new();
    let mut config = fakes.config();
    config.egress = None;
    let err = download(&VideoTools::new(config), &scenario("ok"))
        .await
        .unwrap_err();
    assert!(matches!(err, VideoError::NoEgress), "{err:?}");
    assert!(!fakes.state("ytdlp.pid").exists());
}

#[tokio::test]
async fn a_missing_binary_is_unavailable() {
    let fakes = Fakes::new();
    let mut config = fakes.config();
    config.ytdlp = fakes.dir.path().join("no-such-yt-dlp");
    let err = download(&VideoTools::new(config), &scenario("ok"))
        .await
        .unwrap_err();
    assert!(matches!(err, VideoError::Unavailable { .. }), "{err:?}");
    assert_eq!(err.code(), "tool_unavailable");
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

// ─── Slots and the probe ────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn yt_dlp_and_ffmpeg_share_two_slots() {
    let fakes = Fakes::new();
    let tools = fakes.tools();
    let inputs = tempfile::tempdir().unwrap();
    let slow = fake_input(inputs.path(), "SLOW");
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let downloader = tools.clone();
        tasks.push(tokio::spawn(async move {
            download(&downloader, &scenario("slowok")).await.map(|_| ())
        }));
        let remuxer = tools.clone();
        let input = slow.clone();
        tasks.push(tokio::spawn(async move {
            remuxer
                .remux(&input, &CancellationToken::new())
                .await
                .map(|_| ())
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(
        fakes.max_concurrency(),
        2,
        "two tool processes at most, and both slots used"
    );
}

#[tokio::test]
async fn cancel_while_waiting_for_a_slot_returns_at_once() {
    let fakes = Fakes::new();
    let mut config = fakes.config();
    config.max_processes = 1;
    let tools = VideoTools::new(config);
    let inputs = tempfile::tempdir().unwrap();
    let hang = fake_input(inputs.path(), "HANG");
    let holder_cancel = CancellationToken::new();
    let holder = {
        let tools = tools.clone();
        let cancel = holder_cancel.clone();
        tokio::spawn(async move { tools.remux(&hang, &cancel).await.map(|_| ()) })
    };
    // Wait until the holder's ffmpeg runs.
    let until = Instant::now() + Duration::from_secs(5);
    while fakes.state_files("run.").is_empty() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        fakes.state_files("run.").len(),
        1,
        "the holder's ffmpeg runs"
    );
    // A second call waits for the only slot; cancelling it ends the wait.
    let cancel = CancellationToken::new();
    let waiter = {
        let tools = tools.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tools
                .download(&scenario("ok"), &cancel, &mut |_| {})
                .await
                .map(|_| ())
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiter.is_finished(), "it waits for the slot");
    let started = Instant::now();
    cancel.cancel();
    let err = waiter.await.unwrap().unwrap_err();
    assert!(matches!(err, VideoError::Cancelled), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!fakes.state("ytdlp.pid").exists(), "yt-dlp never started");
    holder_cancel.cancel();
    let err = holder.await.unwrap().unwrap_err();
    assert!(matches!(err, VideoError::Cancelled), "{err:?}");
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

#[tokio::test]
async fn the_probe_reports_each_tool() {
    let fakes = Fakes::new();
    let availability = fakes.tools().probe().await;
    assert_eq!(
        availability.ytdlp,
        ToolStatus::Ready {
            version: "2026.08.19".into()
        }
    );
    assert_eq!(
        availability.ffmpeg,
        ToolStatus::Ready {
            version: "5.1.6-0+deb12u1".into()
        }
    );
    assert!(availability.can_download());

    let plain = fakes.dir.path().join("not-executable");
    fs::write(&plain, "#!/bin/sh\necho 1\n").unwrap();
    let mut config = fakes.config();
    config.ytdlp = fakes.dir.path().join("missing");
    config.ffmpeg = plain;
    let availability = VideoTools::new(config).probe().await;
    assert_eq!(availability.ytdlp, ToolStatus::Missing);
    assert_eq!(availability.ffmpeg, ToolStatus::Missing);
    assert!(!availability.can_download());

    // Runs but is not the tool: exits 1, or prints no version.
    let mut config = fakes.config();
    config.ytdlp = "/usr/bin/false".into();
    config.ffmpeg = fakes.bin("yt-dlp"); // answers `-hide_banner -version` with nothing
    let availability = VideoTools::new(config).probe().await;
    assert_eq!(availability.ytdlp, ToolStatus::Broken);
    assert_eq!(availability.ffmpeg, ToolStatus::Broken);

    // A relative path is never looked up.
    let mut config = fakes.config();
    config.ytdlp = "yt-dlp".into();
    assert_eq!(
        VideoTools::new(config).probe().await.ytdlp,
        ToolStatus::Missing
    );
}

// ─── ffmpeg runs (fake binary) ──────────────────────────────────────────────

#[tokio::test]
async fn a_remux_runs_ffmpeg_with_its_hardening_flags() {
    let fakes = Fakes::new();
    let inputs = tempfile::tempdir().unwrap();
    let input = fake_input(inputs.path(), "OKAY");
    let remuxed = fakes
        .tools()
        .remux(&input, &CancellationToken::new())
        .await
        .expect("the remux");
    assert_eq!(remuxed.file.kind(), MediaKind::Mp4);
    let bytes = fs::read(remuxed.file.path()).unwrap();
    assert_eq!(remuxed.file.bytes(), bytes.len() as u64);
    assert_eq!(remuxed.digest, Digest::of(&bytes));
    // The fake's stream dump.
    assert_eq!(remuxed.info.duration_ms, Some(4_000));
    assert_eq!(
        (remuxed.info.width, remuxed.info.height),
        (Some(640), Some(360))
    );

    let argvs = fakes.state_files("argv.ffmpeg.");
    assert_eq!(argvs.len(), 2, "the remux, then the report of its result");
    let all: Vec<String> = argvs
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect();
    let remux = all
        .iter()
        .find(|argv| argv.contains("+faststart"))
        .expect("the remux argv");
    let args: Vec<&str> = remux.lines().collect();
    let pair = |flag: &str, value: &str| args.windows(2).any(|w| w[0] == flag && w[1] == value);
    assert!(pair("-protocol_whitelist", "file"));
    assert!(pair("-threads", "1"));
    assert!(pair("-filter_threads", "1"));
    assert!(pair("-f", "mov"), "the demuxer is forced");
    assert!(pair("-i", &format!("file:{}", input.display())));
    assert!(pair("-c", "copy"));
    assert!(pair("-movflags", "+faststart"));
    assert!(pair("-map_metadata", "-1"));
    assert!(pair("-f", "mp4"));
    assert!(args.contains(&"-nostdin"));
    assert!(args.last().unwrap().starts_with("file:"));
    // ffmpeg's environment and priority too.
    for env in fakes.state_files("env.ffmpeg.") {
        let env = fs::read_to_string(env).unwrap();
        assert!(!env.contains("CARGO"), "{env}");
        assert!(!env.contains("_PROXY"), "{env}");
    }
    for nice in fakes.state_files("nice.ffmpeg.") {
        let nice: i32 = fs::read_to_string(nice).unwrap().trim().parse().unwrap();
        assert!(nice >= 10);
    }

    // Persisting moves the file out and removes the run directory.
    let run_dir = remuxed.file.path().parent().unwrap().to_owned();
    let dest = inputs.path().join(format!("{}.mp4", remuxed.digest));
    remuxed.file.persist(&dest).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), bytes);
    assert!(!run_dir.exists());
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

#[tokio::test]
async fn a_download_with_its_index_first_is_ready_without_a_remux() {
    let fakes = Fakes::new();
    let tools = fakes.tools();
    let cancel = CancellationToken::new();
    let file = tools
        .download(&scenario("faststart"), &cancel, &mut |_| {})
        .await
        .unwrap();
    let path = file.path().to_owned();
    let bytes = fs::read(&path).unwrap();
    assert_eq!(index_first(&path).unwrap(), Some(true));
    let ready = tools.ready(file, &cancel).await.expect("ready");
    assert!(!ready.remuxed, "SPIKE-9: no remux when moov comes first");
    assert_eq!(ready.file.path(), path, "the same file, not a copy");
    assert_eq!(ready.digest, Digest::of(&bytes));
    assert_eq!(ready.file.bytes(), bytes.len() as u64);
    // Inspected, not remuxed: one ffmpeg run without `+faststart`.
    let argvs = fakes.state_files("argv.ffmpeg.");
    assert_eq!(argvs.len(), 1);
    assert!(
        !fs::read_to_string(&argvs[0])
            .unwrap()
            .contains("+faststart")
    );
    assert_eq!(ready.info.duration_ms, Some(4_000));
    drop(ready);

    // Without `moov` first (here: no `moov` at all), the download is remuxed
    // and dropped.
    let file = tools
        .download(&scenario("ok"), &cancel, &mut |_| {})
        .await
        .unwrap();
    let downloaded = file.path().to_owned();
    assert_eq!(index_first(&downloaded).unwrap(), None);
    let ready = tools.ready(file, &cancel).await.expect("ready");
    assert!(ready.remuxed);
    assert!(!downloaded.exists(), "the download's directory is gone");
    assert!(
        ready
            .file
            .path()
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".ffmpeg-")
    );
    drop(ready);
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

#[test]
fn index_first_reads_the_top_level_boxes() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, boxes: &[(&[u8; 4], usize)]| {
        let mut bytes = Vec::new();
        for (kind, len) in boxes {
            bytes.extend_from_slice(&u32::try_from(*len).unwrap().to_be_bytes());
            bytes.extend_from_slice(*kind);
            bytes.resize(bytes.len() + len - 8, 0);
        }
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        path
    };
    let first = write(
        "first.mp4",
        &[(b"ftyp", 24), (b"moov", 100), (b"mdat", 1000)],
    );
    assert_eq!(index_first(&first).unwrap(), Some(true));
    let last = write(
        "last.mp4",
        &[(b"ftyp", 24), (b"free", 8), (b"mdat", 1000), (b"moov", 100)],
    );
    assert_eq!(index_first(&last).unwrap(), Some(false));
    let neither = write("neither.mp4", &[(b"ftyp", 24), (b"free", 50)]);
    assert_eq!(index_first(&neither).unwrap(), None);
    // A 64-bit `mdat` size, then `moov`.
    let mut large = Vec::new();
    large.extend_from_slice(&24u32.to_be_bytes());
    large.extend_from_slice(b"ftypisom");
    large.resize(24, 0);
    large.extend_from_slice(&1u32.to_be_bytes());
    large.extend_from_slice(b"mdat");
    large.extend_from_slice(&32u64.to_be_bytes());
    large.resize(24 + 32, 0);
    let large_path = dir.path().join("large.mp4");
    fs::write(&large_path, &large).unwrap();
    assert_eq!(index_first(&large_path).unwrap(), Some(false));
    // A box that claims less than its header, a truncated file, not a box.
    let broken = write("broken.mp4", &[(b"ftyp", 24)]);
    let mut bytes = fs::read(&broken).unwrap();
    bytes.extend_from_slice(&4u32.to_be_bytes());
    bytes.extend_from_slice(b"free");
    fs::write(&broken, bytes).unwrap();
    assert_eq!(index_first(&broken).unwrap(), None);
    let text = dir.path().join("text.mp4");
    fs::write(&text, "not a video").unwrap();
    assert_eq!(index_first(&text).unwrap(), None);
    let link = dir.path().join("link.mp4");
    std::os::unix::fs::symlink(&first, &link).unwrap();
    assert!(index_first(&link).is_err(), "a symlink is not followed");
}

#[tokio::test]
async fn inputs_that_are_not_videos_never_reach_ffmpeg() {
    let fakes = Fakes::new();
    let mut config = fakes.config();
    config.max_video_bytes = 4096;
    let tools = VideoTools::new(config);
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();

    // An HLS playlist dressed as an MP4.
    let playlist = dir.path().join("video.mp4");
    fs::write(&playlist, "#EXTM3U\n#EXTINF:1,\nfile:///etc/passwd\n").unwrap();
    let empty = dir.path().join("empty.mp4");
    fs::write(&empty, b"").unwrap();
    let image = dir.path().join("image.mp4");
    fs::write(&image, b"\x89PNG\r\n\x1a\n0000").unwrap();
    let link = dir.path().join("link.mp4");
    std::os::unix::fs::symlink(fake_input(dir.path(), "OKAY"), &link).unwrap();
    for input in [&playlist, &empty, &image, &link, &dir.path().to_owned()] {
        let err = tools.inspect(input, &cancel).await.unwrap_err();
        assert!(
            matches!(err, VideoError::InvalidMedia),
            "{}: {err:?}",
            input.display()
        );
    }
    let err = tools
        .remux(Path::new("relative.mp4"), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, VideoError::InvalidMedia), "{err:?}");
    let big = dir.path().join("big.mp4");
    let mut bytes = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2".to_vec();
    bytes.resize(8192, 0);
    fs::write(&big, bytes).unwrap();
    let err = tools.remux(&big, &cancel).await.unwrap_err();
    assert!(
        matches!(err, VideoError::TooLarge { limit: 4096 }),
        "{err:?}"
    );
    let err = tools
        .inspect(&dir.path().join("missing.mp4"), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, VideoError::Io(_)), "{err:?}");

    assert!(fakes.state_files("argv.ffmpeg.").is_empty(), "ffmpeg ran");
}

#[tokio::test]
async fn a_failing_or_hanging_ffmpeg_is_reported_and_cleaned_up() {
    let fakes = Fakes::new();
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let err = fakes
        .tools()
        .remux(&fake_input(dir.path(), "FAIL"), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, VideoError::InvalidMedia), "{err:?}");
    assert!(!err.is_transient());
    // The fake hangs for 30 s.
    let mut config = fakes.config();
    config.ffmpeg_timeout = Duration::from_secs(2);
    let err = VideoTools::new(config)
        .remux(&fake_input(dir.path(), "HANG"), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, VideoError::TimedOut), "{err:?}");
    assert_eq!(fakes.leftovers(), Vec::<String>::new());
}

#[test]
fn sweep_removes_stale_run_directories_only() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = dir.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    fs::create_dir(scratch.join(".ytdlp-abc")).unwrap();
    fs::write(scratch.join(".ytdlp-abc").join("video.f137.mp4.part"), b"x").unwrap();
    fs::create_dir(scratch.join(".ffmpeg-def")).unwrap();
    fs::create_dir(scratch.join("keep")).unwrap();
    fs::write(scratch.join("cache.mp4"), b"x").unwrap();
    // Not stale yet.
    assert_eq!(
        sweep_scratch(&scratch, Duration::from_secs(3600)).unwrap(),
        0
    );
    assert_eq!(sweep_scratch(&scratch, Duration::ZERO).unwrap(), 2);
    let mut left: Vec<String> = fs::read_dir(&scratch)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["cache.mp4", "keep"]);
    assert_eq!(
        sweep_scratch(&dir.path().join("missing"), Duration::ZERO).unwrap(),
        0
    );
}

#[test]
fn error_codes_are_stable() {
    let codes: Vec<&str> = [
        VideoError::Ytdlp(YtdlpFailure::NotFound),
        VideoError::InvalidMedia,
        VideoError::InvalidOutput,
        VideoError::TooLarge { limit: 1 },
        VideoError::TimedOut,
        VideoError::Cancelled,
        VideoError::NoEgress,
        VideoError::Io(std::io::Error::other("x")),
    ]
    .iter()
    .map(VideoError::code)
    .collect();
    assert_eq!(
        codes,
        [
            "not_found",
            "invalid_media",
            "invalid_output",
            "too_large",
            "timed_out",
            "cancelled",
            "no_egress",
            "io"
        ]
    );
}

// ─── The real ffmpeg (SHELFY_TEST_FFMPEG) ───────────────────────────────────

/// The real ffmpeg, when the run asks for it.
fn real_ffmpeg() -> Option<PathBuf> {
    let path = std::env::var_os("SHELFY_TEST_FFMPEG")?;
    let path = PathBuf::from(path);
    assert!(
        path.is_absolute(),
        "SHELFY_TEST_FFMPEG must be an absolute path"
    );
    Some(path)
}

/// Generates a synthetic video with the real ffmpeg (not through the tools):
/// a test pattern and a tone, without `+faststart`.
fn generate(ffmpeg: &Path, out: &Path, size: &str, seconds: u32, extra: &[&str]) {
    let status = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!("testsrc=size={size}:rate=10"))
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=8000",
            "-t",
        ])
        .arg(seconds.to_string())
        .args(["-c:v", "mpeg4", "-c:a", "aac", "-shortest"])
        .args(extra)
        .arg("-y")
        .arg(out)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "ffmpeg could not generate {}",
        out.display()
    );
}

/// The order of the top-level boxes of an MP4.
fn top_boxes(path: &Path) -> Vec<String> {
    let mut file = fs::File::open(path).unwrap();
    let mut data = Vec::new();
    file.read_to_end(&mut data).unwrap();
    let mut boxes = Vec::new();
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        boxes.push(String::from_utf8_lossy(&data[at + 4..at + 8]).into_owned());
        if size < 8 {
            break;
        }
        at += size;
    }
    boxes
}

fn real_tools(ffmpeg: &Path, scratch: &Path) -> VideoTools {
    VideoTools::new(VideoToolsConfig::new(
        ToolPaths {
            ytdlp: "/nonexistent/yt-dlp".into(),
            ffmpeg: ffmpeg.to_owned(),
        },
        scratch,
        None,
    ))
}

#[tokio::test]
async fn real_ffmpeg_remuxes_with_the_index_first() {
    let Some(ffmpeg) = real_ffmpeg() else {
        eprintln!("SHELFY_TEST_FFMPEG is not set: skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("plain.mp4");
    generate(
        &ffmpeg,
        &input,
        "320x240",
        2,
        &["-metadata", "title=private title"],
    );
    let boxes = top_boxes(&input);
    let moov = boxes.iter().position(|b| b == "moov").unwrap();
    let mdat = boxes.iter().position(|b| b == "mdat").unwrap();
    assert!(
        mdat < moov,
        "the input must not be faststart already: {boxes:?}"
    );
    assert_eq!(index_first(&input).unwrap(), Some(false));
    let fast = dir.path().join("fast.mp4");
    generate(&ffmpeg, &fast, "320x240", 1, &["-movflags", "+faststart"]);
    assert_eq!(index_first(&fast).unwrap(), Some(true));

    let tools = real_tools(&ffmpeg, &dir.path().join("scratch"));
    let availability = tools.probe().await;
    assert!(availability.ffmpeg.is_ready(), "{availability:?}");
    let cancel = CancellationToken::new();
    let remuxed = tools.remux(&input, &cancel).await.expect("the remux");
    assert!(remuxed.remuxed);
    let boxes = top_boxes(remuxed.file.path());
    let moov = boxes.iter().position(|b| b == "moov").unwrap();
    let mdat = boxes.iter().position(|b| b == "mdat").unwrap();
    assert!(moov < mdat, "+faststart puts the index first: {boxes:?}");
    assert_eq!(index_first(remuxed.file.path()).unwrap(), Some(true));
    let bytes = fs::read(remuxed.file.path()).unwrap();
    assert_eq!(remuxed.digest, Digest::of(&bytes));
    assert_eq!(MediaKind::sniff(&bytes), Some(MediaKind::Mp4));
    let info = &remuxed.info;
    assert!(
        info.duration_ms
            .is_some_and(|ms| (1_900..=2_100).contains(&ms)),
        "{info:?}"
    );
    assert_eq!((info.width, info.height), (Some(320), Some(240)));
    assert_eq!(info.video_codec.as_deref(), Some("mpeg4"));
    assert_eq!(info.audio_codec.as_deref(), Some("aac"));
    // The metadata did not travel.
    assert!(!bytes.windows(13).any(|w| w == b"private title"));
    // Stream copy: the same samples, so about the same size.
    let input_len = fs::metadata(&input).unwrap().len();
    assert!(remuxed.file.bytes().abs_diff(input_len) < 4096);
    // The input inspects the same way.
    let direct = tools.inspect(&input, &cancel).await.unwrap();
    assert_eq!((direct.width, direct.height), (Some(320), Some(240)));
}

#[tokio::test]
async fn real_ffmpeg_extracts_a_poster_and_keyframes() {
    let Some(ffmpeg) = real_ffmpeg() else {
        eprintln!("SHELFY_TEST_FFMPEG is not set: skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("wide.mp4");
    generate(&ffmpeg, &input, "1920x1080", 4, &[]);
    let tools = real_tools(&ffmpeg, &dir.path().join("scratch"));
    let cancel = CancellationToken::new();

    let poster = tools.poster(&input, &cancel).await.expect("the poster");
    assert_eq!((poster.source_width, poster.source_height), (1080, 608));
    assert_eq!((poster.width, poster.height), (1080, 608));
    assert_eq!(MediaKind::sniff(&poster.webp), Some(MediaKind::Webp));
    assert!(!poster.thumbhash.is_empty() && poster.thumbhash.len() <= 25);

    let g480 = RenderSpec {
        max_side: 480,
        quality: 75.0,
    };
    let frames = tools
        .keyframes(&input, 4, g480, &cancel)
        .await
        .expect("the keyframes");
    let times: Vec<u64> = frames.iter().map(|frame| frame.at_ms).collect();
    assert_eq!(times, [500, 1_500, 2_500, 3_500]);
    for frame in &frames {
        assert_eq!((frame.image.width, frame.image.height), (480, 270));
    }
    // Different moments of a moving pattern.
    assert_ne!(frames[0].image.webp, frames[3].image.webp);
    // At most MAX_KEYFRAMES.
    let many = tools.keyframes(&input, 100, g480, &cancel).await.unwrap();
    assert!(many.len() <= MAX_KEYFRAMES);
    assert_eq!(
        fs::read_dir(dir.path().join("scratch")).unwrap().count(),
        0,
        "every run directory is gone"
    );
}

#[tokio::test]
async fn real_ffmpeg_applies_rotation_and_pixel_shape() {
    let Some(ffmpeg) = real_ffmpeg() else {
        eprintln!("SHELFY_TEST_FFMPEG is not set: skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.mp4");
    generate(&ffmpeg, &plain, "320x240", 2, &[]);
    let tools = real_tools(&ffmpeg, &dir.path().join("scratch"));
    let cancel = CancellationToken::new();

    // Anamorphic: 320x240 stored, SAR 4:3, shown 426x240.
    let wide = dir.path().join("wide.mp4");
    let status = Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&plain)
        .args(["-c", "copy", "-aspect", "16:9", "-y"])
        .arg(&wide)
        .status()
        .unwrap();
    assert!(status.success());
    let info = tools.inspect(&wide, &cancel).await.unwrap();
    assert_eq!((info.width, info.height), (Some(426), Some(240)));
    let poster = tools.poster(&wide, &cancel).await.unwrap();
    assert_eq!((poster.width, poster.height), (426, 240));

    // Rotated a quarter turn (`-display_rotation` needs ffmpeg 6 or later).
    let turned = dir.path().join("turned.mp4");
    let rotated = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-display_rotation",
            "90",
            "-i",
        ])
        .arg(&plain)
        .args(["-c", "copy", "-y"])
        .arg(&turned)
        .status()
        .unwrap()
        .success();
    if rotated {
        let info = tools.inspect(&turned, &cancel).await.unwrap();
        assert_eq!((info.width, info.height), (Some(240), Some(320)));
        // The remux keeps the rotation.
        let remuxed = tools.remux(&turned, &cancel).await.unwrap();
        assert_eq!(
            (remuxed.info.width, remuxed.info.height),
            (Some(240), Some(320))
        );
        let poster = tools.poster(&turned, &cancel).await.unwrap();
        assert_eq!((poster.width, poster.height), (240, 320));
    } else {
        eprintln!("this ffmpeg has no -display_rotation: rotation not checked");
    }
}

#[tokio::test]
async fn real_ffmpeg_refuses_damaged_files() {
    let Some(ffmpeg) = real_ffmpeg() else {
        eprintln!("SHELFY_TEST_FFMPEG is not set: skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let tools = real_tools(&ffmpeg, &dir.path().join("scratch"));
    let cancel = CancellationToken::new();
    // The right magic bytes, then garbage.
    let damaged = dir.path().join("damaged.mp4");
    let mut bytes = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2".to_vec();
    bytes.extend((0..4096u32).map(|i| (i * 7 % 251) as u8));
    fs::write(&damaged, bytes).unwrap();
    for result in [
        tools.inspect(&damaged, &cancel).await.map(|_| ()),
        tools.remux(&damaged, &cancel).await.map(|_| ()),
        tools.poster(&damaged, &cancel).await.map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(VideoError::InvalidMedia)),
            "{result:?}"
        );
    }
    // Audio only: no video stream to copy.
    let audio = dir.path().join("audio.mp4");
    let status = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=8000",
            "-t",
            "1",
            "-c:a",
            "aac",
            "-y",
        ])
        .arg(&audio)
        .status()
        .unwrap();
    assert!(status.success());
    let err = tools.remux(&audio, &cancel).await.unwrap_err();
    assert!(matches!(err, VideoError::InvalidMedia), "{err:?}");
    assert_eq!(fs::read_dir(dir.path().join("scratch")).unwrap().count(), 0);
}

// ─── The real yt-dlp (SHELFY_TEST_YTDLP) ────────────────────────────────────

#[tokio::test]
async fn real_yt_dlp_takes_the_argv_and_goes_through_the_proxy() {
    let Some(ytdlp) = std::env::var_os("SHELFY_TEST_YTDLP").map(PathBuf::from) else {
        eprintln!("SHELFY_TEST_YTDLP is not set: skipped");
        return;
    };
    // A proxy that refuses every CONNECT, as Smokescreen refuses a denied
    // destination: no request leaves the machine.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_ok() {
                recorded
                    .lock()
                    .unwrap()
                    .push(request_line.trim().to_owned());
            }
            let mut stream = stream;
            let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = real_ffmpeg().unwrap_or_else(|| "/usr/bin/ffmpeg".into());
    let mut config = VideoToolsConfig::new(
        ToolPaths { ytdlp, ffmpeg },
        dir.path().join("scratch"),
        Some(Egress::Proxy(
            ProxyUrl::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
        )),
    );
    config.download_timeout = Duration::from_secs(60);
    let tools = VideoTools::new(config);
    let availability = tools.probe().await;
    assert!(availability.ytdlp.is_ready(), "{availability:?}");

    let request = DownloadRequest::new(post(
        VideoPlatform::Twitter,
        "https://x.com/someone/status/1790000000000000001",
    ));
    let err = tools
        .download(&request, &CancellationToken::new(), &mut |_| {})
        .await
        .unwrap_err();
    // Every flag was accepted (a bad one exits with a usage error and no
    // request); the refusal is a transient proxy error.
    assert!(
        matches!(err, VideoError::Ytdlp(YtdlpFailure::Transient)),
        "{err:?}"
    );
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .any(|line| line.starts_with("CONNECT ") && line.contains("x.com:443")),
        "the proxy saw {seen:?}"
    );
    assert!(
        seen.iter().all(|line| line.starts_with("CONNECT ")),
        "{seen:?}"
    );
    assert_eq!(fs::read_dir(dir.path().join("scratch")).unwrap().count(), 0);
}
