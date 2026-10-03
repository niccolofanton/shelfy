//! The operator provider's settings, from the environment (plan §2.15, L15,
//! L16, P3-09).
//!
//! The owner's own AI node is an OpenAI-compatible server (`ornith-1.5-35b-a3b`
//! for text, `qwen3.8-27b` for vision) and a whisper.cpp server, reached over
//! Tailscale. The server defines it entirely from these variables; its key is
//! a [`SecretString`] that never reaches a client, a log or a vault. Its
//! endpoints are reached directly, even at a private address, only because
//! they are in `SHELFY_EGRESS_ALLOW_ORIGINS` (checked at start): a user can
//! never enter them.
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `SHELFY_OPERATOR_AI_URL` | none | the OpenAI-compatible base URL (`http://<node>:8080/v1`); unset turns the operator provider off |
//! | `SHELFY_OPERATOR_AI_KEY` | none | the Bearer key; a secret |
//! | `SHELFY_OPERATOR_AI_MODEL` | none | the text model (`ornith-1.5-35b-a3b`); required with the URL |
//! | `SHELFY_OPERATOR_AI_VISION_MODEL` | none | the vision model (`qwen3.8-27b`); without it cataloging and QC have no route |
//! | `SHELFY_OPERATOR_AI_EMBED_MODEL` | none | the embedding model; optional |
//! | `SHELFY_OPERATOR_AI_LABEL` | `Operator node` | the display name |
//! | `SHELFY_OPERATOR_AI_CONCURRENCY` | `1` | calls in flight to the node, 1 or 2 (L19) |
//! | `SHELFY_OPERATOR_AI_TIMEOUT` | `60` | seconds per call; raise well above 60 for cataloging (qwen runs ~5 tok/s) |
//! | `SHELFY_OPERATOR_STT_URL` | none | the whisper.cpp `/inference` URL; without it dictation has no route |
//! | `SHELFY_OPERATOR_STT_KEY` | none | the whisper.cpp Bearer key; a secret |

use std::fmt;
use std::time::Duration;

use clap::Args;
use shelfy_ai::secrecy::SecretString;
use url::Url;

use crate::outbound::{Origin, OriginAllowlist};

/// Default of `SHELFY_OPERATOR_AI_LABEL`.
pub const DEFAULT_LABEL: &str = "Operator node";
/// Default of `SHELFY_OPERATOR_AI_CONCURRENCY`.
pub const DEFAULT_CONCURRENCY: u8 = 1;
/// Default of `SHELFY_OPERATOR_AI_TIMEOUT`, in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// The connect timeout for operator calls: a sleeping node is `offline`
/// quickly (G3-29).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// The provider id of the operator provider, everywhere it is named.
pub const OPERATOR_PROVIDER_ID: &str = "operator";

/// `--operator-ai-*` / `SHELFY_OPERATOR_*`, flattened into `serve`.
#[derive(Clone, Debug, Args)]
pub struct OperatorArgs {
    /// The operator node's OpenAI-compatible base URL, used verbatim plus the
    /// path suffix (`http://<node>:8080/v1`). Its origin must be in
    /// `SHELFY_EGRESS_ALLOW_ORIGINS`. Unset: the operator provider is off.
    #[arg(
        long = "operator-ai-url",
        env = "SHELFY_OPERATOR_AI_URL",
        value_name = "URL"
    )]
    pub url: Option<String>,

    /// The node's Bearer key. A secret; never logged or returned.
    #[arg(
        long = "operator-ai-key",
        env = "SHELFY_OPERATOR_AI_KEY",
        value_name = "KEY"
    )]
    pub key: Option<String>,

    /// The text model id (`ornith-1.5-35b-a3b`). Required when the URL is set.
    #[arg(
        long = "operator-ai-model",
        env = "SHELFY_OPERATOR_AI_MODEL",
        value_name = "MODEL"
    )]
    pub model: Option<String>,

    /// The vision model id (`qwen3.8-27b`). Without it, cataloging and
    /// screenshot QC have no operator route (Q1).
    #[arg(
        long = "operator-ai-vision-model",
        env = "SHELFY_OPERATOR_AI_VISION_MODEL",
        value_name = "MODEL"
    )]
    pub vision_model: Option<String>,

    /// The embedding model id. Optional.
    #[arg(
        long = "operator-ai-embed-model",
        env = "SHELFY_OPERATOR_AI_EMBED_MODEL",
        value_name = "MODEL"
    )]
    pub embed_model: Option<String>,

    /// The display name shown in `GET /me/providers`.
    #[arg(
        long = "operator-ai-label",
        env = "SHELFY_OPERATOR_AI_LABEL",
        value_name = "LABEL",
        default_value = DEFAULT_LABEL
    )]
    pub label: String,

    /// Calls in flight to the node, 1 or 2 (L19): the node runs each model
    /// with `--parallel 1`, so 1 leaves a slot for Hermes.
    #[arg(
        long = "operator-ai-concurrency",
        env = "SHELFY_OPERATOR_AI_CONCURRENCY",
        value_name = "N",
        default_value_t = DEFAULT_CONCURRENCY,
        value_parser = clap::value_parser!(u8).range(1..=2)
    )]
    pub concurrency: u8,

    /// Seconds per call. The default 60 is low for cataloging: qwen runs about
    /// 5 tok/s, so a 768-token answer takes ~150 s. Raise it with SPIKE-6.
    #[arg(
        long = "operator-ai-timeout",
        env = "SHELFY_OPERATOR_AI_TIMEOUT",
        value_name = "SECONDS",
        default_value_t = DEFAULT_TIMEOUT_SECS,
        value_parser = clap::value_parser!(u64).range(1..=3600)
    )]
    pub timeout_secs: u64,

    /// The whisper.cpp `/inference` URL. Its origin must be in
    /// `SHELFY_EGRESS_ALLOW_ORIGINS`. Without it, dictation has no operator
    /// route.
    #[arg(
        long = "operator-stt-url",
        env = "SHELFY_OPERATOR_STT_URL",
        value_name = "URL"
    )]
    pub stt_url: Option<String>,

    /// The whisper.cpp Bearer key. A secret.
    #[arg(
        long = "operator-stt-key",
        env = "SHELFY_OPERATOR_STT_KEY",
        value_name = "KEY"
    )]
    pub stt_key: Option<String>,
}

/// The validated operator provider settings. `None` fields mean the operator
/// provider, or one of its optional abilities, is off.
#[derive(Clone)]
pub struct OperatorConfig {
    /// The OpenAI-compatible base URL. `None` turns the operator provider off.
    pub url: Option<Url>,
    /// The node's key.
    pub key: Option<SecretString>,
    /// The text model id.
    pub model: Option<String>,
    /// The vision model id.
    pub vision_model: Option<String>,
    /// The embedding model id.
    pub embed_model: Option<String>,
    /// The display name.
    pub label: String,
    /// Calls in flight to the node (1 or 2).
    pub concurrency: u8,
    /// The per-call timeout.
    pub timeout: Duration,
    /// The whisper.cpp `/inference` URL.
    pub stt_url: Option<Url>,
    /// The whisper.cpp key.
    pub stt_key: Option<SecretString>,
}

impl Default for OperatorConfig {
    fn default() -> Self {
        Self {
            url: None,
            key: None,
            model: None,
            vision_model: None,
            embed_model: None,
            label: DEFAULT_LABEL.to_owned(),
            concurrency: DEFAULT_CONCURRENCY,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            stt_url: None,
            stt_key: None,
        }
    }
}

impl OperatorConfig {
    /// Validates the operator arguments against the egress allowlist.
    ///
    /// The AI and STT endpoints must be among `SHELFY_EGRESS_ALLOW_ORIGINS`,
    /// so the outbound client reaches them (`Purpose::AiOperator`); otherwise
    /// the start stops with a message naming the variable.
    ///
    /// # Errors
    ///
    /// A message naming the variable and what is wrong. It never echoes the
    /// key.
    pub fn from_args(args: OperatorArgs, allow: &OriginAllowlist) -> Result<Self, String> {
        let url = parse_endpoint(args.url.as_deref(), "SHELFY_OPERATOR_AI_URL", allow)?;
        let stt_url = parse_endpoint(args.stt_url.as_deref(), "SHELFY_OPERATOR_STT_URL", allow)?;
        let key = secret(args.key);
        let stt_key = secret(args.stt_key);
        let model = non_empty(args.model);
        if url.is_some() && model.is_none() {
            return Err(
                "SHELFY_OPERATOR_AI_MODEL is required when SHELFY_OPERATOR_AI_URL is set"
                    .to_owned(),
            );
        }
        if url.is_none() {
            // The AI URL is the operator provider: without it, STT alone is
            // not an operator provider (it has nothing to route tasks through).
            if stt_url.is_some() {
                return Err(
                    "SHELFY_OPERATOR_STT_URL needs SHELFY_OPERATOR_AI_URL: the operator \
                     provider is defined by its AI endpoint"
                        .to_owned(),
                );
            }
            if key.is_some() || model.is_some() {
                tracing::warn!(
                    "operator AI settings are set without SHELFY_OPERATOR_AI_URL; the operator \
                     provider is off"
                );
            }
        }
        Ok(Self {
            url,
            key,
            model,
            vision_model: non_empty(args.vision_model),
            embed_model: non_empty(args.embed_model),
            label: non_empty(Some(args.label)).unwrap_or_else(|| DEFAULT_LABEL.to_owned()),
            concurrency: args.concurrency.clamp(1, 2),
            timeout: Duration::from_secs(args.timeout_secs.max(1)),
            stt_url,
            stt_key,
        })
    }

    /// Whether the operator provider is configured (its AI URL is set).
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.url.is_some()
    }

    /// The exact origins the operator reaches: its AI and STT endpoints. The
    /// egress policy and the allowlist must both include them.
    #[must_use]
    pub fn origins(&self) -> Vec<Origin> {
        [self.url.as_ref(), self.stt_url.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(Origin::of)
            .collect()
    }
}

impl fmt::Debug for OperatorConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperatorConfig")
            .field("url", &self.url.as_ref().map(Url::as_str))
            .field("key", &self.key.as_ref().map(|_| "[redacted]"))
            .field("model", &self.model)
            .field("vision_model", &self.vision_model)
            .field("embed_model", &self.embed_model)
            .field("label", &self.label)
            .field("concurrency", &self.concurrency)
            .field("timeout", &self.timeout)
            .field("stt_url", &self.stt_url.as_ref().map(Url::as_str))
            .field("stt_key", &self.stt_key.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// Parses an optional endpoint URL and checks its origin is allowlisted.
fn parse_endpoint(
    value: Option<&str>,
    var: &str,
    allow: &OriginAllowlist,
) -> Result<Option<Url>, String> {
    let Some(text) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let url = Url::parse(text).map_err(|e| format!("{var}: {text:?} is not a URL ({e})"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{var}: the scheme must be http or https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{var}: credentials in the URL are not allowed"));
    }
    let origin = Origin::of(&url).ok_or_else(|| format!("{var}: not a valid origin"))?;
    if !allow.contains(&origin) {
        return Err(format!(
            "{var}: the origin {origin} must be in SHELFY_EGRESS_ALLOW_ORIGINS, so the \
             server may reach it directly"
        ));
    }
    Ok(Some(url))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn secret(value: Option<String>) -> Option<SecretString> {
    value
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .map(SecretString::from)
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use shelfy_ai::secrecy::ExposeSecret as _;

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        operator: OperatorArgs,
    }

    fn args(argv: &[&str]) -> OperatorArgs {
        let mut full = vec!["shelfy"];
        full.extend_from_slice(argv);
        Cli::try_parse_from(full).expect("valid arguments").operator
    }

    fn allow(list: &str) -> OriginAllowlist {
        OriginAllowlist::parse(list).unwrap()
    }

    #[test]
    fn unset_is_off() {
        let config = OperatorConfig::from_args(args(&[]), &OriginAllowlist::default()).unwrap();
        assert!(!config.is_configured());
        assert_eq!(config.label, DEFAULT_LABEL);
        assert_eq!(config.concurrency, 1);
        assert_eq!(config.timeout, Duration::from_secs(60));
    }

    #[test]
    fn a_full_operator_parses_and_keeps_its_key_secret() {
        let config = OperatorConfig::from_args(
            args(&[
                "--operator-ai-url",
                "http://100.94.10.20:8080/v1",
                "--operator-ai-key",
                "node-secret-key",
                "--operator-ai-model",
                "ornith-1.5-35b-a3b",
                "--operator-ai-vision-model",
                "qwen3.8-27b",
                "--operator-ai-concurrency",
                "2",
                "--operator-ai-timeout",
                "180",
                "--operator-stt-url",
                "http://100.94.10.20:8178/inference",
                "--operator-stt-key",
                "stt-secret",
            ]),
            &allow("http://100.94.10.20:8080,http://100.94.10.20:8178"),
        )
        .unwrap();
        assert!(config.is_configured());
        assert_eq!(config.model.as_deref(), Some("ornith-1.5-35b-a3b"));
        assert_eq!(config.vision_model.as_deref(), Some("qwen3.8-27b"));
        assert_eq!(config.concurrency, 2);
        assert_eq!(config.timeout, Duration::from_secs(180));
        assert_eq!(
            config.key.as_ref().unwrap().expose_secret(),
            "node-secret-key"
        );
        // The key never shows in Debug.
        assert!(!format!("{config:?}").contains("node-secret-key"));
        assert!(!format!("{config:?}").contains("stt-secret"));
        assert_eq!(config.origins().len(), 2);
    }

    #[test]
    fn the_url_needs_a_model_and_an_allowlisted_origin() {
        let no_model = OperatorConfig::from_args(
            args(&["--operator-ai-url", "http://100.94.10.20:8080/v1"]),
            &allow("http://100.94.10.20:8080"),
        );
        assert!(no_model.unwrap_err().contains("SHELFY_OPERATOR_AI_MODEL"));

        let not_allowed = OperatorConfig::from_args(
            args(&[
                "--operator-ai-url",
                "http://100.94.10.20:8080/v1",
                "--operator-ai-model",
                "m",
            ]),
            &OriginAllowlist::default(),
        );
        assert!(
            not_allowed
                .unwrap_err()
                .contains("SHELFY_EGRESS_ALLOW_ORIGINS")
        );
    }

    #[test]
    fn stt_alone_is_refused() {
        let err = OperatorConfig::from_args(
            args(&["--operator-stt-url", "http://100.94.10.20:8178/inference"]),
            &allow("http://100.94.10.20:8178"),
        )
        .unwrap_err();
        assert!(err.contains("SHELFY_OPERATOR_AI_URL"), "{err}");
    }
}
