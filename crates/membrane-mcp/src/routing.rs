//! In-memory routing tables for the MCP membrane.
//!
//! Two tables coexist:
//! - `RoutingTable` — legacy per-agent route records pushed via `UpdateMcpRoutes`
//! - `McpEndpointTable` — config-driven table pushed via `update_mcp_config`
//!
//! `McpEndpointTable` takes priority for `tools/list` and `tools/call` when
//! an endpoint config is present. The legacy table acts as fallback.

use ansible_mesh_core::mcp_endpoint::{McpEndpointConfig, McpToolSpec};
use ansible_mesh_core::mcp_route::McpRouteRecord;
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

    /// All routes that a caller is authorized to see.
    ///
    /// This filters descriptor visibility before `tools/list` so public MCP
    /// surfaces do not disclose tool names to callers without a valid grant.
    pub fn visible_descriptors_by(
        &self,
        mut is_visible: impl FnMut(&ResolvedRoute) -> bool,
    ) -> Vec<McpToolDescriptor> {
        self.routes
            .values()
            .filter(|route| is_visible(route))
            .map(|route| route.as_descriptor())
            .collect()
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
    /// A late or reordered maintenance push cannot overwrite a newer config.
    /// Refusal keeps the old finite approval; persistence is not an apply ACK.
    pub fn extend_native_preapprovals(&mut self, next: McpEndpointConfig) -> bool {
        let Some(current) = self.config.as_ref() else {
            return false;
        };
        if current == &next {
            return true;
        }
        if next.updated_at <= current.updated_at
            || current.preapproval_rules.len() != 2
            || next.preapproval_rules.len() != 2
        {
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut unchanged = next.clone();
        for (old, new) in current
            .preapproval_rules
            .iter()
            .zip(&next.preapproval_rules)
        {
            if !matches!(
                old.action_pattern.as_str(),
                "native-memory.remember" | "native-memory.recall"
            ) || !matches!(
                &old.target,
                Some(ansible_mesh_core::mcp_route::McpRouteTarget::NativeMemory { .. })
            ) || !matches!((old.expires_at,new.expires_at),(Some(a),Some(b)) if b>a && b>now && b<=now.saturating_add(86400))
            {
                return false;
            }
        }
        unchanged.preapproval_rules = current.preapproval_rules.clone();
        unchanged.updated_at = current.updated_at;
        if &unchanged != current {
            return false;
        }
        for (old, new) in current
            .preapproval_rules
            .iter()
            .zip(&next.preapproval_rules)
        {
            if old.action_pattern != new.action_pattern
                || old.target != new.target
                || new.approved_by_turn.trim().is_empty()
            {
                return false;
            }
        }
        self.config = Some(next);
        true
    }
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

    /// The full endpoint config, when one has been pushed.
    pub fn config(&self) -> Option<&McpEndpointConfig> {
        self.config.as_ref()
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

        self.config.as_ref().is_some_and(|c| {
            c.preapproval_rules.iter().any(|rule| {
                if let Some(exp) = rule.expires_at
                    && now > exp
                {
                    return false;
                }
                rule.action_pattern == "*" || rule.action_pattern == action
            })
        })
    }
}

pub type SharedEndpointTable = Arc<RwLock<McpEndpointTable>>;

pub fn new_shared_endpoint_table() -> SharedEndpointTable {
    Arc::new(RwLock::new(McpEndpointTable::default()))
}

#[cfg(test)]
mod native_maintenance_tests {
    use super::*;
    fn config() -> McpEndpointConfig {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        serde_json::from_value(serde_json::json!({"endpoint_id":"pilot","owner_agent_id":"owner","port":19101,"tools":[],"updated_at":now,
            "preapproval_rules":[
                {"action_pattern":"native-memory.remember","target":{"kind":"native_memory","runner_id":"runner","target_node":"hotel"},"approved_by_turn":"old","approved_at":now,"expires_at":now+10},
                {"action_pattern":"native-memory.recall","target":{"kind":"native_memory","runner_id":"runner","target_node":"hotel"},"approved_by_turn":"old","approved_at":now,"expires_at":now+10}]})).unwrap()
    }
    fn extend(mut c: McpEndpointConfig, seconds: u64) -> McpEndpointConfig {
        c.updated_at += seconds;
        for rule in &mut c.preapproval_rules {
            rule.expires_at = Some(rule.expires_at.unwrap() + seconds);
            rule.approved_by_turn = "renewal".into();
        }
        c
    }
    #[test]
    fn native_maintenance_push_reorders_duplicates_and_revocation_fail_closed() {
        let old = config();
        let next = extend(old.clone(), 100);
        let latest = extend(next.clone(), 100);
        let mut table = McpEndpointTable::default();
        table.update(old);
        assert!(table.extend_native_preapprovals(latest.clone()));
        assert!(!table.extend_native_preapprovals(next));
        assert!(table.extend_native_preapprovals(latest.clone()));
        assert_eq!(table.config(), Some(&latest));
        table.revoke();
        assert!(!table.extend_native_preapprovals(latest.clone()));
        // Full provisioning mints a new incarnation even for an otherwise
        // identical baseline. A delayed old approval cannot cross it.
        let mut recreated = config();
        recreated.approval_incarnation = Some("new-hotel-incarnation".into());
        table.update(recreated.clone());
        assert!(!table.extend_native_preapprovals(latest));
        assert_eq!(table.config(), Some(&recreated));
    }
    #[test]
    fn native_maintenance_push_cannot_change_scopes_routes_auth_or_listener() {
        for case in 0..6 {
            let old = config();
            let mut next = extend(old.clone(), 100);
            match case {
                0 => next.port += 1,
                1 => next.allow_unauthenticated = true,
                2 => next.owner_agent_id = "other".into(),
                3 => next.preapproval_rules[0].action_pattern = "*".into(),
                4 => next.preapproval_rules[0].target = None,
                _ => next.preapproval_rules[0].expires_at = None,
            }
            let mut table = McpEndpointTable::default();
            table.update(old.clone());
            assert!(!table.extend_native_preapprovals(next));
            assert_eq!(table.config(), Some(&old));
        }
    }
}
