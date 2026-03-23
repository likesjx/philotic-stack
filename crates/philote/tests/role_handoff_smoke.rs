/// Smoke tests for the philote role handoff cycle.
///
/// These tests spin up a lightweight in-process hotel harness over a Unix socket
/// and exercise the full dispatch path through AgentRuntime:
///
///   handoff_bundle → role_activation applied, summary stashed
///   handoff_return → role_activation cleared
///
/// The harness speaks the same length-prefixed JSON framing as the real aiua daemon.
use agent_core::protocol::InboundTaskPayload;
use agent_core::runtime::AgentRuntime;
use philotic_client::{GuestIdentity, HandoffBundle, IpcRequest, IpcResponse};
use std::path::Path;
use std::sync::Once;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use uuid::Uuid;

static TRACING: Once = Once::new();

fn init_tracing() {
    TRACING.call_once(|| {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_test_writer()
            .init();
    });
}

// ── Frame I/O helpers (same framing as PhiloticClient) ───────────────────────

async fn write_frame(stream: &mut tokio::net::UnixStream, payload: &[u8]) {
    let len = u32::try_from(payload.len()).expect("frame length fits u32");
    stream
        .write_all(&len.to_be_bytes())
        .await
        .expect("write header");
    stream.write_all(payload).await.expect("write payload");
}

fn ok_response() -> Vec<u8> {
    serde_json::to_vec(&IpcResponse::success("ok", None)).unwrap()
}

fn no_config_response(key: &str) -> Vec<u8> {
    serde_json::to_vec(&IpcResponse::ConfigData {
        key: key.to_string(),
        value_json: None,
    })
    .unwrap()
}

// ── Hotel harness ─────────────────────────────────────────────────────────────

/// Runs a bare-minimum hotel harness that:
///   1. Accepts one client connection.
///   2. Responds `ok` to Register.
///   3. Responds `ConfigData { value_json: None }` to any GetConfig.
///   4. Responds `ok` to SyncApartment and CompleteTask.
///   5. Exits as soon as the client closes the connection.
async fn run_stub_hotel(listener: UnixListener) {
    let (mut stream, _) = listener.accept().await.expect("accept");

    loop {
        // Read until EOF (client disconnects).
        let buf = match async {
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await?;
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut buf = vec![0u8; len];
            stream.read_exact(&mut buf).await?;
            Ok::<_, std::io::Error>(buf)
        }
        .await
        {
            Ok(b) => b,
            Err(_) => return, // client disconnected
        };

        let req: IpcRequest = serde_json::from_slice(&buf).expect("decode request");

        let reply = match &req {
            IpcRequest::Register(_) => ok_response(),
            IpcRequest::GetConfig { key } => no_config_response(key),
            IpcRequest::SyncApartment { .. } => ok_response(),
            IpcRequest::CompleteTask { .. } => ok_response(),
            other => panic!("stub hotel received unexpected request: {other:?}"),
        };

        write_frame(&mut stream, &reply).await;
    }
}

// ── Test helpers ──────────────────────────────────────────────────────────────

fn test_socket_path() -> String {
    format!("/tmp/philote-smoke-{}.sock", Uuid::new_v4().simple())
}

async fn connect_runtime(socket_path: &str, agent_id: &str) -> AgentRuntime {
    let identity = GuestIdentity {
        guest_id: agent_id.into(),
        role: "agent".into(),
        supported_tools: Vec::new(),
    };
    let client = philotic_client::PhiloticClient::connect_at(socket_path, identity)
        .await
        .expect("connect to stub hotel");
    AgentRuntime::new(client, agent_id)
}

fn handoff_bundle_task(
    session_id: &str,
    to_role: &str,
    summary: &str,
) -> (InboundTaskPayload, Uuid) {
    let task_id = Uuid::new_v4();
    let bundle = HandoffBundle {
        to_role: Some(to_role.into()),
        from_role: Some("orchestrator".into()),
        handoff_reason: Some("smoke_test".into()),
        working_summary: Some(summary.into()),
        ..Default::default()
    };
    let task = InboundTaskPayload {
        action: Some("handoff_bundle".into()),
        session_id: Some(session_id.into()),
        turn_id: Some(format!("turn-{}", task_id.simple())),
        handoff_bundle: Some(bundle),
        ..Default::default()
    };
    (task, task_id)
}

fn handoff_return_task(session_id: &str) -> (InboundTaskPayload, Uuid) {
    let task_id = Uuid::new_v4();
    let task = InboundTaskPayload {
        action: Some("handoff_return".into()),
        session_id: Some(session_id.into()),
        turn_id: Some(format!("turn-{}", task_id.simple())),
        agent_action: None,
        ..Default::default()
    };
    (task, task_id)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Receiving a `handoff_bundle` applies role_activation and stashes the summary.
#[tokio::test]
async fn handoff_bundle_sets_role_activation_and_summary() {
    init_tracing();
    eprintln!("[smoke] ── handoff_bundle_sets_role_activation_and_summary ──────────");
    let socket_path = test_socket_path();
    let listener = UnixListener::bind(&socket_path).expect("bind");

    let server = tokio::spawn({
        let socket_path = socket_path.clone();
        async move {
            run_stub_hotel(listener).await;
            let _ = std::fs::remove_file(&socket_path);
        }
    });

    let mut runtime = connect_runtime(&socket_path, "agent-jane-01").await;
    eprintln!("[smoke] runtime connected");

    let session_id = "sess-smoke-1";
    let (task, task_id) =
        handoff_bundle_task(session_id, "researcher", "Investigating Q1 anomaly.");

    eprintln!("[smoke] → handle_handoff_bundle (role=researcher)");
    runtime
        .handle_handoff_bundle(task, task_id)
        .await
        .expect("handle_handoff_bundle must not error");
    eprintln!("[smoke] ✓ handoff_bundle handled");

    let state = runtime
        .session(session_id)
        .expect("session must exist after handoff");
    assert_eq!(
        state.role_activation.as_ref().map(|r| r.role_name.as_str()),
        Some("researcher"),
        "role_activation must be set to the requested role"
    );
    assert_eq!(
        state.last_handoff_summary.as_deref(),
        Some("Investigating Q1 anomaly."),
        "last_handoff_summary must carry the bundle working_summary"
    );

    drop(runtime); // closes the socket → server exits
    let _ = server.await;
    if Path::new(&socket_path).exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
}

/// Receiving a `handoff_return` clears role_activation.
#[tokio::test]
async fn handoff_return_clears_role_activation() {
    init_tracing();
    eprintln!("[smoke] ── handoff_return_clears_role_activation ────────────────────");
    let socket_path = test_socket_path();
    let listener = UnixListener::bind(&socket_path).expect("bind");

    let server = tokio::spawn({
        let socket_path = socket_path.clone();
        async move {
            run_stub_hotel(listener).await;
            let _ = std::fs::remove_file(&socket_path);
        }
    });

    let mut runtime = connect_runtime(&socket_path, "agent-jane-01").await;
    eprintln!("[smoke] runtime connected");

    let session_id = "sess-smoke-2";

    // First: apply a bundle to activate a role.
    let (bundle_task, bundle_id) =
        handoff_bundle_task(session_id, "analyst", "Revenue anomaly context.");
    eprintln!("[smoke] → handle_handoff_bundle (role=analyst)");
    runtime
        .handle_handoff_bundle(bundle_task, bundle_id)
        .await
        .expect("handle_handoff_bundle");
    eprintln!(
        "[smoke] ✓ role active: {:?}",
        runtime
            .session(session_id)
            .unwrap()
            .role_activation
            .as_ref()
            .map(|r| &r.role_name)
    );

    assert!(
        runtime
            .session(session_id)
            .unwrap()
            .role_activation
            .is_some(),
        "role must be active before return"
    );

    // Then: return from role.
    let (return_task, return_id) = handoff_return_task(session_id);
    eprintln!("[smoke] → handle_handoff_return");
    runtime
        .handle_handoff_return(return_task, return_id)
        .await
        .expect("handle_handoff_return");
    eprintln!("[smoke] ✓ role cleared");

    let state = runtime.session(session_id).expect("session must exist");
    assert!(
        state.role_activation.is_none(),
        "role_activation must be cleared after handoff_return"
    );

    drop(runtime);
    let _ = server.await;
    if Path::new(&socket_path).exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
}

/// Full cycle: bundle → summary visible in context → clear → return → clean context.
#[tokio::test]
async fn full_handoff_cycle_session_context() {
    init_tracing();
    eprintln!("[smoke] ── full_handoff_cycle_session_context ─────────────────────────");
    let socket_path = test_socket_path();
    let listener = UnixListener::bind(&socket_path).expect("bind");

    let server = tokio::spawn({
        let socket_path = socket_path.clone();
        async move {
            run_stub_hotel(listener).await;
            let _ = std::fs::remove_file(&socket_path);
        }
    });

    let mut runtime = connect_runtime(&socket_path, "agent-jane-01").await;
    eprintln!("[smoke] runtime connected");

    let session_id = "sess-smoke-cycle";

    // ── Step 1: bundle arrives ────────────────────────────────────────────────
    let (bundle_task, bundle_id) =
        handoff_bundle_task(session_id, "coder", "Implement the auth fix.");
    eprintln!("[smoke] step 1 → handle_handoff_bundle (role=coder)");
    runtime
        .handle_handoff_bundle(bundle_task, bundle_id)
        .await
        .expect("handle_handoff_bundle");
    eprintln!("[smoke] ✓ bundle applied");

    let state = runtime.session(session_id).unwrap();

    // Role and summary present.
    assert_eq!(
        state.role_activation.as_ref().map(|r| r.role_name.as_str()),
        Some("coder")
    );
    assert_eq!(
        state.last_handoff_summary.as_deref(),
        Some("Implement the auth fix.")
    );

    // Context on first turn includes both.
    let ctx = {
        let projection = state.build_context_projection("what should I do?");
        state.model_context_from_projection(&projection)
    };
    let session_text: String = ctx["instructions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["projection_kind"] == "session")
        .filter_map(|i| i["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        session_text.contains("Active role: coder."),
        "role in envelope"
    );
    assert!(
        session_text.contains("Handoff context: Implement the auth fix."),
        "summary in envelope"
    );

    // ── Step 2: runtime clears one-shot summary ───────────────────────────────
    // (In production this happens in AgentRuntime after build_context_projection.)
    eprintln!("[smoke] step 2 → clear_handoff_summary");
    runtime
        .session_mut_for_test(session_id)
        .unwrap()
        .clear_handoff_summary();
    eprintln!("[smoke] ✓ summary cleared");

    let state = runtime.session(session_id).unwrap();
    let ctx2 = {
        let projection = state.build_context_projection("next turn");
        state.model_context_from_projection(&projection)
    };
    let turn2_text: String = ctx2["instructions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["projection_kind"] == "session")
        .filter_map(|i| i["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(turn2_text.contains("Active role: coder."), "role persists");
    assert!(!turn2_text.contains("Handoff context:"), "summary consumed");

    // ── Step 3: handoff_return ────────────────────────────────────────────────
    let (return_task, return_id) = handoff_return_task(session_id);
    eprintln!("[smoke] step 3 → handle_handoff_return");
    runtime
        .handle_handoff_return(return_task, return_id)
        .await
        .expect("handle_handoff_return");
    eprintln!("[smoke] ✓ role cleared, back to orchestrator");

    let state = runtime.session(session_id).unwrap();
    let ctx3 = {
        let projection = state.build_context_projection("after return");
        state.model_context_from_projection(&projection)
    };
    let turn3_text: String = ctx3["instructions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["projection_kind"] == "session")
        .filter_map(|i| i["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!turn3_text.contains("Active role:"), "no role after return");
    assert!(
        !turn3_text.contains("Handoff context:"),
        "no summary after return"
    );

    drop(runtime);
    let _ = server.await;
    if Path::new(&socket_path).exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
}
