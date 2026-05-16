use anyhow::Result;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

use crate::init::{active_profile, profile_dir};
use crate::start::{pid_path, process_alive, read_pid, socket_path};

pub async fn run(config: Option<PathBuf>, hotel: String) -> Result<()> {
    let default_config = match active_profile() {
        Some(_) => profile_dir().join("config.json"),
        None => PathBuf::from("mesh-config.json"),
    };
    let config_path = config.unwrap_or(default_config);

    // ── Daemon status ──────────────────────────────────────────────────────
    println!("aiua daemon");
    println!("───────────────────────────────────────");
    if let Some(ref p) = active_profile() {
        println!("  profile  {p}  ({})", profile_dir().display());
    }

    let daemon_alive = match read_pid() {
        Some(pid) if process_alive(pid) => {
            println!("  status   running  (pid {pid})");
            println!("  pidfile  {}", pid_path().display());
            true
        }
        Some(_) => {
            println!("  status   stopped  (stale pidfile)");
            false
        }
        None => {
            println!("  status   stopped");
            false
        }
    };

    // ── IPC health check ───────────────────────────────────────────────────
    let socket_path = socket_path(&hotel);

    if daemon_alive {
        println!("\nipc");
        println!("───────────────────────────────────────");
        match probe_ipc(&socket_path).await {
            Ok(msg) => println!("  status   ok  — {msg}"),
            Err(e) => println!("  status   unreachable  ({e})"),
        }
    }

    // ── Configured agents ─────────────────────────────────────────────────
    println!();
    print_agents_from_config(&config_path);

    Ok(())
}

pub async fn run_agents(config: Option<PathBuf>) -> Result<()> {
    let config_path = config.unwrap_or_else(|| PathBuf::from("mesh-config.json"));
    print_agents_from_config(&config_path);
    Ok(())
}

// ── IPC probe ─────────────────────────────────────────────────────────────

async fn probe_ipc(socket_path: &str) -> Result<String> {
    let identity = GuestIdentity {
        guest_id: "philotic-web-status".into(),
        role: "management".into(),
        supported_tools: vec![],
    };
    let mut client = PhiloticClient::connect_at(socket_path, identity).await?;
    let resp = client
        .send_request(IpcRequest::QueryStatus {
            task_id: Uuid::new_v4(),
        })
        .await?;
    match resp {
        IpcResponse::Standard {
            ok: true, message, ..
        } => Ok(if message.is_empty() {
            "ok".into()
        } else {
            message
        }),
        IpcResponse::Standard { message, .. } => Ok(if message.is_empty() {
            "responded".into()
        } else {
            message
        }),
        _ => Ok("responded".into()),
    }
}

// ── Config parsing ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MeshConfig {
    hotels: HashMap<String, HotelConfig>,
}

#[derive(Deserialize)]
struct HotelConfig {
    agents: HashMap<String, AgentConfig>,
}

#[derive(Deserialize)]
struct AgentConfig {
    persona_name: Option<String>,
    agent_id: Option<String>,
    system_prompt: Option<String>,
    import_workspace: Option<String>,
    #[serde(default)]
    default_skillset: Vec<String>,
    #[serde(default)]
    response_route_policy: Option<ResponseRoutePolicyConfig>,
    #[serde(default)]
    toolset_tags: Vec<String>,
}

#[derive(Deserialize)]
struct ResponseRoutePolicyConfig {
    default_route: Option<String>,
}

fn print_agents_from_config(config_path: &PathBuf) {
    println!("agents (from {})", config_path.display());
    println!("───────────────────────────────────────");

    let raw = match std::fs::read_to_string(config_path) {
        Ok(s) => s,
        Err(e) => {
            println!("  (cannot read config: {e})");
            return;
        }
    };
    let config: MeshConfig = match serde_json::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            println!("  (cannot parse config: {e})");
            return;
        }
    };

    for (hotel_name, hotel) in &config.hotels {
        if config.hotels.len() > 1 {
            println!("  hotel: {hotel_name}");
        }
        if hotel.agents.is_empty() {
            println!("  (no agents configured)");
            continue;
        }
        for (key, agent) in &hotel.agents {
            let name = agent.persona_name.as_deref().unwrap_or(key.as_str());
            let id = agent.agent_id.as_deref().unwrap_or("(no id)");
            let tags = if agent.toolset_tags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", agent.toolset_tags.join(", "))
            };
            let blurb = agent
                .system_prompt
                .as_deref()
                .and_then(|s| s.split('.').next())
                .unwrap_or("");
            println!("  {name:<16} {id:<24}{tags}");
            if !blurb.is_empty() {
                println!("    {blurb}.");
            }
            if let Some(workspace) = agent
                .import_workspace
                .as_deref()
                .map(str::trim)
                .filter(|workspace| !workspace.is_empty())
            {
                println!("    workspace: {workspace}");
            }
            if !agent.default_skillset.is_empty() {
                println!("    skills: {}", agent.default_skillset.join(", "));
            }
            if let Some(route) = agent
                .response_route_policy
                .as_ref()
                .and_then(|policy| policy.default_route.as_deref())
            {
                println!("    route: {route}");
            }
        }
    }
}
