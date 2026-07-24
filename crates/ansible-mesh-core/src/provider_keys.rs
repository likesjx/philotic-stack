//! Shared provider API-key config metadata.
//!
//! Provider API keys may come from process env for ephemeral/CI use, or from
//! vault-backed config refs for at-rest storage. Plaintext config keys are
//! legacy migration inputs only.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderKeySpec {
    pub provider: &'static str,
    pub display_name: &'static str,
    pub vault_name: &'static str,
    pub api_key_ref_key: &'static str,
    pub legacy_api_key_key: &'static str,
    pub env_api_key: &'static str,
    pub env_api_key_ref: &'static str,
    pub default_model_key: Option<&'static str>,
    pub base_url_key: Option<&'static str>,
    pub embedding_model_key: Option<&'static str>,
    pub fallback_models_key: Option<&'static str>,
    pub route_key: Option<&'static str>,
    pub default_model: Option<&'static str>,
    pub default_base_url: Option<&'static str>,
    pub allowed_roles: &'static [&'static str],
}

pub const PROVIDER_KEY_SPECS: &[ProviderKeySpec] = &[
    ProviderKeySpec {
        provider: "anthropic",
        display_name: "Anthropic",
        vault_name: "anthropic_api_key",
        api_key_ref_key: "anthropic_api_key_ref",
        legacy_api_key_key: "anthropic_api_key",
        env_api_key: "PHILOTIC_ANTHROPIC_API_KEY",
        env_api_key_ref: "PHILOTIC_ANTHROPIC_API_KEY_REF",
        default_model_key: Some("anthropic_default_model"),
        base_url_key: Some("anthropic_base_url"),
        embedding_model_key: None,
        fallback_models_key: None,
        route_key: None,
        default_model: Some("claude-sonnet-5"),
        default_base_url: Some("https://api.anthropic.com"),
        allowed_roles: &["model", "model.anthropic"],
    },
    ProviderKeySpec {
        provider: "gemini",
        display_name: "Gemini",
        vault_name: "gemini_api_key",
        api_key_ref_key: "gemini_api_key_ref",
        legacy_api_key_key: "gemini_api_key",
        env_api_key: "PHILOTIC_GEMINI_API_KEY",
        env_api_key_ref: "PHILOTIC_GEMINI_API_KEY_REF",
        default_model_key: Some("gemini_default_model"),
        base_url_key: Some("gemini_base_url"),
        embedding_model_key: None,
        fallback_models_key: None,
        route_key: None,
        default_model: Some("gemini-2.0-flash-exp"),
        default_base_url: Some("https://generativelanguage.googleapis.com"),
        allowed_roles: &["model", "model.gemini"],
    },
    ProviderKeySpec {
        provider: "openrouter",
        display_name: "OpenRouter",
        vault_name: "openrouter_api_key",
        api_key_ref_key: "openrouter_api_key_ref",
        legacy_api_key_key: "openrouter_api_key",
        env_api_key: "PHILOTIC_OPENROUTER_API_KEY",
        env_api_key_ref: "PHILOTIC_OPENROUTER_API_KEY_REF",
        default_model_key: Some("openrouter_default_model"),
        base_url_key: Some("openrouter_base_url"),
        embedding_model_key: Some("openrouter_default_embedding_model"),
        fallback_models_key: Some("openrouter_fallback_models"),
        route_key: Some("openrouter_route"),
        default_model: Some("openai/gpt-4.1-mini"),
        default_base_url: Some("https://openrouter.ai/api"),
        allowed_roles: &["model", "model.openrouter"],
    },
    ProviderKeySpec {
        provider: "openai",
        display_name: "OpenAI",
        vault_name: "openai_api_key",
        api_key_ref_key: "openai_api_key_ref",
        legacy_api_key_key: "openai_api_key",
        env_api_key: "PHILOTIC_OPENAI_API_KEY",
        env_api_key_ref: "PHILOTIC_OPENAI_API_KEY_REF",
        default_model_key: Some("openai_default_model"),
        base_url_key: Some("openai_base_url"),
        embedding_model_key: Some("openai_default_embedding_model"),
        fallback_models_key: None,
        route_key: None,
        default_model: Some("gpt-4.1-mini"),
        default_base_url: Some("https://api.openai.com"),
        allowed_roles: &["model", "model.openai"],
    },
    ProviderKeySpec {
        provider: "elevenlabs",
        display_name: "ElevenLabs",
        vault_name: "elevenlabs_api_key",
        api_key_ref_key: "elevenlabs_api_key_ref",
        legacy_api_key_key: "elevenlabs_api_key",
        env_api_key: "PHILOTIC_ELEVENLABS_API_KEY",
        env_api_key_ref: "PHILOTIC_ELEVENLABS_API_KEY_REF",
        default_model_key: Some("elevenlabs_default_model"),
        base_url_key: Some("elevenlabs_base_url"),
        embedding_model_key: None,
        fallback_models_key: None,
        route_key: None,
        default_model: None,
        default_base_url: None,
        allowed_roles: &["model", "model.elevenlabs"],
    },
];

pub fn provider_key_specs() -> &'static [ProviderKeySpec] {
    PROVIDER_KEY_SPECS
}

pub fn provider_key_spec(provider: &str) -> Option<&'static ProviderKeySpec> {
    let normalized = provider.trim().to_ascii_lowercase().replace('_', "-");
    let compact = normalized.replace('-', "");
    PROVIDER_KEY_SPECS
        .iter()
        .find(|spec| spec.provider == normalized || spec.provider.replace('-', "") == compact)
}
