use anyhow::Result;
use std::time::Duration;
use tokio::time::sleep;

/// Run `muninn init` interactively. Called from `phil init`.
/// Skips gracefully if muninn is not installed.
pub async fn run_init() -> Result<()> {
    match which_muninn() {
        None => {
            println!("  muninn    not found in PATH — skipping (install via brew)");
            return Ok(());
        }
        Some(bin) => {
            println!("\n  Running muninn init...");
            // muninn init is interactive — inherit stdio so the user can drive it
            let status = tokio::process::Command::new(&bin)
                .arg("init")
                .status()
                .await;
            match status {
                Ok(s) if s.success() => println!("  muninn    init complete"),
                Ok(s) => println!("  muninn    init exited with status {s}"),
                Err(e) => println!("  muninn    init failed: {e}"),
            }
        }
    }
    Ok(())
}

/// Ensure muninn is running before aiua starts. Called from `phil start`.
/// - If already running: no-op.
/// - If not running: run `muninn start` and wait up to 8s for the REST port.
/// - If muninn not installed: warn and continue (aiua will fail on connect if it needs it).
pub async fn ensure_running() -> Result<()> {
    let Some(bin) = which_muninn() else {
        println!("  muninn    not found in PATH — skipping (aiua may fail if it needs MuninnDB)");
        return Ok(());
    };

    if is_running().await {
        println!("  muninn    already running");
        return Ok(());
    }

    print!("  muninn    starting...");
    let status = tokio::process::Command::new(&bin)
        .arg("start")
        .status()
        .await;

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            println!(" exited with {s}");
            return Ok(());
        }
        Err(e) => {
            println!(" failed to spawn: {e}");
            return Ok(());
        }
    }

    // Wait up to 8s for muninn REST port (8475)
    for _ in 0..16 {
        sleep(Duration::from_millis(500)).await;
        if is_running().await {
            println!(" ready");
            return Ok(());
        }
        print!(".");
    }
    println!(" timed out (continuing anyway)");
    Ok(())
}

async fn is_running() -> bool {
    // Probe the muninn REST port
    tokio::net::TcpStream::connect("127.0.0.1:8475")
        .await
        .is_ok()
}

fn which_muninn() -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("which")
        .arg("muninn")
        .output()
        .ok()?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Some(std::path::PathBuf::from(s))
    } else {
        None
    }
}
