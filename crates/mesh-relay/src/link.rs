//! [`RelayLink`]: the always-on client lifecycle a hotel runs.
//!
//! A hotel opens ONE persistent outbound QUIC connection to the relay (dialing
//! the relay's public address, so it survives Tailscale being up or down) and
//! keeps it alive: reconnect with capped backoff on any drop, pump outbound
//! frames to the relay, and hand delivered frames back for injection into the
//! local inbox.
//!
//! This is deliberately opaque about mesh semantics — it moves
//! `(node_id, plane, bytes)` triples. The aiua glue serializes a `BeaconMessage`
//! into the bytes on the way out and injects the delivered bytes into the inbox
//! on the way in; that glue lives in the hotel, not here.

use crate::protocol::{Plane, ServerMsg};
use crate::quic::connect_relay;
use crate::transport::decode_inner;
use ed25519_dalek::SigningKey;
use rustls::pki_types::CertificateDer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// A frame delivered to this hotel via the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    pub from_node_id: String,
    pub plane: Plane,
    pub inner: Vec<u8>,
}

/// An outbound send request queued on the link.
struct Outbound {
    to_node_id: String,
    plane: Plane,
    inner: Vec<u8>,
}

/// Connection parameters for the relay link.
#[derive(Clone)]
pub struct RelayLinkConfig {
    /// The relay's reachable address — in production the vps **public** IP:port,
    /// so the link works whether or not Tailscale is up.
    pub relay_addr: SocketAddr,
    /// The relay's pinned self-signed certificate (distributed out of band).
    pub pinned_cert: CertificateDer<'static>,
    /// This hotel's mesh node id.
    pub node_id: String,
    /// This hotel's ed25519 member signing key (proves identity to the relay).
    pub signing_key: Arc<SigningKey>,
    /// First reconnect delay; doubles up to `max_backoff`.
    pub base_backoff: Duration,
    /// Cap on the reconnect delay.
    pub max_backoff: Duration,
}

impl RelayLinkConfig {
    pub fn new(
        relay_addr: SocketAddr,
        pinned_cert: CertificateDer<'static>,
        node_id: String,
        signing_key: Arc<SigningKey>,
    ) -> Self {
        Self {
            relay_addr,
            pinned_cert,
            node_id,
            signing_key,
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }
}

/// Handle to a running relay link. Cloneable; dropping all clones does not stop
/// the link (the supervisor task owns its own lifecycle) — hold the returned
/// [`RelayLink`] to keep it referenced.
#[derive(Clone)]
pub struct RelayLink {
    outbound: mpsc::Sender<Outbound>,
}

impl RelayLink {
    /// Start the link supervisor. Returns the handle plus the receiver for
    /// frames delivered to this hotel. The supervisor runs until the process
    /// exits; it reconnects on its own after any drop.
    pub fn spawn(config: RelayLinkConfig) -> (RelayLink, mpsc::Receiver<Delivered>) {
        // Bounded so a stalled relay applies backpressure rather than growing
        // unboundedly; the caller falls back to the direct path when full.
        let (outbound_tx, outbound_rx) = mpsc::channel::<Outbound>(1024);
        let (delivered_tx, delivered_rx) = mpsc::channel::<Delivered>(1024);
        tokio::spawn(supervisor(config, outbound_rx, delivered_tx));
        (
            RelayLink {
                outbound: outbound_tx,
            },
            delivered_rx,
        )
    }

    /// Queue a frame for relay delivery to `to_node_id`. Best-effort: returns an
    /// error if the outbound queue is full (relay stalled) so the caller can
    /// fall back to the direct path rather than block the dispatcher.
    pub fn try_send(
        &self,
        to_node_id: &str,
        plane: Plane,
        inner: Vec<u8>,
    ) -> Result<(), RelaySendError> {
        self.outbound
            .try_send(Outbound {
                to_node_id: to_node_id.to_string(),
                plane,
                inner,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => RelaySendError::Full,
                mpsc::error::TrySendError::Closed(_) => RelaySendError::Closed,
            })
    }
}

/// Why a [`RelayLink::try_send`] could not enqueue.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RelaySendError {
    #[error("relay outbound queue full (relay stalled) — fall back to direct")]
    Full,
    #[error("relay link supervisor stopped")]
    Closed,
}

/// Next backoff delay: double, capped.
fn next_backoff(current: Duration, max: Duration) -> Duration {
    (current * 2).min(max)
}

async fn supervisor(
    config: RelayLinkConfig,
    mut outbound_rx: mpsc::Receiver<Outbound>,
    delivered_tx: mpsc::Sender<Delivered>,
) {
    let mut backoff = config.base_backoff;
    loop {
        match connect_relay(
            config.relay_addr,
            config.pinned_cert.clone(),
            config.node_id.clone(),
            &config.signing_key,
        )
        .await
        {
            Ok(client) => {
                info!(
                    node_id = %config.node_id,
                    relay = %config.relay_addr,
                    "relay link connected"
                );
                backoff = config.base_backoff; // reset on a good connection
                let (mut writer, mut reader) = client.into_split();

                // Receive pump: deliver inbound frames until the stream closes.
                let delivered_tx2 = delivered_tx.clone();
                let mut recv_task = tokio::spawn(async move {
                    loop {
                        match reader.recv().await {
                            Ok(Some(ServerMsg::Deliver {
                                from_node_id,
                                plane,
                                inner_b64,
                            })) => match decode_inner(&inner_b64) {
                                Ok(inner) => {
                                    if delivered_tx2
                                        .send(Delivered {
                                            from_node_id,
                                            plane,
                                            inner,
                                        })
                                        .await
                                        .is_err()
                                    {
                                        break; // consumer gone
                                    }
                                }
                                Err(e) => warn!("relay link: bad delivered frame: {e}"),
                            },
                            Ok(Some(ServerMsg::Undeliverable { to_node_id })) => {
                                debug!("relay link: {to_node_id} not reachable via relay");
                            }
                            Ok(Some(other)) => {
                                debug!("relay link: unexpected post-auth message {other:?}");
                            }
                            Ok(None) => break, // relay closed the stream
                            Err(e) => {
                                debug!("relay link: recv error: {e}");
                                break;
                            }
                        }
                    }
                });

                // Send pump: forward queued outbound frames until the relay
                // errors or the outbound channel closes.
                loop {
                    tokio::select! {
                        maybe = outbound_rx.recv() => match maybe {
                            Some(o) => {
                                if let Err(e) = writer.send(&o.to_node_id, o.plane, &o.inner).await {
                                    warn!("relay link: send failed, reconnecting: {e}");
                                    break;
                                }
                            }
                            None => {
                                // All handles dropped: nothing more to send.
                                recv_task.abort();
                                return;
                            }
                        },
                        _ = &mut recv_task => {
                            // Receive side ended (stream closed) — reconnect.
                            break;
                        }
                    }
                }
                recv_task.abort();
            }
            Err(e) => {
                warn!(
                    node_id = %config.node_id,
                    relay = %config.relay_addr,
                    backoff_secs = backoff.as_secs(),
                    "relay link connect failed: {e}"
                );
            }
        }

        tokio::time::sleep(backoff).await;
        backoff = next_backoff(backoff, config.max_backoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let max = Duration::from_secs(30);
        assert_eq!(
            next_backoff(Duration::from_secs(1), max),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_backoff(Duration::from_secs(8), max),
            Duration::from_secs(16)
        );
        assert_eq!(next_backoff(Duration::from_secs(20), max), max);
        assert_eq!(next_backoff(max, max), max);
    }
}
