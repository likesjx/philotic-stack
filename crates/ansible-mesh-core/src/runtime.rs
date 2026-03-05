use crate::agent::AgentBundle;
use crate::{AgentId, ToolRef};
use anyhow::Result;
use serde_json::Value;

/// Input for a generic agent run request.
#[derive(Debug, Clone)]
pub struct AgentInput {
    pub message: String,
    pub session_id: String,
    pub context: Value,
}

/// Output from executing an agent run.
#[derive(Debug, Clone)]
pub struct AgentResult {
    pub output: String,
    pub tool_calls_made: usize,
}

/// Represents a runtime environment capable of managing agent lifecycles.
pub trait AgentRuntime: Send + Sync {
    /// Load an agent bundle into the runtime.
    fn load_bundle(&mut self, bundle: AgentBundle) -> Result<()>;

    /// Execute a loaded agent with the given input context.
    fn run(&self, agent_id: AgentId, input: AgentInput) -> Result<AgentResult>;
}

/// A capability endpoint for invoking local or remote tools over the mesh.
pub trait ToolInvoker: Send + Sync {
    /// Call a specific tool capability with JSON arguments.
    fn call_tool(&self, tool: ToolRef, args: Value) -> Result<Value>;
}
