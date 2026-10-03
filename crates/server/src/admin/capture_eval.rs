//! Operator corpus probe: temporary artifacts, no user, aggregate-only output.
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::Args;
use serde::Serialize;

use crate::capture::{self, Options, protocol::Line};
use crate::config::{Config, DataDir, PublicUrl};
use crate::outbound::{OutboundArgs, OutboundConfig};
use crate::state::AppState;

#[derive(Debug, Args)]
pub struct EvalArgs {
    /// UTF-8 JSON array of public http(s) URLs, or one URL per line.
    #[arg(long)]
    pub corpus: PathBuf,
    /// Save the aggregate JSON report here as well as stdout.
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[command(flatten)]
    pub outbound: Box<OutboundArgs>,
    #[command(flatten)]
    pub capture: capture::CaptureArgs,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    attempted: usize,
    done: usize,
    blocked: usize,
    failed: usize,
    pages: usize,
    duration_ms: f64,
    peak_rss_bytes: f64,
    bytes: u64,
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn run(data: &DataDir, args: &EvalArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let meta =
        std::fs::metadata(&args.corpus).map_err(|_| anyhow::anyhow!("corpus unavailable"))?;
    anyhow::ensure!(
        meta.is_file() && meta.len() <= 1024 * 1024,
        "corpus over cap or not a file"
    );
    let bytes = std::fs::read(&args.corpus).map_err(|_| anyhow::anyhow!("corpus unavailable"))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| anyhow::anyhow!("corpus is not UTF-8"))?;
    let urls: Vec<String> = if text.trim_start().starts_with('[') {
        serde_json::from_str(text).map_err(|_| anyhow::anyhow!("invalid corpus JSON"))?
    } else {
        text.lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };
    anyhow::ensure!(
        !urls.is_empty() && urls.len() <= 1000,
        "corpus population must be 1–1000"
    );
    let urls = urls
        .into_iter()
        .map(|url| capture::validate_url(&url).map_err(|_| anyhow::anyhow!("invalid corpus URL")))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut config = Config::with_data_dir(data.clone());
    config.outbound = OutboundConfig::from_args(
        (*args.outbound).clone(),
        &PublicUrl::parse("http://localhost:8080").expect("static origin"),
    )
    .map_err(|_| anyhow::anyhow!("invalid capture egress configuration"))?;
    config.capture = args.capture.clone().into();
    anyhow::ensure!(
        config.outbound.capture.is_some() && config.capture.internal_token.is_some(),
        "capture must be configured"
    );
    let rt = crate::serve::runtime()?;
    let report = rt.block_on(async move {
        let state = tokio::task::spawn_blocking(move || AppState::open(config))
            .await
            .map_err(|_| anyhow::anyhow!("capture probe startup failed"))?
            .map_err(|_| anyhow::anyhow!("capture probe startup failed"))?;
        let root = capture::work_root(&state);
        crate::config::create_private_dir(&root)?;
        let mut report = Report::default();
        for url in urls {
            let id = crate::ids::new_ulid();
            let path = root.join(&id);
            std::fs::create_dir(&path)?;
            let cleanup = Cleanup(path.clone());
            let observed = Arc::new(Mutex::new((0.0_f64, 0.0_f64)));
            let callback = observed.clone();
            let result = capture::client::run(
                &state,
                &id,
                &url,
                Options::default(),
                state.shutdown_token(),
                move |line| {
                    if let Line::Done {
                        duration_ms,
                        peak_rss_bytes,
                        ..
                    } = line
                    {
                        *callback
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = (
                            duration_ms.clamp(0.0, 660_000.0),
                            peak_rss_bytes.clamp(0.0, 2.0 * 1024.0 * 1024.0 * 1024.0),
                        );
                    }
                    std::future::ready(())
                },
            )
            .await;
            report.attempted += 1;
            if let Ok(blocked) = result {
                let p = path.clone();
                if let Ok(Ok(validated)) = tokio::task::spawn_blocking(move || {
                    capture::ingest::validate(&p, &url, Options::default(), blocked)
                })
                .await
                {
                    if blocked {
                        report.blocked += 1;
                    } else {
                        report.done += 1;
                    }
                    report.pages += validated.manifest["pages"].as_array().map_or(0, Vec::len);
                    report.bytes += validated.bytes;
                } else {
                    report.failed += 1;
                }
            } else {
                report.failed += 1;
            }
            let stats = *observed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            report.duration_ms += stats.0;
            report.peak_rss_bytes = report.peak_rss_bytes.max(stats.1);
            drop(cleanup);
        }
        Ok::<_, anyhow::Error>(report)
    })?;
    let json = serde_json::to_string(&report)?;
    if let Some(path) = &args.out {
        std::fs::write(path, format!("{json}\n"))?;
    }
    writeln!(out, "{json}")?;
    Ok(())
}
