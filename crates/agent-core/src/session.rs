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

#[derive(Debug, Clone)]
pub struct SessionState {
    pub session_id: String,
    pub agent_id: String,
    pub source: String,
    pub approval_policy: ApprovalPolicy,
    pub recent_turns: Vec<TurnRecord>,
    pub active_turn: Option<WorkingTurn>,
}

impl SessionState {
    pub fn new(session_id: String, agent_id: String, source: String) -> Self {
        Self {
            session_id,
            agent_id,
            source,
            approval_policy: ApprovalPolicy::default(),
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

    pub fn build_prompt(&self, user_content: &str) -> String {
        let mut prompt = String::from(
            "You are Jane, a hyper-intelligent Hegemon AI. Be concise, helpful, and context-aware.\n",
        );

        if !self.recent_turns.is_empty() {
            prompt.push_str("\n[Recent session context]\n");
            for turn in &self.recent_turns {
                prompt.push_str(&format!("User: {}\n", turn.user_content));
                if let Some(reply) = &turn.assistant_content {
                    prompt.push_str(&format!("Assistant: {}\n", reply));
                }
            }
        }

        prompt.push_str("\n[Approval policy]\n");
        if self.approval_policy.auto_approve_all {
            prompt.push_str(
                "This session is pre-approved. Do not ask for approval for actions in this session unless the action is explicitly forbidden.\n",
            );
        } else if !self.approval_policy.preapproved_tools.is_empty()
            || !self.approval_policy.preapproved_classes.is_empty()
        {
            if !self.approval_policy.preapproved_tools.is_empty() {
                prompt.push_str(&format!(
                    "Pre-approved tools: {}.\n",
                    self.approval_policy.preapproved_tools.join(", ")
                ));
            }
            if !self.approval_policy.preapproved_classes.is_empty() {
                prompt.push_str(&format!(
                    "Pre-approved classes: {}.\n",
                    self.approval_policy.preapproved_classes.join(", ")
                ));
            }
            prompt.push_str("Do not request approval for pre-approved actions.\n");
        } else {
            prompt.push_str(
                "No pre-approvals are configured. Request approval before side-effecting actions.\n",
            );
        }

        prompt.push_str("\n[Current user message]\n");
        prompt.push_str(user_content);
        prompt
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
            "approval_policy": self.approval_policy,
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
        let approval_policy = checkpoint
            .get("approval_policy")
            .cloned()
            .and_then(|value| serde_json::from_value::<ApprovalPolicy>(value).ok())
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
            approval_policy,
            recent_turns,
            active_turn,
        })
    }
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
        entry.get("session_id")
            .and_then(serde_json::Value::as_str)
            != Some(state.session_id.as_str())
    });
    sessions.push(state.checkpoint_index_entry());
    sessions.sort_by(|a, b| {
        let a_ts = a.get("updated_at").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let b_ts = b.get("updated_at").and_then(serde_json::Value::as_u64).unwrap_or(0);
        b_ts.cmp(&a_ts)
    });
    sessions.truncate(32);

    json!({
        "agent_id": state.agent_id,
        "active_sessions": sessions,
    })
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
        merge_session_index, session_checkpoint_memory_type, ApprovalPolicy, SessionState,
        WorkingTurn,
    };
    use crate::r#loop::{ApprovalRequest, ToolCall, TurnPhase};
    use uuid::Uuid;

    #[test]
    fn checkpoint_contains_active_turn_and_history() {
        let mut state = SessionState::new(
            "sess-1".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "hello".into(),
            final_reply_to: "local-ansible-01".into(),
            final_reply_role: "hegemon".into(),
            phase: TurnPhase::Queued,
            iteration: 0,
            pending_tool_call: None,
            pending_approval: None,
        });

        let checkpoint = state.checkpoint_json();
        assert_eq!(checkpoint["session_id"], "sess-1");
        assert_eq!(checkpoint["active_turn"]["turn_id"], "turn-1");
        assert_eq!(checkpoint["active_turn"]["phase"], "queued");
    }

    #[test]
    fn completing_turn_rolls_into_recent_history() {
        let mut state = SessionState::new(
            "sess-1".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "hello".into(),
            final_reply_to: "local-ansible-01".into(),
            final_reply_role: "hegemon".into(),
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
            "approval_policy": {
                "auto_approve_all": true
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
        assert_eq!(
            state.approval_policy,
            ApprovalPolicy {
                auto_approve_all: true,
                preapproved_tools: Vec::new(),
                preapproved_classes: Vec::new(),
            }
        );
        assert_eq!(state.recent_turns.len(), 1);
        assert_eq!(state.active_turn.as_ref().unwrap().turn_id, "turn-2");
        assert_eq!(state.active_turn.as_ref().unwrap().phase, TurnPhase::WaitingModel);
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
    }

    #[test]
    fn approval_policy_can_auto_approve_session_requests() {
        let mut state = SessionState::new(
            "sess-1".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );
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
        let mut state = SessionState::new(
            "sess-1".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );
        state.set_preapprove_this_session();

        let prompt = state.build_prompt("deploy the thing");
        assert!(prompt.contains("This session is pre-approved."));
    }

    #[test]
    fn status_text_reports_when_no_preapproval_exists() {
        let state = SessionState::new(
            "sess-1".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );
        assert_eq!(
            state.approval_policy_status_text(),
            "Approval policy: no pre-approvals configured."
        );
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
        let mut first = SessionState::new(
            "sess-1".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );
        first.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "hello".into(),
            final_reply_to: "local-ansible-01".into(),
            final_reply_role: "hegemon".into(),
            phase: TurnPhase::Queued,
            iteration: 0,
            pending_tool_call: None,
            pending_approval: None,
        });
        let index = merge_session_index(None, &first);
        assert_eq!(index["active_sessions"].as_array().unwrap().len(), 1);

        let second = SessionState::new(
            "sess-2".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );
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
