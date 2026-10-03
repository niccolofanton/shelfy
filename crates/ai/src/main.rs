//! `shelfy-ai-stub`: the test provider as a process, for e2e runs, local
//! servers and P5's load run (§6.3 scenario C). See `shelfy_ai::stub`.
//!
//! It prints `shelfy-ai-stub listening on http://<addr>` once it accepts
//! connections, and stops on Ctrl-C or SIGTERM. The key, if any, comes from
//! an environment variable, never from the command line.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use secrecy::SecretString;
use shelfy_ai::stub::{Fault, FaultRule, Stub, StubConfig};

/// A deterministic AI provider for tests: OpenAI-compatible, Anthropic and
/// whisper.cpp endpoints, with fault and latency injection.
#[derive(Debug, Parser)]
#[command(name = "shelfy-ai-stub", version)]
struct Args {
    /// The address to listen on.
    #[arg(long, default_value = "127.0.0.1:18381")]
    listen: SocketAddr,
    /// The environment variable that holds the key requests must send.
    #[arg(long, value_name = "NAME")]
    key_env: Option<String>,
    /// The models `…/models` lists, comma-separated.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "stub-text,stub-vision,stub-embed"
    )]
    models: Vec<String>,
    /// A delay before every answer, in milliseconds.
    #[arg(long, default_value_t = 0)]
    latency_ms: u64,
    /// A delay between streamed chunks, in milliseconds.
    #[arg(long, default_value_t = 0)]
    chunk_delay_ms: u64,
    /// The size of embedding vectors.
    #[arg(long, default_value_t = 16)]
    embedding_dims: usize,
    /// A directory of recorded answers, `<key>.json`.
    #[arg(long)]
    recordings: Option<PathBuf>,
    /// A JSON file of canned answers: `{"<key>": "<text>"}`.
    #[arg(long)]
    canned: Option<PathBuf>,
    /// A fault for every request until cleared, by name (`rate_limited`,
    /// `server_error`, `timeout`, `empty_stream`…). Repeatable.
    #[arg(long = "fault", value_name = "NAME")]
    faults: Vec<String>,
    /// Answer WebP images with 400, as llama.cpp does (the owner's node).
    #[arg(long)]
    no_webp: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = Args::parse();
    let api_key = match &args.key_env {
        Some(name) => match std::env::var(name) {
            Ok(key) if !key.is_empty() => Some(SecretString::from(key)),
            _ => {
                eprintln!("shelfy-ai-stub: the environment variable {name} is empty or unset");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let canned: HashMap<String, String> = match &args.canned {
        Some(path) => match std::fs::read(path).map(|bytes| serde_json::from_slice(&bytes)) {
            Ok(Ok(canned)) => canned,
            _ => {
                eprintln!(
                    "shelfy-ai-stub: {} is not a JSON object of strings",
                    path.display()
                );
                return ExitCode::FAILURE;
            }
        },
        None => HashMap::new(),
    };
    let mut faults = Vec::new();
    for name in &args.faults {
        match Fault::from_name(name) {
            Some(fault) => faults.push(FaultRule::new(fault).always()),
            None => {
                eprintln!("shelfy-ai-stub: unknown fault {name}");
                return ExitCode::FAILURE;
            }
        }
    }
    let config = StubConfig {
        listen: args.listen,
        api_key,
        models: args.models,
        latency: Duration::from_millis(args.latency_ms),
        chunk_delay: Duration::from_millis(args.chunk_delay_ms),
        embedding_dims: args.embedding_dims,
        recordings: args.recordings,
        canned,
        faults,
        webp_images: !args.no_webp,
    };
    let stub = match Stub::start(config).await {
        Ok(stub) => stub,
        Err(error) => {
            eprintln!("shelfy-ai-stub: cannot listen on {}: {error}", args.listen);
            return ExitCode::FAILURE;
        }
    };
    println!("shelfy-ai-stub listening on {}", stub.url());
    stopped().await;
    stub.shutdown().await;
    ExitCode::SUCCESS
}

/// Waits for Ctrl-C or SIGTERM.
async fn stopped() {
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
