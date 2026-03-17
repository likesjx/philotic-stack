use crate::graph::{
    ColumnSpec, Edge, EdgeFilter, EdgeInput, GraphMeta, GraphSchema, GraphSpec, Identity, Node,
    NodeFilter, NodeInput, Row, RowInput, RowQuery, TableMeta, TableSpec, TraversalQuery,
    TraversalResult,
};
use anyhow::Result;

pub mod sqlite;

/// Combined supertrait for the full store capability — both graph and table operations.
/// All concrete store implementations implement this.
pub trait GraphTableStore: GraphStore + TableStore {}

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

/// Table adapter storage interface.
///
/// Tables live alongside graphs in the same runner instance. A table may be
/// linked to a graph via `TableSpec::graph_id`, or standalone. Rows are plain
/// JSON objects; column specs describe the intended schema but are not strictly
/// enforced at the storage layer (enforcement is a tool-layer concern).
///
/// Access control for tables is capability-based: knowing the `table_id` is
/// sufficient for reads. Write access requires the caller to be the creator
/// or to have been explicitly granted access (enforced by the tool layer;
/// the store trusts what the tool layer passes in).
pub trait TableStore: Send + Sync {
    // ── Table lifecycle ────────────────────────────────────────────────────────

    /// Create a new table. Returns the stable `table_id` (ULID).
    fn create_table(&self, spec: TableSpec) -> Result<String>;

    fn get_table(&self, table_id: &str) -> Result<Option<TableMeta>>;

    /// List all tables, optionally filtered to those linked to a specific graph.
    fn list_tables(&self, graph_id: Option<&str>) -> Result<Vec<TableMeta>>;

    /// Update table metadata (name, description, columns). Additive-only on columns —
    /// the implementation must reject removal of columns that have row data.
    fn update_table(&self, table_id: &str, name: Option<String>, description: Option<String>, columns: Option<Vec<ColumnSpec>>) -> Result<()>;

    /// Permanently delete a table and all its rows.
    fn drop_table(&self, table_id: &str) -> Result<()>;

    // ── Row operations ─────────────────────────────────────────────────────────

    /// Insert or upsert a row. Returns the stable `row_id` (ULID).
    fn insert_row(&self, table_id: &str, input: RowInput) -> Result<String>;

    fn get_row(&self, table_id: &str, row_id: &str) -> Result<Option<Row>>;

    /// Merge `patch` into the existing row's data (shallow merge).
    fn update_row(&self, table_id: &str, row_id: &str, patch: serde_json::Value) -> Result<()>;

    /// Soft-delete a row.
    fn delete_row(&self, table_id: &str, row_id: &str) -> Result<()>;

    /// Return rows matching the query. Equality filter evaluated in-memory.
    fn query_rows(&self, table_id: &str, query: &RowQuery) -> Result<Vec<Row>>;
}
