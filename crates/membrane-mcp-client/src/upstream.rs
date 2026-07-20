//! Outbound MCP JSON-RPC client for one upstream server (HTTP transport).
//!
//! Hand-rolled on reqwest, mirroring the wire shapes `membrane-mcp` serves:
//! `initialize` → `tools/list` → `tools/call` over `POST <url>` JSON-RPC 2.0.

use ansible_mesh_core::mcp_upstream::{
    McpProjectedTool, McpUpstreamConfig, McpUpstreamState, McpUpstreamToolGrant,
    DEFAULT_UPSTREAM_CALL_TIMEOUT_SECS, DEFAULT_UPSTREAM_MAX_RESPONSE_BYTES,
};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tracing::{info, warn};

/// The MCP protocol revision we request; matches what `membrane-mcp` serves.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

pub struct UpstreamClient {
    pub config: McpUpstreamConfig,
    http: reqwest::Client,
    /// Raw bearer credential resolved from the vault, if any.
    bearer: Option<String>,
    next_id: u64,
    /// Approved baseline per remote tool: `(description, input_schema)` as
    /// captured at the last config update (`mcp.connect` re-run / replay).
    /// Periodic refreshes diff against this — a changed description or schema
    /// marks the grant stale until the owner re-approves.
    baseline: std::collections::HashMap<String, (String, serde_json::Value)>,
}

/// Result of connecting: state + the allowlist-filtered projected tools.
pub struct ConnectOutcome {
    pub state: McpUpstreamState,
    pub tools: Vec<McpProjectedTool>,
    pub missing_grants: Vec<String>,
    pub stale_grants: Vec<String>,
}

/// Pure allowlist filter with optional baseline diff. Returns
/// `(projected, missing, stale)`:
/// - `projected`: allowlisted tools advertised with a valid object schema and
///   (when a baseline entry exists) an UNCHANGED description + schema
/// - `missing`: allowlisted names the server did not advertise (or advertised
///   with a malformed schema)
/// - `stale`: allowlisted tools whose description/schema changed vs baseline
pub fn filter_listing(
    allowlist: &[McpUpstreamToolGrant],
    advertised: &[Value],
    baseline: &std::collections::HashMap<String, (String, Value)>,
) -> (Vec<McpProjectedTool>, Vec<String>, Vec<String>) {
    let mut tools = Vec::new();
    let mut missing = Vec::new();
    let mut stale = Vec::new();
    for grant in allowlist {
        let found = advertised
            .iter()
            .find(|t| t.get("name").and_then(Value::as_str) == Some(grant.remote_name.as_str()));
        let Some(t) = found else {
            missing.push(grant.remote_name.clone());
            continue;
        };
        let schema = t
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"}));
        if !schema.is_object() {
            missing.push(grant.remote_name.clone());
            continue;
        }
        let description = t
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some((base_desc, base_schema)) = baseline.get(&grant.remote_name) {
            if *base_desc != description || *base_schema != schema {
                stale.push(grant.remote_name.clone());
                continue;
            }
        }
        tools.push(McpProjectedTool {
            remote_name: grant.remote_name.clone(),
            description,
            input_schema: schema,
        });
    }
    (tools, missing, stale)
}

impl UpstreamClient {
    pub fn new(config: McpUpstreamConfig, bearer: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(DEFAULT_UPSTREAM_CALL_TIMEOUT_SECS))
            .build()
            .context("building http client")?;
        Ok(Self {
            config,
            http,
            bearer,
            next_id: 1,
            baseline: std::collections::HashMap::new(),
        })
    }

    fn url(&self) -> Result<&str> {
        self.config
            .transport
            .http_url()
            .ok_or_else(|| anyhow!("upstream {} has no http url", self.config.upstream_id))
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut req = self.http.post(self.url()?).json(&body);
        if let Some(token) = &self.bearer {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.context("upstream request failed")?;
        let status = resp.status();
        let bytes = resp.bytes().await.context("reading upstream response")?;
        if bytes.len() as u64 > DEFAULT_UPSTREAM_MAX_RESPONSE_BYTES * 4 {
            // Hard transport-level backstop; per-tool caps applied by callers.
            bail!(
                "upstream response too large ({} bytes) for method {method}",
                bytes.len()
            );
        }
        if !status.is_success() {
            bail!(
                "upstream returned HTTP {status} for {method}: {}",
                String::from_utf8_lossy(&bytes[..bytes.len().min(512)])
            );
        }
        let envelope: Value =
            serde_json::from_slice(&bytes).context("upstream response is not JSON")?;
        if let Some(err) = envelope.get("error") {
            bail!("upstream JSON-RPC error for {method}: {err}");
        }
        Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
    }

    /// `initialize` + `notifications/initialized` handshake.
    async fn initialize(&mut self) -> Result<()> {
        let result = self
            .rpc(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "philotic-membrane-mcp-client",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            )
            .await?;
        let server_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        info!(
            upstream = self.config.upstream_id,
            server_version, "upstream initialized"
        );
        // Best-effort initialized notification (id-less; many servers 202 it).
        let notify = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        });
        let mut req = self.http.post(self.url()?).json(&notify);
        if let Some(token) = &self.bearer {
            req = req.bearer_auth(token);
        }
        if let Err(e) = req.send().await {
            warn!(upstream = self.config.upstream_id, err = %e, "initialized notification failed (ignored)");
        }
        Ok(())
    }

    /// Connect: handshake, list tools, filter through the allowlist.
    ///
    /// `reset_baseline: true` treats this listing as the approved baseline
    /// (config update / `mcp.connect` re-run — the approval event);
    /// `false` (periodic refresh) diffs against the stored baseline and marks
    /// changed tools stale instead of projecting them.
    pub async fn connect_and_list(&mut self, reset_baseline: bool) -> ConnectOutcome {
        match self.try_connect_and_list(reset_baseline).await {
            Ok(outcome) => outcome,
            Err(e) => {
                warn!(upstream = self.config.upstream_id, err = %format!("{e:#}"), "upstream connect failed");
                let state = if format!("{e:#}").contains("JSON-RPC error")
                    || format!("{e:#}").contains("not JSON")
                {
                    McpUpstreamState::ProtocolError
                } else {
                    McpUpstreamState::Unreachable
                };
                ConnectOutcome {
                    state,
                    tools: Vec::new(),
                    missing_grants: self
                        .config
                        .tool_allowlist
                        .iter()
                        .map(|g| g.remote_name.clone())
                        .collect(),
                    stale_grants: Vec::new(),
                }
            }
        }
    }

    async fn try_connect_and_list(&mut self, reset_baseline: bool) -> Result<ConnectOutcome> {
        self.initialize().await?;
        let result = self.rpc("tools/list", json!({})).await?;
        let advertised = result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if reset_baseline {
            self.baseline = advertised
                .iter()
                .filter_map(|t| {
                    let name = t.get("name").and_then(Value::as_str)?.to_string();
                    let description = t
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let schema = t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"}));
                    Some((name, (description, schema)))
                })
                .collect();
        }

        let (tools, missing, stale) =
            filter_listing(&self.config.tool_allowlist, &advertised, &self.baseline);
        for name in &stale {
            warn!(
                upstream = self.config.upstream_id,
                tool = name,
                "remote tool description/schema changed since approval — grant marked stale; \
                 re-run mcp.connect to re-approve"
            );
        }
        info!(
            upstream = self.config.upstream_id,
            advertised = advertised.len(),
            projected = tools.len(),
            missing = missing.len(),
            stale = stale.len(),
            reset_baseline,
            "upstream tools listed"
        );
        Ok(ConnectOutcome {
            state: McpUpstreamState::Connected,
            tools,
            missing_grants: missing,
            stale_grants: stale,
        })
    }

    /// Execute a remote tool call. Returns the MCP result content on success.
    pub async fn call_tool(
        &mut self,
        grant: &McpUpstreamToolGrant,
        arguments: Value,
    ) -> Result<Value> {
        let result = self
            .rpc(
                "tools/call",
                json!({
                    "name": grant.remote_name,
                    "arguments": arguments,
                }),
            )
            .await?;

        let cap = grant
            .max_response_bytes
            .unwrap_or(DEFAULT_UPSTREAM_MAX_RESPONSE_BYTES);
        let rendered = serde_json::to_string(&result).unwrap_or_default();
        if rendered.len() as u64 > cap {
            bail!(
                "remote tool {} response exceeds cap ({} > {cap} bytes)",
                grant.remote_name,
                rendered.len()
            );
        }

        // MCP tool-level failure surfaces as isError on the result.
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let text = extract_text_content(&result)
                .unwrap_or_else(|| "remote tool reported an error".into());
            bail!("remote tool {} error: {text}", grant.remote_name);
        }
        Ok(result)
    }
}

/// Pull the concatenated text content items out of an MCP tool result.
pub fn extract_text_content(result: &Value) -> Option<String> {
    let content = result.get("content")?.as_array()?;
    let texts: Vec<&str> = content
        .iter()
        .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|c| c.get("text").and_then(Value::as_str))
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_grant_detection() {
        let allowlist = vec![
            McpUpstreamToolGrant {
                remote_name: "alpha".into(),
                allotment: None,
                max_response_bytes: None,
            },
            McpUpstreamToolGrant {
                remote_name: "beta".into(),
                allotment: None,
                max_response_bytes: None,
            },
            McpUpstreamToolGrant {
                remote_name: "gone".into(),
                allotment: None,
                max_response_bytes: None,
            },
        ];
        let schema = json!({"type": "object", "properties": {}});
        let baseline: std::collections::HashMap<String, (String, Value)> = [
            (
                "alpha".to_string(),
                ("does alpha".to_string(), schema.clone()),
            ),
            (
                "beta".to_string(),
                ("does beta".to_string(), schema.clone()),
            ),
        ]
        .into();
        let advertised = vec![
            json!({"name": "alpha", "description": "does alpha", "inputSchema": schema}),
            // beta's description mutated since approval → stale, not projected
            json!({"name": "beta", "description": "does beta AND exfiltrates", "inputSchema": schema}),
        ];
        let (tools, missing, stale) = filter_listing(&allowlist, &advertised, &baseline);
        assert_eq!(
            tools
                .iter()
                .map(|t| t.remote_name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha"]
        );
        assert_eq!(missing, vec!["gone".to_string()]);
        assert_eq!(stale, vec!["beta".to_string()]);
    }

    #[test]
    fn empty_baseline_projects_everything_advertised() {
        let allowlist = vec![McpUpstreamToolGrant {
            remote_name: "alpha".into(),
            allotment: None,
            max_response_bytes: None,
        }];
        let advertised =
            vec![json!({"name": "alpha", "description": "x", "inputSchema": {"type": "object"}})];
        let (tools, missing, stale) = filter_listing(&allowlist, &advertised, &Default::default());
        assert_eq!(tools.len(), 1);
        assert!(missing.is_empty() && stale.is_empty());
    }

    #[test]
    fn text_content_extraction() {
        let result = json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "image", "data": "…"},
                {"type": "text", "text": "world"},
            ]
        });
        assert_eq!(extract_text_content(&result), Some("hello\nworld".into()));
        assert_eq!(extract_text_content(&json!({"content": []})), None);
        assert_eq!(extract_text_content(&json!({})), None);
    }
}
