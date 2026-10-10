//! Frozen baseline provider source, loopback stub only; no credential or external model.
use axum::{Json, Router, http::HeaderMap, routing::post};
use model_router::controller::{ControllerTask, ModelProvider, ProviderOutput};
use model_router::providers::OpenAIProvider;
use serde_json::{Value, json};
use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() {
    let mut raw = String::new();
    io::stdin().take(200_001).read_to_string(&mut raw).unwrap();
    assert!(raw.len() <= 200_000);
    let value: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["task"]["session_id"], "synthetic-rollout");
    let task = ControllerTask::from_value(&value["task"]).unwrap();
    let prompt = task.composed_prompt_text().unwrap();

    // Address is generated here, never supplied by configuration or fixture.
    // No auth, proxy, redirect, catalog/tools, or remote model endpoint exists.
    assert!(task.tools.is_empty());
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = Arc::clone(&captured);
    let app = Router::new().route("/v1/chat/completions", post(move |headers: HeaderMap, Json(body): Json<Value>| {
        let capture = Arc::clone(&capture);
        async move {
            assert!(!headers.contains_key("authorization"));
            capture.lock().await.push(body);
            Json(json!({"choices":[{"message":{"content":"PUBLIC_SYNTHETIC_REPLY"}}]}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5)).build().unwrap();
    let provider = OpenAIProvider::new_compatible("openrouter", client, None,
        Some(format!("http://{address}")), None, Some("public-synthetic-model".into()), None,
        Vec::new(), None);
    let output = tokio::time::timeout(Duration::from_secs(8), provider.invoke(&task)).await.unwrap().unwrap();
    match output {
        ProviderOutput::Text { content, .. } => assert_eq!(content, "PUBLIC_SYNTHETIC_REPLY"),
        _ => panic!("synthetic stub returned unexpected provider output"),
    }
    let requests = captured.lock().await;
    assert_eq!(requests.len(), 1);
    let body = &requests[0];
    assert_eq!(body["model"], "public-synthetic-model");
    let message = body["messages"].as_array().unwrap().iter()
        .find(|message| message["role"] == "user").unwrap();
    assert_eq!(message["content"], prompt);
    assert!(body.get("max_tokens").is_none() && body.get("context_limits").is_none());
    server.abort();
    println!("{}", json!({
        "bytes":prompt.len(),
        "identity":prompt.contains("SYNTHETIC_MANDATORY_IDENTITY"),
        "rules":prompt.contains("SYNTHETIC_MANDATORY_ROLE"),
        "current_message_occurrences":prompt.matches("SYNTHETIC_CURRENT_MESSAGE").count(),
        "unattested_recall":prompt.contains("SYNTHETIC_UNATTESTED_RECALL"),
        "agent_graph":prompt.contains("SYNTHETIC_AGENT_GRAPH"),
        "result_tail":prompt.contains("SYNTHETIC_RESULT_TAIL"),
        "loopback_provider_calls":requests.len(), "external_provider_calls":0,
        "serialized_prompt_matches":true, "context_budget_forwarded":false
    }));
}
