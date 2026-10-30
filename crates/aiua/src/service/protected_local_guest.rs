//! Opt-in supervisor adapter for the hotel's local authority. Legacy Register
//! cannot enroll protected children. No launch credentials or implicit grants.
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
    native_authority: Option<Arc<super::native_authority::NativeAuthority>>,
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
        Self::adopt(command.spawn()?, registry, guest, expected_uid, principal)
    }
    /// The materializer hands over the concrete child it just spawned, after
    /// configuring its IO. Never adopts a caller-supplied PID.
    pub fn adopt(
        child: Child,
        registry: Arc<LocalLaunchRegistry>,
        guest: &str,
        expected_uid: u32,
        principal: LaunchPrincipal,
    ) -> Result<Self> {
        let child = Arc::new(Mutex::new(child));
        let process: Arc<dyn SupervisedProcess> = Arc::new(OwnedChild(child.clone()));
        let generation = registry.attach_new(guest, expected_uid, principal, process)?;
        Ok(Self {
            native_authority: None,
            child,
            registry,
            guest: guest.into(),
            generation,
        })
    }
    pub fn with_native_authority(
        mut self,
        authority: Arc<super::native_authority::NativeAuthority>,
    ) -> Result<Self> {
        anyhow::ensure!(
            authority.owns_launches(&self.registry),
            "foreign native launch owner"
        );
        self.native_authority = Some(authority);
        Ok(self)
    }
    async fn retire(&self) -> Result<()> {
        if let Some(authority) = &self.native_authority {
            authority.retire(&self.guest, self.generation).await
        } else {
            self.registry.retire(&self.guest, self.generation)
        }
    }
    pub async fn stop(&self) -> Result<()> {
        // Retire authority BEFORE kill/wait; parked envelopes never transfer to
        // the replacement process merely because it claims the same guest ID.
        self.retire().await?;
        let mut child = self.child.lock().await;
        if child.try_wait()?.is_none() {
            child.kill().await?;
        }
        Ok(())
    }
    pub async fn exited(&self) -> Result<bool> {
        let exited = self.child.lock().await.try_wait()?.is_some();
        if exited {
            self.retire().await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use tokio::net::UnixListener;
    #[test]
    #[ignore = "synthetic child helper invoked only by its parent test"]
    fn owned_child_peer() {
        use std::io::Read;
        let path = std::env::var("NATIVE_SYNTHETIC_PEER_SOCKET").unwrap();
        let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
        let mut byte = [0];
        let _ = stream.read(&mut byte);
    }
    #[tokio::test]
    async fn concrete_child_retirement_and_replacement_do_not_transfer_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("peer.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let registry = Arc::new(LocalLaunchRegistry::default());
        let command = || {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "service::protected_local_guest::tests::owned_child_peer",
                    "--ignored",
                ])
                .env("NATIVE_SYNTHETIC_PEER_SOCKET", &socket)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            command.kill_on_drop(true);
            command
        };
        let spawn = || {
            let mut command = command();
            ProtectedLocalGuest::spawn(
                &mut command,
                registry.clone(),
                "synthetic",
                unsafe { libc::geteuid() },
                LaunchPrincipal {
                    stable_agent_id: "synthetic-agent".into(),
                    roles: BTreeSet::new(),
                },
            )
            .unwrap()
        };
        // Force the eager-child window: connect before materializer enrollment.
        let child = command().spawn().unwrap();
        let (stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
        assert!(
            registry
                .authenticate("hotel", &stream, "synthetic")
                .is_err()
        );
        let first = ProtectedLocalGuest::adopt(
            child,
            registry.clone(),
            "synthetic",
            unsafe { libc::geteuid() },
            LaunchPrincipal {
                stable_agent_id: "synthetic-agent".into(),
                roles: BTreeSet::new(),
            },
        )
        .unwrap();
        let session = registry
            .authenticate("hotel", &stream, "synthetic")
            .unwrap();
        assert!(session.principal().is_ok());
        assert!(registry.authenticate("hotel", &stream, "other").is_err());
        first.stop().await.unwrap();
        assert!(session.principal().is_err());
        let second = spawn();
        let (next, _) = tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let replacement = registry.authenticate("hotel", &next, "synthetic").unwrap();
        assert_ne!(session.consumer(), replacement.consumer());
        drop(first); // Old generation's Drop must not retire its replacement.
        assert!(replacement.principal().is_ok());
        assert!(session.principal().is_err());
        second.stop().await.unwrap();
    }
}
