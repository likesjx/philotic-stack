use crate::ModelRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Provider-neutral catalog for the models Philotic knows how to talk about.
///
/// This is static capability metadata, not live routing truth. Node availability,
/// auth material, and runtime health remain owned by the hotel/runtime surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCatalog {
    pub version: String,
    #[serde(default)]
    pub capability_tree: Vec<ModelCapabilityNode>,
    #[serde(default)]
    pub providers: Vec<ModelProviderRecord>,
    #[serde(default)]
    pub models: Vec<ModelRecord>,
}

impl ModelCatalog {
    pub fn validate(&self) -> Result<(), String> {
        let mut capability_ids = BTreeSet::new();
        for node in &self.capability_tree {
            node.collect_ids(&mut capability_ids)?;
        }

        let provider_ids = self
            .providers
            .iter()
            .map(|provider| provider.provider_id.as_str())
            .collect::<BTreeSet<_>>();

        let endpoint_ids = self
            .providers
            .iter()
            .flat_map(|provider| {
                provider
                    .endpoints
                    .iter()
                    .map(|endpoint| endpoint.endpoint_id.as_str())
            })
            .collect::<BTreeSet<_>>();

        let mut model_refs = BTreeSet::new();
        for model in &self.models {
            if !provider_ids.contains(model.provider_id.as_str()) {
                return Err(format!(
                    "model [{}] references unknown provider [{}]",
                    model.model_ref, model.provider_id
                ));
            }

            for capability in &model.capabilities {
                if !capability_ids.contains(capability.as_str()) {
                    return Err(format!(
                        "model [{}] references unknown capability [{}]",
                        model.model_ref, capability
                    ));
                }
            }

            for endpoint_ref in &model.endpoint_refs {
                if !endpoint_ids.contains(endpoint_ref.as_str()) {
                    return Err(format!(
                        "model [{}] references unknown endpoint [{}]",
                        model.model_ref, endpoint_ref
                    ));
                }
            }

            if !model_refs.insert(model.model_ref.as_str()) {
                return Err(format!("duplicate model_ref [{}]", model.model_ref));
            }

            let mut variant_ids = BTreeSet::new();
            for variant in &model.variants {
                if !variant_ids.insert(variant.variant_id.as_str()) {
                    return Err(format!(
                        "model [{}] has duplicate variant [{}]",
                        model.model_ref, variant.variant_id
                    ));
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilityNode {
    pub capability_id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub children: Vec<ModelCapabilityNode>,
}

impl ModelCapabilityNode {
    fn collect_ids<'a>(&'a self, acc: &mut BTreeSet<&'a str>) -> Result<(), String> {
        if !acc.insert(self.capability_id.as_str()) {
            return Err(format!(
                "duplicate capability_id [{}] in capability tree",
                self.capability_id
            ));
        }

        for child in &self.children {
            child.collect_ids(acc)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelProviderRecord {
    pub provider_id: String,
    pub display_name: String,
    #[serde(default)]
    pub auth_families: Vec<String>,
    #[serde(default)]
    pub endpoints: Vec<ModelEndpointRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelEndpointRecord {
    pub endpoint_id: String,
    pub api_family: String,
    pub transport: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub path_stem: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRecord {
    pub model_ref: ModelRef,
    pub provider_id: String,
    pub display_name: String,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub variant_group: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub endpoint_refs: Vec<String>,
    pub weights: ModelScoreWeights,
    #[serde(default)]
    pub context_window_tokens: Option<u32>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub output_modalities: Vec<String>,
    #[serde(default)]
    pub variants: Vec<ModelVariantRecord>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelScoreWeights {
    #[serde(default)]
    pub capability: Option<u8>,
    #[serde(default)]
    pub speed: Option<u8>,
    #[serde(default)]
    pub thinking: Option<u8>,
    #[serde(default)]
    pub cost_efficiency: Option<u8>,
    #[serde(default)]
    pub tool_use: Option<u8>,
    #[serde(default)]
    pub audio_native: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelVariantRecord {
    pub variant_id: String,
    pub display_name: String,
    pub weights: ModelScoreWeights,
    #[serde(default)]
    pub notes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_catalog() -> ModelCatalog {
        ModelCatalog {
            version: "2026-03-26".into(),
            capability_tree: vec![ModelCapabilityNode {
                capability_id: "text.generate".into(),
                display_name: "Text Generate".into(),
                description: None,
                children: vec![ModelCapabilityNode {
                    capability_id: "response.generate".into(),
                    display_name: "Response Generate".into(),
                    description: None,
                    children: vec![],
                }],
            }],
            providers: vec![ModelProviderRecord {
                provider_id: "gemini".into(),
                display_name: "Gemini".into(),
                auth_families: vec!["api_key".into()],
                endpoints: vec![ModelEndpointRecord {
                    endpoint_id: "gemini-rest".into(),
                    api_family: "gemini-rest".into(),
                    transport: "https-json".into(),
                    base_url: Some("https://generativelanguage.googleapis.com".into()),
                    path_stem: Some("/v1beta/models/{model}:generateContent".into()),
                    notes: None,
                }],
            }],
            models: vec![ModelRecord {
                model_ref: "gemini-flash-latest".into(),
                provider_id: "gemini".into(),
                display_name: "Gemini Flash Latest".into(),
                family: Some("gemini-flash".into()),
                variant_group: None,
                capabilities: vec!["text.generate".into()],
                endpoint_refs: vec!["gemini-rest".into()],
                weights: ModelScoreWeights {
                    capability: Some(4),
                    speed: Some(5),
                    thinking: Some(3),
                    cost_efficiency: Some(4),
                    tool_use: Some(4),
                    audio_native: None,
                },
                context_window_tokens: Some(128_000),
                max_output_tokens: Some(8_192),
                input_modalities: vec!["text".into()],
                output_modalities: vec!["text".into()],
                variants: vec![ModelVariantRecord {
                    variant_id: "default".into(),
                    display_name: "Default".into(),
                    weights: ModelScoreWeights::default(),
                    notes: None,
                }],
                notes: None,
            }],
        }
    }

    #[test]
    fn validates_well_formed_catalog() {
        sample_catalog().validate().unwrap();
    }

    #[test]
    fn rejects_unknown_capability_reference() {
        let mut catalog = sample_catalog();
        catalog.models[0].capabilities.push("voice.dialogue".into());
        let err = catalog.validate().unwrap_err();
        assert!(err.contains("unknown capability"));
    }

    #[test]
    fn rejects_duplicate_model_refs() {
        let mut catalog = sample_catalog();
        catalog.models.push(catalog.models[0].clone());
        let err = catalog.validate().unwrap_err();
        assert!(err.contains("duplicate model_ref"));
    }
}
