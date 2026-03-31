pub mod tools;

use ansible_mesh_core::agent_graph_storage::SqliteAgentGraphStorage;
use anyhow::Result;
use async_trait::async_trait;
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct AgentDatasourceProvider {
    storage: Arc<SqliteAgentGraphStorage>,
    agent_id: String,
}

impl AgentDatasourceProvider {
    pub fn new(agent_id: String, storage: Arc<SqliteAgentGraphStorage>) -> Self {
        Self { agent_id, storage }
    }
}

#[async_trait]
impl DatasourceProvider for AgentDatasourceProvider {
    fn id(&self) -> &str {
        "agent-sqlite"
    }

    fn supports(&self, task: &DatasourceTask) -> bool {
        let name = task.kind.as_str();
        name.starts_with("agent.graph.")
    }

    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let tool_name = task.kind.as_str();

        let result_str = tools::dispatch(
            self.storage.as_ref(),
            tool_name,
            &task.parameters,
            &self.agent_id,
        );

        let result_json: Value =
            serde_json::from_str(&result_str).unwrap_or_else(|_| json!({"raw": result_str}));

        let outcome = if result_json
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "success"
        } else {
            "failure"
        };
        tools::record_tool_call_trace(
            self.storage.as_ref(),
            &self.agent_id,
            tool_name,
            &task.parameters,
            &result_json,
            outcome,
        );

        Ok(ProviderOutput::ResultSet(result_json))
    }
}
