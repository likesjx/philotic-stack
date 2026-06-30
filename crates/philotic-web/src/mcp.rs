use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Subcommand, Debug)]
pub enum McpAction {
    /// Run MCP client UAT through scripts/mcp-client-uat.sh.
    ///
    /// Defaults to strict live mode, which requires Perplexity and LifeGraph
    /// bearer tokens through env vars or token files.
    Uat {
        /// UAT mode passed to scripts/mcp-client-uat.sh.
        #[arg(default_value = "live")]
        mode: String,

        /// File containing the Perplexity/frontdoor bearer token.
        #[arg(long)]
        perplexity_token_file: Option<PathBuf>,

        /// File containing the LifeGraph readonly bearer token.
        #[arg(long)]
        lifegraph_token_file: Option<PathBuf>,

        /// Override the Perplexity/frontdoor MCP URL.
        #[arg(long)]
        perplexity_url: Option<String>,

        /// Override the LifeGraph MCP URL.
        #[arg(long)]
        lifegraph_url: Option<String>,

        /// Include remote native Muninn private smoke where the script supports it.
        #[arg(long)]
        run_remote: bool,

        /// Override the UAT script path.
        #[arg(long)]
        script: Option<PathBuf>,
    },
}

pub async fn run(action: McpAction) -> Result<()> {
    match action {
        McpAction::Uat {
            mode,
            perplexity_token_file,
            lifegraph_token_file,
            perplexity_url,
            lifegraph_url,
            run_remote,
            script,
        } => {
            run_uat(
                mode,
                perplexity_token_file,
                lifegraph_token_file,
                perplexity_url,
                lifegraph_url,
                run_remote,
                script,
            )
            .await
        }
    }
}

async fn run_uat(
    mode: String,
    perplexity_token_file: Option<PathBuf>,
    lifegraph_token_file: Option<PathBuf>,
    perplexity_url: Option<String>,
    lifegraph_url: Option<String>,
    run_remote: bool,
    script: Option<PathBuf>,
) -> Result<()> {
    let script = match script {
        Some(path) => path,
        None => find_uat_script()?,
    };
    ensure_readable(&script).with_context(|| format!("UAT script {}", script.display()))?;

    let mut cmd = Command::new(&script);
    cmd.arg(&mode);

    if let Some(path) = perplexity_token_file {
        ensure_readable(&path)
            .with_context(|| format!("Perplexity token file {}", path.display()))?;
        cmd.env("PERPLEXITY_MCP_TOKEN_FILE", path);
    }
    if let Some(path) = lifegraph_token_file {
        ensure_readable(&path)
            .with_context(|| format!("LifeGraph token file {}", path.display()))?;
        cmd.env("LIFEGRAPH_MCP_TOKEN_FILE", path);
    }
    if let Some(url) = perplexity_url {
        cmd.env("PERPLEXITY_MCP_URL", url);
    }
    if let Some(url) = lifegraph_url {
        cmd.env("LIFEGRAPH_MCP_URL", url);
    }
    if run_remote {
        cmd.env("RUN_REMOTE", "1");
    }

    println!(
        "Running MCP client UAT mode '{mode}' via {}",
        script.display()
    );
    println!("Token values are passed by environment or token files and are not printed.");

    let status = cmd.status().await.context("run MCP client UAT script")?;
    if !status.success() {
        anyhow::bail!("MCP client UAT failed with status {status}");
    }

    Ok(())
}

fn find_uat_script() -> Result<PathBuf> {
    let current = std::env::current_dir().context("resolve current directory")?;
    for dir in current.ancestors() {
        let candidate = dir.join("scripts").join("mcp-client-uat.sh");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    anyhow::bail!(
        "could not find scripts/mcp-client-uat.sh from {}; run inside philotic-stack or pass --script",
        current.display()
    )
}

fn ensure_readable(path: &Path) -> Result<()> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("read metadata for {}", path.display()))?;
    if !meta.is_file() {
        anyhow::bail!("{} is not a file", path.display());
    }
    std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(())
}
