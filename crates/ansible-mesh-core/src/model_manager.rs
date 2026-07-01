use crate::registry::NodeRegistry;
use crate::runtime::ToolInvoker;
use crate::{graph::ModelProfileRecord, provider_keys::provider_key_specs};
use crate::{ModelRef, NodeId, ToolRef};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Configuration for a routing request to the model manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRouteConstraints {
    pub latency_ms: Option<u32>,
    pub privacy: Option<String>,
    pub cost_tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRouteRequest {
    pub task: String,
    pub constraints: ModelRouteConstraints,
    #[serde(default)]
    pub preferred_models: Vec<ModelRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRouteResponse {
    pub model_ref: ModelRef,
    pub endpoint_node: NodeId,
    pub invocation_params: Value,
}

/// Static, provider-neutral catalog fact for a model family.
///
/// This catalog intentionally avoids live availability, queue depth, auth, and
/// final turn routing. Join it with `ModelProfileRecord` for inspection; do not
/// treat it as dispatch authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelCatalogRecord {
    pub catalog_ref: String,
    pub provider: String,
    pub provider_display_name: String,
    pub endpoint_family: String,
    pub model_family: String,
    #[serde(default)]
    pub model_refs: Vec<String>,
    #[serde(default)]
    pub modalities: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<ModelCatalogCapability>,
    pub lifecycle: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelCatalogCapability {
    pub task_kind: String,
    pub score_hint: f32,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalogProjection {
    pub catalog: ModelCatalogRecord,
    #[serde(default)]
    pub live_profiles: Vec<ModelProfileRecord>,
}

pub fn seeded_model_catalog() -> Vec<ModelCatalogRecord> {
    let mut records = vec![
        catalog_record(
            "gemini",
            "google-hosted",
            "gemini",
            &["gemini", "gemini-flash-latest", "gemini-2.0-flash-exp"],
            &["text", "image", "audio"],
            &[
                ("text.generate", 0.82),
                ("media.analyze", 0.78),
                ("audio.transcribe", 0.65),
            ],
            "supported",
        ),
        catalog_record(
            "openai",
            "openai-hosted",
            "gpt",
            &["openai", "gpt-4.1-mini"],
            &["text", "image"],
            &[("text.generate", 0.84), ("response.generate", 0.84)],
            "supported",
        ),
        catalog_record(
            "openrouter",
            "openrouter-hosted",
            "openrouter-compatible",
            &["openrouter", "openai/gpt-4.1-mini"],
            &["text", "image"],
            &[("text.generate", 0.8), ("response.generate", 0.8)],
            "supported",
        ),
        catalog_record(
            "elevenlabs",
            "elevenlabs-hosted",
            "elevenlabs",
            &["elevenlabs"],
            &["audio"],
            &[("voice.synthesize", 0.82), ("audio.transcribe", 0.68)],
            "supported",
        ),
        catalog_record(
            "ollama",
            "ollama-compatible-local",
            "ollama-compatible",
            &["ollama"],
            &["text"],
            &[("text.generate", 0.62)],
            "supported",
        ),
        catalog_record(
            "onnx",
            "onnx-local",
            "florence",
            &["vision", "onnx-community/Florence-2-base-ft"],
            &["image"],
            &[("image.ocr", 0.62), ("image.ground", 0.62)],
            "experimental",
        ),
        catalog_record(
            "mlx",
            "mlx-local",
            "mlx",
            &["mlx"],
            &["text"],
            &[("text.generate", 0.64)],
            "experimental",
        ),
    ];
    records.sort_by(|a, b| a.catalog_ref.cmp(&b.catalog_ref));
    records
}

pub fn project_model_catalog(live_profiles: &[ModelProfileRecord]) -> Vec<ModelCatalogProjection> {
    seeded_model_catalog()
        .into_iter()
        .map(|catalog| {
            let live_profiles = live_profiles
                .iter()
                .filter(|profile| {
                    profile.provider == catalog.provider
                        || catalog.model_refs.iter().any(|model_ref| {
                            model_ref == &profile.model_ref || model_ref == &profile.provider
                        })
                })
                .cloned()
                .collect();
            ModelCatalogProjection {
                catalog,
                live_profiles,
            }
        })
        .collect()
}

fn catalog_record(
    provider: &str,
    endpoint_family: &str,
    model_family: &str,
    model_refs: &[&str],
    modalities: &[&str],
    capabilities: &[(&str, f32)],
    lifecycle: &str,
) -> ModelCatalogRecord {
    let provider_display_name = provider_key_specs()
        .iter()
        .find(|spec| spec.provider == provider)
        .map(|spec| spec.display_name)
        .unwrap_or(provider);
    ModelCatalogRecord {
        catalog_ref: format!("{provider}:{model_family}"),
        provider: provider.to_string(),
        provider_display_name: provider_display_name.to_string(),
        endpoint_family: endpoint_family.to_string(),
        model_family: model_family.to_string(),
        model_refs: model_refs.iter().map(|value| value.to_string()).collect(),
        modalities: modalities.iter().map(|value| value.to_string()).collect(),
        capabilities: capabilities
            .iter()
            .map(|(task_kind, score_hint)| ModelCatalogCapability {
                task_kind: task_kind.to_string(),
                score_hint: *score_hint,
                source: "seed".to_string(),
            })
            .collect(),
        lifecycle: lifecycle.to_string(),
        source: "seed".to_string(),
    }
}

/// A ToolInvoker that exposes `model.manager.*` capabilities.
pub struct ModelManagerInvoker {
    registry: Arc<RwLock<NodeRegistry>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_catalog_includes_current_provider_families() {
        let catalog = seeded_model_catalog();
        let providers: std::collections::BTreeSet<_> = catalog
            .iter()
            .map(|record| record.provider.as_str())
            .collect();
        for provider in [
            "gemini",
            "openai",
            "openrouter",
            "elevenlabs",
            "ollama",
            "onnx",
            "mlx",
        ] {
            assert!(providers.contains(provider), "missing {provider}");
        }
    }

    #[test]
    fn projection_joins_live_profile_without_changing_catalog() {
        let projections = project_model_catalog(&[ModelProfileRecord {
            model_ref: "openrouter".into(),
            node_id: "bjork".into(),
            provider: "openrouter".into(),
            task_kinds: vec!["text.generate".into()],
            trust_tier: "remote_cloud".into(),
            max_context_tokens: 0,
            latency_p50_ms: 1234,
            error_rate: 0.2,
            status: "healthy".into(),
            last_healthy_secs: 1,
            updated_secs: 2,
        }]);
        let openrouter = projections
            .iter()
            .find(|projection| projection.catalog.provider == "openrouter")
            .expect("openrouter projection");
        assert_eq!(openrouter.live_profiles.len(), 1);
        assert_eq!(openrouter.live_profiles[0].latency_p50_ms, 1234);
    }
}

impl ModelManagerInvoker {
    pub fn new(registry: Arc<RwLock<NodeRegistry>>) -> Self {
        Self { registry }
    }

    async fn handle_list(&self) -> Result<Value> {
        let registry = self.registry.read().await;
        let mut available_models = vec![];

        for node in registry.active_nodes() {
            if node
                .capabilities
                .roles
                .contains(&crate::NodeRole::ModelNode)
                || !node.capabilities.models.is_empty()
            {
                for model in &node.capabilities.models {
                    available_models.push(json!({
                        "model_ref": model,
                        "node_id": node.capabilities.node_id,
                    }));
                }
            }
        }

        Ok(json!({
            "status": "success",
            "models": available_models,
        }))
    }

    async fn handle_route(&self, args: Value) -> Result<Value> {
        let req: ModelRouteRequest = serde_json::from_value(args.clone())?;
        let registry = self.registry.read().await;

        // Simplified routing logic for MVP 2:
        // Try to find the first node that supports one of the preferred models.
        for pref in &req.preferred_models {
            for node in registry.active_nodes() {
                if node.capabilities.models.contains(pref) {
                    let resp = ModelRouteResponse {
                        model_ref: pref.clone(),
                        endpoint_node: node.capabilities.node_id.clone(),
                        invocation_params: json!({"max_tokens": 256, "temperature": 0.4}),
                    };
                    return Ok(serde_json::to_value(resp)?);
                }
            }
        }

        bail!("No active nodes found matching the requested model constraints")
    }
}

// In a real async trait, we'd use async_trait, but since ToolInvoker is currently sync,
// we block or bridge it. For MVP 2 we will change ToolInvoker to be async if needed,
// or run this in a blocking thread. We'll stub it sync for the trait.
impl ToolInvoker for ModelManagerInvoker {
    fn call_tool(&self, tool: ToolRef, args: Value) -> Result<Value> {
        let _registry_clone = self.registry.clone();

        // Blocking bridge since the trait is sync in MVP 1
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                match tool.as_str() {
                    "model.manager.list@1" => self.handle_list().await,
                    "model.manager.route@1" => self.handle_route(args).await,
                    _ => bail!("Unknown model manager tool: {}", tool),
                }
            })
        })
    }
}
