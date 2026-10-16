use crate::runtime::ToolInvoker;
use crate::ToolRef;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use tracing::info;

/// A simple mock tool invoker for MVP 1 testing.
pub struct LocalMockInvoker;

impl ToolInvoker for LocalMockInvoker {
    fn call_tool(&self, tool: ToolRef, args: Value) -> Result<Value> {
        info!("Invoking local mock tool: {} with args: {}", tool, args);

        match tool.as_str() {
            "mcp.echo@1" => {
                // Simply echo back the arguments provided
                Ok(json!({
                    "status": "success",
                    "echoed": args
                }))
            }
            "mcp.health.query@1" => {
                // Mock health returning a static heartbeat
                Ok(json!({
                    "status": "healthy",
                    "heart_rate_bpm": 72,
                    "steps_today": 4200
                }))
            }
            _ => bail!("Unknown or unsupported tool reference: {}", tool),
        }
    }
}
