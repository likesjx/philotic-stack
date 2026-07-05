//! Discord gateway lease, driven by the shared [`membrane::LeaseDriver`].
//!
//! This module owns only the Discord-specific pieces:
//!
//! * the lease key derivation (`discord:gateway:<token fingerprint>`), and
//! * the mapping between the `DiscordGatewayLease` IPC request/response shapes
//!   and the driver's transport-agnostic outcomes.
//!
//! All policy — epoch fencing, re-acquire-in-place on `NeedsReacquire` or renew
//! IPC errors, exponential backoff with reset-on-success, and machine-sleep TTL
//! drift tolerance — lives in the shared driver. The hotel-side IPC surface
//! (`AcquireDiscordGatewayLease` / `RenewDiscordGatewayLease` /
//! `ReleaseDiscordGatewayLease`) is unchanged.
//!
//! Follow-up: membrane-discord does not yet consume `NetworkState` pushes, so
//! the driver's offline-suppression hook (`LeaseDriver::set_online`) is not
//! wired. When NetworkState plumbing lands here, feed transitions through
//! [`DiscordGatewayLease`]'s driver instead of adding a second mechanism.

use anyhow::{Result, bail};
use membrane::{
    LeaseAcquireOutcome, LeaseBackend, LeaseDriver, LeaseDriverConfig, LeaseEvent, LeaseRenewResult,
};
use philotic_client::{IpcRequest, IpcResponse, PhiloticClient};
use sha2::{Digest, Sha256};
use tokio::time::Instant;
use tracing::info;

/// Derive the lease key for a Discord bot token.
/// Uses only the first 16 hex chars of SHA-256(token) — same pattern as Telegram.
pub fn gateway_lease_key(bot_token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bot_token.as_bytes());
    let hash = hasher.finalize();
    let fingerprint = &hex::encode(hash)[..16];
    format!("discord:gateway:{}", fingerprint)
}

/// Map an `AcquireDiscordGatewayLease` response onto the shared driver outcome.
fn map_acquire_response(response: IpcResponse) -> Result<LeaseAcquireOutcome> {
    match response {
        IpcResponse::DiscordGatewayLease {
            granted: true,
            lease: Some(lease),
        } => Ok(LeaseAcquireOutcome::Granted {
            epoch: lease.lease_epoch,
        }),
        IpcResponse::DiscordGatewayLease {
            granted: false,
            lease,
        } => {
            let (owner, epoch) = lease
                .map(|l| (Some(l.owner_guest_id), l.lease_epoch))
                .unwrap_or((None, 0));
            Ok(LeaseAcquireOutcome::Held { owner, epoch })
        }
        other => bail!(
            "Unexpected response to AcquireDiscordGatewayLease: {:?}",
            other
        ),
    }
}

/// Map a `RenewDiscordGatewayLease` response onto the shared driver outcome.
///
/// `granted: false, lease: None` means "no current owner" (the hotel reset or
/// expired the lease) — the driver re-acquires in place instead of tearing the
/// seat down, which is the exact fragility the old hand-rolled renew had.
fn map_renew_response(response: IpcResponse) -> Result<LeaseRenewResult> {
    match response {
        IpcResponse::DiscordGatewayLease {
            granted: true,
            lease: Some(lease),
        } => Ok(LeaseRenewResult::Ok {
            epoch: lease.lease_epoch,
        }),
        IpcResponse::DiscordGatewayLease {
            granted: false,
            lease: None,
        } => Ok(LeaseRenewResult::NeedsReacquire),
        IpcResponse::DiscordGatewayLease {
            granted: false,
            lease: Some(lease),
        } => Ok(LeaseRenewResult::Lost {
            owner: Some(lease.owner_guest_id),
        }),
        other => bail!(
            "Unexpected response to RenewDiscordGatewayLease: {:?}",
            other
        ),
    }
}

/// Short-lived IPC adapter: borrows the client for a single driver tick.
struct DiscordLeaseBackend<'a> {
    ipc: &'a mut PhiloticClient,
    lease_key: &'a str,
    agent_id: &'a str,
}

#[async_trait::async_trait]
impl LeaseBackend for DiscordLeaseBackend<'_> {
    async fn acquire(&mut self) -> Result<LeaseAcquireOutcome> {
        let response = self
            .ipc
            .send_request(IpcRequest::AcquireDiscordGatewayLease {
                lease_key: self.lease_key.to_string(),
                agent_id: self.agent_id.to_string(),
            })
            .await?;
        map_acquire_response(response)
    }

    async fn renew(&mut self, epoch: u64) -> Result<LeaseRenewResult> {
        let response = self
            .ipc
            .send_request(IpcRequest::RenewDiscordGatewayLease {
                lease_key: self.lease_key.to_string(),
                agent_id: self.agent_id.to_string(),
                lease_epoch: epoch,
            })
            .await?;
        map_renew_response(response)
    }

    async fn release(&mut self) -> Result<()> {
        self.ipc
            .send_request(IpcRequest::ReleaseDiscordGatewayLease {
                lease_key: self.lease_key.to_string(),
            })
            .await?;
        Ok(())
    }
}

/// Discord gateway lease seat, backed by the shared [`LeaseDriver`].
///
/// Lifecycle contract for the main loop:
///
/// * [`acquire`](Self::acquire) once at startup (hard-fails so the outer
///   supervisor restarts with a fresh IPC connection, matching the old entry
///   semantics for a truly-dead hotel socket or a split-brain denial).
/// * Await [`next_deadline`](Self::next_deadline) and call
///   [`tick`](Self::tick) when it fires. Renewals, re-acquires, and error
///   backoff are all scheduled by the driver; only [`LeaseEvent::Lost`] is
///   terminal (another holder owns the token — yield the seat).
/// * [`release`](Self::release) on clean shutdown.
pub struct DiscordGatewayLease {
    lease_key: String,
    agent_id: String,
    driver: LeaseDriver,
}

impl DiscordGatewayLease {
    /// Acquire the gateway lease. If another instance holds the lease, this
    /// fails — operator must ensure only one membrane-discord instance runs
    /// per bot token. IPC errors also fail here (the outer supervisor loop
    /// reconnects and retries with backoff).
    pub async fn acquire(
        ipc: &mut PhiloticClient,
        bot_token: &str,
        agent_id: &str,
    ) -> Result<Self> {
        let lease_key = gateway_lease_key(bot_token);
        info!(
            "Acquiring Discord gateway lease [{}] for agent [{}]",
            lease_key, agent_id
        );

        let mut seat = Self {
            lease_key,
            agent_id: agent_id.to_string(),
            driver: LeaseDriver::new(LeaseDriverConfig::default()),
        };
        match seat.tick(ipc).await {
            LeaseEvent::Acquired { epoch } => {
                info!(
                    "Discord gateway lease [{}] granted — epoch {}",
                    seat.lease_key, epoch
                );
                Ok(seat)
            }
            LeaseEvent::Lost { owner } => bail!(
                "Discord gateway lease [{}] denied — currently held by [{}]. \
                 Only one membrane-discord instance may run per bot token.",
                seat.lease_key,
                owner.as_deref().unwrap_or("unknown")
            ),
            LeaseEvent::BackingOff { error, .. } => bail!(
                "Discord gateway lease [{}] acquire failed: {}",
                seat.lease_key,
                error
            ),
            other => bail!("Unexpected lease driver event during acquire: {:?}", other),
        }
    }

    /// Run one lease state-machine step. Never fails: IPC errors surface as
    /// [`LeaseEvent::BackingOff`] and are retried on the driver's schedule.
    pub async fn tick(&mut self, ipc: &mut PhiloticClient) -> LeaseEvent {
        let mut backend = DiscordLeaseBackend {
            ipc,
            lease_key: &self.lease_key,
            agent_id: &self.agent_id,
        };
        self.driver.tick(&mut backend).await
    }

    /// When the main loop should next call [`tick`](Self::tick).
    /// `None` once the lease is lost or released (terminal).
    pub fn next_deadline(&self) -> Option<Instant> {
        self.driver.next_deadline()
    }

    /// Release the lease on clean shutdown.
    pub async fn release(&mut self, ipc: &mut PhiloticClient) -> Result<()> {
        info!("Releasing Discord gateway lease [{}]", self.lease_key);
        let mut backend = DiscordLeaseBackend {
            ipc,
            lease_key: &self.lease_key,
            agent_id: &self.agent_id,
        };
        self.driver.release(&mut backend).await
    }
}

// sha2 doesn't re-export hex — pull in the hex crate via a small inline helper
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use philotic_client::{LeaseEnvelope, LeaseStatus};
    use std::collections::VecDeque;
    use tokio::time::{Duration, advance};

    fn envelope(owner: &str, epoch: u64) -> LeaseEnvelope {
        LeaseEnvelope {
            lease_type: "discord_gateway".into(),
            lease_scope: "discord:gateway:deadbeefdeadbeef".into(),
            authority_hotel: "test-hotel".into(),
            authority_component: None,
            owner_guest_id: owner.into(),
            owner_hotel: None,
            owner_component_type: None,
            lease_epoch: epoch,
            lease_expires_at: 0,
            last_heartbeat_at: 0,
            status: LeaseStatus::Active,
            delegated_from: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn lease_key_is_stable_scoped_and_token_specific() {
        let a = gateway_lease_key("token-a");
        let b = gateway_lease_key("token-b");
        assert_eq!(a, gateway_lease_key("token-a"));
        assert_ne!(a, b);
        let fingerprint = a.strip_prefix("discord:gateway:").expect("prefix");
        assert_eq!(fingerprint.len(), 16);
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn acquire_mapping_covers_granted_held_and_unexpected() {
        assert_eq!(
            map_acquire_response(IpcResponse::DiscordGatewayLease {
                granted: true,
                lease: Some(envelope("us", 3)),
            })
            .unwrap(),
            LeaseAcquireOutcome::Granted { epoch: 3 }
        );
        assert_eq!(
            map_acquire_response(IpcResponse::DiscordGatewayLease {
                granted: false,
                lease: Some(envelope("incumbent", 12)),
            })
            .unwrap(),
            LeaseAcquireOutcome::Held {
                owner: Some("incumbent".into()),
                epoch: 12
            }
        );
        // Denied with no envelope: owner unknown, not a crash.
        assert_eq!(
            map_acquire_response(IpcResponse::DiscordGatewayLease {
                granted: false,
                lease: None,
            })
            .unwrap(),
            LeaseAcquireOutcome::Held {
                owner: None,
                epoch: 0
            }
        );
        // granted:true with no envelope is malformed → error (absorbed as backoff).
        assert!(
            map_acquire_response(IpcResponse::DiscordGatewayLease {
                granted: true,
                lease: None,
            })
            .is_err()
        );
        assert!(
            map_acquire_response(IpcResponse::Ack {
                req_id: "r1".into()
            })
            .is_err()
        );
    }

    #[test]
    fn renew_mapping_distinguishes_ok_reacquire_and_lost() {
        assert_eq!(
            map_renew_response(IpcResponse::DiscordGatewayLease {
                granted: true,
                lease: Some(envelope("us", 4)),
            })
            .unwrap(),
            LeaseRenewResult::Ok { epoch: 4 }
        );
        // "No current owner" is NOT fatal — it maps to NeedsReacquire so the
        // driver re-acquires in place instead of killing the gateway.
        assert_eq!(
            map_renew_response(IpcResponse::DiscordGatewayLease {
                granted: false,
                lease: None,
            })
            .unwrap(),
            LeaseRenewResult::NeedsReacquire
        );
        assert_eq!(
            map_renew_response(IpcResponse::DiscordGatewayLease {
                granted: false,
                lease: Some(envelope("other-seat", 9)),
            })
            .unwrap(),
            LeaseRenewResult::Lost {
                owner: Some("other-seat".into())
            }
        );
        assert!(
            map_renew_response(IpcResponse::DiscordGatewayLease {
                granted: true,
                lease: None,
            })
            .is_err()
        );
        assert!(
            map_renew_response(IpcResponse::Ack {
                req_id: "r1".into()
            })
            .is_err()
        );
    }

    /// Scripted hotel speaking raw `DiscordGatewayLease` IPC shapes, run
    /// through the same mapping functions as the real backend.
    #[derive(Default)]
    struct ScriptedHotel {
        acquires: VecDeque<IpcResponse>,
        renews: VecDeque<IpcResponse>,
        released: bool,
    }

    #[async_trait::async_trait]
    impl LeaseBackend for ScriptedHotel {
        async fn acquire(&mut self) -> Result<LeaseAcquireOutcome> {
            map_acquire_response(self.acquires.pop_front().expect("unexpected acquire"))
        }

        async fn renew(&mut self, _epoch: u64) -> Result<LeaseRenewResult> {
            map_renew_response(self.renews.pop_front().expect("unexpected renew"))
        }

        async fn release(&mut self) -> Result<()> {
            self.released = true;
            Ok(())
        }
    }

    /// The regression the migration exists to fix: a renew answered with
    /// `granted: false, lease: None` (hotel reset/expired the lease, nobody
    /// else holds it) must re-acquire in place — not tear the seat down.
    #[tokio::test(start_paused = true)]
    async fn hotel_lease_reset_reacquires_in_place_instead_of_hard_bailing() {
        let mut hotel = ScriptedHotel::default();
        hotel.acquires.push_back(IpcResponse::DiscordGatewayLease {
            granted: true,
            lease: Some(envelope("us", 1)),
        });
        hotel.renews.push_back(IpcResponse::DiscordGatewayLease {
            granted: false,
            lease: None,
        });
        hotel.acquires.push_back(IpcResponse::DiscordGatewayLease {
            granted: true,
            lease: Some(envelope("us", 5)),
        });

        let mut driver = LeaseDriver::new(LeaseDriverConfig::default());
        assert_eq!(
            driver.tick(&mut hotel).await,
            LeaseEvent::Acquired { epoch: 1 }
        );

        advance(Duration::from_secs(20)).await;
        assert_eq!(
            driver.tick(&mut hotel).await,
            LeaseEvent::Reacquired { epoch: 5 }
        );
        assert!(driver.is_held());
        assert_eq!(driver.epoch(), Some(5));
    }

    /// Losing the lease to another holder stays terminal: the seat must yield.
    #[tokio::test(start_paused = true)]
    async fn renew_lost_to_other_holder_is_terminal() {
        let mut hotel = ScriptedHotel::default();
        hotel.acquires.push_back(IpcResponse::DiscordGatewayLease {
            granted: true,
            lease: Some(envelope("us", 1)),
        });
        hotel.renews.push_back(IpcResponse::DiscordGatewayLease {
            granted: false,
            lease: Some(envelope("other-seat", 2)),
        });

        let mut driver = LeaseDriver::new(LeaseDriverConfig::default());
        assert_eq!(
            driver.tick(&mut hotel).await,
            LeaseEvent::Acquired { epoch: 1 }
        );

        advance(Duration::from_secs(20)).await;
        assert_eq!(
            driver.tick(&mut hotel).await,
            LeaseEvent::Lost {
                owner: Some("other-seat".into())
            }
        );
        assert!(driver.is_lost());
        assert_eq!(driver.next_deadline(), None);
    }
}
