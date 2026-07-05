//! `phil flush [--restart <hotel>]`
//!
//! Nuclear kill switch for abandoned philotic processes. Sends SIGKILL to every
//! matching process (no graceful-shutdown attempt), then wipes all sockets from
//! both `/tmp/` and every `~/.philotic/*/` profile directory.
//!
//! Designed as the recovery action for OS error 24 (EMFILE / "too many open
//! files") — the condition where orphaned smoke-test or stale guest processes
//! exhaust the file-descriptor table and prevent SQLite from opening temp/lock
//! files.
//!
//! After flushing, pass `--restart <hotel>` to immediately boot the hotel again.

use anyhow::Result;
use std::fs;

use crate::init::{philotic_dir, profile_dir};
use crate::start::pid_path;

/// All philotic binary names to match against running process list.
const PHILOTIC_BINS: &[&str] = &[
    "aiua",
    "membrane",
    "philote",
    "model-router",
    "model-controller-gemini",
    "model-controller-elevenlabs",
    "model-controller-mlx",
    "tool-runner",
    "graph-runner",
    // Legacy per-agent graph binary; kept so old processes are still flushed.
    "agent-graph-runner",
    "agent-datasource",
    "philotic-web",
];

pub async fn run(hotel: Option<String>) -> Result<()> {
    println!("Flushing all philotic processes…");

    // ── 1. Kill all processes (SIGKILL — no mercy) ────────────────────────
    let killed = kill_all();
    if killed == 0 {
        println!("  no processes found");
    } else {
        println!("  killed {killed} process(es)");
        // Brief pause so the OS reclaims FDs before we clean sockets
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    // ── 2. Remove all sockets ─────────────────────────────────────────────
    let removed = remove_all_sockets();
    if removed > 0 {
        println!("  removed {removed} socket(s)");
    }

    // ── 3. Remove PID file ────────────────────────────────────────────────
    let pid_file = pid_path();
    if pid_file.exists() {
        let _ = fs::remove_file(&pid_file);
        println!("  removed PID file");
    }

    println!("Flush complete.");

    // ── 4. Optional restart ───────────────────────────────────────────────
    if let Some(hotel_name) = hotel {
        println!("\nRestarting hotel '{hotel_name}'…");
        // Small delay — let the OS finish reclaiming resources
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        crate::start::run(hotel_name, false).await?;
    }

    Ok(())
}

/// Send SIGKILL to every running process whose command line contains a
/// philotic binary name. Returns the number of processes killed.
fn kill_all() -> usize {
    let lines = process_lines();
    let mut count = 0;

    for line in &lines {
        for bin in PHILOTIC_BINS {
            if line.contains(bin) {
                // Extract the first numeric token as PID
                if let Some(pid) = extract_pid(line) {
                    // Don't kill ourselves
                    if pid == std::process::id() {
                        continue;
                    }
                    let ok = std::process::Command::new("kill")
                        .args(["-KILL", &pid.to_string()])
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                    if ok {
                        println!("  SIGKILL → pid {pid:<6}  {bin}");
                        count += 1;
                    }
                }
                break;
            }
        }
    }

    // Wait for processes to actually die (up to 2s)
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Check if any known processes are still alive
        let still_running = process_lines()
            .iter()
            .any(|l| PHILOTIC_BINS.iter().any(|b| l.contains(b)));
        if !still_running {
            break;
        }
    }

    count
}

/// Remove all philotic sockets: `/tmp/philotic-*.sock` and all
/// `~/.philotic/*/aiua-*.sock` files across every profile directory.
fn remove_all_sockets() -> usize {
    let mut count = 0;

    // /tmp sockets
    if let Ok(paths) = glob::glob("/tmp/philotic-*.sock") {
        for path in paths.filter_map(|p| p.ok()) {
            if fs::remove_file(&path).is_ok() {
                println!("  removed {}", path.display());
                count += 1;
            }
        }
    }

    // Profile dir sockets — current profile
    let profile_sock_pattern = profile_dir().join("*.sock");
    if let Ok(paths) = glob::glob(&profile_sock_pattern.to_string_lossy()) {
        for path in paths.filter_map(|p| p.ok()) {
            if fs::remove_file(&path).is_ok() {
                println!("  removed {}", path.display());
                count += 1;
            }
        }
    }

    // All other profile dirs under ~/.philotic/
    let all_socks_pattern = philotic_dir().join("*").join("*.sock");
    if let Ok(paths) = glob::glob(&all_socks_pattern.to_string_lossy()) {
        for path in paths.filter_map(|p| p.ok()) {
            if fs::remove_file(&path).is_ok() {
                println!("  removed {}", path.display());
                count += 1;
            }
        }
    }

    count
}

fn process_lines() -> Vec<String> {
    std::process::Command::new("ps")
        .args(["aux"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn extract_pid(line: &str) -> Option<u32> {
    line.split_whitespace().find_map(|t| t.parse::<u32>().ok())
}

/// Show all processes and socket counts — used by `phil footprint` for the
/// "how bad is it?" diagnostic before deciding to flush.
pub fn fd_pressure_report() -> String {
    let lines = process_lines();
    let procs: Vec<(u32, &'static str)> = lines
        .iter()
        .filter_map(|line| {
            PHILOTIC_BINS.iter().find_map(|bin| {
                if line.contains(bin) {
                    extract_pid(line).map(|pid| (pid, *bin))
                } else {
                    None
                }
            })
        })
        .collect();

    if procs.is_empty() {
        return "  no philotic processes found".into();
    }

    let mut out = format!("  {} philotic process(es) found:\n", procs.len());
    for (pid, name) in &procs {
        // Count open FDs for this PID
        let fd_count = fd_count_for(*pid);
        out.push_str(&format!(
            "    pid {pid:<6}  {name:<35}  {fd_count} open FDs\n"
        ));
    }
    out
}

fn fd_count_for(pid: u32) -> usize {
    let lsof = std::process::Command::new("lsof")
        .args(["-p", &pid.to_string()])
        .output();
    match lsof {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .count()
            .saturating_sub(1), // subtract header line
        Err(_) => 0,
    }
}
