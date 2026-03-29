use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must be set for telegram_lease_probe")?;
    let lease_key = std::env::var("PHILOTIC_TELEGRAM_LEASE_KEY")
        .context("PHILOTIC_TELEGRAM_LEASE_KEY must be set")?;

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "telegram-lease-probe".into(),
        role: "membrane".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect lease probe to {socket_path}"))?;

    let response = client
        .send_request(IpcRequest::GetTelegramPollLeaseOwner { lease_key })
        .await
        .context("failed to query telegram poll lease owner")?;

    match response {
        IpcResponse::TelegramPollLeaseStatus { active, lease } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "active": active,
                    "lease": lease,
                }))?
            );
            Ok(())
        }
        other => bail!("unexpected telegram poll lease owner response: {other:?}"),
    }
}
