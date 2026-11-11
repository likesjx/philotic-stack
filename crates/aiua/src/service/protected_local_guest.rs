//! Opt-in supervisor adapter for the hotel's local authority. Legacy Register
//! cannot enroll protected children. No launch credentials or implicit grants.
use ansible_mesh_core::privacy_local::{LaunchPrincipal, LocalLaunchRegistry, SupervisedProcess};
use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

struct OwnedChild {
    child: Arc<Mutex<Child>>,
    attempt: Uuid,
    started: Instant,
    exit_logged: AtomicBool,
}

fn exit_fields(status: std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        (status.code(), status.signal())
    }
    #[cfg(not(unix))]
    {
        (status.code(), None)
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}
impl SupervisedProcess for OwnedChild {
    fn pid(&self) -> Result<u32> {
        self.child
            .try_lock()
            .map_err(|_| anyhow!("supervisor child busy"))?
            .id()
            .ok_or_else(|| anyhow!("child exited"))
    }
    fn alive(&self) -> Result<bool> {
        let status = self
            .child
            .try_lock()
            .map_err(|_| anyhow!("supervisor child busy"))?
            .try_wait()?;
        if let Some(status) = status.filter(|_| !self.exit_logged.swap(true, Ordering::Relaxed)) {
            let (exit_code, signal) = exit_fields(status);
            tracing::warn!(event = "protected_child_lifecycle", stage = "child_exit", outcome = "exited", attempt_id = %self.attempt, elapsed_ms = elapsed_ms(self.started), exit_code = ?exit_code, signal = ?signal);
        }
        Ok(status.is_none())
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
        Self::adopt_observed(
            child,
            registry,
            guest,
            expected_uid,
            principal,
            Uuid::new_v4(),
            Instant::now(),
        )
    }
    pub(super) fn adopt_observed(
        child: Child,
        registry: Arc<LocalLaunchRegistry>,
        guest: &str,
        expected_uid: u32,
        principal: LaunchPrincipal,
        attempt: Uuid,
        started: Instant,
    ) -> Result<Self> {
        let child = Arc::new(Mutex::new(child));
        let process: Arc<dyn SupervisedProcess> = Arc::new(OwnedChild {
            child: child.clone(),
            attempt,
            started,
            exit_logged: AtomicBool::new(false),
        });
        let generation = match registry.attach_new(guest, expected_uid, principal, process) {
            Ok(generation) => {
                tracing::info!(event = "protected_child_startup", stage = "adoption", outcome = "accepted", attempt_id = %attempt, elapsed_ms = elapsed_ms(started));
                generation
            }
            Err(error) => {
                tracing::warn!(event = "protected_child_startup", stage = "adoption", outcome = "rejected", attempt_id = %attempt, elapsed_ms = elapsed_ms(started));
                return Err(error);
            }
        };
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
    use std::io::{self, Write};
    use tokio::net::UnixListener;

    #[derive(Clone)]
    struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    #[test]
    fn protected_diag_exit_fields_distinguish_exit_code_and_signal() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            exit_fields(std::process::ExitStatus::from_raw(7 << 8)),
            (Some(7), None)
        );
        assert_eq!(
            exit_fields(std::process::ExitStatus::from_raw(15)),
            (None, Some(15))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn protected_diag_early_exit_is_correlated_once_and_does_not_log_child_input() {
        let captured = Captured(Arc::new(std::sync::Mutex::new(Vec::new())));
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _default = tracing::subscriber::set_default(subscriber);
        let attempt = Uuid::new_v4();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exit 7"])
            .env("SYNTHETIC_SECRET", "do-not-log-this");
        command.kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        assert_eq!(child.wait().await.unwrap().code(), Some(7));
        let owned = OwnedChild {
            child: Arc::new(Mutex::new(child)),
            attempt,
            started: Instant::now(),
            exit_logged: AtomicBool::new(false),
        };
        assert!(!owned.alive().unwrap());
        assert!(!owned.alive().unwrap());
        let output = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert_eq!(output.matches("protected_child_lifecycle").count(), 1);
        assert!(output.contains(&attempt.to_string()));
        assert!(output.contains("exit_code=Some(7)"));
        assert!(!output.contains("do-not-log-this"));
        assert!(!output.contains("/bin/sh"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn protected_diag_adoption_rejection_keeps_exit_and_rejection_correlated() {
        let captured = Captured(Arc::new(std::sync::Mutex::new(Vec::new())));
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _default = tracing::subscriber::set_default(subscriber);
        let attempt = Uuid::new_v4();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exit 7"])
            .env("SYNTHETIC_SECRET", "do-not-log-this");
        command.kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        child.wait().await.unwrap();
        let result = ProtectedLocalGuest::adopt_observed(
            child,
            Arc::new(LocalLaunchRegistry::default()),
            "sensitive-guest-name",
            unsafe { libc::geteuid() },
            LaunchPrincipal {
                stable_agent_id: "synthetic".into(),
                roles: BTreeSet::new(),
            },
            attempt,
            Instant::now(),
        );
        assert!(result.is_err());
        let output = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert_eq!(output.matches(&attempt.to_string()).count(), 2);
        assert!(output.contains("child_exit"));
        assert!(output.contains("rejected"));
        assert!(!output.contains("sensitive-guest-name"));
        assert!(!output.contains("do-not-log-this"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn protected_diag_live_child_adoption_is_logged_without_identity() {
        let captured = Captured(Arc::new(std::sync::Mutex::new(Vec::new())));
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _default = tracing::subscriber::set_default(subscriber);
        let attempt = Uuid::new_v4();
        let mut command = Command::new("/bin/sleep");
        command.arg("10").kill_on_drop(true);
        let child = command.spawn().unwrap();
        let guest = ProtectedLocalGuest::adopt_observed(
            child,
            Arc::new(LocalLaunchRegistry::default()),
            "private-guest-name",
            unsafe { libc::geteuid() },
            LaunchPrincipal {
                stable_agent_id: "synthetic".into(),
                roles: BTreeSet::new(),
            },
            attempt,
            Instant::now(),
        )
        .unwrap();
        let output = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(output.contains(&attempt.to_string()));
        assert!(output.contains("accepted"));
        assert!(!output.contains("private-guest-name"));
        drop(guest);
    }
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
