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

    let (role, guest_id) =
        role_registration(&agent_id, role_name.as_deref(), role_inbox.as_deref());

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

/// The IPC identity a philote registers under.
///
/// A role-incarnation philote (`PHILOTIC_ROLE_NAME` set) must register under
/// the hotel's routing key `role:{agent_id}:{role_name}` — the same string
/// `RoleIncarnationRecord::routing_role()` produces and the only role string
/// (besides `agent`) that `is_agent_handoff_caller` accepts. aiua injects it
/// as `PHILOTIC_ROLE_INBOX` when it materialises a role, but a guest seeded
/// from mesh-config carries only `PHILOTIC_ROLE_NAME`; falling back to the
/// bare role name left it able to receive a handoff but never hand back
/// (live 2026-09-15 16:06 UTC: bjork's theoretician, registered as role
/// "theoretician", got HANDOFF_FORBIDDEN on `handoff.back`, DEF-134).
fn role_registration(
    agent_id: &str,
    role_name: Option<&str>,
    role_inbox: Option<&str>,
) -> (String, String) {
    match role_name {
        Some(rn) => {
            let role = role_inbox
                .map(str::to_string)
                .unwrap_or_else(|| format!("role:{agent_id}:{rn}"));
            (role, format!("{agent_id}:{rn}"))
        }
        None => ("agent".to_string(), agent_id.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::role_registration;

    #[test]
    fn base_philote_registers_as_agent() {
        assert_eq!(
            role_registration("agent-bjork-01", None, None),
            ("agent".to_string(), "agent-bjork-01".to_string())
        );
    }

    #[test]
    fn injected_role_inbox_wins() {
        assert_eq!(
            role_registration(
                "agent-bjork-01",
                Some("theoretician"),
                Some("role:agent-bjork-01:theoretician")
            ),
            (
                "role:agent-bjork-01:theoretician".to_string(),
                "agent-bjork-01:theoretician".to_string()
            )
        );
    }

    /// The mesh-config-seeded guest: role name only. It must still register
    /// under the routing key, not the bare name.
    #[test]
    fn missing_role_inbox_defaults_to_the_routing_role() {
        assert_eq!(
            role_registration("agent-bjork-01", Some("theoretician"), None),
            (
                "role:agent-bjork-01:theoretician".to_string(),
                "agent-bjork-01:theoretician".to_string()
            )
        );
    }
}
