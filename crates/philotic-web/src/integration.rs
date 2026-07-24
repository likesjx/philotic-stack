use std::io::Read;
use std::path::PathBuf;

use ansible_mesh_core::integration::IntegrationBinding;
use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};

use crate::start::socket_path;

#[derive(Subcommand, Debug)]
pub enum IntegrationAction {
    /// List bindings and live placement decisions. Never prints secret values.
    List {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Register or update a complete IntegrationBinding JSON document.
    Apply { binding_file: PathBuf },
    /// Revoke a binding. Owner is explicit so operator intent is auditable.
    Remove {
        binding_id: String,
        #[arg(long)]
        owner: String,
    },
    /// Store or rotate a credential at the binding's selected execution hotel.
    ///
    /// Use `-` to read from stdin. A command-line plaintext flag is
    /// intentionally absent so shell history cannot become a surprise vault.
    SetCredential {
        binding_id: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        credential_file: PathBuf,
    },
    /// Show recent secret-free, content-free execution audit records.
    Audit {
        #[arg(long)]
        binding_id: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(action: IntegrationAction) -> Result<()> {
    match action {
        IntegrationAction::List { json } => list(json).await,
        IntegrationAction::Apply { binding_file } => apply(binding_file).await,
        IntegrationAction::Remove { binding_id, owner } => remove(binding_id, owner).await,
        IntegrationAction::SetCredential {
            binding_id,
            owner,
            credential_file,
        } => set_credential(binding_id, owner, credential_file).await,
        IntegrationAction::Audit {
            binding_id,
            limit,
            json,
        } => audit(binding_id, limit, json).await,
    }
}

async fn audit(binding_id: Option<String>, limit: u32, json: bool) -> Result<()> {
    let mut client = client().await?;
    match client
        .send_request(IpcRequest::GetIntegrationAudit {
            binding_id,
            limit: Some(limit),
        })
        .await?
    {
        IpcResponse::IntegrationAuditState { integration_audits } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&integration_audits)?);
            } else if integration_audits.is_empty() {
                println!("no matching integration audit records");
            } else {
                for audit in integration_audits {
                    println!(
                        "{}  {} {}{}  node={}  outcome={}  {}ms  redirects={}  credential={}",
                        audit.binding_id,
                        audit.method,
                        audit.target_origin,
                        audit.path,
                        audit.executor_node_id,
                        audit.outcome,
                        audit.finished_at_ms.saturating_sub(audit.started_at_ms),
                        audit.redirect_count,
                        audit.credential_injected
                    );
                }
            }
            Ok(())
        }
        IpcResponse::Standard { message, .. } | IpcResponse::Error(message) => {
            Err(anyhow!(message))
        }
        other => Err(anyhow!("unexpected integration audit response: {other:?}")),
    }
}

async fn client() -> Result<PhiloticClient> {
    let socket = socket_path("aiua");
    PhiloticClient::connect_at(
        &socket,
        GuestIdentity {
            guest_id: "phil-integration".into(),
            role: "operator".into(),
            supported_tools: vec![],
        },
    )
    .await
    .with_context(|| format!("connect to aiua at {socket}"))
}

async fn list(json: bool) -> Result<()> {
    let mut client = client().await?;
    match client
        .send_request(IpcRequest::GetIntegrationBindings {})
        .await?
    {
        IpcResponse::IntegrationBindingsState {
            integration_bindings,
        } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&integration_bindings)?);
            } else if integration_bindings.is_empty() {
                println!("no outbound integration bindings registered");
            } else {
                for entry in integration_bindings {
                    println!(
                        "{}  owner={}  node={}  placement={:?}  reachable={}  tool={}",
                        entry.binding.binding_id,
                        entry.binding.owner_agent_id,
                        entry.execution_node_id.as_deref().unwrap_or("-"),
                        entry.placement,
                        entry.exit_hotel_reachable,
                        entry
                            .binding
                            .projected_tool_name()
                            .as_deref()
                            .unwrap_or("(MCP transport binding)")
                    );
                    if !entry.binding.grant_skills.is_empty() {
                        println!("    skills: {}", entry.binding.grant_skills.join(", "));
                    }
                }
            }
            Ok(())
        }
        IpcResponse::Standard { message, .. } | IpcResponse::Error(message) => {
            Err(anyhow!(message))
        }
        other => Err(anyhow!("unexpected integration list response: {other:?}")),
    }
}

async fn apply(path: PathBuf) -> Result<()> {
    let bytes = std::fs::read(&path)
        .with_context(|| format!("read integration binding {}", path.display()))?;
    let binding: IntegrationBinding = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse integration binding {}", path.display()))?;
    binding
        .validate()
        .map_err(|message| anyhow!("invalid integration binding: {message}"))?;
    let mut client = client().await?;
    match client
        .send_request(IpcRequest::RegisterIntegrationBinding {
            binding: binding.clone(),
        })
        .await?
    {
        IpcResponse::IntegrationBindingRegistered {
            materialized_node_id,
            ..
        } => {
            println!(
                "registered '{}' (runner node: {})",
                binding.binding_id,
                materialized_node_id
                    .as_deref()
                    .unwrap_or("not materialized")
            );
            Ok(())
        }
        IpcResponse::Standard { message, .. } | IpcResponse::Error(message) => {
            Err(anyhow!(message))
        }
        other => Err(anyhow!("unexpected integration apply response: {other:?}")),
    }
}

async fn remove(binding_id: String, owner: String) -> Result<()> {
    let mut client = client().await?;
    match client
        .send_request(IpcRequest::RevokeIntegrationBinding {
            binding_id: binding_id.clone(),
            owner_agent_id: owner,
        })
        .await?
    {
        IpcResponse::IntegrationBindingRegistered { .. } => {
            println!("revoked '{binding_id}'");
            Ok(())
        }
        IpcResponse::Standard { message, .. } | IpcResponse::Error(message) => {
            Err(anyhow!(message))
        }
        other => Err(anyhow!("unexpected integration remove response: {other:?}")),
    }
}

async fn set_credential(binding_id: String, owner: String, path: PathBuf) -> Result<()> {
    let mut credential = String::new();
    if path.as_os_str() == "-" {
        std::io::stdin()
            .read_to_string(&mut credential)
            .context("read credential from stdin")?;
    } else {
        credential = std::fs::read_to_string(&path)
            .with_context(|| format!("read credential file {}", path.display()))?;
    }
    let credential = credential.trim_end_matches(['\r', '\n']).to_string();
    if credential.is_empty() {
        anyhow::bail!("credential input is empty");
    }
    let mut client = client().await?;
    match client
        .send_request(IpcRequest::ProvisionIntegrationCredential {
            binding_id: binding_id.clone(),
            owner_agent_id: owner,
            credential,
        })
        .await?
    {
        IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        } => {
            println!(
                "credential {} for '{}' at node '{}'",
                if data
                    .get("rotated")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    "rotated"
                } else {
                    "stored"
                },
                binding_id,
                data.get("execution_node_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
            );
            Ok(())
        }
        IpcResponse::Standard { message, .. } | IpcResponse::Error(message) => {
            Err(anyhow!(message))
        }
        other => Err(anyhow!(
            "unexpected integration credential response: {other:?}"
        )),
    }
}
