use crate::{NodeCapabilities, NodeHealthSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};
use uuid::Uuid;

const DEFAULT_NODE_TTL: Duration = Duration::from_secs(15);
const PENDING_SYNC_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityAdvertisement {
    pub hotel_id: String,
    pub node_id: String,
    pub incarnation_id: String,
    pub target_role: String,
    pub availability_state: String,
    pub selection_hint: Option<String>,
    pub latency_hint_ms: Option<u32>,
    pub max_concurrent_jobs: Option<u32>,
    pub active_jobs: u32,
    pub queue_depth: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionReachability {
    pub protocol: String,
    pub host: String,
    pub port: u16,
}

/// Status and capability info for a discovered mesh node.
#[derive(Debug, Clone)]
pub struct NodeStatus {
    pub capabilities: NodeCapabilities,
    pub advertisements: Vec<CapabilityAdvertisement>,
    pub execution_reachability: Option<ExecutionReachability>,
    /// Latest environment vitals reported by this node; None if the node
    /// has never sent health data or is running an older build.
    pub node_health: Option<NodeHealthSnapshot>,
    pub last_seen: Instant,
}

#[derive(Debug, Clone)]
struct PendingCapabilitySync {
    capabilities: NodeCapabilities,
    execution_reachability: Option<ExecutionReachability>,
    node_health: Option<NodeHealthSnapshot>,
    chunk_total: u32,
    received_chunks: BTreeMap<u32, Vec<CapabilityAdvertisement>>,
    started_at: Instant,
}

/// Registry for storing and querying known mesh nodes.
#[derive(Debug, Default)]
pub struct NodeRegistry {
    nodes: HashMap<String, NodeStatus>,
    pending_capability_syncs: HashMap<(String, Uuid), PendingCapabilitySync>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            pending_capability_syncs: HashMap::new(),
        }
    }

    fn is_fresh(status: &NodeStatus) -> bool {
        status.last_seen.elapsed() <= DEFAULT_NODE_TTL
    }

    pub fn freshness_ttl_secs() -> u64 {
        DEFAULT_NODE_TTL.as_secs()
    }

    /// Returns `true` if the node was not seen recently (stale or first-ever), which means
    /// the caller should treat the incoming heartbeat as a reconnect event.
    pub fn is_node_stale(&self, node_id: &str) -> bool {
        self.nodes
            .get(node_id)
            .map(|s| !Self::is_fresh(s))
            .unwrap_or(true)
    }

    pub fn observe_heartbeat(
        &mut self,
        capabilities: NodeCapabilities,
        execution_reachability: Option<ExecutionReachability>,
        node_health: Option<NodeHealthSnapshot>,
    ) {
        let node_id = capabilities.node_id.clone();
        if let Some(existing) = self.nodes.get_mut(&node_id) {
            existing.capabilities = capabilities;
            existing.execution_reachability = execution_reachability;
            existing.node_health = node_health;
            existing.last_seen = Instant::now();
            return;
        }

        self.nodes.insert(
            node_id,
            NodeStatus {
                capabilities,
                advertisements: Vec::new(),
                execution_reachability,
                node_health,
                last_seen: Instant::now(),
            },
        );
    }

    /// Transitional compatibility shim for older call sites that still update a whole node in
    /// one shot. New code should prefer `observe_heartbeat` and `observe_capability_sync_chunk`.
    pub fn update_node(
        &mut self,
        capabilities: NodeCapabilities,
        advertisements: Vec<CapabilityAdvertisement>,
        execution_reachability: Option<ExecutionReachability>,
        node_health: Option<NodeHealthSnapshot>,
    ) {
        let sync_id = Uuid::new_v4();
        self.observe_capability_sync_chunk(
            capabilities,
            execution_reachability,
            None,
            sync_id,
            0,
            1,
            advertisements,
        );
    }

    pub fn observe_capability_sync_chunk(
        &mut self,
        capabilities: NodeCapabilities,
        execution_reachability: Option<ExecutionReachability>,
        node_health: Option<NodeHealthSnapshot>,
        sync_id: Uuid,
        chunk_index: u32,
        chunk_total: u32,
        advertisements: Vec<CapabilityAdvertisement>,
    ) {
        self.prune_pending_syncs();
        let node_id = capabilities.node_id.clone();
        let pending = self
            .pending_capability_syncs
            .entry((node_id.clone(), sync_id))
            .or_insert_with(|| PendingCapabilitySync {
                capabilities: capabilities.clone(),
                execution_reachability: execution_reachability.clone(),
                node_health: node_health.clone(),
                chunk_total,
                received_chunks: BTreeMap::new(),
                started_at: Instant::now(),
            });

        pending.capabilities = capabilities;
        pending.execution_reachability = execution_reachability;
        pending.node_health = node_health;
        pending.chunk_total = chunk_total;
        pending.received_chunks.insert(chunk_index, advertisements);

        if pending.received_chunks.len() as u32 != pending.chunk_total {
            return;
        }

        let mut merged = Vec::new();
        for index in 0..pending.chunk_total {
            let Some(chunk) = pending.received_chunks.get(&index) else {
                return;
            };
            merged.extend(chunk.clone());
        }

        let completed = self
            .pending_capability_syncs
            .remove(&(node_id.clone(), sync_id))
            .expect("pending sync should exist when all chunks were received");

        self.nodes.insert(
            node_id,
            NodeStatus {
                capabilities: completed.capabilities,
                advertisements: merged,
                execution_reachability: completed.execution_reachability,
                node_health: completed.node_health,
                last_seen: Instant::now(),
            },
        );
    }

    /// Returns true if the node is fresh AND has not reported degraded vitals.
    /// A node with no health data is considered healthy (old build / first boot).
    pub fn is_node_healthy(&self, node_id: &str) -> bool {
        let Some(status) = self.nodes.get(node_id) else {
            return false;
        };
        if !Self::is_fresh(status) {
            return false;
        }
        let Some(ref h) = status.node_health else {
            return true;
        };
        h.disk_free_pct.map_or(true, |v| v > 10.0)
            && h.mem_free_pct.map_or(true, |v| v > 5.0)
            && h.load_avg_1m.map_or(true, |v| v < 16.0)
    }

    fn prune_pending_syncs(&mut self) {
        self.pending_capability_syncs
            .retain(|_, pending| pending.started_at.elapsed() <= PENDING_SYNC_TTL);
    }

    /// Retrieve capabilities for a specific node.
    pub fn get_node(&self, node_id: &str) -> Option<&NodeStatus> {
        self.nodes.get(node_id)
    }

    pub fn active_nodes(&self) -> impl Iterator<Item = &NodeStatus> {
        self.nodes.values().filter(|status| Self::is_fresh(status))
    }

    pub fn find_nodes_with_tool<'a>(
        &'a self,
        tool: &'a str,
    ) -> impl Iterator<Item = &'a NodeStatus> {
        self.nodes
            .values()
            .filter(|status| Self::is_fresh(status))
            .filter(move |status| status.capabilities.tools.iter().any(|t| t == tool))
    }

    pub fn find_nodes_with_role<'a>(
        &'a self,
        role: &'a crate::NodeRole,
    ) -> impl Iterator<Item = &'a NodeStatus> {
        self.nodes
            .values()
            .filter(|status| Self::is_fresh(status))
            .filter(move |status| status.capabilities.roles.contains(role))
    }

    pub fn advertisements_for_role<'a>(
        &'a self,
        target_role: &'a str,
    ) -> impl Iterator<Item = &'a CapabilityAdvertisement> {
        self.active_nodes().flat_map(move |status| {
            status
                .advertisements
                .iter()
                .filter(move |advertisement| advertisement.target_role == target_role)
        })
    }

    /// Find the fresh node currently hosting a specific guest by incarnation_id (guest_id).
    pub fn find_node_for_incarnation(&self, guest_id: &str) -> Option<&NodeStatus> {
        self.nodes.values().find(|status| {
            Self::is_fresh(status)
                && status
                    .advertisements
                    .iter()
                    .any(|ad| ad.incarnation_id == guest_id)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeConstraints, NodeRole};

    fn caps(node_id: &str) -> NodeCapabilities {
        NodeCapabilities {
            node_id: node_id.into(),
            roles: vec![NodeRole::AnsibleNode],
            models: vec![],
            tools: vec![],
            constraints: NodeConstraints {
                max_concurrent_jobs: Some(4),
                latency_hint_ms: Some(12),
                trust_level: None,
            },
        }
    }

    fn advertisement(node_id: &str, target_role: &str) -> CapabilityAdvertisement {
        CapabilityAdvertisement {
            hotel_id: "aria-architect-hotel".into(),
            node_id: node_id.into(),
            incarnation_id: format!("{node_id}:{target_role}"),
            target_role: target_role.into(),
            availability_state: "live".into(),
            selection_hint: Some("remote_fallback".into()),
            latency_hint_ms: Some(12),
            max_concurrent_jobs: Some(4),
            active_jobs: 1,
            queue_depth: 0,
        }
    }

    #[test]
    fn heartbeat_observation_preserves_existing_advertisements() {
        let mut registry = NodeRegistry::new();
        let node_id = "aria-architect-hotel-ansible-01";
        registry.observe_capability_sync_chunk(
            caps(node_id),
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
            None,
            Uuid::new_v4(),
            0,
            1,
            vec![advertisement(node_id, "model")],
        );

        registry.observe_heartbeat(
            caps(node_id),
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
            Some(NodeHealthSnapshot {
                guest_count: Some(3),
                ..NodeHealthSnapshot::default()
            }),
        );

        let node = registry.get_node(node_id).expect("node should exist");
        assert_eq!(node.advertisements.len(), 1);
        assert_eq!(
            node.node_health
                .as_ref()
                .and_then(|value| value.guest_count),
            Some(3)
        );
    }

    #[test]
    fn capability_sync_reassembles_chunked_advertisements() {
        let mut registry = NodeRegistry::new();
        let sync_id = Uuid::new_v4();
        let node_id = "aria-node";

        registry.observe_capability_sync_chunk(
            caps(node_id),
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
            None,
            sync_id,
            1,
            2,
            vec![advertisement(node_id, "tool.echo")],
        );
        assert!(registry.get_node(node_id).is_none());

        registry.observe_capability_sync_chunk(
            caps(node_id),
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
            None,
            sync_id,
            0,
            2,
            vec![advertisement(node_id, "model")],
        );

        let node = registry.get_node(node_id).expect("node should exist");
        assert_eq!(node.advertisements.len(), 2);
        assert_eq!(node.advertisements[0].target_role, "model");
        assert_eq!(node.advertisements[1].target_role, "tool.echo");
    }

    #[test]
    fn advertisements_for_role_filters_across_nodes() {
        let mut registry = NodeRegistry::new();
        let first_sync = Uuid::new_v4();
        registry.observe_capability_sync_chunk(
            caps("jane-node"),
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "jane-vps".into(),
                port: 9002,
            }),
            None,
            first_sync,
            0,
            1,
            vec![advertisement("jane-node", "membrane")],
        );

        let second_sync = Uuid::new_v4();
        registry.observe_capability_sync_chunk(
            caps("aria-node"),
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
            None,
            second_sync,
            0,
            1,
            vec![advertisement("aria-node", "model")],
        );

        let model_ads: Vec<_> = registry.advertisements_for_role("model").collect();
        assert_eq!(model_ads.len(), 1);
        assert_eq!(model_ads[0].incarnation_id, "aria-node:model");
    }

    #[test]
    fn active_nodes_filters_stale_entries() {
        let mut registry = NodeRegistry::new();
        registry.observe_heartbeat(caps("fresh-node"), None, None);
        registry.observe_heartbeat(caps("stale-node"), None, None);

        registry
            .nodes
            .get_mut("stale-node")
            .expect("stale node should exist")
            .last_seen = Instant::now() - (DEFAULT_NODE_TTL + Duration::from_secs(1));

        let active: Vec<_> = registry.active_nodes().collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].capabilities.node_id, "fresh-node");
    }

    #[test]
    fn advertisements_for_role_ignores_stale_nodes() {
        let mut registry = NodeRegistry::new();
        let fresh_sync = Uuid::new_v4();
        registry.observe_capability_sync_chunk(
            caps("fresh-node"),
            None,
            None,
            fresh_sync,
            0,
            1,
            vec![advertisement("fresh-node", "model")],
        );
        let stale_sync = Uuid::new_v4();
        registry.observe_capability_sync_chunk(
            caps("stale-node"),
            None,
            None,
            stale_sync,
            0,
            1,
            vec![advertisement("stale-node", "model")],
        );

        registry
            .nodes
            .get_mut("stale-node")
            .expect("stale node should exist")
            .last_seen = Instant::now() - (DEFAULT_NODE_TTL + Duration::from_secs(1));

        let ads: Vec<_> = registry.advertisements_for_role("model").collect();
        assert_eq!(ads.len(), 1);
        assert_eq!(ads[0].node_id, "fresh-node");
    }
}
