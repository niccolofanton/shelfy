//! Prometheus metrics (plan §3.6), served on their own listener (`:9464`),
//! which only the internal network reaches; the edge proxy never forwards it.
//!
//! The recorder is process-wide: [`install`] sets it up once and every
//! `metrics::counter!`/`histogram!` in the crate reports to it. Labels never
//! carry user ids or other per-user values; routes are templates.
//!
//! P0 metrics: `shelfy_http_requests_total{route,method,status}`,
//! `shelfy_http_request_duration_seconds{route}` (histogram) and
//! `shelfy_build_info{version}`. P1-15 adds the rest of §3.6.

use std::sync::OnceLock;
use std::time::Duration;

use axum::Router;
use axum::http::{Method, StatusCode, header};
use axum::routing::get;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

/// Counter of HTTP requests by route template, method and status.
pub const HTTP_REQUESTS_TOTAL: &str = "shelfy_http_requests_total";
/// Histogram of request handling time by route template, in seconds.
pub const HTTP_REQUEST_DURATION_SECONDS: &str = "shelfy_http_request_duration_seconds";
/// Constant 1, labelled with the build version.
pub const BUILD_INFO: &str = "shelfy_build_info";

/// Content type of the Prometheus text exposition format.
pub const PROMETHEUS_TEXT: &str = "text/plain; version=0.0.4; charset=utf-8";

/// How often [`PrometheusHandle::run_upkeep`] must run (the exporter's default).
pub const UPKEEP_INTERVAL: Duration = Duration::from_secs(5);

/// Histogram buckets of request durations, in seconds, with bounds at the
/// §6.2 budgets (5, 15, 40, 60, 100, 300 and 500 ms).
const DURATION_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.015, 0.025, 0.04, 0.06, 0.1, 0.25, 0.3, 0.5, 1.0, 2.5, 5.0, 10.0,
    30.0,
];

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Installs the process-wide recorder on the first call; every call returns
/// its handle.
pub fn install() -> PrometheusHandle {
    HANDLE
        .get_or_init(|| {
            let recorder = PrometheusBuilder::new()
                .set_buckets_for_metric(
                    Matcher::Full(HTTP_REQUEST_DURATION_SECONDS.to_owned()),
                    DURATION_BUCKETS,
                )
                .expect("the bucket list is not empty")
                .build_recorder();
            let handle = recorder.handle();
            if metrics::set_global_recorder(recorder).is_err() {
                tracing::warn!("another metrics recorder is installed; /metrics stays empty");
            }
            describe();
            metrics::gauge!(BUILD_INFO, "version" => crate::VERSION).set(1.0);
            handle
        })
        .clone()
}

fn describe() {
    metrics::describe_counter!(
        HTTP_REQUESTS_TOTAL,
        "HTTP requests by route template, method and status."
    );
    metrics::describe_histogram!(
        HTTP_REQUEST_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Time from request to response headers, by route template."
    );
    metrics::describe_gauge!(BUILD_INFO, "Always 1; the label carries the version.");
}

/// The router of the metrics listener: `GET /metrics` only.
pub fn router(handle: PrometheusHandle) -> Router {
    Router::new().route(
        "/metrics",
        get(move || {
            let body = handle.render();
            async move { ([(header::CONTENT_TYPE, PROMETHEUS_TEXT)], body) }
        }),
    )
}

/// Records one handled request (called by [`super::http::observe`]).
pub fn record_http_request(route: &str, method: &Method, status: StatusCode, elapsed: Duration) {
    metrics::counter!(
        HTTP_REQUESTS_TOTAL,
        "route" => route.to_owned(),
        "method" => method_label(method),
        "status" => status.as_str().to_owned(),
    )
    .increment(1);
    metrics::histogram!(HTTP_REQUEST_DURATION_SECONDS, "route" => route.to_owned())
        .record(elapsed.as_secs_f64());
}

/// Standard methods keep their name; anything else is `OTHER`, so a client
/// cannot grow the label set.
fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_methods_share_one_label() {
        assert_eq!(method_label(&Method::GET), "GET");
        assert_eq!(method_label(&Method::from_bytes(b"BREW").unwrap()), "OTHER");
        assert_eq!(method_label(&Method::CONNECT), "OTHER");
    }
}
