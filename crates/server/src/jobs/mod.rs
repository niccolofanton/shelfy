//! Job scheduler and workers (plan §2.12). **Empty in P0**: P1-07 fills it.
//!
//! What lands here:
//!
//! - the in-process, DB-backed scheduler over the control DB's `jobs` table:
//!   per-kind, per-user FIFOs served round-robin, claims by conditional
//!   `UPDATE`, leases, backoff, `queue_state` pause and resume;
//! - the job-kind registry and the workers (`archive.drain`, `ai.drain`,
//!   `capture.site`, …), each with its global and per-user concurrency;
//! - the nightly schedule (03:00 UTC) and the drain sweeper hook.
//!
//! Seams already in place:
//!
//! - [`crate::serve::Server::run`] marks where the scheduler starts, with a
//!   child of the shutdown token ([`crate::state::AppState::shutdown_token`]),
//!   and where shutdown waits for the workers before the databases close
//!   (within the 25 s grace of §2.3);
//! - the `jobs` and `queue_state` tables exist in the control schema v1;
//! - [`crate::error::ErrorCode`] is where job-facing error codes go.
