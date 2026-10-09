//! Synthetic framed IPC payload contracts, not full controller acceptance.
//! No authority is issued here; runtime-only capability reports must not be
//! accepted from the task transported by the caller.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn round_trip_task(payload: &Value) -> ControllerTask {
    let response = IpcResponse::InboundTask {
        source_node: "synthetic-hotel".into(),
        task_id: "00000000-0000-0000-0000-000000000000".parse().unwrap(),
        task_json: payload.to_string(),
    };
    let bytes = serde_json::to_vec(&response).unwrap();
    let (mut writer, mut reader) = tokio::net::UnixStream::pair().unwrap();
    let send = async {
        writer.write_u32(bytes.len() as u32).await.unwrap();
        writer.write_all(&bytes).await.unwrap();
    };
    let receive = async {
        let length = reader.read_u32().await.unwrap();
        assert!(length < 65_536, "bounded synthetic fixture");
        let mut bytes = vec![0; length as usize];
        reader.read_exact(&mut bytes).await.unwrap();
        let IpcResponse::InboundTask { task_json, .. } = serde_json::from_slice(&bytes).unwrap()
        else {
            panic!("expected inbound task");
        };
        ControllerTask::from_value(&serde_json::from_str::<Value>(&task_json).unwrap()).unwrap()
    };
    let (_, task) = tokio::join!(send, receive);
    task
}

fn payload() -> Value {
    json!({
        "kind":"text.generate", "model":"synthetic/primary",
        "prompt":"LEGACY_DUPLICATE",
        "context":{"active_turn":{"role":"user","text":"CURRENT_USER"},
            "instructions":[{"text":"REQUIRED_SYSTEM"}]},
        "resolved_context_capabilities":[{"provider":"openrouter",
            "model_id":"synthetic/primary", "context_tokens":1000000,
            "output_tokens":100000, "source":"caller-controlled"}]
    })
}

#[tokio::test]
async fn framed_ipc_caller_cannot_expand_runtime_capability_budget() {
    let mut original = payload();
    let task = round_trip_task(&original).await;
    assert!(task.resolved_context_capabilities.is_empty());
    assert_eq!(crate::context_management::limits(&task).unwrap().1, 16_384);
    original["provider_options"] = json!({"max_tokens":100000});
    let task = round_trip_task(&original).await;
    assert!(crate::context_management::prepare(&task).is_err());
}

struct Probe;
#[async_trait::async_trait]
impl ModelProvider for Probe {
    fn id(&self) -> &'static str {
        "openrouter"
    }
    fn supports(&self, _: &ControllerTask) -> bool {
        true
    }
    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let expected = if task.model.as_deref() == Some("synthetic/fallback") {
            8_000
        } else {
            16_384
        };
        assert_eq!(crate::context_management::limits(task)?.1, expected);
        assert_eq!(task.provider_options["max_tokens"], 4096);
        let prompt = task.composed_prompt_text().unwrap();
        assert!(prompt.contains("CURRENT_USER"));
        assert!(prompt.contains("REQUIRED_SYSTEM"));
        assert!(!prompt.contains("LEGACY_DUPLICATE"));
        anyhow::bail!("synthetic attempt observed")
    }
}

#[tokio::test]
async fn framed_ipc_retry_and_fallback_resolve_each_attempt_without_mutating_task() {
    let original = payload();
    let task = round_trip_task(&original).await;
    let catalog = crate::context_management::parse_openrouter_capabilities(
        r#"[{"id":"synthetic/primary","ctx":1000000},{"id":"synthetic/fallback","ctx":8000}]"#,
    );
    for _ in 0..2 {
        let error = invoke_ordinary_provider_attempt(&Probe, &task, None, &catalog)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("synthetic attempt observed"));
    }
    let mut fallback = original.clone();
    fallback["model"] = json!("synthetic/fallback");
    let fallback = round_trip_task(&fallback).await;
    let error = invoke_ordinary_provider_attempt(&Probe, &fallback, None, &catalog)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("synthetic attempt observed"));
    assert!(task.resolved_context_capabilities.is_empty());
    assert!(task.provider_options.is_empty());
    assert_eq!(task.model.as_deref(), Some("synthetic/primary"));
    assert_eq!(task.prompt.as_deref(), Some("LEGACY_DUPLICATE"));
}
