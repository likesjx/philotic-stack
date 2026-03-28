use anyhow::Result;
use graph_intelligence::schema::{Mutation, Node};
use graph_intelligence::GraphEngine;

/// Query the graph for an agent's self-model — what proposals govern it,
/// what code implements it, what decisions it has made.
pub struct AgentSelfModel<'a> {
    engine: &'a GraphEngine,
    agent_name: String,
}

impl<'a> AgentSelfModel<'a> {
    pub fn new(engine: &'a GraphEngine, agent_name: &str) -> Self {
        Self {
            engine,
            agent_name: agent_name.to_string(),
        }
    }

    /// Get all proposals this agent is associated with (via decisions)
    pub fn governing_proposals(&self) -> Result<Vec<Node>> {
        let mutations = self.engine.get_mutations(None, 1000)?;
        let proposal_ids: Vec<String> = mutations
            .iter()
            .filter(|m| m.agent.as_deref() == Some(&self.agent_name))
            .filter_map(|m| m.target_node.clone())
            .filter(|id| id.starts_with("proposal:") || id.starts_with("doc:"))
            .collect();

        let mut proposals = vec![];
        for id in &proposal_ids {
            if let Some(node) = self.engine.get_node(id)? {
                if !proposals.iter().any(|p: &Node| p.id == node.id) {
                    proposals.push(node);
                }
            }
        }
        Ok(proposals)
    }

    /// Get all decisions this agent has made, most recent first
    pub fn recent_decisions(&self, limit: usize) -> Result<Vec<Mutation>> {
        let all = self.engine.get_mutations(None, limit * 10)?;
        Ok(all
            .into_iter()
            .filter(|m| m.agent.as_deref() == Some(&self.agent_name))
            .take(limit)
            .collect())
    }

    /// Get a summary of this agent's activity
    pub fn activity_summary(&self) -> Result<serde_json::Value> {
        let decisions = self.recent_decisions(100)?;
        let proposals = self.governing_proposals()?;

        let decision_count = decisions.len();
        let proposal_count = proposals.len();
        let last_active = decisions.first().map(|d| d.timestamp.to_string());

        let mut action_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for d in &decisions {
            *action_counts.entry(d.action.clone()).or_default() += 1;
        }

        Ok(serde_json::json!({
            "agent": self.agent_name,
            "total_decisions": decision_count,
            "proposals_touched": proposal_count,
            "last_active": last_active,
            "action_breakdown": action_counts,
            "recent_proposals": proposals.iter().take(5).map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "status": p.properties.get("status"),
                })
            }).collect::<Vec<_>>(),
        }))
    }
}
