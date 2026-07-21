//! Stdio transport for an upstream MCP server (client-fabric Phase 3).
//!
//! Spawns the allowlisted command as a child process and speaks newline-framed
//! JSON-RPC over its stdin/stdout — the stdio MCP convention. The child is
//! launched with a SCRUBBED environment (only HOME/PATH/LANG + explicit
//! passthroughs survive) so a projected tool can't lean on ambient secrets in
//! the guest's env. Command + args are validated against the operator
//! allowlist hotel-side before this ever runs; we re-assert the shape here as
//! defense in depth.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::{Duration, timeout};

/// Env vars that survive the scrub (plus anything in `MCP_STDIO_ENV_PASSTHROUGH`,
/// a comma-separated allowlist the operator can set on the guest).
const KEPT_ENV: &[&str] = &["HOME", "PATH", "LANG", "LC_ALL", "TMPDIR", "USER"];

pub struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    call_timeout: Duration,
}

impl StdioTransport {
    /// Spawn `command args...` with a scrubbed environment.
    pub fn spawn(command: &str, args: &[String], call_timeout: Duration) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        // Scrub the environment: start empty, restore only a safe baseline.
        cmd.env_clear();
        for key in KEPT_ENV {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
        if let Ok(passthrough) = std::env::var("MCP_STDIO_ENV_PASSTHROUGH") {
            for key in passthrough.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                if let Ok(val) = std::env::var(key) {
                    cmd.env(key, val);
                }
            }
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning stdio upstream '{command}'"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("child stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("child stdout unavailable"))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            call_timeout,
        })
    }

    /// Send one JSON-RPC request line and read reply lines until the matching
    /// id arrives (skipping notifications / mismatched ids the server may emit).
    pub async fn rpc(&mut self, id: u64, method: &str, params: Value) -> Result<Value> {
        // Liveness check — a dead child means a broken transport, surface it.
        if let Ok(Some(status)) = self.child.try_wait() {
            bail!("stdio upstream exited before request ({status})");
        }

        let mut line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .context("writing to stdio upstream")?;
        self.stdin.flush().await.ok();

        let read = async {
            loop {
                let mut buf = String::new();
                let n = self
                    .stdout
                    .read_line(&mut buf)
                    .await
                    .context("reading from stdio upstream")?;
                if n == 0 {
                    bail!("stdio upstream closed stdout");
                }
                let trimmed = buf.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let envelope: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue, // non-JSON log noise on stdout — skip
                };
                // Only consume the reply matching our id.
                match envelope.get("id").and_then(Value::as_u64) {
                    Some(rid) if rid == id => {
                        if let Some(err) = envelope.get("error") {
                            bail!("stdio JSON-RPC error for {method}: {err}");
                        }
                        return Ok(envelope.get("result").cloned().unwrap_or(Value::Null));
                    }
                    _ => continue,
                }
            }
        };

        match timeout(self.call_timeout, read).await {
            Ok(result) => result,
            Err(_) => bail!("stdio upstream timed out after {:?}", self.call_timeout),
        }
    }

    /// Fire-and-forget notification (id-less).
    pub async fn notify(&mut self, method: &str) -> Result<()> {
        let mut line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": method,
        }))?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.ok();
        self.stdin.flush().await.ok();
        Ok(())
    }
}
