use anyhow::{Result, bail};
use philotic_client::{IpcRequest, IpcResponse, PhiloticClient};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

const LEASE_RENEW_INTERVAL_SECS: u64 = 20;

/// Derive the lease key for a Discord bot token.
/// Uses only the first 16 hex chars of SHA-256(token) — same pattern as Telegram.
pub fn gateway_lease_key(bot_token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bot_token.as_bytes());
    let hash = hasher.finalize();
    let fingerprint = &hex::encode(hash)[..16];
    format!("discord:gateway:{}", fingerprint)
}

pub struct DiscordGatewayLease {
    lease_key: String,
    agent_id: String,
    epoch: u64,
}

impl DiscordGatewayLease {
    /// Acquire the gateway lease. Blocks until granted or returns an error.
    /// If another instance holds the lease, this fails — operator must ensure
    /// only one membrane-discord instance runs per bot token.
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

        let response = ipc
            .send_request(IpcRequest::AcquireDiscordGatewayLease {
                lease_key: lease_key.clone(),
                agent_id: agent_id.to_string(),
            })
            .await?;

        match response {
            IpcResponse::DiscordGatewayLease {
                granted: true,
                lease: Some(ref lease),
            } => {
                info!(
                    "Discord gateway lease [{}] granted — epoch {}",
                    lease_key, lease.lease_epoch
                );
                Ok(Self {
                    lease_key,
                    agent_id: agent_id.to_string(),
                    epoch: lease.lease_epoch,
                })
            }
            IpcResponse::DiscordGatewayLease {
                granted: false,
                lease,
            } => {
                let holder = lease
                    .as_ref()
                    .map(|l| l.owner_guest_id.as_str())
                    .unwrap_or("unknown");
                bail!(
                    "Discord gateway lease [{}] denied — currently held by [{}]. \
                     Only one membrane-discord instance may run per bot token.",
                    lease_key,
                    holder
                )
            }
            other => bail!(
                "Unexpected response to AcquireDiscordGatewayLease: {:?}",
                other
            ),
        }
    }

    /// Renew the lease. Call on the LEASE_RENEW_INTERVAL_SECS tick.
    /// Returns the new epoch on success; logs a warning and returns current epoch if lost.
    pub async fn renew(&mut self, ipc: &mut PhiloticClient) -> Result<()> {
        let response = ipc
            .send_request(IpcRequest::RenewDiscordGatewayLease {
                lease_key: self.lease_key.clone(),
                agent_id: self.agent_id.clone(),
                lease_epoch: self.epoch,
            })
            .await?;

        match response {
            IpcResponse::DiscordGatewayLease {
                granted: true,
                lease: Some(ref lease),
            } => {
                self.epoch = lease.lease_epoch;
                Ok(())
            }
            IpcResponse::DiscordGatewayLease {
                granted: false,
                lease,
            } => {
                let holder = lease
                    .as_ref()
                    .map(|l| l.owner_guest_id.as_str())
                    .unwrap_or("unknown");
                warn!(
                    "Discord gateway lease [{}] renewal lost to [{}] — shutting down",
                    self.lease_key, holder
                );
                bail!("Discord gateway lease lost to [{}]", holder)
            }
            other => bail!(
                "Unexpected response to RenewDiscordGatewayLease: {:?}",
                other
            ),
        }
    }

    /// Release the lease on clean shutdown.
    pub async fn release(self, ipc: &mut PhiloticClient) -> Result<()> {
        info!("Releasing Discord gateway lease [{}]", self.lease_key);
        ipc.send_request(IpcRequest::ReleaseDiscordGatewayLease {
            lease_key: self.lease_key,
        })
        .await?;
        Ok(())
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn renew_interval_secs() -> u64 {
        LEASE_RENEW_INTERVAL_SECS
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
