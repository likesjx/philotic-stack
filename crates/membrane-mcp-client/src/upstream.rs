//! Outbound MCP JSON-RPC client for one upstream server (HTTP transport).
//!
//! Hand-rolled on reqwest, mirroring the wire shapes `membrane-mcp` serves:
//! `initialize` → `tools/list` → `tools/call` over `POST <url>` JSON-RPC 2.0.

use ansible_mesh_core::mcp_upstream::{
    DEFAULT_UPSTREAM_CALL_TIMEOUT_SECS, DEFAULT_UPSTREAM_MAX_RESPONSE_BYTES, McpProjectedTool,
    McpUpstreamConfig, McpUpstreamState, McpUpstreamToolGrant,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
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
}

/// Result of connecting: state + the allowlist-filtered projected tools.
pub struct ConnectOutcome {
    pub state: McpUpstreamState,
    pub tools: Vec<McpProjectedTool>,
    pub missing_grants: Vec<String>,
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
    pub async fn connect_and_list(&mut self) -> ConnectOutcome {
        match self.try_connect_and_list().await {
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
                }
            }
        }
    }

    async fn try_connect_and_list(&mut self) -> Result<ConnectOutcome> {
        self.initialize().await?;
        let result = self.rpc("tools/list", json!({})).await?;
        let advertised = result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut tools = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for grant in &self.config.tool_allowlist {
            let found = advertised.iter().find(|t| {
                t.get("name").and_then(Value::as_str) == Some(grant.remote_name.as_str())
            });
            match found {
                Some(t) => {
                    let schema = t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"}));
                    // Schemas must be JSON objects — reject anything else rather
                    // than projecting a malformed shape into a philote catalog.
                    if !schema.is_object() {
                        warn!(
                            upstream = self.config.upstream_id,
                            tool = grant.remote_name,
                            "remote tool schema is not an object; skipping projection"
                        );
                        missing.push(grant.remote_name.clone());
                        continue;
                    }
                    tools.push(McpProjectedTool {
                        remote_name: grant.remote_name.clone(),
                        description: t
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        input_schema: schema,
                    });
                }
                None => missing.push(grant.remote_name.clone()),
            }
        }
        info!(
            upstream = self.config.upstream_id,
            advertised = advertised.len(),
            projected = tools.len(),
            missing = missing.len(),
            "upstream tools listed"
        );
        Ok(ConnectOutcome {
            state: McpUpstreamState::Connected,
            tools,
            missing_grants: missing,
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
