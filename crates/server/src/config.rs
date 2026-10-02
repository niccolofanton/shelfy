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
//! | `SHELFY_PUBLIC_URL` | `http://localhost:8080` | public origin of the web app: links the server hands out, the CSRF `Origin` check |
//! | `SHELFY_TRUSTED_PROXIES` | none | CIDR blocks whose `CF-Connecting-IP` is believed; otherwise the TCP peer is the client |
//! | `SHELFY_LOG_FORMAT` | `json` | `json` (one object per line) or `text` |
//! | `RUST_LOG` | `info` | log filter (`tracing` env-filter syntax) |
//! | `SHELFY_OWNER_EMAIL` | none | default `--email` of `admin create-owner` and `admin login-link` |
//! | `SHELFY_SMTP_HOST` | none | SMTP relay (`host[:port]`) for sign-in emails; email is off without it |
//! | `SHELFY_SMTP_TLS` | `starttls` | `starttls`, `tls` (implicit) or `none` (local catcher, no credentials) |
//! | `SHELFY_SMTP_USER`, `SHELFY_SMTP_PASSWORD` | none | SMTP credentials, set together |
//! | `SHELFY_SMTP_FROM` | none | sender, required with SMTP |
//! | `SHELFY_DEV_MAILBOX` | `false` | write emails to `<data>/dev-mailbox/*.eml` instead; loopback public URL only |
//!
//! [`crate::mail`] validates the email settings. Later tasks add their
//! variables here (master key, capture and egress endpoints, media budgets).

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, ValueEnum};
use shelfy_core::db::{ControlDbConfig, LIBRARY_FILE_NAME, UserDbCacheConfig, UserDbConfig};
use url::Url;

use crate::auth::AuthConfig;
use crate::mail::{MailArgs, MailConfig};
use crate::net::TrustedProxies;

/// Default of `SHELFY_DATA_DIR`.
pub const DEFAULT_DATA_DIR: &str = "/data/shelfy";
/// Default of `SHELFY_LISTEN_ADDR`.
pub const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:8080";
/// Default of `SHELFY_METRICS_ADDR`.
pub const DEFAULT_METRICS_ADDR: &str = "0.0.0.0:9464";
/// Default of `SHELFY_PUBLIC_URL`.
pub const DEFAULT_PUBLIC_URL: &str = "http://localhost:8080";

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
    /// with it, and the CSRF check requires it as the `Origin` of every
    /// state-changing cookie request; later it is the passkey RP ID too. Use
    /// https unless the host is localhost: the session cookie is `Secure`.
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
        Ok(Self {
            listen: args.listen,
            metrics_listen: args.metrics_listen,
            public_url,
            log_format: args.log_format,
            trusted_proxies: args.trusted_proxies,
            mail,
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
        assert!(DataDir::new("").is_err());
        assert!(DataDir::new("relative/dir").unwrap().root().is_absolute());
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
