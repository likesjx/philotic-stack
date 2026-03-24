use anyhow::{Context, Result};
use std::process::{Child, Command};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

/// Owns and supervises an `mlx_lm.server` subprocess (managed mode only).
pub struct MlxServerHandle {
    child: Child,
    port: u16,
    repo_id: String,
}

impl MlxServerHandle {
    /// Resolve the Python interpreter to use: `python3` if available, else `python`.
    /// Public so other modules (e.g. whisper.rs) can reuse the same detection.
    pub fn python_bin() -> &'static str {
        // On macOS / modern Linux there is often no bare `python`, only `python3`.
        if std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            "python3"
        } else {
            "python"
        }
    }

    /// Verify that `mlx_lm` is importable. Call before spawning managed instances to fail fast.
    pub fn check_available() -> Result<()> {
        let python = Self::python_bin();
        let status = Command::new(python)
            .args(["-c", "import mlx_lm"])
            .status()
            .with_context(|| format!("failed to invoke {python} to check mlx_lm availability"))?;
        if !status.success() {
            anyhow::bail!("`mlx_lm` is not installed or not importable. Install with: pip install mlx-lm");
        }
        Ok(())
    }

    /// Spawn `python3 -m mlx_lm.server --model <repo> --port <port> [extra_args...]`.
    pub fn spawn(repo_id: &str, port: u16, extra_args: &[String]) -> Result<Self> {
        let python = Self::python_bin();
        info!(repo_id, port, python, "spawning mlx_lm.server");
        let child = Command::new(python)
            .args(["-m", "mlx_lm.server", "--model", repo_id, "--port", &port.to_string()])
            .args(extra_args)
            .spawn()
            .with_context(|| format!("failed to spawn mlx_lm.server for {repo_id}"))?;
        Ok(Self {
            child,
            port,
            repo_id: repo_id.to_string(),
        })
    }

    /// Poll `/v1/models` until the server is ready or the timeout elapses.
    pub async fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let url = format!("http://127.0.0.1:{}/v1/models", self.port);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()?;
        let deadline = tokio::time::Instant::now() + timeout;
        let mut attempts = 0u32;
        loop {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "mlx_lm.server for {} did not become ready within {:?} ({} attempts)",
                    self.repo_id,
                    timeout,
                    attempts
                );
            }
            attempts += 1;
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    info!(
                        repo_id = %self.repo_id,
                        port = self.port,
                        attempts,
                        "mlx_lm.server ready"
                    );
                    return Ok(());
                }
                Ok(resp) => {
                    warn!(status = %resp.status(), attempt = attempts, "mlx_lm.server not yet ready");
                }
                Err(_) => {}
            }
            sleep(Duration::from_secs(2)).await;
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for MlxServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        info!(
            repo_id = %self.repo_id,
            port = self.port,
            "mlx_lm.server subprocess terminated"
        );
    }
}
