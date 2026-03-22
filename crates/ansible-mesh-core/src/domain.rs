//! GraphDomain — reusable middle layer between callers and the graph adapter backend.
//!
//! `GraphDomain` expresses domain operations purely in terms of [`GraphAdapter`]
//! node/edge primitives. No SQL, no direct backend access, no schema duplication.
//!
//! All graph stores in the system (hotel CG, agent graph, trace store) hold a
//! `GraphDomain` over their own `GraphAdapter` instance. Adding a new entity type
//! means adding kind constants and domain methods here — the backend never changes.
//!
//! # Node key convention
//!
//! Node keys follow `"{kind}:{id}"` — e.g. `"hotel:default"`,
//! `"abstract_tool:bash.exec"`, `"rule:rule-001"`. This keeps keys globally
//! unique within a store and makes kind-scoped lookups fast.

use crate::graph::{AbstractToolRecord, GraphNode, RuleRecord};
use crate::storage::{GraphAdapter, HotelRecord};
use anyhow::{Context, Result};
use std::sync::Arc;

// ── Kind constants ────────────────────────────────────────────────────────────
//
// These are the shared data vocabulary for all graph stores in the system.
// When a new entity type is added, add its kind constant here first.

pub const NODE_KIND_HOTEL: &str = "hotel";
pub const NODE_KIND_ABSTRACT_TOOL: &str = "abstract_tool";
pub const NODE_KIND_RULE: &str = "rule";

// ── GraphDomain ───────────────────────────────────────────────────────────────

/// Domain-operation layer over a generic [`GraphAdapter`].
///
/// All persistence is expressed via `GraphNode` upserts and queries on the
/// adapter. Callers hold `Arc<GraphDomain>` and never interact with the adapter
/// directly.
pub struct GraphDomain {
    adapter: Arc<dyn GraphAdapter>,
}

impl GraphDomain {
    /// Construct a domain layer backed by `adapter`.
    pub fn new(adapter: Arc<dyn GraphAdapter>) -> Self {
        Self { adapter }
    }

    // ── Node key helpers ──────────────────────────────────────────────────────

    fn hotel_key(hotel_name: &str) -> String {
        format!("{}:{}", NODE_KIND_HOTEL, hotel_name)
    }

    fn abstract_tool_key(tool_name: &str) -> String {
        format!("{}:{}", NODE_KIND_ABSTRACT_TOOL, tool_name)
    }

    fn rule_key(rule_id: &str) -> String {
        format!("{}:{}", NODE_KIND_RULE, rule_id)
    }

    // ── Hotel methods ─────────────────────────────────────────────────────────

    /// Upsert a hotel record as a graph node.
    pub fn upsert_hotel(&self, hotel: &HotelRecord) -> Result<()> {
        let data = serde_json::to_value(hotel)
            .context("GraphDomain::upsert_hotel: serialize HotelRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::hotel_key(&hotel.hotel_name),
            kind: NODE_KIND_HOTEL.to_string(),
            label: Some(hotel.hotel_name.clone()),
            data,
        })
    }

    /// Load a hotel record by name.
    pub fn get_hotel(&self, hotel_name: &str) -> Result<Option<HotelRecord>> {
        match self.adapter.get_node(&Self::hotel_key(hotel_name))? {
            None => Ok(None),
            Some(node) => {
                let record = serde_json::from_value(node.data)
                    .context("GraphDomain::get_hotel: deserialize HotelRecord")?;
                Ok(Some(record))
            }
        }
    }

    /// List all hotel records.
    pub fn list_hotels(&self) -> Result<Vec<HotelRecord>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_HOTEL)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_hotels: deserialize HotelRecord")
            })
            .collect()
    }

    // ── Abstract tool methods ─────────────────────────────────────────────────

    /// Upsert an abstract tool record as a graph node.
    pub fn upsert_abstract_tool(&self, tool: &AbstractToolRecord) -> Result<()> {
        let data = serde_json::to_value(tool)
            .context("GraphDomain::upsert_abstract_tool: serialize AbstractToolRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::abstract_tool_key(&tool.tool_name),
            kind: NODE_KIND_ABSTRACT_TOOL.to_string(),
            label: Some(tool.tool_name.clone()),
            data,
        })
    }

    /// Load an abstract tool record by tool name.
    pub fn get_abstract_tool(&self, tool_name: &str) -> Result<Option<AbstractToolRecord>> {
        match self.adapter.get_node(&Self::abstract_tool_key(tool_name))? {
            None => Ok(None),
            Some(node) => {
                let record = serde_json::from_value(node.data)
                    .context("GraphDomain::get_abstract_tool: deserialize AbstractToolRecord")?;
                Ok(Some(record))
            }
        }
    }

    /// List all abstract tool records.
    pub fn list_abstract_tools(&self) -> Result<Vec<AbstractToolRecord>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_ABSTRACT_TOOL)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_abstract_tools: deserialize AbstractToolRecord")
            })
            .collect()
    }

    // ── Rule methods ──────────────────────────────────────────────────────────

    /// Upsert a rule record as a graph node.
    pub fn upsert_rule(&self, rule: &RuleRecord) -> Result<()> {
        let data = serde_json::to_value(rule)
            .context("GraphDomain::upsert_rule: serialize RuleRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::rule_key(&rule.rule_id),
            kind: NODE_KIND_RULE.to_string(),
            label: Some(rule.rule_id.clone()),
            data,
        })
    }

    /// Load a rule record by rule_id.
    pub fn get_rule(&self, rule_id: &str) -> Result<Option<RuleRecord>> {
        match self.adapter.get_node(&Self::rule_key(rule_id))? {
            None => Ok(None),
            Some(node) => {
                let record = serde_json::from_value(node.data)
                    .context("GraphDomain::get_rule: deserialize RuleRecord")?;
                Ok(Some(record))
            }
        }
    }

    /// List all rules owned by `agent_id`.
    ///
    /// Loads all rule nodes and filters in Rust — no SQL predicate required.
    pub fn list_rules(&self, agent_id: &str) -> Result<Vec<RuleRecord>> {
        let mut rules = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_RULE)? {
            let record: RuleRecord = serde_json::from_value(node.data)
                .context("GraphDomain::list_rules: deserialize RuleRecord")?;
            if record.agent_id == agent_id {
                rules.push(record);
            }
        }
        Ok(rules)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_storage::SqliteGraphStorage;
    use crate::{NodeCapabilities, NodeConstraints};

    fn make_domain() -> GraphDomain {
        let storage =
            SqliteGraphStorage::open_in_memory().expect("in-memory SqliteGraphStorage failed");
        GraphDomain::new(Arc::new(storage.adapter()))
    }

    fn caps() -> NodeCapabilities {
        NodeCapabilities {
            node_id: "test-node".to_string(),
            roles: vec![],
            models: vec![],
            tools: vec![],
            constraints: NodeConstraints::default(),
        }
    }

    fn hotel(name: &str) -> HotelRecord {
        HotelRecord {
            hotel_name: name.to_string(),
            capabilities: caps(),
            mesh_port: 8999,
            blob_port: 9001,
            execution_port: 9002,
            ipc_socket_path: "/tmp/philotic-aiua.sock".to_string(),
            active_pid: None,
        }
    }

    fn tool(name: &str) -> AbstractToolRecord {
        AbstractToolRecord {
            tool_name: name.to_string(),
            description: format!("Description for {}", name),
            input_schema: serde_json::json!({"type": "object"}),
            class: "utility".to_string(),
        }
    }

    fn rule(id: &str, agent: &str) -> RuleRecord {
        RuleRecord {
            rule_id: id.to_string(),
            agent_id: agent.to_string(),
            description: "Always ask before deleting files.".to_string(),
            rationale: "Prevents accidental data loss.".to_string(),
            created_at: 1_700_000_000,
        }
    }

    // ── Hotel ─────────────────────────────────────────────────────────────────

    #[test]
    fn hotel_roundtrip() {
        let d = make_domain();
        d.upsert_hotel(&hotel("default")).unwrap();
        let h = d.get_hotel("default").unwrap().unwrap();
        assert_eq!(h.hotel_name, "default");
        assert_eq!(h.mesh_port, 8999);
    }

    #[test]
    fn hotel_missing_returns_none() {
        assert!(make_domain().get_hotel("ghost").unwrap().is_none());
    }

    #[test]
    fn hotel_list() {
        let d = make_domain();
        for name in ["alpha", "beta", "gamma"] {
            d.upsert_hotel(&hotel(name)).unwrap();
        }
        let names: Vec<_> = d.list_hotels().unwrap().into_iter().map(|h| h.hotel_name).collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"alpha".to_string()));
        assert!(names.contains(&"beta".to_string()));
        assert!(names.contains(&"gamma".to_string()));
    }

    #[test]
    fn hotel_upsert_overwrites() {
        let d = make_domain();
        d.upsert_hotel(&hotel("default")).unwrap();
        let mut h2 = hotel("default");
        h2.mesh_port = 9100;
        d.upsert_hotel(&h2).unwrap();
        assert_eq!(d.get_hotel("default").unwrap().unwrap().mesh_port, 9100);
        assert_eq!(d.list_hotels().unwrap().len(), 1);
    }

    // ── AbstractTool ──────────────────────────────────────────────────────────

    #[test]
    fn abstract_tool_roundtrip() {
        let d = make_domain();
        d.upsert_abstract_tool(&tool("bash.exec")).unwrap();
        let t = d.get_abstract_tool("bash.exec").unwrap().unwrap();
        assert_eq!(t.tool_name, "bash.exec");
        assert_eq!(t.class, "utility");
    }

    #[test]
    fn abstract_tool_missing_returns_none() {
        assert!(make_domain().get_abstract_tool("no.such.tool").unwrap().is_none());
    }

    #[test]
    fn abstract_tool_list() {
        let d = make_domain();
        for name in ["tool.a", "tool.b", "tool.c"] {
            d.upsert_abstract_tool(&tool(name)).unwrap();
        }
        assert_eq!(d.list_abstract_tools().unwrap().len(), 3);
    }

    // ── Rule ──────────────────────────────────────────────────────────────────

    #[test]
    fn rule_roundtrip() {
        let d = make_domain();
        d.upsert_rule(&rule("rule-001", "agent-alice")).unwrap();
        let r = d.get_rule("rule-001").unwrap().unwrap();
        assert_eq!(r.rule_id, "rule-001");
        assert_eq!(r.agent_id, "agent-alice");
    }

    #[test]
    fn rule_missing_returns_none() {
        assert!(make_domain().get_rule("rule-999").unwrap().is_none());
    }

    #[test]
    fn rule_list_filters_by_agent() {
        let d = make_domain();
        d.upsert_rule(&rule("r1", "agent-alice")).unwrap();
        d.upsert_rule(&rule("r2", "agent-bob")).unwrap();
        d.upsert_rule(&rule("r3", "agent-alice")).unwrap();
        d.upsert_rule(&rule("r4", "agent-alice")).unwrap();

        let alice = d.list_rules("agent-alice").unwrap();
        assert_eq!(alice.len(), 3);
        assert!(alice.iter().all(|r| r.agent_id == "agent-alice"));

        assert_eq!(d.list_rules("agent-bob").unwrap().len(), 1);
        assert!(d.list_rules("agent-nobody").unwrap().is_empty());
    }
}
