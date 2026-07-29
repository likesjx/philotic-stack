//! Bounded, opt-out access to the macOS Keychain.
//!
//! `security find-generic-password` / `add-generic-password` do **not** fail on
//! a host without an unlocked login keychain — they block forever. That hung
//! `cargo test --workspace` for 5h50m on a CI runner (orphaned `security`
//! processes were still alive at job teardown), and is the most likely
//! explanation for the long-standing "cargo test -p aiua stalls at
//! desktop_membrane" reports, which had been assumed to be a tokio deadlock.
//!
//! Every caller in the workspace goes through here so the gating rule and the
//! deadline are defined once. Duplicated security-critical timeout logic is
//! exactly the sort of thing that rots out of sync.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

/// Explicit override. Truthy forces the Keychain on even in CI; falsey turns it
/// off entirely, leaving env -> file as the only key sources.
pub const KEYCHAIN_ENABLED_ENV: &str = "PHILOTIC_VAULT_KEYCHAIN";
pub const KEYCHAIN_TIMEOUT_ENV: &str = "PHILOTIC_VAULT_KEYCHAIN_TIMEOUT_SECS";
pub const KEYCHAIN_DEFAULT_TIMEOUT_SECS: u64 = 10;

/// Whether the macOS Keychain may be consulted.
///
/// Order: explicit override wins; otherwise off for non-macOS and for detected
/// CI; on for an interactive Mac, which preserves the zero-config bootstrap.
pub fn enabled() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    match env_flag(KEYCHAIN_ENABLED_ENV) {
        Some(explicit) => explicit,
        // GitHub Actions and most runners set CI=true. There is no unlocked
        // login keychain there, so consulting it can only hang.
        None => std::env::var_os("CI").is_none(),
    }
}

/// Parse a tri-state boolean env var: `Some(true)`, `Some(false)`, or `None`
/// when unset or unrecognised.
pub fn env_flag(name: &str) -> Option<bool> {
    let raw = std::env::var(name).ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

pub fn timeout() -> Duration {
    let secs = std::env::var(KEYCHAIN_TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(KEYCHAIN_DEFAULT_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Run the macOS `security` CLI with a hard deadline.
pub fn run_security(args: &[&str], while_doing: &str) -> Result<Output> {
    let mut command = Command::new("security");
    command.args(args);
    run_with_deadline(command, timeout(), while_doing)
}

/// Run a command, killing it if it outlives `timeout`.
///
/// `Command::output()` waits forever, which is the failure mode this module
/// exists to prevent. stdin is closed so the child can never block prompting,
/// and it is killed past the deadline so it cannot be left orphaned.
pub fn run_with_deadline(
    mut command: Command,
    timeout: Duration,
    while_doing: &str,
) -> Result<Output> {
    use std::io::Read;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run macOS security CLI while {while_doing}"))?;

    // Drain the pipes on threads so a chatty child cannot fill a pipe buffer
    // and deadlock against our own polling loop.
    let mut child_stdout = child
        .stdout
        .take()
        .context("security stdout was not piped")?;
    let mut child_stderr = child
        .stderr
        .take()
        .context("security stderr was not piped")?;
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stderr.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child
            .try_wait()
            .with_context(|| format!("failed to poll macOS security CLI while {while_doing}"))?
        {
            Some(status) => break status,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!(
                        "macOS security CLI timed out after {}s while {}. This host most likely has \
                         no unlocked login Keychain (CI runner, fresh Mac, or headless box), where \
                         `security` blocks instead of failing. Set {}=0 and provide \
                         PHILOTIC_VAULT_MASTER_KEY or ~/.philotic/vault-master-key.env.",
                        timeout.as_secs(),
                        while_doing,
                        KEYCHAIN_ENABLED_ENV
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };

    Ok(Output {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    #[test]
    fn env_flag_parses_tri_state() {
        let _guard = env_lock();
        for truthy in ["1", "true", "YES", " on "] {
            unsafe { std::env::set_var("PHILOTIC_KEYCHAIN_TEST_FLAG", truthy) };
            assert_eq!(
                env_flag("PHILOTIC_KEYCHAIN_TEST_FLAG"),
                Some(true),
                "{truthy:?}"
            );
        }
        for falsey in ["0", "false", "NO", " off "] {
            unsafe { std::env::set_var("PHILOTIC_KEYCHAIN_TEST_FLAG", falsey) };
            assert_eq!(
                env_flag("PHILOTIC_KEYCHAIN_TEST_FLAG"),
                Some(false),
                "{falsey:?}"
            );
        }
        unsafe { std::env::set_var("PHILOTIC_KEYCHAIN_TEST_FLAG", "banana") };
        assert_eq!(env_flag("PHILOTIC_KEYCHAIN_TEST_FLAG"), None);
        unsafe { std::env::remove_var("PHILOTIC_KEYCHAIN_TEST_FLAG") };
        assert_eq!(env_flag("PHILOTIC_KEYCHAIN_TEST_FLAG"), None);
    }

    #[test]
    fn timeout_defaults_and_honours_override() {
        let _guard = env_lock();
        unsafe { std::env::remove_var(KEYCHAIN_TIMEOUT_ENV) };
        assert_eq!(
            timeout(),
            Duration::from_secs(KEYCHAIN_DEFAULT_TIMEOUT_SECS)
        );

        unsafe { std::env::set_var(KEYCHAIN_TIMEOUT_ENV, "3") };
        assert_eq!(timeout(), Duration::from_secs(3));

        // Zero and garbage must not disable the deadline.
        for bad in ["0", "not-a-number"] {
            unsafe { std::env::set_var(KEYCHAIN_TIMEOUT_ENV, bad) };
            assert_eq!(
                timeout(),
                Duration::from_secs(KEYCHAIN_DEFAULT_TIMEOUT_SECS),
                "{bad:?} should fall back to the default"
            );
        }
        unsafe { std::env::remove_var(KEYCHAIN_TIMEOUT_ENV) };
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_is_skipped_when_disabled_or_in_ci() {
        let _guard = env_lock();
        let restore_ci = std::env::var("CI").ok();

        unsafe { std::env::set_var(KEYCHAIN_ENABLED_ENV, "0") };
        unsafe { std::env::remove_var("CI") };
        assert!(!enabled(), "explicit 0 must disable the backend");

        unsafe { std::env::remove_var(KEYCHAIN_ENABLED_ENV) };
        unsafe { std::env::set_var("CI", "true") };
        assert!(!enabled(), "CI must disable the backend by default");

        // ...but an explicit opt-in still wins, for a self-hosted interactive Mac.
        unsafe { std::env::set_var(KEYCHAIN_ENABLED_ENV, "1") };
        assert!(enabled(), "explicit 1 must override CI detection");

        unsafe { std::env::remove_var(KEYCHAIN_ENABLED_ENV) };
        match restore_ci {
            Some(value) => unsafe { std::env::set_var("CI", value) },
            None => unsafe { std::env::remove_var("CI") },
        }
    }

    #[test]
    fn run_with_deadline_kills_a_hung_child() {
        let mut command = Command::new("sleep");
        command.arg("30");

        let started = Instant::now();
        let err = run_with_deadline(command, Duration::from_millis(300), "testing the deadline")
            .expect_err("a 30s sleep must not satisfy a 300ms deadline");
        let elapsed = started.elapsed();

        assert!(
            err.to_string().contains("timed out"),
            "unexpected error: {err}"
        );
        // The whole point: it returns instead of blocking for the full 30s.
        assert!(
            elapsed < Duration::from_secs(10),
            "deadline did not fire promptly: {elapsed:?}"
        );
    }

    #[test]
    fn run_with_deadline_returns_output_for_a_fast_child() {
        let mut command = Command::new("echo");
        command.arg("philotic");

        let output = run_with_deadline(command, Duration::from_secs(10), "testing fast path")
            .expect("echo should succeed well inside the deadline");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "philotic");
    }
}
