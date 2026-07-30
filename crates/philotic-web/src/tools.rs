//! `phil tools` — operator surface for the per-hotel tool grant registry.
//!
//! Implements the runtime-edit half of `proposal:data-driven-tool-grants-skilldag`:
//! tool grants and tool policy are data in the local hotel context graph, so
//! enabling, disabling, re-granting, or re-routing a tool is a DB write plus a
//! session refresh rather than a code change and a deploy.
//!
//! - slice 1 — `show` / `disable` / `enable` / `set-class` / `set-skill`
//! - slice 2 — `set-runner`, binding a runner's routes to a class grant
//! - slice 3 — `audit`, the append-only trail of who changed what
//!
//! Deliberately an operator CLI, not an agent-facing tool. Letting a model widen
//! its own reach is a governance question the proposal answers in slice 4 via the
//! LifeGraph compile-down path (agent proposes, the change compiles into this
//! registry) — not by handing agents a direct mutation tool.
//!
//! Every write stamps [`GrantSource::Runtime`], which is what stops the boot
//! seeder from reverting the edit on the next hotel restart, and every write is
//! audited before it lands.

use std::path::PathBuf;
use std::sync::Arc;

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::graph::{
    GrantSource, ToolClassGrant, ToolGrantAuditRecord, ToolGrantRegistryRecord, ToolRunnerGrant,
};
use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
use anyhow::{Context as _, Result};
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum ToolsAction {
    /// Show the tool grant registry: class grants and the disabled-tool policy
    Show {
        /// Path to the hotel context DB (default: active profile's aiua_context.db)
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// Disable a tool hotel-wide, regardless of which class or skill grants it
    Disable {
        /// Tool name, e.g. life.observe.batch
        tool: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// Re-enable a previously disabled tool
    Enable {
        /// Tool name, e.g. life.observe.batch
        tool: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// Replace the tools granted by a tool class
    ///
    /// Pass no tools to empty the class. An empty class grants nothing and is
    /// preserved across restarts — it does NOT fall back to the built-in table.
    SetClass {
        /// Tool class name, e.g. life_graph
        class: String,
        /// Tool names granted by this class (repeat the flag per tool)
        #[arg(long = "tool")]
        tools: Vec<String>,
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// Show the append-only audit trail of tool grant and policy changes
    ///
    /// Runtime grants took the deploy out of the loop, and with it the git
    /// history that used to record who changed what. This is that record.
    Audit {
        /// Show at most this many entries, most recent last (default 50)
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// Re-point a remote tool runner at the tool class that defines what it serves
    ///
    /// The runner's route list is derived from that class grant, so the routes
    /// and the model's grants cannot drift apart.
    SetRunner {
        /// Runner guest role, e.g. life-graph-runner
        runner: String,
        /// Tool class whose grant defines what this runner serves
        #[arg(long)]
        class: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// Replace the tools implied by a skill
    ///
    /// Writes the skill's `implied_tools` and marks the record runtime-owned, so
    /// the boot seeder stops refreshing it from the compiled-in catalog.
    SetSkill {
        /// Skill name, e.g. life.steward
        skill: String,
        /// Tool names implied by this skill (repeat the flag per tool)
        #[arg(long = "tool")]
        tools: Vec<String>,
        #[arg(long)]
        db: Option<PathBuf>,
    },
}

pub fn run(action: ToolsAction) -> Result<()> {
    match action {
        ToolsAction::Show { db } => show(db),
        ToolsAction::Disable { tool, db } => set_disabled(db, &tool, true),
        ToolsAction::Enable { tool, db } => set_disabled(db, &tool, false),
        ToolsAction::SetClass { class, tools, db } => set_class(db, &class, tools),
        ToolsAction::SetRunner { runner, class, db } => set_runner(db, &runner, &class),
        ToolsAction::SetSkill { skill, tools, db } => set_skill(db, &skill, tools),
        ToolsAction::Audit { limit, db } => audit(db, limit),
    }
}

fn audit(db: Option<PathBuf>, limit: usize) -> Result<()> {
    let graph = open_graph(db)?;
    let entries = graph.list_tool_grant_audits()?;
    if entries.is_empty() {
        println!("No tool grant changes recorded on this hotel.");
        return Ok(());
    }
    let skipped = entries.len().saturating_sub(limit);
    if skipped > 0 {
        println!("({skipped} older entries not shown; raise --limit to see them)");
    }
    println!("Tool grant audit trail (oldest first):");
    for entry in entries.iter().skip(skipped) {
        println!(
            "  #{} [{}] {} {} — {} → {}  (by {})",
            entry.sequence,
            entry.changed_at,
            entry.action,
            entry.target,
            entry.before,
            entry.after,
            entry.changed_by
        );
    }
    Ok(())
}

fn set_runner(db: Option<PathBuf>, runner: &str, class: &str) -> Result<()> {
    let runner_role = runner.trim().to_string();
    let tool_class = class.trim().to_string();
    if runner_role.is_empty() || tool_class.is_empty() {
        anyhow::bail!("runner role and tool class must not be empty");
    }
    update_registry(db, |registry| {
        let before = registry
            .runner_class(&runner_role)
            .unwrap_or("(unbound)")
            .to_string();
        let grant = ToolRunnerGrant {
            runner_role: runner_role.clone(),
            tool_class: tool_class.clone(),
            grant_source: GrantSource::Runtime,
        };
        match registry
            .runner_grants
            .iter_mut()
            .find(|existing| existing.runner_role == runner_role)
        {
            Some(existing) => *existing = grant,
            None => registry.runner_grants.push(grant),
        }
        println!(
            "Runner '{runner_role}' now serves whatever class '{tool_class}' grants. \
             Route changes apply when the hotel next seeds toolset profiles."
        );
        Ok(Some(GrantChange {
            action: "set_runner".into(),
            target: runner_role.clone(),
            before,
            after: tool_class.clone(),
        }))
    })
}

fn resolve_db(db: Option<PathBuf>) -> PathBuf {
    db.unwrap_or_else(|| match crate::init::active_profile() {
        Some(_) => crate::init::profile_dir().join("aiua_context.db"),
        None => PathBuf::from("aiua_context.db"),
    })
}

fn open_graph(db: Option<PathBuf>) -> Result<GraphDomain> {
    let db_path = resolve_db(db);
    let storage = SqliteGraphStorage::open(&db_path)
        .with_context(|| format!("open {}", db_path.display()))?;
    Ok(GraphDomain::new(Arc::new(storage.adapter())))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Who to attribute a grant change to.
///
/// Prefers the invoking OS user so the audit trail names a person rather than
/// the binary; falls back to the tool name when the environment is bare (cron,
/// a container with no `USER`).
fn actor() -> String {
    match std::env::var("PHILOTIC_GRANT_ACTOR")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
    {
        Some(user) => format!("phil tools ({user})"),
        None => "phil tools".to_string(),
    }
}

/// Loads the registry, applies `edit`, and writes it back with fresh provenance.
///
/// `edit` returns the audit description of what it changed, or `None` when the
/// call was a no-op. The audit entry is written BEFORE the registry mutation:
/// runtime grants removed the deploy from the loop, and with it the git history
/// that used to record who changed what — so a change that cannot be audited
/// must not land at all (fail closed).
fn update_registry(
    db: Option<PathBuf>,
    edit: impl FnOnce(&mut ToolGrantRegistryRecord) -> Result<Option<GrantChange>>,
) -> Result<()> {
    let graph = open_graph(db)?;
    let mut registry = graph.get_tool_grant_registry()?.unwrap_or_default();
    let Some(change) = edit(&mut registry)? else {
        return Ok(());
    };

    let audit = ToolGrantAuditRecord {
        audit_id: uuid::Uuid::new_v4().to_string(),
        action: change.action,
        target: change.target,
        before: change.before,
        after: change.after,
        changed_by: actor(),
        changed_at: now_secs(),
        // Assigned by record_tool_grant_audit, which owns trail ordering.
        sequence: 0,
    };
    graph
        .record_tool_grant_audit(&audit)
        .context("refusing to change tool grants: the audit entry could not be recorded")?;

    registry.updated_at = audit.changed_at;
    registry.updated_by = audit.changed_by.clone();
    graph.upsert_tool_grant_registry(&registry)?;
    Ok(())
}

/// One audited change to the grant registry.
struct GrantChange {
    action: String,
    target: String,
    before: String,
    after: String,
}

fn show(db: Option<PathBuf>) -> Result<()> {
    let graph = open_graph(db)?;
    let Some(registry) = graph.get_tool_grant_registry()? else {
        println!(
            "No tool grant registry in this hotel's context graph yet.\n\
             It is seeded on hotel boot; built-in grants are in effect until then."
        );
        return Ok(());
    };

    println!("Tool class grants:");
    if registry.class_grants.is_empty() {
        println!("  (none)");
    }
    for grant in &registry.class_grants {
        let source = match grant.grant_source {
            GrantSource::Seed => "seed",
            GrantSource::Runtime => "runtime",
        };
        if grant.tools.is_empty() {
            println!(
                "  {} [{}] — (empty: grants nothing)",
                grant.class_name, source
            );
        } else {
            println!(
                "  {} [{}] — {}",
                grant.class_name,
                source,
                grant.tools.join(", ")
            );
        }
    }

    println!("\nRemote runner routes (bound to a class grant, so routes cannot drift):");
    if registry.runner_grants.is_empty() {
        println!("  (none)");
    }
    for grant in &registry.runner_grants {
        let source = match grant.grant_source {
            GrantSource::Seed => "seed",
            GrantSource::Runtime => "runtime",
        };
        let served = registry
            .class_tools(&grant.tool_class)
            .map(|tools| {
                if tools.is_empty() {
                    "(empty)".to_string()
                } else {
                    tools.join(", ")
                }
            })
            .unwrap_or_else(|| "(class not in registry — built-in fallback)".to_string());
        println!(
            "  {} [{}] — class '{}' → {}",
            grant.runner_role, source, grant.tool_class, served
        );
    }

    println!("\nDisabled tools (policy, applied after every grant):");
    if registry.disabled_tools.is_empty() {
        println!("  (none)");
    }
    for tool in &registry.disabled_tools {
        println!("  {tool}");
    }

    if registry.updated_at > 0 {
        println!(
            "\nLast updated: {} by {}",
            registry.updated_at, registry.updated_by
        );
    }
    Ok(())
}

fn set_disabled(db: Option<PathBuf>, tool: &str, disable: bool) -> Result<()> {
    let tool_name = tool.trim().to_string();
    if tool_name.is_empty() {
        anyhow::bail!("tool name must not be empty");
    }
    update_registry(db, |registry| {
        let already = registry.is_disabled(&tool_name);
        if disable {
            if already {
                println!("'{tool_name}' is already disabled.");
                return Ok(None);
            }
            registry.disabled_tools.push(tool_name.clone());
            println!(
                "Disabled '{tool_name}'. Existing sessions keep their current toolset \
                 until their bindings are recomposed."
            );
            Ok(Some(GrantChange {
                action: "disable".into(),
                target: tool_name.clone(),
                before: "allowed".into(),
                after: "disabled".into(),
            }))
        } else {
            if !already {
                println!("'{tool_name}' is not disabled.");
                return Ok(None);
            }
            registry.disabled_tools.retain(|name| name != &tool_name);
            println!("Re-enabled '{tool_name}'.");
            Ok(Some(GrantChange {
                action: "enable".into(),
                target: tool_name.clone(),
                before: "disabled".into(),
                after: "allowed".into(),
            }))
        }
    })
}

fn set_class(db: Option<PathBuf>, class: &str, tools: Vec<String>) -> Result<()> {
    let class_name = class.trim().to_string();
    if class_name.is_empty() {
        anyhow::bail!("class name must not be empty");
    }
    update_registry(db, |registry| {
        let before = registry
            .class_tools(&class_name)
            .map(|tools| tools.join(", "))
            .unwrap_or_else(|| "(built-in fallback)".to_string());
        let grant = ToolClassGrant {
            class_name: class_name.clone(),
            tools: tools.clone(),
            grant_source: GrantSource::Runtime,
        };
        match registry
            .class_grants
            .iter_mut()
            .find(|existing| existing.class_name == class_name)
        {
            Some(existing) => *existing = grant,
            None => registry.class_grants.push(grant),
        }
        if tools.is_empty() {
            println!("Class '{class_name}' now grants nothing.");
        } else {
            println!("Class '{class_name}' now grants: {}", tools.join(", "));
        }
        Ok(Some(GrantChange {
            action: "set_class".into(),
            target: class_name.clone(),
            before,
            after: if tools.is_empty() {
                "(empty)".to_string()
            } else {
                tools.join(", ")
            },
        }))
    })
}

fn set_skill(db: Option<PathBuf>, skill: &str, tools: Vec<String>) -> Result<()> {
    let skill_name = skill.trim();
    if skill_name.is_empty() {
        anyhow::bail!("skill name must not be empty");
    }
    let graph = open_graph(db)?;
    let mut record = graph
        .get_abstract_skill(skill_name)?
        .ok_or_else(|| anyhow::anyhow!("skill '{skill_name}' not found in the context graph"))?;

    let before = record.implied_tools.join(", ");
    record.implied_tools = tools.clone();
    // Marks the grant runtime-owned so the boot seeder preserves it instead of
    // refreshing it from the compiled-in catalog on the next restart.
    record.grant_source = GrantSource::Runtime;

    // Audit first, then mutate — same fail-closed order as the registry path.
    graph
        .record_tool_grant_audit(&ToolGrantAuditRecord {
            audit_id: uuid::Uuid::new_v4().to_string(),
            action: "set_skill".into(),
            target: skill_name.to_string(),
            before,
            after: if tools.is_empty() {
                "(none)".to_string()
            } else {
                tools.join(", ")
            },
            changed_by: actor(),
            changed_at: now_secs(),
            // Assigned by record_tool_grant_audit, which owns trail ordering.
            sequence: 0,
        })
        .context("refusing to change skill grants: the audit entry could not be recorded")?;
    graph.upsert_abstract_skill(&record)?;

    if tools.is_empty() {
        println!("Skill '{skill_name}' now implies no tools.");
    } else {
        println!("Skill '{skill_name}' now implies: {}", tools.join(", "));
    }
    Ok(())
}
