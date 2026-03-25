use anyhow::Result;
use std::fs;

use crate::init::philotic_dir;
use crate::start::{process_alive, read_pid};

pub async fn run(keep_identity: bool) -> Result<()> {
    println!("phil reset");
    println!("─────────────────────────────────────────");

    // ── Stop aiua ─────────────────────────────────────────────────────────
    if let Some(pid) = read_pid() {
        if process_alive(pid) {
            print!("  aiua      stopping (pid {pid})...");
            let _ = std::process::Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status();
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if !process_alive(pid) {
                    break;
                }
            }
            if process_alive(pid) {
                let _ = std::process::Command::new("kill")
                    .arg("-KILL")
                    .arg(pid.to_string())
                    .status();
            }
            println!(" done");
        } else {
            println!("  aiua      not running");
        }
    } else {
        println!("  aiua      not running");
    }

    // ── Stop muninn ────────────────────────────────────────────────────────
    print!("  muninn    stopping...");
    let muninn_result = std::process::Command::new("muninn").arg("stop").output();
    match muninn_result {
        Ok(out) if out.status.success() => println!(" done"),
        Ok(_) => println!(" (not running)"),
        Err(_) => println!(" (muninn not found in PATH — skipping)"),
    }

    // ── Clear stale sockets ────────────────────────────────────────────────
    let socks: Vec<_> = glob::glob("/tmp/philotic-*.sock")
        .map(|paths| paths.filter_map(|p| p.ok()).collect())
        .unwrap_or_default();
    if socks.is_empty() {
        println!("  sockets   none found");
    } else {
        for sock in &socks {
            let _ = fs::remove_file(sock);
        }
        println!("  sockets   removed {}", socks.len());
    }

    // ── Wipe ~/.philotic ──────────────────────────────────────────────────
    let philotic = philotic_dir();
    if keep_identity {
        // Remove everything except identity/
        for entry in fs::read_dir(&philotic).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.file_name().map(|n| n != "identity").unwrap_or(false) {
                let _ = if path.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
            }
        }
        println!("  ~/.philotic  cleared (identity preserved)");
    } else if philotic.exists() {
        fs::remove_dir_all(&philotic)?;
        println!("  ~/.philotic  removed");
    }

    // ── Wipe ~/.muninn ─────────────────────────────────────────────────────
    let muninn_dir = dirs::home_dir()
        .expect("cannot determine home directory")
        .join(".muninn");
    if muninn_dir.exists() {
        fs::remove_dir_all(&muninn_dir)?;
        println!("  ~/.muninn    removed");
    } else {
        println!("  ~/.muninn    not found");
    }

    println!("\nReset complete. Run `phil init && muninn init && muninn start && phil start` to start fresh.");
    Ok(())
}
