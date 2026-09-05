//! MCP endpoint configuration — philote-declared, transform-driven.
//!
//! A philote uses `mcp.provision` to declare an `McpEndpointConfig`. The hotel
//! persists it, fans out an `update_mcp_config` push to the relevant
//! `membrane-mcp` guest, and stores pre-approval rules so future calls
//! matching the declared envelope shapes are not re-parked for approval.
//!
//! These types are the shared contract between philote, hotel, and membrane-mcp.

use crate::mcp_route::{McpAuthScheme, McpRouteTarget};
use crate::ExposureTier;
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
    /// Minimum exposure tier at which this endpoint is intentionally served.
    /// The hotel validates that `exposure <= hotel_ceiling` at provision time.
    /// Defaults to `Local` for backward compatibility; explicitly set to `Mesh`
    /// or `Internet` for externally-reachable endpoints.
    #[serde(default)]
    pub exposure: ExposureTier,
    /// Tools advertised to MCP clients via `tools/list`.
    pub tools: Vec<McpToolSpec>,
    /// Endpoint-wide default auth scheme. A tool without its own `auth`
    /// inherits this; absent means `McpAuthScheme::None` (loopback-only
    /// callers). Token grants added via `mcp.grant_token` without a
    /// `tool_name` land here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_auth: Option<McpAuthScheme>,
    /// Explicit operator acknowledgment that this endpoint intentionally
    /// serves unauthenticated tools beyond loopback. Without it, provisioning
    /// a `Lan`/`Mesh`/`Internet` endpoint with any effective-auth-`None` tool
    /// is rejected. Surfaced in the approval prompt via the provisioning args.
    #[serde(default)]
    pub allow_unauthenticated: bool,
    /// Pre-approval rules established by the philote's provisioning turn.
    #[serde(default)]
    pub preapproval_rules: Vec<McpPreapprovalRule>,
    /// Unix epoch (seconds). LWW merge key.
    pub updated_at: u64,
}

impl McpEndpointConfig {
    /// The auth scheme actually enforced for a tool: per-tool override,
    /// else the endpoint default, else `None` (loopback-only).
    pub fn effective_auth<'a>(&'a self, tool: &'a McpToolSpec) -> McpAuthScheme {
        tool.auth
            .clone()
            .or_else(|| self.default_auth.clone())
            .unwrap_or(McpAuthScheme::None)
    }
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
    /// How the receiving philote handles a call to this tool: an ordered
    /// deterministic ladder (input validation, static results, built-in
    /// reflexes) that runs before any model inference, plus the declared
    /// fallback when the ladder does not produce a result. Absent = the
    /// legacy behaviour (validate nothing, go straight to a model turn).
    ///
    /// Only meaningful for `McpRouteTarget::Philote` targets; datasource and
    /// tool targets never reach a philote and are deterministic by
    /// construction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<McpHandlerPolicy>,
}

// ── Handler policy (deterministic-first, inference-second) ────────────────────

/// Built-in deterministic reflexes a philote can run for an MCP call without
/// a model turn. Kept as a closed list so a provisioning turn cannot name an
/// arbitrary philote tool and have it execute unattended.
pub const MCP_REFLEX_KINDS: &[&str] = &["echo", "memory.recall", "memory.capture"];

fn default_true() -> bool {
    true
}

/// One rung of the deterministic ladder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpHandlerStep {
    /// Answer with a fixed JSON value. Use for capability descriptors, health
    /// probes, and any tool whose answer was known at provisioning time.
    Static { result: serde_json::Value },
    /// Run a built-in philote reflex (see [`MCP_REFLEX_KINDS`]) with
    /// `args` rendered against the call payload. String values of the form
    /// `"${payload.<dot.path>}"` are substituted from the inbound payload;
    /// the bare string `"${payload}"` substitutes the whole payload object.
    /// When `escalate_on_empty` is set and the reflex yields nothing, the
    /// ladder continues to the next step (and ultimately the fallback)
    /// instead of returning an empty result.
    Reflex {
        reflex: String,
        #[serde(default)]
        args: serde_json::Value,
        #[serde(default)]
        escalate_on_empty: bool,
    },
}

/// What happens when no deterministic step produced a result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpHandlerFallback {
    /// Hand the call to the philote's cognitive loop. `instructions` is
    /// rendered into the turn so the model knows it is answering an MCP tool
    /// call and what shape the caller expects back.
    Model {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    /// Refuse deterministically. The caller receives an `isError` result
    /// carrying `message`; no model turn is ever started.
    Error { message: String },
}

impl Default for McpHandlerFallback {
    fn default() -> Self {
        Self::Model { instructions: None }
    }
}

/// Per-tool handling policy carried on [`McpToolSpec::handler`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpHandlerPolicy {
    /// Reject calls whose arguments violate the tool's `input_schema`
    /// (`required` keys and primitive `type`s) before any step runs.
    /// Defaults to `true`.
    #[serde(default = "default_true")]
    pub validate_input: bool,
    /// Deterministic ladder, tried in order.
    #[serde(default)]
    pub steps: Vec<McpHandlerStep>,
    /// Fallback once the ladder is exhausted. Defaults to a model turn.
    #[serde(default)]
    pub fallback: McpHandlerFallback,
}

impl Default for McpHandlerPolicy {
    fn default() -> Self {
        Self {
            validate_input: true,
            steps: Vec::new(),
            fallback: McpHandlerFallback::default(),
        }
    }
}

impl McpHandlerPolicy {
    /// Structural validation performed at provisioning time so a bad policy
    /// fails the `mcp.provision` turn instead of every later call.
    pub fn validate(&self) -> Result<(), String> {
        for (idx, step) in self.steps.iter().enumerate() {
            if let McpHandlerStep::Reflex { reflex, args, .. } = step {
                if !MCP_REFLEX_KINDS.contains(&reflex.as_str()) {
                    return Err(format!(
                        "handler step {idx}: unknown reflex '{reflex}' (known: {})",
                        MCP_REFLEX_KINDS.join(", ")
                    ));
                }
                if !(args.is_null() || args.is_object()) {
                    return Err(format!(
                        "handler step {idx}: reflex args must be an object or omitted"
                    ));
                }
            }
        }
        if let McpHandlerFallback::Error { message } = &self.fallback {
            if message.trim().is_empty() {
                return Err("handler fallback 'error' requires a non-empty message".into());
            }
        }
        Ok(())
    }

    /// True when the ladder can never reach a model turn.
    pub fn is_fully_deterministic(&self) -> bool {
        matches!(self.fallback, McpHandlerFallback::Error { .. })
    }
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
            exposure: ExposureTier::Mesh,
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
                handler: None,
            }],
            default_auth: None,
            allow_unauthenticated: false,
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

    #[test]
    fn handler_policy_defaults_are_validate_then_model() {
        let policy: McpHandlerPolicy = serde_json::from_str("{}").unwrap();
        assert!(policy.validate_input);
        assert!(policy.steps.is_empty());
        assert_eq!(
            policy.fallback,
            McpHandlerFallback::Model { instructions: None }
        );
        assert!(!policy.is_fully_deterministic());
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn handler_policy_round_trips_and_validates_reflex_names() {
        let json = serde_json::json!({
            "validate_input": true,
            "steps": [
                { "kind": "static", "result": { "ok": true } },
                { "kind": "reflex", "reflex": "memory.recall",
                  "args": { "query": "${payload.query}" }, "escalate_on_empty": true }
            ],
            "fallback": { "kind": "error", "message": "not answerable" }
        });
        let policy: McpHandlerPolicy = serde_json::from_value(json.clone()).unwrap();
        assert!(policy.validate().is_ok());
        assert!(policy.is_fully_deterministic());
        assert_eq!(serde_json::to_value(&policy).unwrap(), json);

        let bad: McpHandlerPolicy = serde_json::from_value(serde_json::json!({
            "steps": [{ "kind": "reflex", "reflex": "bash.exec" }]
        }))
        .unwrap();
        let err = bad.validate().unwrap_err();
        assert!(err.contains("unknown reflex 'bash.exec'"), "{err}");
    }

    #[test]
    fn tool_spec_without_handler_still_deserializes() {
        let spec: McpToolSpec = serde_json::from_value(serde_json::json!({
            "name": "t", "description": "d", "input_schema": {},
            "inbound_transform": { "kind": "field_map", "action": "a",
                "target": { "kind": "philote", "agent_id": "x" } },
            "outbound_transform": { "kind": "pass_through" }
        }))
        .unwrap();
        assert!(spec.handler.is_none());
    }
}
