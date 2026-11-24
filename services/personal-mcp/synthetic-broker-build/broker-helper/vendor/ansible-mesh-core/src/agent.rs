use crate::{AgentId, ModelRef, ToolRef};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// An Agent Bundle definition (`agent_bundle.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBundle {
    pub agent_id: AgentId,
    pub version: String,
    pub identity: AgentIdentity,
    #[serde(default)]
    pub models: Vec<ModelRef>,
    #[serde(default)]
    pub tools: Vec<ToolRef>,
    #[serde(default)]
    pub hooks: HashMap<String, String>,
    #[serde(default)]
    pub routing_prefs: RoutingPrefs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub soul: String,
    pub profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoutingPrefs {
    #[serde(default)]
    pub preferred_roles: Vec<crate::NodeRole>,
    #[serde(default)]
    pub avoid_roles: Vec<crate::NodeRole>,
}
