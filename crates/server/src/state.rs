//! The state shared by every request: the databases, the configuration,
//! authentication, the mailer, the realtime event bus, the job system, the
//! library caches and the shutdown token.

use std::sync::Arc;

use anyhow::Context as _;
use shelfy_core::db::{ControlDb, UserDb, UserDbCache};
use shelfy_core::schema::{Kind, Upgrade};
use tokio_util::sync::CancellationToken;

use crate::auth::{self, AuthState};
use crate::config::Config;
use crate::error::ApiError;
use crate::events::EventBus;
use crate::jobs::Jobs;
use crate::library::LibraryCaches;
use crate::mail::Mailer;

/// Cheap to clone: everything lives behind one `Arc`.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    config: Config,
    control: Arc<ControlDb>,
    user_dbs: Arc<UserDbCache>,
    auth: AuthState,
    mailer: Mailer,
    events: EventBus,
    jobs: Jobs,
    library_caches: LibraryCaches,
    shutdown: CancellationToken,
}

impl AppState {
    /// Creates the data directory layout, opens (creating and migrating) the
    /// control database and sets up the mailer. Blocking: call it from
    /// `spawn_blocking` inside the runtime.
    ///
    /// # Errors
    ///
    /// The directories cannot be created, the control database cannot be
    /// opened, or the mail transport cannot be set up.
    pub fn open(config: Config) -> anyhow::Result<Self> {
        let data = &config.data_dir;
        data.create_layout()
            .with_context(|| format!("cannot create the layout of {}", data.root().display()))?;
        let control_path = data.control_db();
        let control = ControlDb::open(&control_path, &config.control_db)
            .with_context(|| format!("cannot open {}", control_path.display()))?;
        log_upgrade(Kind::Control, None, control.schema_upgrade());
        let user_dbs = UserDbCache::new(
            data.users_dir(),
            &config.user_db_cache,
            config.user_db.clone(),
        )
        .with_upgrade_listener(|user_id, upgrade| {
            log_upgrade(Kind::Library, Some(user_id), upgrade);
        });
        let mailer = Mailer::new(&config.mail).context("cannot set up email")?;
        tracing::info!(transport = mailer.kind().as_str(), "email transport");
        if config.trusted_proxies.is_empty() {
            tracing::info!(
                "no trusted proxy: CF-Connecting-IP is ignored, the TCP peer is the client"
            );
        } else {
            tracing::info!(
                trusted_proxies = %config.trusted_proxies,
                "CF-Connecting-IP names the client behind these proxies"
            );
        }
        if !auth::cookie::secure_cookies_work(config.public_url.as_str()) {
            tracing::warn!(
                public_url = %config.public_url,
                "browsers drop the Secure session cookie on this origin: use https or localhost"
            );
        }
        let auth = AuthState::new(config.auth.clone(), &config.public_url);
        let control = Arc::new(control);
        let events = EventBus::new();
        let jobs = Jobs::new(&config.jobs, Arc::clone(&control), events.clone());
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                control,
                user_dbs: Arc::new(user_dbs),
                auth,
                mailer,
                events,
                jobs,
                library_caches: LibraryCaches::new(),
                shutdown: CancellationToken::new(),
            }),
        })
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    /// The control database. Its API is blocking: use [`blocking`].
    #[must_use]
    pub fn control(&self) -> &Arc<ControlDb> {
        &self.inner.control
    }

    /// The open user databases.
    #[must_use]
    pub fn user_dbs(&self) -> &Arc<UserDbCache> {
        &self.inner.user_dbs
    }

    /// Sessions, sign-in limits and the email slots.
    #[must_use]
    pub fn auth(&self) -> &AuthState {
        &self.inner.auth
    }

    /// The outgoing-email transport.
    #[must_use]
    pub fn mailer(&self) -> &Mailer {
        &self.inner.mailer
    }

    /// The realtime event bus: publish after a write commits.
    #[must_use]
    pub fn events(&self) -> &EventBus {
        &self.inner.events
    }

    /// The job system: enqueue and control jobs. Its scheduler runs from
    /// [`crate::serve::Server::run`].
    #[must_use]
    pub fn jobs(&self) -> &Jobs {
        &self.inner.jobs
    }

    /// Counts and stats cached per library generation ([`crate::library`]).
    #[must_use]
    pub fn library_caches(&self) -> &LibraryCaches {
        &self.inner.library_caches
    }

    /// Cancelled when the server starts shutting down. Long-running work (job
    /// workers, SSE streams) takes a child token and stops when it fires.
    #[must_use]
    pub fn shutdown_token(&self) -> &CancellationToken {
        &self.inner.shutdown
    }

    /// The database of `user_id`, opened (and created and migrated) off the
    /// async workers when it is not cached yet.
    ///
    /// # Errors
    ///
    /// The database cannot be opened.
    pub async fn user_db(&self, user_id: &str) -> Result<Arc<UserDb>, ApiError> {
        let user_dbs = Arc::clone(&self.inner.user_dbs);
        let user_id = user_id.to_owned();
        blocking(move || user_dbs.get(&user_id)).await
    }

    /// Closes the databases after the listeners have stopped: checkpoints the
    /// control database's WAL and drops the open user databases, which
    /// checkpoints and closes them too. Blocking.
    ///
    /// Requests that still hold the state (a handler that outlived its
    /// connection) keep their databases until they finish.
    pub fn close(self) {
        if let Err(err) = self.inner.control.checkpoint() {
            tracing::warn!(error = %err, "control database checkpoint failed");
        }
        match Arc::try_unwrap(self.inner) {
            Ok(inner) => drop(inner),
            Err(shared) => tracing::warn!(
                holders = Arc::strong_count(&shared) - 1,
                "state still in use at shutdown; its databases close when the last holder ends"
            ),
        }
    }
}

/// Logs what opening a database of `kind` did to its schema (plan §3.8): an
/// upgrade, or a database left ahead by a newer release (a rollback). A new
/// database (`from` 0) and a current one are not worth a line.
fn log_upgrade(kind: Kind, user_id: Option<&str>, upgrade: Upgrade) {
    let database = kind.name();
    match upgrade {
        Upgrade::Current | Upgrade::Upgraded { from: 0, .. } => {}
        Upgrade::Upgraded { from, to } => {
            tracing::info!(database, user_id, from, to, "schema upgraded");
        }
        Upgrade::Ahead { found } => tracing::warn!(
            database,
            user_id,
            schema = found,
            supported = kind.latest_version(),
            "schema is newer than this build: running on it (a rollback)"
        ),
    }
}

/// Runs blocking work (every SQLite call) on the blocking pool (§2.3) and
/// maps its error into an [`ApiError`]. A panic in `f` becomes a 500.
///
/// # Errors
///
/// `f`'s error, converted; or a 500 when `f` panicked.
pub async fn blocking<T, E, F>(f: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Into<ApiError> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result.map_err(Into::into),
        Err(join) => Err(ApiError::internal(anyhow::anyhow!(
            "blocking task failed: {join}"
        ))),
    }
}
