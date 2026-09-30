use anyhow::{bail, Context, Result};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use std::fs;
use std::path::PathBuf;

use crate::start::socket_path;

async fn ipc_client(hotel_name: &str) -> Result<PhiloticClient> {
    let identity = GuestIdentity {
        guest_id: "philotic-web-mesh".into(),
        role: "management".into(),
        supported_tools: vec![],
    };
    let socket = socket_path(hotel_name);
    PhiloticClient::connect_at(&socket, identity)
        .await
        .with_context(|| format!("failed to connect to hotel IPC at {socket}"))
}

pub async fn invite(hotel_name: String, mesh_host: String, out: Option<PathBuf>) -> Result<()> {
    if mesh_host.trim().is_empty() {
        bail!("--host must not be empty");
    }

    let mut client = ipc_client(&hotel_name).await?;
    let response = client
        .send_request(IpcRequest::CreateMeshInvite {
            hotel_name: hotel_name.clone(),
            mesh_host: mesh_host.trim().to_string(),
            ttl_secs: None,
        })
        .await
        .context("IPC request failed")?;

    match response {
        IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        } => {
            let invite_json = data
                .get("invite_json")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow::anyhow!("mesh invite response missing invite_json"))?;
            let fingerprint = data
                .get("fingerprint")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let output_path =
                out.unwrap_or_else(|| PathBuf::from(format!("{hotel_name}-mesh-invite.json")));
            // Owner-only: possession of this invite is what grants mesh
            // membership, and a mesh peer can mutate every reachable hotel. It
            // lands in the CWD, so 0644 would expose it to other local accounts
            // and to anything that later archives the directory.
            crate::init::write_private_file(&output_path, invite_json)?;
            println!("Mesh invite written: {}", output_path.display());
            println!("Inviter: {hotel_name}");
            println!("Mesh endpoint: {}", mesh_host.trim());
            println!("Fingerprint: {}", fingerprint);
            println!("Share this file out-of-band with the hotel you want to trust.");
            Ok(())
        }
        IpcResponse::Standard { message, .. } => {
            bail!("hotel rejected mesh invite generation: {message}");
        }
        IpcResponse::Error(message) => bail!("hotel error: {message}"),
        other => bail!("unexpected response from hotel: {:?}", other),
    }
}

pub async fn accept(invite_path: PathBuf, hotel_name: String, mesh_host: String) -> Result<()> {
    if mesh_host.trim().is_empty() {
        bail!("--host must not be empty");
    }

    let invite_json = fs::read_to_string(&invite_path)
        .with_context(|| format!("read {}", invite_path.display()))?;
    let mut client = ipc_client(&hotel_name).await?;
    let response = client
        .send_request(IpcRequest::AcceptMeshInvite {
            hotel_name: hotel_name.clone(),
            mesh_host: mesh_host.trim().to_string(),
            invite_json,
        })
        .await
        .context("IPC request failed")?;

    match response {
        IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        } => {
            let inviter = data
                .get("inviter_hotel")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let target = data
                .get("target_addr")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            println!("Accepted invite from {}.", inviter);
            println!("Join request sent to {}", target);
            println!(
                "Local hotel {} is now staged for mesh membership once the inviter verifies the proof chain.",
                hotel_name
            );
            Ok(())
        }
        IpcResponse::Standard { message, .. } => {
            bail!("hotel rejected mesh invite acceptance: {message}");
        }
        IpcResponse::Error(message) => bail!("hotel error: {message}"),
        other => bail!("unexpected response from hotel: {:?}", other),
    }
}
