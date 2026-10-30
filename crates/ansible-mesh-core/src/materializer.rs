use anyhow::Result;

/// A Universal Materialization Interface representing the ability to
/// spawn and manage Guest identities within the local Hotel ecosystem.
///
/// Implementations can back this trait with local OS processes,
/// Docker containers, systemd units, Kubernetes pods, etc.
#[async_trait::async_trait]
pub trait Materializer: Send + Sync {
    /// Request the Materializer to conjure the Guest configuration into existence.
    /// Returns the active handle/ID of the materialized entity (e.g. PID "1234", Docker Container "d25a6r9...")
    async fn spawn_guest(
        &mut self,
        guest_id: &str,
        config_json: &serde_json::Value,
    ) -> Result<String>;

    /// Request the Materializer to gracefully or forcefully retire the identity
    async fn reclaim_guest(&mut self, guest_id: &str) -> Result<()>;

    /// Optional legacy orphan retirement. A persisted handle is not ownership;
    /// protected materializers must refuse this path for enrolled identities.
    async fn reclaim_untracked(&mut self, _guest_id: &str, _active_id: &str) -> Result<()> {
        anyhow::bail!("untracked retirement unsupported")
    }

    /// Check if the materialized Guest identity is currently running and healthy
    async fn check_status(&mut self, guest_id: &str, active_id: &str) -> Result<bool>;
}
