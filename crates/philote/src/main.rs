use agent_core::runtime::{AgentRuntime, DEFAULT_AGENT_ID};
use anyhow::Result;
use clap::Parser;
use philotic_client::GuestIdentity;
use tracing::info;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value_t = 9000)]
    ansible_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let _args = Args::parse();

    info!("Starting Materialized Persona (Agent Core) Guest Process...");

    let agent_id = std::env::var("PHILOTIC_AGENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_AGENT_ID.to_string());

    // When PHILOTIC_ROLE_NAME is set, this philote is a role-specific incarnation
    // materialized by the hotel for paracrine dispatch. It registers with that role
    // so the hotel's inbox registry routes tasks correctly.
    // guest_id is "{agent_id}:{role_name}" to match the RoleIncarnationRecord.guest_id.
    let role_name = std::env::var("PHILOTIC_ROLE_NAME")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    // PHILOTIC_ROLE_INBOX is injected by aiua as "role:{agent_id}:{role_name}".
    // Use it as the IPC subscription key so the hotel's role_route_is_live check matches.
    let role_inbox = std::env::var("PHILOTIC_ROLE_INBOX")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    let (role, guest_id) = match role_name {
        Some(ref rn) => {
            let role = role_inbox.unwrap_or_else(|| rn.clone());
            (role, format!("{}:{}", agent_id, rn))
        }
        None => ("agent".to_string(), agent_id.clone()),
    };

    let identity = GuestIdentity {
        guest_id: guest_id.clone(),
        role: role.clone(),
        supported_tools: Vec::new(),
    };

    if role_name.is_some() {
        info!(
            "Starting as role-incarnation philote: agent={} role={} guest_id={}",
            agent_id, role, guest_id
        );
    }

    let mut ipc_client = philotic_client::PhiloticClient::connect(identity).await?;

    // Role incarnation philotes subscribe to "role:{agent}:{role_name}" for handoff delivery,
    // but also need to be in the "agent" subscribers so aiua's is_registered check (which
    // reads inboxes["agent"]) can find them for subsequent turn routing after a handoff.
    if role_name.is_some() {
        ipc_client
            .send_request(philotic_client::IpcRequest::SubscribeInbox {
                role: "agent".to_string(),
            })
            .await?;
    }

    let mut runtime = AgentRuntime::new(ipc_client, agent_id);
    if let Some(ref rn) = role_name {
        runtime.set_role_name(rn.clone());
    }
    runtime.run().await
}
