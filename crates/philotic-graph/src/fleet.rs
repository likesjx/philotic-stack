use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub name: String,
    pub display_name: String,
    pub role: String,
    pub description: String,
}

/// Known agent identities in the Philotic fleet.
/// Used for traceability — linking decisions to the agent that made them.
pub fn known_agents() -> Vec<AgentIdentity> {
    vec![
        AgentIdentity {
            name: "jane".into(),
            display_name: "Jane".into(),
            role: "assistant".into(),
            description: "Primary conversational assistant".into(),
        },
        AgentIdentity {
            name: "aria".into(),
            display_name: "Aria".into(),
            role: "architect".into(),
            description: "Technical lead and development specialist".into(),
        },
        AgentIdentity {
            name: "beacon".into(),
            display_name: "Beacon".into(),
            role: "chief-of-staff".into(),
            description: "Keeper of goals, projects, and commitments".into(),
        },
        AgentIdentity {
            name: "hermes".into(),
            display_name: "Hermes".into(),
            role: "communicator".into(),
            description: "Communications and correspondence specialist".into(),
        },
        AgentIdentity {
            name: "astrid".into(),
            display_name: "Astrid".into(),
            role: "librarian".into(),
            description: "Knowledge organization and documentation".into(),
        },
        AgentIdentity {
            name: "perplexity-computer".into(),
            display_name: "Perplexity Computer".into(),
            role: "external-agent".into(),
            description: "External agent harness (Perplexity)".into(),
        },
        AgentIdentity {
            name: "claude-code".into(),
            display_name: "Claude Code".into(),
            role: "external-agent".into(),
            description: "External coding agent (Anthropic)".into(),
        },
        AgentIdentity {
            name: "codex".into(),
            display_name: "Codex".into(),
            role: "external-agent".into(),
            description: "External coding agent (OpenAI)".into(),
        },
        AgentIdentity {
            name: "windsurf-cascade".into(),
            display_name: "Windsurf Cascade".into(),
            role: "external-agent".into(),
            description: "External coding agent (Windsurf)".into(),
        },
        AgentIdentity {
            name: "gemini-antigravity".into(),
            display_name: "Gemini Antigravity".into(),
            role: "external-agent".into(),
            description: "External coding agent (Google)".into(),
        },
        AgentIdentity {
            name: "human".into(),
            display_name: "Human Operator".into(),
            role: "operator".into(),
            description: "Manual human intervention".into(),
        },
    ]
}

pub fn lookup_agent(name: &str) -> Option<AgentIdentity> {
    known_agents().into_iter().find(|a| a.name == name)
}
