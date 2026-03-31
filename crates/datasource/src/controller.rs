use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

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
    pub graph_id: Option<String>,
    pub query: Option<String>,
    pub parameters: Value,
    pub identity: Value, // Caller identity
}

impl DatasourceTask {
    pub fn from_value(task: &Value) -> Result<Self> {
        let kind_str = task
            .get("kind")
            .or_else(|| task.get("action"))
            .or_else(|| task.get("tool_name"))
            .or_else(|| task.get("tool"))
            .and_then(Value::as_str);

        let kind = match kind_str {
            Some("graph.query") => TaskKind::Query,
            Some("graph.create") => TaskKind::CreatePartition,
            Some("graph.drop") => TaskKind::DropPartition,
            Some("graph.grant_access") => TaskKind::GrantAccess,
            Some(other) => TaskKind::Custom(other.to_string()),
            None => anyhow::bail!("task is missing a recognized kind/action/tool enum payloads"),
        };

        Ok(Self {
            kind,
            provider: task
                .get("provider")
                .and_then(Value::as_str)
                .map(str::to_string),
            graph_id: task
                .get("graph_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            query: task
                .get("query")
                .and_then(Value::as_str)
                .map(str::to_string),
            parameters: task
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
            identity: task
                .get("identity")
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
    /// Unique identifier for this provider (e.g., "sqlite", "age")
    fn id(&self) -> &str;

    /// Is this provider capable of handling the current task type?
    fn supports(&self, task: &DatasourceTask) -> bool;

    /// Execute the payload.
    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput>;
}

pub struct ProviderRegistry {
    providers: Vec<std::sync::Arc<dyn DatasourceProvider>>,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<std::sync::Arc<dyn DatasourceProvider>>) -> Self {
        Self { providers }
    }

    pub fn resolve(&self, task: &DatasourceTask) -> Result<std::sync::Arc<dyn DatasourceProvider>> {
        if let Some(hint) = task.provider_hint() {
            for provider in &self.providers {
                if provider.id() == hint {
                    return Ok(provider.clone());
                }
            }
            anyhow::bail!("requested provider [{}] not found or not initialized", hint);
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
