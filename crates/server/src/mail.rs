//! Outgoing email through lettre (plan §2.11, §2.21): sign-in links now,
//! feedback later.
//!
//! Email is optional (E4). The configuration picks one transport:
//!
//! | Setting | Transport |
//! |---|---|
//! | `SHELFY_SMTP_HOST` (+ `SHELFY_SMTP_FROM`, credentials) | SMTP: STARTTLS by default, implicit TLS, or plain text for a local catcher (`SHELFY_SMTP_TLS`) |
//! | `SHELFY_DEV_MAILBOX=true` | the dev mailbox: every message becomes an `.eml` file in `<data>/dev-mailbox/`, for local runs and tests |
//! | neither | none: email is disabled; `admin login-link` is the way in |
//!
//! Setting both is a configuration error, so a production instance never
//! writes sign-in links to disk by accident.
//!
//! Addresses, subjects and bodies are never logged, and [`MailError`] never
//! carries them: an SMTP reply can quote the recipient, so only its status
//! code is kept.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::str::FromStr as _;
use std::time::Duration;

use clap::{Args, ValueEnum};
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{
    AsyncFileTransport, AsyncSmtpTransport, AsyncTransport as _, Message, Tokio1Executor,
};

use crate::config::{DataDir, create_private_dir};
use crate::telemetry::redact::Redacted;

/// The dev mailbox, inside the data directory.
pub const DEV_MAILBOX_DIR: &str = "dev-mailbox";

/// Sender of the dev mailbox when `SHELFY_SMTP_FROM` is not set.
pub const DEV_MAILBOX_FROM: &str = "Shelfy <shelfy@localhost>";

/// How long one SMTP command may take.
const SMTP_TIMEOUT: Duration = Duration::from_secs(20);

/// `SHELFY_SMTP_*` and `SHELFY_DEV_MAILBOX`, flattened into `serve`.
#[derive(Clone, Debug, Default, Args)]
pub struct MailArgs {
    /// SMTP relay for sign-in emails, as `host[:port]` (for example
    /// `smtp.resend.com:587`). Setting it turns email sign-in on. The port
    /// defaults to 587 (starttls), 465 (tls) or 25 (none).
    #[arg(
        long = "smtp-host",
        env = "SHELFY_SMTP_HOST",
        value_name = "HOST[:PORT]"
    )]
    pub smtp_host: Option<String>,

    /// How the SMTP connection is secured: `starttls` (required, not
    /// opportunistic), `tls` (implicit TLS) or `none` (plain text, for a local
    /// catcher only; refused together with credentials).
    #[arg(
        long = "smtp-tls",
        env = "SHELFY_SMTP_TLS",
        value_enum,
        default_value_t = SmtpTls::Starttls
    )]
    pub smtp_tls: SmtpTls,

    /// SMTP user name.
    #[arg(long = "smtp-user", env = "SHELFY_SMTP_USER", value_name = "USER")]
    pub smtp_user: Option<String>,

    /// SMTP password. Prefer the environment variable: flags show up in `ps`.
    #[arg(
        long = "smtp-password",
        env = "SHELFY_SMTP_PASSWORD",
        hide_env_values = true,
        value_name = "PASSWORD",
        value_parser = parse_secret
    )]
    pub smtp_password: Option<Redacted<String>>,

    /// Sender of the emails, as `address` or `Name <address>`. Required with
    /// SMTP.
    #[arg(long = "smtp-from", env = "SHELFY_SMTP_FROM", value_name = "MAILBOX")]
    pub smtp_from: Option<String>,

    /// Write emails as `.eml` files to `<data>/dev-mailbox/` instead of sending
    /// them. For local runs and tests; refused together with SMTP.
    #[arg(long = "dev-mailbox", env = "SHELFY_DEV_MAILBOX")]
    pub dev_mailbox: bool,
}

#[allow(clippy::unnecessary_wraps)] // clap's value parser signature
fn parse_secret(value: &str) -> Result<Redacted<String>, String> {
    Ok(Redacted(value.to_owned()))
}

/// `SHELFY_SMTP_TLS`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum SmtpTls {
    /// Plain connection upgraded with STARTTLS; fails if the server cannot.
    #[default]
    Starttls,
    /// TLS from the first byte (SMTPS).
    Tls,
    /// No encryption: only for a catcher on a trusted network.
    None,
}

impl SmtpTls {
    /// The usual port of this mode.
    #[must_use]
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Starttls => 587,
            Self::Tls => 465,
            Self::None => 25,
        }
    }
}

/// The validated email settings.
#[derive(Clone, Debug, Default)]
pub enum MailConfig {
    /// No transport: email is off.
    #[default]
    Disabled,
    /// Send through an SMTP relay.
    Smtp(SmtpConfig),
    /// Write `.eml` files to `dir`.
    DevMailbox {
        /// `<data>/dev-mailbox`.
        dir: PathBuf,
        /// Sender.
        from: Mailbox,
    },
}

/// An SMTP relay.
#[derive(Clone, Debug)]
pub struct SmtpConfig {
    /// Host name or IP address (no brackets).
    pub host: String,
    /// Port.
    pub port: u16,
    /// Connection security.
    pub tls: SmtpTls,
    /// User name and password.
    pub credentials: Option<(String, Redacted<String>)>,
    /// Sender.
    pub from: Mailbox,
}

impl MailConfig {
    /// Validates the arguments; `data` places the dev mailbox. Empty values
    /// count as unset, so a compose file may pass `SHELFY_SMTP_HOST=`.
    ///
    /// # Errors
    ///
    /// A message naming the variable at fault.
    pub fn from_args(args: MailArgs, data: &DataDir) -> Result<Self, String> {
        let host = non_empty(args.smtp_host);
        let from = non_empty(args.smtp_from);
        let user = non_empty(args.smtp_user);
        let password = args.smtp_password.filter(|p| !p.expose().is_empty());
        let Some(host) = host else {
            if !args.dev_mailbox {
                return Ok(Self::Disabled);
            }
            let from = parse_mailbox(from.as_deref().unwrap_or(DEV_MAILBOX_FROM))?;
            return Ok(Self::DevMailbox {
                dir: data.root().join(DEV_MAILBOX_DIR),
                from,
            });
        };
        if args.dev_mailbox {
            return Err(
                "SHELFY_SMTP_HOST and SHELFY_DEV_MAILBOX exclude each other: set one".to_owned(),
            );
        }
        let (host, port) = parse_host(&host, args.smtp_tls)?;
        let Some(from) = from else {
            return Err("SHELFY_SMTP_FROM is required with SHELFY_SMTP_HOST".to_owned());
        };
        let from = parse_mailbox(&from)?;
        let credentials = match (user, password) {
            (Some(user), Some(password)) => Some((user, password)),
            (None, None) => None,
            _ => {
                return Err(
                    "SHELFY_SMTP_USER and SHELFY_SMTP_PASSWORD must be set together".to_owned(),
                );
            }
        };
        if credentials.is_some() && args.smtp_tls == SmtpTls::None {
            return Err(
                "SHELFY_SMTP_TLS=none would send the SMTP password in clear text: use starttls or tls"
                    .to_owned(),
            );
        }
        Ok(Self::Smtp(SmtpConfig {
            host,
            port,
            tls: args.smtp_tls,
            credentials,
            from,
        }))
    }

    /// The dev mailbox of `data`, with the default sender (tests use it).
    #[must_use]
    pub fn dev_mailbox(data: &DataDir) -> Self {
        Self::DevMailbox {
            dir: data.root().join(DEV_MAILBOX_DIR),
            from: parse_mailbox(DEV_MAILBOX_FROM).expect("a valid default sender"),
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn parse_mailbox(value: &str) -> Result<Mailbox, String> {
    Mailbox::from_str(value.trim())
        .map_err(|_| "SHELFY_SMTP_FROM is not an address or `Name <address>`".to_owned())
}

/// `host[:port]`, `[v6]:port` or a bare host; the port defaults by TLS mode.
fn parse_host(value: &str, tls: SmtpTls) -> Result<(String, u16), String> {
    let invalid = || format!("SHELFY_SMTP_HOST: {value:?} is not host[:port]");
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or_else(invalid)?;
        match after {
            "" => (host, None),
            _ => (host, Some(after.strip_prefix(':').ok_or_else(invalid)?)),
        }
    } else {
        match value.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (value, None),
        }
    };
    let host_ok = !host.is_empty()
        && !host
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '/' | '@' | '[' | ']'))
        && (value.starts_with('[') || !host.contains(':'));
    if !host_ok {
        return Err(invalid());
    }
    let port = match port {
        None => tls.default_port(),
        Some(port) => port
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(invalid)?,
    };
    Ok((host.to_owned(), port))
}

/// What a [`Mailer`] sends through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    /// Email is off.
    Disabled,
    /// An SMTP relay.
    Smtp,
    /// `.eml` files in the dev mailbox.
    DevMailbox,
}

impl TransportKind {
    /// For logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Smtp => "smtp",
            Self::DevMailbox => "dev-mailbox",
        }
    }
}

enum Transport {
    Disabled,
    Smtp(AsyncSmtpTransport<Tokio1Executor>),
    File(AsyncFileTransport<Tokio1Executor>),
}

/// Sends email through the configured transport. Cheap to share behind the
/// application state.
pub struct Mailer {
    transport: Transport,
    from: Option<Mailbox>,
}

impl fmt::Debug for Mailer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mailer")
            .field("transport", &self.kind())
            .finish_non_exhaustive()
    }
}

/// A plain-text message.
#[derive(Clone, Debug)]
pub struct Email {
    /// Recipient address.
    pub to: Redacted<String>,
    /// Subject.
    pub subject: String,
    /// Body.
    pub text: Redacted<String>,
}

impl Mailer {
    /// The transport of `config`. Creates the dev mailbox directory; opens
    /// no connection (SMTP connects per message).
    ///
    /// # Errors
    ///
    /// The dev mailbox cannot be created, or the TLS settings cannot be built.
    pub fn new(config: &MailConfig) -> Result<Self, MailError> {
        let mailer = match config {
            MailConfig::Disabled => Self {
                transport: Transport::Disabled,
                from: None,
            },
            MailConfig::DevMailbox { dir, from } => {
                create_private_dir(dir).map_err(MailError::File)?;
                Self {
                    transport: Transport::File(AsyncFileTransport::new(dir)),
                    from: Some(from.clone()),
                }
            }
            MailConfig::Smtp(smtp) => Self {
                transport: Transport::Smtp(smtp_transport(smtp)?),
                from: Some(smtp.from.clone()),
            },
        };
        Ok(mailer)
    }

    /// A mailer that sends nothing.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            transport: Transport::Disabled,
            from: None,
        }
    }

    /// The transport in use.
    #[must_use]
    pub fn kind(&self) -> TransportKind {
        match self.transport {
            Transport::Disabled => TransportKind::Disabled,
            Transport::Smtp(_) => TransportKind::Smtp,
            Transport::File(_) => TransportKind::DevMailbox,
        }
    }

    /// Whether email can be sent.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.kind() != TransportKind::Disabled
    }

    /// Sends `email`.
    ///
    /// # Errors
    ///
    /// Email is off, the message cannot be built, or the transport failed.
    pub async fn send(&self, email: Email) -> Result<(), MailError> {
        let Some(from) = &self.from else {
            return Err(MailError::Disabled);
        };
        let to = Mailbox::from_str(email.to.expose()).map_err(|_| MailError::Address)?;
        let message = Message::builder()
            .from(from.clone())
            .to(to)
            .subject(email.subject)
            .header(ContentType::TEXT_PLAIN)
            .body(email.text.into_inner())
            .map_err(|_| MailError::Build)?;
        match &self.transport {
            Transport::Disabled => Err(MailError::Disabled),
            Transport::Smtp(smtp) => smtp
                .send(message)
                .await
                .map(drop)
                .map_err(|err| MailError::Smtp(SmtpFailure::of(&err))),
            Transport::File(file) => file
                .send(message)
                .await
                .map(drop)
                .map_err(|err| MailError::File(io::Error::other(err))),
        }
    }
}

fn smtp_transport(smtp: &SmtpConfig) -> Result<AsyncSmtpTransport<Tokio1Executor>, MailError> {
    let tls_parameters = || TlsParameters::new(smtp.host.clone()).map_err(|_| MailError::Tls);
    let tls = match smtp.tls {
        SmtpTls::Starttls => Tls::Required(tls_parameters()?),
        SmtpTls::Tls => Tls::Wrapper(tls_parameters()?),
        SmtpTls::None => Tls::None,
    };
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp.host.as_str())
        .port(smtp.port)
        .tls(tls)
        .timeout(Some(SMTP_TIMEOUT));
    if let Some((user, password)) = &smtp.credentials {
        builder = builder.credentials(Credentials::new(user.clone(), password.expose().clone()));
    }
    Ok(builder.build())
}

/// Why an email was not sent. Never carries an address or content.
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    /// No transport is configured.
    #[error("email is disabled")]
    Disabled,
    /// The recipient is not a valid address.
    #[error("the recipient address is not valid")]
    Address,
    /// The message could not be assembled.
    #[error("the message could not be built")]
    Build,
    /// The TLS settings for the SMTP host could not be built.
    #[error("the SMTP host is not a valid TLS server name")]
    Tls,
    /// The SMTP exchange failed.
    #[error("SMTP failed: {0}")]
    Smtp(SmtpFailure),
    /// Writing to the dev mailbox failed.
    #[error("the dev mailbox failed: {}", .0.kind())]
    File(io::Error),
}

/// An SMTP failure without the server's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmtpFailure {
    /// What failed.
    pub kind: &'static str,
    /// The reply code, when the server answered.
    pub code: Option<u16>,
}

impl SmtpFailure {
    fn of(err: &lettre::transport::smtp::Error) -> Self {
        let kind = if err.is_timeout() {
            "timeout"
        } else if err.is_tls() {
            "tls"
        } else if err.is_permanent() {
            "permanent"
        } else if err.is_transient() {
            "transient"
        } else if err.is_response() {
            "response"
        } else if err.is_client() {
            "client"
        } else {
            "connection"
        };
        let code = err
            .status()
            .and_then(|code| code.to_string().parse::<u16>().ok());
        Self { kind, code }
    }
}

impl fmt::Display for SmtpFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            Some(code) => write!(f, "{} (reply {code})", self.kind),
            None => f.write_str(self.kind),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> DataDir {
        DataDir::new("/srv/shelfy").unwrap()
    }

    fn args() -> MailArgs {
        MailArgs::default()
    }

    #[test]
    fn nothing_set_means_disabled() {
        let config = MailConfig::from_args(args(), &data()).unwrap();
        assert!(matches!(config, MailConfig::Disabled));
        // Empty values from a compose file count as unset; so does a lone sender.
        let config = MailConfig::from_args(
            MailArgs {
                smtp_host: Some(String::new()),
                smtp_from: Some("Shelfy <login@example.test>".into()),
                ..args()
            },
            &data(),
        )
        .unwrap();
        assert!(matches!(config, MailConfig::Disabled));
    }

    #[test]
    fn the_dev_mailbox_lives_in_the_data_directory() {
        let config = MailConfig::from_args(
            MailArgs {
                dev_mailbox: true,
                ..args()
            },
            &data(),
        )
        .unwrap();
        let MailConfig::DevMailbox { dir, from } = config else {
            panic!("expected the dev mailbox, got {config:?}");
        };
        assert_eq!(dir, PathBuf::from("/srv/shelfy/dev-mailbox"));
        assert_eq!(from.email.to_string(), "shelfy@localhost");
    }

    #[test]
    fn smtp_settings_are_validated() {
        let smtp = |edit: fn(&mut MailArgs)| {
            let mut args = MailArgs {
                smtp_host: Some("smtp.resend.com:587".into()),
                smtp_user: Some("resend".into()),
                smtp_password: Some(Redacted("re_secret".into())),
                smtp_from: Some("Shelfy <login@example.test>".into()),
                ..MailArgs::default()
            };
            edit(&mut args);
            MailConfig::from_args(args, &DataDir::new("/srv/shelfy").unwrap())
        };
        let MailConfig::Smtp(config) = smtp(|_| {}).unwrap() else {
            panic!("expected SMTP");
        };
        assert_eq!(
            (config.host.as_str(), config.port),
            ("smtp.resend.com", 587)
        );
        assert_eq!(config.tls, SmtpTls::Starttls);
        assert_eq!(config.from.email.to_string(), "login@example.test");
        assert!(
            !format!("{config:?}").contains("re_secret"),
            "Debug hides the password"
        );

        let MailConfig::Smtp(config) = smtp(|a| {
            a.smtp_host = Some("mail.example.test".into());
            a.smtp_tls = SmtpTls::Tls;
        })
        .unwrap() else {
            panic!("expected SMTP");
        };
        assert_eq!(config.port, 465, "the port defaults by TLS mode");

        for (edit, message) in [
            (
                (|a: &mut MailArgs| a.dev_mailbox = true) as fn(&mut MailArgs),
                "exclude each other",
            ),
            (|a| a.smtp_from = None, "SHELFY_SMTP_FROM is required"),
            (
                |a| a.smtp_from = Some("not a mailbox".into()),
                "SHELFY_SMTP_FROM",
            ),
            (|a| a.smtp_password = None, "must be set together"),
            (|a| a.smtp_tls = SmtpTls::None, "clear text"),
            (|a| a.smtp_host = Some("host:0".into()), "SHELFY_SMTP_HOST"),
            (
                |a| a.smtp_host = Some("host:smtp".into()),
                "SHELFY_SMTP_HOST",
            ),
            (|a| a.smtp_host = Some("a b:25".into()), "SHELFY_SMTP_HOST"),
            (|a| a.smtp_host = Some("[::1".into()), "SHELFY_SMTP_HOST"),
        ] {
            let err = smtp(edit).unwrap_err();
            assert!(err.contains(message), "{err}");
        }

        // A local catcher: plain text without credentials is fine.
        let MailConfig::Smtp(config) = smtp(|a| {
            a.smtp_host = Some("[::1]:1025".into());
            a.smtp_tls = SmtpTls::None;
            a.smtp_user = None;
            a.smtp_password = None;
        })
        .unwrap() else {
            panic!("expected SMTP");
        };
        assert_eq!((config.host.as_str(), config.port), ("::1", 1025));
        assert!(config.credentials.is_none());
    }

    #[test]
    fn hosts_parse_with_or_without_a_port() {
        assert_eq!(
            parse_host("mailpit", SmtpTls::None).unwrap(),
            ("mailpit".to_owned(), 25)
        );
        assert_eq!(
            parse_host("127.0.0.1:2525", SmtpTls::None).unwrap(),
            ("127.0.0.1".to_owned(), 2525)
        );
        assert_eq!(
            parse_host("[2001:db8::1]", SmtpTls::Tls).unwrap(),
            ("2001:db8::1".to_owned(), 465)
        );
        assert!(parse_host("2001:db8::1", SmtpTls::Tls).is_err());
        assert!(parse_host("smtp://host", SmtpTls::Tls).is_err());
        assert!(parse_host("user@host", SmtpTls::Tls).is_err());
    }

    #[tokio::test]
    async fn a_disabled_mailer_sends_nothing() {
        let mailer = Mailer::disabled();
        assert!(!mailer.is_enabled());
        let err = mailer
            .send(Email {
                to: Redacted("owner@example.test".into()),
                subject: "s".into(),
                text: Redacted("t".into()),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MailError::Disabled));
    }

    #[tokio::test]
    async fn the_dev_mailbox_writes_one_file_per_message() {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path()).unwrap();
        let mailer = Mailer::new(&MailConfig::dev_mailbox(&data)).unwrap();
        assert_eq!(mailer.kind(), TransportKind::DevMailbox);
        mailer
            .send(Email {
                to: Redacted("owner@example.test".into()),
                subject: "Hello".into(),
                text: Redacted("Body line".into()),
            })
            .await
            .unwrap();
        let files: Vec<_> = std::fs::read_dir(data.root().join(DEV_MAILBOX_DIR))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].extension().unwrap(), "eml");
        let text = std::fs::read_to_string(&files[0]).unwrap();
        assert!(text.contains("To: owner@example.test"), "{text}");
        assert!(text.contains("Subject: Hello"), "{text}");
        assert!(text.contains("Body line"), "{text}");

        let err = mailer
            .send(Email {
                to: Redacted("not an address".into()),
                subject: "s".into(),
                text: Redacted("t".into()),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MailError::Address));
        assert!(!err.to_string().contains("not an address"));
    }
}
