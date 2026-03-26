use anyhow::Result;
use std::fs;

// Binaries that are part of the philotic footprint
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
    "agent-graph-runner",
];

pub async fn run(kill_pattern: Option<String>) -> Result<()> {
    let procs = find_processes();
    let sockets = find_sockets();
    let pid_file = crate::start::pid_path();

    if procs.is_empty() && sockets.is_empty() {
        println!("No philotic footprint found.");
        return Ok(());
    }

    // ── Processes ─────────────────────────────────────────────────────────
    if !procs.is_empty() {
        println!("Processes:");
        for p in &procs {
            println!("  pid {:>6}  {}", p.pid, p.name);
        }
    }

    // ── Sockets ───────────────────────────────────────────────────────────
    if !sockets.is_empty() {
        println!("Sockets:");
        for s in &sockets {
            println!("  {s}");
        }
    }

    // ── PID file ──────────────────────────────────────────────────────────
    if pid_file.exists() {
        let pid = fs::read_to_string(&pid_file).unwrap_or_default();
        println!("PID file:  {} (pid {})", pid_file.display(), pid.trim());
    }

    // ── Kill ──────────────────────────────────────────────────────────────
    if let Some(pattern) = kill_pattern {
        let kill_all = pattern == "*" || pattern == "all";
        let to_kill: Vec<&Process> = if kill_all {
            procs.iter().collect()
        } else {
            procs.iter().filter(|p| p.name.contains(&pattern)).collect()
        };

        if to_kill.is_empty() {
            println!("\nNo processes matched '{pattern}'.");
            return Ok(());
        }

        // Use SIGKILL for "all" (the p-53 kill switch for abandoned processes);
        // use SIGTERM for targeted pattern kills.
        let (signal, signal_name) = if kill_all { ("-KILL", "SIGKILL") } else { ("-TERM", "SIGTERM") };

        println!("\nKilling {} process(es) with {}:", to_kill.len(), signal_name);
        for p in &to_kill {
            let result = std::process::Command::new("kill")
                .arg(signal)
                .arg(p.pid.to_string())
                .status();
            match result {
                Ok(s) if s.success() => println!("  {} → pid {}  {}", signal_name, p.pid, p.name),
                _ => println!("  FAILED  → pid {}  {}", p.pid, p.name),
            }
        }

        // Clean up sockets and PID file if killing everything
        if kill_all {
            for s in &sockets {
                let _ = fs::remove_file(s);
            }
            let _ = fs::remove_file(&pid_file);
            if !sockets.is_empty() {
                println!("  sockets removed");
            }
            println!("\nTip: run `phil flush --restart <hotel>` to reboot cleanly.");
        }
    }

    Ok(())
}

struct Process {
    pid: u32,
    name: String,
}

fn find_processes() -> Vec<Process> {
    let mut results = Vec::new();

    let output = std::process::Command::new("pgrep")
        .arg("-a")
        .arg("-l")
        .output();

    // pgrep -a not available on all platforms; fall back to ps
    let lines = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => {
            // Fallback: ps aux
            std::process::Command::new("ps")
                .args(["aux"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default()
        }
    };

    for line in lines.lines() {
        for bin in PHILOTIC_BINS {
            if line.contains(bin) {
                // Try to parse pid from first token
                let mut parts = line.split_whitespace();
                // ps aux: USER PID ..., pgrep -l: PID name
                // Just grab first numeric token
                let pid_str = parts.find(|t| t.parse::<u32>().is_ok());
                if let Some(pid_str) = pid_str {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        // Avoid duplicates
                        if !results.iter().any(|p: &Process| p.pid == pid) {
                            results.push(Process {
                                pid,
                                name: bin.to_string(),
                            });
                        }
                    }
                }
                break;
            }
        }
    }

    results
}

fn find_sockets() -> Vec<String> {
    let mut sockets = Vec::new();

    // /tmp sockets
    if let Ok(paths) = glob::glob("/tmp/philotic-*.sock") {
        sockets.extend(paths.filter_map(|p| p.ok()).map(|p| p.display().to_string()));
    }

    // Profile dir sockets — ~/.philotic/*.sock and ~/.philotic/*/*.sock
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let base = home.join(".philotic");
    for pattern in [
        base.join("*.sock").to_string_lossy().into_owned(),
        base.join("*").join("*.sock").to_string_lossy().into_owned(),
    ] {
        if let Ok(paths) = glob::glob(&pattern) {
            sockets.extend(paths.filter_map(|p| p.ok()).map(|p| p.display().to_string()));
        }
    }

    sockets
}
