//! In-memory routing table for the MCP membrane.
//!
//! The table is keyed by `tool_name`. Multiple agents can contribute routes;
//! each route is tagged with its owning agent. The hotel pushes updates via
//! `IpcRequest::UpdateMcpRoutes` / `IpcRequest::RevokeMcpRoutes`.

use ansible_mesh_core::mcp_route::{McpRouteRecord, McpRouteTarget};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::protocol::McpToolDescriptor;

// ── Resolved route ────────────────────────────────────────────────────────────

/// A route as it lives in the hot-path table: record + owning agent.
#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    pub owner_agent_id: String,
    pub record: McpRouteRecord,
}

impl ResolvedRoute {
    pub fn tool_name(&self) -> &str {
        &self.record.tool_name
    }

    pub fn target(&self) -> &McpRouteTarget {
        &self.record.target
    }

    pub fn as_descriptor(&self) -> McpToolDescriptor {
        McpToolDescriptor {
            name: self.record.tool_name.clone(),
            description: self.record.description.clone(),
            input_schema: self.record.input_schema.clone(),
        }
    }
}

// ── Routing table ─────────────────────────────────────────────────────────────

/// Thread-safe routing table. Wrapped in `Arc` by `MembraneState`.
#[derive(Debug, Default)]
pub struct RoutingTable {
    /// tool_name → resolved route
    routes: HashMap<String, ResolvedRoute>,
    /// agent_id → set of tool names owned by that agent (for bulk revoke)
    agent_index: HashMap<String, Vec<String>>,
}

impl RoutingTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the full route set for an agent (LWW per tool_name).
    pub fn upsert_agent_routes(&mut self, agent_id: &str, records: Vec<McpRouteRecord>) {
        // Remove existing routes owned by this agent.
        if let Some(old_tools) = self.agent_index.remove(agent_id) {
            for tool_name in old_tools {
                let should_remove = self
                    .routes
                    .get(&tool_name)
                    .map(|route| route.owner_agent_id == agent_id)
                    .unwrap_or(false);
                if should_remove {
                    self.routes.remove(&tool_name);
                }
            }
        }

        let mut tool_names = Vec::with_capacity(records.len());
        for record in records {
            tool_names.push(record.tool_name.clone());
            self.routes.insert(
                record.tool_name.clone(),
                ResolvedRoute {
                    owner_agent_id: agent_id.to_string(),
                    record,
                },
            );
        }
        self.agent_index.insert(agent_id.to_string(), tool_names);

        info!(
            agent_id,
            route_count = self.routes.len(),
            "routing table updated"
        );
    }

    /// Remove all routes owned by an agent.
    pub fn revoke_agent_routes(&mut self, agent_id: &str) {
        if let Some(tool_names) = self.agent_index.remove(agent_id) {
            for tool_name in tool_names {
                let should_remove = self
                    .routes
                    .get(&tool_name)
                    .map(|route| route.owner_agent_id == agent_id)
                    .unwrap_or(false);
                if should_remove {
                    self.routes.remove(&tool_name);
                }
            }
        }
        info!(agent_id, "routes revoked");
    }

    /// Look up a route by tool name.
    pub fn get(&self, tool_name: &str) -> Option<&ResolvedRoute> {
        self.routes.get(tool_name)
    }

    /// All routes as MCP tool descriptors (for tools/list).
    pub fn all_descriptors(&self) -> Vec<McpToolDescriptor> {
        self.routes.values().map(|r| r.as_descriptor()).collect()
    }

    /// All routes that a caller is authorized to see.
    ///
    /// For now this returns all routes — the auth layer enforces
    /// per-call authorization. Future slice: filter by caller scopes here.
    pub fn visible_descriptors(&self, _caller_id: Option<&str>) -> Vec<McpToolDescriptor> {
        self.all_descriptors()
    }
}

/// Shared handle to the routing table.
pub type SharedRoutingTable = Arc<RwLock<RoutingTable>>;

pub fn new_shared_table() -> SharedRoutingTable {
    Arc::new(RwLock::new(RoutingTable::new()))
}
