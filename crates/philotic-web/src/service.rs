use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::init::{active_profile, profile_dir};
use crate::start::find_aiua_binary;

/// Build the `<key>EnvironmentVariables</key>` block for the launchd plist.
///
/// Returns an empty string when no profile is active (backward-compatible with
/// legacy single-hotel installs that never set `PHILOTIC_PROFILE`).
///
/// When a profile IS active we pin two variables:
///   - `PHILOTIC_PROFILE` — the hotel's profile namespace.
///   - `PHILOTIC_GRAPH_DB_PATH` — the hotel's per-profile `context.db`. This is
///     the same DB the hotel daemon owns; the model-router guest opens it as a
///     sidecar (WAL) to persist model latency/error signals that feed the model
///     oracle's health-aware ranking. On macOS this var was never set (only the
///     Linux systemd unit set it), so the oracle health feed was dormant on all
///     Mac hotels. See `crates/model-router/src/runtime.rs` (reads the env var
///     directly) and the oracle read path in `crates/aiua/src/service/ipc.rs`.
fn render_env_block(profile: Option<&str>, graph_db_path: &Path) -> String {
    let Some(profile) = profile else {
        return String::new();
    };
    let entries = format!(
        "        <key>PHILOTIC_PROFILE</key>\n        <string>{profile}</string>\n\
         \x20       <key>PHILOTIC_GRAPH_DB_PATH</key>\n        <string>{db}</string>",
        profile = profile,
        db = graph_db_path.display(),
    );
    format!("\n    <key>EnvironmentVariables</key>\n    <dict>\n{entries}\n    </dict>")
}

fn plist_label(hotel: &str) -> String {
    match active_profile() {
        Some(ref p) => format!("com.philotic.aiua.{p}.{hotel}"),
        None => format!("com.philotic.aiua.{hotel}"),
    }
}

fn plist_path(hotel: &str) -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", plist_label(hotel)))
}

fn launchd_domain() -> String {
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "501".to_string());
    format!("gui/{uid}")
}

pub(crate) fn service_target(hotel: &str) -> String {
    format!("{}/{}", launchd_domain(), plist_label(hotel))
}

fn launchctl_status(args: &[&str]) -> Result<std::process::Output> {
    std::process::Command::new("launchctl")
        .args(args)
        .output()
        .context("launchctl")
}

fn service_loaded(hotel: &str) -> bool {
    launchctl_status(&["print", &service_target(hotel)])
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub async fn install(hotel: String) -> Result<()> {
    let aiua_bin = find_aiua_binary()?;
    let label = plist_label(&hotel);
    let plist = plist_path(&hotel);
    let log_dir = profile_dir();
    let out_log = log_dir.join("aiua.log");
    let err_log = log_dir.join("aiua.err.log");

    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("create log dir {}", log_dir.display()))?;

    // Pin the per-profile context.db as PHILOTIC_GRAPH_DB_PATH so the model
    // oracle health feed (fed by the model-router sidecar) works on macOS.
    let graph_db_path = profile_dir().join("context.db");
    let env_block = render_env_block(active_profile().as_deref(), &graph_db_path);

    let plist_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{aiua}</string>
        <string>--hotel</string>
        <string>{hotel}</string>
    </array>{env_block}
    <key>SoftResourceLimits</key>
    <dict>
        <key>NumberOfFiles</key>
        <integer>65536</integer>
    </dict>
    <key>HardResourceLimits</key>
    <dict>
        <key>NumberOfFiles</key>
        <integer>65536</integer>
    </dict>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
</dict>
</plist>
"#,
        label = label,
        aiua = aiua_bin.display(),
        hotel = hotel,
        env_block = env_block,
        out = out_log.display(),
        err = err_log.display(),
    );

    std::fs::write(&plist, &plist_xml)
        .with_context(|| format!("write plist {}", plist.display()))?;
    println!("Wrote plist: {}", plist.display());

    // If already bootstrapped, bootout first so we can re-bootstrap cleanly.
    let domain = launchd_domain();
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &domain, plist.to_str().unwrap_or("")])
        .output();

    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain, plist.to_str().context("plist path")?])
        .status()
        .context("launchctl bootstrap")?;

    if !status.success() {
        bail!(
            "launchctl bootstrap failed — check plist at {}",
            plist.display()
        );
    }

    println!("✓ Service '{label}' installed and started.");
    println!("  Logs: {}", out_log.display());
    println!("  Run `phil service status` to verify.");
    Ok(())
}

pub async fn start(hotel: String) -> Result<()> {
    let label = plist_label(&hotel);
    let plist = plist_path(&hotel);
    let domain = launchd_domain();
    let target = service_target(&hotel);

    if !plist.exists() {
        bail!("no plist found at {}", plist.display());
    }

    if service_loaded(&hotel) {
        let status = std::process::Command::new("launchctl")
            .args(["kickstart", "-k", &target])
            .status()
            .context("launchctl kickstart")?;
        if !status.success() {
            bail!("launchctl kickstart failed for {label}");
        }
    } else {
        let status = std::process::Command::new("launchctl")
            .args(["bootstrap", &domain, plist.to_str().context("plist path")?])
            .status()
            .context("launchctl bootstrap")?;
        if !status.success() {
            bail!(
                "launchctl bootstrap failed — check plist at {}",
                plist.display()
            );
        }
    }

    println!("Started service '{label}'.");
    println!("  Run `phil service status` to verify.");
    Ok(())
}

pub async fn stop(hotel: String) -> Result<()> {
    let label = plist_label(&hotel);
    let target = service_target(&hotel);

    if service_loaded(&hotel) {
        let status = std::process::Command::new("launchctl")
            .args(["bootout", &target])
            .status()
            .context("launchctl bootout")?;
        if !status.success() {
            bail!("launchctl bootout failed for {label}");
        }
        println!("Stopped service '{label}'.");
    } else {
        println!("Service '{label}' is not loaded.");
    }

    Ok(())
}

pub async fn restart(hotel: String) -> Result<()> {
    let label = plist_label(&hotel);
    let target = service_target(&hotel);

    if service_loaded(&hotel) {
        let stop_status = std::process::Command::new("launchctl")
            .args(["bootout", &target])
            .status()
            .context("launchctl bootout")?;
        if !stop_status.success() {
            bail!("launchctl bootout failed for {label}");
        }
    }

    start(hotel).await?;
    println!("Restarted service '{label}'.");
    Ok(())
}

pub async fn uninstall(hotel: String) -> Result<()> {
    let label = plist_label(&hotel);
    let plist = plist_path(&hotel);
    let domain = launchd_domain();

    let status = std::process::Command::new("launchctl")
        .args(["bootout", &format!("{domain}/{label}")])
        .status()
        .context("launchctl bootout")?;

    if status.success() {
        println!("Stopped service '{label}'.");
    } else {
        println!("Service '{label}' was not running (or already stopped).");
    }

    if plist.exists() {
        std::fs::remove_file(&plist)
            .with_context(|| format!("remove plist {}", plist.display()))?;
        println!("Removed plist: {}", plist.display());
    } else {
        println!("No plist found at {}", plist.display());
    }

    println!("✓ Service '{label}' uninstalled.");
    Ok(())
}

pub async fn status(hotel: String) -> Result<()> {
    let label = plist_label(&hotel);
    let plist = plist_path(&hotel);
    let domain = launchd_domain();

    println!("Service: {label}");
    println!("Plist:   {}", plist.display());
    println!("Exists:  {}", if plist.exists() { "yes" } else { "no" });
    println!();

    let output = std::process::Command::new("launchctl")
        .args(["print", &format!("{domain}/{label}")])
        .output()
        .context("launchctl print")?;

    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        // Extract the key lines
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("state")
                || trimmed.starts_with("pid")
                || trimmed.starts_with("last exit")
                || trimmed.starts_with("runs")
            {
                println!("  {trimmed}");
            }
        }
    } else {
        println!("  status: not loaded");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn env_block_pins_profile_and_graph_db_path() {
        let db = PathBuf::from("/Users/test/.philotic/jane/context.db");
        let block = render_env_block(Some("jane"), &db);

        assert!(block.contains("<key>EnvironmentVariables</key>"));
        assert!(block.contains("<key>PHILOTIC_PROFILE</key>"));
        assert!(block.contains("<string>jane</string>"));
        // The oracle health feed regression fix: the generated plist must carry
        // PHILOTIC_GRAPH_DB_PATH pointing at the hotel's per-profile context.db.
        assert!(block.contains("<key>PHILOTIC_GRAPH_DB_PATH</key>"));
        assert!(
            block.contains("<string>/Users/test/.philotic/jane/context.db</string>"),
            "graph db path missing/wrong in env block:\n{block}"
        );
    }

    #[test]
    fn env_block_uses_per_profile_path() {
        let bjork = render_env_block(
            Some("bjork"),
            &PathBuf::from("/Users/test/.philotic/bjork/context.db"),
        );
        assert!(bjork.contains("<string>/Users/test/.philotic/bjork/context.db</string>"));
        assert!(!bjork.contains("/jane/"));
    }

    #[test]
    fn env_block_empty_without_profile() {
        let block = render_env_block(None, &PathBuf::from("/x/context.db"));
        assert!(
            block.is_empty(),
            "no-profile installs must not emit an env block"
        );
    }
}
