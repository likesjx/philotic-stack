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
    async fn spawn_guest(&mut self, guest_id: &str, config_json: &serde_json::Value) -> Result<String>;
    
    /// Request the Materializer to gracefully or forcefully retire the identity
    async fn reclaim_guest(&mut self, guest_id: &str) -> Result<()>;
    
    /// Check if the materialized Guest identity is currently running and healthy
    async fn check_status(&mut self, guest_id: &str, active_id: &str) -> Result<bool>;
}
