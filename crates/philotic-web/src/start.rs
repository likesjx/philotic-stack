use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::time::sleep;

use crate::init::{active_profile, philotic_dir, profile_dir};

pub fn pid_path() -> PathBuf {
    profile_dir().join("aiua.pid")
}

/// Returns the expected aiua socket path for the current profile.
/// When PHILOTIC_PROFILE is set: ~/.philotic/<profile>/aiua.sock
/// When unset: falls back to PHILOTIC_HOTEL_SOCKET env var or /tmp/philotic-<hotel>.sock
pub fn socket_path(hotel: &str) -> String {
    if active_profile().is_some() {
        profile_dir()
            .join("aiua.sock")
            .to_string_lossy()
            .into_owned()
    } else {
        std::env::var("PHILOTIC_HOTEL_SOCKET")
            .unwrap_or_else(|_| format!("/tmp/philotic-{hotel}.sock"))
    }
}

pub async fn run(hotel: String, detach: bool) -> Result<()> {
    // ── Ensure muninn is running ───────────────────────────────────────────
    crate::muninn::ensure_running().await?;

    // ── Already running? ───────────────────────────────────────────────────
    if let Some(existing_pid) = read_pid() {
        if process_alive(existing_pid) {
            println!("aiua is already running (pid {existing_pid})");
            println!("Run `phil status` to inspect, or `phil stop` to stop it.");
            return Ok(());
        } else {
            // Stale PID file
            let _ = std::fs::remove_file(pid_path());
        }
    }

    // ── Find aiua binary ───────────────────────────────────────────────────
    let aiua_bin = find_aiua_binary()?;
    println!("Starting aiua from: {}", aiua_bin.display());
    println!("Hotel: {hotel}");
    if let Some(ref p) = active_profile() {
        println!("Profile: {p}  ({})", profile_dir().display());
    }

    // ── Create profile dir if needed ──────────────────────────────────────
    std::fs::create_dir_all(profile_dir())
        .with_context(|| format!("create {}", profile_dir().display()))?;
    // Always ensure ~/.philotic/ itself exists too
    std::fs::create_dir_all(philotic_dir()).context("create ~/.philotic/")?;

    // ── Spawn ──────────────────────────────────────────────────────────────
    let log_path = profile_dir().join("aiua.log");
    let log_file = std::fs::File::create(&log_path)
        .with_context(|| format!("create log file {}", log_path.display()))?;
    let log_file2 = log_file.try_clone().context("clone log file handle")?;

    let mut cmd = tokio::process::Command::new(&aiua_bin);
    cmd.arg("--hotel")
        .arg(&hotel)
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file2));

    // Propagate PHILOTIC_PROFILE to the aiua child so its paths stay in sync.
    if let Some(ref p) = active_profile() {
        cmd.env("PHILOTIC_PROFILE", p);
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", aiua_bin.display()))?;

    let pid = child.id().context("could not read child pid")?;
    write_pid(pid)?;
    println!("aiua started (pid {pid}), log: {}", log_path.display());

    if detach {
        println!("Detached. Run `phil status` to check startup.");
        return Ok(());
    }

    // ── Wait for socket to appear ─────────────────────────────────────────
    let socket_path = socket_path(&hotel);

    print!("Waiting for aiua socket");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);

    loop {
        if tokio::time::Instant::now() > deadline {
            println!(
                "\nTimed out waiting for socket. Check log: {}",
                log_path.display()
            );
            bail!("aiua did not start in time");
        }
        if UnixStream::connect(&socket_path).await.is_ok() {
            break;
        }
        // Check if the process died already
        if !process_alive(pid) {
            println!(
                "\naiua exited unexpectedly. Check log: {}",
                log_path.display()
            );
            let _ = std::fs::remove_file(pid_path());
            bail!("aiua exited during startup");
        }
        print!(".");
        sleep(Duration::from_millis(500)).await;
    }
    // flush the dots line
    println!(" ready");
    println!("aiua is up. Run `phil status` for details.");
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

pub fn find_aiua_binary() -> Result<PathBuf> {
    // 1. On PATH
    if let Ok(p) = which_bin("aiua") {
        return Ok(p);
    }
    // 2. Cargo release build
    let release = PathBuf::from("target/release/aiua");
    if release.exists() {
        return Ok(release);
    }
    // 3. Cargo debug build
    let debug = PathBuf::from("target/debug/aiua");
    if debug.exists() {
        return Ok(debug);
    }
    bail!(
        "aiua binary not found. Install via `brew install likesjx/philotic/aiua` \
         or build with `cargo build -p aiua --release`."
    )
}

fn which_bin(name: &str) -> Result<PathBuf> {
    let output = std::process::Command::new("which")
        .arg(name)
        .output()
        .context("which")?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(PathBuf::from(s))
    } else {
        bail!("{name} not on PATH")
    }
}

pub fn read_pid() -> Option<u32> {
    let raw = std::fs::read_to_string(pid_path()).ok()?;
    raw.trim().parse().ok()
}

fn write_pid(pid: u32) -> Result<()> {
    std::fs::write(pid_path(), pid.to_string()).context("write aiua.pid")
}

pub fn process_alive(pid: u32) -> bool {
    // kill -0 checks existence without signalling
    let status = std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status();
    matches!(status, Ok(s) if s.success())
}
