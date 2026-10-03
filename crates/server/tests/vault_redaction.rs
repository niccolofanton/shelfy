//! Captures the real log layer, metrics endpoint, CLI Debug and API problems.

mod support;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::Parser as _;
use secrecy::SecretString;
use shelfy_server::ai::vault::KeyVault;
use shelfy_server::cli::{Cli, Command};
use shelfy_server::config::Config;
use shelfy_server::control::provider_keys;
use shelfy_server::error::ApiError;
use shelfy_server::telemetry::{json_layer, metrics};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use support::{TestState, auth::owner, body, get, send};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;

const PROVIDER: &str = "synthetic-planted-provider-key-4352";
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl<'a> MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[tokio::test]
async fn provider_and_master_keys_never_leave_the_vault() {
    let capture = Capture::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(json_layer(capture.clone())),
    )
    .unwrap();
    let master = STANDARD.encode([73; 32]);
    let cli = Cli::try_parse_from(["shelfy-server", "serve", "--master-key", &master]).unwrap();
    let args_debug = format!("{cli:?}");
    let Command::Serve(args) = cli.command else {
        unreachable!()
    };
    let config_debug = format!("{:?}", Config::from_args(*args).unwrap());
    let t = TestState::with_config(|c| {
        c.vault = KeyVault::new(Some(SecretString::from(master.clone())), None).unwrap()
    });
    assert!(
        t.state.ai().is_configured(),
        "vault enables the BYOK capability"
    );
    let user = owner(&t);
    let sealed = t
        .state
        .vault()
        .seal(&user, "test", &SecretString::from(PROVIDER))
        .unwrap();
    t.state
        .control()
        .write(|tx| provider_keys::put(tx, &user, "test", &sealed, 42))
        .unwrap();
    let key = t
        .state
        .control()
        .read(|conn| provider_keys::get(conn, &user, "test"))
        .unwrap()
        .unwrap();
    let mut seen = format!("{args_debug}{config_debug}{:?}{key:?}", t.state);
    let error = t
        .state
        .vault()
        .open("another-user", "test", &key.sealed)
        .unwrap_err();
    seen.push_str(&serde_json::to_string(&ApiError::from(error).problem()).unwrap());
    let metrics_body =
        body(send(&metrics::router(metrics::install()), get("/metrics")).await).await;
    seen.push_str(std::str::from_utf8(&metrics_body).unwrap());
    seen.push_str(std::str::from_utf8(&capture.0.lock().unwrap()).unwrap());
    assert!(seen.contains("provider key vault"));
    assert!(seen.contains("key_version"));
    for secret in [PROVIDER, master.as_str()] {
        assert!(!seen.contains(secret));
    }

    for variable in ["--master-key", "--master-key-previous"] {
        let cli = Cli::try_parse_from(["shelfy-server", "serve", variable, PROVIDER]).unwrap();
        let Command::Serve(args) = cli.command else {
            unreachable!()
        };
        let error = Config::from_args(*args).unwrap_err();
        assert!(!format!("{error:?}: {error}").contains(PROVIDER));
    }
}
