use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::{Duration, timeout};
use uuid::Uuid;

const DRIVER_GUEST_ID: &str = "life-graph-ipc-smoke-driver";
const DRIVER_ROLE: &str = "life-graph.ipc.smoke.reply";
const LIFE_GRAPH_EMBEDDING_DIMS: usize = 768;
const DEFAULT_LABEL: &str = "Signal";
const DEFAULT_CLAIM_SUMMARY: &str = "Life Graph IPC smoke test: Signal node through hotel route";
const DEFAULT_SOURCE_ID: &str = DRIVER_GUEST_ID;
const DEFAULT_CONFIDENCE: f64 = 0.9;

#[tokio::main]
async fn main() -> Result<()> {
    let target_node =
        std::env::var("PHILOTIC_TARGET_NODE").unwrap_or_else(|_| "local-aiua-01".to_string());
    let reply_node = std::env::var("PHILOTIC_REPLY_NODE").unwrap_or_else(|_| target_node.clone());
    let node_id = format!("smoke-signal-{}", Uuid::new_v4().simple());
    let observation_id = format!("obs-{}", Uuid::new_v4().simple());
    let packet_id = format!("pkt-{}", Uuid::new_v4().simple());

    let mut client = SmokeIpc::connect(GuestIdentity {
        guest_id: DRIVER_GUEST_ID.into(),
        role: DRIVER_ROLE.into(),
        supported_tools: Vec::new(),
    })
    .await
    .context("failed to connect life graph IPC smoke driver")?;

    subscribe_reply_inbox(&mut client).await?;

    let observe_payload = execute_life_tool(
        &mut client,
        &target_node,
        &reply_node,
        "life.observe",
        observe_input(&node_id, &observation_id, &packet_id),
    )
    .await?;
    assert_success_capability(&observe_payload, "life.observe")?;
    let observe_data = &observe_payload["result"]["data"];
    if observe_data["status"].as_str() != Some("proposed") {
        bail!("life.observe expected status=proposed, got {observe_payload}");
    }
    if observe_data["embed_status"].as_str() != Some("ok") {
        bail!("life.observe expected embed_status=ok, got {observe_payload}");
    }
    println!("life.observe IPC ok  node_id={node_id}");

    let recall_payload = execute_life_tool(
        &mut client,
        &target_node,
        &reply_node,
        "life.recall",
        recall_input(),
    )
    .await?;
    assert_success_capability(&recall_payload, "life.recall")?;
    let recall_data = &recall_payload["result"]["data"];
    if recall_data["status"].as_str() != Some("ok") {
        bail!("life.recall expected status=ok, got {recall_payload}");
    }
    let packet = &recall_data["context_packet"];
    if packet["context_id"].as_str().is_none() || !packet["ranked_packets"].is_array() {
        bail!("life.recall returned malformed context packet: {recall_payload}");
    }
    let result_count = packet["ranked_packets"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();
    println!("life.recall IPC ok   result_count={result_count}");

    let context_id = packet["context_id"]
        .as_str()
        .context("life.recall context packet missing context_id")?
        .to_string();
    let feedback_payload = execute_life_tool(
        &mut client,
        &target_node,
        &reply_node,
        "life.recall.feedback",
        feedback_input(&context_id),
    )
    .await?;
    assert_success_capability(&feedback_payload, "life.recall.feedback")?;
    println!("life.recall.feedback IPC ok  context_id={context_id}");

    println!("life graph IPC smoke passed");
    Ok(())
}

async fn subscribe_reply_inbox(client: &mut SmokeIpc) -> Result<()> {
    let subscribe = client
        .send_request(IpcRequest::SubscribeInbox {
            role: DRIVER_ROLE.into(),
        })
        .await
        .context("failed to subscribe life graph IPC smoke inbox")?;
    match subscribe {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        other => bail!("unexpected subscribe response: {other:?}"),
    }
}

async fn execute_life_tool(
    client: &mut SmokeIpc,
    target_node: &str,
    reply_node: &str,
    tool_name: &str,
    arguments: Value,
) -> Result<Value> {
    let response = client
        .send_request(IpcRequest::EmitTask {
            target_node: target_node.to_string(),
            target_role: "life-graph-runner".into(),
            target_guest_id: std::env::var("PHILOTIC_TARGET_GUEST_ID")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            task_json: json!({
                "action": "execute_tool",
                "tool_name": tool_name,
                "arguments": arguments,
                "session_id": format!("smoke:life-graph:{tool_name}"),
                "turn_id": format!("smoke-turn-life-graph-{}", tool_name.replace('.', "-")),
                "chat_id": "smoke-chat",
                "agent_id": DRIVER_GUEST_ID,
                "reply_to": reply_node,
                "reply_role": DRIVER_ROLE,
            })
            .to_string(),
        })
        .await
        .with_context(|| format!("{tool_name}: send EmitTask"))?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("{tool_name}: unexpected emit response: {other:?}"),
    }

    let payload = timeout(Duration::from_secs(20), async {
        loop {
            let reply = client.recv_inbound_task().await?;
            let IpcResponse::InboundTask { task_json, .. } = reply else {
                bail!("{tool_name}: unexpected reply envelope: {reply:?}");
            };
            let payload: Value = serde_json::from_str(&task_json)
                .context("failed to decode datasource_response json")?;

            if payload["action"].as_str() != Some("datasource_response") {
                bail!("{tool_name}: expected datasource_response, got {payload}");
            }
            if payload.get("error").is_some() {
                bail!(
                    "{tool_name}: datasource returned error: {}",
                    payload["error"]
                );
            }
            if payload["capability"].as_str() == Some(tool_name) {
                return Ok(payload);
            }
            eprintln!(
                "{tool_name}: ignoring stale datasource_response for capability={:?}",
                payload["capability"].as_str()
            );
        }
    })
    .await
    .with_context(|| format!("{tool_name}: timed out waiting for datasource_response"))??;

    Ok(payload)
}

fn assert_success_capability(payload: &Value, tool_name: &str) -> Result<()> {
    if payload["capability"].as_str() != Some(tool_name) {
        bail!("{tool_name}: unexpected capability payload: {payload}");
    }
    if payload["result"]["status"].as_str() != Some("success") {
        bail!("{tool_name}: expected result.status=success, got {payload}");
    }
    Ok(())
}

fn observe_input(node_id: &str, observation_id: &str, packet_id: &str) -> Value {
    let label = std::env::var("LIFE_GRAPH_OBSERVE_LABEL").unwrap_or_else(|_| DEFAULT_LABEL.into());
    let claim_summary =
        std::env::var("LIFE_GRAPH_OBSERVE_CLAIM").unwrap_or_else(|_| DEFAULT_CLAIM_SUMMARY.into());
    let source_id =
        std::env::var("LIFE_GRAPH_OBSERVE_SOURCE").unwrap_or_else(|_| DEFAULT_SOURCE_ID.into());
    let confidence = std::env::var("LIFE_GRAPH_OBSERVE_CONFIDENCE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(DEFAULT_CONFIDENCE);

    json!({
        "observation_id": observation_id,
        "evidence": {
            "packet_id": packet_id,
            "claim_ref": {
                "id": node_id,
                "label": label
            },
            "claim_summary": claim_summary,
            "source_refs": [
                {
                    "source_id": source_id,
                    "source_kind": "runtime_observation",
                    "reliability": {
                        "score": 0.95,
                        "basis": "direct_observation"
                    }
                }
            ],
            "passage_refs": [],
            "confidence": confidence,
            "validation_state": "proposed",
            "observed_at": "2026-06-05T00:00:00Z",
            "source_reliability": 0.95,
            "conflict_ids": [],
            "adjudication_status": "not_needed",
            "metadata": {
                "smoke": true,
                "route": "hotel_ipc"
            }
        },
        "proposed_graph_refs": []
    })
}

fn recall_input() -> Value {
    let mut embedding = vec![0.0_f32; LIFE_GRAPH_EMBEDDING_DIMS];
    embedding[0] = 1.0;

    json!({
        "query_id": format!("smoke-recall-{}", Uuid::new_v4().simple()),
        "query_text": "life graph ipc smoke",
        "strategy": "memory_aware_graph_rank",
        "semantic_pivots": [
            {
                "space": "life_event_semantic",
                "embedding_model": "Xenova/all-mpnet-base-v2",
                "embedding_dims": LIFE_GRAPH_EMBEDDING_DIMS,
                "query_text_hash": "smoke-hash-placeholder"
            }
        ],
        "expansion_policy": {
            "max_hops": 2,
            "max_nodes": 16,
            "allowed_edge_types": []
        },
        "policy_filters": ["exclude_retired"],
        "ranking_weights": {
            "semantic_similarity": 0.45,
            "graph_specificity": 0.2,
            "recency": 0.1,
            "confirmation": 0.15,
            "active_commitment": 0.1
        },
        "operator_intent": "open_loops_by_context",
        "max_context_packets": 5,
        "embedding": embedding
    })
}

fn feedback_input(context_id: &str) -> Value {
    json!({
        "feedback_id": format!("smoke-feedback-{}", Uuid::new_v4().simple()),
        "packet_id": context_id,
        "rating": "useful",
        "candidate_count": 1,
        "connected_candidate_count": 1,
        "missing_context_refs": [],
        "noisy_node_refs": [],
        "stale_node_refs": [],
        "evidence_packets": []
    })
}

struct SmokeIpc {
    stream: UnixStream,
    read_buf: Vec<u8>,
}

impl SmokeIpc {
    async fn connect(identity: GuestIdentity) -> Result<Self> {
        let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
            .unwrap_or_else(|_| "/tmp/philotic-aiua.sock".to_string());
        let stream = UnixStream::connect(&socket_path)
            .await
            .with_context(|| format!("failed to connect hotel IPC socket at {socket_path}"))?;
        let mut client = Self {
            stream,
            read_buf: Vec::new(),
        };
        match client.send_request(IpcRequest::Register(identity)).await? {
            IpcResponse::Standard { ok: true, .. } => Ok(client),
            other => bail!("hotel rejected smoke driver registration: {other:?}"),
        }
    }

    async fn send_request(&mut self, request: IpcRequest) -> Result<IpcResponse> {
        self.write_response_frame(&request).await?;
        loop {
            let response = self.read_response().await?;
            if matches!(
                response,
                IpcResponse::InboundTask { .. }
                    | IpcResponse::ApartmentUpdate { .. }
                    | IpcResponse::GracefulShutdown { .. }
                    | IpcResponse::MemoryConfig(_)
                    | IpcResponse::MuninnStatus { .. }
                    | IpcResponse::NetworkState { .. }
            ) {
                continue;
            }
            return Ok(response);
        }
    }

    async fn recv_inbound_task(&mut self) -> Result<IpcResponse> {
        loop {
            let response = self.read_response().await?;
            match response {
                IpcResponse::InboundTask { .. } => return Ok(response),
                IpcResponse::Standard { ok: true, .. }
                | IpcResponse::ApartmentUpdate { .. }
                | IpcResponse::GracefulShutdown { .. }
                | IpcResponse::MemoryConfig(_)
                | IpcResponse::MuninnStatus { .. }
                | IpcResponse::NetworkState { .. } => continue,
                other => bail!("unexpected non-task response while waiting for reply: {other:?}"),
            }
        }
    }

    async fn write_response_frame<T: serde::Serialize>(&mut self, value: &T) -> Result<()> {
        let payload = serde_json::to_vec(value).context("failed to serialize IPC frame")?;
        let len = payload.len() as u32;
        self.stream
            .write_all(&len.to_be_bytes())
            .await
            .context("failed to write IPC frame header")?;
        self.stream
            .write_all(&payload)
            .await
            .context("failed to write IPC frame payload")?;
        Ok(())
    }

    async fn read_response(&mut self) -> Result<IpcResponse> {
        loop {
            if self.read_buf.len() >= 4 {
                let len = u32::from_be_bytes([
                    self.read_buf[0],
                    self.read_buf[1],
                    self.read_buf[2],
                    self.read_buf[3],
                ]) as usize;
                let frame_len = 4 + len;
                if self.read_buf.len() >= frame_len {
                    let payload = self.read_buf[4..frame_len].to_vec();
                    self.read_buf.drain(..frame_len);
                    return serde_json::from_slice(&payload)
                        .context("failed to decode IPC response frame");
                }
            }

            let mut chunk = [0_u8; 8192];
            let n = self
                .stream
                .read(&mut chunk)
                .await
                .context("failed to read IPC frame")?;
            if n == 0 {
                bail!("IPC stream closed while waiting for frame");
            }
            self.read_buf.extend_from_slice(&chunk[..n]);
        }
    }
}
