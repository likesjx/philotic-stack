use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod component;
mod flush;
mod footprint;
mod init;
mod load;
mod muninn;
mod reset;
mod serve;
mod service;
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

    /// Start the local aiua daemon (boots from DB — run `phil load` first if DB is empty)
    Start {
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

    /// Apply a config file to the Context Graph DB (run once on setup or when config changes)
    Load {
        /// Path to config file (default: mesh-config.json, or ~/.philotic/<profile>/config.json)
        #[arg(long, short)]
        file: Option<PathBuf>,

        /// Hotel section to seed (default: "default")
        #[arg(long, default_value = "default")]
        hotel: String,
    },

    /// Manage the aiua launchd service (macOS only)
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },

    /// Show all running philotic processes, sockets, and PID files
    Footprint {
        /// Kill matched processes (* or 'all' to SIGKILL everything, or a name pattern e.g. 'membrane')
        #[arg(long)]
        kill: Option<String>,
    },

    /// Kill ALL philotic processes (SIGKILL) and wipe all sockets, then optionally restart a hotel.
    ///
    /// Use this when abandoned processes are exhausting file descriptors (OS error 24).
    /// Equivalent to `just clear-aiua` but restarts the hotel afterward if --restart is given.
    Flush {
        /// Hotel to restart after flushing (omit to flush only)
        #[arg(long)]
        restart: Option<String>,
    },

    /// Manage registered components (model controllers, tool runners, etc.)
    Component {
        #[command(subcommand)]
        action: ComponentAction,
    },

    /// Start the local management web server (REST + WebSocket)
    Serve {
        /// Port to listen on (default: 7700)
        #[arg(long, default_value = "7700")]
        port: u16,

        /// Path to aiua_context.db (default: ./aiua_context.db)
        #[arg(long)]
        db: Option<PathBuf>,

        /// Path to mesh-config.json for agent metadata (default: ./mesh-config.json)
        #[arg(long, short)]
        config: Option<PathBuf>,

        /// Allowed CORS origins, comma-separated (default: http://localhost:5173)
        #[arg(long)]
        allow_origins: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum ComponentAction {
    /// Register (or update) a component from a ComponentManifest JSON file.
    ///
    /// Example: phil component add mlx-fleet.json
    Add {
        /// Path to the ComponentManifest JSON file.
        manifest: std::path::PathBuf,
    },
    /// List registered components.
    List,
    /// Deactivate a registered component (sets is_active=false).
    Remove {
        /// Guest ID of the component to remove.
        guest_id: String,
    },
}

#[derive(Subcommand, Debug)]
enum ServiceAction {
    /// Install and start the aiua launchd service
    Install {
        /// Hotel to run (default: "default")
        #[arg(long, default_value = "default")]
        hotel: String,
    },
    /// Stop and uninstall the aiua launchd service
    Uninstall {
        /// Hotel to uninstall (default: "default")
        #[arg(long, default_value = "default")]
        hotel: String,
    },
    /// Show launchd service status
    Status {
        /// Hotel to query (default: "default")
        #[arg(long, default_value = "default")]
        hotel: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { config, force } => init::run(config, force).await,
        Command::Load { file, hotel } => load::run(file, hotel).await,
        Command::Start { hotel, detach } => start::run(hotel, detach).await,
        Command::Stop => stop::run().await,
        Command::Status { config } => status::run(config).await,
        Command::Agents { config } => status::run_agents(config).await,
        Command::Reset { keep_identity } => reset::run(keep_identity).await,
        Command::Service { action } => match action {
            ServiceAction::Install { hotel } => service::install(hotel).await,
            ServiceAction::Uninstall { hotel } => service::uninstall(hotel).await,
            ServiceAction::Status { hotel } => service::status(hotel).await,
        },
        Command::Footprint { kill } => footprint::run(kill).await,
        Command::Flush { restart } => flush::run(restart).await,
        Command::Component { action } => match action {
            ComponentAction::Add { manifest } => component::add(manifest).await,
            ComponentAction::List => component::list().await,
            ComponentAction::Remove { guest_id } => component::remove(guest_id).await,
        },
        Command::Serve {
            port,
            db,
            config,
            allow_origins,
        } => serve::run(port, db, config, allow_origins).await,
    }
}
