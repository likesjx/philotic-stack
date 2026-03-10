use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnPhase {
    Queued,
    LoadingContext,
    WaitingModel,
    Thinking,
    WaitingTool,
    WaitingApproval,
    WaitingVoice,
    Completed,
    Failed,
}

impl TurnPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::LoadingContext => "loading_context",
            Self::WaitingModel => "waiting_model",
            Self::Thinking => "thinking",
            Self::WaitingTool => "waiting_tool",
            Self::WaitingApproval => "waiting_approval",
            Self::WaitingVoice => "waiting_voice",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "loading_context" => Self::LoadingContext,
            "waiting_model" => Self::WaitingModel,
            "thinking" => Self::Thinking,
            "waiting_tool" => Self::WaitingTool,
            "waiting_approval" => Self::WaitingApproval,
            "waiting_voice" => Self::WaitingVoice,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Queued,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentAction {
    Respond { content: String },
    ToolCall(ToolCall),
    RequestApproval(ApprovalRequest),
    Fail { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    #[serde(default)]
    pub approval_id: Option<String>,
    pub reason: String,
    pub approved_response: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_name: String,
    pub content: String,
}

pub fn interpret_model_payload(agent_action: Option<&Value>, content: Option<&str>) -> AgentAction {
    if let Some(agent_action) = agent_action {
        if let Some(kind) = agent_action.get("kind").and_then(Value::as_str) {
            match kind {
                "respond" => {
                    if let Some(content) = agent_action.get("content").and_then(Value::as_str) {
                        return AgentAction::Respond {
                            content: content.to_string(),
                        };
                    }
                }
                "tool_call" => {
                    if let Some(tool_name) = agent_action.get("tool_name").and_then(Value::as_str) {
                        return AgentAction::ToolCall(ToolCall {
                            tool_name: tool_name.to_string(),
                            arguments: agent_action
                                .get("arguments")
                                .cloned()
                                .unwrap_or_else(|| serde_json::json!({})),
                        });
                    }
                }
                "request_approval" => {
                    return AgentAction::RequestApproval(ApprovalRequest {
                        approval_id: agent_action
                            .get("approval_id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        reason: agent_action
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("Approval required")
                            .to_string(),
                        approved_response: agent_action
                            .get("approved_response")
                            .and_then(Value::as_str)
                            .unwrap_or("Approved.")
                            .to_string(),
                    });
                }
                "fail" => {
                    return AgentAction::Fail {
                        message: agent_action
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("Model reported failure")
                            .to_string(),
                    };
                }
                _ => {}
            }
        }
    }

    interpret_model_output(content.unwrap_or_default())
}

pub fn interpret_model_output(content: &str) -> AgentAction {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return AgentAction::Fail {
            message: "Model returned an empty response".into(),
        };
    }

    if let Some(path) = trimmed.strip_prefix("TOOL:echo ") {
        return AgentAction::ToolCall(ToolCall {
            tool_name: "echo".into(),
            arguments: serde_json::json!({
                "text": path.trim(),
            }),
        });
    }

    AgentAction::Respond {
        content: trimmed.to_string(),
    }
}

pub fn interpret_tool_result(result: &ToolResult) -> AgentAction {
    AgentAction::Respond {
        content: format!("Tool {} says: {}", result.tool_name, result.content),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentAction, ApprovalRequest, ToolCall, ToolResult, TurnPhase, interpret_model_output,
        interpret_model_payload, interpret_tool_result,
    };

    #[test]
    fn empty_model_output_becomes_fail_action() {
        assert_eq!(
            interpret_model_output("   "),
            AgentAction::Fail {
                message: "Model returned an empty response".into()
            }
        );
    }

    #[test]
    fn non_empty_model_output_becomes_respond_action() {
        assert_eq!(
            interpret_model_output("hello"),
            AgentAction::Respond {
                content: "hello".into()
            }
        );
    }

    #[test]
    fn tool_directive_becomes_tool_call_action() {
        assert_eq!(
            interpret_model_output("TOOL:echo hello"),
            AgentAction::ToolCall(ToolCall {
                tool_name: "echo".into(),
                arguments: serde_json::json!({ "text": "hello" }),
            })
        );
    }

    #[test]
    fn structured_payload_becomes_tool_call_action() {
        assert_eq!(
            interpret_model_payload(
                Some(&serde_json::json!({
                    "kind": "tool_call",
                    "tool_name": "echo",
                    "arguments": { "text": "hello" }
                })),
                None
            ),
            AgentAction::ToolCall(ToolCall {
                tool_name: "echo".into(),
                arguments: serde_json::json!({ "text": "hello" }),
            })
        );
    }

    #[test]
    fn structured_payload_becomes_request_approval_action() {
        assert_eq!(
            interpret_model_payload(
                Some(&serde_json::json!({
                    "kind": "request_approval",
                    "reason": "Need confirmation",
                    "approved_response": "Confirmed"
                })),
                None
            ),
            AgentAction::RequestApproval(ApprovalRequest {
                approval_id: None,
                reason: "Need confirmation".into(),
                approved_response: "Confirmed".into(),
            })
        );
    }

    #[test]
    fn tool_result_becomes_follow_up_response() {
        assert_eq!(
            interpret_tool_result(&ToolResult {
                tool_name: "echo".into(),
                content: "hello".into(),
            }),
            AgentAction::Respond {
                content: "Tool echo says: hello".into(),
            }
        );
    }

    #[test]
    fn turn_phase_round_trips_with_strings() {
        assert_eq!(TurnPhase::from_str("thinking"), TurnPhase::Thinking);
        assert_eq!(TurnPhase::WaitingModel.as_str(), "waiting_model");
        assert_eq!(TurnPhase::WaitingTool.as_str(), "waiting_tool");
        assert_eq!(TurnPhase::WaitingApproval.as_str(), "waiting_approval");
    }
}
