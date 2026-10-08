//! Opt-in supervisor adapter for the local privacy authority. Not installed in
//! hotel startup or legacy Register. No launch credentials or live grants.
use ansible_mesh_core::privacy_local::{LaunchPrincipal, LocalLaunchRegistry, SupervisedProcess};
use anyhow::{Result, anyhow};
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

struct OwnedChild(Arc<Mutex<Child>>);
impl SupervisedProcess for OwnedChild {
    fn pid(&self) -> Result<u32> {
        self.0
            .try_lock()
            .map_err(|_| anyhow!("supervisor child busy"))?
            .id()
            .ok_or_else(|| anyhow!("child exited"))
    }
    fn alive(&self) -> Result<bool> {
        Ok(self
            .0
            .try_lock()
            .map_err(|_| anyhow!("supervisor child busy"))?
            .try_wait()?
            .is_none())
    }
}

/// Supervisor owns both the actual OS child and its authoritative launch map.
/// Principal/roles must come from validated server records, not command args or
/// guest registration. Busy/unknown child state denies instead of falling back.
pub struct ProtectedLocalGuest {
    child: Arc<Mutex<Child>>,
    registry: Arc<LocalLaunchRegistry>,
    guest: String,
    generation: Uuid,
}
impl ProtectedLocalGuest {
    pub fn spawn(
        command: &mut Command,
        registry: Arc<LocalLaunchRegistry>,
        guest: &str,
        expected_uid: u32,
        principal: LaunchPrincipal,
    ) -> Result<Self> {
        command.kill_on_drop(true);
        let child = Arc::new(Mutex::new(command.spawn()?));
        let process: Arc<dyn SupervisedProcess> = Arc::new(OwnedChild(child.clone()));
        let generation = registry.attach(guest, expected_uid, principal, process)?;
        Ok(Self {
            child,
            registry,
            guest: guest.into(),
            generation,
        })
    }
    pub async fn stop(&self) -> Result<()> {
        // Retire authority BEFORE kill/wait; parked envelopes never transfer to
        // the replacement process merely because it claims the same guest ID.
        self.registry.retire(&self.guest, self.generation)?;
        let mut child = self.child.lock().await;
        if child.try_wait()?.is_none() {
            child.kill().await?;
        }
        Ok(())
    }
    pub async fn exited(&self) -> Result<bool> {
        let exited = self.child.lock().await.try_wait()?.is_some();
        if exited {
            self.registry.retire(&self.guest, self.generation)?;
        }
        Ok(exited)
    }
}
impl Drop for ProtectedLocalGuest {
    fn drop(&mut self) {
        let _ = self.registry.retire(&self.guest, self.generation);
        if let Ok(mut child) = self.child.try_lock() {
            let _ = child.start_kill();
        }
    }
}
