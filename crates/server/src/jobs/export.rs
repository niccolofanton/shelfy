//! One export per user, one writer overall, two attempts and a one-hour lease.
use super::{JobContext, JobResult, Kind, KindSpec, Outcome};
use serde::{Deserialize, Serialize};
use std::time::Duration;
pub const KIND: &str = "export";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    pub export_id: String,
}
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
    let payload: Payload = ctx.payload_as()?;
    crate::exports::run(&ctx, &payload.export_id).await?;
    Ok(Outcome::Succeeded)
}
