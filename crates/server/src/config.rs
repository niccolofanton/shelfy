//! Configuration from the environment (plan §3.2, §3.4).
//!
//! Every setting is an environment variable with a matching command-line flag
//! (the flag wins). `shelfy-server serve --help` lists them with their defaults;
//! `deploy/README.md` has the same table. Values are validated once, at start:
//! a bad value stops the process with a message instead of surfacing later.
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `SHELFY_DATA_DIR` | `/data/shelfy` | data directory (§2.5) |
//! | `SHELFY_LISTEN_ADDR` | `0.0.0.0:8080` | API listener, reached through the edge proxy |
//! | `SHELFY_METRICS_ADDR` | `0.0.0.0:9464` | Prometheus listener, internal network only |
//! | `SHELFY_PUBLIC_URL` | `http://localhost:8080` | public origin of the web app: links the server hands out, the CSRF `Origin` check, the passkey relying party (RP ID = its host) |
//! | `SHELFY_TRUSTED_PROXIES` | none | CIDR blocks whose `CF-Connecting-IP` is believed; otherwise the TCP peer is the client |
//! | `SHELFY_LOG_FORMAT` | `json` | `json` (one object per line) or `text` |
//! | `RUST_LOG` | `info` | log filter (`tracing` env-filter syntax) |
//! | `SHELFY_OWNER_EMAIL` | none | default `--email` of `admin create-owner` and `admin login-link` |
//! | `SHELFY_SMTP_HOST` | none | SMTP relay (`host[:port]`) for sign-in emails; email is off without it |
//! | `SHELFY_SMTP_TLS` | `starttls` | `starttls`, `tls` (implicit) or `none` (local catcher, no credentials) |
//! | `SHELFY_SMTP_USER`, `SHELFY_SMTP_PASSWORD` | none | SMTP credentials, set together |
//! | `SHELFY_SMTP_FROM` | none | sender, required with SMTP |
//! | `SHELFY_DEV_MAILBOX` | `false` | write emails to `<data>/dev-mailbox/*.eml` instead; loopback public URL only |
//! | `SHELFY_WEB_DIR` | none | the built web app (`web/dist`) to serve; `/app/web` in the image. Unset: the API only |
//! | `SHELFY_EGRESS_PROXY` | none | the egress proxy every outbound request goes through; unset: direct, with the resolver's address check |
//! | `SHELFY_EGRESS_ALLOW_ORIGINS` | none | exact origins of the operator's AI node, reachable at a private address (L15) |
//! | `SHELFY_CAPTURE_URL` | none | the capture service, the only origin of the internal client |
//! | `SHELFY_ARCHIVE_RATE_INSTAGRAM`, `…_X`, `…_PINTEREST` | `2` | CDN requests per second per host group |
//! | `SHELFY_DEV_EGRESS_HOSTS`, `SHELFY_DEV_EGRESS_CA` | none | dev and tests: fixture hosts on loopback ports, and their CA; loopback public URL only |
//! | `SHELFY_IMPORT_MAX_GB` | `10` | largest file to import (a JSON export or a bundle), in GiB: the cap of an `import` upload |
//! | `SHELFY_YTDLP_BIN` | `/opt/yt-dlp/yt-dlp` | yt-dlp, for on-demand videos (the image's pinned build, L6) |
//! | `SHELFY_FFMPEG_BIN` | `/usr/bin/ffmpeg` | ffmpeg, which readies videos and extracts frames (Debian's, L5) |
//! | `SHELFY_MEDIA_BUDGET_GB` | `30` | the media budget of every user library together, in GiB: stores that would pass it are refused with `storage_full` ([`crate::quota`]); 0 turns it off |
//!
//! [`crate::mail`] validates the email settings and [`crate::outbound`] the
//! outbound ones. Later tasks add their variables here (master key).

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, ValueEnum};
use shelfy_core::db::{ControlDbConfig, LIBRARY_FILE_NAME, UserDbCacheConfig, UserDbConfig};
use shelfy_media::video::{DEFAULT_FFMPEG_BIN, DEFAULT_YTDLP_BIN, ToolPaths};
use url::Url;

use crate::auth::AuthConfig;
use crate::jobs::JobsConfig;
use crate::mail::{MailArgs, MailConfig};
use crate::net::TrustedProxies;
use crate::outbound::{OutboundArgs, OutboundConfig};
use crate::quota::{DEFAULT_MEDIA_BUDGET_GB, QuotaConfig};
use crate::rate_limit::RateLimitConfig;
use crate::static_files::WebApp;

/// Default of `SHELFY_DATA_DIR`.
pub const DEFAULT_DATA_DIR: &str = "/data/shelfy";
/// Default of `SHELFY_LISTEN_ADDR`.
pub const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:8080";
/// Default of `SHELFY_METRICS_ADDR`.
pub const DEFAULT_METRICS_ADDR: &str = "0.0.0.0:9464";
/// Default of `SHELFY_PUBLIC_URL`.
pub const DEFAULT_PUBLIC_URL: &str = "http://localhost:8080";
/// Default of `SHELFY_IMPORT_MAX_GB`.
pub const DEFAULT_IMPORT_MAX_GB: u64 = 10;
/// Largest accepted `SHELFY_IMPORT_MAX_GB`.
pub const MAX_IMPORT_MAX_GB: u64 = 1024;
const GIB: u64 = 1024 * 1024 * 1024;

/// How long a graceful shutdown may take in total (plan §2.3: exit in ≤25 s,
/// inside compose's `stop_grace_period: 30s`).
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(25);

/// File name of the control database inside `<data>/control/`.
pub const CONTROL_DB_FILE: &str = "control.sqlite";

/// `--data-dir` / `SHELFY_DATA_DIR`, shared by every command.
#[derive(Clone, Debug, Args)]
pub struct DataDirArg {
    /// Data directory: control database, user libraries, media, caches and
    /// work directories. `serve` creates its layout if missing.
    #[arg(
        long = "data-dir",
        env = "SHELFY_DATA_DIR",
        value_name = "DIR",
        default_value = DEFAULT_DATA_DIR,
        global = true
    )]
    pub data_dir: PathBuf,
}

/// `--public-url` / `SHELFY_PUBLIC_URL`.
#[derive(Clone, Debug, Args)]
pub struct PublicUrlArg {
    /// Public origin of the web app, without a path (for example
    /// `https://refs.niccolofanton.dev`). Links the server hands out start
    /// with it, the CSRF check requires it as the `Origin` of every
    /// state-changing cookie request, and passkeys are bound to it: the RP ID
    /// is its host, and changing the host orphans every registered passkey.
    /// Use https unless the host is localhost: the session cookie is
    /// `Secure`, and browsers offer passkeys in secure contexts only.
    #[arg(
        long = "public-url",
        env = "SHELFY_PUBLIC_URL",
        value_name = "URL",
        default_value = DEFAULT_PUBLIC_URL,
        value_parser = PublicUrl::parse
    )]
    pub public_url: PublicUrl,
}

/// Arguments of `shelfy-server serve`.
#[derive(Clone, Debug, Args)]
pub struct ServeArgs {
    #[command(flatten)]
    pub data: DataDirArg,

    #[command(flatten)]
    pub public: PublicUrlArg,

    /// Address of the API listener. The edge proxy is its only client.
    #[arg(
        long = "listen",
        env = "SHELFY_LISTEN_ADDR",
        value_name = "ADDR",
        default_value = DEFAULT_LISTEN_ADDR
    )]
    pub listen: SocketAddr,

    /// Address of the Prometheus listener (`GET /metrics`). Expose it on the
    /// internal network only: the edge proxy never forwards it.
    #[arg(
        long = "metrics-listen",
        env = "SHELFY_METRICS_ADDR",
        value_name = "ADDR",
        default_value = DEFAULT_METRICS_ADDR
    )]
    pub metrics_listen: SocketAddr,

    /// Log format on stdout. The level comes from `RUST_LOG` (default `info`).
    #[arg(
        long = "log-format",
        env = "SHELFY_LOG_FORMAT",
        value_enum,
        default_value_t = LogFormat::Json
    )]
    pub log_format: LogFormat,

    /// Proxies whose `CF-Connecting-IP` header names the client, as CIDR
    /// blocks separated by commas: the network of the edge nginx. Empty: the
    /// header is ignored and the TCP peer is the client (rate limits).
    #[arg(
        long = "trusted-proxies",
        env = "SHELFY_TRUSTED_PROXIES",
        value_name = "CIDRS",
        default_value = "",
        hide_default_value = true,
        value_parser = TrustedProxies::parse
    )]
    pub trusted_proxies: TrustedProxies,

    #[command(flatten)]
    pub mail: MailArgs,

    /// Boxed: `serve`'s arguments stay small enough for clippy's
    /// `large_enum_variant` in `cli::Command`.
    #[command(flatten)]
    pub outbound: Box<OutboundArgs>,

    /// The built web app (`web/dist`) to serve to browsers: its
    /// `index.html` answers every path no route takes. The image sets
    /// `/app/web`. Unset: the API only.
    #[arg(long = "web-dir", env = "SHELFY_WEB_DIR", value_name = "DIR")]
    pub web_dir: Option<PathBuf>,

    /// Largest file a user may import (a JSON export or an export bundle),
    /// in GiB (1–1024): the cap of an `import` upload. A user's uploads
    /// waiting to be used may hold this plus 1 GiB.
    #[arg(
        long = "import-max-gb",
        env = "SHELFY_IMPORT_MAX_GB",
        value_name = "GIB",
        default_value_t = DEFAULT_IMPORT_MAX_GB,
        value_parser = clap::value_parser!(u64).range(1..=MAX_IMPORT_MAX_GB)
    )]
    pub import_max_gb: u64,

    #[command(flatten)]
    pub video_tools: VideoToolArgs,

    /// The media budget of every user library together, in GiB (2^30
    /// bytes): a store that would take the `users` area of the data
    /// directory past it is refused with `storage_full`, for every user.
    /// 0 turns the budget off.
    #[arg(
        long = "media-budget-gb",
        env = "SHELFY_MEDIA_BUDGET_GB",
        value_name = "GIB",
        default_value_t = DEFAULT_MEDIA_BUDGET_GB,
        value_parser = parse_media_budget
    )]
    pub media_budget_gb: u64,
}

/// The binaries of the video tools (P4-06, plan §2.13).
#[derive(Clone, Debug, Args)]
pub struct VideoToolArgs {
    /// yt-dlp, for on-demand videos (D15, L17). The image ships its pinned,
    /// unpacked build there (L6). An absolute path; a missing binary turns
    /// the yt-dlp route off, it does not stop the server.
    #[arg(
        long = "ytdlp-bin",
        env = "SHELFY_YTDLP_BIN",
        value_name = "PATH",
        default_value = DEFAULT_YTDLP_BIN
    )]
    pub ytdlp: PathBuf,

    /// ffmpeg, which readies videos for the browser and extracts posters and
    /// keyframes. The image ships Debian's there (L5). An absolute path; a
    /// missing binary turns those features off.
    #[arg(
        long = "ffmpeg-bin",
        env = "SHELFY_FFMPEG_BIN",
        value_name = "PATH",
        default_value = DEFAULT_FFMPEG_BIN
    )]
    pub ffmpeg: PathBuf,
}

impl VideoToolArgs {
    /// Validates the paths: absolute, or empty for the default.
    ///
    /// # Errors
    ///
    /// [`ConfigError::ToolPath`] naming the variable of a relative path.
    pub fn paths(self) -> Result<ToolPaths, ConfigError> {
        let defaults = ToolPaths::default();
        let pick = |path: PathBuf, default: PathBuf, var: &'static str| {
            if path.as_os_str().is_empty() {
                Ok(default)
            } else if path.is_absolute() {
                Ok(path)
            } else {
                Err(ConfigError::ToolPath(var))
            }
        };
        Ok(ToolPaths {
            ytdlp: pick(self.ytdlp, defaults.ytdlp, "SHELFY_YTDLP_BIN")?,
            ffmpeg: pick(self.ffmpeg, defaults.ffmpeg, "SHELFY_FFMPEG_BIN")?,
        })
    }
}

/// Parses `SHELFY_MEDIA_BUDGET_GB`: a whole number of GiB. Empty counts as
/// unset, as for the other variables: the default.
fn parse_media_budget(raw: &str) -> Result<u64, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(DEFAULT_MEDIA_BUDGET_GB);
    }
    raw.parse()
        .map_err(|_| format!("{raw:?} is not a whole number of GiB"))
}

/// Format of the logs on stdout (plan §3.7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    /// One JSON object per line, for Docker's `json-file` driver.
    #[default]
    Json,
    /// Human-readable lines, for local runs.
    Text,
}

/// Validated settings of `serve`.
#[derive(Clone, Debug)]
pub struct Config {
    /// Data directory (absolute).
    pub data_dir: DataDir,
    /// API listener.
    pub listen: SocketAddr,
    /// Prometheus listener.
    pub metrics_listen: SocketAddr,
    /// Public origin.
    pub public_url: PublicUrl,
    /// Log format.
    pub log_format: LogFormat,
    /// Total time a graceful shutdown may take.
    pub shutdown_grace: Duration,
    /// Control database connections (plan §2.3: 1 writer, 4 readers).
    pub control_db: ControlDbConfig,
    /// Settings of each user database (plan §2.3).
    pub user_db: UserDbConfig,
    /// Limits of the open-user-database cache (plan §2.3: 64, 10 min idle).
    pub user_db_cache: UserDbCacheConfig,
    /// Proxies whose `CF-Connecting-IP` is believed.
    pub trusted_proxies: TrustedProxies,
    /// Outgoing email (SMTP, the dev mailbox, or off).
    pub mail: MailConfig,
    /// Session lifetimes and sign-in limits (plan §2.11).
    pub auth: AuthConfig,
    /// The request limits of signed-in users (plan §2.9); the sign-in limit
    /// per client address is in `auth`.
    pub rate_limits: RateLimitConfig,
    /// The job kinds and the clock of the job system (plan §2.12).
    pub jobs: JobsConfig,
    /// The web app to serve, if any (P1-09).
    pub web: Option<WebApp>,
    /// Outbound HTTP: the proxy or direct mode, the operator allowlist, the
    /// capture service, the CDN limits (P2-04).
    pub outbound: OutboundConfig,
    /// The largest `import` upload, in bytes (`SHELFY_IMPORT_MAX_GB`, P4-08).
    pub import_max_bytes: u64,
    /// The binaries of the video tools (P4-06).
    pub video_tools: ToolPaths,
    /// The global media budget (plan §3.1; P4-07).
    pub quota: QuotaConfig,
}

impl Config {
    /// Validates the arguments of `serve`.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] when a value is unusable.
    pub fn from_args(args: ServeArgs) -> Result<Self, ConfigError> {
        let data_dir = DataDir::new(args.data.data_dir).map_err(ConfigError::DataDir)?;
        if overlaps(args.listen, args.metrics_listen) {
            return Err(ConfigError::SameListener(args.listen, args.metrics_listen));
        }
        let public_url = args.public.public_url;
        let mail =
            MailConfig::from_args(args.mail, &data_dir, &public_url).map_err(ConfigError::Mail)?;
        let outbound = OutboundConfig::from_args(*args.outbound, &public_url)
            .map_err(ConfigError::Outbound)?;
        let web = args
            .web_dir
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(WebApp::load)
            .transpose()
            .map_err(ConfigError::WebDir)?;
        let video_tools = args.video_tools.paths()?;
        let quota = QuotaConfig::from_gib(args.media_budget_gb)
            .ok_or(ConfigError::MediaBudget(args.media_budget_gb))?;
        Ok(Self {
            listen: args.listen,
            metrics_listen: args.metrics_listen,
            public_url,
            log_format: args.log_format,
            trusted_proxies: args.trusted_proxies,
            mail,
            web,
            outbound,
            import_max_bytes: args.import_max_gb.saturating_mul(GIB),
            video_tools,
            quota,
            ..Self::with_data_dir(data_dir)
        })
    }

    /// The defaults of every setting, with `data_dir`. Tests start from it.
    #[must_use]
    pub fn with_data_dir(data_dir: DataDir) -> Self {
        Self {
            data_dir,
            listen: DEFAULT_LISTEN_ADDR.parse().expect("valid default address"),
            metrics_listen: DEFAULT_METRICS_ADDR.parse().expect("valid default address"),
            public_url: PublicUrl::parse(DEFAULT_PUBLIC_URL).expect("valid default URL"),
            log_format: LogFormat::Json,
            shutdown_grace: SHUTDOWN_GRACE,
            control_db: ControlDbConfig::default(),
            user_db: UserDbConfig::default(),
            user_db_cache: UserDbCacheConfig::default(),
            trusted_proxies: TrustedProxies::default(),
            mail: MailConfig::Disabled,
            auth: AuthConfig::default(),
            rate_limits: RateLimitConfig::default(),
            jobs: JobsConfig::default(),
            web: None,
            outbound: OutboundConfig::default(),
            import_max_bytes: DEFAULT_IMPORT_MAX_GB * GIB,
            video_tools: ToolPaths::default(),
            quota: QuotaConfig::default(),
        }
    }
}

/// Whether two listeners would claim the same port. Port 0 (an ephemeral port)
/// never clashes.
fn overlaps(a: SocketAddr, b: SocketAddr) -> bool {
    a.port() != 0
        && a.port() == b.port()
        && (a.ip() == b.ip() || a.ip().is_unspecified() || b.ip().is_unspecified())
}

/// An invalid setting.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The data directory path is unusable.
    #[error("SHELFY_DATA_DIR: {0}")]
    DataDir(io::Error),
    /// The API and metrics listeners would share a port.
    #[error(
        "SHELFY_LISTEN_ADDR ({0}) and SHELFY_METRICS_ADDR ({1}) must use different ports: metrics stay off the public listener"
    )]
    SameListener(SocketAddr, SocketAddr),
    /// The email settings are inconsistent.
    #[error("{0}")]
    Mail(String),
    /// The web app directory is unusable.
    #[error("SHELFY_WEB_DIR: {0}")]
    WebDir(io::Error),
    /// The outbound settings are inconsistent.
    #[error("{0}")]
    Outbound(String),
    /// A video tool's path is relative.
    #[error("{0}: give an absolute path")]
    ToolPath(&'static str),
    /// The media budget does not fit in bytes.
    #[error("SHELFY_MEDIA_BUDGET_GB ({0}) is too large")]
    MediaBudget(u64),
}

/// The public origin of the web app: `http(s)://host[:port]`, no trailing slash.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PublicUrl(String);

impl PublicUrl {
    /// Parses and normalizes an origin. A trailing `/` is accepted; a path,
    /// query, fragment or credentials are not.
    ///
    /// # Errors
    ///
    /// A message naming what is wrong.
    pub fn parse(input: &str) -> Result<Self, String> {
        let url = Url::parse(input.trim()).map_err(|e| format!("not an absolute URL ({e})"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("the scheme must be http or https".into());
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err("a host is required".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("credentials are not allowed".into());
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err("give the origin only, without a path, query or fragment".into());
        }
        Ok(Self(url.origin().ascii_serialization()))
    }

    /// The origin, for example `https://refs.niccolofanton.dev`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// An absolute URL for `path`, which must start with `/`.
    #[must_use]
    pub fn join(&self, path: &str) -> String {
        debug_assert!(path.starts_with('/'), "paths are absolute");
        format!("{}{path}", self.0)
    }
}

impl fmt::Display for PublicUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for PublicUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicUrl({})", self.0)
    }
}

/// The data directory and its layout (plan §2.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataDir(PathBuf);

impl DataDir {
    /// A data directory rooted at `root`; a relative path is resolved against
    /// the current directory. Nothing is created yet.
    ///
    /// # Errors
    ///
    /// An empty path, or a current directory that cannot be read.
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        if root.as_os_str().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the path is empty",
            ));
        }
        std::path::absolute(root).map(Self)
    }

    /// The root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.0
    }

    /// `control/`.
    #[must_use]
    pub fn control_dir(&self) -> PathBuf {
        self.0.join("control")
    }

    /// `control/control.sqlite`.
    #[must_use]
    pub fn control_db(&self) -> PathBuf {
        self.control_dir().join(CONTROL_DB_FILE)
    }

    /// `users/`: one directory per user.
    #[must_use]
    pub fn users_dir(&self) -> PathBuf {
        self.0.join("users")
    }

    /// `users/<user_id>/library.sqlite`. The caller validates `user_id`.
    #[must_use]
    pub fn library_db(&self, user_id: &str) -> PathBuf {
        self.users_dir().join(user_id).join(LIBRARY_FILE_NAME)
    }

    /// `backup-staging/db/`: where `admin snapshot` writes by default (§3.5).
    #[must_use]
    pub fn snapshot_dir(&self) -> PathBuf {
        self.0.join("backup-staging").join("db")
    }

    /// `work/uploads/`: tus uploads, in progress and complete (§2.5).
    #[must_use]
    pub fn uploads_dir(&self) -> PathBuf {
        self.0.join("work").join("uploads")
    }

    /// `work/migrations/`: scratch space of migration installs, one directory
    /// per install, removed when it ends.
    #[must_use]
    pub fn migrations_dir(&self) -> PathBuf {
        self.0.join("work").join("migrations")
    }

    /// Creates `control/` and `users/` (mode 0750) if they are missing.
    ///
    /// # Errors
    ///
    /// The file system refused.
    pub fn create_layout(&self) -> io::Result<()> {
        create_private_dir(&self.control_dir())?;
        create_private_dir(&self.users_dir())
    }
}

/// Creates `path` and its missing parents; new directories get mode 0750 (§2.5).
///
/// # Errors
///
/// The file system refused.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o750);
    }
    builder.create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_url_keeps_the_origin_only() {
        let url = PublicUrl::parse("https://Shelfy.Example.com/").unwrap();
        assert_eq!(url.as_str(), "https://shelfy.example.com");
        assert_eq!(
            url.join("/invite/abc"),
            "https://shelfy.example.com/invite/abc"
        );
        let local = PublicUrl::parse("http://localhost:18087").unwrap();
        assert_eq!(local.as_str(), "http://localhost:18087");
        // The default port is dropped, as browsers do in `Origin`.
        let default_port = PublicUrl::parse("https://example.com:443").unwrap();
        assert_eq!(default_port.as_str(), "https://example.com");
    }

    #[test]
    fn public_url_refuses_anything_but_an_origin() {
        for bad in [
            "",
            "shelfy.example.com",
            "ftp://example.com",
            "https://example.com/app",
            "https://example.com/?a=1",
            "https://example.com/#x",
            "https://user:pw@example.com",
            "file:///tmp",
        ] {
            assert!(PublicUrl::parse(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn listeners_must_not_share_a_port() {
        let any: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let local: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let other: SocketAddr = "127.0.0.1:9464".parse().unwrap();
        let ephemeral: SocketAddr = "127.0.0.1:0".parse().unwrap();
        assert!(overlaps(any, local));
        assert!(overlaps(local, local));
        assert!(!overlaps(local, other));
        assert!(!overlaps(ephemeral, ephemeral));
        assert!(!overlaps(
            "127.0.0.1:8080".parse().unwrap(),
            "127.0.0.2:8080".parse().unwrap()
        ));
    }

    #[test]
    fn data_dir_layout_follows_the_plan() {
        let dir = DataDir::new("/data/shelfy").unwrap();
        assert_eq!(
            dir.control_db(),
            Path::new("/data/shelfy/control/control.sqlite")
        );
        assert_eq!(
            dir.library_db("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
            Path::new("/data/shelfy/users/01ARZ3NDEKTSV4RRFFQ69G5FAV/library.sqlite")
        );
        assert_eq!(
            dir.snapshot_dir(),
            Path::new("/data/shelfy/backup-staging/db")
        );
        assert_eq!(dir.uploads_dir(), Path::new("/data/shelfy/work/uploads"));
        assert_eq!(
            dir.migrations_dir(),
            Path::new("/data/shelfy/work/migrations")
        );
        assert!(DataDir::new("").is_err());
        assert!(DataDir::new("relative/dir").unwrap().root().is_absolute());
    }

    #[test]
    fn video_tool_paths_are_absolute_or_the_default() {
        let args = |ytdlp: &str, ffmpeg: &str| VideoToolArgs {
            ytdlp: ytdlp.into(),
            ffmpeg: ffmpeg.into(),
        };
        let image = args(DEFAULT_YTDLP_BIN, DEFAULT_FFMPEG_BIN).paths().unwrap();
        assert_eq!(image, ToolPaths::default());
        assert_eq!(image.ytdlp, Path::new("/opt/yt-dlp/yt-dlp"));
        assert_eq!(image.ffmpeg, Path::new("/usr/bin/ffmpeg"));
        // Empty counts as unset.
        assert_eq!(args("", "").paths().unwrap(), ToolPaths::default());
        let local = args("/opt/homebrew/bin/yt-dlp", "/opt/homebrew/bin/ffmpeg")
            .paths()
            .unwrap();
        assert_eq!(local.ffmpeg, Path::new("/opt/homebrew/bin/ffmpeg"));
        assert!(matches!(
            args("yt-dlp", DEFAULT_FFMPEG_BIN).paths(),
            Err(ConfigError::ToolPath("SHELFY_YTDLP_BIN"))
        ));
        assert!(matches!(
            args(DEFAULT_YTDLP_BIN, "bin/ffmpeg").paths(),
            Err(ConfigError::ToolPath("SHELFY_FFMPEG_BIN"))
        ));
    }

    #[test]
    fn the_layout_is_created_private() {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path().join("shelfy")).unwrap();
        data.create_layout().unwrap();
        data.create_layout().unwrap(); // idempotent
        for sub in [data.control_dir(), data.users_dir()] {
            assert!(sub.is_dir(), "{}", sub.display());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&sub).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o750, "{}", sub.display());
            }
        }
    }
}
