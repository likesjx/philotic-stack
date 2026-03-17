use crate::graph::{
    ColumnSpec, Edge, EdgeFilter, EdgeInput, GraphMeta, GraphSchema, GraphSpec, Identity, Node,
    NodeFilter, NodeInput, Row, RowInput, RowQuery, TableMeta, TableSpec, TraversalQuery,
    TraversalResult,
};
use anyhow::Result;

pub mod sqlite;

/// Unified storage interface for the context graph runner.
///
/// All read operations accept an `Identity` and are responsible for enforcing
/// visibility rules internally. Callers must not apply additional filtering —
/// the contract is that the store returns only what the identity may see.
///
/// Write operations do not take an `Identity` because the runner trusts that
/// the IPC dispatch layer has already validated the caller's right to write.
/// The `creator` field on `NodeInput` and `EdgeInput` carries provenance.
pub trait GraphStore: Send + Sync {
    // ── Graph lifecycle ───────────────────────────────────────────────────────

    /// Create a new named graph. Returns the stable `graph_id` (ULID).
    fn create_graph(&self, spec: GraphSpec) -> Result<String>;

    fn get_graph(&self, graph_id: &str) -> Result<Option<GraphMeta>>;

    fn list_graphs(&self) -> Result<Vec<GraphMeta>>;

    /// Update the graph's schema. Additive only — the implementation must
    /// reject removal of node/edge types that are in use.
    fn update_schema(&self, graph_id: &str, schema: GraphSchema) -> Result<()>;

    // ── Nodes ─────────────────────────────────────────────────────────────────

    /// Create or update a node. If `input.node_id` is `Some`, upsert by that ID.
    /// If absent, a new ULID is minted. Returns the stable `node_id`.
    fn upsert_node(&self, graph_id: &str, input: NodeInput) -> Result<String>;

    /// Returns `None` if the node does not exist or is not visible to `identity`.
    fn get_node(
        &self,
        graph_id: &str,
        node_id: &str,
        identity: &Identity,
    ) -> Result<Option<Node>>;

    fn list_nodes(
        &self,
        graph_id: &str,
        filter: &NodeFilter,
        identity: &Identity,
    ) -> Result<Vec<Node>>;

    /// Soft-delete. The node is hidden from all reads after this call.
    fn delete_node(&self, graph_id: &str, node_id: &str) -> Result<()>;

    // ── Edges ─────────────────────────────────────────────────────────────────

    /// Create or update an edge. Returns the stable `edge_id`.
    fn upsert_edge(&self, graph_id: &str, input: EdgeInput) -> Result<String>;

    /// Returns `None` if the edge does not exist or is not visible to `identity`
    /// (including endpoint visibility checks).
    fn get_edge(
        &self,
        graph_id: &str,
        edge_id: &str,
        identity: &Identity,
    ) -> Result<Option<Edge>>;

    fn list_edges(
        &self,
        graph_id: &str,
        filter: &EdgeFilter,
        identity: &Identity,
    ) -> Result<Vec<Edge>>;

    /// Soft-delete.
    fn delete_edge(&self, graph_id: &str, edge_id: &str) -> Result<()>;

    // ── Traversal + Search (Slice 2) ──────────────────────────────────────────

    /// BFS/DFS from a start node. Depth-limited. Visibility-aware on both
    /// nodes and edges encountered during traversal.
    fn traverse(
        &self,
        graph_id: &str,
        query: &TraversalQuery,
        identity: &Identity,
    ) -> Result<TraversalResult>;

    /// Full-text search across node labels and content within a graph.
    fn search_nodes(
        &self,
        graph_id: &str,
        query: &str,
        identity: &Identity,
    ) -> Result<Vec<Node>>;
}
