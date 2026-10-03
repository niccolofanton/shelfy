//! The job kinds of the server: the job-kind registry.
//!
//! This file is a shared registry (P1 lane rule 3): a task that adds a kind
//! lists it under "Owns", puts its worker in a module of its own under
//! `jobs/`, and adds one `.register(…)` line to [`registry`].
//!
//! P1-07 ships the job system without kinds. The kinds of the plan (§2.12)
//! and the tasks that bring them:
//!
//! | Kind | Task | Global | Per user | Tries | Lease | Notes |
//! |---|---|---|---|---|---|---|
//! | `bulk` | P1-11 | 2 | 1 | 3 | 5 min | new kind, bulk actions over 500 posts (G6) ([`super::bulk`]) |
//! | `purge` | P1-11 | 1 | 1 | 3 | 60 min | `nightly` (30-day retention), and `POST /trash/empty`; waits for the user's earlier `bulk` jobs ([`super::purge`]) |
//! | `usage.recompute` | P1-17 | 2 | 1 | 3 | 5 min | new kind, `nightly` and after an install or a purge ([`super::usage`]) |
//! | `migrate` | P1-19 | 1 | 1 | 2 | 60 min | the install of a migration bundle ([`super::migrate`]) |
//! | `archive.drain` | P2-10 | 4 fetches, 2 encodes | 2 fetches | 5 per item | 5 min | a drain: dedupe key = the kind, plus a [`Sweep`](super::Sweep) check ([`super::archive`]) |
//! | `link.hydrate` | P2-11 | 2 | 1 | 5 | 2 min | a shared link's post, from the platforms' public endpoints ([`super::hydrate`]) |
//! | `ai.drain` | P3 | 32 in flight | 4 | 3 per item | 10 min | a drain |
//! | `ai.run` | P3 | 4 | 1 | 1 | 30 min | |
//! | `media.video` | P4 | 2 | 1 | 3 | 15 min | |
//! | `capture.site` | P4 | 1 | 1 | 2 | 20 min | |
//! | `import`, `export` | P4 | 1 | 1 | 2 | 60 min | |
//! | `gc` | P4 | 1 | — | 3 | 60 min | `nightly` |
//!
//! For a drain, the limits of the table apply to its items, inside the
//! worker; the job itself is one per user (`per_user` 1). A row becomes a
//! [`KindSpec`](super::KindSpec):
//!
//! ```ignore
//! .register(Kind::new(
//!     KindSpec::new("migrate").max_attempts(2).lease(Duration::from_secs(3600)),
//!     migrate::run,
//! ))
//! ```

use super::{Registry, ai_drain, archive, bulk, hydrate, migrate, purge, usage};

/// Every kind this server runs.
#[must_use]
pub fn registry() -> Registry {
    Registry::new()
        .register(ai_drain::kind())
        .register(usage::kind())
        .register(migrate::kind())
        .register(super::export::kind())
        .register(bulk::kind())
        .register(purge::kind())
        .register(hydrate::kind())
        .register(archive::kind())
}
