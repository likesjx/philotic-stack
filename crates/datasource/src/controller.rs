use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskKind {
    Query,
    CreatePartition,
    DropPartition,
    GrantAccess,
    Custom(String),
}

impl TaskKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Query => "graph.query",
            Self::CreatePartition => "graph.create",
            Self::DropPartition => "graph.drop",
            Self::GrantAccess => "graph.grant_access",
            Self::Custom(name) => name.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasourceTask {
    pub kind: TaskKind,
    pub provider: Option<String>,
    /// Named database to target. None means "default". Used by table-datasource for multi-DB routing.
    pub db: Option<String>,
    pub graph_id: Option<String>,
    pub query: Option<String>,
    pub parameters: Value,
    pub identity: Value,
}

impl DatasourceTask {
    pub fn from_value(task: &Value) -> Result<Self> {
        // When dispatched via ToolExecutionPayload (philote → EmitTask), tool arguments
        // are nested under "arguments". Fall through to that sub-object when top-level
        // lookups miss, so both direct and ToolExecutionPayload-shaped tasks work.
        let args = task.get("arguments");

        let get = |key: &str| task.get(key).or_else(|| args.and_then(|a| a.get(key)));

        let kind_str = task
            .get("kind")
            .or_else(|| task.get("tool_name"))
            .or_else(|| task.get("tool"))
            .and_then(Value::as_str);

        let kind = match kind_str {
            Some("graph.query") => TaskKind::Query,
            Some("graph.create") => TaskKind::CreatePartition,
            Some("graph.drop") => TaskKind::DropPartition,
            Some("graph.grant_access") => TaskKind::GrantAccess,
            Some(other) => TaskKind::Custom(other.to_string()),
            None => anyhow::bail!("task is missing a recognized kind/action/tool field"),
        };

        Ok(Self {
            kind,
            provider: get("provider")
                .and_then(Value::as_str)
                .map(str::to_string),
            db: get("db").and_then(Value::as_str).map(str::to_string),
            graph_id: get("graph_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            query: get("query")
                .and_then(Value::as_str)
                .map(str::to_string),
            // For ToolExecutionPayload dispatch, the entire arguments object serves as
            // parameters when no explicit "parameters" key is present.
            parameters: get("parameters")
                .cloned()
                .unwrap_or_else(|| args.cloned().unwrap_or_else(|| serde_json::json!({}))),
            identity: get("identity")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        })
    }

    pub fn provider_hint(&self) -> Option<&str> {
        self.provider.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderOutput {
    ResultSet(Value),
    PartitionCreated { graph_id: String },
    Acknowledge,
}

#[async_trait]
pub trait DatasourceProvider: Send + Sync {
    fn id(&self) -> &str;

    fn supports(&self, task: &DatasourceTask) -> bool;

    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput>;
}

pub struct ProviderRegistry {
    providers: Vec<Arc<dyn DatasourceProvider>>,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<Arc<dyn DatasourceProvider>>) -> Self {
        Self { providers }
    }

    pub fn resolve(&self, task: &DatasourceTask) -> Result<Arc<dyn DatasourceProvider>> {
        if let Some(hint) = task.provider_hint() {
            for provider in &self.providers {
                if provider.id() == hint {
                    return Ok(provider.clone());
                }
            }
            anyhow::bail!("requested provider [{hint}] not found or not initialized");
        }

        for provider in &self.providers {
            if provider.supports(task) {
                return Ok(provider.clone());
            }
        }

        anyhow::bail!(
            "no suitable datasource provider found for task kind [{}]",
            task.kind.as_str()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datasource_task_accepts_multiple_kind_fields() {
        let payload = serde_json::json!({
            "tool_name": "graph.query",
            "provider": "graph",
            "graph_id": "workspace",
            "query": "MATCH (n) RETURN n",
            "parameters": {"limit": 10},
            "identity": {"agent_id": "codex"},
        });

        let task = DatasourceTask::from_value(&payload).expect("task parsed");

        assert_eq!(task.kind, TaskKind::Query);
        assert_eq!(task.provider_hint(), Some("graph"));
        assert_eq!(task.graph_id.as_deref(), Some("workspace"));
    }

    #[test]
    fn datasource_task_prefers_tool_name_over_execute_action() {
        let payload = serde_json::json!({
            "action": "execute_tool",
            "tool_name": "graph.create",
            "graph_id": "workspace",
        });

        let task = DatasourceTask::from_value(&payload).expect("task parsed");

        assert_eq!(task.kind, TaskKind::CreatePartition);
        assert_eq!(task.graph_id.as_deref(), Some("workspace"));
    }

    #[test]
    fn datasource_task_rejects_missing_kind() {
        let payload = serde_json::json!({"parameters": {}});
        let err = DatasourceTask::from_value(&payload).expect_err("missing kind should fail");
        assert!(err.to_string().contains("recognized kind"));
    }
}
