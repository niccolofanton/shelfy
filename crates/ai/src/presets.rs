//! The providers a user can pick when adding one (plan §2.15, P3-19, P3-26).
//!
//! Each preset gives a protocol, a base URL, the optional tasks it serves and
//! a structured-output mode. Every row is provisional: `verified` stays false
//! until SPIKE-6's cloud rows (P3-10) confirm it. No model id is a default
//! (G3-23): models come from `/models` or from the user.

use secrecy::SecretString;
use serde::Serialize;
use url::Url;

use crate::error::AiError;
use crate::provider::{MaxTokensField, ProviderConfig, ProviderKind, Source};
use crate::structured::StructuredMode;

/// The optional tasks a provider serves. Every chat provider serves the text
/// tasks (chat, suggestions, cluster and alias runs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Tasks {
    /// Images in chat (cataloging, screenshot QC).
    pub vision: bool,
    /// `/embeddings` (cluster runs).
    pub embeddings: bool,
    /// Speech to text (dictation).
    pub stt: bool,
}

/// A provider a user can pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Preset {
    /// A stable id (`openai`, `anthropic`…).
    pub id: &'static str,
    /// The name shown.
    pub label: &'static str,
    /// The protocol.
    pub kind: ProviderKind,
    /// The base URL; `None` for Custom, where the user enters it.
    pub base_url: Option<&'static str>,
    /// The optional tasks it serves.
    pub tasks: Tasks,
    /// How JSON answers are asked for.
    pub structured: StructuredMode,
    /// The token cap's field.
    pub max_tokens_field: MaxTokensField,
    /// Whether streams end with a usage chunk on request.
    pub stream_usage: bool,
    /// Whether it decodes WebP images ([`ProviderConfig::webp_images`]).
    pub webp_images: bool,
    /// Whether it takes `temperature` ([`ProviderConfig::send_temperature`]).
    pub send_temperature: bool,
    /// Whether SPIKE-6 confirmed the row.
    pub verified: bool,
}

const fn tasks(vision: bool, embeddings: bool, stt: bool) -> Tasks {
    Tasks {
        vision,
        embeddings,
        stt,
    }
}

/// Every preset, in the order the wizard lists them.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "openai",
        label: "OpenAI",
        kind: ProviderKind::OpenAiCompatible,
        base_url: Some("https://api.openai.com/v1"),
        tasks: tasks(true, true, true),
        structured: StructuredMode::JsonSchema,
        max_tokens_field: MaxTokensField::MaxCompletionTokens,
        stream_usage: true,
        webp_images: true,
        send_temperature: true,
        verified: false,
    },
    Preset {
        id: "anthropic",
        label: "Anthropic",
        kind: ProviderKind::Anthropic,
        base_url: Some("https://api.anthropic.com"),
        tasks: tasks(true, false, false),
        // `output_config.format` works on the current models; forced tool use
        // (`StructuredMode::Tool`) is refused by some of them.
        structured: StructuredMode::JsonSchema,
        max_tokens_field: MaxTokensField::MaxTokens,
        stream_usage: false,
        webp_images: true,
        // The newest Claude models answer 400 to `temperature`.
        send_temperature: false,
        verified: false,
    },
    Preset {
        id: "gemini",
        label: "Google Gemini",
        kind: ProviderKind::OpenAiCompatible,
        base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        tasks: tasks(true, true, false),
        structured: StructuredMode::JsonSchema,
        max_tokens_field: MaxTokensField::MaxTokens,
        stream_usage: true,
        webp_images: true,
        send_temperature: true,
        verified: false,
    },
    Preset {
        id: "openrouter",
        label: "OpenRouter",
        kind: ProviderKind::OpenAiCompatible,
        base_url: Some("https://openrouter.ai/api/v1"),
        tasks: tasks(true, false, false),
        structured: StructuredMode::JsonSchema,
        max_tokens_field: MaxTokensField::MaxTokens,
        stream_usage: true,
        webp_images: true,
        send_temperature: true,
        verified: false,
    },
    Preset {
        id: "groq",
        label: "Groq",
        kind: ProviderKind::OpenAiCompatible,
        base_url: Some("https://api.groq.com/openai/v1"),
        tasks: tasks(true, false, true),
        structured: StructuredMode::JsonObject,
        max_tokens_field: MaxTokensField::MaxTokens,
        stream_usage: true,
        webp_images: true,
        send_temperature: true,
        verified: false,
    },
    Preset {
        id: "mistral",
        label: "Mistral",
        kind: ProviderKind::OpenAiCompatible,
        base_url: Some("https://api.mistral.ai/v1"),
        tasks: tasks(true, true, false),
        structured: StructuredMode::JsonSchema,
        max_tokens_field: MaxTokensField::MaxTokens,
        stream_usage: false,
        webp_images: true,
        send_temperature: true,
        verified: false,
    },
    Preset {
        id: "together",
        label: "Together AI",
        kind: ProviderKind::OpenAiCompatible,
        base_url: Some("https://api.together.xyz/v1"),
        tasks: tasks(true, true, false),
        structured: StructuredMode::JsonObject,
        max_tokens_field: MaxTokensField::MaxTokens,
        stream_usage: true,
        webp_images: true,
        send_temperature: true,
        verified: false,
    },
    Preset {
        id: "custom",
        label: "Custom (OpenAI-compatible)",
        kind: ProviderKind::OpenAiCompatible,
        base_url: None,
        tasks: tasks(false, false, false),
        structured: StructuredMode::JsonObject,
        max_tokens_field: MaxTokensField::MaxTokens,
        stream_usage: false,
        // llama.cpp-based servers decode no WebP (stb_image): send JPEG.
        webp_images: false,
        send_temperature: true,
        verified: false,
    },
];

/// The preset `id`.
#[must_use]
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

impl Preset {
    /// A user provider's settings from this preset: its base URL (`base_url`
    /// for Custom, which needs one), its modes and `key`. The provider's
    /// constructor judges the URL ([`crate::Provider::new`]).
    ///
    /// # Errors
    ///
    /// [`crate::ErrorKind::BadRequest`] when Custom gets no base URL.
    pub fn config(
        &self,
        base_url: Option<Url>,
        key: Option<SecretString>,
    ) -> Result<ProviderConfig, AiError> {
        let base_url = match (self.base_url, base_url) {
            (_, Some(url)) => url,
            (Some(url), None) => {
                Url::parse(url).map_err(|_| AiError::bad_request("the preset URL is not valid"))?
            }
            (None, None) => return Err(AiError::bad_request("a custom provider needs a base URL")),
        };
        let mut config =
            ProviderConfig::new(self.kind, Source::User, base_url).with_structured(self.structured);
        config.max_tokens_field = self.max_tokens_field;
        config.stream_usage = self.stream_usage;
        config.webp_images = self.webp_images;
        config.send_temperature = self.send_temperature;
        config.key = key;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::EgressPolicy;

    #[test]
    fn every_preset_url_passes_the_user_guard() {
        let policy = EgressPolicy::new();
        for preset in PRESETS {
            assert!(!preset.verified, "{}: verified before SPIKE-6", preset.id);
            if let Some(url) = preset.base_url {
                let url = Url::parse(url).unwrap();
                assert!(policy.check_user_url(&url).is_ok(), "{}", preset.id);
            }
        }
        assert_eq!(PRESETS.len(), 8);
    }

    #[test]
    fn custom_needs_a_url() {
        let custom = preset("custom").unwrap();
        assert!(custom.config(None, None).is_err());
        let url = Url::parse("https://llm.example.com/v1").unwrap();
        let config = custom.config(Some(url.clone()), None).unwrap();
        assert_eq!(config.base_url, url);
        assert_eq!(config.source, Source::User);
        assert_eq!(config.structured, StructuredMode::JsonObject);
    }

    #[test]
    fn presets_ask_only_what_their_protocol_offers() {
        for preset in PRESETS {
            if preset.kind == ProviderKind::Anthropic {
                assert!(
                    !preset.tasks.embeddings && !preset.tasks.stt,
                    "{}",
                    preset.id
                );
            } else {
                assert_ne!(preset.structured, StructuredMode::Tool, "{}", preset.id);
            }
        }
    }
}
