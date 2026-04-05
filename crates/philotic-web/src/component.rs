use anyhow::{Context, Result};
use philotic_client::{
    ComponentManifest, GuestIdentity, IpcRequest, IpcResponse, PhiloticClient,
};
use std::path::PathBuf;

use crate::start::socket_path;

/// phil component add <manifest.json>
pub async fn add(manifest_path: PathBuf) -> Result<()> {
    let content = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read manifest: {}", manifest_path.display()))?;
    let manifest: ComponentManifest = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse manifest: {}", manifest_path.display()))?;

    println!(
        "Registering component  {}  (role: {})",
        manifest.guest_id, manifest.role
    );
    println!("  hotel:    {}", manifest.hotel);
    println!("  command:  {}", manifest.command);
    if !manifest.args.is_empty() {
        println!("  args:     {:?}", manifest.args);
    }
    println!("  auto_start: {}", manifest.auto_start);

    let socket = socket_path("aiua");
    let mut client = ipc_client(&socket).await?;

    let resp = client
        .send_request(IpcRequest::RegisterComponent { manifest })
        .await
        .context("IPC request failed")?;

    match resp {
        IpcResponse::ComponentRegistered {
            registered_guest_id,
            registered_role,
        } => {
            println!("\n  OK  guest_id={}  role={}", registered_guest_id, registered_role);
        }
        IpcResponse::Standard { ok: false, message, .. } => {
            anyhow::bail!("hotel rejected component registration: {message}");
        }
        IpcResponse::Error(msg) => {
            anyhow::bail!("hotel error: {msg}");
        }
        other => {
            anyhow::bail!("unexpected response from hotel: {:?}", other);
        }
    }
    Ok(())
}

/// phil component list
pub async fn list() -> Result<()> {
    let socket = socket_path("aiua");
    let mut client = ipc_client(&socket).await?;

    let resp = client
        .send_request(IpcRequest::GetConfig {
            key: "__component_list__".to_string(),
        })
        .await
        .context("IPC request failed")?;

    match resp {
        IpcResponse::ConfigData { value_json: Some(json), .. } => {
            let guests: Vec<serde_json::Value> = serde_json::from_str(&json)
                .unwrap_or_else(|_| vec![serde_json::json!({"raw": json})]);
            if guests.is_empty() {
                println!("No components registered.");
            } else {
                println!("{:<20} {:<25} {}", "GUEST ID", "ROLE", "ACTIVE");
                println!("{}", "-".repeat(60));
                for g in &guests {
                    println!(
                        "{:<20} {:<25} {}",
                        g["guest_id"].as_str().unwrap_or("?"),
                        g["role"].as_str().unwrap_or("?"),
                        g["is_active"].as_bool().unwrap_or(false),
                    );
                }
            }
        }
        IpcResponse::ConfigData { value_json: None, .. } => {
            println!("No components registered.");
        }
        other => {
            // Fallback: query via GetConfig per-guest is not yet supported in list mode.
            // The hotel will need a dedicated list endpoint; for now suggest using status.
            eprintln!("Note: component list requires hotel support for __component_list__.");
            eprintln!("  Run `phil status` to see active guests.");
            eprintln!("  (raw response: {:?})", other);
        }
    }
    Ok(())
}

/// phil component remove <guest_id>
pub async fn remove(guest_id: String) -> Result<()> {
    let socket = socket_path("aiua");
    let mut client = ipc_client(&socket).await?;

    let resp = client
        .send_request(IpcRequest::RemoveComponent {
            guest_id: guest_id.clone(),
        })
        .await
        .context("IPC request failed")?;

    match resp {
        IpcResponse::Standard { ok: true, .. } => {
            println!("Component {} removed.", guest_id);
        }
        other => {
            anyhow::bail!("unexpected response: {:?}", other);
        }
    }
    Ok(())
}

async fn ipc_client(socket: &str) -> Result<PhiloticClient> {
    let identity = GuestIdentity {
        guest_id: "philotic-web-component".into(),
        role: "management".into(),
        supported_tools: vec![],
    };
    PhiloticClient::connect_at(socket, identity)
        .await
        .with_context(|| format!("failed to connect to hotel IPC at {socket} — is aiua running?"))
}
