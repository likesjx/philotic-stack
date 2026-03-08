use crate::session::ToolDefinition;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct InboundTaskPayload {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub agent_action: Option<serde_json::Value>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub arguments: Option<serde_json::Value>,
    #[serde(default)]
    pub final_reply_to: Option<String>,
    #[serde(default)]
    pub final_reply_role: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelRequestPayload {
    pub action: &'static str,
    pub session_id: String,
    pub turn_id: String,
    pub prompt: String,
    pub user_content: String,
    pub tools_for_model: Vec<ToolDefinition>,
    pub chat_id: String,
    pub reply_to: String,
    pub reply_role: String,
    pub final_reply_to: String,
    pub final_reply_role: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FinalReplyPayload {
    pub action: &'static str,
    pub session_id: String,
    pub turn_id: String,
    pub chat_id: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolExecutionPayload {
    pub action: &'static str,
    pub session_id: String,
    pub turn_id: String,
    pub chat_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub execution_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incarnation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotel_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_ref: Option<String>,
    pub reply_to: String,
    pub reply_role: String,
    pub final_reply_to: String,
    pub final_reply_role: String,
}

impl InboundTaskPayload {
    pub fn is_model_response(&self) -> bool {
        self.action.as_deref() == Some("model_response")
    }

    pub fn is_tool_result(&self) -> bool {
        self.action.as_deref() == Some("tool_result")
    }

    pub fn session_id_or_default(&self, agent_id: &str) -> String {
        if let Some(session_id) = self.session_id.as_ref().filter(|s| !s.is_empty()) {
            return session_id.clone();
        }

        let source = self.source.as_deref().unwrap_or("unknown");
        let chat_id = self.chat_id.as_deref().unwrap_or("ephemeral");
        format!("{source}:{chat_id}:{agent_id}")
    }
}

#[cfg(test)]
mod tests {
    use super::InboundTaskPayload;

    #[test]
    fn session_id_defaults_from_source_and_chat() {
        let payload = InboundTaskPayload {
            action: None,
            agent_action: None,
            source: Some("telegram".into()),
            session_id: None,
            turn_id: None,
            chat_id: Some("1234".into()),
            content: Some("hello".into()),
            tool_name: None,
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
        };

        assert_eq!(
            payload.session_id_or_default("agent-jane-01"),
            "telegram:1234:agent-jane-01"
        );
    }

    #[test]
    fn model_response_detection_is_action_based() {
        let payload = InboundTaskPayload {
            action: Some("model_response".into()),
            agent_action: None,
            source: None,
            session_id: None,
            turn_id: None,
            chat_id: None,
            content: None,
            tool_name: None,
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
        };

        assert!(payload.is_model_response());
    }

    #[test]
    fn tool_result_detection_is_action_based() {
        let payload = InboundTaskPayload {
            action: Some("tool_result".into()),
            agent_action: None,
            source: None,
            session_id: None,
            turn_id: None,
            chat_id: None,
            content: None,
            tool_name: Some("echo".into()),
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
        };

        assert!(payload.is_tool_result());
    }
}
