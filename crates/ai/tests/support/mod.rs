//! Helpers of the adapter tests: a stub, providers pointed at it, fast
//! options, synthetic media.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use shelfy_ai::direct::DirectTransport;
use shelfy_ai::stub::{LoggedRequest, Stub, StubConfig};
use shelfy_ai::{
    CallOptions, EgressPolicy, JsonOutput, Origin, Provider, ProviderConfig, ProviderKind,
    RetryPolicy, Source, StructuredMode, Timeouts,
};
use url::Url;

/// A stub with the default settings.
pub async fn stub() -> Stub {
    Stub::start(StubConfig::default())
        .await
        .expect("the stub starts")
}

/// A stub with `config`.
pub async fn stub_with(config: StubConfig) -> Stub {
    Stub::start(config).await.expect("the stub starts")
}

/// The policy that allowlists the stub's origin, as the operator provider's.
pub fn policy(stub: &Stub) -> EgressPolicy {
    EgressPolicy::new().allow(Origin::of(&stub.url()).unwrap())
}

/// The base URL of `kind` on the stub.
pub fn base(stub: &Stub, kind: ProviderKind) -> Url {
    match kind {
        ProviderKind::OpenAiCompatible => stub.openai_base(),
        ProviderKind::Anthropic => stub.url(),
        ProviderKind::WhisperCpp => stub.whisper_url(),
    }
}

/// An operator provider of `kind` on the stub, without a key.
pub fn operator(stub: &Stub, kind: ProviderKind) -> Provider {
    provider(
        stub,
        ProviderConfig::new(kind, Source::Operator, base(stub, kind)),
    )
}

/// A provider of `config`, allowlisted on the stub, through the direct
/// transport.
pub fn provider(stub: &Stub, config: ProviderConfig) -> Provider {
    Provider::new(config, &policy(stub), Arc::new(DirectTransport::new()))
        .expect("the stub is allowlisted")
}

/// Retries that wait milliseconds, not seconds.
pub fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 3,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        retry_after_cap: Duration::from_secs(2),
    }
}

/// Short timeouts and fast retries.
pub fn options() -> CallOptions {
    CallOptions::new(Timeouts::new(
        Duration::from_secs(2),
        Duration::from_secs(10),
    ))
    .with_retry(fast_retry())
}

/// A catalog-like schema.
pub fn catalog_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "description": {"type": "string"},
            "general_tags": {"type": "array", "items": {"type": "string"}},
            "specific_tags": {"type": "array", "items": {"type": "string"}},
            "language": {"type": "string"}
        },
        "required": ["description", "general_tags", "specific_tags", "language"]
    })
}

/// [`catalog_schema`] as an output.
pub fn catalog() -> JsonOutput {
    JsonOutput::new("catalog", catalog_schema()).unwrap()
}

/// Bytes that stand for a small WebP (the stub does not decode images).
pub fn webp() -> Vec<u8> {
    let mut bytes = b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec();
    bytes.extend((0_u8..24).map(|n| n.wrapping_mul(37)));
    bytes
}

/// A 16 kHz mono 16-bit WAV of `samples` samples (a 440 Hz tone).
pub fn wav(samples: u32) -> Vec<u8> {
    let data_len = samples * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&16_000_u32.to_le_bytes());
    out.extend_from_slice(&32_000_u32.to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for n in 0..samples {
        let phase = f64::from(n) * 440.0 * std::f64::consts::TAU / 16_000.0;
        let sample = (phase.sin() * 8_000.0) as i16;
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// The only request the stub logged.
pub fn only_request(stub: &Stub) -> LoggedRequest {
    let requests = stub.requests();
    assert_eq!(requests.len(), 1, "{requests:#?}");
    requests.into_iter().next().unwrap()
}

/// The structured mode name a JSON body asked for (`response_format.type`).
pub fn response_format(request: &LoggedRequest) -> Option<String> {
    request
        .body
        .as_ref()?
        .get("response_format")?
        .get("type")?
        .as_str()
        .map(str::to_owned)
}

/// An operator provider of `kind` with structured mode `mode`.
pub fn operator_with_mode(stub: &Stub, kind: ProviderKind, mode: StructuredMode) -> Provider {
    provider(
        stub,
        ProviderConfig::new(kind, Source::Operator, base(stub, kind)).with_structured(mode),
    )
}
