use crate::{ModelRef, NodeId, ToolRef};
use crate::runtime::ToolInvoker;
use crate::registry::NodeRegistry;
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

/// A ToolInvoker that exposes `model.manager.*` capabilities.
pub struct ModelManagerInvoker {
    registry: Arc<RwLock<NodeRegistry>>,
}

impl ModelManagerInvoker {
    pub fn new(registry: Arc<RwLock<NodeRegistry>>) -> Self {
        Self { registry }
    }

    async fn handle_list(&self) -> Result<Value> {
        let registry = self.registry.read().await;
        let mut available_models = vec![];

        for node in registry.active_nodes() {
            if node.capabilities.roles.contains(&crate::NodeRole::ModelNode) || !node.capabilities.models.is_empty() {
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
