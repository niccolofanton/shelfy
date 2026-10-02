//! Logs and metrics (plan §3.6, §3.7).
//!
//! - **Logs:** `tracing` events as JSON lines on stdout. Every request runs in
//!   a `request` span carrying its id, method and route template; the
//!   response is logged with status and latency ([`http`]). URLs, query
//!   strings, headers and bodies are never logged; values that must not leak
//!   travel in [`redact::Redacted`]. Libraries that log secrets at debug or
//!   trace level ([`SECRET_TARGETS`]) are held at WARN, whatever `RUST_LOG`
//!   says.
//! - **Metrics:** Prometheus text on a separate listener ([`metrics`]), with
//!   no per-user labels.

pub mod http;
pub mod metrics;
pub mod redact;

use std::io::IsTerminal as _;

use anyhow::Context as _;
use tracing::level_filters::LevelFilter;
use tracing::{Level, Metadata, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{FilterFn, filter_fn};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::config::LogFormat;

/// Targets (crate prefixes) whose events below WARN are dropped, whatever
/// `RUST_LOG` says: webauthn-rs logs ceremony states (challenges, credential
/// ids, public keys) at debug and trace level.
pub const SECRET_TARGETS: &[&str] = &["webauthn_rs", "webauthn_attestation_ca"];

/// Whether an event or span of `metadata` may be logged: everything but the
/// debug and trace output of [`SECRET_TARGETS`].
fn not_secret(metadata: &Metadata<'_>) -> bool {
    *metadata.level() <= Level::WARN
        || !SECRET_TARGETS
            .iter()
            .any(|target| metadata.target().starts_with(target))
}

/// The per-layer filter of [`not_secret`].
fn secret_filter() -> FilterFn<fn(&Metadata<'_>) -> bool> {
    filter_fn(not_secret as fn(&Metadata<'_>) -> bool)
}

/// Installs the global subscriber of `serve`: `format` on stdout, filtered by
/// `RUST_LOG` (default `info`).
///
/// # Errors
///
/// `RUST_LOG` is not a valid filter, or a subscriber is already installed.
pub fn init(format: LogFormat) -> anyhow::Result<()> {
    let filter = env_filter(LevelFilter::INFO)?;
    let registry = tracing_subscriber::registry().with(filter);
    match format {
        LogFormat::Json => registry.with(json_layer(std::io::stdout)).try_init(),
        LogFormat::Text => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(std::io::stdout().is_terminal())
                    .with_writer(std::io::stdout)
                    .with_filter(secret_filter()),
            )
            .try_init(),
    }
    .context("cannot install the log subscriber")
}

/// Installs the subscriber of the admin commands: human-readable warnings on
/// stderr, so stdout carries only the command's own output.
///
/// # Errors
///
/// `RUST_LOG` is not a valid filter, or a subscriber is already installed.
pub fn init_for_cli() -> anyhow::Result<()> {
    let filter = env_filter(LevelFilter::WARN)?;
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(std::io::stderr().is_terminal())
                .with_writer(std::io::stderr),
        )
        .try_init()
        .context("cannot install the log subscriber")
}

/// The JSON log format: one object per event with `timestamp`, `level`,
/// `target`, the event's fields at the top level and the current span's
/// fields under `span` (request id, method, route, user). The debug and
/// trace output of [`SECRET_TARGETS`] never gets through.
pub fn json_layer<S, W>(writer: W) -> impl Layer<S> + Send + Sync + 'static
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_writer(writer)
        .with_filter(secret_filter())
}

fn env_filter(default: LevelFilter) -> anyhow::Result<EnvFilter> {
    EnvFilter::builder()
        .with_default_directive(default.into())
        .from_env()
        .context("RUST_LOG is not a valid log filter")
}

#[cfg(test)]
mod tests {
    use tracing::callsite::Identifier;
    use tracing::field::FieldSet;
    use tracing::metadata::Kind;

    use super::*;

    struct Callsite;

    impl tracing::Callsite for Callsite {
        fn set_interest(&self, _: tracing::subscriber::Interest) {}

        fn metadata(&self) -> &Metadata<'_> {
            unreachable!("not registered")
        }
    }

    static CALLSITE: Callsite = Callsite;

    fn metadata(target: &'static str, level: Level) -> Metadata<'static> {
        Metadata::new(
            "event",
            target,
            level,
            None,
            None,
            None,
            FieldSet::new(&[], Identifier(&CALLSITE)),
            Kind::EVENT,
        )
    }

    #[test]
    fn secret_targets_are_held_at_warn() {
        for target in ["webauthn_rs_core::core", "webauthn_rs", "webauthn_rs_proto"] {
            assert!(not_secret(&metadata(target, Level::ERROR)), "{target}");
            assert!(not_secret(&metadata(target, Level::WARN)), "{target}");
            for level in [Level::INFO, Level::DEBUG, Level::TRACE] {
                assert!(!not_secret(&metadata(target, level)), "{target} {level}");
            }
        }
        for level in [Level::INFO, Level::DEBUG, Level::TRACE] {
            assert!(not_secret(&metadata("shelfy_server::auth", level)));
        }
    }
}
