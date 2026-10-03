//! Internal-only HTTP dispatch, cancellation and bounded NDJSON framing.
use std::future::Future;
use std::time::Duration;

use axum::http::{HeaderName, HeaderValue, header};
use futures_util::StreamExt as _;
use secrecy::ExposeSecret as _;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{
    Options,
    protocol::{self, Line, Request},
};
use crate::jobs::JobError;
use crate::outbound::EgressError;
use crate::state::AppState;

// L21: the service keeps completed pages when its 12-minute site budget
// expires. Allow another minute to serialize, deliver and validate that result.
// Keep this above CAPTURE_SITE_BUDGET_MS when changing the deployment budget.
pub const DEADLINE: Duration = Duration::from_secs(13 * 60);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Health {
    ok: bool,
    slots_free: u32,
}

pub async fn health(state: &AppState) -> Result<bool, JobError> {
    let origin = state
        .config()
        .outbound
        .capture
        .as_ref()
        .ok_or_else(unavailable)?;
    let client = state.outbound().internal().ok_or_else(unavailable)?;
    let response = client
        .get(&format!("{origin}/health"))
        .timeout(Duration::from_secs(5))
        .max_redirects(0)
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(unavailable());
    }
    let health: Health = response
        .json_capped(16 * 1024)
        .await
        .map_err(|_| unavailable())?;
    if !health.ok {
        return Err(unavailable());
    }
    Ok(health.slots_free > 0)
}

pub fn unavailable() -> JobError {
    JobError::transient("capture_unavailable")
}

/// `true` means a blocked result, which may have an on-disk OG fallback.
pub async fn run<F, Fut>(
    state: &AppState,
    id: &str,
    url: &str,
    opts: Options,
    token: &CancellationToken,
    mut on_line: F,
) -> Result<bool, JobError>
where
    F: FnMut(Line) -> Fut,
    Fut: Future<Output = ()>,
{
    let run = async {
        if !health(state).await? {
            return Err(unavailable());
        }
        let origin = state
            .config()
            .outbound
            .capture
            .as_ref()
            .ok_or_else(unavailable)?;
        let secret = state
            .config()
            .capture
            .internal_token
            .as_ref()
            .ok_or_else(unavailable)?;
        let client = state.outbound().internal().ok_or_else(unavailable)?;
        let request = Request {
            capture_id: id,
            url,
            max_pages: opts.max_pages,
            single_page: opts.single_page,
            video: true,
            work_dir: format!("/work/{id}"),
        };
        let response = client
            .post(&format!("{origin}/v1/captures"))
            .header(
                HeaderName::from_static("x-shelfy-internal-token"),
                HeaderValue::from_str(secret.expose_secret()).map_err(|_| unavailable())?,
            )
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )
            .body(serde_json::to_vec(&request).map_err(|_| protocol::invalid())?)
            .max_redirects(0)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if matches!(response.status().as_u16(), 429 | 502 | 503 | 504) {
            return Err(unavailable());
        }
        if !response.status().is_success() {
            return Err(JobError::permanent("capture_unavailable"));
        }
        if !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').next() == Some("application/x-ndjson"))
        {
            return Err(protocol::invalid());
        }
        let mut stream =
            response.stream_capped(((protocol::LINE_BYTES + 1) * protocol::MAX_LINES) as u64);
        let mut pending = Vec::new();
        let (mut lines, mut events, mut terminal) = (0, 0, None);
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(EgressError::TooLarge { .. }) => return Err(protocol::invalid()),
                Err(_) if terminal == Some(false) => break,
                Err(_) => return Err(JobError::transient("capture_stream_broken")),
            };
            for part in chunk.split_inclusive(|b| *b == b'\n') {
                if pending.len().saturating_add(part.len()) > protocol::LINE_BYTES + 1 {
                    return Err(protocol::invalid());
                }
                pending.extend_from_slice(part);
                if pending.last() != Some(&b'\n') {
                    continue;
                }
                pending.pop();
                lines += 1;
                if lines > protocol::MAX_LINES || terminal.is_some() {
                    return Err(protocol::invalid());
                }
                let line = protocol::line(&pending)?;
                pending.clear();
                match &line {
                    Line::Event { .. } => {
                        events += 1;
                        if events > protocol::MAX_EVENTS {
                            return Err(protocol::invalid());
                        }
                    }
                    Line::Done { .. } => terminal = Some(false),
                    Line::Failed { code } if code == "capture_blocked" => terminal = Some(true),
                    Line::Failed { code } => {
                        return Err(if code == "empty" {
                            JobError::permanent("capture_empty")
                        } else {
                            JobError::transient(format!("capture_{code}"))
                        });
                    }
                    Line::Page { .. } => {}
                }
                on_line(line).await;
            }
        }
        if !pending.is_empty() {
            return Err(protocol::invalid());
        }
        terminal.ok_or_else(|| JobError::transient("capture_stream_broken"))
    };
    tokio::select! {
        biased;
        () = token.cancelled() => Err(JobError::cancelled()),
        result = tokio::time::timeout(DEADLINE, run) => result.unwrap_or_else(|_| Err(JobError::transient("capture_timeout"))),
    }
}
