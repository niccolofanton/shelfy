//! `migrate` (plan §2.12, §4.1 step 5; P1-19): installs a desktop library
//! that `shelfy-migrate run` uploaded. The work is in
//! [`crate::migrations::install`].
//!
//! **Limits.** One install at a time overall and per user, two tries, a
//! 60-minute lease (§2.12: `import`, `export`, `migrate`). A user has at most
//! one active install: the dedupe key is the kind. The worker reports its
//! stage and progress on `job.updated` ([`crate::migrations::MigrationStage`]),
//! and checks for a cancel, a lost lease or a shutdown between chunks.
//!
//! **Errors.** A refused bundle (`validation_failed`), a library that is not
//! empty without `--merge` (`conflict`), the user's quota (`quota_exceeded`)
//! and the media budget (`storage_full`) fail at once; a busy or locked
//! library, a full disk or an interrupted worker are tried again.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Enqueued, JobContext, JobResult, Jobs, Kind, KindSpec, NewJob, Outcome};
use crate::error::ApiError;
use crate::migrations::install;

/// The kind's name.
pub const KIND: &str = "migrate";
/// Tries of an install.
pub const MAX_ATTEMPTS: u32 = 2;
/// How long an install may go without a sign of life.
pub const LEASE: Duration = Duration::from_secs(3600);

/// The kind, for the registry ([`super::kinds::registry`]).
#[must_use]
pub fn kind() -> Kind {
    Kind::new(
        KindSpec::new(KIND)
            .global(1)
            .per_user(1)
            .max_attempts(MAX_ATTEMPTS)
            .lease(LEASE),
        run,
    )
}

/// What an install job works on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    /// The complete upload of the bundle's database.
    pub db_upload_id: String,
    /// Merge into a library that is not empty.
    #[serde(default)]
    pub merge: bool,
}

/// Enqueues the install of the bundle whose database is `payload`'s upload
/// for `user_id`. When an install of the user is already queued or running,
/// that one is returned instead (`created` false).
///
/// # Errors
///
/// The control database failed.
pub async fn enqueue(jobs: &Jobs, user_id: &str, payload: &Payload) -> Result<Enqueued, ApiError> {
    let payload = serde_json::to_value(payload).map_err(ApiError::internal)?;
    jobs.enqueue(NewJob::new(user_id, KIND).dedupe(KIND).payload(payload))
        .await
}

async fn run(ctx: JobContext) -> JobResult {
    let payload: Payload = ctx.payload_as()?;
    install::run(&ctx, &payload).await?;
    Ok(Outcome::Succeeded)
}
