use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod autonomy;
mod component;
mod doctor;
mod explain;
mod flush;
mod footprint;
mod harness;
mod heal;
mod init;
mod keys;
mod load;
mod mcp;
mod memory_explain;
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

        /// Hotel name to inspect (default: default)
        #[arg(long, default_value = "default")]
        hotel: String,
    },

    /// Self-diagnosis: detect known failure patterns, optionally repairing them
    ///
    /// Without --fix this is read-only: prints each finding's repair plan
    /// (Planned/NeedsConfirm/NotRepairable) but never writes. With --fix,
    /// auto-repairable checks (logs rotation, stale IPC sockets) are applied;
    /// checks that need an operator decision (port drift, orphan processes)
    /// still only print NeedsConfirm — they are never auto-applied. The
    /// context DB itself is always opened read-only.
    Doctor {
        /// Hotel name to inspect (default: default). Display/label only —
        /// does not select which context DB is opened; see --profile/--db.
        #[arg(long, default_value = "default")]
        hotel: String,

        /// Emit machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,

        /// Minimum severity to report and gate the exit code on
        #[arg(long, default_value = "warning")]
        severity_min: String,

        /// Run only these check IDs (repeatable)
        #[arg(long = "only")]
        only: Vec<String>,

        /// Skip these check IDs (repeatable)
        #[arg(long = "skip")]
        skip: Vec<String>,

        /// Print the check catalog (id + severity) and exit without running anything
        #[arg(long)]
        list_checks: bool,

        /// Apply auto-repairable fixes (logs rotation, stale IPC sockets).
        /// Checks that need operator confirmation (ports, orphan processes)
        /// are never auto-applied even with this flag; vault divergence is
        /// never touched by doctor at all.
        #[arg(long)]
        fix: bool,

        /// Target this profile's context DB (~/.philotic/<name>/context.db),
        /// independent of the PHILOTIC_PROFILE env var. Overridden by --db.
        #[arg(long)]
        profile: Option<String>,

        /// Open this exact context DB path, bypassing --profile and
        /// PHILOTIC_PROFILE entirely. Highest-precedence targeting option.
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// Explain the decision chain behind an agent-facing action
    Explain {
        #[command(subcommand)]
        action: explain::ExplainAction,
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

    /// MCP credential and client UAT helpers
    Mcp {
        #[command(subcommand)]
        action: mcp::McpAction,
    },

    /// Manage provider keys and model configuration in the hotel vault/config plane
    Keys {
        #[command(subcommand)]
        action: keys::KeysAction,
    },

    /// Inspect and close self-heal circuit work items (Autopoiesis Slice A3)
    Heal {
        #[command(subcommand)]
        action: heal::HealAction,
    },

    /// Autonomy trust ledger — per-lane posture, budget, and promotion
    /// eligibility (Autopoiesis Slice A9)
    Autonomy {
        #[command(subcommand)]
        action: autonomy::AutonomyAction,
    },

    /// Memory Transparency — merged provenance query across Muninn, the intel
    /// graph, and LifeGraph (Memory Transparency Slice M2)
    Memory {
        #[command(subcommand)]
        action: memory_explain::MemoryAction,
    },

    /// Project intelligence graph — scan, query, and serve the codebase graph
    Graph {
        #[command(subcommand)]
        action: GraphAction,
    },

    /// Manage agent roles
    Role {
        #[command(subcommand)]
        action: RoleAction,
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

        /// Optional browser path to open after the server starts (default: /)
        #[arg(long)]
        open_path: Option<String>,
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

    /// Show what is green right now: active proposals + their latest recorded
    /// test run (pass/fail counts, age), read straight from recorded
    /// TestRun/TestedBy evidence rather than the prose verification_level field.
    Green,

    /// Search the graph
    Search {
        /// Search query
        query: String,
    },

    /// Manage local external harnesses and record desired/rendered/observed state in intel-graph
    Harness {
        /// Graph DB to operate on (default: live DB; env: PHILOTIC_GRAPH_DB).
        /// Use a scratch path when testing so dev iterations never pollute
        /// the live registry.
        #[arg(long, global = true)]
        db: Option<String>,
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
enum RoleAction {
    /// Pin a role to a specific hotel so its philote guest process runs there.
    ///
    /// Example: phil role set-home librarian obsidian-keeper mac-jane
    SetHome {
        /// Agent ID (e.g. "librarian")
        agent_id: String,

        /// Role name (e.g. "obsidian-keeper")
        role_name: String,

        /// Hotel node_id where this role should run (e.g. "mac-jane").
        /// Pass "-" or "none" to clear the pin (run on authority hotel).
        home_node: String,

        /// Path to aiua_context.db (default: profile db or ./aiua_context.db)
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// Show the home_node pin for all roles of an agent.
    ///
    /// Example: phil role list-homes librarian
    ListHomes {
        /// Agent ID (e.g. "librarian")
        agent_id: String,

        /// Path to aiua_context.db
        #[arg(long)]
        db: Option<PathBuf>,
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

/// Format a chrono::Duration as a short human-readable age string (e.g. "5m", "3h", "2d").
fn format_age(age: chrono::Duration) -> String {
    let secs = age.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
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
        Command::Status { config, hotel } => status::run(config, hotel).await,
        Command::Doctor {
            hotel,
            json,
            severity_min,
            only,
            skip,
            list_checks,
            fix,
            profile,
            db,
        } => doctor::run(
            hotel,
            json,
            severity_min,
            only,
            skip,
            list_checks,
            fix,
            profile,
            db,
        ),
        Command::Explain { action } => explain::run(action),
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
        Command::Mcp { action } => mcp::run(action).await,
        Command::Keys { action } => keys::run(action).await,
        Command::Heal { action } => heal::run(action).await,
        Command::Autonomy { action } => autonomy::run(action).await,
        Command::Memory { action } => memory_explain::run(action).await,
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
                GraphAction::Green => {
                    use graph_intelligence::schema::{EdgeRelation, NodeKind};

                    let engine = GraphEngine::open(&config.db_path)?;
                    let mut proposals = engine.query_nodes(Some(NodeKind::Proposal), None)?;
                    proposals.retain(|p| {
                        p.properties.get("status").and_then(|v| v.as_str()) == Some("active")
                    });
                    proposals.sort_by(|a, b| a.name.cmp(&b.name));

                    let now = chrono::Utc::now();
                    println!(
                        "{:<42} {:<12} {:<10} {}",
                        "PROPOSAL", "RUN", "AGE", "STATUS"
                    );
                    println!("{}", "\u{2500}".repeat(80));

                    if proposals.is_empty() {
                        println!("(no active proposals found)");
                        return Ok(());
                    }

                    for p in &proposals {
                        let mut latest: Option<graph_intelligence::schema::Node> = None;
                        for e in engine
                            .get_edges_to(&p.id)?
                            .into_iter()
                            .filter(|e| e.relation == EdgeRelation::TestedBy)
                        {
                            if let Some(run) = engine.get_node(&e.source_id)? {
                                if run.kind == NodeKind::TestRun
                                    && latest
                                        .as_ref()
                                        .map(|l| run.created_at > l.created_at)
                                        .unwrap_or(true)
                                {
                                    latest = Some(run);
                                }
                            }
                        }

                        match latest {
                            Some(run) => {
                                let pass = run
                                    .properties
                                    .get("pass_count")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(0);
                                let total = run
                                    .properties
                                    .get("test_count")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(0);
                                let fail = run
                                    .properties
                                    .get("fail_count")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(0);
                                let age = now.signed_duration_since(run.created_at);
                                let status = if total == 0 {
                                    "empty"
                                } else if fail == 0 {
                                    "green"
                                } else {
                                    "red"
                                };
                                println!(
                                    "{:<42} {:<12} {:<10} {}",
                                    p.name,
                                    format!("{}/{}", pass, total),
                                    format_age(age),
                                    status
                                );
                            }
                            None => {
                                println!("{:<42} {:<12} {:<10} {}", p.name, "-", "-", "none");
                            }
                        }
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
                GraphAction::Harness { db, action } => harness::run(action, db),
            }
        }
        Command::Role { action } => {
            use ansible_mesh_core::domain::GraphDomain;
            use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
            use anyhow::Context as _;
            use std::sync::Arc;

            fn resolve_db(db: Option<PathBuf>) -> PathBuf {
                db.unwrap_or_else(|| match crate::init::active_profile() {
                    Some(_) => crate::init::profile_dir().join("aiua_context.db"),
                    None => PathBuf::from("aiua_context.db"),
                })
            }

            match action {
                RoleAction::SetHome {
                    agent_id,
                    role_name,
                    home_node,
                    db,
                } => {
                    let db_path = resolve_db(db);
                    let storage = SqliteGraphStorage::open(&db_path)
                        .with_context(|| format!("open {}", db_path.display()))?;
                    let graph = GraphDomain::new(Arc::new(storage.adapter()));

                    let mut record = graph
                        .get_role_incarnation(&agent_id, &role_name)?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "role '{}' not found for agent '{}'",
                                role_name,
                                agent_id
                            )
                        })?;

                    let new_home = if home_node == "-" || home_node.to_lowercase() == "none" {
                        None
                    } else {
                        Some(home_node.clone())
                    };

                    record.home_node = new_home.clone();
                    graph.upsert_role_incarnation(&record)?;

                    match new_home {
                        Some(node) => println!(
                            "Role '{role_name}' (agent '{agent_id}') pinned to home_node '{node}'."
                        ),
                        None => println!(
                            "Role '{role_name}' (agent '{agent_id}') home_node cleared — runs on authority hotel."
                        ),
                    }
                    Ok(())
                }

                RoleAction::ListHomes { agent_id, db } => {
                    let db_path = resolve_db(db);
                    let storage = SqliteGraphStorage::open(&db_path)
                        .with_context(|| format!("open {}", db_path.display()))?;
                    let graph = GraphDomain::new(Arc::new(storage.adapter()));

                    let roles = graph.list_role_incarnations(&agent_id)?;
                    if roles.is_empty() {
                        println!("No roles found for agent '{agent_id}'.");
                    } else {
                        println!("{:<20} {:<20} {}", "ROLE", "HOME_NODE", "GUEST_ID");
                        for r in &roles {
                            println!(
                                "{:<20} {:<20} {}",
                                r.role_name,
                                r.home_node.as_deref().unwrap_or("(authority hotel)"),
                                r.guest_id
                            );
                        }
                    }
                    Ok(())
                }
            }
        }
        Command::Serve {
            port,
            db,
            config,
            allow_origins,
            open_path,
        } => serve::run(port, db, config, allow_origins, open_path).await,
    }
}
