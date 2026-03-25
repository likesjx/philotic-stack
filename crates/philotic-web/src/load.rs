use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;

use crate::init::{active_profile, profile_dir};
use crate::start::find_aiua_binary;

/// Apply a config file to the Context Graph DB via `aiua load`.
/// Idempotent — safe to run at any time, with or without aiua running.
pub async fn run(file: Option<PathBuf>, hotel: String) -> Result<()> {
    let default_config = match active_profile() {
        Some(_) => profile_dir().join("config.json"),
        None => PathBuf::from("mesh-config.json"),
    };
    let config_path = file
        .unwrap_or(default_config)
        .canonicalize()
        .context("config file not found — check the path or run `phil init` first")?;

    let aiua_bin = find_aiua_binary()?;

    println!("Loading config into DB...");
    println!("  config: {}", config_path.display());
    println!("  hotel:  {hotel}");

    let mut cmd = tokio::process::Command::new(&aiua_bin);
    cmd.arg("load")
        .arg("--file")
        .arg(&config_path)
        .arg("--hotel")
        .arg(&hotel)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Some(ref p) = active_profile() {
        cmd.env("PHILOTIC_PROFILE", p);
    }

    let status = cmd
        .status()
        .await
        .with_context(|| format!("failed to run {}", aiua_bin.display()))?;

    if !status.success() {
        anyhow::bail!("`aiua load` exited with status {status}");
    }

    Ok(())
}
