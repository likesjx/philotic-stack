use crate::graph::{MemoryApartment, MemoryEntry};
use crate::runtime::ToolInvoker;
use crate::{AgentId, ToolRef};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// MVP 3 Context Graph Node implementation that provides memory capability tools.
pub struct ContextGraphInvoker {
    apartments: Arc<RwLock<HashMap<AgentId, MemoryApartment>>>,
}

impl ContextGraphInvoker {
    pub fn new() -> Self {
        Self {
            apartments: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn read_memory(&self, args: Value) -> Result<Value> {
        info!("Executing memory.read with args: {}", args);
        let agent_id = args
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let lock = self.apartments.read().await;
        if let Some(apt) = lock.get(agent_id) {
            Ok(serde_json::to_value(apt)?)
        } else {
            Ok(json!({
                "status": "not_found",
                "message": format!("No memory apartment found for agent {}", agent_id)
            }))
        }
    }

    async fn write_memory(&self, args: Value) -> Result<Value> {
        info!("Executing memory.write with args: {}", args);
        let agent_id = args
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let entry: MemoryEntry =
            serde_json::from_value(args.get("entry").cloned().unwrap_or_default())?;

        let mut lock = self.apartments.write().await;
        let apt = lock
            .entry(agent_id.to_string())
            .or_insert_with(|| MemoryApartment {
                id: format!("agent:{}:memory", agent_id),
                kind: "MemoryApartment".to_string(),
                owner_agent: agent_id.to_string(),
                entries: vec![],
            });

        apt.entries.push(entry.clone());

        Ok(json!({
            "status": "success",
            "entry_id": entry.id
        }))
    }
}

impl ToolInvoker for ContextGraphInvoker {
    fn call_tool(&self, tool: ToolRef, args: Value) -> Result<Value> {
        // Bridging async for MVP 1 synchronous trait definition
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                match tool.as_str() {
                    "memory.read@1" => self.read_memory(args).await,
                    "memory.write@1" => self.write_memory(args).await,
                    _ => bail!("Unknown or unsupported memory tool reference: {}", tool),
                }
            })
        })
    }
}
