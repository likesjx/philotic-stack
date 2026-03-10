use crate::r#loop::{ApprovalRequest, ToolCall, TurnPhase};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TurnRecord {
    pub turn_id: String,
    pub user_content: String,
    pub assistant_content: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkingTurn {
    pub task_id: Uuid,
    pub turn_id: String,
    pub chat_id: String,
    pub user_content: String,
    pub final_reply_to: String,
    pub final_reply_role: String,
    pub final_reply_guest_id: Option<String>,
    pub phase: TurnPhase,
    pub iteration: u32,
    pub pending_tool_call: Option<ToolCall>,
    pub pending_approval: Option<ApprovalRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ApprovalPolicy {
    #[serde(default)]
    pub auto_approve_all: bool,
    #[serde(default)]
    pub preapproved_tools: Vec<String>,
    #[serde(default)]
    pub preapproved_classes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentProfile {
    #[serde(default)]
    pub persona_name: Option<String>,
    #[serde(default)]
    pub soul_text: Option<String>,
    #[serde(default)]
    pub identity_text: Option<String>,
    #[serde(default)]
    pub user_context_text: Option<String>,
    #[serde(default)]
    pub agents_text: Option<String>,
    #[serde(default)]
    pub memory_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionBindings {
    #[serde(default)]
    pub effective_toolset: Vec<String>,
    #[serde(default)]
    pub effective_skillset: Vec<String>,
    #[serde(default)]
    pub effective_workspace_ref: Option<String>,
    #[serde(default)]
    pub workspace_runner_config: Option<TaskRunnerBaseConfig>,
    #[serde(default)]
    pub transport_reply_target: Option<TransportReplyTargetBinding>,
    #[serde(default)]
    pub component_routes: Vec<ComponentRouteBinding>,
    #[serde(default)]
    pub effective_model_controller: Option<String>,
    #[serde(default)]
    pub preferred_tool_runner_incarnation: Option<String>,
    #[serde(default)]
    pub preferred_tool_runner: Option<String>,
    #[serde(default)]
    pub preferred_hotel_id: Option<String>,
    #[serde(default)]
    pub preferred_environment_id: Option<String>,
    #[serde(default)]
    pub allowed_tool_runner_incarnations: Vec<ToolRunnerIncarnationBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskRunnerBaseConfig {
    #[serde(default)]
    pub default_workspace_ref: Option<String>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub max_read_bytes: Option<usize>,
    #[serde(default)]
    pub max_search_results: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TransportReplyTargetBinding {
    pub target_node: String,
    pub target_role: String,
    #[serde(default)]
    pub target_guest_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ComponentRouteBinding {
    pub capability: String,
    #[serde(default = "default_selection_mode")]
    pub selection_mode: String,
    #[serde(default)]
    pub implementation: Option<String>,
    #[serde(default)]
    pub incarnation: Option<String>,
    #[serde(default)]
    pub preferred_hotel_id: Option<String>,
    #[serde(default)]
    pub preferred_environment_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolDefinition {
    pub tool_name: String,
    pub description: String,
    #[serde(default)]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolExecutionRoute {
    pub target_node: String,
    pub target_role: String,
    #[serde(default)]
    pub runner_id: Option<String>,
    #[serde(default)]
    pub incarnation_id: Option<String>,
    #[serde(default)]
    pub hotel_id: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub task_runner_kind: Option<String>,
    #[serde(default)]
    pub task_runner_config: Option<TaskRunnerBaseConfig>,
    pub execution_mode: String,
    #[serde(default = "default_route_availability")]
    pub availability_state: String,
    #[serde(default)]
    pub selection_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolRunnerIncarnationBinding {
    pub incarnation_id: String,
    #[serde(default)]
    pub runner_id: Option<String>,
    #[serde(default)]
    pub hotel_id: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub target_node: Option<String>,
    #[serde(default)]
    pub target_role: Option<String>,
    #[serde(default)]
    pub supported_tools: Vec<String>,
    #[serde(default = "default_capability_execution_mode")]
    pub execution_mode: String,
    #[serde(default = "default_route_availability")]
    pub availability_state: String,
    #[serde(default)]
    pub selection_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolPolicyAnnotation {
    pub policy_class: String,
    pub approval_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolAssembly {
    #[serde(default)]
    pub tools_for_model: Vec<ToolDefinition>,
    #[serde(default)]
    pub execution_routes: std::collections::BTreeMap<String, ToolExecutionRoute>,
    #[serde(default)]
    pub policy_annotations: std::collections::BTreeMap<String, ToolPolicyAnnotation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ComponentExecutionRoute {
    pub target_node: String,
    pub target_role: String,
    #[serde(default)]
    pub incarnation_id: Option<String>,
    #[serde(default)]
    pub hotel_id: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
    pub execution_mode: String,
    #[serde(default = "default_route_availability")]
    pub availability_state: String,
    #[serde(default)]
    pub selection_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ComponentRouteAssembly {
    #[serde(default)]
    pub execution_routes: std::collections::BTreeMap<String, ComponentExecutionRoute>,
}

fn default_route_availability() -> String {
    "live".into()
}

fn default_capability_execution_mode() -> String {
    "capability".into()
}

fn default_selection_mode() -> String {
    "preferred".into()
}

#[derive(Debug, Clone)]
pub struct SessionState {
    pub session_id: String,
    pub agent_id: String,
    pub source: String,
    pub agent_profile: AgentProfile,
    pub status: String,
    pub approval_policy: ApprovalPolicy,
    pub bindings: SessionBindings,
    pub component_route_assembly: ComponentRouteAssembly,
    pub tool_assembly: ToolAssembly,
    pub recent_turns: Vec<TurnRecord>,
    pub active_turn: Option<WorkingTurn>,
}

impl SessionState {
    pub fn new(session_id: String, agent_id: String, source: String) -> Self {
        let bindings = SessionBindings::default();
        Self {
            session_id,
            agent_id,
            source,
            agent_profile: AgentProfile::default(),
            status: "active".into(),
            approval_policy: ApprovalPolicy::default(),
            tool_assembly: default_tool_assembly_for_bindings(&bindings),
            component_route_assembly: ComponentRouteAssembly::default(),
            bindings,
            recent_turns: Vec::new(),
            active_turn: None,
        }
    }

    pub fn start_turn(&mut self, turn: WorkingTurn) {
        self.active_turn = Some(turn);
    }

    pub fn set_active_turn_phase(&mut self, phase: TurnPhase) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.phase = phase;
        }
    }

    pub fn bump_active_turn_iteration(&mut self) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.iteration += 1;
        }
    }

    pub fn set_pending_tool_call(&mut self, tool_call: ToolCall) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.pending_tool_call = Some(tool_call);
        }
    }

    pub fn clear_pending_tool_call(&mut self) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.pending_tool_call = None;
        }
    }

    pub fn set_pending_approval(&mut self, approval: ApprovalRequest) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.pending_approval = Some(approval);
        }
    }

    pub fn clear_pending_approval(&mut self) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.pending_approval = None;
        }
    }

    pub fn complete_active_turn(&mut self, assistant_content: String) -> Option<WorkingTurn> {
        let turn = self.active_turn.take()?;
        self.recent_turns.push(TurnRecord {
            turn_id: turn.turn_id.clone(),
            user_content: turn.user_content.clone(),
            assistant_content: Some(assistant_content),
        });
        if self.recent_turns.len() > 8 {
            let drain = self.recent_turns.len() - 8;
            self.recent_turns.drain(0..drain);
        }
        Some(turn)
    }

    pub fn approval_policy_allows(&self, _approval: &ApprovalRequest) -> bool {
        self.approval_policy.auto_approve_all
    }

    pub fn set_preapprove_this_session(&mut self) {
        self.approval_policy.auto_approve_all = true;
    }

    pub fn reset_approval_policy(&mut self) {
        self.approval_policy = ApprovalPolicy::default();
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    pub fn add_tool_binding(&mut self, tool: impl Into<String>) {
        let tool = tool.into();
        if !self
            .bindings
            .effective_toolset
            .iter()
            .any(|existing| existing == &tool)
        {
            self.bindings.effective_toolset.push(tool);
            self.bindings.effective_toolset.sort();
            self.rebuild_default_tool_assembly();
        }
    }

    pub fn clear_tool_bindings(&mut self) {
        self.bindings.effective_toolset.clear();
        self.rebuild_default_tool_assembly();
    }

    pub fn add_skill_binding(&mut self, skill: impl Into<String>) {
        let skill = skill.into();
        if !self
            .bindings
            .effective_skillset
            .iter()
            .any(|existing| existing == &skill)
        {
            self.bindings.effective_skillset.push(skill);
            self.bindings.effective_skillset.sort();
        }
    }

    pub fn clear_skill_bindings(&mut self) {
        self.bindings.effective_skillset.clear();
    }

    pub fn set_workspace_binding(&mut self, workspace: impl Into<String>) {
        self.bindings.effective_workspace_ref = Some(workspace.into());
    }

    pub fn clear_workspace_binding(&mut self) {
        self.bindings.effective_workspace_ref = None;
    }

    pub fn set_transport_reply_target(
        &mut self,
        target_node: impl Into<String>,
        target_role: impl Into<String>,
        target_guest_id: Option<String>,
    ) {
        self.bindings.transport_reply_target = Some(TransportReplyTargetBinding {
            target_node: target_node.into(),
            target_role: target_role.into(),
            target_guest_id,
        });
    }

    pub fn transport_reply_target(&self) -> Option<&TransportReplyTargetBinding> {
        self.bindings.transport_reply_target.as_ref()
    }

    pub fn resolved_transport_reply_target(
        &self,
        fallback_node: impl Into<String>,
        fallback_role: impl Into<String>,
        fallback_guest_id: Option<String>,
    ) -> TransportReplyTargetBinding {
        self.transport_reply_target()
            .cloned()
            .unwrap_or_else(|| TransportReplyTargetBinding {
                target_node: fallback_node.into(),
                target_role: fallback_role.into(),
                target_guest_id: fallback_guest_id,
            })
    }

    pub fn component_route_for_capability(
        &self,
        capability: &str,
    ) -> Option<&ComponentRouteBinding> {
        self.bindings
            .component_routes
            .iter()
            .find(|route| route.capability == capability)
    }

    pub fn preferred_component_implementation(&self, capability: &str) -> Option<&str> {
        self.component_route_for_capability(capability)
            .and_then(|route| route.implementation.as_deref())
            .or_else(|| {
                if capability == "text.generate" {
                    self.bindings.effective_model_controller.as_deref()
                } else {
                    None
                }
            })
    }

    pub fn resolve_component_execution_route(
        &self,
        capability: &str,
    ) -> Option<&ComponentExecutionRoute> {
        self.component_route_assembly
            .execution_routes
            .get(capability)
    }

    pub fn component_route_summary(&self) -> Option<String> {
        if !self.bindings.component_routes.is_empty() {
            return Some(
                self.bindings
                    .component_routes
                    .iter()
                    .map(|route| {
                        let mut line =
                            format!("{} [{}]", route.capability, route.selection_mode.as_str());
                        if let Some(implementation) = route.implementation.as_deref() {
                            line.push_str(&format!(" impl={implementation}"));
                        }
                        if let Some(incarnation) = route.incarnation.as_deref() {
                            line.push_str(&format!(" inc={incarnation}"));
                        }
                        if let Some(hotel_id) = route.preferred_hotel_id.as_deref() {
                            line.push_str(&format!(" hotel={hotel_id}"));
                        }
                        if let Some(environment_id) = route.preferred_environment_id.as_deref() {
                            line.push_str(&format!(" env={environment_id}"));
                        }
                        line
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
            );
        }

        self.bindings
            .effective_model_controller
            .as_deref()
            .map(|controller| format!("text.generate [legacy] impl={controller}"))
    }

    pub fn tool_is_enabled(&self, tool_name: &str) -> bool {
        if self
            .tool_assembly
            .tools_for_model
            .iter()
            .any(|tool| tool.tool_name == tool_name)
        {
            return true;
        }

        if self.tool_assembly.execution_routes.contains_key(tool_name) {
            return true;
        }

        self.bindings.effective_toolset.is_empty()
            || self
                .bindings
                .effective_toolset
                .iter()
                .any(|allowed| allowed == tool_name)
    }

    pub fn resolve_tool_route(&self, tool_name: &str) -> Option<&ToolExecutionRoute> {
        self.tool_assembly.execution_routes.get(tool_name)
    }

    pub fn rebuild_default_tool_assembly(&mut self) {
        self.tool_assembly = default_tool_assembly_for_bindings(&self.bindings);
    }

    pub fn approval_policy_status_text(&self) -> String {
        if self.approval_policy.auto_approve_all {
            return "Approval policy: pre-approved for this session.".into();
        }

        let mut parts = Vec::new();
        if !self.approval_policy.preapproved_tools.is_empty() {
            parts.push(format!(
                "tools={}",
                self.approval_policy.preapproved_tools.join(", ")
            ));
        }
        if !self.approval_policy.preapproved_classes.is_empty() {
            parts.push(format!(
                "classes={}",
                self.approval_policy.preapproved_classes.join(", ")
            ));
        }

        if parts.is_empty() {
            "Approval policy: no pre-approvals configured.".into()
        } else {
            format!("Approval policy: {}.", parts.join(" | "))
        }
    }

    pub fn session_status_text(&self) -> String {
        let active_turn = self
            .active_turn
            .as_ref()
            .map(|turn| format!("active turn {} ({})", turn.turn_id, turn.phase.as_str()))
            .unwrap_or_else(|| "no active turn".into());
        let toolset = if self.bindings.effective_toolset.is_empty() {
            "default".into()
        } else {
            self.bindings.effective_toolset.join(", ")
        };
        let skillset = if self.bindings.effective_skillset.is_empty() {
            "default".into()
        } else {
            self.bindings.effective_skillset.join(", ")
        };
        let workspace = self
            .bindings
            .effective_workspace_ref
            .clone()
            .unwrap_or_else(|| "default".into());
        let routing = self
            .component_route_summary()
            .unwrap_or_else(|| "default".into());
        let delivery = self
            .transport_reply_target()
            .map(|target| {
                let mut text = format!("{} / {}", target.target_node, target.target_role);
                if let Some(guest_id) = target.target_guest_id.as_deref() {
                    text.push_str(&format!(" guest={guest_id}"));
                }
                text
            })
            .unwrap_or_else(|| "unbound".into());

        format!(
            "Session status: {}. {}. Toolset: {}. Skillset: {}. Workspace: {}. Component routes: {}. Delivery target: {}.",
            self.status, active_turn, toolset, skillset, workspace, routing, delivery
        )
    }

    pub fn project_tools_for_turn(&self, user_content: &str) -> Vec<ToolDefinition> {
        let all_tools = self.tool_assembly.tools_for_model.clone();
        if all_tools.is_empty() {
            return all_tools;
        }

        let normalized = user_content.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return all_tools;
        }

        if normalized.starts_with("/status")
            || normalized.starts_with("/pause")
            || normalized.starts_with("/resume")
        {
            let projected = all_tools
                .iter()
                .filter(|tool| tool.tool_name == "session.status")
                .cloned()
                .collect::<Vec<_>>();
            return if projected.is_empty() {
                all_tools
            } else {
                projected
            };
        }

        let explicitly_named = all_tools
            .iter()
            .filter(|tool| tool_name_matches_goal(&tool.tool_name, &normalized))
            .cloned()
            .collect::<Vec<_>>();
        if !explicitly_named.is_empty() {
            return explicitly_named;
        }

        if looks_like_conversational_goal(&normalized) {
            return Vec::new();
        }

        all_tools
    }

    pub fn build_prompt(&self, user_content: &str) -> String {
        let projected_tools = self.project_tools_for_turn(user_content);
        self.build_prompt_with_tools(user_content, &projected_tools)
    }

    pub fn build_prompt_with_tools(
        &self,
        user_content: &str,
        projected_tools: &[ToolDefinition],
    ) -> String {
        let mut prompt = String::new();
        prompt.push_str("\n[Agent self projection]\n");
        prompt.push_str(&self.project_agent_self());

        prompt.push_str("\n\n[User projection]\n");
        prompt.push_str(&self.project_user(user_content));

        prompt.push_str("\n\n[Knowledge projection]\n");
        prompt.push_str(&self.project_knowledge(user_content, projected_tools));

        prompt.push_str("\n[Current user message]\n");
        prompt.push_str(user_content);
        prompt
    }

    pub fn project_agent_self(&self) -> String {
        let mut lines = Vec::new();

        if let Some(identity) = self
            .agent_profile
            .identity_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            lines.push(identity.to_string());
        } else {
            lines.push("You are Jane, a hyper-intelligent Hegemon AI.".to_string());
        }

        if let Some(soul) = self
            .agent_profile
            .soul_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            lines.push(soul.to_string());
        } else {
            lines.push(
                "Be concise, helpful, context-aware, and willing to push back when it improves the work."
                    .to_string(),
            );
        }

        if !self.bindings.effective_skillset.is_empty() {
            lines.push(format!(
                "Current skill posture: {}.",
                self.bindings.effective_skillset.join(", ")
            ));
        }

        lines.join("\n")
    }

    pub fn project_user(&self, _user_content: &str) -> String {
        let mut lines = Vec::new();

        if let Some(user_context) = self
            .agent_profile
            .user_context_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            lines.push(user_context.to_string());
        } else {
            lines.push(format!(
                "You are speaking with a collaborator over {}.",
                self.source
            ));
        }

        if self.source == "telegram" {
            lines.push(
                "Keep replies compact and legible for chat, but do not flatten important tradeoffs."
                    .to_string(),
            );
        }

        if self.recent_turns.is_empty() {
            lines.push(
                "No durable user-specific profile is loaded yet; learn cautiously from the conversation."
                    .to_string(),
            );
        } else {
            lines.push(
                "Use the recent session history to preserve continuity and collaboration style."
                    .to_string(),
            );
        }

        lines.join("\n")
    }

    pub fn project_knowledge(
        &self,
        _user_content: &str,
        projected_tools: &[ToolDefinition],
    ) -> String {
        let mut sections = Vec::new();

        if !self.recent_turns.is_empty() {
            let mut recent = String::from("[Recent session context]\n");
            for turn in &self.recent_turns {
                recent.push_str(&format!("User: {}\n", turn.user_content));
                if let Some(reply) = &turn.assistant_content {
                    recent.push_str(&format!("Assistant: {}\n", reply));
                }
            }
            sections.push(recent.trim_end().to_string());
        }

        let mut policy = String::from("[Approval policy]\n");
        if self.approval_policy.auto_approve_all {
            policy.push_str(
                "This session is pre-approved. Do not ask for approval for actions in this session unless the action is explicitly forbidden.\n",
            );
        } else if !self.approval_policy.preapproved_tools.is_empty()
            || !self.approval_policy.preapproved_classes.is_empty()
        {
            if !self.approval_policy.preapproved_tools.is_empty() {
                policy.push_str(&format!(
                    "Pre-approved tools: {}.\n",
                    self.approval_policy.preapproved_tools.join(", ")
                ));
            }
            if !self.approval_policy.preapproved_classes.is_empty() {
                policy.push_str(&format!(
                    "Pre-approved classes: {}.\n",
                    self.approval_policy.preapproved_classes.join(", ")
                ));
            }
            policy.push_str("Do not request approval for pre-approved actions.\n");
        } else {
            policy.push_str(
                "No pre-approvals are configured. Request approval before side-effecting actions.\n",
            );
        }
        sections.push(policy.trim_end().to_string());

        let mut envelope = String::from("[Session envelope]\n");
        envelope.push_str(&format!("Session status: {}.\n", self.status));
        if !self.bindings.effective_toolset.is_empty() {
            envelope.push_str(&format!(
                "Effective tools: {}.\n",
                self.bindings.effective_toolset.join(", ")
            ));
        }
        if !projected_tools.is_empty() {
            let abstract_tools = projected_tools
                .iter()
                .map(|tool| tool.tool_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            envelope.push_str(&format!("Abstract tools available: {}.\n", abstract_tools));
        }
        if let Some(workspace) = &self.bindings.effective_workspace_ref {
            envelope.push_str(&format!("Workspace: {}.\n", workspace));
        }
        if let Some(target) = self.transport_reply_target() {
            envelope.push_str(&format!(
                "Delivery target: {} / {}{}.\n",
                target.target_node,
                target.target_role,
                target
                    .target_guest_id
                    .as_deref()
                    .map(|guest_id| format!(" guest={guest_id}"))
                    .unwrap_or_default()
            ));
        }
        if let Some(routes) = self.component_route_summary() {
            envelope.push_str(&format!("Component routes: {}.\n", routes));
        }
        if !self.summary_text().is_empty() {
            envelope.push_str(&format!("Recent summary: {}.\n", self.summary_text()));
        }
        sections.push(envelope.trim_end().to_string());

        if let Some(memory_summary) = self
            .agent_profile
            .memory_summary
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            sections.push(format!("[Memory seed]\n{memory_summary}"));
        }

        sections.join("\n\n")
    }

    pub fn checkpoint_json(&self) -> serde_json::Value {
        let active_turn = self.active_turn.as_ref().map(|turn| {
            json!({
                "turn_id": turn.turn_id,
                "task_id": turn.task_id.to_string(),
                "chat_id": turn.chat_id,
                "user_content": turn.user_content,
                "final_reply_to": turn.final_reply_to,
                "final_reply_role": turn.final_reply_role,
                "final_reply_guest_id": turn.final_reply_guest_id,
                "phase": turn.phase.as_str(),
                "iteration": turn.iteration,
                "pending_tool_call": turn.pending_tool_call,
                "pending_approval": turn.pending_approval,
            })
        });

        json!({
            "session_id": self.session_id,
            "agent_id": self.agent_id,
            "source": self.source,
            "agent_profile": self.agent_profile,
            "status": self.status,
            "approval_policy": self.approval_policy,
            "bindings": self.bindings,
            "component_route_assembly": self.component_route_assembly,
            "tool_assembly": self.tool_assembly,
            "active_turn": active_turn,
            "recent_turns": self.recent_turns.iter().map(|turn| {
                json!({
                    "turn_id": turn.turn_id,
                    "user_content": turn.user_content,
                    "assistant_content": turn.assistant_content,
                })
            }).collect::<Vec<_>>(),
            "summary": self.summary_text(),
        })
    }

    pub fn checkpoint_memory_type(&self) -> String {
        session_checkpoint_memory_type(&self.session_id)
    }

    pub fn checkpoint_index_entry(&self) -> serde_json::Value {
        json!({
            "session_id": self.session_id,
            "source": self.source,
            "has_active_turn": self.active_turn.is_some(),
            "updated_at": current_unix_ts(),
        })
    }

    fn summary_text(&self) -> String {
        self.recent_turns
            .iter()
            .rev()
            .take(3)
            .map(|turn| match &turn.assistant_content {
                Some(reply) => format!("{} -> {}", turn.user_content, reply),
                None => turn.user_content.clone(),
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    pub fn from_checkpoint(checkpoint: &serde_json::Value) -> Option<Self> {
        let session_id = checkpoint.get("session_id")?.as_str()?.to_string();
        let agent_id = checkpoint
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("agent-jane-01")
            .to_string();
        let source = checkpoint
            .get("source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let agent_profile = checkpoint
            .get("agent_profile")
            .cloned()
            .and_then(|value| serde_json::from_value::<AgentProfile>(value).ok())
            .unwrap_or_default();
        let status = checkpoint
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("active")
            .to_string();
        let approval_policy = checkpoint
            .get("approval_policy")
            .cloned()
            .and_then(|value| serde_json::from_value::<ApprovalPolicy>(value).ok())
            .unwrap_or_default();
        let bindings = checkpoint
            .get("bindings")
            .cloned()
            .and_then(|value| serde_json::from_value::<SessionBindings>(value).ok())
            .unwrap_or_default();
        let tool_assembly = checkpoint
            .get("tool_assembly")
            .cloned()
            .and_then(|value| serde_json::from_value::<ToolAssembly>(value).ok())
            .unwrap_or_else(|| default_tool_assembly_for_bindings(&bindings));
        let component_route_assembly = checkpoint
            .get("component_route_assembly")
            .cloned()
            .and_then(|value| serde_json::from_value::<ComponentRouteAssembly>(value).ok())
            .unwrap_or_default();

        let recent_turns = checkpoint
            .get("recent_turns")
            .and_then(serde_json::Value::as_array)
            .map(|turns| {
                turns
                    .iter()
                    .filter_map(|turn| {
                        Some(TurnRecord {
                            turn_id: turn.get("turn_id")?.as_str()?.to_string(),
                            user_content: turn.get("user_content")?.as_str()?.to_string(),
                            assistant_content: turn
                                .get("assistant_content")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let active_turn = checkpoint.get("active_turn").and_then(|turn| {
            if turn.is_null() {
                return None;
            }

            let task_id = turn
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .unwrap_or_else(Uuid::nil);

            Some(WorkingTurn {
                task_id,
                turn_id: turn.get("turn_id")?.as_str()?.to_string(),
                chat_id: turn
                    .get("chat_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                user_content: turn
                    .get("user_content")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                final_reply_to: turn
                    .get("final_reply_to")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("local-ansible-01")
                    .to_string(),
                final_reply_role: turn
                    .get("final_reply_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("hegemon")
                    .to_string(),
                final_reply_guest_id: turn
                    .get("final_reply_guest_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                phase: TurnPhase::from_str(
                    turn.get("phase")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("queued"),
                ),
                iteration: turn
                    .get("iteration")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32,
                pending_tool_call: turn
                    .get("pending_tool_call")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<ToolCall>(value).ok()),
                pending_approval: turn
                    .get("pending_approval")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<ApprovalRequest>(value).ok()),
            })
        });

        Some(Self {
            session_id,
            agent_id,
            source,
            agent_profile,
            status,
            approval_policy,
            bindings,
            component_route_assembly,
            tool_assembly,
            recent_turns,
            active_turn,
        })
    }
}

fn looks_like_conversational_goal(normalized: &str) -> bool {
    normalized.contains('?')
        || [
            "what",
            "why",
            "how",
            "who",
            "when",
            "tell me",
            "explain",
            "help me understand",
            "i think",
            "i am thinking",
            "let's think",
            "can we talk",
        ]
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
}

fn tool_name_matches_goal(tool_name: &str, normalized: &str) -> bool {
    let tool_name = tool_name.to_ascii_lowercase();
    if normalized.contains(&tool_name) {
        return true;
    }

    tool_name
        .split(['.', '_', '-'])
        .filter(|part| !part.is_empty())
        .any(|part| normalized.contains(part))
}

pub fn session_checkpoint_memory_type(session_id: &str) -> String {
    format!("short_session:{session_id}")
}

pub fn merge_session_index(
    existing_index: Option<&serde_json::Value>,
    state: &SessionState,
) -> serde_json::Value {
    let mut sessions = existing_index
        .and_then(|index| index.get("active_sessions"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    sessions.retain(|entry| {
        entry.get("session_id").and_then(serde_json::Value::as_str)
            != Some(state.session_id.as_str())
    });
    sessions.push(state.checkpoint_index_entry());
    sessions.sort_by(|a, b| {
        let a_ts = a
            .get("updated_at")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let b_ts = b
            .get("updated_at")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        b_ts.cmp(&a_ts)
    });
    sessions.truncate(32);

    json!({
        "agent_id": state.agent_id,
        "active_sessions": sessions,
    })
}

pub fn default_tool_assembly_for_bindings(bindings: &SessionBindings) -> ToolAssembly {
    if !bindings.allowed_tool_runner_incarnations.is_empty() {
        return tool_assembly_from_allowed_incarnations(bindings);
    }

    let toolset = default_visible_toolset(bindings);

    let tools_for_model = toolset
        .iter()
        .map(|tool_name| ToolDefinition {
            tool_name: tool_name.clone(),
            description: format!("Execute the {} tool.", tool_name),
            input_schema: json!({
                "type": "object"
            }),
        })
        .collect::<Vec<_>>();

    let execution_routes = toolset
        .iter()
        .map(|tool_name| {
            let execution_mode = if is_local_agent_tool(tool_name) {
                "local_agent"
            } else if is_pinned_tool(tool_name) {
                "pinned"
            } else {
                "capability"
            };
            (
                tool_name.clone(),
                ToolExecutionRoute {
                    target_node: if execution_mode == "local_agent" {
                        "agent-jane-01".into()
                    } else {
                        "local-ansible-01".into()
                    },
                    target_role: if execution_mode == "local_agent" {
                        "agent".into()
                    } else {
                        format!("tool.{tool_name}")
                    },
                    runner_id: if execution_mode == "local_agent" {
                        None
                    } else {
                        Some("tool-runner-01".into())
                    },
                    incarnation_id: None,
                    hotel_id: if execution_mode == "local_agent" {
                        None
                    } else {
                        Some("local-ansible-01".into())
                    },
                    environment_id: None,
                    task_runner_kind: task_runner_kind_for_tool(tool_name),
                    task_runner_config: task_runner_base_config_for_tool(bindings, tool_name),
                    execution_mode: execution_mode.into(),
                    availability_state: "live".into(),
                    selection_reason: Some(if execution_mode == "local_agent" {
                        "agent_local_tool".into()
                    } else if execution_mode == "pinned" {
                        "default_pinned_route".into()
                    } else {
                        "default_capability_route".into()
                    }),
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    let policy_annotations = toolset
        .iter()
        .map(|tool_name| {
            (
                tool_name.clone(),
                ToolPolicyAnnotation {
                    policy_class: format!("tool:{tool_name}"),
                    approval_required: false,
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    ToolAssembly {
        tools_for_model,
        execution_routes,
        policy_annotations,
    }
}

fn default_visible_toolset(bindings: &SessionBindings) -> Vec<String> {
    if bindings.effective_toolset.is_empty() {
        vec!["echo".to_string()]
    } else {
        bindings.effective_toolset.clone()
    }
}

fn is_local_agent_tool(tool_name: &str) -> bool {
    matches!(tool_name, "session.status")
}

fn is_pinned_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "workspace.list" | "workspace.read" | "workspace.search" | "workspace.write"
    )
}

fn task_runner_kind_for_tool(tool_name: &str) -> Option<String> {
    if tool_name.starts_with("workspace.") {
        return Some("workspace".into());
    }

    if tool_name.starts_with("shell.") {
        return Some("shell".into());
    }

    None
}

fn task_runner_base_config_for_tool(
    bindings: &SessionBindings,
    tool_name: &str,
) -> Option<TaskRunnerBaseConfig> {
    if !tool_name.starts_with("workspace.") {
        return None;
    }

    let config = bindings.workspace_runner_config.clone().unwrap_or_default();
    let default_workspace_ref = config
        .default_workspace_ref
        .or_else(|| bindings.effective_workspace_ref.clone());
    let allowed_tools = config.allowed_tools.or_else(|| {
        let workspace_tools = bindings
            .effective_toolset
            .iter()
            .filter(|tool| tool.starts_with("workspace."))
            .cloned()
            .collect::<Vec<_>>();
        if workspace_tools.is_empty() {
            None
        } else {
            Some(workspace_tools)
        }
    });

    Some(TaskRunnerBaseConfig {
        default_workspace_ref,
        allowed_tools,
        max_read_bytes: config.max_read_bytes,
        max_search_results: config.max_search_results,
    })
}

fn tool_assembly_from_allowed_incarnations(bindings: &SessionBindings) -> ToolAssembly {
    let visible_tools = if bindings.effective_toolset.is_empty() {
        bindings
            .allowed_tool_runner_incarnations
            .iter()
            .flat_map(|incarnation| incarnation.supported_tools.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        bindings.effective_toolset.clone()
    };

    let tools_for_model = visible_tools
        .iter()
        .map(|tool_name| ToolDefinition {
            tool_name: tool_name.clone(),
            description: format!("Execute the {} tool.", tool_name),
            input_schema: json!({
                "type": "object"
            }),
        })
        .collect::<Vec<_>>();

    let execution_routes = visible_tools
        .iter()
        .filter_map(|tool_name| {
            select_incarnation_route(bindings, tool_name).map(|route| (tool_name.clone(), route))
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    let policy_annotations = visible_tools
        .iter()
        .map(|tool_name| {
            (
                tool_name.clone(),
                ToolPolicyAnnotation {
                    policy_class: format!("tool:{tool_name}"),
                    approval_required: false,
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    ToolAssembly {
        tools_for_model,
        execution_routes,
        policy_annotations,
    }
}

fn select_incarnation_route(
    bindings: &SessionBindings,
    tool_name: &str,
) -> Option<ToolExecutionRoute> {
    let mut candidates = bindings
        .allowed_tool_runner_incarnations
        .iter()
        .filter(|incarnation| {
            incarnation
                .supported_tools
                .iter()
                .any(|tool| tool == tool_name)
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|a, b| compare_incarnation_bindings(bindings, a, b));
    let selected = candidates.first()?;
    let selection_reason = selection_reason_for_binding(bindings, selected);

    Some(ToolExecutionRoute {
        target_node: selected
            .target_node
            .clone()
            .or_else(|| selected.hotel_id.clone())
            .unwrap_or_else(|| "local-ansible-01".into()),
        target_role: selected
            .target_role
            .clone()
            .unwrap_or_else(|| format!("tool.{tool_name}")),
        runner_id: selected
            .runner_id
            .clone()
            .or_else(|| Some(selected.incarnation_id.clone())),
        incarnation_id: Some(selected.incarnation_id.clone()),
        hotel_id: selected.hotel_id.clone(),
        environment_id: selected.environment_id.clone(),
        task_runner_kind: task_runner_kind_for_tool(tool_name),
        task_runner_config: task_runner_base_config_for_tool(bindings, tool_name),
        execution_mode: selected.execution_mode.clone(),
        availability_state: selected.availability_state.clone(),
        selection_reason: Some(selection_reason),
    })
}

fn compare_incarnation_bindings(
    bindings: &SessionBindings,
    left: &ToolRunnerIncarnationBinding,
    right: &ToolRunnerIncarnationBinding,
) -> std::cmp::Ordering {
    binding_preference_rank(bindings, right)
        .cmp(&binding_preference_rank(bindings, left))
        .then_with(|| {
            let left_live = left.availability_state == "live";
            let right_live = right.availability_state == "live";
            right_live.cmp(&left_live)
        })
        .then_with(|| {
            let left_local = left.hotel_id.as_deref() == Some("local-ansible-01");
            let right_local = right.hotel_id.as_deref() == Some("local-ansible-01");
            right_local.cmp(&left_local)
        })
        .then_with(|| left.incarnation_id.cmp(&right.incarnation_id))
}

fn binding_preference_rank(
    bindings: &SessionBindings,
    binding: &ToolRunnerIncarnationBinding,
) -> u8 {
    if bindings.preferred_tool_runner_incarnation.as_deref()
        == Some(binding.incarnation_id.as_str())
    {
        return 4;
    }
    if bindings.preferred_tool_runner.as_deref() == binding.runner_id.as_deref() {
        return 3;
    }
    if bindings.preferred_environment_id.as_deref() == binding.environment_id.as_deref() {
        return 2;
    }
    if bindings.preferred_hotel_id.as_deref() == binding.hotel_id.as_deref() {
        return 1;
    }
    0
}

fn selection_reason_for_binding(
    bindings: &SessionBindings,
    binding: &ToolRunnerIncarnationBinding,
) -> String {
    let suffix = if binding.availability_state == "live" {
        "live"
    } else {
        "requires_materialization"
    };

    let computed = if bindings.preferred_tool_runner_incarnation.as_deref()
        == Some(binding.incarnation_id.as_str())
    {
        format!("preferred_incarnation_{suffix}")
    } else if bindings.preferred_tool_runner.as_deref() == binding.runner_id.as_deref() {
        format!("preferred_runner_{suffix}")
    } else if bindings.preferred_environment_id.as_deref() == binding.environment_id.as_deref() {
        format!("preferred_environment_{suffix}")
    } else if bindings.preferred_hotel_id.as_deref() == binding.hotel_id.as_deref() {
        format!("preferred_hotel_{suffix}")
    } else if binding.availability_state == "live"
        && binding.hotel_id.as_deref() == Some("local-ansible-01")
    {
        "live_local_fallback".into()
    } else if binding.availability_state == "live" {
        "live_allowed_incarnation".into()
    } else {
        "allowed_incarnation_requires_materialization".into()
    };

    let used_preference = binding_preference_rank(bindings, binding) > 0;
    if used_preference {
        computed
    } else {
        binding.selection_hint.clone().unwrap_or(computed)
    }
}

fn current_unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalPolicy, ComponentExecutionRoute, ComponentRouteAssembly, ComponentRouteBinding,
        SessionBindings, SessionState, TaskRunnerBaseConfig, ToolRunnerIncarnationBinding,
        TransportReplyTargetBinding, WorkingTurn, merge_session_index,
        session_checkpoint_memory_type,
    };
    use crate::r#loop::{ApprovalRequest, ToolCall, TurnPhase};
    use uuid::Uuid;

    #[test]
    fn checkpoint_contains_active_turn_and_history() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "hello".into(),
            final_reply_to: "local-ansible-01".into(),
            final_reply_role: "hegemon".into(),
            final_reply_guest_id: Some("hegemon-telegram-01".into()),
            phase: TurnPhase::Queued,
            iteration: 0,
            pending_tool_call: None,
            pending_approval: None,
        });

        let checkpoint = state.checkpoint_json();
        assert_eq!(checkpoint["session_id"], "sess-1");
        assert_eq!(checkpoint["active_turn"]["turn_id"], "turn-1");
        assert_eq!(checkpoint["active_turn"]["phase"], "queued");
        assert_eq!(
            checkpoint["active_turn"]["final_reply_guest_id"],
            "hegemon-telegram-01"
        );
        assert!(checkpoint["component_route_assembly"].is_object());
        assert!(checkpoint["tool_assembly"].is_object());
    }

    #[test]
    fn checkpoint_round_trip_preserves_component_route_assembly() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.component_route_assembly = ComponentRouteAssembly {
            execution_routes: std::collections::BTreeMap::from([(
                "text.generate".into(),
                ComponentExecutionRoute {
                    target_node: "aria-node".into(),
                    target_role: "model.gemini".into(),
                    incarnation_id: Some("aria-architect-hotel:model-controller-gemini".into()),
                    hotel_id: Some("aria-architect-hotel".into()),
                    environment_id: None,
                    execution_mode: "capability".into(),
                    availability_state: "live".into(),
                    selection_reason: Some("remote_latency_capacity".into()),
                },
            )]),
        };

        let checkpoint = state.checkpoint_json();
        let restored =
            SessionState::from_checkpoint(&checkpoint).expect("checkpoint should restore");
        let route = restored
            .resolve_component_execution_route("text.generate")
            .expect("component route should restore");
        assert_eq!(route.target_node, "aria-node");
        assert_eq!(
            route.incarnation_id.as_deref(),
            Some("aria-architect-hotel:model-controller-gemini")
        );
    }

    #[test]
    fn completing_turn_rolls_into_recent_history() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "hello".into(),
            final_reply_to: "local-ansible-01".into(),
            final_reply_role: "hegemon".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::Queued,
            iteration: 0,
            pending_tool_call: None,
            pending_approval: None,
        });

        state.complete_active_turn("hi".into());
        let checkpoint = state.checkpoint_json();
        assert!(checkpoint["active_turn"].is_null());
        assert_eq!(checkpoint["recent_turns"][0]["assistant_content"], "hi");
    }

    #[test]
    fn state_rehydrates_from_checkpoint() {
        let checkpoint = serde_json::json!({
            "session_id": "sess-1",
            "agent_id": "agent-jane-01",
            "source": "telegram",
            "status": "paused",
            "approval_policy": {
                "auto_approve_all": true
            },
            "bindings": {
                "effective_toolset": ["echo", "workspace.read"],
                "effective_skillset": ["planning"],
                "effective_workspace_ref": "workspace://main",
                "transport_reply_target": {
                    "target_node": "local-ansible-01",
                    "target_role": "hegemon",
                    "target_guest_id": "hegemon-telegram-01"
                },
                "effective_model_controller": "gemini-flash"
            },
            "active_turn": {
                "turn_id": "turn-2",
                "task_id": Uuid::nil().to_string(),
                "chat_id": "123",
                "user_content": "status?",
                "final_reply_to": "local-ansible-01",
                "final_reply_role": "hegemon",
                "phase": "waiting_model",
                "iteration": 1,
                "pending_tool_call": {
                    "tool_name": "echo",
                    "arguments": { "text": "hello" }
                },
                "pending_approval": {
                    "approval_id": "appr-1",
                    "reason": "Need confirmation",
                    "approved_response": "Confirmed"
                }
            },
            "recent_turns": [{
                "turn_id": "turn-1",
                "user_content": "hello",
                "assistant_content": "hi"
            }]
        });

        let state = SessionState::from_checkpoint(&checkpoint).expect("rehydrate state");
        assert_eq!(state.session_id, "sess-1");
        assert_eq!(state.status, "paused");
        assert_eq!(
            state.approval_policy,
            ApprovalPolicy {
                auto_approve_all: true,
                preapproved_tools: Vec::new(),
                preapproved_classes: Vec::new(),
            }
        );
        assert_eq!(
            state.bindings,
            SessionBindings {
                effective_toolset: vec!["echo".into(), "workspace.read".into()],
                effective_skillset: vec!["planning".into()],
                effective_workspace_ref: Some("workspace://main".into()),
                transport_reply_target: Some(TransportReplyTargetBinding {
                    target_node: "local-ansible-01".into(),
                    target_role: "hegemon".into(),
                    target_guest_id: Some("hegemon-telegram-01".into()),
                }),
                component_routes: Vec::new(),
                effective_model_controller: Some("gemini-flash".into()),
                workspace_runner_config: None,
                preferred_tool_runner_incarnation: None,
                preferred_tool_runner: None,
                preferred_hotel_id: None,
                preferred_environment_id: None,
                allowed_tool_runner_incarnations: Vec::new(),
            }
        );
        assert_eq!(state.recent_turns.len(), 1);
        assert_eq!(state.active_turn.as_ref().unwrap().turn_id, "turn-2");
        assert_eq!(
            state.active_turn.as_ref().unwrap().phase,
            TurnPhase::WaitingModel
        );
        assert_eq!(
            state.active_turn.as_ref().unwrap().pending_tool_call,
            Some(ToolCall {
                tool_name: "echo".into(),
                arguments: serde_json::json!({ "text": "hello" }),
            })
        );
        assert_eq!(
            state.active_turn.as_ref().unwrap().pending_approval,
            Some(ApprovalRequest {
                approval_id: Some("appr-1".into()),
                reason: "Need confirmation".into(),
                approved_response: "Confirmed".into(),
            })
        );
        assert_eq!(state.tool_assembly.tools_for_model[0].tool_name, "echo");
        assert_eq!(
            state
                .tool_assembly
                .execution_routes
                .get("echo")
                .and_then(|route| route.runner_id.as_deref()),
            Some("tool-runner-01")
        );
    }

    #[test]
    fn approval_policy_can_auto_approve_session_requests() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.approval_policy = ApprovalPolicy {
            auto_approve_all: true,
            preapproved_tools: Vec::new(),
            preapproved_classes: Vec::new(),
        };

        assert!(state.approval_policy_allows(&ApprovalRequest {
            approval_id: Some("appr-2".into()),
            reason: "deploy the thing".into(),
            approved_response: "Approved: deploy the thing".into(),
        }));
    }

    #[test]
    fn prompt_reflects_session_preapproval() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.set_preapprove_this_session();

        let prompt = state.build_prompt("deploy the thing");
        assert!(prompt.contains("This session is pre-approved."));
    }

    #[test]
    fn prompt_reflects_session_bindings_and_status() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.status = "paused".into();
        state.bindings = SessionBindings {
            effective_toolset: vec!["echo".into()],
            effective_skillset: vec!["planning".into()],
            effective_workspace_ref: Some("workspace://main".into()),
            transport_reply_target: Some(TransportReplyTargetBinding {
                target_node: "local-ansible-01".into(),
                target_role: "hegemon".into(),
                target_guest_id: Some("hegemon-telegram-01".into()),
            }),
            component_routes: Vec::new(),
            effective_model_controller: Some("gemini-flash".into()),
            workspace_runner_config: None,
            preferred_tool_runner_incarnation: None,
            preferred_tool_runner: None,
            preferred_hotel_id: None,
            preferred_environment_id: None,
            allowed_tool_runner_incarnations: Vec::new(),
        };

        let prompt = state.build_prompt("status");
        assert!(prompt.contains("Session status: paused."));
        assert!(prompt.contains("Effective tools: echo."));
        assert!(prompt.contains("Workspace: workspace://main."));
        assert!(
            prompt
                .contains("Delivery target: local-ansible-01 / hegemon guest=hegemon-telegram-01.")
        );
    }

    #[test]
    fn prompt_uses_agent_profile_sources_when_present() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.agent_profile.identity_text = Some("Identity anchor: Jane".into());
        state.agent_profile.soul_text = Some("Soul anchor: sharp, warm, witty.".into());
        state.agent_profile.user_context_text =
            Some("User anchor: Jared prefers direct collaboration.".into());
        state.agent_profile.agents_text = Some("Workspace rule: read the soul first.".into());
        state.agent_profile.memory_summary = Some("Memory seed: architecture matters.".into());

        let prompt = state.build_prompt("status");
        assert!(prompt.contains("Identity anchor: Jane"));
        assert!(prompt.contains("Soul anchor: sharp, warm, witty."));
        assert!(prompt.contains("User anchor: Jared prefers direct collaboration."));
        assert!(prompt.contains("Memory seed: architecture matters."));
    }

    #[test]
    fn status_text_reports_when_no_preapproval_exists() {
        let state = SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        assert_eq!(
            state.approval_policy_status_text(),
            "Approval policy: no pre-approvals configured."
        );
    }

    #[test]
    fn session_status_text_reports_bindings() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.status = "paused".into();
        state.bindings.effective_toolset = vec!["echo".into()];
        state.bindings.effective_skillset = vec!["planning".into()];
        state.bindings.effective_workspace_ref = Some("workspace://main".into());
        state.bindings.effective_model_controller = Some("gemini-flash".into());
        state.bindings.transport_reply_target = Some(TransportReplyTargetBinding {
            target_node: "local-ansible-01".into(),
            target_role: "hegemon".into(),
            target_guest_id: Some("hegemon-telegram-01".into()),
        });

        let text = state.session_status_text();
        assert!(text.contains("Session status: paused."));
        assert!(text.contains("Toolset: echo."));
        assert!(text.contains("Workspace: workspace://main."));
        assert!(text.contains("Component routes: text.generate [legacy] impl=gemini-flash."));
        assert!(
            text.contains("Delivery target: local-ansible-01 / hegemon guest=hegemon-telegram-01.")
        );
    }

    #[test]
    fn component_route_summary_prefers_structured_routes() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.bindings.component_routes.push(ComponentRouteBinding {
            capability: "text.generate".into(),
            selection_mode: "preferred".into(),
            implementation: Some("gemini".into()),
            incarnation: Some("model-controller-gemini-01".into()),
            preferred_hotel_id: Some("local-ansible-01".into()),
            preferred_environment_id: Some("env://local".into()),
        });

        let summary = state.component_route_summary().expect("route summary");
        assert!(summary.contains("text.generate [preferred]"));
        assert!(summary.contains("impl=gemini"));
        assert!(summary.contains("inc=model-controller-gemini-01"));
    }

    #[test]
    fn resolved_transport_reply_target_prefers_session_binding() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.set_transport_reply_target(
            "local-ansible-01",
            "hegemon",
            Some("hegemon-telegram-01".into()),
        );

        let target = state.resolved_transport_reply_target(
            "fallback-node",
            "fallback-role",
            Some("fallback-guest".into()),
        );

        assert_eq!(target.target_node, "local-ansible-01");
        assert_eq!(target.target_role, "hegemon");
        assert_eq!(
            target.target_guest_id.as_deref(),
            Some("hegemon-telegram-01")
        );
    }

    #[test]
    fn tool_binding_gates_enabled_tools() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        assert!(state.tool_is_enabled("echo"));
        assert_eq!(
            state
                .resolve_tool_route("echo")
                .map(|route| route.target_role.as_str()),
            Some("tool.echo")
        );

        state.add_tool_binding("echo");
        assert!(state.tool_is_enabled("echo"));
        assert!(!state.tool_is_enabled("workspace.read"));
        assert_eq!(
            state
                .resolve_tool_route("echo")
                .map(|route| route.target_role.as_str()),
            Some("tool.echo")
        );
    }

    #[test]
    fn allowed_incarnations_define_visible_tools_and_routes() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.bindings.allowed_tool_runner_incarnations = vec![
            ToolRunnerIncarnationBinding {
                incarnation_id: "tool-echo-remote".into(),
                runner_id: Some("tool-runner-remote".into()),
                hotel_id: Some("remote-hotel".into()),
                environment_id: Some("env://remote".into()),
                target_node: Some("remote-hotel".into()),
                target_role: Some("tool.echo".into()),
                supported_tools: vec!["echo".into()],
                execution_mode: "capability".into(),
                availability_state: "materialization_required".into(),
                selection_hint: Some("remote_fallback".into()),
            },
            ToolRunnerIncarnationBinding {
                incarnation_id: "tool-echo-local".into(),
                runner_id: Some("tool-runner-local".into()),
                hotel_id: Some("local-ansible-01".into()),
                environment_id: Some("env://local".into()),
                target_node: Some("local-ansible-01".into()),
                target_role: Some("tool.echo".into()),
                supported_tools: vec!["echo".into()],
                execution_mode: "capability".into(),
                availability_state: "live".into(),
                selection_hint: Some("local_live_preferred".into()),
            },
        ];
        state.rebuild_default_tool_assembly();

        assert!(state.tool_is_enabled("echo"));
        let route = state
            .resolve_tool_route("echo")
            .expect("echo route should be assembled");
        assert_eq!(route.incarnation_id.as_deref(), Some("tool-echo-local"));
        assert_eq!(route.hotel_id.as_deref(), Some("local-ansible-01"));
        assert_eq!(
            route.selection_reason.as_deref(),
            Some("local_live_preferred")
        );
    }

    #[test]
    fn preferred_environment_overrides_live_local_fallback() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.bindings.preferred_environment_id = Some("env://remote".into());
        state.bindings.allowed_tool_runner_incarnations = vec![
            ToolRunnerIncarnationBinding {
                incarnation_id: "tool-echo-local".into(),
                runner_id: Some("tool-runner-local".into()),
                hotel_id: Some("local-ansible-01".into()),
                environment_id: Some("env://local".into()),
                target_node: Some("local-ansible-01".into()),
                target_role: Some("tool.echo".into()),
                supported_tools: vec!["echo".into()],
                execution_mode: "capability".into(),
                availability_state: "live".into(),
                selection_hint: Some("local_live_preferred".into()),
            },
            ToolRunnerIncarnationBinding {
                incarnation_id: "tool-echo-remote".into(),
                runner_id: Some("tool-runner-remote".into()),
                hotel_id: Some("remote-hotel".into()),
                environment_id: Some("env://remote".into()),
                target_node: Some("remote-hotel".into()),
                target_role: Some("tool.echo".into()),
                supported_tools: vec!["echo".into()],
                execution_mode: "capability".into(),
                availability_state: "materialization_required".into(),
                selection_hint: Some("remote_fallback".into()),
            },
        ];
        state.rebuild_default_tool_assembly();

        let route = state
            .resolve_tool_route("echo")
            .expect("echo route should be assembled");
        assert_eq!(route.incarnation_id.as_deref(), Some("tool-echo-remote"));
        assert_eq!(route.environment_id.as_deref(), Some("env://remote"));
        assert_eq!(
            route.selection_reason.as_deref(),
            Some("preferred_environment_requires_materialization")
        );
    }

    #[test]
    fn local_agent_tools_get_local_execution_routes() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.clear_tool_bindings();
        state.add_tool_binding("session.status");

        let route = state
            .resolve_tool_route("session.status")
            .expect("local agent tool should have a route");
        assert_eq!(route.execution_mode, "local_agent");
        assert_eq!(route.target_role, "agent");
    }

    #[test]
    fn workspace_tools_get_pinned_execution_routes() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.clear_tool_bindings();
        state.add_tool_binding("workspace.read");
        state.add_tool_binding("workspace.list");
        state.add_tool_binding("workspace.search");
        state.bindings.effective_workspace_ref = Some("workspace://main".into());
        state.bindings.workspace_runner_config = Some(TaskRunnerBaseConfig {
            default_workspace_ref: Some("workspace://policy".into()),
            allowed_tools: Some(vec!["workspace.read".into(), "workspace.search".into()]),
            max_read_bytes: Some(8192),
            max_search_results: Some(25),
        });
        state.rebuild_default_tool_assembly();

        let read_route = state
            .resolve_tool_route("workspace.read")
            .expect("workspace.read route should exist");
        let list_route = state
            .resolve_tool_route("workspace.list")
            .expect("workspace.list route should exist");
        let search_route = state
            .resolve_tool_route("workspace.search")
            .expect("workspace.search route should exist");

        assert_eq!(read_route.execution_mode, "pinned");
        assert_eq!(list_route.execution_mode, "pinned");
        assert_eq!(search_route.execution_mode, "pinned");
        assert_eq!(read_route.task_runner_kind.as_deref(), Some("workspace"));
        assert_eq!(list_route.task_runner_kind.as_deref(), Some("workspace"));
        assert_eq!(search_route.task_runner_kind.as_deref(), Some("workspace"));
        assert_eq!(
            read_route
                .task_runner_config
                .as_ref()
                .and_then(|config| config.default_workspace_ref.as_deref()),
            Some("workspace://policy")
        );
        assert_eq!(
            read_route
                .task_runner_config
                .as_ref()
                .and_then(|config| config.max_read_bytes),
            Some(8192)
        );
        assert_eq!(read_route.target_role, "tool.workspace.read");
        assert_eq!(list_route.target_role, "tool.workspace.list");
        assert_eq!(search_route.target_role, "tool.workspace.search");
    }

    #[test]
    fn conversational_turns_project_no_tools_by_default() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("echo");

        let projected = state.project_tools_for_turn("What do you think about this architecture?");
        assert!(projected.is_empty());
    }

    #[test]
    fn explicit_tool_mentions_project_matching_tools() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.clear_tool_bindings();
        state.add_tool_binding("echo");
        state.add_tool_binding("session.status");

        let projected = state.project_tools_for_turn("use echo hello there");
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].tool_name, "echo");
    }

    #[test]
    fn skill_and_workspace_bindings_can_be_mutated() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_skill_binding("planning");
        state.set_workspace_binding("workspace://main");
        assert_eq!(state.bindings.effective_skillset, vec!["planning"]);
        assert_eq!(
            state.bindings.effective_workspace_ref.as_deref(),
            Some("workspace://main")
        );

        state.clear_skill_bindings();
        state.clear_workspace_binding();
        assert!(state.bindings.effective_skillset.is_empty());
        assert!(state.bindings.effective_workspace_ref.is_none());
    }

    #[test]
    fn checkpoint_memory_type_is_session_scoped() {
        assert_eq!(
            session_checkpoint_memory_type("telegram:123:agent-jane-01"),
            "short_session:telegram:123:agent-jane-01"
        );
    }

    #[test]
    fn session_index_tracks_multiple_sessions_without_duplicates() {
        let mut first =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        first.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "hello".into(),
            final_reply_to: "local-ansible-01".into(),
            final_reply_role: "hegemon".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::Queued,
            iteration: 0,
            pending_tool_call: None,
            pending_approval: None,
        });
        let index = merge_session_index(None, &first);
        assert_eq!(index["active_sessions"].as_array().unwrap().len(), 1);

        let second = SessionState::new("sess-2".into(), "agent-jane-01".into(), "telegram".into());
        let index = merge_session_index(Some(&index), &second);
        assert_eq!(index["active_sessions"].as_array().unwrap().len(), 2);

        let index = merge_session_index(Some(&index), &first);
        let sessions = index["active_sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        let session_ids = sessions
            .iter()
            .filter_map(|entry| entry.get("session_id").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(session_ids.iter().filter(|id| **id == "sess-1").count(), 1);
        assert_eq!(session_ids.iter().filter(|id| **id == "sess-2").count(), 1);
    }
}
