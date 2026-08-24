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
    /// Whether the provider lists tool/function calling for this model
    /// (`supported_parameters` contains "tools" on OpenRouter). `Some(false)`
    /// means the parameter list was published WITHOUT tools — dispatch must
    /// strip tool declarations or the call 404s. `None` = not reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,
    /// Provider-declared task kinds (a claim, not verified capability).
    #[serde(default)]
    pub declared_task_kinds: Vec<String>,
    /// Provider-native task label retained verbatim for provenance. This is
    /// deliberately separate from `declared_task_kinds`, which contains only
    /// task kinds Philotic can map without inventing capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_task: Option<String>,
    /// Provider/model-card declared license. Absence means unreported, never
    /// "unlicensed" or permission to use the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub likes: Option<u64>,
    /// Provider-declared lifecycle hint, e.g. `deprecating` when an expiration
    /// date is published. Retirement is otherwise inferred by [`diff_catalog`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_hint: Option<String>,
    pub source_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at_secs: Option<u64>,
    /// Provider revision or immutable content identifier, when published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
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
        let supports_tools = if m.supported_parameters.is_empty() {
            None
        } else {
            Some(m.supported_parameters.iter().any(|p| p == "tools"))
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
            supports_tools,
            declared_task_kinds,
            provider_task: None,
            license: None,
            library_name: None,
            downloads: None,
            likes: None,
            lifecycle_hint: None,
            source_url: source_url.clone(),
            fetched_at_secs,
            source_revision: None,
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
            // Google's model list does not publish a tools flag.
            supports_tools: None,
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
            provider_task: None,
            license: None,
            library_name: None,
            downloads: None,
            likes: None,
            lifecycle_hint: None,
            source_url: source_url.clone(),
            fetched_at_secs,
            source_revision: None,
        });
    }
    Ok(out)
}

// ------------------------------------------------------------------------
// Hugging Face Hub: GET https://huggingface.co/api/models
// ------------------------------------------------------------------------

/// Defense in depth if the Hub ignores or changes the request's `limit` query.
pub const HUGGINGFACE_MODEL_LIMIT: usize = 100;

#[derive(Debug, Deserialize)]
struct HuggingFaceModel {
    id: String,
    #[serde(default)]
    pipeline_tag: Option<String>,
    #[serde(default)]
    library_name: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    downloads: Option<u64>,
    #[serde(default)]
    likes: Option<u64>,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    sha: Option<String>,
    #[serde(default, rename = "cardData")]
    card_data: Option<HuggingFaceCardData>,
}

#[derive(Debug, Deserialize)]
struct HuggingFaceCardData {
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    pipeline_tag: Option<String>,
    #[serde(default)]
    library_name: Option<String>,
}

fn huggingface_license(model: &HuggingFaceModel) -> Option<String> {
    model
        .card_data
        .as_ref()
        .and_then(|card| card.license.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            model.tags.iter().find_map(|tag| {
                tag.strip_prefix("license:")
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
        })
}

fn huggingface_task_metadata(task: &str) -> (Vec<String>, Vec<String>) {
    match task {
        "text-generation" => (vec!["text.generate".into()], vec!["text".into()]),
        "feature-extraction" | "sentence-similarity" => {
            (vec!["text.embed".into()], vec!["text".into()])
        }
        "automatic-speech-recognition" => (
            vec!["audio.transcribe".into()],
            vec!["audio".into(), "text".into()],
        ),
        "text-to-speech" => (
            vec!["voice.synthesize".into()],
            vec!["text".into(), "audio".into()],
        ),
        "text-to-image" => (
            vec!["image.generate".into()],
            vec!["text".into(), "image".into()],
        ),
        "image-to-text" => (
            vec!["media.analyze".into()],
            vec!["image".into(), "text".into()],
        ),
        _ => (Vec::new(), Vec::new()),
    }
}

/// Parse a bounded public Hugging Face Hub model-list response.
///
/// Private and disabled rows are ignored defensively. Provider-native task
/// labels are always retained, while Philotic task kinds are populated only
/// for conservative mappings we can state without treating popularity as
/// capability proof.
pub fn parse_huggingface_models(
    body: &str,
    fetched_at_secs: Option<u64>,
    source_url: &str,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    let models: Vec<HuggingFaceModel> = serde_json::from_str(body)?;
    Ok(models
        .into_iter()
        .filter(|model| !model.private && !model.disabled)
        .take(HUGGINGFACE_MODEL_LIMIT)
        .map(|model| {
            let provider_task = model.pipeline_tag.clone().or_else(|| {
                model
                    .card_data
                    .as_ref()
                    .and_then(|card| card.pipeline_tag.clone())
            });
            let (declared_task_kinds, modalities) = provider_task
                .as_deref()
                .map(huggingface_task_metadata)
                .unwrap_or_default();
            let library_name = model.library_name.clone().or_else(|| {
                model
                    .card_data
                    .as_ref()
                    .and_then(|card| card.library_name.clone())
            });
            DiscoveredModel {
                provider: "huggingface".into(),
                endpoint_family: "huggingface-hub".into(),
                model_ref: model.id.clone(),
                provider_model_ref: model.id.clone(),
                display_name: None,
                context_window_tokens: None,
                input_cost_per_million: None,
                output_cost_per_million: None,
                modalities,
                reasoning_default: None,
                supports_tools: None,
                declared_task_kinds,
                provider_task,
                license: huggingface_license(&model),
                library_name,
                downloads: model.downloads,
                likes: model.likes,
                lifecycle_hint: None,
                source_url: source_url.to_string(),
                fetched_at_secs,
                source_revision: model.sha,
            }
        })
        .collect())
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

    const HUGGINGFACE_SAMPLE: &str = r#"[
      {
        "id": "sentence-transformers/all-MiniLM-L6-v2",
        "cardData": {
          "license": "apache-2.0",
          "pipeline_tag": "sentence-similarity",
          "library_name": "sentence-transformers"
        },
        "downloads": 259675300,
        "likes": 5208,
        "private": false,
        "disabled": false,
        "sha": "1110a243fdf4706b3f48f1d95db1a4f5529b4d41",
        "tags": ["license:apache-2.0"]
      },
      {
        "id": "example/tag-license-only",
        "pipeline_tag": "text-generation",
        "private": false,
        "tags": ["transformers", "license:mit"]
      },
      {
        "id": "private/hidden",
        "private": true,
        "tags": ["license:other"]
      }
    ]"#;

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

    #[test]
    fn huggingface_parse_projects_task_license_popularity_and_revision() {
        let models = parse_huggingface_models(
            HUGGINGFACE_SAMPLE,
            Some(99),
            "https://huggingface.co/api/models",
        )
        .unwrap();
        assert_eq!(models.len(), 2, "private rows must not enter the catalog");
        let model = &models[0];
        assert_eq!(model.provider, "huggingface");
        assert_eq!(model.provider_task.as_deref(), Some("sentence-similarity"));
        assert_eq!(model.declared_task_kinds, ["text.embed"]);
        assert_eq!(model.license.as_deref(), Some("apache-2.0"));
        assert_eq!(model.library_name.as_deref(), Some("sentence-transformers"));
        assert_eq!(model.downloads, Some(259_675_300));
        assert_eq!(model.likes, Some(5_208));
        assert_eq!(model.fetched_at_secs, Some(99));
        assert_eq!(
            model.source_revision.as_deref(),
            Some("1110a243fdf4706b3f48f1d95db1a4f5529b4d41")
        );
        assert_eq!(models[1].license.as_deref(), Some("mit"));
        assert_eq!(models[1].declared_task_kinds, ["text.generate"]);
    }

    #[test]
    fn huggingface_parse_caps_rows_even_if_upstream_ignores_limit() {
        let body = serde_json::to_string(
            &(0..(HUGGINGFACE_MODEL_LIMIT + 7))
                .map(|i| serde_json::json!({ "id": format!("org/model-{i}") }))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            parse_huggingface_models(&body, None, "https://huggingface.co/api/models")
                .unwrap()
                .len(),
            HUGGINGFACE_MODEL_LIMIT
        );
    }

    fn model(provider: &str, model_ref: &str, reasoning: Option<bool>) -> DiscoveredModel {
        DiscoveredModel {
            supports_tools: None,
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
            provider_task: None,
            license: None,
            library_name: None,
            downloads: None,
            likes: None,
            lifecycle_hint: None,
            source_url: "u".to_string(),
            fetched_at_secs: None,
            source_revision: None,
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
