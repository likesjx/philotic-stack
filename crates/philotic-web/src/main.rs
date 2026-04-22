use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod component;
mod flush;
mod footprint;
mod harness;
mod init;
mod load;
mod mesh;
mod muninn;
mod onboard;
mod presets;
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

        /// Run the interactive setup wizard (default when no config exists)
        #[arg(long, short)]
        interactive: bool,

        /// Skip the interactive wizard — write the raw template
        #[arg(long)]
        non_interactive: bool,
    },

    /// List available agent fleet presets
    Presets,

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

    /// Manage the aiua launchd service lifecycle (macOS only)
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

    /// Explicit mesh trust ceremony: create invites and accept memberships
    Mesh {
        #[command(subcommand)]
        action: MeshAction,
    },

    /// Project intelligence graph — scan, query, and serve the codebase graph
    Graph {
        #[command(subcommand)]
        action: GraphAction,
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
enum GraphAction {
    /// Run a full scan of code, docs, and git state
    Scan,

    /// Start the graph server (REST + MCP + WebSocket)
    Serve {
        /// HTTP port for REST API
        #[arg(long, default_value = "8900")]
        port: u16,

        /// MCP port for agent tools
        #[arg(long, default_value = "8901")]
        mcp_port: u16,

        /// Path to graph database
        #[arg(long)]
        db: Option<String>,
    },

    /// Show graph status summary
    Status,

    /// Generate PlantUML skeleton for a crate
    Skeleton {
        /// Crate name
        crate_name: String,
    },

    /// List all proposals with status
    Proposals,

    /// List all seams
    Seams,

    /// Search the graph
    Search {
        /// Search query
        query: String,
    },

    /// Manage local external harnesses and record desired/rendered/observed state in intel-graph
    Harness {
        #[command(subcommand)]
        action: harness::HarnessAction,
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
enum MeshAction {
    /// Create an invite file for another hotel to join this mesh
    Invite {
        /// Local hotel to invite from
        #[arg(long, default_value = "default")]
        hotel: String,

        /// Publicly reachable host or IP for this hotel's mesh UDP listener
        #[arg(long)]
        host: String,

        /// Output path for the invite JSON
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
    /// Accept an invite file and announce membership back to the inviter
    Accept {
        /// Path to the invite JSON file
        invite: PathBuf,

        /// Local hotel accepting the invite
        #[arg(long, default_value = "default")]
        hotel: String,

        /// Publicly reachable host or IP for this hotel's mesh UDP listener
        #[arg(long)]
        host: String,
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
    /// Start the aiua launchd service
    Start {
        /// Hotel to run (default: "default")
        #[arg(long, default_value = "default")]
        hotel: String,
    },
    /// Stop the aiua launchd service without removing the plist
    Stop {
        /// Hotel to run (default: "default")
        #[arg(long, default_value = "default")]
        hotel: String,
    },
    /// Stop and then start the aiua launchd service
    Restart {
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
        Command::Init {
            config,
            force,
            interactive,
            non_interactive,
        } => {
            let config_path = config.unwrap_or_else(|| match init::active_profile() {
                Some(_) => init::profile_dir().join("config.json"),
                None => std::path::PathBuf::from("mesh-config.json"),
            });

            // Interactive by default when no config exists, unless --non-interactive
            let should_interact = if non_interactive {
                false
            } else if interactive {
                true
            } else {
                !config_path.exists()
            };

            // Identity + muninn always run; config template only in non-interactive
            init::run_inner(Some(config_path.clone()), force, should_interact).await?;

            if should_interact {
                onboard::run_interactive(&config_path, force).await?;
            }

            Ok(())
        }
        Command::Presets => {
            println!("Available fleet presets:");
            println!();
            for (name, desc) in presets::list_preset_names() {
                let agents = presets::load_preset(&name)
                    .map(|p| {
                        p.agents
                            .iter()
                            .map(|a| a.persona_name.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                println!("  {name:<8} {desc}");
                println!("  {:<8} Agents: {agents}", "");
                println!();
            }
            Ok(())
        }
        Command::Load { file, hotel } => load::run(file, hotel).await,
        Command::Start { hotel, detach } => start::run(hotel, detach).await,
        Command::Stop => stop::run().await,
        Command::Status { config } => status::run(config).await,
        Command::Agents { config } => status::run_agents(config).await,
        Command::Reset { keep_identity } => reset::run(keep_identity).await,
        Command::Service { action } => match action {
            ServiceAction::Install { hotel } => service::install(hotel).await,
            ServiceAction::Start { hotel } => service::start(hotel).await,
            ServiceAction::Stop { hotel } => service::stop(hotel).await,
            ServiceAction::Restart { hotel } => service::restart(hotel).await,
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
        Command::Mesh { action } => match action {
            MeshAction::Invite { hotel, host, out } => mesh::invite(hotel, host, out).await,
            MeshAction::Accept {
                invite,
                hotel,
                host,
            } => mesh::accept(invite, hotel, host).await,
        },
        Command::Graph { action } => {
            use graph_intelligence::{scanner, GraphEngine};
            use philotic_graph::PhiloticGraphConfig;

            let config = PhiloticGraphConfig::default();

            match action {
                GraphAction::Scan => {
                    let mut engine = GraphEngine::open(&config.db_path)?;
                    let root = std::env::current_dir()?;
                    let result = scanner::full_scan(&root, &config.to_scan_config(), &mut engine)?;
                    println!("Scan complete:");
                    println!(
                        "  {} crates, {} modules, {} types, {} functions",
                        result.crates, result.modules, result.types, result.functions
                    );
                    println!(
                        "  {} tests, {} snippets, {} docs",
                        result.tests, result.snippets, result.docs
                    );
                    println!("  {} commits, {} branches", result.commits, result.branches);
                    println!("  Duration: {}ms", result.duration_ms);
                    Ok(())
                }
                GraphAction::Serve { port, mcp_port, db } => {
                    let mut cfg = config;
                    cfg.http_port = port;
                    cfg.mcp_port = mcp_port;
                    if let Some(db) = db {
                        cfg.db_path = db;
                    }
                    let repo_root = std::env::current_dir()?.to_string_lossy().to_string();
                    let server_config = cfg.to_server_config(&repo_root);
                    graph_intelligence::server::serve(server_config).await
                }
                GraphAction::Status => {
                    let engine = GraphEngine::open(&config.db_path)?;
                    let proposals = engine
                        .query_nodes(Some(graph_intelligence::schema::NodeKind::Proposal), None)?;
                    let crates = engine
                        .query_nodes(Some(graph_intelligence::schema::NodeKind::Crate), None)?;
                    let types = engine
                        .query_nodes(Some(graph_intelligence::schema::NodeKind::Type), None)?;
                    let fns = engine
                        .query_nodes(Some(graph_intelligence::schema::NodeKind::Function), None)?;

                    println!("Graph Intelligence Status");
                    println!("{}", "\u{2500}".repeat(25));
                    println!("  Proposals:  {}", proposals.len());
                    println!("  Crates:     {}", crates.len());
                    println!("  Types:      {}", types.len());
                    println!("  Functions:  {}", fns.len());

                    let mut by_status: std::collections::HashMap<String, usize> =
                        std::collections::HashMap::new();
                    for p in &proposals {
                        let status = p
                            .properties
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        *by_status.entry(status.to_string()).or_default() += 1;
                    }
                    println!("\n  Proposal Pipeline:");
                    for (status, count) in &by_status {
                        println!("    {status:<30} {count}");
                    }
                    Ok(())
                }
                GraphAction::Skeleton { crate_name } => {
                    let engine = GraphEngine::open(&config.db_path)?;
                    let uml =
                        graph_intelligence::plantuml::generate_crate_diagram(&engine, &crate_name)?;
                    println!("{uml}");
                    Ok(())
                }
                GraphAction::Proposals => {
                    let engine = GraphEngine::open(&config.db_path)?;
                    let proposals = engine
                        .query_nodes(Some(graph_intelligence::schema::NodeKind::Proposal), None)?;
                    println!("{:<45} {:<25} {}", "PROPOSAL", "STATUS", "DOMAIN");
                    println!("{}", "\u{2500}".repeat(90));
                    for p in &proposals {
                        let status = p
                            .properties
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let domain = p
                            .properties
                            .get("domain")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        println!("{:<45} {:<25} {}", p.name, status, domain);
                    }
                    Ok(())
                }
                GraphAction::Seams => {
                    let engine = GraphEngine::open(&config.db_path)?;
                    let seams = engine
                        .query_nodes(Some(graph_intelligence::schema::NodeKind::Seam), None)?;
                    println!("Registered seams: {}", seams.len());
                    for s in &seams {
                        println!("  {}", s.name);
                    }
                    Ok(())
                }
                GraphAction::Search { query } => {
                    let engine = GraphEngine::open(&config.db_path)?;
                    let nodes = engine.search_nodes(&query)?;
                    let snippets = engine.search_snippets(&query)?;
                    println!("Nodes matching '{}': {}", query, nodes.len());
                    for n in nodes.iter().take(20) {
                        println!("  [{:?}] {} \u{2014} {}", n.kind, n.id, n.name);
                    }
                    if !snippets.is_empty() {
                        println!("\nSnippets matching '{}': {}", query, snippets.len());
                        for s in snippets.iter().take(10) {
                            println!(
                                "  [{}] {} \u{2014} {}",
                                s.kind.as_str(),
                                s.node_id,
                                s.signature
                            );
                        }
                    }
                    Ok(())
                }
                GraphAction::Harness { action } => harness::run(action),
            }
        }
        Command::Serve {
            port,
            db,
            config,
            allow_origins,
        } => serve::run(port, db, config, allow_origins).await,
    }
}
