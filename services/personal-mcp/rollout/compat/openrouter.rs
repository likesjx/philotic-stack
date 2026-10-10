//! Run against frozen baseline model-router source; no provider call or credential.
use model_router::controller::ControllerTask;
use serde_json::{Value, json};
use std::io::{self, Read};

fn main() {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw).unwrap();
    assert!(raw.len() <= 200_000);
    let value: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["task"]["session_id"], "synthetic-rollout");
    let task = ControllerTask::from_value(&value["task"]).unwrap();
    let prompt = task.composed_prompt_text().unwrap();
    println!("{}", json!({
        "bytes":prompt.len(),
        "identity":prompt.contains("SYNTHETIC_MANDATORY_IDENTITY"),
        "rules":prompt.contains("SYNTHETIC_MANDATORY_ROLE"),
        "current_message_occurrences":prompt.matches("SYNTHETIC_CURRENT_MESSAGE").count(),
        "unattested_recall":prompt.contains("SYNTHETIC_UNATTESTED_RECALL"),
        "agent_graph":prompt.contains("SYNTHETIC_AGENT_GRAPH"),
        "result_tail":prompt.contains("SYNTHETIC_RESULT_TAIL")
    }));
}
