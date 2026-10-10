//! Synthetic two-revision fixture; never starts a guest, reads a DB, or calls a provider.
use agent_core::r#loop::{ToolCall, ToolResult};
use agent_core::session::{RecalledMemoryRecord, SessionState};
use serde_json::{Value, json};
use std::io::{self, Read};

fn main() {
    let mode = std::env::args().nth(1).expect("emit or restore");
    if mode == "restore" {
        let mut raw = String::new();
        io::stdin().read_to_string(&mut raw).unwrap();
        assert!(raw.len() <= 200_000);
        let checkpoint: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(checkpoint["session_id"], "synthetic-rollout");
        let state = SessionState::from_checkpoint(&checkpoint).unwrap();
        println!("{}", json!({"checkpoint": state.checkpoint_json()}));
        return;
    }
    assert_eq!(mode, "emit");
    let size: usize = std::env::args().nth(2).unwrap().parse().unwrap();
    assert!(size <= 20_000);
    let checkpoint = json!({
        "session_id": "synthetic-rollout", "agent_id": "synthetic-agent", "source": "synthetic",
        "fallback_override": ["model.openrouter", "model.ollama"],
        "context_request_limits": {"input_tokens":512, "output_tokens":256},
        "active_turn": {"turn_id":"synthetic-turn", "task_id":"00000000-0000-0000-0000-000000000001",
                        "phase":"waiting_tool", "user_content":"SYNTHETIC_CURRENT_MESSAGE"}
    });
    let mut state = SessionState::from_checkpoint(&checkpoint).unwrap();
    state.agent_profile.identity_text = Some("SYNTHETIC_MANDATORY_IDENTITY".into());
    state.agent_profile.user_principal_id = Some("synthetic-principal".into());
    state.agent_profile.agent_role_names = vec!["SYNTHETIC_MANDATORY_ROLE".into()];
    state.agent_graph_snapshot = Some("SYNTHETIC_AGENT_GRAPH".into());
    state.settings.context_window.max_tool_result_chars = 1000;
    let turn = state.active_turn.as_mut().unwrap();
    turn.recalled_memories = vec![RecalledMemoryRecord {
        id: Some("synthetic-recall".into()), concept: "synthetic".into(),
        content: "SYNTHETIC_UNATTESTED_RECALL".into(), ..Default::default()
    }];
    if size > 0 {
        turn.working_tool_history.push((
            ToolCall {tool_name:"synthetic.read".into(), arguments:json!({"fixture":true})},
            ToolResult {tool_name:"synthetic.read".into(), content:format!("{}SYNTHETIC_RESULT_TAIL", "x".repeat(size))}
        ));
    }
    let (prompt, context, projection) = state.model_request_payloads("SYNTHETIC_CURRENT_MESSAGE", &[]);
    println!("{}", json!({
        "task": {"action":"generate_text", "request_class":"cognitive", "session_id":"synthetic-rollout",
                 "turn_id":"synthetic-turn", "prompt":prompt, "context":context,
                 "context_projection":projection,
                 "provider_options":{"context_limits":{"input_tokens":512,"output_tokens":256}}},
        "checkpoint":state.checkpoint_json()
    }));
}
