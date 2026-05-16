//! MCP endpoint configuration — philote-declared, transform-driven.
//!
//! A philote uses `mcp.provision` to declare an `McpEndpointConfig`. The hotel
//! persists it, fans out an `update_mcp_config` push to the relevant
//! `membrane-mcp` guest, and stores pre-approval rules so future calls
//! matching the declared envelope shapes are not re-parked for approval.
//!
//! These types are the shared contract between philote, hotel, and membrane-mcp.

use crate::mcp_route::{McpAuthScheme, McpRouteTarget};
use serde::{Deserialize, Serialize};

// ── Endpoint config ───────────────────────────────────────────────────────────

/// Complete configuration for one MCP endpoint.
///
/// Stored in the hotel context graph under `__mcp_endpoint__:<endpoint_id>`.
/// LWW-merged on `updated_at`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpEndpointConfig {
    /// Stable ID for this endpoint (e.g. `"bjork-mcp-01"`).
    pub endpoint_id: String,
    /// Agent that owns and may update this endpoint.
    pub owner_agent_id: String,
    /// Port the membrane-mcp guest should bind on.
    pub port: u16,
    /// Path prefix. Defaults to `"/mcp"` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Tools advertised to MCP clients via `tools/list`.
    pub tools: Vec<McpToolSpec>,
    /// Pre-approval rules established by the philote's provisioning turn.
    #[serde(default)]
    pub preapproval_rules: Vec<McpPreapprovalRule>,
    /// Unix epoch (seconds). LWW merge key.
    pub updated_at: u64,
}

// ── Tool specification ────────────────────────────────────────────────────────

/// One tool exported through an MCP endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolSpec {
    /// MCP tool name (e.g. `"search_docs"`).
    pub name: String,
    /// Human-readable description forwarded in `tools/list`.
    pub description: String,
    /// JSON Schema object describing the tool's input arguments.
    pub input_schema: serde_json::Value,
    /// How to map MCP `tools/call` arguments → router envelope.
    pub inbound_transform: McpInboundTransform,
    /// How to map the router response → MCP result.
    pub outbound_transform: McpOutboundTransform,
    /// Per-tool auth override. Inherits the endpoint caller's token if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<McpAuthScheme>,
}

// ── Inbound transform ─────────────────────────────────────────────────────────

/// Maps MCP `tools/call` arguments to a router envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpInboundTransform {
    /// Declarative field mapping: MCP arg path → router envelope field path.
    /// Covers the common case with no code evaluation.
    FieldMap {
        /// Envelope action (e.g. `"datasource.query"`).
        action: String,
        /// Dispatch target — which guest receives the envelope.
        target: McpRouteTarget,
        /// Field-level mappings from MCP args to envelope payload.
        #[serde(default)]
        mappings: Vec<FieldMapping>,
    },
    /// Jinja2-style template rendered against the full MCP request context.
    /// Reserved for non-trivial shapes (Phase 4).
    Template { template: String },
}

/// One field-level path mapping used by `McpInboundTransform::FieldMap`.
///
/// Both `from` and `to` are dot-path strings (e.g. `"args.query"`,
/// `"payload.q"`). No JSONPath eval in the hot path — dot-path only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldMapping {
    /// Source path in the MCP request args object (dot-separated).
    pub from: String,
    /// Destination path in the router envelope payload (dot-separated).
    pub to: String,
}

// ── Outbound transform ────────────────────────────────────────────────────────

/// Maps a router envelope response to an MCP `tools/call` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpOutboundTransform {
    /// Extract a single field from the response payload as the MCP result.
    /// `path` is a dot-separated key path into the response JSON.
    Extract { path: String },
    /// Return the full response payload as JSON.
    PassThrough,
    /// Render a template against the response (Phase 4).
    Template { template: String },
}

// ── Pre-approval rules ────────────────────────────────────────────────────────

/// A pre-approval rule established by the philote's provisioning turn.
///
/// Stored alongside the endpoint config under
/// `__mcp_preapproval__:<endpoint_id>`. When membrane-mcp dispatches an
/// envelope, it checks this table before parking for approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpPreapprovalRule {
    /// Envelope action pattern this rule matches (exact string or `*` glob).
    pub action_pattern: String,
    /// Optional target constraint. `None` matches any target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<McpRouteTarget>,
    /// Turn ID of the philote turn that established this approval.
    pub approved_by_turn: String,
    /// Unix epoch (seconds) when this rule was established.
    pub approved_at: u64,
    /// Optional expiry. `None` = permanent until config update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_route::McpRouteTarget;

    fn round_trip<
        T: serde::Serialize + for<'de> serde::Deserialize<'de> + std::fmt::Debug + PartialEq,
    >(
        v: &T,
    ) {
        let json = serde_json::to_string(v).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(*v, back);
    }

    #[test]
    fn field_map_inbound_transform() {
        round_trip(&McpInboundTransform::FieldMap {
            action: "datasource.query".into(),
            target: McpRouteTarget::Datasource {
                datasource_id: "graph-datasource-01".into(),
            },
            mappings: vec![FieldMapping {
                from: "args.query".into(),
                to: "payload.q".into(),
            }],
        });
    }

    #[test]
    fn outbound_extract() {
        round_trip(&McpOutboundTransform::Extract {
            path: "results.0.text".into(),
        });
    }

    #[test]
    fn outbound_passthrough() {
        round_trip(&McpOutboundTransform::PassThrough);
    }

    #[test]
    fn full_endpoint_config() {
        round_trip(&McpEndpointConfig {
            endpoint_id: "bjork-mcp-01".into(),
            owner_agent_id: "agent-bjork-01".into(),
            port: 8910,
            path: Some("/mcp".into()),
            tools: vec![McpToolSpec {
                name: "search_docs".into(),
                description: "Search project documentation".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }),
                inbound_transform: McpInboundTransform::FieldMap {
                    action: "datasource.query".into(),
                    target: McpRouteTarget::Datasource {
                        datasource_id: "graph-datasource-01".into(),
                    },
                    mappings: vec![FieldMapping {
                        from: "query".into(),
                        to: "payload.query".into(),
                    }],
                },
                outbound_transform: McpOutboundTransform::PassThrough,
                auth: None,
            }],
            preapproval_rules: vec![McpPreapprovalRule {
                action_pattern: "datasource.query".into(),
                target: None,
                approved_by_turn: "turn-abc123".into(),
                approved_at: 1_700_000_000,
                expires_at: None,
            }],
            updated_at: 1_700_000_000,
        });
    }
}
