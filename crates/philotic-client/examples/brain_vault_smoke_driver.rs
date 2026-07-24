/// Brain vault smoke driver — injects a real inbound turn into Astrid's telegram
/// session (which is configured to the `brain` role on mac-jane) via IPC, exactly
/// like the Telegram membrane does. Demonstrates that brain materializes cross-hotel
/// and actually executes obsidian-cli + mempalace through bash.exec.
///
/// Run against mbp-jane (Astrid's authority hotel):
///   PHILOTIC_HOTEL_SOCKET=~/.philotic/jane/aiua-mbp-jane.sock \
///   cargo run -p philotic-client --example brain_vault_smoke_driver
use anyhow::{Context, Result};
use philotic_client::{GuestIdentity, IpcRequest};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .unwrap_or_else(|_| "/Users/jaredlikes/.philotic/jane/aiua-mbp-jane.sock".to_string());

    let session_id = "telegram:7898847424:agent-astrid".to_string();
    let turn_id = format!("brain-vault-test-{}", Uuid::new_v4().simple());
    let content = "Run these two commands with bash.exec and paste the raw output of each, \
        verbatim, in your reply: (1) `/opt/homebrew/bin/obsidian-cli --help`  \
        (2) `/Users/jaredlikes/.local/bin/mempalace status`. Do not summarize — show the actual output.";

    let payload = serde_json::json!({
        "session_id": session_id,
        "turn_id": turn_id,
        "content": content,
        "command": serde_json::Value::Null,
        "attachments": [],
        "transport": "telegram",
        "sender_id": "7898847424",
        "sender_username": "operator-smoke",
        "raw_transport_event": { "transport": "telegram", "chat_id": "7898847424" },
        "requires_approval": false,
    });

    eprintln!("smoke: connecting to {socket_path}");
    eprintln!("smoke: injecting brain-vault turn  session={session_id} turn={turn_id}");

    let mut client = philotic_client::PhiloticClient::connect_at(
        &socket_path,
        GuestIdentity {
            guest_id: format!("brain-vault-smoke-{}", Uuid::new_v4().simple()),
            role: "brain-vault-smoke".into(),
            supported_tools: Vec::new(),
        },
    )
    .await
    .context("failed to connect IPC socket")?;

    let resp = client
        .send_request(IpcRequest::CreateTask {
            target_role: "agent".into(),
            payload,
        })
        .await
        .context("CreateTask failed")?;

    eprintln!("smoke: CreateTask response = {resp:?}");
    eprintln!(
        "smoke: turn injected — watch mac-jane aiua.log for brain bash.exec / obsidian-cli / mempalace,"
    );
    eprintln!(
        "smoke: and watch the operator's Telegram for brain's reply with the command output."
    );
    Ok(())
}
