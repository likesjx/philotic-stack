//! Transport-agnostic relay core: length-delimited framing, the server-side
//! connection handler (handshake + routing registry), and the client-side
//! connection (handshake + send/receive).
//!
//! Everything here is written against generic [`AsyncRead`] + [`AsyncWrite`]
//! streams so the same logic drives an in-process `tokio::io::duplex` pair in
//! tests and a real QUIC bidirectional stream in production — the QUIC socket
//! adapter is a thin, separable layer that hands its streams to these types.

use crate::KeyResolver;
use crate::protocol::{ClientMsg, Plane, ServerMsg, sign_challenge, verify_challenge};
use ansible_mesh_core::membership::generate_nonce;
use anyhow::{Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, warn};

/// Hard ceiling on a single framed message. Relayed inner frames are mesh
/// events / beacons (small) plus base64 overhead; anything larger is a bug or an
/// attack, and we refuse rather than allocate it.
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// Write one length-delimited (`u32` big-endian length + JSON body) message.
/// Matches the hotel's own IPC framing convention.
pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(msg)?;
    let len = u32::try_from(body.len()).map_err(|_| anyhow!("frame too large to encode"))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-delimited message. Returns `Ok(None)` on a clean EOF before
/// any bytes of the next frame (peer closed the stream between messages).
pub async fn read_frame<R, T>(r: &mut R) -> Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        bail!("frame length {len} exceeds MAX_FRAME_BYTES {MAX_FRAME_BYTES}");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

/// The relay's routing table: authenticated node_id -> a sender that writes
/// [`ServerMsg`]s onto that node's connection.
type Registry = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<ServerMsg>>>>;

/// A relay instance. Cheap to clone; all clones share one routing table.
#[derive(Clone, Default)]
pub struct RelayServer {
    registry: Registry,
}

impl RelayServer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of currently-registered (authenticated, live) nodes.
    pub async fn connected_node_count(&self) -> usize {
        self.registry.lock().await.len()
    }

    /// Drive one client connection to completion: authenticate it, register it,
    /// then route frames until the stream closes. `resolver` supplies the
    /// trusted published key for the claimed node — it must NOT come from the
    /// client. Returns the authenticated node_id when the connection ends
    /// cleanly.
    pub async fn handle_connection<S>(
        &self,
        stream: S,
        resolver: &dyn KeyResolver,
    ) -> Result<String>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (mut read_half, write_half) = tokio::io::split(stream);

        // ── Handshake ──────────────────────────────────────────────────────
        let node_id = match read_frame::<_, ClientMsg>(&mut read_half).await? {
            Some(ClientMsg::Hello { node_id }) => node_id,
            other => bail!("relay: expected Hello first, got {other:?}"),
        };

        // Per-connection outbound queue, drained by a dedicated writer task so
        // other connections can push ServerMsgs to this node without sharing the
        // write half across tasks.
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerMsg>();
        let mut write_half = write_half;
        let writer = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if let Err(e) = write_frame(&mut write_half, &msg).await {
                    debug!("relay: writer task ending: {e}");
                    break;
                }
            }
        });

        let Some(member_public_key) = resolver.member_public_key(&node_id) else {
            let _ = out_tx.send(ServerMsg::AuthFailed {
                reason: "unknown mesh node".into(),
            });
            writer.abort();
            bail!("relay: rejected unknown node {node_id}");
        };

        let nonce = generate_nonce();
        out_tx
            .send(ServerMsg::Challenge {
                nonce: nonce.clone(),
            })
            .map_err(|_| anyhow!("relay: connection closed before challenge"))?;

        let signature_hex = match read_frame::<_, ClientMsg>(&mut read_half).await? {
            Some(ClientMsg::Auth { signature_hex }) => signature_hex,
            other => {
                writer.abort();
                bail!("relay: expected Auth, got {other:?}");
            }
        };

        if let Err(e) = verify_challenge(&member_public_key, &node_id, &nonce, &signature_hex) {
            let _ = out_tx.send(ServerMsg::AuthFailed {
                reason: "signature verification failed".into(),
            });
            writer.abort();
            bail!("relay: auth failed for {node_id}: {e}");
        }

        out_tx
            .send(ServerMsg::AuthOk)
            .map_err(|_| anyhow!("relay: connection closed after auth"))?;

        // Register (last writer wins if a node reconnects).
        self.registry
            .lock()
            .await
            .insert(node_id.clone(), out_tx.clone());
        debug!("relay: node {node_id} authenticated and registered");

        // ── Routing loop ───────────────────────────────────────────────────
        let route_result = self.route_loop(&mut read_half, &node_id).await;

        // Deregister only if we're still the current entry (a reconnect may have
        // replaced us).
        {
            let mut reg = self.registry.lock().await;
            if reg
                .get(&node_id)
                .is_some_and(|cur| cur.same_channel(&out_tx))
            {
                reg.remove(&node_id);
            }
        }
        writer.abort();
        route_result.map(|_| node_id)
    }

    async fn route_loop<R>(&self, read_half: &mut R, from_node: &str) -> Result<()>
    where
        R: AsyncRead + Unpin,
    {
        while let Some(msg) = read_frame::<_, ClientMsg>(read_half).await? {
            match msg {
                ClientMsg::Relay {
                    to_node_id,
                    plane,
                    inner_b64,
                } => {
                    self.route_frame(from_node, &to_node_id, plane, inner_b64)
                        .await;
                }
                ClientMsg::Hello { .. } | ClientMsg::Auth { .. } => {
                    warn!("relay: {from_node} sent a handshake frame after auth; ignoring");
                }
            }
        }
        Ok(())
    }

    async fn route_frame(
        &self,
        from_node: &str,
        to_node_id: &str,
        plane: Plane,
        inner_b64: String,
    ) {
        let target = self.registry.lock().await.get(to_node_id).cloned();
        match target {
            Some(tx) => {
                let _ = tx.send(ServerMsg::Deliver {
                    from_node_id: from_node.to_string(),
                    plane,
                    inner_b64,
                });
            }
            None => {
                // Tell the sender so it can fall back / retry rather than
                // assuming delivery.
                if let Some(back) = self.registry.lock().await.get(from_node).cloned() {
                    let _ = back.send(ServerMsg::Undeliverable {
                        to_node_id: to_node_id.to_string(),
                    });
                }
                debug!("relay: no live connection for {to_node_id}; frame dropped");
            }
        }
    }
}

/// Client side of a relay connection. Owns the authenticated stream; use
/// [`RelayClient::send`] to forward a frame and [`RelayClient::recv`] to receive
/// delivered frames.
pub struct RelayClient<S> {
    stream: S,
    node_id: String,
}

impl<S> RelayClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Perform the client handshake over `stream`: announce `node_id`, answer
    /// the relay's challenge by signing it with `signing_key`, and confirm
    /// `AuthOk`. Returns the ready client on success.
    pub async fn connect(mut stream: S, node_id: String, signing_key: &SigningKey) -> Result<Self> {
        write_frame(
            &mut stream,
            &ClientMsg::Hello {
                node_id: node_id.clone(),
            },
        )
        .await?;

        let nonce = match read_frame::<_, ServerMsg>(&mut stream).await? {
            Some(ServerMsg::Challenge { nonce }) => nonce,
            Some(ServerMsg::AuthFailed { reason }) => bail!("relay refused connection: {reason}"),
            other => bail!("relay: expected Challenge, got {other:?}"),
        };

        let signature_hex = sign_challenge(signing_key, &node_id, &nonce);
        write_frame(&mut stream, &ClientMsg::Auth { signature_hex }).await?;

        match read_frame::<_, ServerMsg>(&mut stream).await? {
            Some(ServerMsg::AuthOk) => {}
            Some(ServerMsg::AuthFailed { reason }) => bail!("relay auth failed: {reason}"),
            other => bail!("relay: expected AuthOk, got {other:?}"),
        }

        Ok(Self { stream, node_id })
    }

    /// The node_id this client authenticated as.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Forward an opaque, already-signed mesh frame to `to_node_id` on `plane`.
    pub async fn send(&mut self, to_node_id: &str, plane: Plane, inner: &[u8]) -> Result<()> {
        use base64::Engine;
        let inner_b64 = base64::engine::general_purpose::STANDARD.encode(inner);
        write_frame(
            &mut self.stream,
            &ClientMsg::Relay {
                to_node_id: to_node_id.to_string(),
                plane,
                inner_b64,
            },
        )
        .await
    }

    /// Await the next message from the relay (a delivered frame, or an
    /// undeliverable notice). Returns `Ok(None)` when the relay closes the
    /// stream.
    pub async fn recv(&mut self) -> Result<Option<ServerMsg>> {
        read_frame::<_, ServerMsg>(&mut self.stream).await
    }
}

/// Decode a delivered frame's inner bytes (base64 -> the opaque signed mesh
/// frame), for the receiving hotel to inject into its inbox.
pub fn decode_inner(inner_b64: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(inner_b64)
        .map_err(|e| anyhow!("relay: bad inner base64: {e}"))
}
