pub use ansible_mesh_core::cron::{CronJob, CronJobId, CronJobSource};
pub use ansible_mesh_core::graph::RoleIncarnationRecord;
pub use ansible_mesh_core::resources::{
    ResourceDenied, ResourceGranted, ResourceMaterializing, ResourceReleased, ResourceRequest,
    ResourceRevoked, ResourceType,
};
pub use ansible_mesh_core::storage::AgentIdentityRecord;
pub use ansible_mesh_core::storage::ComponentManifest;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::ErrorKind;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Represents the identity of a Guest materializing in the Hotel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestIdentity {
    pub guest_id: String,
    pub role: String,
    #[serde(default)]
    pub supported_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopMembraneStatusView {
    pub hotel: String,
    pub daemon: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopMembraneGuestView {
    pub guest_id: String,
    pub name: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorAgentView {
    pub agent_id: String,
    pub persona_name: String,
    pub authority_hotel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soul_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_context_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_workspace: Option<String>,
    #[serde(default)]
    pub toolset_tags: Vec<String>,
    #[serde(default)]
    pub default_toolset: Vec<String>,
    #[serde(default)]
    pub default_skillset: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_route_policy: Option<ResponseRoutePolicyView>,
    #[serde(default)]
    pub active_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseRoutePolicyView {
    pub default_route: String,
}

pub type DesktopMembraneAgentView = OperatorAgentView;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetReachabilityView {
    pub protocol: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    #[serde(default)]
    pub is_local: bool,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub advertised_roles: Vec<String>,
    pub freshness_state: String,
    pub freshness_age_secs: u64,
    pub freshness_ttl_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachability: Option<OperatorTargetReachabilityView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetStatusView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    pub observation_kind: String,
    pub daemon_status: String,
    pub freshness_state: String,
    pub freshness_age_secs: u64,
    pub freshness_ttl_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachability: Option<OperatorTargetReachabilityView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetGuestInventoryView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    pub observation_kind: String,
    pub available: bool,
    pub pending_remote_query_state: String,
    #[serde(default)]
    pub guests: Vec<DesktopMembraneGuestView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetAgentInventoryView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    pub observation_kind: String,
    pub available: bool,
    pub pending_remote_query_state: String,
    #[serde(default)]
    pub agents: Vec<OperatorAgentView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentInventoryEntryView {
    pub guest_id: String,
    pub role: String,
    pub hotel: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub component_type: String,
    pub is_active: bool,
    pub auto_start: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_pid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<u64>,
    #[serde(default)]
    pub component_config: serde_json::Value,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetComponentInventoryView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    pub observation_kind: String,
    pub available: bool,
    pub pending_remote_query_state: String,
    #[serde(default)]
    pub components: Vec<ComponentInventoryEntryView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetComponentMutationAckView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    pub guest_id: String,
    pub operation: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetConfigView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    pub observation_kind: String,
    pub available: bool,
    pub pending_remote_query_state: String,
    #[serde(default)]
    pub config: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetConfigMutationAckView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    pub key: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetSecretEntryView {
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetSecretInventoryView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    pub observation_kind: String,
    pub available: bool,
    pub pending_remote_query_state: String,
    #[serde(default)]
    pub vault_entries: Vec<OperatorTargetSecretEntryView>,
    #[serde(default)]
    pub config_refs: Vec<OperatorTargetSecretEntryView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetSecretMutationAckView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_name: Option<String>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorTargetPlacementView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    pub observation_kind: String,
    pub available: bool,
    pub pending_remote_query_state: String,
    pub placement: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTargetRoleHomeAckView {
    pub target_node_id: String,
    pub target_hotel: String,
    pub source_hotel: String,
    pub agent_id: String,
    pub role_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_node: Option<String>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub type DesktopMembraneTargetReachabilityView = OperatorTargetReachabilityView;
pub type DesktopMembraneTargetView = OperatorTargetView;
pub type DesktopMembraneTargetStatusView = OperatorTargetStatusView;
pub type DesktopMembraneTargetGuestInventoryView = OperatorTargetGuestInventoryView;
pub type DesktopMembraneTargetAgentInventoryView = OperatorTargetAgentInventoryView;
pub type DesktopMembraneTargetComponentInventoryView = OperatorTargetComponentInventoryView;

pub const OPERATOR_SURFACE_QUERY_ROLE: &str = "management.operator_surface_query";
/// Mesh role for muninn-cluster single-writer routing: shared-vault memory
/// writes from lobe hotels are forwarded as TaskInvoke envelopes with this
/// target role to the Cortex hotel, whose aiua applies them to the cluster
/// PRIMARY in-process (`deliver_event_envelope_or_park` interception — same
/// pattern as `OPERATOR_SURFACE_QUERY_ROLE`). Never subscribed by any guest.
pub const MEMORY_WRITE_FORWARD_ROLE: &str = "hotel.memory_write_forward";
pub const OPERATOR_SURFACE_QUERY_REPLY_ROLE: &str = "management.operator_surface_query.reply";
pub const OPERATOR_SURFACE_QUERY_HANDOFF_KIND: &str = "operator_surface_query";
pub const OPERATOR_REMOTE_CONFIG_KEYS: &[&str] =
    &["execution_host", "vault_registry", "tool_runner_registry"];
pub const OPERATOR_REMOTE_MUTABLE_CONFIG_KEYS: &[&str] =
    &["execution_host", "tool_runner_registry"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorSurfaceQueryHandoff {
    pub handoff_kind: String,
    pub surface: String,
    pub request_id: String,
    pub source_hotel: String,
    pub target_hotel: String,
    pub target_node_id: String,
    pub caller_kind: String,
    pub caller_id: String,
    pub visibility_scope: String,
    pub grant_scope: String,
    pub intent: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub reply_to_node: String,
    pub reply_to_role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_guest_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
}

pub const OPERATOR_CHAT_REPLY_ROLE: &str = "management.operator_chat.reply";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorChatTurnReply {
    pub source_hotel: String,
    pub target_hotel: String,
    pub target_node_id: String,
    pub target_agent_id: String,
    pub operator_session_id: String,
    pub conversation_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub delivery_kind: String,
    pub reply_action: String,
    #[serde(default)]
    pub observed_events: Vec<String>,
    #[serde(default)]
    pub observed_partial_replies: Vec<String>,
    pub content: String,
}

/// A single entry in an agent's command manifest — describes one slash command the agent handles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandManifestEntry {
    /// The command name without the leading slash, e.g. `"status"`.
    pub command: String,
    /// One-line human-readable description shown in Telegram's command menu.
    pub description: String,
    /// Optional short usage string shown in /help, e.g. `"/role <name>"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_hint: Option<String>,
}

/// Shared cross-component task failure envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskErrorPayload {
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub capability: Option<String>,
    #[serde(default)]
    pub retryable: Option<bool>,
    /// Narrow error subtype for precise routing decisions.
    /// Values: "network_error", "streaming_timeout", "rate_limit",
    /// "provider_error", "content_error", "empty_response",
    /// "invalid_request", "provider_auth".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_kind: Option<String>,
    /// HTTP status code from the provider, when one could be determined
    /// (e.g. 400, 429, 503). Additive — absent from older controllers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Machine-readable escalation class so consumers don't string-parse
    /// `message`. Values: "retry_same_provider" (transient — the same
    /// provider may succeed on retry), "switch_provider" (the request will
    /// fail identically on the same provider — 4xx contract errors,
    /// refusals, rate limits), "fatal" (auth/key misconfiguration — retrying
    /// anywhere is pointless until an operator intervenes). Additive —
    /// absent from older controllers; consumers must keep a sub_kind/string
    /// fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReturnRoute {
    pub node: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl ReturnRoute {
    pub fn from_task(
        task: &serde_json::Value,
        default_node: impl Into<String>,
        default_role: impl Into<String>,
    ) -> Self {
        let default_node = default_node.into();
        let default_role = default_role.into();
        let object = task
            .get("return_route")
            .and_then(serde_json::Value::as_object);

        let read = |field: &str| -> Option<String> {
            object
                .and_then(|route| route.get(field))
                .and_then(serde_json::Value::as_str)
                .or_else(|| match field {
                    "node" => task.get("reply_to").and_then(serde_json::Value::as_str),
                    "role" => task.get("reply_role").and_then(serde_json::Value::as_str),
                    "session_id" => task.get("session_id").and_then(serde_json::Value::as_str),
                    "turn_id" => task.get("turn_id").and_then(serde_json::Value::as_str),
                    "correlation_id" => task
                        .get("correlation_id")
                        .and_then(serde_json::Value::as_str),
                    _ => None,
                })
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
        };

        let node = read("node").unwrap_or(default_node);
        let role = read("role").unwrap_or(default_role);
        let guest_id = object
            .and_then(|route| route.get("guest_id"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                object
                    .and_then(|route| route.get("guest"))
                    .and_then(serde_json::Value::as_str)
            })
            .or_else(|| {
                task.get("reply_guest_id")
                    .and_then(serde_json::Value::as_str)
            })
            .or_else(|| {
                (role == "agent")
                    .then(|| task.get("agent_id").and_then(serde_json::Value::as_str))
                    .flatten()
            })
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string);

        Self {
            node,
            role,
            guest_id,
            session_id: read("session_id"),
            turn_id: read("turn_id"),
            correlation_id: read("correlation_id"),
        }
    }

    pub fn as_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

impl TaskErrorPayload {
    pub fn provider_failure(
        component: impl Into<String>,
        capability: Option<&str>,
        provider: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: "provider_failure".into(),
            message: message.into(),
            code: None,
            component: Some(component.into()),
            provider: provider.map(str::to_string),
            capability: capability.map(str::to_string),
            retryable: None,
            sub_kind: None,
            status: None,
            error_class: None,
        }
    }

    /// A tool execution failure originating inside agent-core local dispatch.
    pub fn tool_execution(
        tool_name: impl Into<String>,
        message: impl Into<String>,
        code: Option<&str>,
    ) -> Self {
        let tool_name = tool_name.into();
        Self {
            kind: "tool_execution_failure".into(),
            message: message.into(),
            code: code.map(str::to_string),
            component: Some("philote".into()),
            capability: Some(tool_name),
            provider: None,
            retryable: Some(false),
            sub_kind: None,
            status: None,
            error_class: None,
        }
    }

    /// A failure returned by the hotel IPC layer (error code + message from hotel).
    pub fn ipc_failure(
        component: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: "ipc_failure".into(),
            message: message.into(),
            code: Some(code.into()),
            component: Some(component.into()),
            provider: None,
            capability: None,
            retryable: Some(true),
            sub_kind: None,
            status: None,
            error_class: None,
        }
    }

    /// A transport-level failure (socket error, serialization failure, etc.).
    pub fn transport_error(component: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: "transport_error".into(),
            message: message.into(),
            code: None,
            component: Some(component.into()),
            provider: None,
            capability: None,
            retryable: Some(true),
            sub_kind: None,
            status: None,
            error_class: None,
        }
    }

    pub fn display_message(&self) -> String {
        let mut parts = vec![self.message.clone(), format!("kind={}", self.kind)];
        if let Some(code) = self.code.as_deref() {
            parts.push(format!("code={code}"));
        }
        if let Some(component) = self.component.as_deref() {
            parts.push(format!("component={component}"));
        }
        if let Some(provider) = self.provider.as_deref() {
            parts.push(format!("provider={provider}"));
        }
        if let Some(capability) = self.capability.as_deref() {
            parts.push(format!("capability={capability}"));
        }
        if let Some(retryable) = self.retryable {
            parts.push(format!("retryable={retryable}"));
        }
        if let Some(status) = self.status {
            parts.push(format!("status={status}"));
        }
        if let Some(error_class) = self.error_class.as_deref() {
            parts.push(format!("error_class={error_class}"));
        }
        parts.join(" | ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HandoffBundle {
    pub goal: String,
    pub context_excerpt: String,
    pub session_id: String,
    pub initiating_turn_id: String,
    #[serde(default)]
    pub return_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_reason: Option<String>,
    /// The role handing off (e.g. "orchestrator", "developer"). None = orchestrator base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_role: Option<String>,
    /// The role receiving the handoff. Always set for same-identity role handoffs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_goal: Option<String>,
    #[serde(default)]
    pub active_constraints: Vec<String>,
    /// Session-local facts still live at handoff time. Owned by the workflow, not the operator.
    #[serde(default)]
    pub relevant_session_facts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_summary: Option<String>,
    #[serde(default)]
    pub suggested_memory_refs: Vec<String>,
    /// One of: "required" (target must hand back), "optional", "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_return_mode: Option<String>,
    #[serde(default)]
    pub cleanup_actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SubagentContextPacket {
    pub summary: String,
    #[serde(default)]
    pub session_facts: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub memory_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SubagentCompletionContract {
    #[serde(default)]
    pub summary_required: bool,
    #[serde(default)]
    pub artifact_refs_expected: bool,
    #[serde(default)]
    pub failure_summary_required: bool,
    #[serde(default)]
    pub requires_parent_ack: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    Progress,
    TurnStarted,
    ToolCall,
    TurnCompleted,
    ApprovalNeeded,
}

/// Where a hook event is routed when it fires.
/// The delegation skill owns this decision — infrastructure does not hardcode it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum HookRoute {
    /// Deliver to the persona agent that spawned this subagent (default).
    PersonaAgent,
    /// Deliver to any currently active role with this name on the mesh.
    Role { role_name: String },
    /// Do not deliver; fire locally for side-effects only (requires `handler_skill`).
    Discard,
}

impl Default for HookRoute {
    fn default() -> Self {
        Self::PersonaAgent
    }
}

/// A single hook subscription declared by the delegation skill.
/// If a hook is listed here it fires. If it is not listed it does not fire.
/// Every subscription must resolve to a valid handler — either a route that
/// can respond, or an explicit local `handler_skill` for Discard routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookSubscription {
    pub hook_kind: HookKind,
    #[serde(default)]
    pub route: HookRoute,
    /// Skill ID of the local handler to invoke, required when `route` is Discard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_skill: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleBehavior {
    Terminate,
    NotifyPersona,
    AutoRenew,
}

impl Default for IdleBehavior {
    fn default() -> Self {
        Self::Terminate
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentLeaseTerms {
    pub ttl_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewal_interval_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lifetime_seconds: Option<u64>,
    pub idle_behavior: IdleBehavior,
}

impl Default for SubagentLeaseTerms {
    fn default() -> Self {
        Self {
            ttl_seconds: 300,
            renewal_interval_seconds: None,
            max_lifetime_seconds: None,
            idle_behavior: IdleBehavior::Terminate,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SpawnSubagentDelta {
    pub requested_ttl: u64,
    pub confirmed_ttl: u64,
    pub requested_max_lifetime: Option<u64>,
    pub confirmed_max_lifetime: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SubagentDelegation {
    pub parent_agent_id: String,
    pub parent_role: String,
    pub subagent_kind: String,
    pub goal: String,
    pub context_packet: SubagentContextPacket,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub allowed_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_allowance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writeback_allowance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_budget: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
    #[serde(default)]
    pub completion_contract: SubagentCompletionContract,
    #[serde(default)]
    pub lease_terms: SubagentLeaseTerms,
    /// Declared hook subscriptions — only hooks listed here will fire.
    /// Each subscription owns its own routing decision.
    #[serde(default)]
    pub hook_subscriptions: Vec<HookSubscription>,
    /// Where to route the `subagent.complete` event. Defaults to PersonaAgent.
    #[serde(default)]
    pub completion_route: HookRoute,
    /// Where to route the `subagent.failed` event. Defaults to PersonaAgent.
    #[serde(default)]
    pub failure_route: HookRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseStatus {
    Active,
    Releasing,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseEnvelope {
    pub lease_type: String,
    pub lease_scope: String,
    pub authority_hotel: String,
    #[serde(default)]
    pub authority_component: Option<String>,
    pub owner_guest_id: String,
    #[serde(default)]
    pub owner_hotel: Option<String>,
    #[serde(default)]
    pub owner_component_type: Option<String>,
    pub lease_epoch: u64,
    pub lease_expires_at: u64,
    pub last_heartbeat_at: u64,
    pub status: LeaseStatus,
    #[serde(default)]
    pub delegated_from: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl LeaseEnvelope {
    pub fn is_active(&self) -> bool {
        matches!(self.status, LeaseStatus::Active)
    }
}

/// How the receiving philote's response should be handled when it arrives
/// back at the emitter. Declared at dispatch time by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ParacrineRouting {
    /// Feed brain's response back into the orchestrator's own paracrine layer as a
    /// new turn. The orchestrator (e.g. Astrid) reasons about the reply and explicitly
    /// calls `delegate.merge` to surface it to the user, or completes silently to
    /// absorb internally. This is the default — it keeps the human out of specialist
    /// sub-conversations until the orchestrator decides to include them.
    #[default]
    ReflectiveReEntry,
    /// Feed the response into a model cognitive re-entry. If an active turn
    /// exists, inject as enriched context; otherwise start a synthesis turn.
    /// Always surfaces a reply to Telegram — prefer ReflectiveReEntry unless you
    /// explicitly want the raw specialist response to go straight to the user.
    CognitiveReEntry,
    /// Replace the "paracrine dispatched" placeholder tool result with the real
    /// response and re-enter the model as if the tool call completed normally.
    EnrichedToolResult,
    /// Structured retrieval payload — inject into a named context slot on the
    /// session. No model invocation unless explicitly requested.
    DatasourceInjection,
    /// Memory recall result — push into the session's memory window.
    MemoryEnrichment,
    /// Mid-turn progress note — emit partial content to membrane without
    /// interrupting or closing the active turn.
    ProgressUpdate,
    /// Lightweight status ping. No model involvement; just ACK and update
    /// pending lookaside state.
    Heartbeat,
    /// Forward the response content directly to membrane. No model loop.
    RawForward,
    /// Arbiter-promoted re-entry: queue at the FRONT of pending_user_tasks so the
    /// orchestrator processes it next, ahead of any already-queued messages.
    PriorityReEntry,
    /// Operator approval decision for a parked turn. The receiving philote restores
    /// the parked turn and applies the resolution (approve or deny) without re-entering
    /// the model loop. Carries `decision` ("approved"/"denied") and optional `note`.
    ApprovalResolution,
}

/// Paracrine message envelope — the vesicle a philote secretes when performing a
/// paracrine dispatch. Carries the prompt and optional context to the receiving
/// philote, which endocytoses it as a `paracrine_request` inbound task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exosome {
    /// The prompt or model-request content for the specialist.
    pub prompt: String,
    /// Optional structured context (e.g. session excerpt, tool results).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    /// Runtime-assigned correlation ID. Ties the `paracrine_response` back to
    /// the originating turn and threads through the full thought graph for
    /// cross-mesh provenance. Always set by the emitting philote; never dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paracrine_id: Option<String>,
    /// How the receiving philote's response should be handled when it arrives
    /// back at the emitter. Declared at dispatch time by the caller.
    /// Defaults to [`ParacrineRouting::ReflectiveReEntry`] if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_routing: Option<ParacrineRouting>,
    /// The session_id of the conversation that triggered this paracrine.
    /// Carried through so the specialist's response can be routed back to the
    /// correct session (e.g. a Telegram session rather than an ephemeral one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    /// The chat_id (Telegram / membrane channel) of the originating conversation.
    /// Used by the routing reflex to deliver the specialist's reply to the right channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_chat_id: Option<String>,
}

fn default_training_limit() -> usize {
    20
}

fn default_true() -> bool {
    true
}

/// Who requested a [`IpcRequest::RestartComponent`], which decides whether the
/// hotel applies flap protection.
///
/// Operator-initiated restarts (desktop UI / CLI) are deliberate and MUST NOT be
/// budget-limited. Heal-dispatcher-initiated restarts are automatic remediation
/// and MUST go through the shared respawn budget so a crash-looping guest that
/// keeps emitting a matching stderr line cannot be restarted every dispatch
/// cycle forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartReason {
    /// Deliberate operator/CLI restart — never budget-limited.
    #[default]
    Operator,
    /// Automatic heal-dispatcher remediation — subject to the respawn budget.
    Heal,
}

/// Represents the types of operations a Guest can perform locally over IPC to the Ansible Hotel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", content = "payload")]
#[serde(rename_all = "snake_case")]
pub enum IpcRequest {
    /// Connect and register as an active materialized guest
    Register(GuestIdentity),
    /// Ask the Hotel for configuration data from the local Context Graph
    GetConfig {
        key: String,
    },
    /// Ask the Hotel vault for a decrypted secret value by secret ref
    GetSecret {
        secret_ref: String,
    },
    /// Section 6 Blueprint Operations
    PublishMessage {
        target_role: String,
        payload: serde_json::Value,
    },
    CreateTask {
        target_role: String,
        payload: serde_json::Value,
    },
    AckEvent {
        event_id: Uuid,
    },
    UpdateTask {
        task_id: Uuid,
        state: String,
        payload: serde_json::Value,
    },
    CompleteTask {
        task_id: Uuid,
        result: serde_json::Value,
    },
    FailTask {
        task_id: Uuid,
        error_code: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    RepairStaleSessionTurns {
        min_age_secs: u64,
    },
    /// Self-heal detector for the role-handoff ping-pong loop: finds any agent
    /// with more than one incarnation at `ActiveInSession`, demotes them, and
    /// clears any session pin pointing at a demoted incarnation.
    HealRoleHandoffLoops {},
    SubscribeInbox {
        role: String,
    },
    AcquireTelegramPollLease {
        lease_key: String,
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resource_ref: Option<String>,
    },
    AcquireDesktopMembraneLease {
        lease_key: String,
        port: u16,
    },
    GetTelegramPollLeaseOwner {
        lease_key: String,
    },
    GetDesktopMembraneLeaseOwner {
        lease_key: String,
    },
    GetDesktopMembraneStatus,
    GetDesktopMembraneTargetStatus {
        target_node_id: String,
    },
    QueryOperatorTargets,
    QueryOperatorTargetStatus {
        target_node_id: String,
    },
    QueryOperatorTargetGuests {
        target_node_id: String,
    },
    QueryOperatorTargetAgents {
        target_node_id: String,
    },
    QueryOperatorTargetComponents {
        target_node_id: String,
    },
    QueryOperatorTargetConfig {
        target_node_id: String,
    },
    QueryOperatorTargetSecrets {
        target_node_id: String,
    },
    QueryOperatorTargetPlacement {
        target_node_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        #[serde(default)]
        required_markers: Vec<String>,
        #[serde(default)]
        prefer_locality: bool,
    },
    RegisterOperatorTargetComponent {
        target_node_id: String,
        manifest: ComponentManifest,
    },
    SetOperatorTargetConfig {
        target_node_id: String,
        key: String,
        value_json: String,
    },
    RotateOperatorTargetSecret {
        target_node_id: String,
        secret_ref: String,
        plaintext: String,
    },
    AddOperatorTargetVaultEntry {
        target_node_id: String,
        vault_name: String,
        plaintext: String,
        #[serde(default)]
        allowed_roles: Vec<String>,
    },
    SetOperatorTargetRoleHome {
        target_node_id: String,
        agent_id: String,
        role_name: String,
        calling_role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_hotel: Option<String>,
    },
    SetTransportHome {
        agent_id: String,
        transport: String,
        resource_ref: String,
        calling_role: String,
        target_hotel: String,
        #[serde(default)]
        standby_hotels: Vec<String>,
    },
    SetOperatorTargetComponentActive {
        target_node_id: String,
        guest_id: String,
        active: bool,
    },
    RestartOperatorTargetComponent {
        target_node_id: String,
        guest_id: String,
    },
    RemoveOperatorTargetComponent {
        target_node_id: String,
        guest_id: String,
    },
    SendOperatorChatTurn {
        target_node_id: String,
        target_agent_id: String,
        operator_session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation_id: Option<String>,
        content: String,
    },
    /// List conversation sessions recorded in this hotel's context graph
    /// (operator session history), most recent activity first.
    /// Responds with [`IpcResponse::OperatorSessionList`].
    ListOperatorSessions {
        /// When set, only sessions whose primary agent matches are returned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_agent_id: Option<String>,
        /// Maximum number of sessions to return (default 50, capped at 500).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// List the turns of one session as operator/agent messages, oldest first.
    /// Responds with [`IpcResponse::SessionTurnList`].
    ListSessionTurns {
        session_id: String,
        /// Maximum number of underlying turn records to expand (default 50, capped at 500).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        /// Pagination cursor: only turns strictly older than this turn_id are
        /// returned. An unknown cursor yields an empty page (end of history).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_turn_id: Option<String>,
    },
    /// Read-only roster of the local node plus every fresh mesh peer known to
    /// the node registry, including reachable listener endpoints and exposure
    /// profile where advertised. Responds with [`IpcResponse::MeshRosterView`].
    GetMeshRoster,
    ListDesktopMembraneGuests,
    ListDesktopMembraneTargetGuests {
        target_node_id: String,
    },
    ListDesktopMembraneAgents,
    ListDesktopMembraneTargetComponents {
        target_node_id: String,
    },
    ListDesktopMembraneTargets,
    RenewTelegramPollLease {
        lease_key: String,
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resource_ref: Option<String>,
        lease_epoch: u64,
    },
    RenewDesktopMembraneLease {
        lease_key: String,
        lease_epoch: u64,
    },
    ReleaseTelegramPollLease {
        lease_key: String,
    },
    ReleaseDesktopMembraneLease {
        lease_key: String,
    },
    AcquireDiscordGatewayLease {
        lease_key: String,
        agent_id: String,
    },
    GetDiscordGatewayLeaseOwner {
        lease_key: String,
    },
    RenewDiscordGatewayLease {
        lease_key: String,
        agent_id: String,
        lease_epoch: u64,
    },
    ReleaseDiscordGatewayLease {
        lease_key: String,
    },
    HandoffToRole {
        session_id: String,
        role_name: String,
        handoff_bundle: HandoffBundle,
    },
    /// Pin or unpin a role to a specific home hotel.
    /// `target_hotel: None` clears the pin (role runs on authority hotel).
    SetRoleHome {
        agent_id: String,
        role_name: String,
        /// The calling role — authority check (orchestrator or admin only).
        calling_role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_hotel: Option<String>,
    },
    HandoffBack {
        session_id: String,
        summary: String,
        #[serde(default)]
        return_to: Option<String>,
    },
    DelegateToPeer {
        target_agent_id: String,
        task_description: String,
        context_package: String,
        chat_id: String,
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        expected_artifacts: Vec<String>,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
    DelegateToExternalPeer {
        target_peer_type: String,
        task_description: String,
        context_package: String,
        #[serde(default)]
        expected_artifacts: Vec<String>,
    },
    SpawnSubagent {
        session_id: String,
        delegation: SubagentDelegation,
    },
    AssignSubagentTask {
        subagent_guest_id: String,
        lease_epoch: u64,
        delegation: SubagentDelegation,
    },
    RenewSubagentLease {
        subagent_guest_id: String,
        lease_epoch: u64,
    },
    ReleaseSubagent {
        subagent_guest_id: String,
    },
    FireSubagentHook {
        subagent_guest_id: String,
        hook_kind: HookKind,
        payload: serde_json::Value,
    },
    AcceptSubagentLease {
        subagent_guest_id: String,
    },
    AbortSubagentSpawn {
        subagent_guest_id: String,
    },
    /// Register a delegation skill with the hotel.
    ///
    /// The hotel validates the skill definition via Layer 1 validation and writes
    /// it to the context graph as an `abstract_skill` node. Returns
    /// [`IpcResponse::SkillRegistered`] on success (even if validation fails —
    /// the registration always writes; the state reflects the validation outcome).
    RegisterSkill {
        skill_name: String,
        description: String,
        /// The subagent worker kind (e.g. `"philote-worker"`).
        subagent_kind: String,
        /// High-level goal statement for this skill.
        goal: String,
        #[serde(default)]
        allowed_tools: Vec<String>,
        #[serde(default)]
        allowed_classes: Vec<String>,
        #[serde(default)]
        hook_subscriptions: Vec<HookSubscription>,
        #[serde(default)]
        completion_route: HookRoute,
        #[serde(default)]
        failure_route: HookRoute,
        #[serde(default)]
        idle_behavior: IdleBehavior,
        #[serde(default)]
        lease_terms: SubagentLeaseTerms,
    },
    /// Assign a registered skill to a role's toolset profile.
    /// Requires orchestrator identity. Skill must exist in catalog.
    AssignSkill {
        agent_id: String,
        role_name: String,
        skill_name: String,
    },
    /// Remove a skill from a role's toolset profile.
    /// Requires orchestrator identity.
    RevokeSkill {
        agent_id: String,
        role_name: String,
        skill_name: String,
    },
    /// Patch mutable fields on an agent's identity bundle.
    /// Requires management identity. Only supplied fields are changed.
    PatchAgentBundle {
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        persona_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        soul_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_context_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        import_workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_toolset: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_skillset: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_route_policy: Option<ResponseRoutePolicyView>,
    },
    /// Get the hotel-scoped user profile.
    GetUserProfile {
        hotel_name: String,
    },
    /// Patch the hotel-scoped user profile. Only provided fields are updated.
    PatchUserProfile {
        hotel_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
    },
    /// List all registered skills with their validation states.
    ListSkills {},
    /// Get a single toolset profile by name.
    GetToolsetProfile {
        profile_name: String,
    },
    /// List all toolset profiles registered in the context graph.
    ListToolsetProfiles {},
    ListRoleIncarnations {
        agent_id: String,
    },
    QueryStatus {
        task_id: Uuid,
    },
    QueryTimeline {
        task_id: Uuid,
    },
    /// Drop a task onto the Philotic Web (Legacy)
    EmitTask {
        target_node: String,
        target_role: String,
        #[serde(default)]
        target_guest_id: Option<String>,
        task_json: String,
    },
    /// Ask the hotel to initiate a WebRTC session offer toward a remote node.
    StartWebRtcSession {
        target_node_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_guest_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// Ask the hotel for the current status of a locally-tracked WebRTC session.
    GetWebRtcSessionStatus {
        session_id: String,
    },
    /// Optimistically push a RAM-based memory apartment update to the Hotel's SQLite Graph
    SyncApartment {
        agent_id: String,
        memory_type: String,
        content_json: serde_json::Value,
    },
    /// Create or update a role incarnation definition (orchestrator only)
    ConfigureRole {
        agent_id: String,
        role_name: String,
        guest_id: String,
        /// The active persona role of the calling agent (e.g. "orchestrator").
        /// Distinct from the IPC process role ("agent") — this is the session-level
        /// persona role stored in the context graph, checked server-side for authority.
        calling_role: String,
        toolset_profile: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role_identity_addendum: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role_manifest: Option<String>,
        #[serde(default)]
        is_admin: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inactive_ttl_seconds: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        iteration_cap: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval_policy: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_profile: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_window_policy: Option<String>,
        /// Ordered model-role fallback ladder for this role incarnation.
        /// `None` (the default when omitted) PRESERVES whatever ladder is
        /// already on the record — every existing IPC caller that predates
        /// this field keeps its DB-edited ladder intact instead of it being
        /// silently wiped to empty on every reconfigure. `Some(tiers)` sets
        /// the ladder explicitly (each tier must be a non-empty string). A
        /// brand-new role with `None` gets `DEFAULT_FALLBACK_TIERS`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback_tiers: Option<Vec<String>>,
        /// Per-agent model NAME binding (Layer 1), keyed by provider role
        /// (a `fallback_tiers` entry, e.g. `"model.openrouter"`) mapping to
        /// the model id to request from that provider. `None` (the default
        /// when omitted) PRESERVES whatever bindings are already on the
        /// record — same preserve-on-None contract as `fallback_tiers`
        /// above. `Some(map)` sets them explicitly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_bindings: Option<std::collections::BTreeMap<String, String>>,
        /// Content-filtering posture for this role: `"unrestricted"` | `"standard"`
        /// | `"strict"`. `None` (the default when omitted) PRESERVES whatever
        /// policy is already on the record — same preserve-on-None contract as
        /// `fallback_tiers` above, so an unrelated reconfigure never silently
        /// resets an operator-set `"unrestricted"` policy back to `"standard"`.
        /// A brand-new role with `None` gets `"standard"` (current behavior).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_policy: Option<String>,
    },
    /// Execute a governed workflow through the hotel's workflow plane.
    ExecuteWorkflow {
        workflow_name: String,
        agent_id: String,
        /// The active persona role of the calling agent (e.g. "orchestrator").
        calling_role: String,
        arguments: serde_json::Value,
    },
    /// Ask the local hotel authority to create a signed mesh invite.
    CreateMeshInvite {
        hotel_name: String,
        mesh_host: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_secs: Option<u64>,
    },
    /// Ask the local hotel authority to verify an invite, sign a join request, and dispatch it.
    AcceptMeshInvite {
        hotel_name: String,
        mesh_host: String,
        invite_json: String,
    },
    /// Write a config value to the hotel's context graph (operator/management only).
    SetConfig {
        key: String,
        value_json: String,
    },
    /// Re-encrypt an existing vault secret in place with new plaintext.
    /// The secret_ref, scope, and ACL are preserved; only the ciphertext changes.
    RotateSecret {
        secret_ref: String,
        plaintext: String,
    },
    /// Store a new vault secret and append it to the vault_registry config key.
    AddVaultEntry {
        vault_name: String,
        plaintext: String,
        #[serde(default)]
        allowed_roles: Vec<String>,
    },
    /// Request the hotel's loaded MuninnDB configuration (vault tokens included).
    FetchMemoryConfig,
    /// Force the hotel to re-probe MuninnDB reachability immediately and broadcast the result.
    /// Responds with [`IpcResponse::MuninnStatus`].
    RefreshMemoryConfig,
    /// MuninnDB rejected the stored bearer token for `vault` (HTTP 401 while
    /// reachable): ask the hotel to re-mint the token from the durable
    /// Context-Graph truth — admin mint via the MuninnDB admin API, then
    /// `rotate_secret` on the registry `secret_ref` in place. Budgeted
    /// per-vault on the hotel side; escalates instead of minting when no
    /// admin credential is available. Responds with
    /// [`IpcResponse::MemoryConfig`] carrying the refreshed config on
    /// success, or [`IpcResponse::error`] on refusal/failure.
    HealMemoryToken {
        vault: String,
    },
    /// Register a graph instance with the hotel's ODS so it can route graph_id → instance_id.
    /// Historically sent by the retired graph-runner guest on startup (for all existing
    /// graphs) and after each graph.create; the hotel-side registry handler remains live.
    RegisterGraphInstance {
        graph_id: String,
        instance_id: String,
    },
    /// Propose a new durable rule for the agent.
    ///
    /// The hotel stores the rule in the context graph and requires operator confirmation
    /// before the rule takes effect. Responds with [`IpcResponse::RuleProposed`].
    ProposeRule {
        agent_id: String,
        description: String,
        rationale: String,
    },
    RecordRoutingPolicyProposal {
        agent_id: String,
        problem: String,
        proposed_change: String,
        evidence: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        affected_stage: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        affected_capability: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        learned_reflex_preference_key: Option<String>,
    },
    ListRoutingPolicies {
        agent_id: String,
    },
    /// List all durable rules owned by the given agent.
    ///
    /// Responds with [`IpcResponse::RuleList`].
    ListRules {
        agent_id: String,
    },
    /// Upsert one learned reflex preference into the agent graph.
    ///
    /// Used by approved routing/reflex refinement flows to write durable
    /// adaptive posture into agent-owned state without turning rule records
    /// into stealth policy storage.
    UpsertAgentReflexPreference {
        agent_id: String,
        preference_key: String,
        precedence: i32,
        reflexes_json: serde_json::Value,
        config_json: serde_json::Value,
    },
    GetAgentReflexPreferences {
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preference_key: Option<String>,
    },
    /// Declare or replace a routing pipeline rule for this agent.
    /// The `rule_id` is the stable key; set with the same rule_id to update.
    UpsertRoutingPipelineRule {
        agent_id: String,
        rule_id: String,
        rule_json: serde_json::Value,
    },
    /// Remove a routing pipeline rule by rule_id.
    RemoveRoutingPipelineRule {
        agent_id: String,
        rule_id: String,
    },
    /// Retrieve routing pipeline rules for this agent.
    GetRoutingPipelineRules {
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule_id: Option<String>,
    },
    /// Record one successful same-self role handoff observation and let the hotel
    /// fold it into agent-owned reflex posture without making philote read/modify/write
    /// the agent graph directly.
    RecordRoleHandoffReflexEvidence {
        agent_id: String,
        role_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        legacy_trigger_class: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_turn: Option<String>,
    },
    AppendRoutingPolicyEvaluation {
        proposal_id: String,
        evaluation_kind: String,
        decision: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_tool: Option<String>,
    },
    SetRoutingPolicyDisposition {
        proposal_id: String,
        state: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_tool: Option<String>,
    },
    /// Agent declares a need for a resource; hotel responds with
    /// [`IpcResponse::ResourceGranted`], [`IpcResponse::ResourceDenied`], or
    /// [`IpcResponse::ResourceMaterializing`].
    ResourceRequest(ResourceRequest),
    /// Agent notifies the hotel that it no longer needs a resource instance.
    ///
    /// The hotel decrements the tenant count; zero-tenant instances may be
    /// torn down per their teardown policy.
    ResourceReleased(ResourceReleased),
    /// Register (or update) a component with the hotel.
    ///
    /// The hotel upserts a `GuestRecord` into `materialized_guests`, stores the
    /// `component_config` blob under `node_config["component:{guest_id}"]`, and
    /// materializes (spawns) the guest if `auto_start` is true.
    ///
    /// Responds with [`IpcResponse::ComponentRegistered`].
    RegisterComponent {
        manifest: ComponentManifest,
    },
    /// List all registered graph runner instances.
    ///
    /// Responds with [`IpcResponse::GraphInstanceList`].
    ListGraphInstances {},
    /// List all registered components (guests that were registered via RegisterComponent).
    ///
    /// Returns every guest record enriched with its component config and tool-runner
    /// capabilities where available.
    ///
    /// Responds with [`IpcResponse::ComponentInventory`].
    ListComponents {},
    /// Enable or disable a registered component.
    ///
    /// When `active` is `true` the hotel sets `is_active=true` in the context graph and
    /// triggers immediate materialization (spawns the process if not already running).
    /// When `active` is `false` the hotel sets `is_active=false` and sends SIGTERM to
    /// the running process if one is present.
    ///
    /// Responds with [`IpcResponse::Standard`].
    SetComponentActive {
        guest_id: String,
        active: bool,
    },
    /// Restart a registered component: terminate the running process (if any) then
    /// immediately re-spawn it (requires `is_active=true` in the context graph).
    ///
    /// `reason` decides whether flap protection applies: [`RestartReason::Heal`]
    /// (automatic remediation) is routed through the shared respawn budget, while
    /// [`RestartReason::Operator`] (the default) is never budget-limited. The field
    /// defaults on the wire so an older heal-dispatcher talking to a newer hotel is
    /// treated as an operator restart (fails open — no accidental budget denial).
    ///
    /// Responds with [`IpcResponse::Standard`].
    RestartComponent {
        guest_id: String,
        #[serde(default)]
        reason: RestartReason,
    },
    /// Remove a registered component entirely.
    ///
    /// The hotel terminates the running process if present, deletes the guest record,
    /// and removes the stored `component:{guest_id}` config blob.
    ///
    /// Responds with [`IpcResponse::Standard`].
    RemoveComponent {
        guest_id: String,
    },
    /// Inject a remote node incarnation into the local node registry.
    ///
    /// Used in smoke / integration tests to simulate mesh discovery without
    /// requiring live UDP beaconing between hotels.  The injected advertisement
    /// survives for the registry's normal TTL (15 s) and is subject to the same
    /// staleness eviction as beacon-sourced entries.
    SeedRemoteIncarnation {
        node_id: String,
        hotel_id: String,
        incarnation_id: String,
        target_role: String,
        /// Optional UDS socket path for this node. When provided the hotel
        /// will forward tasks addressed to this node directly via the socket
        /// (smoke-test cross-hotel delivery without full mesh).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        socket_path: Option<String>,
    },
    // ── Cron scheduler IPC ───────────────────────────────────────────────────
    /// Register (or update) a cron job with the hotel.
    ///
    /// The hotel stores the job in the Context Graph and the `CronTicker`
    /// will begin firing it on schedule.  Responds with
    /// [`IpcResponse::Standard`] (ok=true) on success.
    RegisterCronJob {
        job: CronJob,
    },
    /// Remove a cron job by id. No-op if not present.
    RemoveCronJob {
        job_id: CronJobId,
    },
    /// List all cron jobs registered with this hotel.
    ///
    /// Responds with [`IpcResponse::CronJobList`].
    ListCronJobs,
    /// Enable a previously disabled cron job.
    EnableCronJob {
        job_id: CronJobId,
    },
    /// Disable a cron job without removing it.
    DisableCronJob {
        job_id: CronJobId,
    },
    // ── MCP membrane IPC ──────────────────────────────────────────────────────
    /// Acquire the singleton MCP membrane lease for a given port.
    ///
    /// Responds with [`IpcResponse::McpMembraneLease`].
    AcquireMcpMembraneLease {
        lease_key: String,
        port: u16,
    },
    /// Renew an active MCP membrane lease.
    RenewMcpMembraneLease {
        lease_key: String,
        lease_epoch: u64,
    },
    /// Release the MCP membrane lease.
    ReleaseMcpMembraneLease {
        lease_key: String,
    },
    /// Push an updated route set for one agent to the membrane.
    ///
    /// The membrane replaces all routes owned by `agent_id` with `routes` (LWW).
    /// The hotel persists the route set and replays it to membrane-mcp on restart.
    /// Responds with [`IpcResponse::McpRoutesAccepted`].
    UpdateMcpRoutes {
        agent_id: String,
        routes: Vec<ansible_mesh_core::mcp_route::McpRouteRecord>,
        /// Optional vault ref for the bearer token protecting these routes.
        /// Stored alongside routes so provisioning survives hotel restarts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vault_ref: Option<String>,
    },
    /// Remove all routes owned by an agent from the membrane.
    RevokeMcpRoutes {
        agent_id: String,
    },
    /// Fetch all persisted MCP route sets from the hotel's context graph.
    ///
    /// Returns routes that survived hotel restarts. Membrane-mcp calls this
    /// during `setup()` to replay provisioned routes without re-provisioning.
    /// Responds with [`IpcResponse::McpRouteState`].
    GetMcpRoutes {},
    /// Declare or update a full MCP endpoint configuration.
    ///
    /// The hotel stores the config, fans out an `update_mcp_config` push to
    /// the relevant membrane-mcp guest, and records pre-approval rules.
    /// Responds with [`IpcResponse::McpEndpointProvisioned`].
    ProvisionMcpEndpoint {
        config: ansible_mesh_core::mcp_endpoint::McpEndpointConfig,
    },
    /// Tear down an MCP endpoint and remove its config.
    ///
    /// The hotel fans out a `revoke_mcp_config` push and clears stored state.
    /// Only the `owner_agent_id` that provisioned the endpoint may revoke it.
    RevokeMcpEndpoint {
        endpoint_id: String,
        owner_agent_id: String,
    },
    /// Return the stored `McpEndpointConfig` for the given endpoint, plus the
    /// hotel's current perimeter ceiling. Responds with [`IpcResponse::Standard`].
    GetMcpEndpointStatus {
        endpoint_id: String,
    },
    /// Mint (or rotate) a bearer-token grant for an MCP endpoint.
    ///
    /// The hotel generates the token, stores `BLAKE3(token)` in the vault under
    /// an `mcp_endpoint_token` secret readable by the `mcp-membrane` role,
    /// attaches the grant to the named tool's auth (or the endpoint's
    /// `default_auth` when `tool_name` is absent), and fans the updated config
    /// out to the membrane guest. Responds with [`IpcResponse::Standard`]; the
    /// raw token appears ONCE in `data.raw_token` and is never stored.
    ///
    /// Only the endpoint's `owner_agent_id` may call this; the hotel verifies
    /// the claim against the registered guest identity on the connection.
    ProvisionMcpTokenGrant {
        endpoint_id: String,
        owner_agent_id: String,
        /// Stable opaque label for this credential (e.g. `"claude-desktop"`).
        token_id: String,
        /// Attach to this tool's auth; absent = endpoint `default_auth`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        scopes: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allotment: Option<ansible_mesh_core::mcp_route::McpCallAllotment>,
        /// Rotate an existing grant in place (same `token_id`, same vault ref,
        /// new credential) instead of creating a new grant.
        #[serde(default)]
        rotate: bool,
    },
    /// Remove a bearer-token grant from an MCP endpoint by `token_id`.
    ///
    /// Removes the grant from every tool auth and the endpoint `default_auth`.
    /// An emptied `BearerToken` grant list stays `BearerToken` (nobody can
    /// call) rather than degrading to `None` (loopback-open).
    /// Responds with [`IpcResponse::Standard`].
    RevokeMcpTokenGrant {
        endpoint_id: String,
        owner_agent_id: String,
        token_id: String,
    },
    // ── MCP upstream (client fabric) IPC ─────────────────────────────────────
    /// Register or update an upstream MCP server this hotel consumes.
    ///
    /// The hotel checks the egress policy, persists the config under the
    /// `__mcp_upstreams__` registry, fans out an `update_mcp_upstream` push to
    /// the `mcp-client` guest, and materializes that guest if needed.
    /// Responds with [`IpcResponse::McpUpstreamRegistered`].
    RegisterMcpUpstream {
        config: ansible_mesh_core::mcp_upstream::McpUpstreamConfig,
    },
    /// Remove an upstream MCP server registration.
    ///
    /// Only the `owner_agent_id` that registered the upstream may revoke it.
    /// The hotel clears stored state and fans out a `revoke_mcp_upstream` push.
    /// Responds with [`IpcResponse::McpUpstreamRegistered`].
    RevokeMcpUpstream {
        upstream_id: String,
        owner_agent_id: String,
    },
    /// Return all registered upstreams with their last reported catalogs.
    /// Responds with [`IpcResponse::McpUpstreamsState`].
    GetMcpUpstreams {},
    /// Guest → hotel: report an upstream's connection state and projected
    /// tool catalog after connect/refresh. Responds with [`IpcResponse::Standard`].
    ReportMcpUpstreamCatalog {
        catalog: ansible_mesh_core::mcp_upstream::McpUpstreamCatalog,
    },
    /// Store (or rotate) the outbound credential for an upstream MCP server.
    ///
    /// The plaintext credential passes through to the hotel vault under
    /// secret-kind `mcp_upstream_credential` (readable by the
    /// `mcp-client-runner` role only) and is never persisted in the graph or
    /// echoed back. The upstream's `credential_ref` is set and the updated
    /// config fans out so the guest reconnects authenticated. Only the
    /// upstream's `owner_agent_id` may call this; the claim is verified
    /// against the registered guest identity. Responds with
    /// [`IpcResponse::Standard`].
    ProvisionMcpUpstreamCredential {
        upstream_id: String,
        owner_agent_id: String,
        credential: String,
    },
    // ── Training data admin IPC ───────────────────────────────────────────────
    /// List voice training samples. Responds with [`IpcResponse::Standard`] (data.samples).
    ListTrainingSamples {
        #[serde(default)]
        agent_id: Option<String>,
        #[serde(default = "default_training_limit")]
        limit: usize,
        #[serde(default)]
        filter: ansible_mesh_core::whisper_training::TrainingFilter,
    },
    /// Apply an operator correction to a training sample. Responds with [`IpcResponse::Standard`].
    CorrectTrainingSample {
        turn_id: String,
        corrected_transcript: String,
    },
    /// Export eligible samples to a file. Responds with [`IpcResponse::Standard`] (data.exported_count).
    ExportTrainingSamples {
        format: ansible_mesh_core::whisper_training::TrainingExportFormat,
        output_path: String,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Return aggregate counts by state. Responds with [`IpcResponse::Standard`] (data.status).
    GetTrainingStatus {
        #[serde(default)]
        agent_id: Option<String>,
    },
    // ── ASR provider lifecycle ────────────────────────────────────────────────
    /// Set up the Parakeet ASR provider: verify/install nemo-toolkit, write
    /// component config, and register the guest for materialization.
    /// Responds with [`IpcResponse::Standard`] (data.message).
    AsrSetup {
        /// Python interpreter path (default: "python3").
        #[serde(default)]
        python_path: Option<String>,
        /// NeMo model name (default: nvidia/parakeet-tdt-0.6b-v2).
        #[serde(default)]
        model_name: Option<String>,
        /// If true, attempt `pip install nemo-toolkit[asr]` when the import check fails.
        #[serde(default = "default_true")]
        auto_install: bool,
    },
    /// Return the current status of the Parakeet ASR provider (guest active, nemo available).
    /// Responds with [`IpcResponse::Standard`] (data.status).
    AsrStatus {},
    // ── Vision provider lifecycle ─────────────────────────────────────────────
    /// Set up the vision inference provider: verify/install transformers+PIL, write
    /// component config, register the model-controller-vision guest, and upsert a
    /// ModelProfileRecord so health-aware routing picks it up.
    /// Responds with [`IpcResponse::Standard`] (data.message).
    VisionSetup {
        /// Florence-2 ONNX repo ID (default: "onnx-community/Florence-2-base-ft").
        #[serde(default)]
        repo_id: Option<String>,
    },
    /// Return the current status of the vision provider (guest active, backend type).
    /// Responds with [`IpcResponse::Standard`] (data.status).
    VisionStatus {},
    /// Invoke a capability using the normalized input envelope.
    /// Routes to the healthiest model-controller that declares the task_kind.
    /// Responds with a stream of [`CapabilityEvent`] frames terminated by Done or Error.
    /// (Slice 7: hotel streaming router — currently returns NOT_IMPLEMENTED)
    CapabilityInvoke {
        request: ansible_mesh_core::capability::CapabilityRequest,
    },
    /// Hotel-to-guest graceful shutdown signal. Guests do not send this to the hotel;
    /// the no-op handler in ipc.rs covers the case where one arrives unexpectedly.
    GracefulShutdown {
        drain_timeout_secs: u64,
    },
    /// Fire-and-forget paracrine dispatch from one agent to a specialist role.
    ParacrineEmit {
        /// Target role name (e.g. "theoretician").
        role: String,
        /// The message envelope to deliver.
        exosome: Exosome,
        /// Node to route the specialist's response to.
        reply_to_node: String,
        /// Role at that node ("membrane", "agent", etc.).
        reply_to_role: String,
        /// Specific guest_id to target for the reply (e.g. "default:membrane-gateway-astrid").
        /// When set, the specialist's final send_reply/partial_reply is routed to this exact
        /// guest rather than fanning out to all subscribers of reply_to_role.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_guest_id: Option<String>,
        /// Materialisation timeout for the target role. `None` uses the hotel default.
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
    /// Return a safe view of hotel state: hotel name, active guests, agent identities.
    /// No secret or credential values are included.
    GetHotelStatus,
    /// Return the hotel's current network security perimeter snapshot.
    GetPerimeterStatus,
    /// Force the hotel's PerimeterService to re-derive the snapshot from live interfaces.
    RefreshPerimeter,
    /// Ask the hotel's EgressGateway whether an outbound request is permitted and
    /// inject vault-backed credentials for the target host if applicable.
    /// Responds with [`IpcResponse::EgressGrant`].
    CheckEgress {
        /// Calling agent's ID (used for vault access decisions).
        agent_id: String,
        /// Full target URL (e.g. "https://api.perplexity.ai/chat/completions").
        target_url: String,
        /// HTTP method (e.g. "POST").
        method: String,
    },
    /// Ask the hotel to recommend the best execution placement for a role or tool need.
    BestPlaceToRun {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        #[serde(default)]
        required_markers: Vec<String>,
        #[serde(default)]
        prefer_locality: bool,
    },
    /// Return the last `lines` lines from the hotel's log file.
    GetHotelLogs {
        lines: u32,
    },
    /// Query aggregated routing performance stats from the router trace store.
    /// `window_secs`: if Some, only include traces from the last N seconds; None = all time.
    GetRouterStats {
        window_secs: Option<u64>,
    },
    // ── User Task Engine IPC ──────────────────────────────────────────────────
    /// Create a new durable user task in the hotel context graph.
    /// Responds with [`IpcResponse::UserTaskCreated`].
    CreateUserTask {
        task_id: String,
        session_id: String,
        agent_id: String,
        chat_id: String,
        goal: String,
        approved_risk_ceiling: String,
        planning_model_tier: u8,
        #[serde(default)]
        quiet: bool,
    },
    /// Update the top-level status and optional plan fields of a user task.
    /// Responds with [`IpcResponse::UserTaskUpdated`].
    UpdateUserTask {
        task_id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        steps_json: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_step_idx: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval_note: Option<String>,
    },
    /// Update a single step within a user task (status, output, error).
    /// Responds with [`IpcResponse::UserTaskUpdated`].
    UpdateUserTaskStep {
        task_id: String,
        step_idx: usize,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Retrieve a single user task by ID.
    /// Responds with [`IpcResponse::UserTaskData`].
    GetUserTask {
        task_id: String,
    },
    /// List user tasks, optionally filtered by session_id and/or agent_id.
    /// Responds with [`IpcResponse::UserTaskList`].
    ListUserTasks {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
    },
    /// Push a heal_queue entry from any guest (e.g. model-router on call failure).
    /// Responds with [`IpcResponse::HealEntryPushed`].
    PushHealEntry {
        guest_id: String,
        raw_text: String,
    },
    /// Retrieve pending (unresolved) heal_queue rows for the dispatcher.
    /// Responds with [`IpcResponse::HealQueuePending`].
    GetHealQueuePending {
        #[serde(default = "default_heal_queue_limit")]
        limit: usize,
    },
    /// Write triage decision back onto a heal_queue row.
    /// Responds with [`IpcResponse::Standard`] (ok=true).
    TriageHealEntry {
        id: String,
        severity: String,
        pattern_tag: String,
        heal_action: String,
    },
    /// Mark a heal_queue row resolved with the observed outcome.
    /// Responds with [`IpcResponse::Standard`] (ok=true).
    ResolveHealEntry {
        id: String,
        outcome: String,
    },
    // ── Agent migration IPC ───────────────────────────────────────────────────
    /// Initiate a full agent migration to a remote hotel.
    ///
    /// The source hotel bundles the agent's identity, apartments, role incarnations,
    /// vault entries (decrypted), guests, and agent-specific config, uploads the bundle
    /// to its blob store, then dispatches it to the destination hotel via the
    /// OperatorSurface cross-hotel relay.  The source stays active throughout.
    ///
    /// Responds with [`IpcResponse::Standard`].
    AgentMigrateToHotel {
        /// The agent making the request.
        agent_id: String,
        /// The hotel the agent wants to move to (e.g. "vps-jane").
        dest_hotel: String,
    },
    /// Apply a serialized [`AgentMigrationBundle`] to the local hotel.
    ///
    /// Writes agent identity, apartments, role incarnations, vault entries, guests,
    /// and config to the local context graph, then materializes the new guests.
    ///
    /// Called internally by the `"agent.deploy_bundle"` operator surface handler.
    ///
    /// Responds with [`IpcResponse::Standard`].
    ApplyAgentBundle {
        bundle_json: String,
    },
    /// Heal-dispatcher → hotel: a recurring `(pattern_tag, guest_id)` failure
    /// pattern breached the filing threshold; file (or bump) a
    /// `heal_work_item` node through the `fleet.heal_slices` autonomy lane
    /// (Autopoiesis Slice A3 — ProposalOnly posture: filing IS the action).
    ///
    /// The hotel consults the lane kill switch, grant freeze state, and daily
    /// action budget before filing, writes an `autonomy_audit` record plus the
    /// work-item node, and pushes a resolved `work_item_filed` heal-queue info
    /// entry for operator visibility. Dedup: one OPEN work item per
    /// `(pattern_tag, guest_id)` — a re-breach while open bumps count/last_seen.
    ///
    /// `evidence_lines` are capped hotel-side to
    /// [`ansible_mesh_core::heal_queue::MAX_HEAL_EVIDENCE_LINES`] lines /
    /// [`ansible_mesh_core::heal_queue::MAX_HEAL_EVIDENCE_BYTES`] bytes total.
    ///
    /// Responds with [`IpcResponse::Standard`] — `data` carries
    /// `{filed, deduped, reason?, work_item_id?, audit_id?}`. No new
    /// `IpcResponse` variant is introduced, so the untagged-ordering invariant
    /// (all-optional variants like `MemoryConfig` stay last) is untouched.
    ///
    /// NOTE: new `IpcRequest` variants are appended at the END of this enum.
    FileHealWorkItem {
        pattern_tag: String,
        guest_id: String,
        occurrence_count: u32,
        window_secs: u64,
        #[serde(default)]
        evidence_lines: Vec<String>,
    },
    /// Guest → hotel: ask permission to take one autonomous action on an
    /// autonomy lane (Autopoiesis Slice A2; A1 grant machinery, lane
    /// `graph.bridge_edges` first).
    ///
    /// The hotel consults the lane kill switch, the per-lane `AutonomyGrant`
    /// posture, freeze state, and daily action budget — exactly the A3
    /// shape — and, for ConfirmFirst/AutoWithAudit postures, writes a
    /// Pending `autonomy_audit` record from `action_summary` / `evidence` /
    /// `reversal_hint` before answering.
    ///
    /// Responds with [`IpcResponse::Standard`] — `data` carries
    /// `{allowed, posture, audit_id?, reason?}`:
    /// - `allowed=true, posture="auto_with_audit", audit_id` → act now.
    /// - `allowed=false, posture="confirm_first", audit_id` → file the
    ///   ready-to-apply spec as `awaiting_confirmation`; the operator's
    ///   confirmation applies it and reports the outcome.
    /// - `allowed=false` with no `audit_id` (posture `proposal_only`, lane
    ///   disabled/frozen, or budget exhausted — see `reason`) → prose-only.
    ///
    /// No new `IpcResponse` variant — the untagged-ordering invariant
    /// (all-optional variants like `MemoryConfig` stay last) is untouched.
    ConsumeAutonomyAction {
        lane: String,
        action_summary: String,
        evidence: String,
        reversal_hint: String,
    },
    /// Operator/steward → hotel: report the reviewed outcome of an audited
    /// autonomous action so the lane earns (or loses) trust (Autopoiesis
    /// Slice A2; feeds A1's `record_autonomy_outcome`; the `outcome` vocabulary
    /// itself — including `"neutral"` — is Slice A9's `trust-ledger`).
    ///
    /// `outcome` is `"confirmed_good"` (feeds the earn counter), `"reversed"`
    /// (demotes one posture level), or `"neutral"` (stamps the audit record
    /// only — no effect on the grant's earn/demote counters; a wash, not a
    /// signal). The hotel stamps the `autonomy_audit` record and, for
    /// `confirmed_good`/`reversed`, applies the grant transition (promotion /
    /// demotion / failure-streak bookkeeping). Recording is idempotent: a
    /// second report against an already-reviewed audit id is refused with
    /// `recorded=false, reason="already_recorded"` so one confirmation can
    /// never double-count toward promotion.
    ///
    /// A configurable timeout-to-`"neutral"` sweep for audits left `Pending`
    /// past some age is wired up in `aiua::autonomy_sweep` (A9
    /// outcome-stamping follow-up slice) — see
    /// `ansible_mesh_core::autonomy::AuditOutcome::Neutral`. `"neutral"` is
    /// also reachable at any time through an explicit report on this path.
    ///
    /// Responds with [`IpcResponse::Standard`] — `data` carries
    /// `{recorded, lane?, transition?, posture?, reason?}`.
    RecordAutonomyOutcome {
        audit_id: String,
        outcome: String,
    },
    /// Agent → hotel: ask the routing oracle for the best live model
    /// controllers for a task, ranked. Consulted by philote when a turn's
    /// configured `fallback_tiers` ladder is empty or exhausted — the oracle
    /// is the dynamic safety net *beneath* operator-configured ladders,
    /// never a replacement for them.
    ///
    /// `exclude_providers` names providers that already failed this turn so
    /// the reply never routes straight back into the failure. All other
    /// fields describe what the task needs (see core `RouteNeed`).
    ///
    /// Responds with [`IpcResponse::Standard`] — `data` carries
    /// `{ranked: [{role, provider, model_ref, score}], disabled: bool}`.
    /// Ranked entries are limited to controller roles with a live guest on
    /// the local hotel. MUST remain the last variant-shape concern for
    /// back-compat: new variants append after this one.
    QueryModelRoute {
        request_class: String,
        needs_tools: bool,
        needs_structured: bool,
        approx_context_tokens: u32,
        /// "interactive" | "background"
        latency_class: String,
        /// "local_trusted" | "local_experimental" | "remote_cloud"
        trust_ceiling: String,
        #[serde(default)]
        exclude_providers: Vec<String>,
    },
    /// Guest → hotel: push a pre-classified turn-level failure event into the
    /// self-heal queue (turn-failure heal intake). Used by philote for
    /// failures only the agent loop can see: watchdog evictions
    /// (`stuck_turn_evicted:{phase}`), fallback-ladder exhaustion
    /// (`fallback_exhausted:{last_provider}`), and paracrine budget breaches
    /// (`paracrine_budget_exhausted`).
    ///
    /// The hotel stores the entry pre-triaged (severity + pattern_tag set at
    /// insert) so the heal-dispatcher's A3 recurrence counter aggregates it
    /// without re-classification, and applies flood control: the same
    /// `(guest_id, pattern_tag)` within the flood window collapses.
    ///
    /// Responds with [`IpcResponse::Standard`] — `data` carries
    /// `{collapsed, id?}`. No new `IpcResponse` variant, so the
    /// untagged-ordering invariant (all-optional variants like `MemoryConfig`
    /// stay last) is untouched. New `IpcRequest` variants append after this
    /// one.
    PushHealEvent {
        guest_id: String,
        severity: String,
        pattern_tag: String,
        detail: String,
    },
    /// Close a heal work item (Autopoiesis Slice A3 closure path, finding F8).
    ///
    /// The hotel filed a `heal_work_item` node when a pattern recurred past the
    /// window threshold; once the underlying fault is repaired the autonomy-lane
    /// loop (or an operator via `phil heal close`) closes it so it stops showing
    /// as open work. Wired straight to
    /// [`ansible_mesh_core::domain::GraphDomain::close_heal_work_item`].
    ///
    /// Responds with [`IpcResponse::Standard`] — `data` carries
    /// `{closed, work_item_id}`: `closed=true` when the item existed (open OR
    /// already-closed → idempotent), `false` when no such id. No new
    /// `IpcResponse` variant, so the untagged-ordering invariant (all-optional
    /// variants like `MemoryConfig` stay last) is untouched. New `IpcRequest`
    /// variants append after this one.
    CloseHealWorkItem {
        work_item_id: String,
    },
}

fn default_heal_queue_limit() -> usize {
    50
}

// ── Agent migration bundle types ─────────────────────────────────────────────

/// A vault secret that travels with an agent during migration.
/// The plaintext is included so the destination hotel can re-encrypt with its
/// own vault key.  Only transmit over a trusted network (e.g. Tailscale VPN).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntryExport {
    /// Config key that points at this secret (e.g. `"telegram_bot_token_beacon"`).
    pub config_key: String,
    /// Vault name (e.g. `"bjork"`), used to place the entry in the dest vault_registry.
    pub vault_name: String,
    /// Decrypted plaintext value.
    pub plaintext: String,
    /// Role ACL preserved from the source secret record.
    #[serde(default)]
    pub allowed_roles: Vec<String>,
}

/// A non-vault config entry to replicate verbatim on the destination hotel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigEntryExport {
    /// Config key without the `"config:"` prefix.
    pub key: String,
    /// Raw JSON value string as stored in the context graph.
    pub value_json: String,
}

/// A guest record stripped of its hotel prefix, ready to be re-homed on dest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestExport {
    /// The suffix of the guest_id after the hotel prefix (e.g. `"philote-beacon"`).
    pub guest_id_suffix: String,
    pub role: String,
    /// Raw config_json string from the source GuestRecord.
    /// The `ApplyAgentBundle` handler replaces occurrences of the source hotel name.
    pub config_json: String,
    pub is_active: bool,
}

/// Full snapshot of an agent suitable for cross-hotel migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMigrationBundle {
    pub migration_id: String,
    pub source_hotel: String,
    /// Stable agent identifier (e.g. `"agent-beacon-01"`).
    pub agent_id: String,
    /// Short agent key used in guest ID patterns (e.g. `"beacon"`).
    pub agent_key: String,
    pub agent_identity: AgentIdentityRecord,
    pub apartments: Vec<(String, serde_json::Value)>,
    pub role_incarnations: Vec<RoleIncarnationRecord>,
    pub vault_entries: Vec<VaultEntryExport>,
    pub config_entries: Vec<ConfigEntryExport>,
    pub guests: Vec<GuestExport>,
    pub timestamp: u64,
}

/// Payload for [`IpcResponse::UserProfileData`].
///
/// MUST use `deny_unknown_fields` so that `#[serde(untagged)]` deserialization rejects
/// objects with unrecognised fields (e.g. `{ "config_json": "..." }`) instead of
/// silently consuming them as `UserProfileData { timezone: None, display_name: None }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UserProfileDataPayload {
    pub timezone: Option<String>,
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_hotel: Option<String>,
    #[serde(default)]
    pub linked_providers: Vec<String>,
}

/// Payload for [`IpcResponse::MemoryConfig`].
///
/// `MemoryConfig` has an optional value by design, but this wrapper must remain
/// strict so untagged deserialization does not accidentally classify unrelated
/// response objects as memory config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfigPayload {
    pub config_json: Option<String>,
}

/// One session summary returned by [`IpcResponse::OperatorSessionList`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperatorSessionView {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Channel/transport the session arrived on (e.g. "operator_chat", "telegram").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// Session status as stored ("active", "paused", ...).
    pub status: String,
    /// Unix epoch seconds of the last recorded activity on the session.
    pub last_activity_at: u64,
    /// Channel session key (e.g. chat id) when one is recorded — a cheap title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Short excerpt of the most recent turn content, when derivable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// One operator/agent message expanded from a session turn record, returned by
/// [`IpcResponse::SessionTurnList`]. A single stored turn record can expand to
/// two items sharing the same `turn_id`: the operator message and the agent reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionTurnView {
    pub turn_id: String,
    /// "operator" for the inbound user message, "agent" for the reply.
    pub role: String,
    pub content: String,
    /// Unix epoch seconds; started_at for operator items, completed_at for agent items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    /// Turn processing status ("queued", "running", "completed", "failed").
    pub status: String,
}

/// One reachable endpoint advertised by a mesh node, returned inside
/// [`MeshRosterEntryView`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeshEndpointView {
    /// Logical purpose: "gateway", "beacon", "execution", "blob", "membrane-mcp", "ipc".
    pub purpose: String,
    pub host: String,
    pub port: u16,
    /// Exposure tier of the listener ("local", "lan", "mesh", "internet"), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Wire protocol for execution reachability entries (e.g. "tcp").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

/// One node (self or peer) returned by [`IpcResponse::MeshRosterView`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeshRosterEntryView {
    pub node_id: String,
    pub is_self: bool,
    /// Hotel name for the node when known (from the local graph for self,
    /// from HotelStateSync for peers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Mesh roles advertised in the node's capabilities manifest.
    #[serde(default)]
    pub roles: Vec<String>,
    /// The node's effective exposure ceiling ("local", "lan", "mesh", "internet"),
    /// when a perimeter snapshot has been advertised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_ceiling: Option<String>,
    /// Reachable listener endpoints (perimeter listeners + execution reachability).
    #[serde(default)]
    pub endpoints: Vec<MeshEndpointView>,
}

/// Represents the canonical response from the local Ansible back to the Guest via IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IpcResponse {
    Ack {
        req_id: String,
    },
    ConfigData {
        key: String,
        value_json: Option<String>,
    },
    SecretData {
        secret_ref: String,
        value_json: Option<String>,
    },
    TelegramPollLease {
        granted: bool,
        lease: Option<LeaseEnvelope>,
    },
    DesktopMembraneLease {
        desktop_granted: bool,
        desktop_lease: Option<LeaseEnvelope>,
    },
    TelegramPollLeaseStatus {
        active: bool,
        lease: Option<LeaseEnvelope>,
    },
    DesktopMembraneLeaseStatus {
        desktop_active: bool,
        desktop_lease: Option<LeaseEnvelope>,
    },
    DesktopMembraneStatusView {
        membrane_status: DesktopMembraneStatusView,
    },
    DesktopMembraneTargetStatusView {
        membrane_target_status: DesktopMembraneTargetStatusView,
    },
    OperatorTargetsView {
        operator_targets: Vec<OperatorTargetView>,
    },
    OperatorTargetStatusView {
        operator_target_status: OperatorTargetStatusView,
    },
    OperatorTargetGuestsView {
        operator_target_guests: OperatorTargetGuestInventoryView,
    },
    OperatorTargetAgentsView {
        operator_target_agents: OperatorTargetAgentInventoryView,
    },
    OperatorTargetComponentsView {
        operator_target_components: OperatorTargetComponentInventoryView,
    },
    OperatorTargetConfigView {
        operator_target_config: OperatorTargetConfigView,
    },
    OperatorTargetSecretsView {
        operator_target_secrets: OperatorTargetSecretInventoryView,
    },
    OperatorTargetPlacementView {
        operator_target_placement: OperatorTargetPlacementView,
    },
    OperatorTargetComponentMutationAckView {
        operator_target_component_mutation: OperatorTargetComponentMutationAckView,
    },
    OperatorTargetConfigMutationAckView {
        operator_target_config_mutation: OperatorTargetConfigMutationAckView,
    },
    OperatorTargetSecretMutationAckView {
        operator_target_secret_mutation: OperatorTargetSecretMutationAckView,
    },
    OperatorTargetRoleHomeAckView {
        operator_target_role_home: OperatorTargetRoleHomeAckView,
    },
    OperatorChatTurnReply {
        operator_chat_reply: OperatorChatTurnReply,
    },
    DesktopMembraneGuestsView {
        membrane_guests: Vec<DesktopMembraneGuestView>,
    },
    DesktopMembraneTargetGuestsView {
        membrane_target_guests: DesktopMembraneTargetGuestInventoryView,
    },
    DesktopMembraneTargetComponentsView {
        membrane_target_components: DesktopMembraneTargetComponentInventoryView,
    },
    DesktopMembraneAgentsView {
        membrane_agents: Vec<DesktopMembraneAgentView>,
    },
    DesktopMembraneTargetsView {
        membrane_targets: Vec<DesktopMembraneTargetView>,
    },
    HandoffAck {
        handoff_guest_id: String,
        became_active: bool,
    },
    HandoffPending {
        role_name: String,
        readiness: String,
        #[serde(default)]
        retry_after_ms: Option<u64>,
    },
    HandoffBackAck {
        // Named differently from HandoffAck.handoff_guest_id so the untagged serde
        // repr can distinguish the two variants (serde tries variants in order; a
        // missing required field causes the attempt to fail and the next is tried).
        return_guest_id: String,
        became_active: bool,
    },
    DelegationAck {
        delegation_id: String,
        status: String,
    },
    SpawnSubagentOk {
        subagent_guest_id: String,
        confirmed_lease: LeaseEnvelope,
    },
    SpawnSubagentProposal {
        subagent_guest_id: String,
        confirmed_lease: LeaseEnvelope,
        delta: SpawnSubagentDelta,
    },
    SubagentLeaseRenewed {
        subagent_guest_id: String,
        new_epoch: u64,
        expires_at: u64,
    },
    /// Response to [`IpcRequest::RegisterSkill`].
    SkillRegistered {
        skill_name: String,
        /// `"validated"` | `"invalid"` | `"draft"` depending on Layer 1 outcome.
        validation_state: String,
        /// Human-readable summary of any validation errors; empty on success.
        #[serde(default)]
        validation_errors: Vec<String>,
    },
    /// Response to [`IpcRequest::PatchAgentBundle`].
    AgentUpdated {
        agent: DesktopMembraneAgentView,
    },
    /// Response to [`IpcRequest::AssignSkill`] and [`IpcRequest::RevokeSkill`].
    SkillAssigned {
        role_name: String,
        skill_name: String,
        operation: String, // "assigned" or "revoked"
    },
    /// Response to [`IpcRequest::ListSkills`].
    SkillList {
        skills: Vec<serde_json::Value>,
    },
    InboundTask {
        source_node: String,
        task_id: Uuid,
        task_json: String,
    },
    /// Response to [`IpcRequest::ConfigureRole`].
    ConfigureRoleOk {
        role_name: String,
    },
    /// Response to [`IpcRequest::SetRoleHome`].
    RoleHomeSet {
        role_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        home_node: Option<String>,
    },
    /// Response to [`IpcRequest::SetTransportHome`].
    TransportHomeSet {
        agent_id: String,
        transport: String,
        resource_ref: String,
        active_home_hotel: String,
        #[serde(default)]
        standby_hotels: Vec<String>,
    },
    /// Response to [`IpcRequest::ExecuteWorkflow`].
    WorkflowExecutionOk {
        workflow_name: String,
        #[serde(default)]
        result: serde_json::Value,
    },
    Error(String),
    Standard {
        ok: bool,
        code: String,
        message: String,
        corr_id: String,
        data: Option<serde_json::Value>,
    },
    /// Hotel actively pushing a memory apartment conflict resolution or external sync to the Guest
    ApartmentUpdate {
        agent_id: String,
        memory_type: String,
        content_json: serde_json::Value,
    },
    /// Response to [`IpcRequest::RegisterGraphInstance`].
    GraphInstanceRegistered {
        graph_id: String,
    },
    /// Response to [`IpcRequest::ProposeRule`].
    RuleProposed {
        rule_id: String,
    },
    RoutingPolicyRecorded {
        proposal_id: String,
    },
    RoutingPolicyList {
        policies: Vec<serde_json::Value>,
    },
    /// Response to [`IpcRequest::ListRules`].
    RuleList {
        rules: Vec<serde_json::Value>,
    },
    /// Hotel → agent: the requested resource is live and the binding is active.
    /// Response to [`IpcRequest::ResourceRequest`].
    ResourceGranted {
        resource_granted: ResourceGranted,
    },
    /// Hotel → agent: the resource request was refused.
    /// Response to [`IpcRequest::ResourceRequest`].
    ResourceDenied {
        resource_denied: ResourceDenied,
    },
    /// Hotel → agent: the resource is being materialised; a `ResourceGranted`
    /// will follow asynchronously once the instance is ready.
    /// Response to [`IpcRequest::ResourceRequest`].
    ResourceMaterializing {
        resource_materializing: ResourceMaterializing,
    },
    /// Hotel → agent: the hotel is withdrawing a previously issued grant.
    /// Pushed without a corresponding request; agents must stop using the
    /// resource immediately.
    ResourceRevoked {
        resource_revoked: ResourceRevoked,
    },
    /// Response to [`IpcRequest::RegisterComponent`].
    ComponentRegistered {
        registered_guest_id: String,
        registered_role: String,
    },
    /// Response to [`IpcRequest::ListComponents`].
    ComponentInventory {
        components: Vec<serde_json::Value>,
    },
    /// Response to [`IpcRequest::ListGraphInstances`].
    GraphInstanceList {
        instances: Vec<serde_json::Value>,
    },
    /// Response to [`IpcRequest::ListCronJobs`].
    CronJobList {
        jobs: Vec<CronJob>,
    },
    // ── MCP membrane IPC responses ────────────────────────────────────────────
    /// Response to [`IpcRequest::AcquireMcpMembraneLease`] /
    /// [`IpcRequest::RenewMcpMembraneLease`].
    McpMembraneLease {
        mcp_granted: bool,
        mcp_lease: Option<LeaseEnvelope>,
    },
    /// Response to [`IpcRequest::UpdateMcpRoutes`].
    McpRoutesAccepted {
        mcp_routes_agent_id: String,
        mcp_route_count: usize,
    },
    /// Response to [`IpcRequest::ProvisionMcpEndpoint`] /
    /// [`IpcRequest::RevokeMcpEndpoint`].
    McpEndpointProvisioned {
        endpoint_id: String,
        port: u16,
        /// `true` if a new membrane-mcp guest was spawned; `false` if an
        /// existing guest's config was updated in place.
        materialized: bool,
    },
    /// Response to [`IpcRequest::RegisterMcpUpstream`] /
    /// [`IpcRequest::RevokeMcpUpstream`].
    McpUpstreamRegistered {
        mcp_upstream_id: String,
        /// `true` if a new mcp-client guest was spawned; `false` if an
        /// existing guest received the config update in place.
        mcp_upstream_materialized: bool,
    },
    /// Response to [`IpcRequest::GetMcpUpstreams`].
    McpUpstreamsState {
        mcp_upstreams: Vec<McpUpstreamEntry>,
    },
    DiscordGatewayLease {
        granted: bool,
        lease: Option<LeaseEnvelope>,
    },
    DiscordGatewayLeaseStatus {
        active: bool,
        lease: Option<LeaseEnvelope>,
    },
    /// Sent from hotel to guests during graceful shutdown. Guests should drain
    /// in-flight work and exit within `drain_timeout_secs`.
    GracefulShutdown {
        drain_timeout_secs: u64,
    },
    /// Hotel → guest: network reachability state changed.
    /// Pushed without a corresponding request. Guests should pause outbound connections
    /// (e.g. long-polling) when `online=false` and resume when `online=true`.
    NetworkState {
        online: bool,
    },
    /// Hotel → guest broadcast: MuninnDB reachability state changed.
    /// Also the direct response to [`IpcRequest::RefreshMemoryConfig`].
    /// When `available=false`, guests should fall back to `NullMemoryEngine`.
    /// When `available=true`, guests should resume using the configured engine.
    MuninnStatus {
        available: bool,
        endpoint: String,
    },
    /// Response to [`IpcRequest::FetchMemoryConfig`].
    /// `config_json` is `None` if MuninnDB is not configured on this hotel.
    ///
    /// Response to [`IpcRequest::GetUserProfile`] and [`IpcRequest::PatchUserProfile`].
    ///
    /// NOTE: `UserProfileDataPayload` uses `#[serde(deny_unknown_fields)]`, which causes
    /// serde to reject JSON objects with fields not in the struct (e.g. `config_json`).
    /// This prevents this variant from swallowing `MemoryConfig` responses.
    UserProfileData(UserProfileDataPayload),
    /// Response to [`IpcRequest::PushHealEntry`].
    HealEntryPushed {
        id: String,
    },
    /// Response to [`IpcRequest::GetHealQueuePending`].
    ///
    /// Keep this before generic `rows: Vec<Value>` response shapes in this
    /// untagged enum, otherwise typed heal queue rows deserialize as generic
    /// rows and callers never see `HealQueuePending`.
    HealQueuePending {
        rows: Vec<ansible_mesh_core::heal_queue::HealQueueRow>,
    },
    /// Response to [`IpcRequest::GetAgentReflexPreferences`].
    AgentReflexPreferences {
        rows: Vec<serde_json::Value>,
    },
    /// Response to [`IpcRequest::GetRoutingPipelineRules`].
    /// Uses `pipeline_rules` (not `rules`) to distinguish from [`RuleList`] in untagged serde.
    RoutingPipelineRules {
        pipeline_rules: Vec<serde_json::Value>,
    },
    /// Response to [`IpcRequest::GetMcpRoutes`].
    /// Contains all persisted route sets, keyed by agent_id.
    McpRouteState {
        agents: Vec<PersistedMcpRouteEntry>,
    },
    UserTaskCreated {
        user_task_id: String,
    },
    UserTaskUpdated {
        user_task_id: String,
        user_task_updated: bool,
    },
    UserTaskData {
        user_task_json: String,
    },
    UserTaskList {
        user_tasks: Vec<serde_json::Value>,
    },
    /// Response to [`IpcRequest::GetPerimeterStatus`] and [`IpcRequest::RefreshPerimeter`].
    /// `snapshot_json` is the serialized [`ansible_mesh_core::PerimeterSnapshot`].
    PerimeterStatus {
        snapshot_json: String,
    },
    /// Hotel → guest broadcast: the hotel's network security perimeter ceiling changed.
    /// Sent to ALL connected guests via the broadcast channel when the perimeter shifts.
    /// Guests should react by re-evaluating in-flight work, adjusting routing assumptions,
    /// or propagating the shift to their own listeners.
    PerimeterShift {
        previous: ansible_mesh_core::ExposureTier,
        current: ansible_mesh_core::ExposureTier,
    },
    /// Response to [`IpcRequest::CheckEgress`].
    EgressGrant {
        /// Whether the request is permitted.
        allowed: bool,
        /// Set to `true` when the policy matched `AllowWithAudit`.
        audit: bool,
        /// If `allowed` is false, the reason for denial.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deny_reason: Option<String>,
        /// Headers to inject into the outbound request (e.g. `Authorization: Bearer <token>`).
        /// Only populated when `allowed` is true and a vault credential was resolved.
        #[serde(default)]
        inject_headers: std::collections::HashMap<String, String>,
    },
    RouterStats {
        stats: Vec<ansible_mesh_core::router_trace::ProviderStats>,
        /// Unix epoch seconds of the query.
        generated_at: u64,
    },
    /// Response to [`IpcRequest::ListOperatorSessions`].
    ///
    /// Untagged-serde safety: the required, uniquely-named `operator_sessions`
    /// field keeps this variant from swallowing `Standard` acks or being
    /// swallowed by earlier variants.
    OperatorSessionList {
        operator_sessions: Vec<OperatorSessionView>,
    },
    /// Response to [`IpcRequest::ListSessionTurns`].
    ///
    /// Untagged-serde safety: both fields are required; `turns_session_id` is
    /// deliberately not named `session_id` so no earlier variant shape matches.
    SessionTurnList {
        turns_session_id: String,
        session_turns: Vec<SessionTurnView>,
    },
    /// Response to [`IpcRequest::GetMeshRoster`].
    ///
    /// Untagged-serde safety: the required, uniquely-named `mesh_roster` field
    /// makes this variant structurally unambiguous.
    MeshRosterView {
        mesh_roster: Vec<MeshRosterEntryView>,
    },
    // CRITICAL: `MemoryConfig` (all-optional payload) must remain the LAST
    // variant of this untagged enum — see `project_cron_scheduler.md` /
    // `bug_ipcresponse_untagged_ordering.md`. Add new variants ABOVE this line
    // and give them at least one required, uniquely-named field.
    MemoryConfig(MemoryConfigPayload),
}

/// One agent's persisted route set, as returned by [`IpcResponse::McpRouteState`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedMcpRouteEntry {
    pub agent_id: String,
    pub routes: Vec<ansible_mesh_core::mcp_route::McpRouteRecord>,
    /// Vault ref for the bearer token, if one was supplied at provisioning time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_ref: Option<String>,
}

/// One registered upstream MCP server plus its last reported catalog, as
/// returned by [`IpcResponse::McpUpstreamsState`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpUpstreamEntry {
    pub config: ansible_mesh_core::mcp_upstream::McpUpstreamConfig,
    /// Last catalog report from the mcp-client guest, if any yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<ansible_mesh_core::mcp_upstream::McpUpstreamCatalog>,
}

impl IpcResponse {
    pub fn success(corr_id: impl Into<String>, data: Option<serde_json::Value>) -> Self {
        Self::Standard {
            ok: true,
            code: "OK".into(),
            message: "Success".into(),
            corr_id: corr_id.into(),
            data,
        }
    }

    pub fn error(
        corr_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Standard {
            ok: false,
            code: code.into(),
            message: message.into(),
            corr_id: corr_id.into(),
            data: None,
        }
    }
}

/// A pushed event delivered to an active inbox subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcPushEvent {
    pub event_id: Uuid,
    pub source_node: String,
    pub payload: serde_json::Value,
}

/// The concrete Universal Hotel Client SDK
pub struct PhiloticClient {
    stream: UnixStream,
    _identity: GuestIdentity,
    pending_push: VecDeque<IpcResponse>,
    read_buf: Vec<u8>,
    /// DEF-045: number of responses still owed to requests whose caller gave up
    /// on them (via [`Self::send_request_with_timeout`] elapsing) before the
    /// hotel's reply arrived. The IPC stream is a single in-order pipe with no
    /// per-message correlation ID, so a dropped future does not un-send the
    /// request — the hotel still processes it and writes exactly one reply
    /// frame back, later, in its turn. Every non-push frame read while this is
    /// > 0 is *that* stale reply (FIFO ordering guarantees it precedes the
    /// reply to any request written after the timeout), so it is discarded
    /// before normal response matching resumes. Pushes are never discarded.
    pending_stale_responses: u32,
}

pub fn is_ipc_disconnect(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(|io_err| {
                matches!(
                    io_err.kind(),
                    ErrorKind::UnexpectedEof
                        | ErrorKind::BrokenPipe
                        | ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::NotConnected
                )
            })
            .unwrap_or(false)
    })
}

pub fn is_graceful_shutdown(resp: &IpcResponse) -> bool {
    matches!(resp, IpcResponse::GracefulShutdown { .. })
}

/// True if `err` (or something in its anyhow chain) is a
/// [`Self::send_request_with_timeout`] elapse rather than a genuine transport/
/// protocol failure. Lets callers that previously distinguished an outer
/// `tokio::time::timeout` elapse from an inner `send_request` error keep doing
/// so after migrating to `send_request_with_timeout`'s single `Result`.
pub fn is_ipc_timeout(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<tokio::time::error::Elapsed>()
            .is_some()
    })
}

impl PhiloticClient {
    async fn write_frame(&mut self, payload: &[u8]) -> Result<()> {
        let len = u32::try_from(payload.len()).context("IPC payload too large")?;
        self.stream
            .write_all(&len.to_be_bytes())
            .await
            .context("Failed to send IPC frame header to Ansible")?;
        self.stream
            .write_all(payload)
            .await
            .context("Failed to send IPC frame payload to Ansible")?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>> {
        loop {
            if self.read_buf.len() >= 4 {
                let len = u32::from_be_bytes([
                    self.read_buf[0],
                    self.read_buf[1],
                    self.read_buf[2],
                    self.read_buf[3],
                ]) as usize;
                let frame_len = 4 + len;
                if self.read_buf.len() >= frame_len {
                    let payload = self.read_buf[4..frame_len].to_vec();
                    self.read_buf.drain(..frame_len);
                    return Ok(payload);
                }
            }

            self.stream
                .readable()
                .await
                .context("Failed to wait for IPC frame bytes")?;

            let mut chunk = [0u8; 8192];
            match self.stream.try_read(&mut chunk) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "IPC stream closed while receiving frame",
                    ))
                    .context("Failed to receive IPC frame payload");
                }
                Ok(n) => self.read_buf.extend_from_slice(&chunk[..n]),
                Err(err) if err.kind() == ErrorKind::WouldBlock => continue,
                Err(err) => return Err(err).context("Failed to receive IPC frame payload"),
            }
        }
    }

    fn socket_path() -> String {
        std::env::var("PHILOTIC_HOTEL_SOCKET")
            .unwrap_or_else(|_| "/tmp/philotic-aiua.sock".to_string())
    }

    /// Connect to the local Ansible daemon automatically, driven by environment variables.
    /// Default Hotel socket is `/tmp/philotic-aiua.sock` unless `PHILOTIC_HOTEL_SOCKET` is specified.
    pub async fn connect_at(socket_path: impl AsRef<str>, identity: GuestIdentity) -> Result<Self> {
        let socket_path = socket_path.as_ref().to_string();
        let stream = UnixStream::connect(&socket_path)
            .await
            .with_context(|| format!("Failed to connect to hotel IPC socket at {}", socket_path))?;

        debug!(
            "PhiloticClient connecting to local Ansible at {}...",
            socket_path
        );

        let mut client = Self {
            stream,
            _identity: identity.clone(),
            pending_push: VecDeque::new(),
            read_buf: Vec::new(),
            pending_stale_responses: 0,
        };

        info!("Registering as Materialized Guest: {:?}", identity);
        let resp = client.send_request(IpcRequest::Register(identity)).await?;
        info!("Ansible Hotel Registration Response: {:?}", resp);

        match resp {
            IpcResponse::Standard { ok, message, .. } if !ok => {
                anyhow::bail!("Hotel rejected registration: {}", message);
            }
            IpcResponse::Error(msg) => {
                anyhow::bail!("Hotel rejected registration: {}", msg);
            }
            _ => {}
        }

        Ok(client)
    }

    /// Connect to the local Ansible daemon automatically, driven by environment variables.
    /// Default Hotel socket is `/tmp/philotic-aiua.sock` unless `PHILOTIC_HOTEL_SOCKET` is specified.
    pub async fn connect(identity: GuestIdentity) -> Result<Self> {
        Self::connect_at(Self::socket_path(), identity).await
    }

    /// Send an IPC request to the local Ansible and wait indefinitely for its reply.
    pub async fn send_request(&mut self, req: IpcRequest) -> Result<IpcResponse> {
        let payload = serde_json::to_vec(&req).context("Failed to serialize IpcRequest")?;
        self.write_frame(&payload).await?;
        self.read_matching_response(&req).await
    }

    /// Same contract as [`Self::send_request`], but bounds the wait for a reply to
    /// `timeout`. This exists because callers historically wrapped `send_request`
    /// in an external `tokio::time::timeout` — but dropping that future when the
    /// timeout elapsed did NOT stop the hotel from eventually writing a reply
    /// frame back. Since this IPC stream carries no per-message correlation ID,
    /// that orphaned reply would sit at the front of the stream and get handed
    /// out as the response to the *next* `send_request` call, permanently
    /// desyncing the connection one frame (DEF-045).
    ///
    /// This method owns the timeout internally so it can record that a reply is
    /// still owed (see [`Self::pending_stale_responses`]) and transparently
    /// discard it — instead of misdelivering it — the next time any response is
    /// read on this connection.
    ///
    /// On elapse, returns an `Err` describing the timeout (mirrors the shape
    /// callers get from `tokio::time::timeout(..).await?` today).
    pub async fn send_request_with_timeout(
        &mut self,
        req: IpcRequest,
        timeout: Duration,
    ) -> Result<IpcResponse> {
        let payload = serde_json::to_vec(&req).context("Failed to serialize IpcRequest")?;
        self.write_frame(&payload).await?;

        match tokio::time::timeout(timeout, self.read_matching_response(&req)).await {
            Ok(result) => result,
            Err(elapsed) => {
                self.pending_stale_responses = self.pending_stale_responses.saturating_add(1);
                warn!(
                    "IPC request timed out after {:?} waiting for a reply; the hotel's eventual \
                     reply will be discarded when it arrives to keep the connection framed: {:?}",
                    timeout, req
                );
                // Wrap (not discard) the tokio::time::error::Elapsed so callers that need to
                // tell "timed out" apart from "the hotel replied with an actual error" can
                // `err.downcast_ref::<tokio::time::error::Elapsed>()` — see is_ipc_timeout.
                Err(anyhow::Error::new(elapsed).context(format!(
                    "IPC request timed out after {:?}: {:?}",
                    timeout, req
                )))
            }
        }
    }

    /// Read frames from the stream until one matches `req`, buffering any push
    /// messages encountered along the way and discarding stale replies owed to
    /// earlier timed-out requests (see [`Self::pending_stale_responses`]).
    async fn read_matching_response(&mut self, req: &IpcRequest) -> Result<IpcResponse> {
        loop {
            let resp = self.read_response().await?;

            // Pushes are never stale-request fallout and must never be lost —
            // classify and buffer them before any stale-discard logic runs.
            if Self::is_push_message(&resp) {
                self.pending_push.push_back(resp);
                continue;
            }

            // A non-push frame arriving while a reply is still owed to an earlier
            // timed-out request MUST be that reply: this stream is FIFO and that
            // request's write strictly preceded `req`'s write, so its response
            // (if not yet consumed) strictly precedes `req`'s response too.
            //
            // BUT: hotel-wide OOB broadcasts (`is_ignorable_push` — lease status
            // pings, MuninnStatus, NetworkState, etc.) are not pushes but also are
            // not owed replies to anything; they can land on this connection at
            // any time regardless of pending_stale_responses. They must be
            // skipped WITHOUT decrementing the counter here, or a broadcast
            // arriving before the real stale reply burns the "owed" credit and
            // the real reply then falls through to be misdelivered as the
            // response to the current request — reintroducing the exact desync
            // this counter exists to prevent.
            if self.pending_stale_responses > 0 {
                if Self::is_ignorable_push(&resp) {
                    continue;
                }
                self.pending_stale_responses -= 1;
                warn!(
                    "Discarding stale IPC response left over from a prior timed-out request: {:?}",
                    resp
                );
                continue;
            }

            if Self::is_expected_response(req, &resp) {
                return Ok(resp);
            }
            // Ignorable hotel-wide broadcasts (MuninnStatus, NetworkState, lease events) can
            // arrive on any connection at any time, including between a request write and its
            // response read. Skip them here so they don't masquerade as request responses.
            if Self::is_ignorable_push(&resp) {
                continue;
            }
            return Ok(resp);
        }
    }

    async fn read_response(&mut self) -> Result<IpcResponse> {
        let buf = self.read_frame().await?;
        let resp: IpcResponse =
            serde_json::from_slice(&buf).context("Failed to decode IpcResponse from Ansible")?;

        Ok(resp)
    }

    fn is_expected_response(req: &IpcRequest, response: &IpcResponse) -> bool {
        matches!(
            (req, response),
            (
                IpcRequest::AcquireTelegramPollLease { .. }
                    | IpcRequest::RenewTelegramPollLease { .. },
                IpcResponse::TelegramPollLease { .. }
            ) | (
                IpcRequest::GetTelegramPollLeaseOwner { .. },
                IpcResponse::TelegramPollLeaseStatus { .. }
            ) | (
                // Acquire/Renew reply with the lease envelope itself, not the
                // owner-status view. Matching the actual reply variant here is
                // load-bearing: `DesktopMembraneLease` is also in
                // `is_ignorable_push`, so without this arm the real reply is
                // swallowed as an OOB broadcast and send_request hangs forever
                // (DEF-005).
                IpcRequest::AcquireDesktopMembraneLease { .. }
                    | IpcRequest::RenewDesktopMembraneLease { .. },
                IpcResponse::DesktopMembraneLease { .. }
            ) | (
                IpcRequest::GetDesktopMembraneLeaseOwner { .. },
                IpcResponse::DesktopMembraneLeaseStatus { .. }
            ) | (
                // `IpcResponse` is untagged, and `DiscordGatewayLease { granted, lease }`
                // has the same shape as `TelegramPollLease` (which is declared first),
                // so discord lease replies deserialize as `TelegramPollLease` on the
                // wire. Accept both variants for discord requests.
                IpcRequest::AcquireDiscordGatewayLease { .. }
                    | IpcRequest::RenewDiscordGatewayLease { .. },
                IpcResponse::DiscordGatewayLease { .. } | IpcResponse::TelegramPollLease { .. }
            ) | (
                IpcRequest::GetDiscordGatewayLeaseOwner { .. },
                IpcResponse::DiscordGatewayLeaseStatus { .. }
                    | IpcResponse::TelegramPollLeaseStatus { .. }
            ) | (
                IpcRequest::AcquireMcpMembraneLease { .. }
                    | IpcRequest::RenewMcpMembraneLease { .. },
                IpcResponse::McpMembraneLease { .. }
            ) | (
                IpcRequest::GetUserProfile { .. } | IpcRequest::PatchUserProfile { .. },
                IpcResponse::UserProfileData(_)
            )
        )
    }

    fn is_push_message(response: &IpcResponse) -> bool {
        matches!(
            response,
            IpcResponse::InboundTask { .. }
                | IpcResponse::ApartmentUpdate { .. }
                | IpcResponse::GracefulShutdown { .. }
                // Hotel-wide OOB broadcasts: never the expected response to a pending request.
                // Buffering them here prevents send_request from returning them as a real
                // response and contaminating subsequent request/response pairs.
                | IpcResponse::MuninnStatus { .. }
                | IpcResponse::NetworkState { .. }
        )
    }

    fn is_ignorable_push(response: &IpcResponse) -> bool {
        matches!(
            response,
            IpcResponse::NetworkState { .. }
                | IpcResponse::MuninnStatus { .. }
                // Lease notifications may be broadcast or arrive out-of-band on any connection.
                // They are never the expected response to an unrelated request, so skip them.
                | IpcResponse::McpMembraneLease { .. }
                | IpcResponse::DesktopMembraneLease { .. }
                | IpcResponse::TelegramPollLeaseStatus { .. }
                | IpcResponse::DesktopMembraneLeaseStatus { .. }
                | IpcResponse::DiscordGatewayLease { .. }
                | IpcResponse::DiscordGatewayLeaseStatus { .. }
        )
    }

    /// Poll for inbound tasks routed from the Philotic Web
    pub async fn recv_task(&mut self) -> Result<IpcResponse> {
        if let Some(pending) = self.pending_push.pop_front() {
            return Ok(pending);
        }

        loop {
            let resp = self.read_response().await?;
            if Self::is_push_message(&resp) {
                return Ok(resp);
            }
            // DEF-045: a reply still owed to an earlier timed-out send_request_with_timeout
            // call can surface here too (recv_task and send_request share one stream).
            // Discard it silently rather than bailing — it is expected fallout, not a
            // protocol violation.
            //
            // As in `read_matching_response`: OOB broadcasts (`is_ignorable_push`) are not
            // owed replies and must be skipped WITHOUT decrementing the counter, or a
            // broadcast landing before the real stale reply burns the credit and the real
            // reply then falls through unrecognized below.
            if self.pending_stale_responses > 0 {
                if Self::is_ignorable_push(&resp) {
                    continue;
                }
                self.pending_stale_responses -= 1;
                warn!(
                    "Discarding stale IPC response left over from a prior timed-out request \
                     (observed in recv_task): {:?}",
                    resp
                );
                continue;
            }
            // Some live EmitTask paths can leave a successful ACK on the stream before
            // the routed push arrives. While explicitly waiting for a pushed task, this
            // ACK is only framing noise. Do not add Standard OK to send_request's
            // ignorable set: that would swallow real request replies such as Register.
            if matches!(resp, IpcResponse::Standard { ok: true, .. }) {
                continue;
            }
            if Self::is_ignorable_push(&resp) {
                continue;
            }
            anyhow::bail!(
                "Unexpected non-push IPC response while waiting for inbound task: {:?}",
                resp
            );
        }
    }

    /// Write a memory apartment update to the hotel and consume the response so the IPC stream stays framed.
    pub async fn sync_apartment(
        &mut self,
        agent_id: &str,
        memory_type: &str,
        content_json: serde_json::Value,
    ) -> Result<()> {
        let req = IpcRequest::SyncApartment {
            agent_id: agent_id.to_string(),
            memory_type: memory_type.to_string(),
            content_json,
        };
        let response = self.send_request(req).await?;
        match response {
            IpcResponse::Standard { ok: true, .. } => {}
            IpcResponse::Standard { message, .. } => {
                anyhow::bail!("SyncApartment failed: {}", message);
            }
            other => {
                anyhow::bail!("Unexpected SyncApartment response: {:?}", other);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::path::Path;
    use std::sync::{LazyLock, Mutex as StdMutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    static IPC_TEST_ENV_LOCK: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

    fn ipc_env_guard() -> std::sync::MutexGuard<'static, ()> {
        IPC_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn test_socket_path() -> String {
        format!("/tmp/pc-{}.sock", Uuid::new_v4().simple())
    }

    #[test]
    fn configure_role_deserializes_old_payload_without_fallback_tiers() {
        // Wire-compat: a pre-existing caller (or a recorded fixture) that never
        // knew about `fallback_tiers` must still deserialize cleanly, with the
        // field defaulting to `None` (preserve semantics), not an empty Vec.
        let old_payload = serde_json::json!({
            "operation": "configure_role",
            "payload": {
                "agent_id": "agent-jane-01",
                "role_name": "developer",
                "guest_id": "agent-jane-01:developer",
                "calling_role": "orchestrator",
                "toolset_profile": "developer",
            }
        });
        let req: IpcRequest = serde_json::from_value(old_payload).expect("decode legacy payload");
        match req {
            IpcRequest::ConfigureRole { fallback_tiers, .. } => {
                assert_eq!(fallback_tiers, None);
            }
            other => panic!("expected ConfigureRole, got {:?}", other),
        }
    }

    #[test]
    fn configure_role_round_trips_fallback_tiers() {
        let req = IpcRequest::ConfigureRole {
            agent_id: "agent-jane-01".into(),
            role_name: "orchestrator".into(),
            guest_id: "agent-jane-01:orchestrator".into(),
            calling_role: "orchestrator".into(),
            toolset_profile: "orchestrator".into(),
            role_identity_addendum: None,
            role_manifest: None,
            is_admin: false,
            inactive_ttl_seconds: None,
            iteration_cap: None,
            approval_policy: None,
            model_profile: None,
            context_window_policy: None,
            fallback_tiers: Some(vec!["model".into(), "model.openrouter".into()]),
            model_bindings: None,
            content_policy: None,
        };
        let json = serde_json::to_value(&req).expect("serialize");
        let decoded: IpcRequest = serde_json::from_value(json).expect("deserialize");
        match decoded {
            IpcRequest::ConfigureRole { fallback_tiers, .. } => {
                assert_eq!(
                    fallback_tiers,
                    Some(vec!["model".to_string(), "model.openrouter".to_string()])
                );
            }
            other => panic!("expected ConfigureRole, got {:?}", other),
        }
    }

    #[test]
    fn task_error_payload_formats_for_logs_and_fallbacks() {
        let payload = TaskErrorPayload {
            kind: "provider_failure".into(),
            message: "Voice synthesis failed".into(),
            code: Some("ELEVENLABS_BAD_RESPONSE".into()),
            component: Some("model-router".into()),
            provider: Some("elevenlabs".into()),
            capability: Some("voice.synthesize".into()),
            retryable: Some(false),
            sub_kind: None,
            status: None,
            error_class: None,
        };

        let rendered = payload.display_message();
        assert!(rendered.contains("Voice synthesis failed"));
        assert!(rendered.contains("kind=provider_failure"));
        assert!(rendered.contains("code=ELEVENLABS_BAD_RESPONSE"));
        assert!(rendered.contains("component=model-router"));
        assert!(rendered.contains("provider=elevenlabs"));
        assert!(rendered.contains("capability=voice.synthesize"));
        assert!(rendered.contains("retryable=false"));
    }

    #[test]
    fn telegram_poll_lease_response_roundtrips_with_envelope() {
        let response = IpcResponse::TelegramPollLease {
            granted: true,
            lease: Some(LeaseEnvelope {
                lease_type: "telegram_poll".into(),
                lease_scope: "telegram:bot-token:abcd".into(),
                authority_hotel: "hotel-alpha".into(),
                authority_component: Some("aiua".into()),
                owner_guest_id: "membrane-telegram-01".into(),
                owner_hotel: Some("hotel-alpha".into()),
                owner_component_type: Some("membrane".into()),
                lease_epoch: 7,
                lease_expires_at: 1234,
                last_heartbeat_at: 1222,
                status: LeaseStatus::Active,
                delegated_from: None,
                metadata: serde_json::json!({ "agent_id": "agent-jane-01" }),
            }),
        };

        let bytes = serde_json::to_vec(&response).expect("serialize lease response");
        let decoded: IpcResponse =
            serde_json::from_slice(&bytes).expect("deserialize lease response");

        match decoded {
            IpcResponse::TelegramPollLease {
                granted: true,
                lease: Some(lease),
            } => {
                assert_eq!(lease.lease_epoch, 7);
                assert_eq!(lease.owner_guest_id, "membrane-telegram-01");
                assert_eq!(lease.metadata["agent_id"], "agent-jane-01");
            }
            other => panic!("unexpected decoded response: {other:?}"),
        }
    }

    #[test]
    fn session_history_responses_roundtrip_to_their_own_variants() {
        // OperatorSessionList round-trip (including the empty-list edge).
        for sessions in [
            vec![OperatorSessionView {
                session_id: "operator-chat:sess-1:agent-jane-01".into(),
                agent_id: Some("agent-jane-01".into()),
                transport: Some("operator_chat".into()),
                status: "active".into(),
                last_activity_at: 1_750_000_000,
                title: Some("chat-1".into()),
                preview: Some("hello there".into()),
            }],
            Vec::new(),
        ] {
            let bytes = serde_json::to_vec(&IpcResponse::OperatorSessionList {
                operator_sessions: sessions.clone(),
            })
            .expect("serialize operator session list");
            match serde_json::from_slice::<IpcResponse>(&bytes)
                .expect("deserialize operator session list")
            {
                IpcResponse::OperatorSessionList { operator_sessions } => {
                    assert_eq!(operator_sessions, sessions);
                }
                other => panic!("operator session list decoded as wrong variant: {other:?}"),
            }
        }

        // SessionTurnList round-trip.
        let turns = vec![
            SessionTurnView {
                turn_id: "turn-1".into(),
                role: "operator".into(),
                content: "ping".into(),
                created_at: Some(1),
                status: "completed".into(),
            },
            SessionTurnView {
                turn_id: "turn-1".into(),
                role: "agent".into(),
                content: "pong".into(),
                created_at: Some(2),
                status: "completed".into(),
            },
        ];
        let bytes = serde_json::to_vec(&IpcResponse::SessionTurnList {
            turns_session_id: "operator-chat:sess-1:agent-jane-01".into(),
            session_turns: turns.clone(),
        })
        .expect("serialize session turn list");
        match serde_json::from_slice::<IpcResponse>(&bytes).expect("deserialize session turn list")
        {
            IpcResponse::SessionTurnList {
                turns_session_id,
                session_turns,
            } => {
                assert_eq!(turns_session_id, "operator-chat:sess-1:agent-jane-01");
                assert_eq!(session_turns, turns);
            }
            other => panic!("session turn list decoded as wrong variant: {other:?}"),
        }

        // MeshRosterView round-trip.
        let roster = vec![MeshRosterEntryView {
            node_id: "local-aiua-01".into(),
            is_self: true,
            display_name: Some("mac-jane".into()),
            roles: vec!["ansible-node".into()],
            exposure_ceiling: Some("mesh".into()),
            endpoints: vec![MeshEndpointView {
                purpose: "execution".into(),
                host: "100.64.1.2".into(),
                port: 16371,
                tier: None,
                protocol: Some("tcp".into()),
            }],
        }];
        let bytes = serde_json::to_vec(&IpcResponse::MeshRosterView {
            mesh_roster: roster.clone(),
        })
        .expect("serialize mesh roster");
        match serde_json::from_slice::<IpcResponse>(&bytes).expect("deserialize mesh roster") {
            IpcResponse::MeshRosterView { mesh_roster } => assert_eq!(mesh_roster, roster),
            other => panic!("mesh roster decoded as wrong variant: {other:?}"),
        }
    }

    #[test]
    fn existing_responses_still_decode_to_their_variants_after_session_history_variants() {
        // Regression guard for the untagged-enum ordering hazard: adding the
        // session-history/mesh-roster variants must not change how any existing
        // response shape deserializes.

        // Standard ack.
        let bytes =
            serde_json::to_vec(&IpcResponse::success("corr-1", None)).expect("serialize standard");
        match serde_json::from_slice::<IpcResponse>(&bytes).expect("deserialize standard") {
            IpcResponse::Standard { ok: true, code, .. } => assert_eq!(code, "OK"),
            other => panic!("standard ack decoded as wrong variant: {other:?}"),
        }

        // Ack.
        let bytes = serde_json::to_vec(&IpcResponse::Ack {
            req_id: "req-1".into(),
        })
        .expect("serialize ack");
        match serde_json::from_slice::<IpcResponse>(&bytes).expect("deserialize ack") {
            IpcResponse::Ack { req_id } => assert_eq!(req_id, "req-1"),
            other => panic!("ack decoded as wrong variant: {other:?}"),
        }

        // Error.
        let bytes = serde_json::to_vec(&IpcResponse::Error("boom".into())).expect("serialize err");
        match serde_json::from_slice::<IpcResponse>(&bytes).expect("deserialize err") {
            IpcResponse::Error(msg) => assert_eq!(msg, "boom"),
            other => panic!("error decoded as wrong variant: {other:?}"),
        }

        // MemoryConfig (the all-optional payload that must stay last).
        let bytes = serde_json::to_vec(&IpcResponse::MemoryConfig(MemoryConfigPayload {
            config_json: None,
        }))
        .expect("serialize memory config");
        match serde_json::from_slice::<IpcResponse>(&bytes).expect("deserialize memory config") {
            IpcResponse::MemoryConfig(_) => {}
            other => panic!("memory config decoded as wrong variant: {other:?}"),
        }

        // UserProfileData (deny_unknown_fields payload).
        let bytes = serde_json::to_vec(&IpcResponse::UserProfileData(UserProfileDataPayload {
            timezone: Some("America/New_York".into()),
            display_name: None,
            principal_id: None,
            preferred_name: None,
            primary_email: None,
            home_hotel: None,
            linked_providers: vec![],
        }))
        .expect("serialize user profile");
        match serde_json::from_slice::<IpcResponse>(&bytes).expect("deserialize user profile") {
            IpcResponse::UserProfileData(p) => {
                assert_eq!(p.timezone.as_deref(), Some("America/New_York"));
            }
            other => panic!("user profile decoded as wrong variant: {other:?}"),
        }
    }

    #[test]
    fn standard_response_does_not_decode_as_memory_config() {
        let bytes = serde_json::to_vec(&IpcResponse::success("reg", None))
            .expect("serialize standard response");
        let decoded: IpcResponse =
            serde_json::from_slice(&bytes).expect("deserialize standard response");

        match decoded {
            IpcResponse::Standard { ok: true, code, .. } => assert_eq!(code, "OK"),
            other => panic!("standard response decoded as wrong variant: {other:?}"),
        }
    }

    #[test]
    fn memory_config_response_roundtrips_strictly() {
        let response = IpcResponse::MemoryConfig(MemoryConfigPayload {
            config_json: Some("{\"base_url\":\"http://127.0.0.1:8475\"}".into()),
        });
        let bytes = serde_json::to_vec(&response).expect("serialize memory config");
        let decoded: IpcResponse =
            serde_json::from_slice(&bytes).expect("deserialize memory config");

        match decoded {
            IpcResponse::MemoryConfig(config) => {
                assert!(
                    config
                        .config_json
                        .as_deref()
                        .unwrap_or("")
                        .contains("base_url")
                );
            }
            other => panic!("memory config decoded as wrong variant: {other:?}"),
        }
    }

    #[test]
    fn requested_lease_status_responses_are_not_ignored_as_push_noise() {
        assert!(PhiloticClient::is_expected_response(
            &IpcRequest::GetTelegramPollLeaseOwner {
                lease_key: "telegram:bot".into(),
            },
            &IpcResponse::TelegramPollLeaseStatus {
                active: true,
                lease: None,
            }
        ));
        // Acquire/Renew reply with the lease envelope itself (`DesktopMembraneLease`),
        // NOT the owner-status view — see the AcquireDesktopMembraneLease handler in
        // crates/aiua/src/service/ipc.rs. Matching the actual reply variant is what
        // keeps it from being swallowed by is_ignorable_push (DEF-005).
        assert!(PhiloticClient::is_expected_response(
            &IpcRequest::AcquireDesktopMembraneLease {
                lease_key: "desktop:local".into(),
                port: 49152,
            },
            &IpcResponse::DesktopMembraneLease {
                desktop_granted: true,
                desktop_lease: None,
            }
        ));
        assert!(PhiloticClient::is_expected_response(
            &IpcRequest::RenewDesktopMembraneLease {
                lease_key: "desktop:local".into(),
                lease_epoch: 1,
            },
            &IpcResponse::DesktopMembraneLease {
                desktop_granted: true,
                desktop_lease: None,
            }
        ));
        assert!(PhiloticClient::is_expected_response(
            &IpcRequest::GetDesktopMembraneLeaseOwner {
                lease_key: "desktop:local".into(),
            },
            &IpcResponse::DesktopMembraneLeaseStatus {
                desktop_active: true,
                desktop_lease: None,
            }
        ));
        assert!(PhiloticClient::is_expected_response(
            &IpcRequest::GetDiscordGatewayLeaseOwner {
                lease_key: "discord:bot".into(),
            },
            &IpcResponse::DiscordGatewayLeaseStatus {
                active: true,
                lease: None,
            }
        ));
        // IpcResponse is untagged and DiscordGatewayLease has the same field shape
        // as TelegramPollLease (declared first), so discord lease replies arrive
        // deserialized as TelegramPollLease. Both variants must be accepted.
        assert!(PhiloticClient::is_expected_response(
            &IpcRequest::AcquireDiscordGatewayLease {
                lease_key: "discord:bot".into(),
                agent_id: "agent-test".into(),
            },
            &IpcResponse::TelegramPollLease {
                granted: true,
                lease: None,
            }
        ));

        assert!(!PhiloticClient::is_expected_response(
            &IpcRequest::GetConfig { key: "x".into() },
            &IpcResponse::TelegramPollLeaseStatus {
                active: true,
                lease: None,
            }
        ));
    }

    #[test]
    fn requested_user_profile_response_is_not_ignored_as_push_noise() {
        let profile = UserProfileDataPayload {
            timezone: Some("America/New_York".into()),
            display_name: Some("Jared".into()),
            principal_id: Some("user:jared".into()),
            preferred_name: Some("Jared".into()),
            primary_email: None,
            home_hotel: Some("vps-jane".into()),
            linked_providers: vec![],
        };

        assert!(PhiloticClient::is_expected_response(
            &IpcRequest::GetUserProfile {
                hotel_name: "vps-jane".into(),
            },
            &IpcResponse::UserProfileData(profile.clone())
        ));
        assert!(PhiloticClient::is_expected_response(
            &IpcRequest::PatchUserProfile {
                hotel_name: "vps-jane".into(),
                timezone: Some("America/New_York".into()),
                display_name: None,
            },
            &IpcResponse::UserProfileData(profile.clone())
        ));
        assert!(!PhiloticClient::is_expected_response(
            &IpcRequest::GetConfig { key: "x".into() },
            &IpcResponse::UserProfileData(profile)
        ));
    }

    #[test]
    fn user_profile_data_is_never_treated_as_an_ignorable_push() {
        // UserProfileData is always a direct, synchronous reply to GetUserProfile/
        // PatchUserProfile on the same connection — the hotel never broadcasts it
        // unprompted. Treating it as ignorable here previously made send_request
        // skip its own response and loop forever waiting for one that would never
        // come (regression: a prior commit hung every GetUserProfile call).
        let profile = UserProfileDataPayload {
            timezone: None,
            display_name: None,
            principal_id: None,
            preferred_name: None,
            primary_email: None,
            home_hotel: None,
            linked_providers: vec![],
        };

        assert!(!PhiloticClient::is_ignorable_push(
            &IpcResponse::UserProfileData(profile)
        ));
    }

    #[test]
    fn heal_queue_rows_deserialize_as_heal_queue_response() {
        let json = r#"{
            "rows": [{
                "id": "01KVDDMB1YZRG327NJG2HNFXH6",
                "guest_id": "vps-jane:agent-graph-agent-beacon",
                "timestamp": 1781788191,
                "raw_text": "Error: Failed to open agent graph SQLite database",
                "severity": "unknown",
                "status": "pending",
                "pattern_tag": null,
                "heal_action": null,
                "outcome": null
            }]
        }"#;

        let response: IpcResponse = serde_json::from_str(json).expect("decode heal queue response");
        match response {
            IpcResponse::HealQueuePending { rows } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].id, "01KVDDMB1YZRG327NJG2HNFXH6");
                assert_eq!(rows[0].guest_id, "vps-jane:agent-graph-agent-beacon");
            }
            other => panic!("expected HealQueuePending, got {other:?}"),
        }
    }

    async fn read_frame(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .await
            .expect("read frame header");
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        stream
            .read_exact(&mut buf)
            .await
            .expect("read frame payload");
        buf
    }

    async fn write_frame(stream: &mut tokio::net::UnixStream, payload: &[u8]) {
        let len = u32::try_from(payload.len()).expect("frame length");
        stream
            .write_all(&len.to_be_bytes())
            .await
            .expect("write frame header");
        stream
            .write_all(payload)
            .await
            .expect("write frame payload");
    }

    #[tokio::test]
    async fn connect_and_get_config_over_uds() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");

                let buf = read_frame(&mut stream).await;
                let req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                match req {
                    IpcRequest::Register(identity) => assert_eq!(identity.guest_id, "guest-test-1"),
                    other => panic!("unexpected register request: {other:?}"),
                }
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                let buf = read_frame(&mut stream).await;
                let req: IpcRequest = serde_json::from_slice(&buf).expect("decode get_config");
                match req {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "telegram_bot_token"),
                    other => panic!("unexpected config request: {other:?}"),
                }
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "telegram_bot_token".into(),
                        value_json: Some("\"secret-token\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-1".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");
        let response = client
            .send_request(IpcRequest::GetConfig {
                key: "telegram_bot_token".into(),
            })
            .await
            .expect("send request");

        match response {
            IpcResponse::ConfigData { key, value_json } => {
                assert_eq!(key, "telegram_bot_token");
                assert_eq!(value_json.as_deref(), Some("\"secret-token\""));
            }
            other => panic!("unexpected response: {other:?}"),
        }

        server.await.expect("join server");
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn send_request_buffers_interleaved_push_messages() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let buf = read_frame(&mut stream).await;
                let _req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                let buf = read_frame(&mut stream).await;
                let req: IpcRequest = serde_json::from_slice(&buf).expect("decode get_config");
                match req {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "interleaved"),
                    other => panic!("unexpected config request: {other:?}"),
                }

                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::InboundTask {
                        source_node: "local-aiua-01".into(),
                        task_id: Uuid::nil(),
                        task_json: serde_json::json!({
                            "action": "send_reply",
                            "content": "pushed first"
                        })
                        .to_string(),
                    })
                    .unwrap(),
                )
                .await;

                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "interleaved".into(),
                        value_json: Some("\"ok\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-2".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");
        let response = client
            .send_request(IpcRequest::GetConfig {
                key: "interleaved".into(),
            })
            .await
            .expect("send request");

        match response {
            IpcResponse::ConfigData { key, value_json } => {
                assert_eq!(key, "interleaved");
                assert_eq!(value_json.as_deref(), Some("\"ok\""));
            }
            other => panic!("unexpected config response: {other:?}"),
        }

        let pushed = client.recv_task().await.expect("receive buffered push");
        match pushed {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("decode pushed task");
                assert_eq!(payload["content"], "pushed first");
            }
            other => panic!("unexpected pushed response: {other:?}"),
        }

        server.await.expect("join server");
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn recv_task_survives_select_cancellation_after_partial_frame_read() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let buf = read_frame(&mut stream).await;
                let _req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                let payload = serde_json::to_vec(&IpcResponse::InboundTask {
                    source_node: "local-aiua-01".into(),
                    task_id: Uuid::nil(),
                    task_json: serde_json::json!({
                        "action": "send_reply",
                        "content": "partial frame survives cancellation",
                    })
                    .to_string(),
                })
                .unwrap();
                let len = u32::try_from(payload.len()).expect("frame length");
                let header = len.to_be_bytes();

                stream.write_all(&header).await.expect("write frame header");
                stream
                    .write_all(&payload[..8])
                    .await
                    .expect("write partial payload");
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                stream
                    .write_all(&payload[8..])
                    .await
                    .expect("write remaining payload");

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-3".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");

        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(10)) => {}
            result = client.recv_task() => panic!("recv_task completed too early: {result:?}"),
        }

        let pushed = client.recv_task().await.expect("receive preserved push");
        match pushed {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("decode pushed task");
                assert_eq!(payload["content"], "partial frame survives cancellation");
            }
            other => panic!("unexpected pushed response: {other:?}"),
        }

        server.await.expect("join server");
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn send_request_with_timeout_discards_stale_response_before_next_reply() {
        // DEF-045 regression: request A times out client-side before the hotel's
        // reply arrives; request B is issued right after. The hotel's late reply
        // to A must be discarded, not misdelivered as B's response.
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let buf = read_frame(&mut stream).await;
                let _req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                // Request A: the client will give up on this before we reply.
                let buf = read_frame(&mut stream).await;
                match serde_json::from_slice::<IpcRequest>(&buf).expect("decode request A") {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "stale-a"),
                    other => panic!("unexpected request A: {other:?}"),
                }

                // Delay well past the client's timeout, then send the late reply.
                tokio::time::sleep(Duration::from_millis(150)).await;
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "stale-a".into(),
                        value_json: Some("\"A\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                // Request B was written by the client right after its timeout fired.
                let buf = read_frame(&mut stream).await;
                match serde_json::from_slice::<IpcRequest>(&buf).expect("decode request B") {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "fresh-b"),
                    other => panic!("unexpected request B: {other:?}"),
                }
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "fresh-b".into(),
                        value_json: Some("\"B\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-stale".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");

        let timed_out = client
            .send_request_with_timeout(
                IpcRequest::GetConfig {
                    key: "stale-a".into(),
                },
                Duration::from_millis(30),
            )
            .await;
        assert!(
            timed_out.is_err(),
            "expected request A to time out client-side, got {timed_out:?}"
        );
        assert_eq!(client.pending_stale_responses, 1);

        let response_b = client
            .send_request(IpcRequest::GetConfig {
                key: "fresh-b".into(),
            })
            .await
            .expect("send request B");

        match response_b {
            IpcResponse::ConfigData { key, value_json } => {
                assert_eq!(key, "fresh-b");
                assert_eq!(value_json.as_deref(), Some("\"B\""));
            }
            other => panic!("expected fresh-b response (stale-a must be discarded), got {other:?}"),
        }
        assert_eq!(
            client.pending_stale_responses, 0,
            "stale counter must be drained once the late reply is discarded"
        );

        server.await.expect("join server");
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn stale_response_discard_does_not_lose_interleaved_push() {
        // DEF-045 regression: a push arriving between the discarded stale reply
        // and the real reply must still surface via recv_task, not be dropped.
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let buf = read_frame(&mut stream).await;
                let _req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                let buf = read_frame(&mut stream).await;
                match serde_json::from_slice::<IpcRequest>(&buf).expect("decode request A") {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "stale-a"),
                    other => panic!("unexpected request A: {other:?}"),
                }

                tokio::time::sleep(Duration::from_millis(150)).await;

                // Stale reply to A, then a push, then (once it arrives) the real
                // reply to B — push sits between the stale frame and the real one.
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "stale-a".into(),
                        value_json: Some("\"A\"".into()),
                    })
                    .unwrap(),
                )
                .await;
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::InboundTask {
                        source_node: "local-aiua-01".into(),
                        task_id: Uuid::nil(),
                        task_json: serde_json::json!({
                            "action": "send_reply",
                            "content": "push between stale and real"
                        })
                        .to_string(),
                    })
                    .unwrap(),
                )
                .await;

                let buf = read_frame(&mut stream).await;
                match serde_json::from_slice::<IpcRequest>(&buf).expect("decode request B") {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "fresh-b"),
                    other => panic!("unexpected request B: {other:?}"),
                }
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "fresh-b".into(),
                        value_json: Some("\"B\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-stale-push".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");

        let timed_out = client
            .send_request_with_timeout(
                IpcRequest::GetConfig {
                    key: "stale-a".into(),
                },
                Duration::from_millis(30),
            )
            .await;
        assert!(timed_out.is_err(), "expected request A to time out");

        let response_b = client
            .send_request(IpcRequest::GetConfig {
                key: "fresh-b".into(),
            })
            .await
            .expect("send request B");
        match response_b {
            IpcResponse::ConfigData { key, .. } => assert_eq!(key, "fresh-b"),
            other => panic!("expected fresh-b response, got {other:?}"),
        }

        let pushed = client
            .recv_task()
            .await
            .expect("interleaved push must not be lost");
        match pushed {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("decode pushed task");
                assert_eq!(payload["content"], "push between stale and real");
            }
            other => panic!("unexpected pushed response: {other:?}"),
        }

        server.await.expect("join server");
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn ignorable_oob_broadcast_during_stale_window_is_not_mistaken_for_the_owed_reply() {
        // DEF-045 regression: an OOB broadcast (lease status ping, MuninnStatus,
        // NetworkState, ...) is NOT a push and NOT the owed stale reply, but it can
        // land on this connection at any time — including while a stale reply is
        // still outstanding. If it were allowed to burn the pending_stale_responses
        // credit, the *real* stale reply arriving right after it would fall through
        // unrecognized and get misdelivered as B's response — reintroducing the
        // exact desync this counter exists to prevent.
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let buf = read_frame(&mut stream).await;
                let _req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                let buf = read_frame(&mut stream).await;
                match serde_json::from_slice::<IpcRequest>(&buf).expect("decode request A") {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "stale-a"),
                    other => panic!("unexpected request A: {other:?}"),
                }

                tokio::time::sleep(Duration::from_millis(150)).await;

                // OOB broadcast lands BEFORE the real stale reply.
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::TelegramPollLeaseStatus {
                        active: true,
                        lease: None,
                    })
                    .unwrap(),
                )
                .await;
                // The real stale reply to A.
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "stale-a".into(),
                        value_json: Some("\"A\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                let buf = read_frame(&mut stream).await;
                match serde_json::from_slice::<IpcRequest>(&buf).expect("decode request B") {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "fresh-b"),
                    other => panic!("unexpected request B: {other:?}"),
                }
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "fresh-b".into(),
                        value_json: Some("\"B\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-stale-oob".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");

        let timed_out = client
            .send_request_with_timeout(
                IpcRequest::GetConfig {
                    key: "stale-a".into(),
                },
                Duration::from_millis(30),
            )
            .await;
        assert!(timed_out.is_err(), "expected request A to time out");

        let response_b = client
            .send_request(IpcRequest::GetConfig {
                key: "fresh-b".into(),
            })
            .await
            .expect("send request B");
        match response_b {
            IpcResponse::ConfigData { key, value_json } => {
                assert_eq!(key, "fresh-b");
                assert_eq!(value_json.as_deref(), Some("\"B\""));
            }
            other => panic!(
                "expected fresh-b response (OOB broadcast must not consume the stale credit), got {other:?}"
            ),
        }
        assert_eq!(
            client.pending_stale_responses, 0,
            "stale counter must be drained by the real stale reply, not the OOB broadcast"
        );

        server.await.expect("join server");
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn send_request_with_timeout_behaves_like_send_request_on_prompt_reply() {
        // Normal (non-timeout) path: send_request_with_timeout must return the
        // real reply and leave no stale-response bookkeeping behind, exactly
        // like plain send_request.
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let buf = read_frame(&mut stream).await;
                let _req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                let buf = read_frame(&mut stream).await;
                match serde_json::from_slice::<IpcRequest>(&buf).expect("decode request") {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "prompt"),
                    other => panic!("unexpected request: {other:?}"),
                }
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "prompt".into(),
                        value_json: Some("\"ok\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-prompt".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");

        let response = client
            .send_request_with_timeout(
                IpcRequest::GetConfig {
                    key: "prompt".into(),
                },
                Duration::from_secs(5),
            )
            .await
            .expect("send request with ample timeout");

        match response {
            IpcResponse::ConfigData { key, value_json } => {
                assert_eq!(key, "prompt");
                assert_eq!(value_json.as_deref(), Some("\"ok\""));
            }
            other => panic!("unexpected response: {other:?}"),
        }
        assert_eq!(client.pending_stale_responses, 0);

        server.await.expect("join server");
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[test]
    fn disconnect_detection_matches_unexpected_eof() {
        let err = anyhow::Error::new(std::io::Error::from(ErrorKind::UnexpectedEof));
        assert!(is_ipc_disconnect(&err));
    }

    #[tokio::test]
    async fn is_ipc_timeout_distinguishes_elapse_from_other_errors() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let buf = read_frame(&mut stream).await;
                let _req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;
                // Never respond to the next request — the client's timeout must elapse.
                tokio::time::sleep(Duration::from_secs(5)).await;
                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }
        let identity = GuestIdentity {
            guest_id: "guest-test-timeout-kind".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");

        let err = client
            .send_request_with_timeout(
                IpcRequest::GetConfig {
                    key: "never".into(),
                },
                Duration::from_millis(20),
            )
            .await
            .expect_err("expected timeout error");
        assert!(
            is_ipc_timeout(&err),
            "expected an is_ipc_timeout error, got {err:?}"
        );

        let other = anyhow::anyhow!("some unrelated failure");
        assert!(!is_ipc_timeout(&other));

        server.abort();
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[test]
    fn return_route_reads_typed_route_before_compat_fields() {
        let task = serde_json::json!({
            "reply_to": "compat-node",
            "reply_role": "agent",
            "reply_guest_id": "compat-guest",
            "session_id": "session-1",
            "turn_id": "turn-1",
            "return_route": {
                "node": "typed-node",
                "role": "agent",
                "guest_id": "typed-guest",
                "session_id": "typed-session",
                "turn_id": "typed-turn",
                "correlation_id": "corr-1"
            }
        });

        let route = ReturnRoute::from_task(&task, "default-node", "default-role");
        assert_eq!(route.node, "typed-node");
        assert_eq!(route.role, "agent");
        assert_eq!(route.guest_id.as_deref(), Some("typed-guest"));
        assert_eq!(route.session_id.as_deref(), Some("typed-session"));
        assert_eq!(route.turn_id.as_deref(), Some("typed-turn"));
        assert_eq!(route.correlation_id.as_deref(), Some("corr-1"));
    }

    #[test]
    fn return_route_reads_compat_fields_and_agent_fallback() {
        let task = serde_json::json!({
            "reply_to": "compat-node",
            "reply_role": "agent",
            "agent_id": "agent-jane",
            "session_id": "session-1",
            "turn_id": "turn-1"
        });

        let route = ReturnRoute::from_task(&task, "default-node", "default-role");
        assert_eq!(route.node, "compat-node");
        assert_eq!(route.role, "agent");
        assert_eq!(route.guest_id.as_deref(), Some("agent-jane"));
        assert_eq!(route.session_id.as_deref(), Some("session-1"));
        assert_eq!(route.turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn return_route_reply_guest_id_beats_agent_id_fallback() {
        // A role-incarnation philote (e.g. a whisper specialist running as a
        // separate `{agent_id}:{role_name}` process) sets `reply_guest_id` to its
        // own incarnation id. It MUST win over the bare `agent_id` fallback so the
        // model response returns to the incarnation process — not the base agent,
        // where it would be dropped and the specialist's turn would hang.
        let task = serde_json::json!({
            "reply_to": "node-1",
            "reply_role": "agent",
            "reply_guest_id": "agent-bjork-01:theoretician",
            "agent_id": "agent-bjork-01"
        });

        let route = ReturnRoute::from_task(&task, "default-node", "default-role");
        assert_eq!(route.role, "agent");
        assert_eq!(
            route.guest_id.as_deref(),
            Some("agent-bjork-01:theoretician"),
            "reply_guest_id (incarnation) must beat the agent_id base fallback"
        );
    }

    #[test]
    fn return_route_agent_id_fallback_is_agent_role_only() {
        let task = serde_json::json!({
            "reply_to": "compat-node",
            "reply_role": "membrane",
            "agent_id": "agent-jane"
        });

        let route = ReturnRoute::from_task(&task, "default-node", "default-role");
        assert_eq!(route.node, "compat-node");
        assert_eq!(route.role, "membrane");
        assert_eq!(route.guest_id, None);
    }
    #[test]
    fn close_heal_work_item_request_serde_round_trip() {
        // F8: appended at the END of IpcRequest, responds with Standard only —
        // no new IpcResponse variant, so the untagged-ordering invariant holds.
        let req = IpcRequest::CloseHealWorkItem {
            work_item_id: "wi-42".into(),
        };
        let wire = serde_json::to_string(&req).expect("serialize");
        assert!(wire.contains("\"close_heal_work_item\""), "wire: {wire}");
        let back: IpcRequest = serde_json::from_str(&wire).expect("deserialize");
        match back {
            IpcRequest::CloseHealWorkItem { work_item_id } => {
                assert_eq!(work_item_id, "wi-42");
            }
            other => panic!("expected CloseHealWorkItem, got {other:?}"),
        }

        // The close response is a plain Standard ack carrying {closed, work_item_id}.
        let resp = serde_json::json!({
            "ok": true,
            "code": "OK",
            "message": "Success",
            "corr_id": "close_heal_work_item",
            "data": { "closed": true, "work_item_id": "wi-42" }
        });
        let back: IpcResponse = serde_json::from_value(resp).expect("response");
        match back {
            IpcResponse::Standard { ok: true, data, .. } => {
                let data = data.expect("data");
                assert_eq!(data["closed"], true);
                assert_eq!(data["work_item_id"], "wi-42");
            }
            other => panic!("expected Standard, got {other:?}"),
        }
    }

    #[test]
    fn file_heal_work_item_request_serde_round_trip() {
        // New variant appended at the END of IpcRequest (externally-order-safe:
        // the enum is internally tagged by "operation", so existing wire shapes
        // are untouched; the END placement is a repo convention).
        let req = IpcRequest::FileHealWorkItem {
            pattern_tag: "connection_refused".into(),
            guest_id: "membrane-telegram-01".into(),
            occurrence_count: 5,
            window_secs: 1800,
            evidence_lines: vec!["connection refused".into(), "econnrefused".into()],
        };
        let wire = serde_json::to_string(&req).expect("serialize");
        assert!(wire.contains("\"file_heal_work_item\""), "wire: {wire}");
        let back: IpcRequest = serde_json::from_str(&wire).expect("deserialize");
        match back {
            IpcRequest::FileHealWorkItem {
                pattern_tag,
                guest_id,
                occurrence_count,
                window_secs,
                evidence_lines,
            } => {
                assert_eq!(pattern_tag, "connection_refused");
                assert_eq!(guest_id, "membrane-telegram-01");
                assert_eq!(occurrence_count, 5);
                assert_eq!(window_secs, 1800);
                assert_eq!(evidence_lines.len(), 2);
            }
            other => panic!("expected FileHealWorkItem, got {other:?}"),
        }
    }

    #[test]
    fn file_heal_work_item_request_back_compat() {
        // A sender built before evidence_lines existed (field omitted) must
        // still deserialize — the field defaults to empty.
        let wire = serde_json::json!({
            "operation": "file_heal_work_item",
            "payload": {
                "pattern_tag": "panic",
                "guest_id": "philote-01",
                "occurrence_count": 7,
                "window_secs": 900,
            }
        });
        let back: IpcRequest = serde_json::from_value(wire).expect("deserialize");
        match back {
            IpcRequest::FileHealWorkItem { evidence_lines, .. } => {
                assert!(evidence_lines.is_empty());
            }
            other => panic!("expected FileHealWorkItem, got {other:?}"),
        }

        // Pre-existing request shapes are unaffected by the appended variant.
        let old = serde_json::json!({
            "operation": "get_heal_queue_pending",
            "payload": { "limit": 5 }
        });
        let back: IpcRequest = serde_json::from_value(old).expect("old request");
        assert!(matches!(back, IpcRequest::GetHealQueuePending { limit: 5 }));

        // The filing response is a plain Standard ack (no new IpcResponse
        // variant), so the untagged-ordering invariant is untouched: a
        // Standard payload with data must still parse as Standard.
        let resp = serde_json::json!({
            "ok": true,
            "code": "OK",
            "message": "Success",
            "corr_id": "file_heal_work_item",
            "data": { "filed": true, "deduped": false, "work_item_id": "wi-1" }
        });
        let back: IpcResponse = serde_json::from_value(resp).expect("response");
        match back {
            IpcResponse::Standard { ok: true, data, .. } => {
                let data = data.expect("data");
                assert_eq!(data["filed"], true);
                assert_eq!(data["work_item_id"], "wi-1");
            }
            other => panic!("expected Standard, got {other:?}"),
        }
    }

    #[test]
    fn autonomy_action_requests_serde_round_trip() {
        // Slice A2 variants live at the END of IpcRequest and respond with
        // Standard only — no new IpcResponse variant, so the untagged-ordering
        // invariant (all-optional variants like MemoryConfig stay last) holds.
        let req = IpcRequest::ConsumeAutonomyAction {
            lane: "graph.bridge_edges".into(),
            action_summary: "bridge 2 RELATES_TO edge(s)".into(),
            evidence: "feedback_id=feedback:recall:1 rating=Disconnected".into(),
            reversal_hint: "MATCH ()-[r:RELATES_TO {feedback_signal_id: 'f1'}]-() DELETE r".into(),
        };
        let wire = serde_json::to_string(&req).expect("serialize");
        assert!(wire.contains("\"consume_autonomy_action\""), "wire: {wire}");
        let back: IpcRequest = serde_json::from_str(&wire).expect("deserialize");
        match back {
            IpcRequest::ConsumeAutonomyAction {
                lane,
                action_summary,
                evidence,
                reversal_hint,
            } => {
                assert_eq!(lane, "graph.bridge_edges");
                assert!(action_summary.contains("RELATES_TO"));
                assert!(evidence.contains("Disconnected"));
                assert!(reversal_hint.contains("DELETE r"));
            }
            other => panic!("expected ConsumeAutonomyAction, got {other:?}"),
        }

        let req = IpcRequest::RecordAutonomyOutcome {
            audit_id: "autonomy:graph.bridge_edges:abc".into(),
            outcome: "confirmed_good".into(),
        };
        let wire = serde_json::to_string(&req).expect("serialize");
        assert!(wire.contains("\"record_autonomy_outcome\""), "wire: {wire}");
        let back: IpcRequest = serde_json::from_str(&wire).expect("deserialize");
        match back {
            IpcRequest::RecordAutonomyOutcome { audit_id, outcome } => {
                assert_eq!(audit_id, "autonomy:graph.bridge_edges:abc");
                assert_eq!(outcome, "confirmed_good");
            }
            other => panic!("expected RecordAutonomyOutcome, got {other:?}"),
        }

        // Pre-existing request shapes are unaffected by the appended variants.
        let old = serde_json::json!({
            "operation": "file_heal_work_item",
            "payload": {
                "pattern_tag": "panic",
                "guest_id": "philote-01",
                "occurrence_count": 1,
                "window_secs": 900,
            }
        });
        let back: IpcRequest = serde_json::from_value(old).expect("old request");
        assert!(matches!(back, IpcRequest::FileHealWorkItem { .. }));

        // The consult response is a plain Standard ack carrying the decision.
        let resp = serde_json::json!({
            "ok": true,
            "code": "OK",
            "message": "Success",
            "corr_id": "consume_autonomy_action",
            "data": {
                "allowed": false,
                "posture": "confirm_first",
                "audit_id": "autonomy:graph.bridge_edges:abc"
            }
        });
        let back: IpcResponse = serde_json::from_value(resp).expect("response");
        match back {
            IpcResponse::Standard { ok: true, data, .. } => {
                let data = data.expect("data");
                assert_eq!(data["allowed"], false);
                assert_eq!(data["posture"], "confirm_first");
            }
            other => panic!("expected Standard, got {other:?}"),
        }
    }

    #[test]
    fn push_heal_event_request_serde_round_trip_and_back_compat() {
        // PushHealEvent lives at the END of IpcRequest and responds with
        // Standard only — no new IpcResponse variant, so the untagged-ordering
        // invariant (all-optional variants like MemoryConfig stay last) holds.
        let req = IpcRequest::PushHealEvent {
            guest_id: "agent-jane-01".into(),
            severity: "medium".into(),
            pattern_tag: "stuck_turn_evicted:WaitingTool".into(),
            detail: "Turn watchdog evicted stuck turn after 91s in WaitingTool.".into(),
        };
        let wire = serde_json::to_string(&req).expect("serialize");
        assert!(wire.contains("\"push_heal_event\""), "wire: {wire}");
        let back: IpcRequest = serde_json::from_str(&wire).expect("deserialize");
        match back {
            IpcRequest::PushHealEvent {
                guest_id,
                severity,
                pattern_tag,
                detail,
            } => {
                assert_eq!(guest_id, "agent-jane-01");
                assert_eq!(severity, "medium");
                assert_eq!(pattern_tag, "stuck_turn_evicted:WaitingTool");
                assert!(detail.contains("watchdog"));
            }
            other => panic!("expected PushHealEvent, got {other:?}"),
        }

        // Pre-existing request shapes are unaffected by the appended variant.
        let old = serde_json::json!({
            "operation": "push_heal_entry",
            "payload": { "guest_id": "g-1", "raw_text": "boom" }
        });
        let back: IpcRequest = serde_json::from_value(old).expect("old request");
        assert!(matches!(back, IpcRequest::PushHealEntry { .. }));

        let old = serde_json::json!({
            "operation": "fail_task",
            "payload": {
                "task_id": "5f2a1a1e-0000-0000-0000-000000000001",
                "error_code": "MODEL_EMPTY_RESPONSE",
                "reason": "Model failed",
            }
        });
        let back: IpcRequest = serde_json::from_value(old).expect("old fail_task");
        assert!(matches!(back, IpcRequest::FailTask { .. }));

        // The push response is a plain Standard ack.
        let resp = serde_json::json!({
            "ok": true,
            "code": "OK",
            "message": "Success",
            "corr_id": "push_heal_event",
            "data": { "collapsed": false, "id": "hq-1" }
        });
        let back: IpcResponse = serde_json::from_value(resp).expect("response");
        match back {
            IpcResponse::Standard { ok: true, data, .. } => {
                assert_eq!(data.expect("data")["collapsed"], false);
            }
            other => panic!("expected Standard, got {other:?}"),
        }
    }
}
