//! Running the server: the tokio runtime, the two listeners and the graceful
//! shutdown (plan §2.3).
//!
//! - **Runtime:** multi-thread with 2 async workers and at most 16 blocking
//!   threads. Every SQLite call runs on the blocking pool.
//! - **Listeners:** the API (`SHELFY_LISTEN_ADDR`) and Prometheus
//!   (`SHELFY_METRICS_ADDR`), served side by side.
//! - **Shutdown** on SIGTERM or Ctrl-C: stop accepting, cancel the shutdown
//!   token (job workers and streams stop), let in-flight requests finish,
//!   checkpoint the WAL and close the databases, all within
//!   [`Config::shutdown_grace`] (25 s). Interrupted jobs are re-queued on the
//!   next boot (P1-07).

use std::future::{Future, IntoFuture as _};
use std::io;
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use axum::Router;
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use crate::app;
use crate::config::{Config, ServeArgs};
use crate::state::AppState;
use crate::telemetry;
use crate::telemetry::metrics::UPKEEP_INTERVAL;

/// Async worker threads (§2.3): the API is I/O-bound and the host's 4 shared
/// vCPUs also run Hermes and capture.
pub const WORKER_THREADS: usize = 2;
/// Upper bound of the blocking pool, where SQLite and file work run (§2.3).
pub const MAX_BLOCKING_THREADS: usize = 16;

/// How often idle user databases are evicted, idle readers closed and the
/// event buses swept.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30);
/// Part of the shutdown grace kept for closing the databases after draining.
const CLOSE_RESERVE: Duration = Duration::from_secs(5);
/// How long the runtime waits for blocking tasks once the server returned.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// The runtime of §2.3.
///
/// # Errors
///
/// The operating system refused to start the threads.
pub fn runtime() -> io::Result<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .max_blocking_threads(MAX_BLOCKING_THREADS)
        .thread_name("shelfy")
        .enable_all()
        .build()
}

/// `shelfy-server serve`: validates the configuration, starts logging and runs
/// the server until SIGTERM or Ctrl-C.
///
/// # Errors
///
/// Invalid configuration, a listener that cannot bind, a database that cannot
/// open, or a server failure.
pub fn run(args: ServeArgs) -> anyhow::Result<()> {
    let config = Config::from_args(args)?;
    telemetry::init(config.log_format)?;
    let runtime = runtime().context("cannot start the tokio runtime")?;
    let result = runtime.block_on(async move {
        let server = Server::bind(config).await?;
        server.run(shutdown_signal()).await
    });
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    match &result {
        Ok(()) => tracing::info!("stopped"),
        Err(err) => tracing::error!(error = %format!("{err:#}"), "server failed"),
    }
    result
}

/// Resolves on the first SIGTERM (from `docker stop`) or Ctrl-C.
///
/// # Panics
///
/// When the signal handlers cannot be installed.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("cannot listen for Ctrl-C");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("cannot listen for SIGTERM")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

/// A server ready to run: the state, the application and both listeners.
pub struct Server {
    state: AppState,
    app: Router,
    api_listener: TcpListener,
    metrics_listener: TcpListener,
    metrics: PrometheusHandle,
}

impl Server {
    /// Opens the state (creating the data layout and the control database)
    /// and binds both listeners of `config`.
    ///
    /// # Errors
    ///
    /// The state cannot open or a listener cannot bind.
    pub async fn bind(config: Config) -> anyhow::Result<Self> {
        let metrics = telemetry::metrics::install();
        let (listen, metrics_listen) = (config.listen, config.metrics_listen);
        let state = tokio::task::spawn_blocking(move || AppState::open(config))
            .await
            .context("the startup task failed")??;
        let api_listener = TcpListener::bind(listen)
            .await
            .with_context(|| format!("cannot listen on {listen} (SHELFY_LISTEN_ADDR)"))?;
        let metrics_listener = TcpListener::bind(metrics_listen)
            .await
            .with_context(|| format!("cannot listen on {metrics_listen} (SHELFY_METRICS_ADDR)"))?;
        let app = app::app(state.clone());
        Ok(Self::new(
            state,
            app,
            api_listener,
            metrics_listener,
            metrics,
        ))
    }

    /// A server from parts; tests pass their own application and listeners.
    #[must_use]
    pub fn new(
        state: AppState,
        app: Router,
        api_listener: TcpListener,
        metrics_listener: TcpListener,
        metrics: PrometheusHandle,
    ) -> Self {
        Self {
            state,
            app,
            api_listener,
            metrics_listener,
            metrics,
        }
    }

    /// Address of the API listener.
    ///
    /// # Errors
    ///
    /// The socket cannot report its address.
    pub fn api_addr(&self) -> io::Result<SocketAddr> {
        self.api_listener.local_addr()
    }

    /// Address of the metrics listener.
    ///
    /// # Errors
    ///
    /// The socket cannot report its address.
    pub fn metrics_addr(&self) -> io::Result<SocketAddr> {
        self.metrics_listener.local_addr()
    }

    /// Serves until `shutdown` resolves, then shuts down gracefully.
    ///
    /// # Errors
    ///
    /// A listener failed.
    pub async fn run(self, shutdown: impl Future<Output = ()> + Send) -> anyhow::Result<()> {
        let Self {
            state,
            app,
            api_listener,
            metrics_listener,
            metrics,
        } = self;
        let token = state.shutdown_token().clone();
        let config = state.config();
        tracing::info!(
            version = crate::VERSION,
            api = %api_listener.local_addr()?,
            metrics = %metrics_listener.local_addr()?,
            data_dir = %config.data_dir.root().display(),
            public_url = %config.public_url,
            "listening"
        );
        let drain_timeout = config.shutdown_grace.saturating_sub(CLOSE_RESERVE);

        let maintenance = tokio::spawn(maintenance(state.clone(), metrics.clone(), token.clone()));
        // P1-07: start the job scheduler here with `token.child_token()`.

        let served = {
            let api = axum::serve(api_listener, app)
                .with_graceful_shutdown(token.clone().cancelled_owned())
                .into_future();
            let metrics = axum::serve(metrics_listener, telemetry::metrics::router(metrics))
                .with_graceful_shutdown(token.clone().cancelled_owned())
                .into_future();
            let mut servers = pin!(async { tokio::try_join!(api, metrics).map(|_| ()) });
            let mut shutdown = pin!(shutdown);
            tokio::select! {
                result = &mut servers => result,
                () = &mut shutdown => {
                    tracing::info!("shutting down");
                    token.cancel();
                    match tokio::time::timeout(drain_timeout, &mut servers).await {
                        Ok(result) => result,
                        Err(_) => {
                            tracing::warn!(
                                timeout_s = drain_timeout.as_secs(),
                                "requests still running after the drain timeout; dropping them"
                            );
                            Ok(())
                        }
                    }
                }
            }
        };
        token.cancel();
        if let Err(err) = maintenance.await {
            tracing::warn!(error = %err, "maintenance task failed");
        }
        // P1-07: wait for the job workers here, before the databases close.
        tokio::task::spawn_blocking(move || state.close())
            .await
            .context("closing the databases failed")?;
        served.context("a listener failed")
    }
}

/// Periodic upkeep until shutdown: evicts idle user databases and closes idle
/// readers, drops expired realtime events and idle event buses, and drains the
/// metrics recorder.
async fn maintenance(state: AppState, metrics: PrometheusHandle, token: CancellationToken) {
    let mut databases = tokio::time::interval(MAINTENANCE_INTERVAL);
    let mut upkeep = tokio::time::interval(UPKEEP_INTERVAL);
    databases.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    upkeep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = token.cancelled() => break,
            _ = databases.tick() => {
                state.events().sweep();
                let user_dbs = Arc::clone(state.user_dbs());
                if let Err(err) = tokio::task::spawn_blocking(move || user_dbs.run_maintenance()).await {
                    tracing::warn!(error = %err, "database maintenance failed");
                }
            }
            _ = upkeep.tick() => metrics.run_upkeep(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_has_two_workers() {
        let runtime = runtime().unwrap();
        assert_eq!(runtime.metrics().num_workers(), WORKER_THREADS);
    }
}
