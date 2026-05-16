//! In-memory routing tables for the MCP membrane.
//!
//! Two tables coexist:
//! - `RoutingTable` — legacy per-agent route records pushed via `UpdateMcpRoutes`
//! - `McpEndpointTable` — config-driven table pushed via `update_mcp_config`
//!
//! `McpEndpointTable` takes priority for `tools/list` and `tools/call` when
//! an endpoint config is present. The legacy table acts as fallback.

use ansible_mesh_core::mcp_endpoint::{McpEndpointConfig, McpPreapprovalRule, McpToolSpec};
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

// ── Config-driven endpoint table ──────────────────────────────────────────────

/// Holds the current `McpEndpointConfig` pushed from the hotel.
/// One config per membrane-mcp instance (each instance manages one endpoint).
#[derive(Debug, Default)]
pub struct McpEndpointTable {
    config: Option<McpEndpointConfig>,
}

impl McpEndpointTable {
    pub fn update(&mut self, config: McpEndpointConfig) {
        info!(
            endpoint_id = %config.endpoint_id,
            tools = config.tools.len(),
            "endpoint config updated"
        );
        self.config = Some(config);
    }

    pub fn revoke(&mut self) {
        if let Some(ref c) = self.config {
            info!(endpoint_id = %c.endpoint_id, "endpoint config revoked");
        }
        self.config = None;
    }

    pub fn is_active(&self) -> bool {
        self.config.is_some()
    }

    /// All tool specs as MCP descriptors for `tools/list`.
    pub fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        self.config.as_ref().map_or(vec![], |c| {
            c.tools
                .iter()
                .map(|t| McpToolDescriptor {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    input_schema: t.input_schema.clone(),
                })
                .collect()
        })
    }

    /// Look up a tool spec by name.
    pub fn find_tool(&self, name: &str) -> Option<&McpToolSpec> {
        self.config.as_ref()?.tools.iter().find(|t| t.name == name)
    }

    /// Returns `true` if a pre-approval rule matches the given action.
    /// A rule matches if its `action_pattern` equals the action exactly or is `"*"`.
    /// Expired rules (by unix epoch) are skipped.
    pub fn is_preapproved(&self, action: &str) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        self.config.as_ref().map_or(false, |c| {
            c.preapproval_rules.iter().any(|rule| {
                if let Some(exp) = rule.expires_at {
                    if now > exp {
                        return false;
                    }
                }
                rule.action_pattern == "*" || rule.action_pattern == action
            })
        })
    }

    pub fn preapproval_rules(&self) -> &[McpPreapprovalRule] {
        self.config
            .as_ref()
            .map_or(&[], |c| c.preapproval_rules.as_slice())
    }
}

pub type SharedEndpointTable = Arc<RwLock<McpEndpointTable>>;

pub fn new_shared_endpoint_table() -> SharedEndpointTable {
    Arc::new(RwLock::new(McpEndpointTable::default()))
}
