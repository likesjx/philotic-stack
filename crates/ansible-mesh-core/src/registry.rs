use crate::NodeCapabilities;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const DEFAULT_NODE_TTL: Duration = Duration::from_secs(15);

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
    pub last_seen: Instant,
}

/// Registry for storing and querying known mesh nodes.
#[derive(Debug, Default)]
pub struct NodeRegistry {
    nodes: HashMap<String, NodeStatus>, // Keyed by NodeId
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    fn is_fresh(status: &NodeStatus) -> bool {
        status.last_seen.elapsed() <= DEFAULT_NODE_TTL
    }

    /// Register or update a node's capabilities from a heartbeat.
    pub fn update_node(
        &mut self,
        capabilities: NodeCapabilities,
        advertisements: Vec<CapabilityAdvertisement>,
        execution_reachability: Option<ExecutionReachability>,
    ) {
        self.nodes.insert(
            capabilities.node_id.clone(),
            NodeStatus {
                capabilities,
                advertisements,
                execution_reachability,
                last_seen: Instant::now(),
            },
        );
    }

    /// Retrieve capabilities for a specific node.
    pub fn get_node(&self, node_id: &str) -> Option<&NodeStatus> {
        self.nodes.get(node_id)
    }

    /// Iterator over all known active nodes.
    pub fn active_nodes(&self) -> impl Iterator<Item = &NodeStatus> {
        self.nodes.values().filter(|status| Self::is_fresh(status))
    }

    /// Find nodes matching a specific tool.
    pub fn find_nodes_with_tool<'a>(
        &'a self,
        tool: &'a str,
    ) -> impl Iterator<Item = &'a NodeStatus> {
        self.nodes
            .values()
            .filter(|status| Self::is_fresh(status))
            .filter(move |status| status.capabilities.tools.iter().any(|t| t == tool))
    }

    /// Find nodes matching a specific role.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeCapabilities, NodeConstraints, NodeRole};

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

    #[test]
    fn update_node_tracks_advertisements() {
        let mut registry = NodeRegistry::new();
        let advertisements = vec![CapabilityAdvertisement {
            hotel_id: "aria-architect-hotel".into(),
            node_id: "aria-architect-hotel-ansible-01".into(),
            incarnation_id: "aria-architect-hotel:model-controller-gemini".into(),
            target_role: "model.gemini".into(),
            availability_state: "live".into(),
            selection_hint: Some("remote_fallback".into()),
            latency_hint_ms: Some(12),
            max_concurrent_jobs: Some(4),
            active_jobs: 1,
            queue_depth: 0,
        }];

        registry.update_node(
            caps("aria-architect-hotel-ansible-01"),
            advertisements.clone(),
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
        );

        let node = registry
            .get_node("aria-architect-hotel-ansible-01")
            .expect("node should exist");
        assert_eq!(node.advertisements, advertisements);
        assert_eq!(
            node.execution_reachability
                .as_ref()
                .map(|value| value.host.as_str()),
            Some("aria-vps")
        );
    }

    #[test]
    fn advertisements_for_role_filters_across_nodes() {
        let mut registry = NodeRegistry::new();
        registry.update_node(
            caps("jane-node"),
            vec![CapabilityAdvertisement {
                hotel_id: "default".into(),
                node_id: "jane-node".into(),
                incarnation_id: "default:membrane-gateway-jane".into(),
                target_role: "membrane".into(),
                availability_state: "live".into(),
                selection_hint: Some("local_live_preferred".into()),
                latency_hint_ms: Some(5),
                max_concurrent_jobs: Some(4),
                active_jobs: 0,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "jane-vps".into(),
                port: 9002,
            }),
        );
        registry.update_node(
            caps("aria-node"),
            vec![CapabilityAdvertisement {
                hotel_id: "aria-architect-hotel".into(),
                node_id: "aria-node".into(),
                incarnation_id: "aria-architect-hotel:model-controller-gemini".into(),
                target_role: "model.gemini".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_fallback".into()),
                latency_hint_ms: Some(12),
                max_concurrent_jobs: Some(4),
                active_jobs: 1,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
        );

        let model_ads: Vec<_> = registry.advertisements_for_role("model.gemini").collect();
        assert_eq!(model_ads.len(), 1);
        assert_eq!(
            model_ads[0].incarnation_id,
            "aria-architect-hotel:model-controller-gemini"
        );
    }

    #[test]
    fn active_nodes_filters_stale_entries() {
        let mut registry = NodeRegistry::new();
        registry.update_node(caps("fresh-node"), vec![], None);
        registry.update_node(caps("stale-node"), vec![], None);

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
        registry.update_node(
            caps("fresh-node"),
            vec![CapabilityAdvertisement {
                hotel_id: "default".into(),
                node_id: "fresh-node".into(),
                incarnation_id: "default:model-controller-gemini".into(),
                target_role: "model.gemini".into(),
                availability_state: "live".into(),
                selection_hint: Some("local_live_preferred".into()),
                latency_hint_ms: Some(5),
                max_concurrent_jobs: Some(4),
                active_jobs: 0,
                queue_depth: 0,
            }],
            None,
        );
        registry.update_node(
            caps("stale-node"),
            vec![CapabilityAdvertisement {
                hotel_id: "stale-hotel".into(),
                node_id: "stale-node".into(),
                incarnation_id: "stale-hotel:model-controller-gemini".into(),
                target_role: "model.gemini".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_fallback".into()),
                latency_hint_ms: Some(50),
                max_concurrent_jobs: Some(1),
                active_jobs: 1,
                queue_depth: 1,
            }],
            None,
        );

        registry
            .nodes
            .get_mut("stale-node")
            .expect("stale node should exist")
            .last_seen = Instant::now() - (DEFAULT_NODE_TTL + Duration::from_secs(1));

        let ads: Vec<_> = registry.advertisements_for_role("model.gemini").collect();
        assert_eq!(ads.len(), 1);
        assert_eq!(ads[0].node_id, "fresh-node");
    }
}
