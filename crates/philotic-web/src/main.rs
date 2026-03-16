use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod footprint;
mod init;
mod muninn;
mod reset;
mod start;
mod status;
mod stop;

/// philotic-web — operator CLI for the Philotic Web
///
/// Alias: phil
#[derive(Parser, Debug)]
#[command(
    name = "philotic-web",
    bin_name = "phil",
    version,
    about = "Manage aiua nodes, agents, and mesh topology",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// First-run setup: generate operator keypair and write mesh-config template
    Init {
        /// Path to write mesh-config.json (default: ./mesh-config.json)
        #[arg(long, short)]
        config: Option<PathBuf>,

        /// Overwrite existing mesh-config.json
        #[arg(long)]
        force: bool,
    },

    /// Start the local aiua daemon
    Start {
        /// Path to mesh-config.json (default: ./mesh-config.json)
        #[arg(long, short)]
        config: Option<PathBuf>,

        /// Hotel name to boot (default: default)
        #[arg(long, default_value = "default")]
        hotel: String,

        /// Don't tail startup output; return immediately after spawning
        #[arg(long)]
        detach: bool,
    },

    /// Stop the local aiua daemon
    Stop,

    /// Show status of the local aiua daemon and its agents
    Status {
        /// Path to mesh-config.json for config-level agent listing (default: ./mesh-config.json)
        #[arg(long, short)]
        config: Option<PathBuf>,
    },

    /// List configured agents
    Agents {
        /// Path to mesh-config.json (default: ./mesh-config.json)
        #[arg(long, short)]
        config: Option<PathBuf>,
    },

    /// Stop all services and wipe local state (aiua, muninn, sockets, data dirs)
    Reset {
        /// Keep ~/.philotic/identity/ (preserves operator keypair)
        #[arg(long)]
        keep_identity: bool,
    },

    /// Show all running philotic processes, sockets, and PID files
    Footprint {
        /// Kill matched processes (* or 'all' to kill everything, or a name pattern e.g. 'membrane')
        #[arg(long)]
        kill: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { config, force } => init::run(config, force).await,
        Command::Start { config, hotel, detach } => start::run(config, hotel, detach).await,
        Command::Stop => stop::run().await,
        Command::Status { config } => status::run(config).await,
        Command::Agents { config } => status::run_agents(config).await,
        Command::Reset { keep_identity } => reset::run(keep_identity).await,
        Command::Footprint { kill } => footprint::run(kill).await,
    }
}
