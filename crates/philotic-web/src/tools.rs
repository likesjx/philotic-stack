//! `phil tools` — operator surface for the per-hotel tool grant registry.
//!
//! Implements the runtime-edit half of `proposal:data-driven-tool-grants-skilldag`
//! slice 1: tool grants and tool policy are data in the local hotel context graph,
//! so enabling, disabling, or re-granting a tool is a DB write plus a session
//! refresh rather than a code change and a deploy.
//!
//! Deliberately an operator CLI, not an agent-facing tool. Exposing grant
//! mutation to models is a governance question (who may widen their own reach,
//! under what audit) that the proposal carves out as a later slice; adding the
//! tool here would also mean granting it through the very tables this slice is
//! demoting.
//!
//! Every write stamps [`GrantSource::Runtime`], which is what stops the boot
//! seeder from reverting the edit on the next hotel restart.

use std::path::PathBuf;
use std::sync::Arc;

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::graph::{GrantSource, ToolClassGrant, ToolGrantRegistryRecord};
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
        ToolsAction::SetSkill { skill, tools, db } => set_skill(db, &skill, tools),
    }
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

/// Loads the registry, applies `edit`, and writes it back with fresh provenance.
fn update_registry(
    db: Option<PathBuf>,
    edit: impl FnOnce(&mut ToolGrantRegistryRecord) -> Result<bool>,
) -> Result<()> {
    let graph = open_graph(db)?;
    let mut registry = graph.get_tool_grant_registry()?.unwrap_or_default();
    if !edit(&mut registry)? {
        return Ok(());
    }
    registry.updated_at = now_secs();
    registry.updated_by = "phil tools".to_string();
    graph.upsert_tool_grant_registry(&registry)?;
    Ok(())
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
                return Ok(false);
            }
            registry.disabled_tools.push(tool_name.clone());
            println!(
                "Disabled '{tool_name}'. Existing sessions keep their current toolset \
                 until their bindings are recomposed."
            );
        } else {
            if !already {
                println!("'{tool_name}' is not disabled.");
                return Ok(false);
            }
            registry.disabled_tools.retain(|name| name != &tool_name);
            println!("Re-enabled '{tool_name}'.");
        }
        Ok(true)
    })
}

fn set_class(db: Option<PathBuf>, class: &str, tools: Vec<String>) -> Result<()> {
    let class_name = class.trim().to_string();
    if class_name.is_empty() {
        anyhow::bail!("class name must not be empty");
    }
    update_registry(db, |registry| {
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
        Ok(true)
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

    record.implied_tools = tools.clone();
    // Marks the grant runtime-owned so the boot seeder preserves it instead of
    // refreshing it from the compiled-in catalog on the next restart.
    record.grant_source = GrantSource::Runtime;
    graph.upsert_abstract_skill(&record)?;

    if tools.is_empty() {
        println!("Skill '{skill_name}' now implies no tools.");
    } else {
        println!("Skill '{skill_name}' now implies: {}", tools.join(", "));
    }
    Ok(())
}
