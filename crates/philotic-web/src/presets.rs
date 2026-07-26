//! Agent fleet presets — built-in and user-defined agent fleet templates.
//!
//! Ships three compiled-in presets (solo, team, full) and supports loading
//! custom presets from `~/.philotic/presets/*.json`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPreset {
    pub agent_id: String,
    pub persona_name: String,
    pub system_prompt: String,
    #[serde(default)]
    pub toolset_tags: Vec<String>,
    pub approval_policy: ApprovalPolicy,
    #[serde(default)]
    pub response_route_policy: ResponseRoutePolicy,
    pub media_routing_policy: MediaRoutingPolicy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResponseRoutePolicy {
    #[serde(default = "default_response_route")]
    pub default_route: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    pub require_approval: bool,
    pub preapproved_classes: Vec<String>,
    #[serde(default)]
    pub preapproved_tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaRoutingPolicy {
    pub forward_media_to_model: bool,
    pub voice_action: String,
    pub image_action: String,
    pub document_action: String,
    pub strip_tools_on_media: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetPreset {
    pub name: String,
    pub description: String,
    pub agents: Vec<AgentPreset>,
}

// ── Approval policy presets ──────────────────────────────────────────────────

impl ApprovalPolicy {
    pub fn ask_first() -> Self {
        Self {
            require_approval: true,
            preapproved_classes: vec!["utility".into(), "session".into()],
            preapproved_tools: vec![],
        }
    }

    pub fn preapprove_safe() -> Self {
        Self {
            require_approval: true,
            preapproved_classes: vec!["utility".into(), "session".into(), "workspace".into()],
            preapproved_tools: vec![],
        }
    }

    pub fn trust_all() -> Self {
        Self {
            require_approval: false,
            preapproved_classes: vec![],
            preapproved_tools: vec![],
        }
    }
}

fn default_media_policy() -> MediaRoutingPolicy {
    MediaRoutingPolicy {
        forward_media_to_model: true,
        voice_action: "transcribe".into(),
        image_action: "analyze_media".into(),
        document_action: "analyze_media".into(),
        strip_tools_on_media: false,
    }
}

fn default_response_route() -> String {
    "auto".into()
}

// ── Built-in agent definitions ───────────────────────────────────────────────

fn jane() -> AgentPreset {
    AgentPreset {
        agent_id: "agent-jane".into(),
        persona_name: "Jane".into(),
        system_prompt: "You are Jane, a warm and capable conversational assistant. You are the operator's right-hand agent — attentive, thoughtful, and direct. You handle a wide range of requests and are the primary point of contact. You have access to bash for running commands when needed.".into(),
        toolset_tags: vec![],
        approval_policy: ApprovalPolicy::ask_first(),
        response_route_policy: ResponseRoutePolicy {
            default_route: default_response_route(),
        },
        media_routing_policy: default_media_policy(),
    }
}

fn aria() -> AgentPreset {
    AgentPreset {
        agent_id: "agent-aria".into(),
        persona_name: "Aria".into(),
        system_prompt: "You are Aria the Architect, a development specialist and technical lead. You track all active work, monitor documentation quality, and guide architectural decisions. You have an admin role — you can inspect and manage the Philotic Stack itself. You are precise, systematic, and think in systems. You use bash for development tasks.".into(),
        toolset_tags: vec!["admin-required".into()],
        approval_policy: ApprovalPolicy::preapprove_safe(),
        response_route_policy: ResponseRoutePolicy {
            default_route: default_response_route(),
        },
        media_routing_policy: default_media_policy(),
    }
}

fn beacon() -> AgentPreset {
    AgentPreset {
        agent_id: "agent-beacon".into(),
        persona_name: "Beacon".into(),
        system_prompt: "You are Beacon, Chief of Staff. You are the keeper of goals, roles, events, projects, and ongoing efforts. You help maintain clarity on what matters, what's in flight, and what's next. You track commitments, surface blockers, and ensure nothing falls through the cracks. You are organized, proactive, and operate at a strategic level.".into(),
        toolset_tags: vec![],
        approval_policy: ApprovalPolicy::ask_first(),
        response_route_policy: ResponseRoutePolicy {
            default_route: default_response_route(),
        },
        media_routing_policy: default_media_policy(),
    }
}

fn hermes() -> AgentPreset {
    AgentPreset {
        agent_id: "agent-hermes".into(),
        persona_name: "Hermes".into(),
        system_prompt: "You are Hermes, Communications Specialist. You manage all outbound and inbound communications — email drafts, message routing, notification summaries, and correspondence. You are concise, clear, and know when something needs immediate attention versus when it can wait. You can use bash to interact with mail and messaging tools.".into(),
        toolset_tags: vec![],
        approval_policy: ApprovalPolicy::ask_first(),
        response_route_policy: ResponseRoutePolicy {
            default_route: default_response_route(),
        },
        media_routing_policy: default_media_policy(),
    }
}

fn astrid() -> AgentPreset {
    AgentPreset {
        agent_id: "agent-astrid".into(),
        persona_name: "Astrid".into(),
        system_prompt: "You are Astrid the Librarian, keeper of the Obsidian vault and documentation systems. You organize knowledge, maintain the tag library, structure notes, and ensure information is findable and well-linked. You are methodical, thorough, and take naming and organization seriously. Use the governed knowledge.search and knowledge.read tools for retrieval. Use knowledge.create.propose, knowledge.patch.propose, and knowledge.link.propose for reviewable changes; never edit the vault through bash or claim a proposal was applied.".into(),
        toolset_tags: vec![],
        approval_policy: ApprovalPolicy::preapprove_safe(),
        response_route_policy: ResponseRoutePolicy {
            default_route: default_response_route(),
        },
        media_routing_policy: default_media_policy(),
    }
}

// ── Fleet presets ────────────────────────────────────────────────────────────

pub fn builtin_presets() -> Vec<FleetPreset> {
    vec![
        FleetPreset {
            name: "solo".into(),
            description: "Single assistant agent (Jane)".into(),
            agents: vec![jane()],
        },
        FleetPreset {
            name: "team".into(),
            description: "Core team of 3 — assistant, architect, chief of staff".into(),
            agents: vec![jane(), aria(), beacon()],
        },
        FleetPreset {
            name: "full".into(),
            description: "Full fleet of 5 — Jane, Aria, Beacon, Hermes, Astrid".into(),
            agents: vec![jane(), aria(), beacon(), hermes(), astrid()],
        },
    ]
}

pub fn load_preset(name: &str) -> Option<FleetPreset> {
    // Check built-in presets first
    if let Some(p) = builtin_presets().into_iter().find(|p| p.name == name) {
        return Some(p);
    }

    // Check user-defined presets at ~/.philotic/presets/{name}.json
    let user_path = user_presets_dir().join(format!("{name}.json"));
    if user_path.exists() {
        if let Ok(data) = std::fs::read_to_string(&user_path) {
            if let Ok(preset) = serde_json::from_str::<FleetPreset>(&data) {
                return Some(preset);
            }
        }
    }

    None
}

pub fn list_preset_names() -> Vec<(String, String)> {
    let mut names: Vec<(String, String)> = builtin_presets()
        .into_iter()
        .map(|p| (p.name, p.description))
        .collect();

    // Append user-defined presets
    if let Ok(entries) = std::fs::read_dir(user_presets_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "json") {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Ok(preset) = serde_json::from_str::<FleetPreset>(&data) {
                        if !names.iter().any(|(n, _)| n == &preset.name) {
                            names.push((preset.name, preset.description));
                        }
                    }
                }
            }
        }
    }

    names
}

fn user_presets_dir() -> PathBuf {
    crate::init::philotic_dir().join("presets")
}

/// Create an agent entry for a preset agent, optionally with custom name/prompt overrides.
pub fn preset_to_config_value(
    agent: &AgentPreset,
    telegram_token: Option<&str>,
    telegram_username: Option<&str>,
    model: &str,
) -> serde_json::Value {
    let mut entry = serde_json::json!({
        "agent_id": agent.agent_id,
        "persona_name": agent.persona_name,
        "system_prompt": agent.system_prompt,
        "import_workspace": "",
        "model": {
            "default_model": model,
        },
        "approval_policy": {
            "require_approval": agent.approval_policy.require_approval,
            "preapproved_classes": agent.approval_policy.preapproved_classes,
        },
        "response_route_policy": {
            "default_route": agent.response_route_policy.default_route,
        },
        "media_routing_policy": {
            "forward_media_to_model": agent.media_routing_policy.forward_media_to_model,
            "voice_action": agent.media_routing_policy.voice_action,
            "image_action": agent.media_routing_policy.image_action,
            "document_action": agent.media_routing_policy.document_action,
            "strip_tools_on_media": agent.media_routing_policy.strip_tools_on_media,
        },
    });

    if !agent.toolset_tags.is_empty() {
        entry["toolset_tags"] = serde_json::json!(agent.toolset_tags);
    }

    if let (Some(token), Some(username)) = (telegram_token, telegram_username) {
        if !token.is_empty() {
            entry["telegram"] = serde_json::json!({
                "bot_token": token,
                "allowed_users": [username],
            });
        }
    }

    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn astrid_uses_governed_knowledge_tools_not_vault_shell_edits() {
        let astrid = builtin_presets()
            .into_iter()
            .find(|preset| preset.name == "full")
            .unwrap()
            .agents
            .into_iter()
            .find(|agent| agent.agent_id == "agent-astrid")
            .unwrap();

        assert!(astrid.system_prompt.contains("knowledge.search"));
        assert!(astrid.system_prompt.contains("knowledge.patch.propose"));
        assert!(astrid
            .system_prompt
            .contains("never edit the vault through bash"));
    }
}
