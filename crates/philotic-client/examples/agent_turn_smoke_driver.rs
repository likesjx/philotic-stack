/// Agent turn smoke driver — tests the full bjork turn loop including life.observe tool routing.
///
/// Unlike life_graph_ipc_smoke_driver which bypasses the agent, this test sends a
/// SendOperatorChatTurn to bjork and verifies the turn completes (not stuck running).
///
/// Run with:
///   PHILOTIC_HOTEL_SOCKET=~/.philotic/bjork/aiua-mac-jane.sock \
///   PHILOTIC_TARGET_NODE=mac-jane-aiua-01 \
///   cargo run -p philotic-client --example agent_turn_smoke_driver
use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse};
use tokio::time::{Duration, timeout};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .unwrap_or_else(|_| "/tmp/philotic-aiua.sock".to_string());
    let target_node =
        std::env::var("PHILOTIC_TARGET_NODE").unwrap_or_else(|_| "mac-jane-aiua-01".to_string());
    let target_agent =
        std::env::var("PHILOTIC_TARGET_AGENT").unwrap_or_else(|_| "agent-bjork-01".to_string());

    let operator_session_id = format!("smoke-agent-turn-{}", Uuid::new_v4().simple());
    let content = "Use the life.observe tool to record this open loop: \
        'verify life-graph routing works end-to-end from mac-jane'. \
        After calling the tool, tell me the node_id from the response.";

    eprintln!("smoke: connecting to IPC socket {socket_path}");
    eprintln!("smoke: sending turn to {target_agent} @ {target_node}");
    eprintln!("smoke: session_id={operator_session_id}");

    let mut client = SmokeIpc::connect(
        &socket_path,
        GuestIdentity {
            guest_id: format!("agent-turn-smoke-{}", Uuid::new_v4().simple()),
            role: "agent-turn-smoke".into(),
            supported_tools: Vec::new(),
        },
    )
    .await
    .context("failed to connect IPC socket")?;

    eprintln!("smoke: connected, sending SendOperatorChatTurn (may take 60s+)...");

    let response = timeout(
        Duration::from_secs(90),
        client.send_request(IpcRequest::SendOperatorChatTurn {
            target_node_id: target_node.clone(),
            target_agent_id: target_agent.clone(),
            operator_session_id: operator_session_id.clone(),
            conversation_id: None,
            content: content.into(),
        }),
    )
    .await
    .context("timed out waiting for agent turn reply (90s)")?
    .context("IPC SendOperatorChatTurn failed")?;

    match response {
        IpcResponse::OperatorChatTurnReply {
            operator_chat_reply,
        } => {
            let reply = &operator_chat_reply.content;
            eprintln!("smoke: turn completed ✓");
            eprintln!("smoke: turn_id={}", operator_chat_reply.turn_id);
            eprintln!("smoke: session_id={}", operator_chat_reply.session_id);
            eprintln!("smoke: reply_action={}", operator_chat_reply.reply_action);
            eprintln!(
                "smoke: observed_events={:?}",
                operator_chat_reply.observed_events
            );
            println!("agent turn reply: {reply}");
            if reply.is_empty() {
                bail!("agent turn reply was empty — turn may have failed");
            }
            if reply.contains("not enabled") || reply.contains("could not record") {
                bail!("agent reported life.observe was unavailable: {reply}");
            }
            if !reply.contains("node_id") && !reply.contains("life:") {
                bail!("agent reply did not include LifeGraph evidence id: {reply}");
            }
            println!("agent turn smoke PASSED");
            Ok(())
        }
        other => bail!("unexpected response: {other:?}"),
    }
}

struct SmokeIpc {
    client: philotic_client::PhiloticClient,
}

impl SmokeIpc {
    async fn connect(socket_path: &str, identity: GuestIdentity) -> Result<Self> {
        let client = philotic_client::PhiloticClient::connect_at(socket_path, identity).await?;
        Ok(Self { client })
    }

    async fn send_request(&mut self, req: IpcRequest) -> Result<IpcResponse> {
        self.client
            .send_request(req)
            .await
            .context("IPC send_request failed")
    }
}
