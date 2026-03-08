use crate::NodeCapabilities;
use std::collections::HashMap;
use std::time::Instant;

/// Status and capability info for a discovered mesh node.
#[derive(Debug, Clone)]
pub struct NodeStatus {
    pub capabilities: NodeCapabilities,
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

    /// Register or update a node's capabilities from a heartbeat.
    pub fn update_node(&mut self, capabilities: NodeCapabilities) {
        self.nodes.insert(
            capabilities.node_id.clone(),
            NodeStatus {
                capabilities,
                last_seen: Instant::now(),
            },
        );
    }

    /// Retrieve capabilities for a specific node.
    pub fn get_node(&self, node_id: &str) -> Option<&NodeStatus> {
        self.nodes.get(node_id)
    }

    /// Iterator over all known active nodes.
    /// In a production system, this would filter out stale nodes.
    pub fn active_nodes(&self) -> impl Iterator<Item = &NodeStatus> {
        self.nodes.values()
    }

    /// Find nodes matching a specific tool.
    pub fn find_nodes_with_tool<'a>(
        &'a self,
        tool: &'a str,
    ) -> impl Iterator<Item = &'a NodeStatus> {
        self.nodes
            .values()
            .filter(move |status| status.capabilities.tools.iter().any(|t| t == tool))
    }

    /// Find nodes matching a specific role.
    pub fn find_nodes_with_role<'a>(
        &'a self,
        role: &'a crate::NodeRole,
    ) -> impl Iterator<Item = &'a NodeStatus> {
        self.nodes
            .values()
            .filter(move |status| status.capabilities.roles.contains(role))
    }
}
