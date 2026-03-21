use anyhow::{bail, Result};
use std::time::Duration;
use tokio::time::sleep;

use crate::start::{pid_path, process_alive, read_pid};

pub async fn run() -> Result<()> {
    let Some(pid) = read_pid() else {
        println!("No aiua PID file found — is it running?");
        println!("Try `phil status` to check.");
        return Ok(());
    };

    if !process_alive(pid) {
        println!("aiua (pid {pid}) is not running — removing stale PID file.");
        let _ = std::fs::remove_file(pid_path());
        return Ok(());
    }

    // Send SIGTERM
    let result = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();

    match result {
        Ok(s) if s.success() => println!("Sent SIGTERM to aiua (pid {pid})"),
        _ => bail!("Failed to send SIGTERM to pid {pid}"),
    }

    // Wait for process to exit (up to 10s)
    print!("Waiting for aiua to stop");
    for _ in 0..20 {
        sleep(Duration::from_millis(500)).await;
        if !process_alive(pid) {
            let _ = std::fs::remove_file(pid_path());
            println!(" stopped");
            return Ok(());
        }
        print!(".");
    }

    println!("\naiua did not exit after 10s — sending SIGKILL");
    let _ = std::process::Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status();
    let _ = std::fs::remove_file(pid_path());
    println!("aiua killed.");
    Ok(())
}
