//! Dispatch layer — converts a resolved route + args into a result.
//!
//! # Slice 1
//!
//! `IpcDispatcher` is a stub that acknowledges the call without forwarding
//! to a live IPC target. Slice 2 wires it to `PhiloticClient` and adds
//! response-correlation so the HTTP caller gets the real tool result.
//!
//! # Dispatch targets
//!
//! - `Philote` — creates an InboundTask directed at the target agent and
//!   parks a oneshot channel awaiting `McpToolResult` IPC push.
//! - `Tool` — sends an IPC tool-invocation request to tool-runner and awaits reply.
//! - `Datasource` — sends a query request to the datasource guest and awaits reply.

use anyhow::Result;
use ansible_mesh_core::mcp_route::McpRouteTarget;
use serde_json::Value;
use tracing::info;

use crate::auth::CallerIdentity;
use crate::routing::ResolvedRoute;

// ── Dispatcher trait ──────────────────────────────────────────────────────────

/// Converts a verified (post-auth) tool call into a result value.
#[async_trait::async_trait]
pub trait Dispatcher: Send + Sync {
    async fn dispatch(
        &self,
        route: &ResolvedRoute,
        args: Value,
        caller: &CallerIdentity,
        correlation_id: &str,
    ) -> Result<Value>;
}

// ── Stub dispatcher (Slice 1) ─────────────────────────────────────────────────

/// Returns a placeholder acknowledging the dispatch. Used in Slice 1 while IPC
/// response-correlation is not yet wired.
pub struct StubDispatcher;

#[async_trait::async_trait]
impl Dispatcher for StubDispatcher {
    async fn dispatch(
        &self,
        route: &ResolvedRoute,
        args: Value,
        caller: &CallerIdentity,
        correlation_id: &str,
    ) -> Result<Value> {
        info!(
            tool = route.tool_name(),
            caller_id = %caller.token_id,
            correlation_id,
            target = ?route.target(),
            "dispatching tool call [stub]"
        );

        let target_label = match route.target() {
            McpRouteTarget::Philote { agent_id } => format!("philote:{}", agent_id),
            McpRouteTarget::Tool { tool_ref } => format!("tool:{}", tool_ref),
            McpRouteTarget::Datasource { datasource_id } => {
                format!("datasource:{}", datasource_id)
            }
        };

        Ok(serde_json::json!({
            "dispatched": true,
            "correlation_id": correlation_id,
            "target": target_label,
            "tool": route.tool_name(),
            "args_received": args,
            "note": "Slice 1 stub — IPC response wiring lands in Slice 2"
        }))
    }
}

// Bring in async_trait for the trait definition above.
// The crate adds it as a proc-macro dependency below.
