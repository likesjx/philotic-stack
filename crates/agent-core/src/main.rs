mod commands;
mod r#loop;
mod protocol;
mod runtime;
mod session;

use anyhow::Result;
use clap::Parser;
use philotic_client::GuestIdentity;
use runtime::{AgentRuntime, DEFAULT_AGENT_ID};
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

    let identity = GuestIdentity {
        guest_id: agent_id.clone(),
        role: "agent".into(),
        supported_tools: Vec::new(),
    };

    let ipc_client = philotic_client::PhiloticClient::connect(identity).await?;
    let mut runtime = AgentRuntime::new(ipc_client, agent_id);
    runtime.run().await
}
