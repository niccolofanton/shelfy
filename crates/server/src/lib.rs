//! `shelfy-server`: the Shelfy Web API process.
//!
//! It owns:
//!
//! - the axum application: REST API, SSE and SPA hosting;
//! - authentication and sessions;
//! - the job scheduler and its workers;
//! - the `shelfy-server admin …` operator CLI.
//!
//! The binary (`main.rs`) stays a thin entry point: the logic lives in this
//! library so tests can drive it in-process.
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`cli`] | the command line: `serve`, `admin` and `healthcheck` |
//! | [`config`] | environment configuration, validated once at start |
//! | [`serve`] | the tokio runtime, the two listeners, graceful shutdown |
//! | [`healthcheck`] | `shelfy-server healthcheck`, the container's probe of `/health` |
//! | [`app`] | the middleware stack around the routes |
//! | [`routes`] | the router composition and the OpenAPI document |
//! | [`limits`] | per-route body limits and handler timeouts |
//! | [`error`] | `application/problem+json` errors with stable codes |
//! | [`extract`] | request extractors whose rejections are problems |
//! | [`current_user`] | the signed-in user of a request: the seam to authentication |
//! | [`conditional`] | ETags from the library generation, `304 Not Modified` |
//! | [`state`] | the shared state: databases, config, auth, mailer, event bus, shutdown token |
//! | [`auth`] | sign-in links, sessions, the CSRF guard, the deny-by-default access gate |
//! | [`mail`] | outgoing email: SMTP, the dev mailbox, or off |
//! | [`net`] | the client address behind trusted proxies, local hosts |
//! | [`telemetry`] | JSON logs, redaction, request ids, Prometheus metrics |
//! | [`control`] | queries on the control database |
//! | [`admin`] | the operator commands |
//! | [`migrations`] | installing a desktop library uploaded by `shelfy-migrate` (T9) |
//! | [`events`] | the per-user realtime bus behind SSE: publish, replay, throttles |
//! | [`jobs`] | the job system: scheduler, workers, job-kind registry, `Idempotency-Key` |
//! | [`static_files`] | the web app's files: precompressed assets, `index.html` for client routes |
//! | [`security_headers`] | the content security policy, HSTS and `nosniff` on every response |
//! | [`library`] | the library write path (events after each write) and the caches keyed by the generation |
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §2.2–§2.4 and §3.

pub mod admin;
pub mod app;
pub mod auth;
pub mod cli;
pub mod conditional;
pub mod config;
pub mod control;
pub mod current_user;
pub mod error;
pub mod events;
pub mod extract;
pub mod healthcheck;
pub mod ids;
pub mod jobs;
pub mod library;
pub mod limits;
pub mod mail;
pub mod migrations;
pub mod net;
pub mod routes;
pub mod security_headers;
pub mod serve;
pub mod state;
pub mod static_files;
pub mod telemetry;
pub mod tokens;

/// Version of this build (the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
