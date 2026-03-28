use anyhow::Result;
use graph_intelligence::schema::Mutation;
use serde::{Deserialize, Serialize};

/// Records a graph decision as a Muninn memory.
/// This allows agents to recall past decisions through Muninn's cognitive search.
pub async fn bridge_decision_to_muninn(
    muninn_endpoint: &str,
    mutation: &Mutation,
) -> Result<()> {
    let memory_text = format_decision_as_memory(mutation);

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{muninn_endpoint}/api/engrams"))
        .json(&serde_json::json!({
            "content": memory_text,
            "metadata": {
                "source": "project-graph",
                "agent": mutation.agent,
                "session": mutation.session,
                "target_node": mutation.target_node,
                "action": mutation.action,
            }
        }))
        .send()
        .await;

    match response {
        Ok(r) if r.status().is_success() => {
            tracing::debug!("Bridged decision to Muninn: {}", mutation.id);
        }
        Ok(r) => {
            tracing::warn!(
                "Muninn bridge got status {}: decision {} not stored",
                r.status(),
                mutation.id
            );
        }
        Err(e) => {
            // Muninn being down should not block graph operations
            tracing::warn!("Muninn bridge failed (non-fatal): {e}");
        }
    }

    Ok(())
}

fn format_decision_as_memory(mutation: &Mutation) -> String {
    let mut parts = vec![];

    if let Some(ref agent) = mutation.agent {
        parts.push(format!("Agent {agent}"));
    }

    parts.push(format!("performed action '{}'", mutation.action));

    if let Some(ref target) = mutation.target_node {
        parts.push(format!("on {target}"));
    }

    if let (Some(ref from), Some(ref to)) = (&mutation.from_value, &mutation.to_value) {
        parts.push(format!("changing from '{from}' to '{to}'"));
    }

    if let Some(ref reason) = mutation.reason {
        parts.push(format!("because: {reason}"));
    }

    parts.join(" ")
}

/// Configuration for the Muninn bridge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MuninnBridgeConfig {
    pub endpoint: String,
    pub enabled: bool,
    pub bridge_decisions: bool,
    pub bridge_scans: bool,
}

impl Default for MuninnBridgeConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:8475".into(),
            enabled: true,
            bridge_decisions: true,
            bridge_scans: false,
        }
    }
}
