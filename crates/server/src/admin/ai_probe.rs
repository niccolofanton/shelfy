//! `shelfy-server admin ai-probe <target>` (plan §3.6, P3-09): a diagnostic
//! that checks the server can reach an AI endpoint.
//!
//! - `operator`: `GET /health`, then `GET /v1/models` with the operator key,
//!   at `SHELFY_OPERATOR_AI_URL` (allowlisted by `SHELFY_EGRESS_ALLOW_ORIGINS`).
//!   Prints the node's state and the model ids.
//! - a preset id (`openai`, `anthropic`, …) or an https base URL: a keyless
//!   `/models` call that is expected to be refused (401 or 403), as proof the
//!   egress path reaches the provider. It never sends a key.
//!
//! The key is never printed. The command needs the same operator and egress
//! environment as the server.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use shelfy_ai::{
    CallOptions, ErrorKind, Provider, ProviderConfig, ProviderKind, RetryPolicy, Source, Timeouts,
    Transport,
};
use url::Url;

use crate::ai::{OperatorArgs, OperatorConfig};
use crate::config::{DataDir, PublicUrl};
use crate::outbound::ai::AiTransport;
use crate::outbound::{Outbound, OutboundArgs, OutboundConfig};

/// Arguments of `admin ai-probe`.
#[derive(Debug, Args)]
pub struct ProbeArgs {
    /// What to probe: `operator`, a preset id, or an https base URL.
    pub target: String,

    #[command(flatten)]
    pub operator: Box<OperatorArgs>,

    #[command(flatten)]
    pub outbound: Box<OutboundArgs>,
}

/// Runs the probe, printing its findings to `out`.
///
/// # Errors
///
/// The settings are inconsistent, or the endpoint is a preset/URL that is not
/// valid. A node that is unreachable is reported, not an error.
pub fn run(_data: &DataDir, args: ProbeArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    // A placeholder public URL: the probe never serves the web app, and the
    // dev-egress settings (which need a loopback URL) are not used here.
    let public = PublicUrl::parse("http://localhost").expect("valid placeholder");
    let outbound_config =
        OutboundConfig::from_args(*args.outbound, &public).map_err(|e| anyhow::anyhow!(e))?;
    let allow = outbound_config.allow_origins.clone();
    let outbound = Outbound::new(&outbound_config).context("cannot build the outbound client")?;
    let transport: Arc<dyn Transport> = Arc::new(AiTransport::new(&outbound));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("cannot start a runtime")?;

    if args.target == "operator" {
        let operator =
            OperatorConfig::from_args(*args.operator, &allow).map_err(|e| anyhow::anyhow!(e))?;
        return runtime.block_on(probe_operator(&operator, &transport, out));
    }
    runtime.block_on(probe_public(&args.target, &transport, out))
}

/// A short, retry-free call policy for a probe.
fn options() -> CallOptions {
    CallOptions::new(Timeouts::new(
        Duration::from_secs(3),
        Duration::from_secs(15),
    ))
    .with_retry(RetryPolicy::NONE)
}

async fn probe_operator(
    config: &OperatorConfig,
    transport: &Arc<dyn Transport>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let Some(url) = config.url.clone() else {
        anyhow::bail!("SHELFY_OPERATOR_AI_URL is not set: the operator provider is off");
    };
    let mut policy = shelfy_ai::EgressPolicy::new();
    if let Ok(origin) = shelfy_ai::Origin::of(&url) {
        policy = policy.allow(origin);
    }
    let mut provider_config = ProviderConfig::new(
        ProviderKind::OpenAiCompatible,
        Source::Operator,
        url.clone(),
    )
    .with_llama_health()
    .without_webp();
    if let Some(key) = config.key.clone() {
        provider_config = provider_config.with_key(key);
    }
    let provider = Provider::new(provider_config, &policy, Arc::clone(transport))
        .context("the operator endpoint is refused by the egress guard")?;

    writeln!(out, "operator: {}", display_url(&url))?;
    match provider.health(&options()).await {
        Ok(()) => writeln!(out, "  health: ok")?,
        Err(error) => {
            writeln!(out, "  health: {} ({})", error.kind(), error.message())?;
            if error.kind() == ErrorKind::Offline {
                writeln!(
                    out,
                    "  state: offline (node asleep or unreachable); retry later"
                )?;
                return Ok(());
            }
        }
    }
    match provider.models(&options()).await {
        Ok(models) => {
            writeln!(out, "  state: ok")?;
            writeln!(out, "  models ({}):", models.len())?;
            for model in models {
                writeln!(out, "    - {}", model.id)?;
            }
        }
        Err(error) => writeln!(out, "  models: {} ({})", error.kind(), error.message())?,
    }
    Ok(())
}

async fn probe_public(
    target: &str,
    transport: &Arc<dyn Transport>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let base = match shelfy_ai::presets::preset(target) {
        Some(preset) => preset
            .base_url
            .map(|url| Url::parse(url).expect("a preset base URL is valid"))
            .with_context(|| format!("the preset {target:?} has no base URL; give an https URL"))?,
        None => Url::parse(target)
            .with_context(|| format!("{target:?} is not `operator`, a preset id or a URL"))?,
    };
    let policy = shelfy_ai::EgressPolicy::new();
    // No key: a correct endpoint refuses with 401 or 403, which proves the
    // egress path reaches it.
    let config = ProviderConfig::new(ProviderKind::OpenAiCompatible, Source::User, base.clone());
    let provider = Provider::new(config, &policy, Arc::clone(transport))
        .context("the URL is refused by the egress guard (https and public only)")?;
    writeln!(out, "egress probe: {}", display_url(&base))?;
    match provider.models(&options()).await {
        Ok(models) => writeln!(
            out,
            "  reachable: the provider listed {} models without a key",
            models.len()
        )?,
        Err(error) if matches!(error.kind(), ErrorKind::InvalidKey) => writeln!(
            out,
            "  reachable: the provider refused the keyless call ({}), as expected",
            error.status().unwrap_or(0)
        )?,
        Err(error) => writeln!(out, "  {}: {}", error.kind(), error.message())?,
    }
    Ok(())
}

/// A URL without its query or fragment, for printing.
fn display_url(url: &Url) -> String {
    let mut shown = url.clone();
    shown.set_query(None);
    shown.set_fragment(None);
    shown.to_string()
}
