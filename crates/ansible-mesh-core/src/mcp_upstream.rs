//! MCP upstream configuration — philote-declared consumption of external MCP servers.
//!
//! The mirror of [`crate::mcp_endpoint`]: where an `McpEndpointConfig` declares
//! tools this hotel *serves* to external MCP clients, an `McpUpstreamConfig`
//! declares an external MCP *server* whose tools philotes may consume. A philote
//! uses `mcp.connect` to declare one; the hotel persists it under
//! `__mcp_upstream__:<upstream_id>`, fans out an `update_mcp_upstream` push to
//! the `mcp-client` guest, and the guest connects, lists the remote tools,
//! filters them through the allowlist, and reports the projected catalog back.
//!
//! Projected tools appear in the owning philote's catalog namespaced as
//! `mcp:<upstream_id>.<remote_name>`.
//!
//! These types are the shared contract between philote, hotel, and the
//! `membrane-mcp-client` guest.

use serde::{Deserialize, Serialize};

/// Default cap on remote tool response size (bytes) when a grant does not
/// override it.
pub const DEFAULT_UPSTREAM_MAX_RESPONSE_BYTES: u64 = 256 * 1024;

/// Default per-call timeout for remote `tools/call` execution (seconds).
pub const DEFAULT_UPSTREAM_CALL_TIMEOUT_SECS: u64 = 30;

/// Namespace prefix under which projected upstream tools appear in a philote
/// catalog: `mcp:<upstream_id>.<remote_name>`.
pub const MCP_PROJECTED_TOOL_PREFIX: &str = "mcp:";

/// Compose the projected (namespaced) tool name for a remote tool.
pub fn projected_tool_name(upstream_id: &str, remote_name: &str) -> String {
    format!("{MCP_PROJECTED_TOOL_PREFIX}{upstream_id}.{remote_name}")
}

/// Split a projected tool name back into `(upstream_id, remote_name)`.
/// Returns `None` if the name is not in the `mcp:<upstream>.<tool>` shape.
pub fn parse_projected_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(MCP_PROJECTED_TOOL_PREFIX)?;
    let (upstream_id, remote_name) = rest.split_once('.')?;
    if upstream_id.is_empty() || remote_name.is_empty() {
        return None;
    }
    Some((upstream_id, remote_name))
}

// ── Upstream config ───────────────────────────────────────────────────────────

/// Complete configuration for one upstream MCP server connection.
///
/// Stored in the hotel context graph under `__mcp_upstream__:<upstream_id>`.
/// LWW-merged on `updated_at`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpUpstreamConfig {
    /// Stable ID for this upstream (e.g. `"intel-graph"`, `"muninn-local"`).
    pub upstream_id: String,
    /// Agent that owns and may update this upstream registration.
    pub owner_agent_id: String,
    /// How to reach the server.
    pub transport: McpUpstreamTransport,
    /// Vault ref for the outbound credential (sent as `Authorization: Bearer`).
    /// `None` = unauthenticated upstream (egress policy confines these to
    /// loopback/tailnet by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    /// Remote tools to project into philote catalogs. Empty = project none —
    /// each remote tool is an explicit opt-in, never "everything by default".
    #[serde(default)]
    pub tool_allowlist: Vec<McpUpstreamToolGrant>,
    /// Agents (besides the owner) allowed to call the projected tools.
    /// Empty = owner only.
    #[serde(default)]
    pub grant_agents: Vec<String>,
    /// Optional periodic `tools/list` refresh. `None` = refresh only on
    /// connect/push. (Refresh lifecycle lands in Phase 2; the field is part
    /// of the wire contract from day one.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_interval_secs: Option<u64>,
    /// Unix epoch (seconds). LWW merge key.
    pub updated_at: u64,
}

/// Transport used to reach an upstream MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpUpstreamTransport {
    /// Streamable-HTTP / plain HTTP JSON-RPC (`POST <url>`). The only
    /// Phase-1 transport.
    Http { url: String },
    /// Stdio subprocess. Phase 3 — gated behind a command allowlist and
    /// sandbox review; registering one is rejected until then.
    Stdio { command: String, args: Vec<String> },
}

impl McpUpstreamTransport {
    /// The URL for HTTP transports, if any.
    pub fn http_url(&self) -> Option<&str> {
        match self {
            McpUpstreamTransport::Http { url } => Some(url),
            McpUpstreamTransport::Stdio { .. } => None,
        }
    }
}

/// One remote tool explicitly allowed for projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpUpstreamToolGrant {
    /// Remote tool name exactly as advertised by the server's `tools/list`.
    pub remote_name: String,
    /// Optional per-tool call budget (calls per sliding hour). `None` = no cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allotment: Option<u32>,
    /// Max response bytes accepted from this tool. `None` = the
    /// [`DEFAULT_UPSTREAM_MAX_RESPONSE_BYTES`] default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_response_bytes: Option<u64>,
}

// ── Projected catalog (guest → hotel report) ──────────────────────────────────

/// One remote tool as observed by the `mcp-client` guest and projected into
/// philote catalogs. The description and schema are third-party content —
/// rendered with provenance, never trusted as instructions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpProjectedTool {
    /// Remote tool name as advertised by the server.
    pub remote_name: String,
    /// Remote description verbatim (untrusted third-party content).
    pub description: String,
    /// Remote input schema verbatim (validated to be a JSON object).
    pub input_schema: serde_json::Value,
}

/// The `mcp-client` guest's report of one upstream's connection state and
/// projected tools. Stored under `__mcp_upstream_catalog__:<upstream_id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpUpstreamCatalog {
    /// Which upstream this report describes.
    pub upstream_id: String,
    /// Connection state at report time.
    pub state: McpUpstreamState,
    /// Remote tools that passed the allowlist filter.
    #[serde(default)]
    pub tools: Vec<McpProjectedTool>,
    /// Allowlisted names the server did NOT advertise (grant is stale).
    #[serde(default)]
    pub missing_grants: Vec<String>,
    /// Unix epoch (seconds) of this report.
    pub reported_at: u64,
}

/// Connection state of an upstream as last reported by the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpUpstreamState {
    /// Initialize + tools/list succeeded.
    Connected,
    /// The server could not be reached or handshake failed.
    Unreachable,
    /// Reached, but the MCP handshake/list was rejected or malformed.
    ProtocolError,
}

// ── Egress policy ─────────────────────────────────────────────────────────────

/// Hotel-level egress policy for upstream MCP connections. Stored under the
/// `mcp_egress_policy` config node; absent = defaults (loopback + tailnet).
/// Widening this list is an operator action, not a philote tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct McpEgressPolicy {
    /// Additional allowed host patterns (exact hostname, or `*.` suffix
    /// wildcard like `*.example.com`). Loopback and the tailnet CGNAT range
    /// (`100.64.0.0/10`) are always allowed and need not be listed.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl McpEgressPolicy {
    /// Whether `host` (a hostname or IP literal from a URL) is permitted.
    ///
    /// Always allowed: loopback names/addresses and IPs in `100.64.0.0/10`
    /// (the Tailscale CGNAT range). Everything else must match
    /// `allowed_hosts` (exact, or `*.suffix` wildcard).
    pub fn host_allowed(&self, host: &str) -> bool {
        let host = host.trim().trim_matches(['[', ']']);
        if host.is_empty() {
            return false;
        }
        if host.eq_ignore_ascii_case("localhost") {
            return true;
        }
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if ip.is_loopback() {
                return true;
            }
            if let std::net::IpAddr::V4(v4) = ip {
                // 100.64.0.0/10 — Tailscale CGNAT range.
                let octets = v4.octets();
                if octets[0] == 100 && (64..128).contains(&octets[1]) {
                    return true;
                }
            }
        }
        self.allowed_hosts.iter().any(|pattern| {
            if let Some(suffix) = pattern.strip_prefix("*.") {
                host.len() > suffix.len()
                    && host
                        .to_ascii_lowercase()
                        .ends_with(&suffix.to_ascii_lowercase())
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
            } else {
                host.eq_ignore_ascii_case(pattern)
            }
        })
    }
}

/// Extract the host portion from an `http://` / `https://` URL without a URL
/// crate: strips the scheme, any userinfo, the port, and IPv6 brackets.
/// Returns `None` for non-HTTP schemes or empty hosts.
pub fn host_from_http_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    // Drop userinfo if present.
    let host_port = authority.rsplit('@').next()?;
    let host = if let Some(v6) = host_port.strip_prefix('[') {
        // IPv6 literal: [::1]:8080
        v6.split(']').next()?
    } else {
        host_port.split(':').next()?
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
    fn full_upstream_config() {
        round_trip(&McpUpstreamConfig {
            upstream_id: "intel-graph".into(),
            owner_agent_id: "agent-bjork-01".into(),
            transport: McpUpstreamTransport::Http {
                url: "http://127.0.0.1:8901/mcp".into(),
            },
            credential_ref: Some("vault_mcp_upstream_intel_graph".into()),
            tool_allowlist: vec![McpUpstreamToolGrant {
                remote_name: "graph_status".into(),
                allotment: Some(60),
                max_response_bytes: None,
            }],
            grant_agents: vec![],
            refresh_interval_secs: None,
            updated_at: 1_700_000_000,
        });
    }

    #[test]
    fn stdio_transport_round_trip() {
        round_trip(&McpUpstreamTransport::Stdio {
            command: "muninn".into(),
            args: vec!["mcp".into()],
        });
    }

    #[test]
    fn catalog_round_trip() {
        round_trip(&McpUpstreamCatalog {
            upstream_id: "intel-graph".into(),
            state: McpUpstreamState::Connected,
            tools: vec![McpProjectedTool {
                remote_name: "graph_status".into(),
                description: "Get overall project graph status".into(),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            }],
            missing_grants: vec!["graph_scan".into()],
            reported_at: 1_700_000_000,
        });
    }

    #[test]
    fn projected_name_round_trip() {
        let name = projected_tool_name("intel-graph", "graph_status");
        assert_eq!(name, "mcp:intel-graph.graph_status");
        assert_eq!(
            parse_projected_tool_name(&name),
            Some(("intel-graph", "graph_status"))
        );
        assert_eq!(parse_projected_tool_name("mcp:"), None);
        assert_eq!(parse_projected_tool_name("mcp:nodot"), None);
        assert_eq!(parse_projected_tool_name("other:x.y"), None);
        // Remote names may themselves contain dots — split on the FIRST dot.
        assert_eq!(
            parse_projected_tool_name("mcp:up.tool.with.dots"),
            Some(("up", "tool.with.dots"))
        );
    }

    #[test]
    fn egress_policy_defaults() {
        let policy = McpEgressPolicy::default();
        assert!(policy.host_allowed("127.0.0.1"));
        assert!(policy.host_allowed("localhost"));
        assert!(policy.host_allowed("100.64.212.8"));
        assert!(policy.host_allowed("100.127.0.1"));
        assert!(!policy.host_allowed("100.128.0.1"));
        assert!(!policy.host_allowed("example.com"));
        assert!(!policy.host_allowed("8.8.8.8"));
        assert!(!policy.host_allowed(""));
    }

    #[test]
    fn host_from_url_shapes() {
        assert_eq!(
            host_from_http_url("http://127.0.0.1:8901/mcp"),
            Some("127.0.0.1".into())
        );
        assert_eq!(
            host_from_http_url("https://api.example.com/mcp?x=1"),
            Some("api.example.com".into())
        );
        assert_eq!(
            host_from_http_url("http://[::1]:8080/mcp"),
            Some("::1".into())
        );
        assert_eq!(
            host_from_http_url("http://user:pw@host.net/x"),
            Some("host.net".into())
        );
        assert_eq!(host_from_http_url("ftp://x.com/"), None);
        assert_eq!(host_from_http_url("http://"), None);
    }

    #[test]
    fn egress_policy_wildcards() {
        let policy = McpEgressPolicy {
            allowed_hosts: vec!["api.example.com".into(), "*.internal.net".into()],
        };
        assert!(policy.host_allowed("api.example.com"));
        assert!(policy.host_allowed("API.EXAMPLE.COM"));
        assert!(policy.host_allowed("svc.internal.net"));
        assert!(!policy.host_allowed("internal.net")); // wildcard requires a label
        assert!(!policy.host_allowed("evil-internal.net"));
        assert!(!policy.host_allowed("other.example.com"));
    }
}
