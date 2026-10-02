//! Running the server: the tokio runtime, the two listeners and the graceful
//! shutdown (plan §2.3).
//!
//! - **Runtime:** multi-thread with 2 async workers and at most 16 blocking
//!   threads. Every SQLite call runs on the blocking pool.
//! - **Listeners:** the API (`SHELFY_LISTEN_ADDR`) and Prometheus
//!   (`SHELFY_METRICS_ADDR`), served side by side.
//! - **Jobs:** the scheduler ([`crate::jobs`]) runs beside the listeners on
//!   a child of the shutdown token.
//! - **Background tasks:** the maintenance timer (idle databases, locked
//!   libraries, event buses, metrics) and, once per boot, the library
//!   upgrade sweep ([`upgrade_libraries`], plan §3.8). The control database
//!   is upgraded before the listeners bind.
//! - **Shutdown** on SIGTERM or Ctrl-C: stop accepting, cancel the shutdown
//!   token (job workers and streams stop), let in-flight requests and job
//!   workers finish, checkpoint the WAL and close the databases, all within
//!   [`Config::shutdown_grace`] (25 s). Interrupted jobs go back to the
//!   queue; a job still running at the deadline is re-queued on the next
//!   boot.

use std::future::{Future, IntoFuture as _};
use std::io;
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::Router;
use metrics_exporter_prometheus::PrometheusHandle;
use shelfy_core::db::{LibraryUpgrade, library_ids};
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
/// How long after boot the library upgrade sweep starts: first requests first.
const SWEEP_DELAY: Duration = Duration::from_secs(10);
/// Pause between two libraries of the sweep, so it stays a background trickle.
const SWEEP_PAUSE: Duration = Duration::from_millis(100);
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
        let web_dir = config.web.as_ref().map(|web| web.root().display());
        tracing::info!(
            version = crate::VERSION,
            api = %api_listener.local_addr()?,
            metrics = %metrics_listener.local_addr()?,
            data_dir = %config.data_dir.root().display(),
            public_url = %config.public_url,
            web_dir = web_dir.map(tracing::field::display),
            "listening"
        );
        let drain_timeout = config.shutdown_grace.saturating_sub(CLOSE_RESERVE);

        let maintenance = tokio::spawn(maintenance(state.clone(), metrics.clone(), token.clone()));
        let sweep = tokio::spawn(upgrade_libraries(
            state.clone(),
            token.clone(),
            SWEEP_DELAY,
            SWEEP_PAUSE,
        ));
        let scheduler = state.jobs().start(state.clone(), token.child_token());
        // When the shutdown began; the job workers stop within the same
        // drain budget as the requests.
        let mut stopping = None;

        let served = {
            // The peer address reaches the handlers as `ConnectInfo`: the
            // client, or the proxy that reports it (crate::net).
            let api = axum::serve(
                api_listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
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
                    stopping = Some(tokio::time::Instant::now());
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
        if let Err(err) = sweep.await {
            tracing::warn!(error = %err, "library upgrade sweep failed");
        }
        // The job workers stop before the databases close; interrupted jobs
        // are queued again, and any still running at the deadline are
        // queued again at the next start.
        let deadline = stopping.unwrap_or_else(tokio::time::Instant::now) + drain_timeout;
        if !scheduler.stop(deadline).await {
            tracing::warn!(
                timeout_s = drain_timeout.as_secs(),
                "jobs still running after the drain timeout were stopped; they run again at the next start"
            );
        }
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

/// What [`upgrade_libraries`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Libraries looked at.
    pub checked: usize,
    /// Libraries migrated to this build's schema.
    pub upgraded: usize,
    /// Libraries a newer release left ahead of this build (a rollback).
    pub ahead: usize,
    /// Libraries skipped because they are locked for maintenance.
    pub locked: usize,
    /// Libraries whose upgrade failed; their next open retries it.
    pub failed: usize,
}

/// The low-priority sweep after boot (plan §3.8): waits `delay`, then
/// upgrades every library that is behind this build, one at a time on the
/// blocking pool with `pause` in between, until done or shutdown.
///
/// Libraries also upgrade lazily when they are first opened, so the sweep
/// only spares a user the migration on their first request; a failure here
/// is logged and retried by that open. Not a job kind: it touches no queue
/// and runs once per boot.
pub async fn upgrade_libraries(
    state: AppState,
    shutdown: CancellationToken,
    delay: Duration,
    pause: Duration,
) -> SweepReport {
    let mut report = SweepReport::default();
    tokio::select! {
        () = shutdown.cancelled() => return report,
        () = tokio::time::sleep(delay) => {}
    }
    let users_dir = state.config().data_dir.users_dir();
    let ids = match tokio::task::spawn_blocking(move || library_ids(&users_dir)).await {
        Ok(Ok(ids)) => ids,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "library upgrade sweep cannot list the users");
            return report;
        }
        Err(err) => {
            tracing::warn!(error = %err, "library upgrade sweep failed");
            return report;
        }
    };
    for id in ids {
        if shutdown.is_cancelled() {
            break;
        }
        report.checked += 1;
        let user_dbs = Arc::clone(state.user_dbs());
        let user = id.clone();
        let started = Instant::now();
        match tokio::task::spawn_blocking(move || user_dbs.upgrade(&user)).await {
            Ok(Ok(LibraryUpgrade::Upgraded { from, to })) => {
                report.upgraded += 1;
                let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                tracing::info!(user_id = %id, from, to, elapsed_ms, "library schema upgraded");
            }
            Ok(Ok(LibraryUpgrade::Ahead { found })) => {
                report.ahead += 1;
                tracing::warn!(
                    user_id = %id,
                    schema = found,
                    "library schema is newer than this build: running on it (a rollback)"
                );
            }
            Ok(Ok(LibraryUpgrade::Locked)) => report.locked += 1,
            Ok(Ok(LibraryUpgrade::Current | LibraryUpgrade::Missing)) => {}
            Ok(Err(err)) => {
                report.failed += 1;
                tracing::warn!(user_id = %id, error = %err, "library schema upgrade failed");
            }
            Err(err) => {
                report.failed += 1;
                tracing::warn!(user_id = %id, error = %err, "library schema upgrade failed");
            }
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(pause) => {}
        }
    }
    tracing::info!(
        checked = report.checked,
        upgraded = report.upgraded,
        ahead = report.ahead,
        locked = report.locked,
        failed = report.failed,
        "library upgrade sweep done"
    );
    report
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
