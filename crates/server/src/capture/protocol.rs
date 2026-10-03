//! Capture service's bounded, untrusted stream and disk manifest.
use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::jobs::JobError;

pub const LINE_BYTES: usize = 256 * 1024;
pub const MAX_LINES: usize = 400;
pub const MAX_EVENTS: usize = 250;
pub const MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
pub const SITE_BYTES: u64 = 80 * 1024 * 1024;
pub const IMAGE_BYTES: u64 = 15 * 1024 * 1024;
pub const VIDEO_BYTES: u64 = 40 * 1024 * 1024;

static SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    let schema: Value =
        serde_json::from_str(include_str!("../../../../capture/protocol.schema.json"))
            .expect("embedded capture schema");
    jsonschema::validator_for(&schema).expect("valid embedded capture schema")
});
static CODES: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../../shared/capture/codes.json"))
        .expect("embedded capture codes")
});

pub fn invalid() -> JobError {
    JobError::permanent("capture_invalid_output")
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asset {
    pub role: String,
    #[serde(default)]
    pub seq: Option<i64>,
    pub file: String,
    pub w: f64,
    pub h: f64,
    #[serde(default)]
    pub top: Option<f64>,
    #[serde(default)]
    pub css_height: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Line {
    Event {
        kind: String,
        code: String,
        #[serde(default)]
        params: BTreeMap<String, Value>,
    },
    #[serde(rename_all = "camelCase")]
    Page {
        index: i64,
        url: String,
        page_type: String,
        assets: Vec<Asset>,
    },
    #[serde(rename_all = "camelCase")]
    Done {
        manifest: String,
        duration_ms: f64,
        peak_rss_bytes: f64,
        bytes: f64,
        #[serde(default)]
        partial: bool,
    },
    Failed {
        code: String,
    },
}

pub fn line(bytes: &[u8]) -> Result<Line, JobError> {
    if bytes.len() > LINE_BYTES {
        return Err(invalid());
    }
    let line: Line = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    match &line {
        Line::Event { kind, code, params } => {
            let known = if code == "stage" {
                kind == "info"
            } else {
                CODES["events"][code]["kind"].as_str() == Some(kind.as_str())
            };
            if !known
                || params.len() > 32
                || params.iter().any(|(key, value)| {
                    key.len() > 64
                        || (!value.is_null()
                            && !value.is_string()
                            && !value.is_number()
                            && !value.is_boolean())
                        || value.as_str().is_some_and(|s| s.len() > 2048)
                })
            {
                return Err(invalid());
            }
            if code == "stage"
                && !params
                    .get("stage")
                    .and_then(Value::as_str)
                    .is_some_and(|stage| CODES["stages"][stage].is_object())
            {
                return Err(invalid());
            }
        }
        Line::Done { manifest, .. } if manifest != "manifest.json" => return Err(invalid()),
        Line::Failed { code }
            if !matches!(
                code.as_str(),
                "capture_blocked" | "timeout" | "navigation" | "empty" | "internal"
            ) =>
        {
            return Err(invalid());
        }
        Line::Page {
            index, url, assets, ..
        } if !(0..8).contains(index) || url.len() > 2048 || assets.len() > 128 => {
            return Err(invalid());
        }
        _ => {}
    }
    Ok(line)
}

pub fn manifest(bytes: &[u8], requested_url: &str, max_pages: u8) -> Result<Value, JobError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if !SCHEMA.is_valid(&value) || value["url"].as_str() != Some(requested_url) {
        return Err(invalid());
    }
    let pages = value["pages"].as_array().ok_or_else(invalid)?;
    if pages.len() > usize::from(max_pages)
        || pages
            .iter()
            .enumerate()
            .any(|(index, page)| page["index"].as_u64() != Some(index as u64))
    {
        return Err(invalid());
    }
    Ok(value)
}

/// Capture service dispatch; `work_dir` belongs to its mount, never a user.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request<'a> {
    pub capture_id: &'a str,
    pub url: &'a str,
    pub max_pages: u8,
    pub single_page: bool,
    pub video: bool,
    pub work_dir: String,
}
