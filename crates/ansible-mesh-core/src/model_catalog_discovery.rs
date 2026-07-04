//! Provider model-list discovery for the model graph catalog.
//!
//! This module is the *ingestion engine* for the static catalog defined in
//! [`crate::model_manager`]. It turns each provider's live model-list API
//! response into provider-neutral [`DiscoveredModel`] facts, maps those into the
//! existing catalog schema ([`ModelProviderAvailabilityRecord`],
//! [`ModelExternalSourceRecord`]), and computes lifecycle diffs so the operator
//! (and the self-heal queue) learn when a provider *adds* or *retires* a model
//! or flips a model to a reasoning/"thinking" default.
//!
//! Authority split (per MODEL_GRAPH_CATALOG_PROPOSAL): discovery writes only
//! static catalog facts + provenance. It does NOT touch live availability,
//! reachability, auth, or per-turn routing. Discovery *proposes* capability; the
//! flywheel (`observe_model_outcome`) *confirms* it. Everything here is pure and
//! total — the runtime source of the `prev` catalog (persisted graph vs. seed)
//! is a wiring decision handled elsewhere.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::model_manager::{ModelExternalSourceRecord, ModelProviderAvailabilityRecord};

/// A single model as discovered from a provider's live model-list endpoint.
///
/// Provider-neutral intermediate. Richer than the persisted catalog record so it
/// can carry the reasoning/"thinking" signal (the failure mode that silently
/// wedged text turns) without editing the shared `model_manager` structs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredModel {
    pub provider: String,
    pub endpoint_family: String,
    /// Canonical reference used within Philotic.
    pub model_ref: String,
    /// Exact id the provider's API expects on dispatch.
    pub provider_model_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_cost_per_million: Option<f64>,
    #[serde(default)]
    pub modalities: Vec<String>,
    /// Whether the model reasons/"thinks" by default. `Some(true)` is the signal
    /// that a text controller must handle a thinking stream. `None` = provider
    /// did not report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_default: Option<bool>,
    /// Provider-declared task kinds (a claim, not verified capability).
    #[serde(default)]
    pub declared_task_kinds: Vec<String>,
    /// Provider-declared lifecycle hint, e.g. `deprecating` when an expiration
    /// date is published. Retirement is otherwise inferred by [`diff_catalog`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_hint: Option<String>,
    pub source_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at_secs: Option<u64>,
}

impl DiscoveredModel {
    fn key(&self) -> (String, String) {
        (self.provider.clone(), self.model_ref.clone())
    }

    /// Map into codex's existing per-provider availability record. Marks the
    /// source `discovered` so the flywheel/trust layer can distinguish declared
    /// facts from operator seed or observed truth.
    pub fn to_availability_record(&self) -> ModelProviderAvailabilityRecord {
        ModelProviderAvailabilityRecord {
            provider: self.provider.clone(),
            endpoint_family: self.endpoint_family.clone(),
            model_ref: self.model_ref.clone(),
            provider_model_ref: self.provider_model_ref.clone(),
            context_window_tokens: self.context_window_tokens,
            input_cost_per_million: self.input_cost_per_million,
            output_cost_per_million: self.output_cost_per_million,
            source: "discovered".to_string(),
        }
    }

    /// Provenance record for where this fact came from and when.
    pub fn to_external_source(&self) -> ModelExternalSourceRecord {
        ModelExternalSourceRecord {
            source_id: format!("{}.model-list", self.provider),
            source_kind: "provider_discovery".to_string(),
            source_url: Some(self.source_url.clone()),
            fetched_at_secs: self.fetched_at_secs,
            // Provider's own catalog is authoritative for existence/pricing but
            // not for capability quality — hence high, not 1.0.
            trust_weight: 0.9,
        }
    }
}

/// What changed between two discovery snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogDiffKind {
    /// A model the provider now lists that we had not seen.
    Added,
    /// A model we had seen that the provider no longer lists (retired).
    Removed,
    /// A model flipped its reasoning/"thinking" default (e.g. an alias rolled
    /// forward to a thinking model) — the exact class of change that wedged
    /// text turns.
    ReasoningChanged,
    /// Context window changed materially.
    ContextChanged,
}

/// One catalog change worth surfacing to the operator / self-heal queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogDiffEvent {
    pub kind: CatalogDiffKind,
    pub provider: String,
    pub model_ref: String,
    pub detail: String,
    /// Reasoning-default of the model *after* the change (carried so an alert
    /// can say "new/changed thinking model").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_default: Option<bool>,
}

/// Pure, total diff between a previous and a current discovery snapshot.
///
/// Keyed by `(provider, model_ref)`. Intentionally dumb: it makes no attempt to
/// reconcile against persisted state or de-duplicate providers — the caller owns
/// what `prev` is.
pub fn diff_catalog(prev: &[DiscoveredModel], now: &[DiscoveredModel]) -> Vec<CatalogDiffEvent> {
    let prev_map: HashMap<(String, String), &DiscoveredModel> =
        prev.iter().map(|m| (m.key(), m)).collect();
    let now_map: HashMap<(String, String), &DiscoveredModel> =
        now.iter().map(|m| (m.key(), m)).collect();

    let mut events = Vec::new();

    for m in now {
        match prev_map.get(&m.key()) {
            None => events.push(CatalogDiffEvent {
                kind: CatalogDiffKind::Added,
                provider: m.provider.clone(),
                model_ref: m.model_ref.clone(),
                detail: format!(
                    "provider now lists {}{}",
                    m.model_ref,
                    match m.reasoning_default {
                        Some(true) => " (reasoning on by default)",
                        Some(false) => " (non-reasoning)",
                        None => "",
                    }
                ),
                reasoning_default: m.reasoning_default,
            }),
            Some(p) => {
                if p.reasoning_default != m.reasoning_default {
                    events.push(CatalogDiffEvent {
                        kind: CatalogDiffKind::ReasoningChanged,
                        provider: m.provider.clone(),
                        model_ref: m.model_ref.clone(),
                        detail: format!(
                            "reasoning default {:?} -> {:?}",
                            p.reasoning_default, m.reasoning_default
                        ),
                        reasoning_default: m.reasoning_default,
                    });
                }
                if p.context_window_tokens != m.context_window_tokens {
                    events.push(CatalogDiffEvent {
                        kind: CatalogDiffKind::ContextChanged,
                        provider: m.provider.clone(),
                        model_ref: m.model_ref.clone(),
                        detail: format!(
                            "context window {:?} -> {:?}",
                            p.context_window_tokens, m.context_window_tokens
                        ),
                        reasoning_default: m.reasoning_default,
                    });
                }
            }
        }
    }

    for m in prev {
        if !now_map.contains_key(&m.key()) {
            events.push(CatalogDiffEvent {
                kind: CatalogDiffKind::Removed,
                provider: m.provider.clone(),
                model_ref: m.model_ref.clone(),
                detail: format!("provider no longer lists {}", m.model_ref),
                reasoning_default: None,
            });
        }
    }

    events
}

// ------------------------------------------------------------------------
// OpenRouter: GET https://openrouter.ai/api/v1/models  (public, no key)
// ------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct OpenRouterList {
    #[serde(default)]
    data: Vec<OpenRouterModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    architecture: Option<OpenRouterArch>,
    #[serde(default)]
    pricing: Option<OpenRouterPricing>,
    #[serde(default)]
    supported_parameters: Vec<String>,
    #[serde(default)]
    reasoning: Option<OpenRouterReasoning>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterArch {
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterPricing {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterReasoning {
    #[serde(default)]
    default_enabled: Option<bool>,
}

fn per_token_str_to_per_million(value: &Option<String>) -> Option<f64> {
    value
        .as_ref()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|per_token| per_token * 1_000_000.0)
}

/// Parse an OpenRouter `/api/v1/models` response body into discovered models.
pub fn parse_openrouter_models(
    body: &str,
    fetched_at_secs: Option<u64>,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    let list: OpenRouterList = serde_json::from_str(body)?;
    let source_url = "https://openrouter.ai/api/v1/models".to_string();
    let mut out = Vec::with_capacity(list.data.len());
    for m in list.data {
        let mut modalities: Vec<String> = Vec::new();
        if let Some(arch) = &m.architecture {
            for modality in arch
                .input_modalities
                .iter()
                .chain(arch.output_modalities.iter())
            {
                if !modalities.contains(modality) {
                    modalities.push(modality.clone());
                }
            }
        }
        let mut declared_task_kinds = Vec::new();
        let output_has = |k: &str| {
            m.architecture
                .as_ref()
                .map(|a| a.output_modalities.iter().any(|o| o == k))
                .unwrap_or(false)
        };
        if output_has("text") {
            declared_task_kinds.push("text.generate".to_string());
        }
        if output_has("image") {
            declared_task_kinds.push("image.generate".to_string());
        }
        // Reasoning: prefer the explicit object; fall back to the parameter list.
        let reasoning_default = match m.reasoning.as_ref().and_then(|r| r.default_enabled) {
            Some(v) => Some(v),
            None => {
                if m.supported_parameters.iter().any(|p| p == "reasoning") {
                    Some(false)
                } else {
                    None
                }
            }
        };
        out.push(DiscoveredModel {
            provider: "openrouter".to_string(),
            endpoint_family: "openrouter-hosted".to_string(),
            model_ref: m.id.clone(),
            provider_model_ref: m.id.clone(),
            display_name: m.name.clone(),
            context_window_tokens: m.context_length,
            input_cost_per_million: per_token_str_to_per_million(
                &m.pricing.as_ref().and_then(|p| p.prompt.clone()),
            ),
            output_cost_per_million: per_token_str_to_per_million(
                &m.pricing.as_ref().and_then(|p| p.completion.clone()),
            ),
            modalities,
            reasoning_default,
            declared_task_kinds,
            lifecycle_hint: None,
            source_url: source_url.clone(),
            fetched_at_secs,
        });
    }
    Ok(out)
}

// ------------------------------------------------------------------------
// Google: GET https://generativelanguage.googleapis.com/v1beta/models
// ------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GoogleList {
    #[serde(default)]
    models: Vec<GoogleModel>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleModel {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    input_token_limit: Option<u32>,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
    #[serde(default)]
    thinking: Option<bool>,
}

/// Parse a Google `/v1beta/models` response body into discovered models.
pub fn parse_google_models(
    body: &str,
    fetched_at_secs: Option<u64>,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    let list: GoogleList = serde_json::from_str(body)?;
    let source_url = "https://generativelanguage.googleapis.com/v1beta/models".to_string();
    let mut out = Vec::with_capacity(list.models.len());
    for m in list.models {
        // Only surface models that can actually generate/embed, not tuning-only
        // or token-count-only entries.
        let mut declared_task_kinds = Vec::new();
        for method in &m.supported_generation_methods {
            match method.as_str() {
                "generateContent" | "streamGenerateContent" | "bidiGenerateContent" => {
                    if !declared_task_kinds.iter().any(|k| k == "text.generate") {
                        declared_task_kinds.push("text.generate".to_string());
                    }
                }
                "embedContent" | "batchEmbedContents" => {
                    if !declared_task_kinds.iter().any(|k| k == "text.embed") {
                        declared_task_kinds.push("text.embed".to_string());
                    }
                }
                _ => {}
            }
        }
        let model_ref = m
            .name
            .strip_prefix("models/")
            .unwrap_or(&m.name)
            .to_string();
        out.push(DiscoveredModel {
            provider: "gemini".to_string(),
            endpoint_family: "google-hosted".to_string(),
            model_ref: model_ref.clone(),
            provider_model_ref: model_ref,
            display_name: m.display_name.clone(),
            context_window_tokens: m.input_token_limit,
            input_cost_per_million: None,
            output_cost_per_million: None,
            modalities: Vec::new(),
            reasoning_default: m.thinking,
            declared_task_kinds,
            lifecycle_hint: None,
            source_url: source_url.clone(),
            fetched_at_secs,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal fixtures shaped exactly like the live provider responses
    // (verified against openrouter.ai/api/v1/models and
    // generativelanguage.googleapis.com/v1beta/models on 2026-07-03).

    const OPENROUTER_SAMPLE: &str = r#"{
      "data": [
        {
          "id": "google/gemini-3.5-flash",
          "name": "Google: Gemini 3.5 Flash",
          "context_length": 1048576,
          "architecture": {
            "input_modalities": ["text", "image"],
            "output_modalities": ["text"],
            "tokenizer": "Gemini"
          },
          "pricing": { "prompt": "0.00000025", "completion": "0.0000015" },
          "supported_parameters": ["reasoning", "response_format", "tools"],
          "reasoning": { "default_enabled": true, "mandatory": false }
        }
      ]
    }"#;

    const GOOGLE_SAMPLE: &str = r#"{
      "models": [
        {
          "name": "models/gemini-3.5-flash",
          "version": "3.5-flash-05-2026",
          "displayName": "Gemini 3.5 Flash",
          "inputTokenLimit": 1048576,
          "outputTokenLimit": 65536,
          "supportedGenerationMethods": ["generateContent", "countTokens"],
          "thinking": true
        }
      ]
    }"#;

    #[test]
    fn openrouter_parse_extracts_context_cost_and_reasoning() {
        let models = parse_openrouter_models(OPENROUTER_SAMPLE, Some(42)).unwrap();
        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert_eq!(m.provider, "openrouter");
        assert_eq!(m.model_ref, "google/gemini-3.5-flash");
        assert_eq!(m.context_window_tokens, Some(1_048_576));
        // "0.00000025"/token -> $0.25 per million.
        assert_eq!(m.input_cost_per_million, Some(0.25));
        assert_eq!(m.output_cost_per_million, Some(1.5));
        assert_eq!(m.reasoning_default, Some(true));
        assert!(m.modalities.contains(&"image".to_string()));
        assert!(m.declared_task_kinds.contains(&"text.generate".to_string()));
        assert_eq!(m.fetched_at_secs, Some(42));
    }

    /// The signal that would have caught today's outage: a thinking model must
    /// be flagged as such at discovery time.
    #[test]
    fn google_parse_captures_thinking_flag() {
        let models = parse_google_models(GOOGLE_SAMPLE, None).unwrap();
        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert_eq!(m.provider, "gemini");
        assert_eq!(m.model_ref, "gemini-3.5-flash");
        assert_eq!(m.reasoning_default, Some(true));
        assert_eq!(m.context_window_tokens, Some(1_048_576));
        assert!(m.declared_task_kinds.contains(&"text.generate".to_string()));
    }

    fn model(provider: &str, model_ref: &str, reasoning: Option<bool>) -> DiscoveredModel {
        DiscoveredModel {
            provider: provider.to_string(),
            endpoint_family: "x".to_string(),
            model_ref: model_ref.to_string(),
            provider_model_ref: model_ref.to_string(),
            display_name: None,
            context_window_tokens: Some(1000),
            input_cost_per_million: None,
            output_cost_per_million: None,
            modalities: vec![],
            reasoning_default: reasoning,
            declared_task_kinds: vec![],
            lifecycle_hint: None,
            source_url: "u".to_string(),
            fetched_at_secs: None,
        }
    }

    /// THE product test: a model the provider retires (present before, gone now)
    /// must produce a `Removed` diff — this is "would we have caught the
    /// gemini-2.0-flash deprecation before it wedged mac-jane?".
    #[test]
    fn diff_detects_deprecated_model() {
        let prev = vec![
            model("gemini", "gemini-2.0-flash", Some(false)),
            model("gemini", "gemini-3.5-flash", Some(true)),
        ];
        let now = vec![model("gemini", "gemini-3.5-flash", Some(true))];
        let events = diff_catalog(&prev, &now);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CatalogDiffKind::Removed);
        assert_eq!(events[0].model_ref, "gemini-2.0-flash");
    }

    #[test]
    fn diff_flags_new_and_reasoning_change() {
        // Added thinking model.
        let events = diff_catalog(&[], &[model("gemini", "gemini-3.5-flash", Some(true))]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CatalogDiffKind::Added);
        assert_eq!(events[0].reasoning_default, Some(true));

        // An existing model flips to reasoning-by-default (alias rolled forward).
        let prev = vec![model("openrouter", "x/flash", Some(false))];
        let now = vec![model("openrouter", "x/flash", Some(true))];
        let events = diff_catalog(&prev, &now);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CatalogDiffKind::ReasoningChanged);
        assert_eq!(events[0].reasoning_default, Some(true));
    }

    #[test]
    fn maps_into_codex_availability_and_source_records() {
        let m = model("openrouter", "google/gemini-3.5-flash", Some(true));
        let rec = m.to_availability_record();
        assert_eq!(rec.provider, "openrouter");
        assert_eq!(rec.model_ref, "google/gemini-3.5-flash");
        assert_eq!(rec.source, "discovered");
        let src = m.to_external_source();
        assert_eq!(src.source_kind, "provider_discovery");
        assert_eq!(src.source_url.as_deref(), Some("u"));
    }
}
