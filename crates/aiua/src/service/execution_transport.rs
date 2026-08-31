use ansible_mesh_core::authz::{MeshAuth, NonceTracker};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::{BeaconMessage, MsgType, NodeCapabilities};
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use crate::mesh::mesh_auth_key_for_node;

pub async fn serve_execution_plane(
    addr: &str,
    local_capabilities: NodeCapabilities,
    inbox_tx: mpsc::Sender<BeaconMessage>,
    graph: Arc<GraphDomain>,
    db_path: &str,
    enable_rust_auth: bool,
) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind execution transport to {}", addr))?;
    let db_path = db_path.to_string();

    loop {
        let (mut stream, peer_addr) = listener.accept().await?;
        let inbox_tx = inbox_tx.clone();
        let local_node_id = local_capabilities.node_id.clone();
        let graph = graph.clone();
        let db_path = db_path.clone();

        tokio::spawn(async move {
            match read_execution_message(&mut stream).await {
                Ok(msg) => {
                    if let Err(err) = validate_execution_message(
                        &msg,
                        &local_node_id,
                        graph.as_ref(),
                        &db_path,
                        enable_rust_auth,
                    ) {
                        warn!(
                            "Execution transport dropped message {} from {}: {}",
                            msg.msg_id, peer_addr, err
                        );
                        return;
                    }

                    match msg.msg_type {
                        MsgType::ExecutionEventBatch
                        | MsgType::ExecutionEventAck
                        | MsgType::MeshCatalogSync => {
                            if inbox_tx.send(msg).await.is_err() {
                                warn!(
                                    "Execution transport could not forward message from {} because inbox receiver was closed",
                                    peer_addr
                                );
                            }
                        }
                        other => {
                            debug!(
                                "Execution transport ignoring unsupported message type {:?} from {}",
                                other, peer_addr
                            );
                        }
                    }
                }
                Err(err) => {
                    error!(
                        "Execution transport failed to read message from {}: {}",
                        peer_addr, err
                    );
                }
            }
        });
    }
}

pub async fn send_execution_message(target_addr: &str, msg: &BeaconMessage) -> Result<()> {
    let mut stream = TcpStream::connect(target_addr)
        .await
        .with_context(|| format!("Failed to connect execution transport to {}", target_addr))?;
    let packet = serde_json::to_vec(msg)?;
    let len = u32::try_from(packet.len()).context("execution packet exceeded u32 length")?;
    stream.write_u32(len).await?;
    stream.write_all(&packet).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_execution_message(stream: &mut TcpStream) -> Result<BeaconMessage> {
    let len = stream.read_u32().await?;
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice::<BeaconMessage>(&buf)?)
}

fn validate_execution_message(
    msg: &BeaconMessage,
    local_node_id: &str,
    graph: &GraphDomain,
    db_path: &str,
    enable_rust_auth: bool,
) -> Result<()> {
    if msg.src_node == local_node_id {
        anyhow::bail!("discarded self-originated execution message");
    }

    if enable_rust_auth {
        let auth_key = mesh_auth_key_for_node(graph, local_node_id, &msg.src_node)?
            .ok_or_else(|| anyhow::anyhow!("no mesh auth key for node {}", msg.src_node))?;
        let auth = MeshAuth::new(auth_key);
        auth.validate(
            &msg.msg_id,
            msg.seq as u64,
            &msg.payload,
            msg.timestamp,
            &msg.hmac,
        )?;
        let tracker = NonceTracker::open(db_path)?;
        tracker.assert_and_record_nonce(&msg.msg_id)?;
    }

    Ok(())
}
