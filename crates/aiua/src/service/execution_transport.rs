use ansible_mesh_core::authz::{MeshAuth, NonceTracker};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::{BeaconMessage, MsgType, NodeCapabilities};
use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, error, warn};

use crate::mesh::mesh_auth_key_for_node;

/// Longest a connect may take. A peer that is asleep or off the tailnet
/// black-holes the SYN, and without a bound the OS waits ~75 s (macOS) to
/// ~127 s (Linux) — and everything queued behind it waits too (DEF-181).
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Base allowance for writing one frame; large frames get one more second per
/// megabyte on top.
const WRITE_TIMEOUT_BASE: Duration = Duration::from_secs(5);

/// Longest a peer may take to deliver one complete frame.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Largest frame either side will send or accept. The biggest legitimate
/// message is the ~826 KB model catalog (~1.1 MB on the wire) and a continuity
/// bundle capped at 8 MB inline; the length prefix used to be allocated as
/// claimed (up to 4 GiB) BEFORE authentication.
pub(crate) const MAX_EXECUTION_FRAME_BYTES: u32 = 32 * 1024 * 1024;

/// Simultaneous inbound connections. Each is a short-lived frame; more than
/// this at once is a flood or a stuck peer.
const MAX_INBOUND_CONNECTIONS: usize = 64;

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
    // The replay window is in memory now; drop the SQLite table it replaced
    // (826k rows on mac-jane, written on every inbound message).
    let legacy_db = db_path.to_string();
    if let Ok(Some(what)) =
        tokio::task::spawn_blocking(move || NonceTracker::retire_legacy_store(&legacy_db)).await
    {
        tracing::info!("{what}");
    }

    let permits = Arc::new(Semaphore::new(MAX_INBOUND_CONNECTIONS));
    loop {
        // A transient accept error (fd exhaustion, an aborted connection) used
        // to end this function with `?` and switch cross-hotel delivery off
        // until the next restart. Log it and keep serving.
        let (mut stream, peer_addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                warn!("Execution transport accept failed (continuing): {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            warn!(
                "Execution transport at its {MAX_INBOUND_CONNECTIONS}-connection cap; refusing {peer_addr}"
            );
            continue;
        };
        let inbox_tx = inbox_tx.clone();
        let local_node_id = local_capabilities.node_id.clone();
        let graph = graph.clone();

        tokio::spawn(async move {
            let _permit = permit;
            match read_execution_message(&mut stream).await {
                Ok(msg) => {
                    if let Err(err) = validate_execution_message(
                        &msg,
                        &local_node_id,
                        graph.as_ref(),
                        enable_rust_auth,
                    ) {
                        warn!(
                            "Execution transport dropped message {} from {}: {}",
                            msg.msg_id, peer_addr, err
                        );
                        return;
                    }

                    match msg.msg_type {
                        MsgType::ExecutionEventBatch | MsgType::ExecutionEventAck => {
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
    let packet = serde_json::to_vec(msg)?;
    let len = u32::try_from(packet.len()).context("execution packet exceeded u32 length")?;
    anyhow::ensure!(
        len <= MAX_EXECUTION_FRAME_BYTES,
        "execution frame of {len} bytes exceeds the {MAX_EXECUTION_FRAME_BYTES}-byte cap"
    );
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(target_addr))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "timed out connecting execution transport to {target_addr} after {CONNECT_TIMEOUT:?}"
            )
        })?
        .with_context(|| format!("Failed to connect execution transport to {}", target_addr))?;
    let _ = stream.set_nodelay(true);
    write_frame(&mut stream, &packet, write_timeout_for(len)).await
}

/// Write one length-prefixed frame within `timeout` (a peer that stops
/// reading stalls `write_all` for minutes otherwise).
async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    packet: &[u8],
    timeout: Duration,
) -> Result<()> {
    let len = u32::try_from(packet.len()).context("execution packet exceeded u32 length")?;
    tokio::time::timeout(timeout, async {
        writer.write_u32(len).await?;
        writer.write_all(packet).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out writing an execution frame after {timeout:?}"))??;
    Ok(())
}

fn write_timeout_for(len: u32) -> Duration {
    WRITE_TIMEOUT_BASE + Duration::from_secs(u64::from(len) / 1_000_000)
}

async fn read_execution_message(stream: &mut TcpStream) -> Result<BeaconMessage> {
    read_frame(stream, MAX_EXECUTION_FRAME_BYTES, READ_TIMEOUT).await
}

/// Read one length-prefixed frame. The claimed length is checked against
/// `max_bytes` before anything is allocated, memory grows only as bytes
/// actually arrive, and the whole frame must arrive within `timeout` — a
/// peer that opens a connection and stalls (or lies about its length) ties up
/// one task for at most that long.
async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_bytes: u32,
    timeout: Duration,
) -> Result<BeaconMessage> {
    tokio::time::timeout(timeout, async {
        let len = reader.read_u32().await?;
        anyhow::ensure!(
            len <= max_bytes,
            "execution frame of {len} bytes exceeds the {max_bytes}-byte cap"
        );
        let mut buf = Vec::new();
        let read = (&mut *reader)
            .take(u64::from(len))
            .read_to_end(&mut buf)
            .await?;
        anyhow::ensure!(
            read as u64 == u64::from(len),
            "execution frame truncated: got {read} of {len} bytes"
        );
        Ok(serde_json::from_slice::<BeaconMessage>(&buf)?)
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out reading an execution frame after {timeout:?}"))?
}

fn validate_execution_message(
    msg: &BeaconMessage,
    local_node_id: &str,
    graph: &GraphDomain,
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
        // The process-wide in-memory window — not a SQLite connection opened
        // against the live hotel DB for every inbound message.
        NonceTracker::shared().assert_and_record_nonce(&msg.msg_id)?;
    }

    Ok(())
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use ansible_mesh_core::MsgType;

    fn message() -> BeaconMessage {
        BeaconMessage {
            version: 1,
            msg_id: uuid::Uuid::new_v4(),
            src_node: "a".into(),
            dest_node: "b".into(),
            msg_type: MsgType::ExecutionEventBatch,
            seq: 1,
            total: 1,
            payload: b"hello".to_vec().into(),
            timestamp: 1,
            hmac: vec![1, 2, 3].into(),
        }
    }

    #[tokio::test]
    async fn a_well_formed_frame_round_trips() {
        let (mut client, mut server) = tokio::io::duplex(64 * 1024);
        let packet = serde_json::to_vec(&message()).unwrap();
        write_frame(&mut client, &packet, Duration::from_secs(1))
            .await
            .unwrap();
        let got = read_frame(&mut server, 1024 * 1024, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(got.src_node, "a");
    }

    /// The length prefix is checked before anything is allocated: a peer
    /// claiming 4 GiB is refused on the spot.
    #[tokio::test]
    async fn an_oversized_length_prefix_is_refused_before_allocating() {
        let (mut client, mut server) = tokio::io::duplex(64);
        client.write_u32(u32::MAX).await.unwrap();
        let err = read_frame(&mut server, 1024, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");
    }

    /// A peer that connects and says nothing (or trickles) is cut off.
    #[tokio::test]
    async fn a_stalled_sender_times_out_instead_of_holding_the_task() {
        let (_client, mut server) = tokio::io::duplex(64);
        let started = std::time::Instant::now();
        let err = read_frame(&mut server, 1024, Duration::from_millis(150))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_frame_cut_short_is_an_error_not_a_hang() {
        let (mut client, mut server) = tokio::io::duplex(64 * 1024);
        client.write_u32(100).await.unwrap();
        client.write_all(b"only ten b").await.unwrap();
        drop(client);
        let err = read_frame(&mut server, 1024, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("truncated"), "{err}");
    }

    /// A peer that stops reading cannot stall the writer past its timeout.
    #[tokio::test]
    async fn a_peer_that_stops_reading_cannot_stall_a_write() {
        let (mut client, _server) = tokio::io::duplex(16);
        let started = std::time::Instant::now();
        let err = write_frame(&mut client, &[7u8; 4096], Duration::from_millis(150))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Whatever the network does with the SYN — black-holes it, refuses it,
    /// reports no route — the connect is bounded by CONNECT_TIMEOUT rather than
    /// the OS's 75-127 s.
    #[tokio::test]
    async fn a_connect_to_an_unresponsive_peer_is_bounded() {
        let started = std::time::Instant::now();
        let result = send_execution_message("10.255.255.1:9", &message()).await;
        assert!(result.is_err());
        assert!(
            started.elapsed() < CONNECT_TIMEOUT + Duration::from_secs(2),
            "connect took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn large_frames_get_proportionally_more_write_time() {
        assert_eq!(write_timeout_for(1_000), Duration::from_secs(5));
        assert_eq!(write_timeout_for(11_000_000), Duration::from_secs(16));
    }
}
