//! Import format dispatch and its scheduler contract.
use super::{JobContext, JobResult, Kind, KindSpec};
use serde::{Deserialize, Serialize};
use std::time::Duration;
/// Job kind.
pub const KIND: &str = "import";
/// Claims are made atomically with this payload's job at POST admission.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    /// Complete, consumed upload owned by this job's user.
    pub upload_id: String,
}
/// One import overall and per user, two attempts, sixty-minute lease.
pub fn kind() -> Kind {
    Kind::new(
        KindSpec::new(KIND)
            .global(1)
            .per_user(1)
            .max_attempts(2)
            .lease(Duration::from_secs(3600)),
        run,
    )
}
async fn run(ctx: JobContext) -> JobResult {
    crate::imports::v1::run(&ctx).await
}
