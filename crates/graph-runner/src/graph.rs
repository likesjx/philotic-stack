use serde::{Deserialize, Serialize};

// ── Identity ──────────────────────────────────────────────────────────────────

/// Calling identity passed on every read operation.
/// Visibility rules are evaluated against this inside the store.
#[derive(Debug, Clone, Default)]
pub struct Identity {
    pub id: String,
    pub roles: Vec<String>,
}

impl Identity {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), roles: vec![] }
    }

    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles = roles;
        self
    }
}

// ── Schema ────────────────────────────────────────────────────────────────────

/// Per-graph vocabulary of allowed node and edge types.
/// Schema is additive — new types may be added but used types cannot be removed.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GraphSchema {
    pub node_types: Vec<NodeTypeSpec>,
    pub edge_types: Vec<EdgeTypeSpec>,
    /// If true, reject nodes/edges with types not in the vocabulary.
    /// If false, allow unknown types (schemaless bootstrapping mode).
    #[serde(default)]
    pub strict: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTypeSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeTypeSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Restrict which node types this edge may originate from.
    #[serde(default)]
    pub allowed_from: Option<Vec<String>>,
    /// Restrict which node types this edge may point to.
    #[serde(default)]
    pub allowed_to: Option<Vec<String>>,
}

// ── Graph ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GraphSpec {
    pub name: String,
    pub description: Option<String>,
    pub schema: GraphSchema,
    /// Default visibility applied to nodes whose visibility list is empty.
    /// "private" | "hotel-public" | "public"
    pub default_visibility: String,
    pub creator: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphMeta {
    pub graph_id: String,
    pub name: String,
    pub description: Option<String>,
    pub schema: GraphSchema,
    pub default_visibility: String,
    pub creator: String,
    pub created_at: i64,
    pub updated_at: i64,
}

// ── Node ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NodeInput {
    /// If provided, upsert by this ID. If absent, a new ULID is minted.
    pub node_id: Option<String>,
    pub node_type: String,
    pub label: String,
    /// Freeform structured data. Callers may store any JSON object.
    pub content: serde_json::Value,
    /// Arbitrary filter tags (not access control).
    pub tags: Vec<String>,
    /// Access control tags. See visibility rules in the proposal doc.
    pub visibility: Vec<String>,
    pub creator: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub node_id: String,
    pub graph_id: String,
    pub node_type: String,
    pub label: String,
    pub content: serde_json::Value,
    pub tags: Vec<String>,
    pub visibility: Vec<String>,
    pub creator: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default)]
pub struct NodeFilter {
    pub node_type: Option<String>,
    /// Return only nodes that have ALL of these tags.
    pub tags: Option<Vec<String>>,
    pub creator: Option<String>,
}

// ── Edge ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct EdgeInput {
    /// If provided, upsert by this ID. If absent, a new ULID is minted.
    pub edge_id: Option<String>,
    pub from_node_id: String,
    pub to_node_id: String,
    pub edge_type: String,
    pub label: Option<String>,
    pub content: serde_json::Value,
    /// Edge visibility is evaluated in addition to both endpoint visibilities.
    pub visibility: Vec<String>,
    pub creator: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub edge_id: String,
    pub graph_id: String,
    pub from_node_id: String,
    pub to_node_id: String,
    pub edge_type: String,
    pub label: Option<String>,
    pub content: serde_json::Value,
    pub visibility: Vec<String>,
    pub creator: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default)]
pub struct EdgeFilter {
    pub from_node_id: Option<String>,
    pub to_node_id: Option<String>,
    pub edge_type: Option<String>,
    pub creator: Option<String>,
}

// ── Traversal ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TraversalDirection {
    Outbound,
    Inbound,
    Both,
}

#[derive(Debug, Clone)]
pub struct TraversalQuery {
    pub start_node_id: String,
    pub direction: TraversalDirection,
    pub max_depth: u32,
    /// If set, only follow edges of these types.
    pub edge_types: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}
