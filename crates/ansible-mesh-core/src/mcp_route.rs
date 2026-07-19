//! MCP membrane routing types — projected from each philote's agent-graph.
//!
//! These types are the shared contract between the agent-graph (source of truth),
//! the hotel (propagator), and the membrane-mcp guest (enforcement point).
//!
//! # Security invariant
//!
//! No secret material lives here. `McpTokenGrant.vault_ref` is a vault key
//! pointer; the actual BLAKE3 hash of the token lives only in the vault.

use serde::{Deserialize, Serialize};

// ── Route record ─────────────────────────────────────────────────────────────

/// One tool exported by a philote through the MCP membrane.
///
/// Stored in the agent-graph under the owning agent's identity and projected
/// to the membrane via IPC (`UpdateMcpRoutes`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpRouteRecord {
    /// The agent that owns (and can modify) this route.
    pub agent_id: String,
    /// Tool name as exposed to MCP clients (e.g. `"search_docs"`).
    pub tool_name: String,
    /// Human-readable description forwarded in `tools/list`.
    pub description: String,
    /// JSON Schema object describing the tool's input arguments.
    pub input_schema: serde_json::Value,
    /// Where incoming calls are dispatched.
    pub target: McpRouteTarget,
    /// Per-route security policy. No secret material stored here.
    pub security: McpRouteSecurity,
    /// Unix epoch (seconds). Used for LWW when the hotel merges updates.
    pub updated_at: u64,
}

// ── Dispatch target ───────────────────────────────────────────────────────────

/// Where the membrane should send an incoming `tools/call` for this route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpRouteTarget {
    /// Route to a running philote (cognitive loop guest).
    /// `target_node` pins the dispatch to a specific hotel node_id for cross-hotel routing.
    /// When absent the dispatch is local (same hotel as membrane-mcp).
    Philote {
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_node: Option<String>,
    },
    /// Route to a tool-runner tool ref (e.g. `"bash.exec@1"`).
    Tool { tool_ref: String },
    /// Route to a datasource guest (e.g. `"graph-datasource-01"`).
    Datasource { datasource_id: String },
}

// ── Security envelope ─────────────────────────────────────────────────────────

/// Per-route security policy. Stored in the agent-graph; projected to membrane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpRouteSecurity {
    /// How incoming callers authenticate for this specific route.
    pub auth: McpAuthScheme,
    /// Rate limit applied across all callers (regardless of token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_allotment: Option<McpCallAllotment>,
    /// If true, every call parks as WaitingApproval before dispatch.
    #[serde(default)]
    pub require_approval: bool,
}

/// Authentication scheme for a single route.
///
/// `hmac_sha256` was removed in the MCP membrane hardening: it was
/// provisionable but never callable (filtered from `tools/list`, rejected on
/// `tools/call`), so a config carrying it could not authenticate anyone.
/// Deserializing a stored `hmac_sha256` scheme now fails with serde's
/// unknown-variant error — re-provision with `bearer_token`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "scheme", rename_all = "snake_case")]
pub enum McpAuthScheme {
    /// Each caller presents a bearer token. Membrane fetches the BLAKE3 hash
    /// from the vault and compares in constant time.
    BearerToken { grants: Vec<McpTokenGrant> },
    /// No authentication — only valid for loopback/internal routes.
    None,
}

/// A single caller grant within a `BearerToken` auth scheme.
///
/// Contains no secret material. The raw token is shown to the operator once
/// at provisioning time (via vault) and never stored elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpTokenGrant {
    /// Stable opaque label identifying this credential (e.g. `"claude-desktop"`).
    pub token_id: String,
    /// Vault key reference under which `BLAKE3(raw_token)` is stored.
    pub vault_ref: String,
    /// Optional capability scopes granted to this token.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Unix epoch (seconds) after which this grant is expired. None = no expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// Per-token call budget. Tracked in-memory by the membrane; resets on restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allotment: Option<McpCallAllotment>,
}

/// A sliding-window call budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpCallAllotment {
    /// Maximum calls allowed within `window_secs`.
    pub max_per_window: u32,
    /// Window size in seconds.
    pub window_secs: u64,
}

// ── Wire types for IPC projection ─────────────────────────────────────────────

/// Hotel → membrane push: replace one agent's entire route set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpRouteUpdate {
    pub agent_id: String,
    pub routes: Vec<McpRouteRecord>,
}

/// Hotel → membrane push: remove all routes owned by this agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpRouteRevoke {
    pub agent_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug + PartialEq>(v: &T) {
        let json = serde_json::to_string(v).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(*v, back);
    }

    #[test]
    fn route_target_variants() {
        round_trip(&McpRouteTarget::Philote {
            agent_id: "bjork-01".into(),
            target_node: None,
        });
        round_trip(&McpRouteTarget::Philote {
            agent_id: "bjork-01".into(),
            target_node: Some("local-telegram-aiua-01".into()),
        });
        round_trip(&McpRouteTarget::Tool {
            tool_ref: "bash.exec@1".into(),
        });
        round_trip(&McpRouteTarget::Datasource {
            datasource_id: "graph-datasource-01".into(),
        });
    }

    #[test]
    fn token_grant_round_trip() {
        round_trip(&McpTokenGrant {
            token_id: "claude-desktop".into(),
            vault_ref: "mcp/routes/search/claude-desktop".into(),
            scopes: vec!["read".into()],
            expires_at: Some(9999999999),
            allotment: Some(McpCallAllotment {
                max_per_window: 100,
                window_secs: 60,
            }),
        });
    }

    #[test]
    fn full_route_record_round_trip() {
        round_trip(&McpRouteRecord {
            agent_id: "bjork-01".into(),
            tool_name: "search_docs".into(),
            description: "Search project documentation".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"]
            }),
            target: McpRouteTarget::Philote {
                agent_id: "bjork-01".into(),
                target_node: None,
            },
            security: McpRouteSecurity {
                auth: McpAuthScheme::BearerToken {
                    grants: vec![McpTokenGrant {
                        token_id: "n8n-prod".into(),
                        vault_ref: "mcp/routes/search_docs/n8n-prod".into(),
                        scopes: vec![],
                        expires_at: None,
                        allotment: None,
                    }],
                },
                global_allotment: None,
                require_approval: false,
            },
            updated_at: 1_700_000_000,
        });
    }
}
