use crate::runtime::ToolInvoker;
use crate::ToolRef;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use tracing::info;

/// A ToolInvoker that exposes `ansible.mesh.*` capabilities.
/// This acts as a wrapper proxying to the instantaneous Ansible Mesh framework
/// (the quantum-networking inspired meshops routing layer).
pub struct MeshopsNodeInvoker;

impl MeshopsNodeInvoker {
    pub fn new() -> Self {
        Self
    }

    fn broadcast(&self, args: Value) -> Result<Value> {
        info!("Executing ansible.mesh.broadcast with args: {}", args);
        // MVP 2 mock
        Ok(json!({
            "status": "success",
            "message": "Broadcast sent across the Ansible mesh.",
            "reach": "global"
        }))
    }

    fn locate_agent(&self, args: Value) -> Result<Value> {
        info!("Executing ansible.mesh.locate_agent with args: {}", args);
        let target_agent = args
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        Ok(json!({
            "status": "found",
            "agent_id": target_agent,
            "node_ref": "mock-edge-node"
        }))
    }
}

impl ToolInvoker for MeshopsNodeInvoker {
    fn call_tool(&self, tool: ToolRef, args: Value) -> Result<Value> {
        match tool.as_str() {
            "ansible.mesh.broadcast@1" => self.broadcast(args),
            "ansible.mesh.locate_agent@1" => self.locate_agent(args),
            _ => bail!("Unknown or unsupported meshops tool reference: {}", tool),
        }
    }
}
