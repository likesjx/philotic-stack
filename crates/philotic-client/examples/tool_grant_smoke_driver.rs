//! Creates a session with a known toolset so the tool-grant smoke has something
//! real to compose a session snapshot against.
//!
//! Used by `scripts/smoke-tool-grants-roundtrip.sh`
//! (`proposal:data-driven-tool-grants-skilldag`). The slash commands below are
//! handled hotel-side, so this needs no model and no philote guest — it only has
//! to make a session exist with the tool bound.

use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use tokio::time::{Duration, timeout};

async fn emit_and_expect(
    client: &mut PhiloticClient,
    session_id: &str,
    chat_id: &str,
    turn_id: &str,
    content: &str,
    expected: &str,
    target_node: &str,
) -> Result<()> {
    client
        .send_request(IpcRequest::EmitTask {
            target_node: target_node.into(),
            target_role: "agent".into(),
            target_guest_id: None,
            task_json: serde_json::json!({
                "source": "smoke",
                "session_id": session_id,
                "turn_id": turn_id,
                "chat_id": chat_id,
                "content": content,
                "final_reply_to": target_node,
                "final_reply_role": "membrane",
                "final_reply_guest_id": "tool-grant-smoke-membrane"
            })
            .to_string(),
        })
        .await?;

    let mut actual = String::new();
    for _ in 0..10 {
        let reply = timeout(Duration::from_secs(5), client.recv_task())
            .await
            .with_context(|| format!("timed out waiting for reply to {content}"))??;
        let IpcResponse::InboundTask { task_json, .. } = reply else {
            continue;
        };
        let payload: serde_json::Value =
            serde_json::from_str(&task_json).context("failed to decode tool grant reply")?;
        if let Some(c) = payload.get("content").and_then(serde_json::Value::as_str) {
            if !c.is_empty() {
                actual = c.to_string();
                break;
            }
        }
    }

    if !actual.contains(expected) {
        bail!("expected reply containing {:?}, got {:?}", expected, actual);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must be set for tool_grant_smoke_driver")?;
    let session_id = std::env::var("PHILOTIC_SMOKE_SESSION_ID")
        .unwrap_or_else(|_| "smoke:tool-grants:agent-jane-01".to_string());
    let chat_id = "tool-grant-smoke-chat";
    let tool = std::env::var("PHILOTIC_SMOKE_TOOL")
        .unwrap_or_else(|_| "life.observe.batch".to_string());

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "tool-grant-smoke-membrane".into(),
        role: "membrane".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect tool grant smoke driver to {socket_path}"))?;

    let target_node =
        std::env::var("PHILOTIC_TARGET_NODE").unwrap_or_else(|_| "local-aiua-01".to_string());

    emit_and_expect(
        &mut client,
        session_id.as_str(),
        chat_id,
        "tool-grant-turn-1",
        &format!("/tools add {tool}"),
        &format!("Tool bindings updated: {tool}."),
        &target_node,
    )
    .await?;

    println!("tool grant smoke session ready: {session_id}");
    Ok(())
}
