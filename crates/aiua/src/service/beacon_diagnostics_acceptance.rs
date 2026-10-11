use super::*;
use model_router::controller::{ControllerTask, ModelProvider, ProviderOutput, RetryPolicy, TaskKind};
use model_router::runtime::{ControllerGuestConfig, run_model_controller};
use std::sync::Mutex;
use tracing_subscriber::{Layer, layer::SubscriberExt};

#[derive(Clone, Default)]
struct Observations(Arc<Mutex<Vec<serde_json::Value>>>);

impl<S: tracing::Subscriber> Layer<S> for Observations {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Fields(Option<String>);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "model_attempt" {
                    self.0 = Some(format!("{value:?}"));
                }
            }
        }
        let mut fields = Fields(None);
        event.record(&mut fields);
        if let Some(raw) = fields.0 {
            self.0.lock().unwrap().push(serde_json::from_str(&raw).unwrap());
        }
    }
}

struct SyntheticProvider {
    id: &'static str,
    calls: Arc<Mutex<Vec<(&'static str, ControllerTask, bool)>>>,
}

#[async_trait::async_trait]
impl ModelProvider for SyntheticProvider {
    fn id(&self) -> &'static str { self.id }
    fn supports(&self, task: &ControllerTask) -> bool { task.kind == TaskKind::TextGenerate }
    fn retry_policy(&self) -> RetryPolicy { RetryPolicy { max_attempts: 1, ..Default::default() } }
    fn supports_streaming(&self, task: &ControllerTask) -> bool {
        task.prompt.as_deref() == Some("PUBLIC_STREAM")
    }
    async fn invoke(&self, task: &ControllerTask) -> anyhow::Result<ProviderOutput> {
        self.calls.lock().unwrap().push((self.id, task.clone(), false));
        if task.prompt.as_deref() == Some("PUBLIC_ERROR") {
            anyhow::bail!("provider_error: HTTP 503 PUBLIC_ERROR_ONLY");
        }
        Ok(public_output())
    }
    async fn invoke_streaming(&self, task: &ControllerTask, tokens: tokio::sync::mpsc::Sender<String>) -> anyhow::Result<ProviderOutput> {
        self.calls.lock().unwrap().push((self.id, task.clone(), true));
        tokens.send("PUBLIC_TOKEN".into()).await.unwrap();
        Ok(public_output())
    }
}

fn public_output() -> ProviderOutput {
    ProviderOutput::Text {
        content: "PUBLIC_REPLY".into(), display_text: None, spoken_text: None,
        partial_replies: vec![], working_memory_delta: None, follow_up_questions: vec![],
        intent_summary: None, memory_concept: None, memory_candidate: None,
        active_plan: None, model_gen: Some("PUBLIC_RESOLVED_MODEL".into()),
    }
}

// A fresh process owns all environment/global emitters. No ambient provider
// credentials, private graph, inherited HOME, or network provider is consulted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn beacon_diagnostics_real_controller_dispatch_reply() {
    const CHILD: &str = "PHILOTIC_BEACON_SYNTHETIC_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let exact = format!("{}::beacon_diagnostics_real_controller_dispatch_reply", module_path!().split_once("::").unwrap().1);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([exact.as_str(), "--exact", "--nocapture"])
            .env_clear().env(CHILD, "1").env("CI", "true")
            .env("PHILOTIC_VAULT_MASTER_KEY", "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=")
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                let summary = String::from_utf8(output.stdout).unwrap();
                assert!(status.success(), "synthetic child failed: {}", String::from_utf8_lossy(&output.stderr));
                assert!(summary.contains("1 passed; 0 failed; 0 ignored;"), "child did not execute exact acceptance");
                break;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap(); let _ = child.wait(); panic!("synthetic acceptance timed out");
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
        }
        return;
    }
    let captures = Observations::default();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(captures.clone())).unwrap();
    let socket = test_socket_path();
    let scratch = std::env::temp_dir().join(format!("beacon-diag-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&scratch).unwrap();
    unsafe {
        std::env::set_var("HOME", &scratch);
        std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket);
        std::env::set_var("PHILOTIC_ROUTER_TRACE_DB", scratch.join("synthetic-trace.db"));
    }
    let (dispatcher, _unused) = test_dispatcher_channel();
    let store = SqliteGraphStorage::open(":memory:").unwrap();
    let graph = Arc::new(GraphDomain::new(Arc::new(store.adapter())));
    let server = IpcServer::new(socket.clone(), "local-aiua-01", dispatcher, graph);
    let hotel = tokio::spawn(async move { server.run().await.unwrap(); });
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let mut caller = PhiloticClient::connect(GuestIdentity {
        guest_id: "synthetic-caller".into(), role: "synthetic-reply".into(), supported_tools: vec![],
    }).await.unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut loops = Vec::new();
    for (provider, role, guest) in [
        ("openrouter", "model.openrouter", "synthetic-openrouter"),
        ("gemini", "model", "synthetic-gemini"),
        ("ollama", "model.ollama", "synthetic-ollama"),
    ] {
        let records = calls.clone();
        loops.push(tokio::spawn(run_model_controller(ControllerGuestConfig {
            guest_id: guest, role, allow_inline_audio: false,
            providers: Box::new(move |_, _| vec![Arc::new(SyntheticProvider { id: provider, calls: records.clone() })]),
            live_providers: Box::new(|_, _| vec![]),
        })));
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        for prompt in ["PUBLIC_BATCH", "PUBLIC_ERROR", "PUBLIC_STREAM"] {
            let turn = format!("{provider}-{prompt}");
            let payload = serde_json::json!({
                "action": "generate_text", "prompt": prompt, "model": "PUBLIC_MODEL",
                "session_id": "PUBLIC_SESSION", "turn_id": turn,
                "provider_options": {"provider": provider, "unchanged": 42},
                "return_route": {"node": "local-aiua-01", "role": "synthetic-reply", "guest_id": "synthetic-caller"}
            });
            let expected = ControllerTask::from_value(&payload).unwrap();
            let before = captures.0.lock().unwrap().len();
            let ack = caller.send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(), target_role: role.into(),
                target_guest_id: Some(guest.into()), task_json: payload.to_string(),
            }).await.unwrap();
            assert!(matches!(ack, IpcResponse::Standard { ok: true, .. }));
            let mut token = false;
            let reply = tokio::time::timeout(tokio::time::Duration::from_secs(10), async {
                loop {
                    let message = caller.recv_task().await.unwrap();
                    let IpcResponse::InboundTask { task_json, .. } = message else { panic!("expected reply IPC"); };
                    let value: serde_json::Value = serde_json::from_str(&task_json).unwrap();
                    assert_eq!(value["turn_id"], turn);
                    if value["action"] == "streaming_token" {
                        assert_eq!(value["content"], "PUBLIC_TOKEN"); token = true; continue;
                    }
                    if value["action"] == "model_response" { break value; }
                }
            }).await.unwrap();
            assert_eq!(reply["session_id"], "PUBLIC_SESSION");
            assert_eq!(reply["agent_action"]["kind"], if prompt == "PUBLIC_ERROR" { "fail" } else { "respond" });
            if prompt != "PUBLIC_ERROR" { assert_eq!(reply["content"], "PUBLIC_REPLY"); }
            // A streaming token may arrive after the final response; drain it
            // before the next turn to prove the actual forwarding IPC as well.
            if prompt == "PUBLIC_STREAM" && !token {
                let message = tokio::time::timeout(tokio::time::Duration::from_secs(2), caller.recv_task()).await.unwrap().unwrap();
                let IpcResponse::InboundTask { task_json, .. } = message else { panic!("expected stream IPC"); };
                let value: serde_json::Value = serde_json::from_str(&task_json).unwrap();
                assert_eq!(value["action"], "streaming_token"); assert_eq!(value["content"], "PUBLIC_TOKEN");
                assert_eq!(value["turn_id"], turn);
            }
            let actual = calls.lock().unwrap().last().cloned().unwrap();
            assert_eq!(actual.0, provider, "wrong provider handled addressed role");
            assert_eq!(actual.1, expected, "observation changed provider input");
            assert_eq!(actual.2, prompt == "PUBLIC_STREAM");
            let records = captures.0.lock().unwrap();
            assert_eq!(records.len(), before + 2, "real attempt start/finish must emit");
            let final_record = records.last().unwrap();
            assert_eq!(final_record["provider"], provider);
            assert_eq!(final_record["outcome"], if prompt == "PUBLIC_ERROR" { "failure" } else { "success" });
            assert_eq!(final_record["route"]["phase"], "unknown");
            let serialized = serde_json::to_string(final_record).unwrap();
            for private in ["PUBLIC_MODEL", "PUBLIC_SESSION", "PUBLIC_REPLY", "PUBLIC_ERROR_ONLY", "PUBLIC_RESOLVED_MODEL"] {
                assert!(!serialized.contains(private), "observation exposed content/identifier");
            }
        }
    }
    assert_eq!(calls.lock().unwrap().len(), 9);
    for task in loops { task.abort(); let _ = task.await; }
    hotel.abort(); let _ = hotel.await;
    let _ = std::fs::remove_file(socket);
    std::fs::remove_dir_all(scratch).unwrap();
}
