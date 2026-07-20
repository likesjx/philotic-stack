use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use graph_intelligence::schema::{Edge, EdgeRelation, Mutation, Node, NodeKind};
use graph_intelligence::GraphEngine;
use philotic_graph::PhiloticGraphConfig;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

const HARNESS_UPDATE_RETRIES: usize = 3;
const MUNINN_HARNESS_INSTRUCTIONS: &str = "\
- For meaningful work, run the Muninn bootstrap/orientation before decisions, resumed work, or implementation.
- Prefer `just session-start`; otherwise run `python3 scripts/muninn_mcp.py bootstrap`, then retrieve self/user/topic context with the Muninn triad.
- If Muninn cannot bootstrap, stop and get explicit operator approval before continuing on repo/runtime truth only.
- At closeout, write only the durable Muninn memory delta: decisions, reality gaps, validation outcomes, next seams, or operator preferences.
- Do not store transcripts, routine task churn, noisy logs, or proposal/docs summaries already committed in the repo.
";

#[derive(clap::Subcommand, Debug)]
pub enum HarnessAction {
    /// List managed harnesses from intel-graph.
    List,

    /// Seed the canonical shared harness model used across Codex, Claude Code, and other harnesses.
    Bootstrap,

    /// Manage harness skill catalog entries and assignments in intel-graph.
    Skills {
        #[command(subcommand)]
        action: HarnessSkillAction,
    },

    /// Manage named harness profiles or bundles in intel-graph.
    Profiles {
        #[command(subcommand)]
        action: HarnessProfileAction,
    },

    /// Manage canonical shared agent profile definitions.
    CanonicalProfiles {
        #[command(subcommand)]
        action: CanonicalProfileAction,
    },

    /// Manage canonical shared workflow definitions.
    Workflows {
        #[command(subcommand)]
        action: CanonicalWorkflowAction,
    },

    /// Manage measured harness trial sessions.
    Trials {
        #[command(subcommand)]
        action: HarnessTrialAction,
    },

    /// Show desired/rendered/observed state for one harness.
    Status { harness_id: String },

    /// Plan local harness changes without writing anything.
    Plan {
        harness_id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        bundle: Option<String>,
    },

    /// Apply desired state and render the local harness projection.
    Apply {
        harness_id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        bundle: Option<String>,
    },

    /// Re-render local harness files from desired state.
    Export { harness_id: String },

    /// Refresh observed state from the local harness projection.
    Verify { harness_id: String },

    /// Report current drift.
    Drift { harness_id: Option<String> },
}

#[derive(clap::Subcommand, Debug)]
pub enum HarnessSkillAction {
    /// List registered harness skills.
    List,

    /// Register or update a harness skill.
    Register {
        skill_name: String,
        #[arg(long)]
        description: Option<String>,
    },

    /// Sync the catalog from the repository's skills/ directory (skills/*/SKILL.md).
    Sync {
        /// Directory containing skill subdirectories (default: <cwd>/skills)
        #[arg(long)]
        dir: Option<PathBuf>,
    },

    /// Assign a registered skill to a harness desired state.
    Assign {
        harness_id: String,
        skill_name: String,
    },

    /// Remove a skill assignment from a harness desired state.
    Unassign {
        harness_id: String,
        skill_name: String,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum HarnessProfileAction {
    /// List registered harness profiles.
    List,

    /// Register or update a harness profile with comma-separated skills.
    Register {
        profile_name: String,
        #[arg(long)]
        skills: String,
        #[arg(long)]
        description: Option<String>,
    },

    /// Assign a registered profile to a harness desired state.
    Assign {
        harness_id: String,
        profile_name: String,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum CanonicalProfileAction {
    /// List canonical shared profiles.
    List,

    /// Register or update a canonical profile with role and comma-separated skills.
    Register {
        profile_name: String,
        #[arg(long)]
        role: String,
        #[arg(long)]
        skills: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        runtimes: Option<String>,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum CanonicalWorkflowAction {
    /// List canonical shared workflows.
    List,

    /// Register or update a canonical workflow with comma-separated phase roles.
    Register {
        workflow_name: String,
        #[arg(long)]
        phases: String,
        #[arg(long)]
        description: Option<String>,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum HarnessTrialAction {
    /// Start a measured harness trial and optionally apply a canonical profile first.
    Start {
        harness_id: String,
        seam_id: String,
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        workflow: Option<String>,
        #[arg(long)]
        proposal_id: Option<String>,
        #[arg(long)]
        task_id: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        agent_model: Option<String>,
        #[arg(long)]
        phase: Option<String>,
    },

    /// Report measured trial activity, including token and elapsed usage.
    Report {
        session_id: String,
        activity_type: String,
        #[arg(long)]
        phase: Option<String>,
        #[arg(long)]
        tokens_input: Option<i64>,
        #[arg(long)]
        tokens_output: Option<i64>,
        #[arg(long)]
        elapsed_ms: Option<i64>,
        #[arg(long)]
        lines_changed: Option<i64>,
        #[arg(long, value_delimiter = ',')]
        files: Vec<String>,
        #[arg(long)]
        note: Option<String>,
    },

    /// Close a measured harness trial and finalize elapsed/token totals.
    Close {
        session_id: String,
        #[arg(long, default_value = "completed")]
        status: String,
        #[arg(long)]
        verified: Option<String>,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        elapsed_ms: Option<i64>,
        #[arg(long)]
        tokens_total: Option<i64>,
        #[arg(long)]
        quota_remaining: Option<i64>,
    },
}

/// Verification contract for an auxiliary file written by `apply` beyond the
/// main projection. `Hash` is for files the harness exclusively owns;
/// `Contains` is for co-owned files (e.g. settings.local.json, ~/.codex/AGENTS.md)
/// where other writers legitimately change unrelated content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case")]
enum AuxCheck {
    Hash { path: String, hash: String },
    Contains { path: String, needle: String },
}

impl AuxCheck {
    fn path(&self) -> &str {
        match self {
            AuxCheck::Hash { path, .. } => path,
            AuxCheck::Contains { path, .. } => path,
        }
    }

    /// Returns None when the check passes, or a short failure description.
    fn evaluate(&self) -> Option<String> {
        match self {
            AuxCheck::Hash { path, hash } => match fs::read(path) {
                Ok(content) if sha256_hex(&content) == *hash => None,
                Ok(_) => Some(format!("{path}: content hash mismatch")),
                Err(_) => Some(format!("{path}: missing or unreadable")),
            },
            AuxCheck::Contains { path, needle } => match fs::read_to_string(path) {
                Ok(content) if content.contains(needle.as_str()) => None,
                Ok(_) => Some(format!("{path}: managed marker/hook missing")),
                Err(_) => Some(format!("{path}: missing or unreadable")),
            },
        }
    }
}

#[derive(Debug, Clone)]
struct HarnessConfigSnapshot {
    profile: String,
    runtime_kind: String,
    skills: Vec<String>,
    target_path: PathBuf,
    content: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
struct GitSessionContext {
    branch: Option<String>,
    worktree_path: Option<String>,
}

trait HarnessAdapter {
    fn runtime_kind(&self) -> &'static str;
    fn plan(&self, harness_id: &str, profile: Option<String>) -> Result<HarnessConfigSnapshot>;
    fn config_from_harness(
        &self,
        harness_id: &str,
        harness: &Node,
    ) -> Result<HarnessConfigSnapshot>;
}

struct CodexAdapter;
struct ClaudeCodeAdapter;
struct WindsurfAdapter;
struct AntigravityAdapter;

impl HarnessAdapter for CodexAdapter {
    fn runtime_kind(&self) -> &'static str {
        "codex"
    }

    fn plan(&self, harness_id: &str, profile: Option<String>) -> Result<HarnessConfigSnapshot> {
        let profile = profile.unwrap_or_else(|| "orchestrator".into());
        let target_path = codex_target_path(harness_id)?;
        let skills = default_skills_for_profile(&profile);
        let content = render_codex_markdown(harness_id, &profile, &skills).into_bytes();
        Ok(HarnessConfigSnapshot {
            profile,
            runtime_kind: self.runtime_kind().into(),
            skills,
            target_path,
            content,
        })
    }

    fn config_from_harness(
        &self,
        harness_id: &str,
        harness: &Node,
    ) -> Result<HarnessConfigSnapshot> {
        let profile = harness
            .properties
            .get("desired")
            .and_then(|v| v.get("role_charter"))
            .and_then(|v| v.as_str())
            .unwrap_or("orchestrator")
            .to_string();
        let mut snapshot = self.plan(harness_id, Some(profile))?;
        if let Some(skill_refs) = harness_skill_refs(harness) {
            snapshot.skills = skill_refs.clone();
            snapshot.content =
                render_codex_markdown(harness_id, &snapshot.profile, &skill_refs).into_bytes();
        }
        Ok(snapshot)
    }
}

impl HarnessAdapter for ClaudeCodeAdapter {
    fn runtime_kind(&self) -> &'static str {
        "claude-code"
    }

    fn plan(&self, harness_id: &str, profile: Option<String>) -> Result<HarnessConfigSnapshot> {
        let profile = profile.unwrap_or_else(|| "orchestrator".into());
        let target_path = claude_code_target_path(harness_id)?;
        let skills = default_skills_for_profile(&profile);
        let content = render_claude_code_markdown(harness_id, &profile, &skills).into_bytes();
        Ok(HarnessConfigSnapshot {
            profile,
            runtime_kind: self.runtime_kind().into(),
            skills,
            target_path,
            content,
        })
    }

    fn config_from_harness(
        &self,
        harness_id: &str,
        harness: &Node,
    ) -> Result<HarnessConfigSnapshot> {
        let profile = harness
            .properties
            .get("desired")
            .and_then(|v| v.get("role_charter"))
            .and_then(|v| v.as_str())
            .unwrap_or("orchestrator")
            .to_string();
        let mut snapshot = self.plan(harness_id, Some(profile))?;
        if let Some(skill_refs) = harness_skill_refs(harness) {
            snapshot.skills = skill_refs.clone();
            snapshot.content =
                render_claude_code_markdown(harness_id, &snapshot.profile, &skill_refs)
                    .into_bytes();
        }
        Ok(snapshot)
    }
}

impl HarnessAdapter for AntigravityAdapter {
    fn runtime_kind(&self) -> &'static str {
        "antigravity"
    }

    fn plan(&self, harness_id: &str, profile: Option<String>) -> Result<HarnessConfigSnapshot> {
        let profile = profile.unwrap_or_else(|| "orchestrator".into());
        let target_path = antigravity_target_path(harness_id)?;
        let skills = default_skills_for_profile(&profile);
        let content = render_antigravity_markdown(harness_id, &profile, &skills).into_bytes();
        Ok(HarnessConfigSnapshot {
            profile,
            runtime_kind: self.runtime_kind().into(),
            skills,
            target_path,
            content,
        })
    }

    fn config_from_harness(
        &self,
        harness_id: &str,
        harness: &Node,
    ) -> Result<HarnessConfigSnapshot> {
        let profile = harness
            .properties
            .get("desired")
            .and_then(|v| v.get("role_charter"))
            .and_then(|v| v.as_str())
            .unwrap_or("orchestrator")
            .to_string();
        let mut snapshot = self.plan(harness_id, Some(profile))?;
        if let Some(skill_refs) = harness_skill_refs(harness) {
            snapshot.skills = skill_refs.clone();
            snapshot.content =
                render_antigravity_markdown(harness_id, &snapshot.profile, &skill_refs)
                    .into_bytes();
        }
        Ok(snapshot)
    }
}

impl HarnessAdapter for WindsurfAdapter {
    fn runtime_kind(&self) -> &'static str {
        "windsurf"
    }

    fn plan(&self, harness_id: &str, profile: Option<String>) -> Result<HarnessConfigSnapshot> {
        let profile = profile.unwrap_or_else(|| "orchestrator".into());
        let target_path = windsurf_target_path(harness_id)?;
        let skills = default_skills_for_profile(&profile);
        let content = render_windsurf_rule_markdown(harness_id, &profile, &skills).into_bytes();
        Ok(HarnessConfigSnapshot {
            profile,
            runtime_kind: self.runtime_kind().into(),
            skills,
            target_path,
            content,
        })
    }

    fn config_from_harness(
        &self,
        harness_id: &str,
        harness: &Node,
    ) -> Result<HarnessConfigSnapshot> {
        let profile = harness
            .properties
            .get("desired")
            .and_then(|v| v.get("role_charter"))
            .and_then(|v| v.as_str())
            .unwrap_or("orchestrator")
            .to_string();
        let mut snapshot = self.plan(harness_id, Some(profile))?;
        if let Some(skill_refs) = harness_skill_refs(harness) {
            snapshot.skills = skill_refs.clone();
            snapshot.content =
                render_windsurf_rule_markdown(harness_id, &snapshot.profile, &skill_refs)
                    .into_bytes();
        }
        Ok(snapshot)
    }
}

pub fn run(action: HarnessAction) -> Result<()> {
    let config = PhiloticGraphConfig::default();
    let engine = GraphEngine::open(&config.db_path)?;
    match action {
        HarnessAction::List => list_harnesses(&engine),
        HarnessAction::Bootstrap => bootstrap_canonical_model(&engine),
        HarnessAction::Skills { action } => run_skill_action(&engine, action),
        HarnessAction::Profiles { action } => run_profile_action(&engine, action),
        HarnessAction::CanonicalProfiles { action } => {
            run_canonical_profile_action(&engine, action)
        }
        HarnessAction::Workflows { action } => run_workflow_action(&engine, action),
        HarnessAction::Trials { action } => run_trial_action(&engine, action),
        HarnessAction::Status { harness_id } => show_status(&engine, &harness_node_id(&harness_id)),
        HarnessAction::Plan {
            harness_id,
            profile,
            bundle,
        } => with_adapter(&engine, &harness_id, |adapter| {
            plan_harness(&engine, adapter, &harness_id, profile, bundle)
        }),
        HarnessAction::Apply {
            harness_id,
            profile,
            bundle,
        } => with_adapter(&engine, &harness_id, |adapter| {
            apply_harness(&engine, adapter, &harness_id, profile, bundle)
        }),
        HarnessAction::Export { harness_id } => with_adapter(&engine, &harness_id, |adapter| {
            export_harness(&engine, adapter, &harness_id)
        }),
        HarnessAction::Verify { harness_id } => with_adapter(&engine, &harness_id, |adapter| {
            verify_harness(&engine, adapter, &harness_id)
        }),
        HarnessAction::Drift { harness_id } => report_drift(&engine, harness_id.as_deref()),
    }
}

fn list_harnesses(engine: &GraphEngine) -> Result<()> {
    let nodes = engine.query_nodes(Some(NodeKind::Harness), None)?;
    if nodes.is_empty() {
        println!("No managed harnesses found.");
        return Ok(());
    }

    println!(
        "{:<24} {:<12} {:<14} {}",
        "HARNESS", "RUNTIME", "DRIFT", "PROFILE"
    );
    println!("{}", "─".repeat(70));
    for node in nodes {
        let runtime = prop_str(&node, &["runtime_kind"]).unwrap_or("?");
        let drift = prop_str(&node, &["observed_summary", "drift_status"]).unwrap_or("unknown");
        let profile = prop_str(&node, &["desired", "role_charter"]).unwrap_or("?");
        println!("{:<24} {:<12} {:<14} {}", node.id, runtime, drift, profile);
    }
    Ok(())
}

fn run_skill_action(engine: &GraphEngine, action: HarnessSkillAction) -> Result<()> {
    match action {
        HarnessSkillAction::List => list_harness_skills(engine),
        HarnessSkillAction::Register {
            skill_name,
            description,
        } => register_harness_skill(engine, &skill_name, description.as_deref()),
        HarnessSkillAction::Sync { dir } => sync_harness_skills(engine, dir.as_deref()),
        HarnessSkillAction::Assign {
            harness_id,
            skill_name,
        } => assign_harness_skill(engine, &harness_id, &skill_name),
        HarnessSkillAction::Unassign {
            harness_id,
            skill_name,
        } => unassign_harness_skill(engine, &harness_id, &skill_name),
    }
}

fn run_profile_action(engine: &GraphEngine, action: HarnessProfileAction) -> Result<()> {
    match action {
        HarnessProfileAction::List => list_harness_profiles(engine),
        HarnessProfileAction::Register {
            profile_name,
            skills,
            description,
        } => register_harness_profile(engine, &profile_name, &skills, description.as_deref()),
        HarnessProfileAction::Assign {
            harness_id,
            profile_name,
        } => assign_harness_profile(engine, &harness_id, &profile_name),
    }
}

fn run_canonical_profile_action(
    engine: &GraphEngine,
    action: CanonicalProfileAction,
) -> Result<()> {
    match action {
        CanonicalProfileAction::List => list_canonical_profiles(engine),
        CanonicalProfileAction::Register {
            profile_name,
            role,
            skills,
            description,
            runtimes,
        } => register_canonical_profile(
            engine,
            &profile_name,
            &role,
            &skills,
            description.as_deref(),
            runtimes.as_deref(),
        ),
    }
}

fn run_workflow_action(engine: &GraphEngine, action: CanonicalWorkflowAction) -> Result<()> {
    match action {
        CanonicalWorkflowAction::List => list_canonical_workflows(engine),
        CanonicalWorkflowAction::Register {
            workflow_name,
            phases,
            description,
        } => register_canonical_workflow(engine, &workflow_name, &phases, description.as_deref()),
    }
}

fn run_trial_action(engine: &GraphEngine, action: HarnessTrialAction) -> Result<()> {
    match action {
        HarnessTrialAction::Start {
            harness_id,
            seam_id,
            session_id,
            profile,
            workflow,
            proposal_id,
            task_id,
            agent,
            agent_model,
            phase,
        } => start_harness_trial(
            engine,
            &harness_id,
            &seam_id,
            session_id.as_deref(),
            profile.as_deref(),
            workflow.as_deref(),
            proposal_id.as_deref(),
            task_id.as_deref(),
            agent.as_deref(),
            agent_model.as_deref(),
            phase.as_deref(),
        ),
        HarnessTrialAction::Report {
            session_id,
            activity_type,
            phase,
            tokens_input,
            tokens_output,
            elapsed_ms,
            lines_changed,
            files,
            note,
        } => report_harness_trial_activity(
            engine,
            &session_id,
            &activity_type,
            phase.as_deref(),
            tokens_input,
            tokens_output,
            elapsed_ms,
            lines_changed,
            &files,
            note.as_deref(),
        ),
        HarnessTrialAction::Close {
            session_id,
            status,
            verified,
            summary,
            elapsed_ms,
            tokens_total,
            quota_remaining,
        } => close_harness_trial(
            engine,
            &session_id,
            &status,
            verified.as_deref(),
            summary.as_deref(),
            elapsed_ms,
            tokens_total,
            quota_remaining,
        ),
    }
}

fn list_harness_skills(engine: &GraphEngine) -> Result<()> {
    let mut nodes = engine.query_nodes(Some(NodeKind::HarnessSkill), None)?;
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    if nodes.is_empty() {
        println!("No harness skills registered.");
        return Ok(());
    }

    println!("{:<24} {}", "SKILL", "DESCRIPTION");
    println!("{}", "─".repeat(70));
    for node in nodes {
        println!(
            "{:<24} {}",
            node.name,
            prop_str(&node, &["description"]).unwrap_or("-")
        );
    }
    Ok(())
}

fn register_harness_skill(
    engine: &GraphEngine,
    skill_name: &str,
    description: Option<&str>,
) -> Result<()> {
    let now = Utc::now();
    let node = Node {
        id: harness_skill_node_id(skill_name),
        kind: NodeKind::HarnessSkill,
        name: skill_name.to_string(),
        properties: json!({
            "description": description.unwrap_or(""),
            "status": "active"
        }),
        file_path: None,
        worktree: String::new(),
        created_at: now,
        updated_at: now,
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&node)?;
    record_mutation(
        engine,
        "harness_skill_register",
        Some(node.id.clone()),
        None,
        Some("active".into()),
        Some(format!("Registered harness skill {}", skill_name)),
        json!({ "description": description.unwrap_or("") }),
    )?;
    println!("Registered harness skill {}", skill_name);
    Ok(())
}

/// Extract a scalar value from simple `key: value` YAML frontmatter delimited by `---` lines.
fn frontmatter_scalar(content: &str, key: &str) -> Option<String> {
    let mut in_frontmatter = false;
    for line in content.lines() {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            if in_frontmatter {
                break;
            }
            in_frontmatter = true;
            continue;
        }
        if !in_frontmatter {
            // No frontmatter at all
            break;
        }
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Upsert one catalog entry per skills/<name>/SKILL.md, keeping the graph's
/// harness skill catalog in lockstep with the on-disk skill corpus. Catalog
/// entries with no on-disk directory are reported (canonical bundles from
/// `bootstrap` are expected to appear there).
fn sync_harness_skills(engine: &GraphEngine, dir: Option<&Path>) -> Result<()> {
    let root = match dir {
        Some(d) => d.to_path_buf(),
        None => std::env::current_dir()
            .context("failed to locate current workspace")?
            .join("skills"),
    };
    if !root.is_dir() {
        bail!("skills directory not found: {}", root.display());
    }

    let mut on_disk = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(&root)
        .with_context(|| format!("failed to read {}", root.display()))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let skill_md = entry.path().join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let content = fs::read_to_string(&skill_md)
            .with_context(|| format!("failed to read {}", skill_md.display()))?;
        let name = frontmatter_scalar(&content, "name").unwrap_or_else(|| dir_name.clone());
        let description = frontmatter_scalar(&content, "description").unwrap_or_default();
        // Keep the description compact for the catalog table.
        let description: String = description.chars().take(200).collect();

        let now = Utc::now();
        let node_id = harness_skill_node_id(&name);
        let existing = engine.get_node(&node_id)?;
        let changed = existing
            .as_ref()
            .map(|node| {
                node.properties
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|d| d != description)
                    .unwrap_or(true)
            })
            .unwrap_or(true);
        let node = Node {
            id: node_id.clone(),
            kind: NodeKind::HarnessSkill,
            name: name.clone(),
            properties: json!({
                "description": description,
                "status": "active",
                "source_path": skill_md.to_string_lossy().to_string(),
                "synced_at": now.to_rfc3339()
            }),
            file_path: Some(skill_md.to_string_lossy().to_string()),
            worktree: String::new(),
            created_at: existing.as_ref().map(|n| n.created_at).unwrap_or(now),
            updated_at: now,
            embedding: None,
            embedding_model: None,
            embedding_dims: None,
            embedding_updated: None,
            embedding_hash: None,
        };
        engine.upsert_node(&node)?;
        let verb = match (&existing, changed) {
            (None, _) => "added",
            (Some(_), true) => "updated",
            (Some(_), false) => "unchanged",
        };
        println!("  {verb}: {name}");
        on_disk.push(name);
    }

    let catalog = engine.query_nodes(Some(NodeKind::HarnessSkill), None)?;
    let orphans: Vec<&str> = catalog
        .iter()
        .filter(|node| !on_disk.contains(&node.name))
        .map(|node| node.name.as_str())
        .collect();

    record_mutation(
        engine,
        "harness_skill_sync",
        None,
        None,
        None,
        Some(format!(
            "Synced {} skills from {}",
            on_disk.len(),
            root.display()
        )),
        json!({ "synced": on_disk, "catalog_only": orphans }),
    )?;

    println!(
        "Synced {} skills from {} into the harness catalog.",
        on_disk.len(),
        root.display()
    );
    if !orphans.is_empty() {
        println!(
            "Catalog entries with no on-disk skill (bundles or stale): {}",
            orphans.join(", ")
        );
    }
    Ok(())
}

fn list_harness_profiles(engine: &GraphEngine) -> Result<()> {
    let mut nodes = engine.query_nodes(Some(NodeKind::HarnessProfile), None)?;
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    if nodes.is_empty() {
        println!("No harness profiles registered.");
        return Ok(());
    }
    println!("{:<24} {:<28} {}", "PROFILE", "SKILLS", "DESCRIPTION");
    println!("{}", "─".repeat(96));
    for node in nodes {
        let skills = join_str_array(node.properties.get("skills")).unwrap_or_else(|| "-".into());
        println!(
            "{:<24} {:<28} {}",
            node.name,
            skills,
            prop_str(&node, &["description"]).unwrap_or("-")
        );
    }
    Ok(())
}

fn register_harness_profile(
    engine: &GraphEngine,
    profile_name: &str,
    skills_csv: &str,
    description: Option<&str>,
) -> Result<()> {
    let skills = parse_csv_list(skills_csv);
    if skills.is_empty() {
        bail!("Harness profile must include at least one skill");
    }
    for skill in &skills {
        ensure_harness_skill_exists(engine, skill)?;
    }
    let now = Utc::now();
    let node = Node {
        id: harness_profile_node_id(profile_name),
        kind: NodeKind::HarnessProfile,
        name: profile_name.to_string(),
        properties: json!({
            "skills": skills,
            "description": description.unwrap_or(""),
            "status": "active"
        }),
        file_path: None,
        worktree: String::new(),
        created_at: now,
        updated_at: now,
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&node)?;
    record_mutation(
        engine,
        "harness_profile_register",
        Some(node.id.clone()),
        None,
        Some("active".into()),
        Some(format!("Registered harness profile {}", profile_name)),
        json!({ "skills": node.properties["skills"].clone() }),
    )?;
    println!("Registered harness profile {}", profile_name);
    Ok(())
}

fn bootstrap_canonical_model(engine: &GraphEngine) -> Result<()> {
    for (skill_name, description) in [
        ("planning", "Planning and orchestration skill bundle"),
        ("implementation", "Implementation and coding skill bundle"),
        ("review", "Code review skill bundle"),
        ("verification", "Verification and validation skill bundle"),
        (
            "muninn-memory-habit",
            "Muninn retrieval and write-back habit for continuity",
        ),
    ] {
        if engine
            .get_node(&harness_skill_node_id(skill_name))?
            .is_none()
        {
            register_harness_skill(engine, skill_name, Some(description))?;
        }
    }

    register_canonical_profile(
        engine,
        "orchestrator",
        "orchestrator",
        "planning,verification,muninn-memory-habit",
        Some("Shared orchestrator profile across coding harnesses"),
        Some("codex,claude-code,windsurf"),
    )?;
    register_canonical_profile(
        engine,
        "implementer",
        "implementer",
        "implementation,muninn-memory-habit",
        Some("Shared implementer profile across coding harnesses"),
        Some("codex,claude-code,windsurf"),
    )?;
    register_canonical_profile(
        engine,
        "reviewer",
        "reviewer",
        "review,verification,muninn-memory-habit",
        Some("Shared reviewer profile across coding harnesses"),
        Some("codex,claude-code,windsurf"),
    )?;
    register_canonical_profile(
        engine,
        "verifier",
        "verifier",
        "verification,muninn-memory-habit",
        Some("Shared verifier profile across coding harnesses"),
        Some("codex,claude-code,windsurf"),
    )?;

    register_canonical_workflow(
        engine,
        "implement-review-verify",
        "implementer,reviewer,verifier",
        Some("Shared implementation workflow for small to medium coding tasks"),
    )?;
    register_canonical_workflow(
        engine,
        "orchestrator-worker",
        "orchestrator,implementer,reviewer,verifier",
        Some("Shared orchestrator-worker workflow for larger coding tasks"),
    )?;

    println!("Bootstrapped canonical harness model.");
    Ok(())
}

fn list_canonical_profiles(engine: &GraphEngine) -> Result<()> {
    let mut nodes = engine.query_nodes(Some(NodeKind::CanonicalProfile), None)?;
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    if nodes.is_empty() {
        println!("No canonical profiles registered.");
        return Ok(());
    }
    println!(
        "{:<18} {:<16} {:<28} {}",
        "PROFILE", "ROLE", "SKILLS", "RUNTIMES"
    );
    println!("{}", "─".repeat(96));
    for node in nodes {
        let skills =
            join_str_array(node.properties.get("skill_refs")).unwrap_or_else(|| "-".into());
        let runtimes =
            join_str_array(node.properties.get("supported_runtimes")).unwrap_or_else(|| "-".into());
        println!(
            "{:<18} {:<16} {:<28} {}",
            node.name,
            prop_str(&node, &["role_charter"]).unwrap_or("-"),
            skills,
            runtimes,
        );
    }
    Ok(())
}

fn register_canonical_profile(
    engine: &GraphEngine,
    profile_name: &str,
    role: &str,
    skills_csv: &str,
    description: Option<&str>,
    runtimes_csv: Option<&str>,
) -> Result<()> {
    let skill_refs = parse_csv_list(skills_csv);
    if skill_refs.is_empty() {
        bail!("Canonical profile must include at least one skill");
    }
    for skill in &skill_refs {
        ensure_harness_skill_exists(engine, skill)?;
    }
    let supported_runtimes = parse_csv_list(runtimes_csv.unwrap_or("codex,claude-code,windsurf"));
    let now = Utc::now();
    let node = Node {
        id: canonical_profile_node_id(profile_name),
        kind: NodeKind::CanonicalProfile,
        name: profile_name.to_string(),
        properties: json!({
            "role_charter": role,
            "skill_refs": skill_refs,
            "description": description.unwrap_or(""),
            "supported_runtimes": supported_runtimes,
            "status": "active"
        }),
        file_path: None,
        worktree: String::new(),
        created_at: now,
        updated_at: now,
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&node)?;
    record_mutation(
        engine,
        "canonical_profile_register",
        Some(node.id.clone()),
        None,
        Some("active".into()),
        Some(format!("Registered canonical profile {}", profile_name)),
        json!({
            "role_charter": role,
            "skill_refs": node.properties["skill_refs"].clone(),
            "supported_runtimes": node.properties["supported_runtimes"].clone()
        }),
    )?;
    println!("Registered canonical profile {}", profile_name);
    Ok(())
}

fn list_canonical_workflows(engine: &GraphEngine) -> Result<()> {
    let mut nodes = engine.query_nodes(Some(NodeKind::CanonicalWorkflow), None)?;
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    if nodes.is_empty() {
        println!("No canonical workflows registered.");
        return Ok(());
    }
    println!("{:<28} {:<36} {}", "WORKFLOW", "PHASES", "DESCRIPTION");
    println!("{}", "─".repeat(112));
    for node in nodes {
        let phases =
            join_str_array(node.properties.get("phase_roles")).unwrap_or_else(|| "-".into());
        println!(
            "{:<28} {:<36} {}",
            node.name,
            phases,
            prop_str(&node, &["description"]).unwrap_or("-"),
        );
    }
    Ok(())
}

fn register_canonical_workflow(
    engine: &GraphEngine,
    workflow_name: &str,
    phases_csv: &str,
    description: Option<&str>,
) -> Result<()> {
    let phase_roles = parse_csv_list(phases_csv);
    if phase_roles.is_empty() {
        bail!("Canonical workflow must include at least one phase role");
    }
    let now = Utc::now();
    let node = Node {
        id: canonical_workflow_node_id(workflow_name),
        kind: NodeKind::CanonicalWorkflow,
        name: workflow_name.to_string(),
        properties: json!({
            "phase_roles": phase_roles,
            "description": description.unwrap_or(""),
            "status": "active"
        }),
        file_path: None,
        worktree: String::new(),
        created_at: now,
        updated_at: now,
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&node)?;
    record_mutation(
        engine,
        "canonical_workflow_register",
        Some(node.id.clone()),
        None,
        Some("active".into()),
        Some(format!("Registered canonical workflow {}", workflow_name)),
        json!({ "phase_roles": node.properties["phase_roles"].clone() }),
    )?;
    println!("Registered canonical workflow {}", workflow_name);
    Ok(())
}

fn assign_harness_profile(
    engine: &GraphEngine,
    harness_id: &str,
    profile_name: &str,
) -> Result<()> {
    let profile_node_id = harness_profile_node_id(profile_name);
    let Some(profile) = engine.get_node(&profile_node_id)? else {
        bail!("Harness profile not found: {}", profile_name);
    };
    let skills = profile
        .properties
        .get("skills")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let harness_node_id = harness_node_id(harness_id);
    let harness = update_harness_node_with_retry(
        engine,
        &harness_node_id,
        harness_id,
        |properties, _now| {
            ensure_desired_object(properties);
            properties["desired"]["profile_name"] = json!(profile_name);
            properties["desired"]["skill_refs"] = json!(skills);
        },
    )?;
    engine.upsert_edge(&Edge {
        source_id: harness.id.clone(),
        target_id: profile_node_id.clone(),
        relation: EdgeRelation::References,
        properties: json!({}),
        worktree: String::new(),
    })?;
    record_mutation(
        engine,
        "harness_profile_assign",
        Some(harness.id.clone()),
        None,
        Some(profile_name.into()),
        Some(format!(
            "Assigned harness profile {} to {}",
            profile_name, harness_id
        )),
        json!({ "profile_id": profile_node_id }),
    )?;
    println!(
        "Assigned harness profile {} to {}",
        profile_name, harness_id
    );
    Ok(())
}

fn assign_harness_skill(engine: &GraphEngine, harness_id: &str, skill_name: &str) -> Result<()> {
    ensure_harness_skill_exists(engine, skill_name)?;
    let harness_node_id = harness_node_id(harness_id);
    let skill_node_id = harness_skill_node_id(skill_name);
    let harness = update_harness_node_with_retry(
        engine,
        &harness_node_id,
        harness_id,
        |properties, _now| {
            let mut skills = skill_refs_from_properties(properties);
            if !skills.iter().any(|s| s == skill_name) {
                skills.push(skill_name.to_string());
                skills.sort();
            }
            ensure_desired_object(properties);
            properties["desired"]["skill_refs"] = json!(skills);
        },
    )?;
    engine.upsert_edge(&Edge {
        source_id: harness.id.clone(),
        target_id: skill_node_id.clone(),
        relation: EdgeRelation::References,
        properties: json!({}),
        worktree: String::new(),
    })?;
    record_mutation(
        engine,
        "harness_skill_assign",
        Some(harness.id.clone()),
        None,
        Some(skill_name.into()),
        Some(format!(
            "Assigned harness skill {} to {}",
            skill_name, harness_id
        )),
        json!({ "skill_id": skill_node_id }),
    )?;
    println!("Assigned harness skill {} to {}", skill_name, harness_id);
    Ok(())
}

fn unassign_harness_skill(engine: &GraphEngine, harness_id: &str, skill_name: &str) -> Result<()> {
    let harness_node_id = harness_node_id(harness_id);
    let harness = update_harness_node_with_retry(
        engine,
        &harness_node_id,
        harness_id,
        |properties, _now| {
            let mut skills = skill_refs_from_properties(properties);
            skills.retain(|s| s != skill_name);
            ensure_desired_object(properties);
            properties["desired"]["skill_refs"] = json!(skills);
        },
    )?;
    record_mutation(
        engine,
        "harness_skill_unassign",
        Some(harness.id.clone()),
        None,
        Some(skill_name.into()),
        Some(format!(
            "Unassigned harness skill {} from {}",
            skill_name, harness_id
        )),
        json!({}),
    )?;
    println!(
        "Unassigned harness skill {} from {}",
        skill_name, harness_id
    );
    Ok(())
}

fn show_status(engine: &GraphEngine, node_id: &str) -> Result<()> {
    let Some(node) = engine.get_node(node_id)? else {
        bail!("Harness not found: {}", node_id);
    };
    let mutations = engine.get_mutations(Some(node_id), 10)?;
    let last_apply = mutations.iter().find(|m| m.action == "harness_apply");
    let last_verify = mutations.iter().find(|m| m.action == "harness_verify");
    let latest_observation =
        latest_harness_node_by_kind(engine, NodeKind::HarnessObservation, node_id)?;
    let open_drift = open_harness_drift(engine, node_id)?;
    let projection_path = node
        .properties
        .get("desired")
        .and_then(|v| v.get("projection_targets"))
        .and_then(|v| v.as_array())
        .and_then(|targets| targets.first())
        .and_then(|v| v.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let canonical_profile = prop_str(&node, &["desired", "canonical_profile_name"]).unwrap_or("-");
    let desired_profile = prop_str(&node, &["desired", "role_charter"]).unwrap_or("-");
    let desired_bundle = prop_str(&node, &["desired", "profile_name"]).unwrap_or("-");
    let desired_skills = join_str_array(
        node.properties
            .get("desired")
            .and_then(|v| v.get("skill_refs")),
    );
    let rendered_hash = prop_str(&node, &["rendered", "config_hash"]).unwrap_or("-");
    let rendered_at = prop_str(&node, &["rendered", "rendered_at"]).unwrap_or("-");
    let observed_status = prop_str(&node, &["observed_summary", "status"]).unwrap_or("-");
    let observed_drift = prop_str(&node, &["observed_summary", "drift_status"]).unwrap_or("-");
    let observed_checked = prop_str(&node, &["observed_summary", "last_checked_at"]).unwrap_or("-");
    let observed_freshness = prop_str(&node, &["observed_summary", "freshness"]).unwrap_or("-");

    println!("Harness: {}", node.id);
    println!("{}", "─".repeat(60));
    println!(
        "Runtime: {}",
        prop_str(&node, &["runtime_kind"]).unwrap_or("-")
    );
    println!("Canonical profile: {}", canonical_profile);
    println!("Desired profile: {}", desired_profile);
    println!("Assigned bundle: {}", desired_bundle);
    println!(
        "Desired skills: {}",
        desired_skills.unwrap_or_else(|| "-".into())
    );
    println!("Projection target: {}", projection_path);
    println!("Rendered at: {}", rendered_at);
    println!("Rendered hash: {}", rendered_hash);
    println!("Observed status: {}", observed_status);
    println!("Observed drift: {}", observed_drift);
    println!("Observed freshness: {}", observed_freshness);
    println!("Last checked: {}", observed_checked);
    println!(
        "Last apply: {}",
        last_apply
            .map(|m| format!(
                "{} ({})",
                m.timestamp.to_rfc3339(),
                m.to_value.clone().unwrap_or_else(|| "-".into())
            ))
            .unwrap_or_else(|| "-".into())
    );
    println!(
        "Last verify: {}",
        last_verify
            .map(|m| format!(
                "{} ({})",
                m.timestamp.to_rfc3339(),
                m.to_value.clone().unwrap_or_else(|| "-".into())
            ))
            .unwrap_or_else(|| "-".into())
    );
    if let Some(observation) = latest_observation {
        println!(
            "Latest observation: {} ({}, {})",
            prop_str(&observation, &["observed_at"]).unwrap_or("-"),
            prop_str(&observation, &["status"]).unwrap_or("-"),
            prop_str(&observation, &["profile_hash"]).unwrap_or("-")
        );
    } else {
        println!("Latest observation: -");
    }
    if let Some(drift) = open_drift {
        println!(
            "Open drift: {} [{}] last seen {}",
            prop_str(&drift, &["summary"]).unwrap_or("drift detected"),
            prop_str(&drift, &["severity"]).unwrap_or("-"),
            prop_str(&drift, &["last_seen_at"]).unwrap_or("-")
        );
    } else {
        println!("Open drift: none");
    }
    println!(
        "\nRaw properties:\n{}",
        serde_json::to_string_pretty(&node.properties)?
    );
    Ok(())
}

fn plan_harness(
    engine: &GraphEngine,
    adapter: &dyn HarnessAdapter,
    harness_id: &str,
    profile: Option<String>,
    bundle: Option<String>,
) -> Result<()> {
    let desired = resolve_desired_apply_state(engine, harness_id, profile, bundle)?;
    let mut config = adapter.plan(harness_id, Some(desired.role_charter.clone()))?;
    if let Some(skill_refs) = desired.skill_refs {
        config.skills = skill_refs.clone();
        config.content = render_harness_config(
            adapter.runtime_kind(),
            harness_id,
            &desired.role_charter,
            skill_refs,
        )?;
    }
    println!("Plan for {}:", harness_id);
    println!("  runtime: {}", config.runtime_kind);
    println!("  profile: {}", config.profile);
    println!(
        "  canonical: {}",
        desired
            .canonical_profile_name
            .clone()
            .unwrap_or_else(|| "-".into())
    );
    println!(
        "  bundle:  {}",
        desired.profile_name.unwrap_or_else(|| "-".into())
    );
    println!("  target:  {}", config.target_path.display());
    println!("\nProjected file:");
    match serde_json::from_slice::<serde_json::Value>(&config.content) {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
        Err(_) => println!("{}", String::from_utf8_lossy(&config.content)),
    }
    Ok(())
}

fn apply_harness(
    engine: &GraphEngine,
    adapter: &dyn HarnessAdapter,
    harness_id: &str,
    profile: Option<String>,
    bundle: Option<String>,
) -> Result<()> {
    let desired = resolve_desired_apply_state(engine, harness_id, profile, bundle)?;
    let mut config = adapter.plan(harness_id, Some(desired.role_charter.clone()))?;
    let existing_harness = engine.get_node(&harness_node_id(harness_id))?;
    let desired_canonical_profile_name = desired
        .canonical_profile_name
        .as_ref()
        .map(|value| json!(value));
    let desired_profile_name = desired.profile_name.as_ref().map(|value| json!(value));
    if let Some(skill_refs) = desired
        .skill_refs
        .clone()
        .or_else(|| existing_harness.as_ref().and_then(harness_skill_refs))
    {
        config.skills = skill_refs;
        config.content = render_harness_config(
            adapter.runtime_kind(),
            harness_id,
            &desired.role_charter,
            config.skills.clone(),
        )?;
    }
    write_harness_file(&config.target_path, &config.content)?;
    let aux_checks: Vec<AuxCheck> = match adapter.runtime_kind() {
        "windsurf" => write_windsurf_workspace_files(
            engine,
            harness_id,
            &desired.role_charter,
            &config.skills,
            &config.target_path,
        )?,
        "claude-code" => write_claude_code_workspace_files(harness_id, &config.target_path)?,
        "antigravity" => write_antigravity_workspace_files(harness_id, &config.target_path)?,
        "codex" => write_codex_workspace_files(harness_id, &config.target_path)?,
        _ => Vec::new(),
    };
    let hash = sha256_hex(&config.content);
    let aux_checks_json = serde_json::to_value(&aux_checks)?;
    let now = Utc::now();
    let harness_node_id = harness_node_id(harness_id);
    let projection_node_id = harness_projection_node_id(harness_id, "main-config");
    let harness =
        update_harness_node_with_retry(engine, &harness_node_id, harness_id, |properties, now| {
            properties["runtime_kind"] = json!(config.runtime_kind);
            properties["status"] = json!("active");
            properties["owner"] = json!("operator");
            properties["desired"] = json!({
                "role_charter": desired.role_charter,
                "skill_refs": config.skills,
                "projection_targets": [{
                    "projection_id": "main-config",
                    "kind": "file",
                    "path": config.target_path.to_string_lossy().to_string()
                }]
            });
            if let Some(canonical_profile_name) = desired_canonical_profile_name.clone() {
                properties["desired"]["canonical_profile_name"] = canonical_profile_name;
            }
            if let Some(profile_name) = desired_profile_name.clone() {
                properties["desired"]["profile_name"] = profile_name;
            }
            properties["rendered"] = json!({
                "revision": now.timestamp(),
                "config_hash": hash,
                "rendered_at": now.to_rfc3339(),
                "renderer_version": "v2",
                "aux_checks": aux_checks_json.clone()
            });
            properties["observed_summary"] = json!({
                "status": "pending_verify",
                "last_checked_at": null,
                "freshness": "unknown",
                "drift_status": "pending_verify"
            });
        })?;

    let projection = Node {
        id: projection_node_id.clone(),
        kind: NodeKind::HarnessProjection,
        name: format!("{} projection", harness_id),
        properties: json!({
            "harness_id": harness_node_id,
            "projection_id": "main-config",
            "target_kind": "file",
            "target_path": config.target_path.to_string_lossy().to_string(),
            "render_status": "rendered",
            "content_hash": hash,
            "rendered_at": now.to_rfc3339(),
            "renderer_version": "v1"
        }),
        file_path: Some(config.target_path.to_string_lossy().to_string()),
        worktree: String::new(),
        created_at: now,
        updated_at: now,
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&projection)?;
    engine.upsert_edge(&Edge {
        source_id: projection_node_id,
        target_id: harness.id.clone(),
        relation: EdgeRelation::References,
        properties: json!({}),
        worktree: String::new(),
    })?;

    record_mutation(
        engine,
        "harness_apply",
        Some(harness.id.clone()),
        None,
        Some("rendered".into()),
        Some(format!(
            "Applied {} harness {}",
            adapter.runtime_kind(),
            harness_id
        )),
        json!({
            "path": config.target_path.to_string_lossy().to_string(),
            "canonical_profile": harness.properties["desired"].get("canonical_profile_name").cloned(),
            "profile": harness.properties["desired"]["role_charter"].clone(),
            "bundle": harness.properties["desired"].get("profile_name").cloned(),
            "runtime_kind": config.runtime_kind
        }),
    )?;

    println!(
        "Applied {} harness {} to {}",
        adapter.runtime_kind(),
        harness_id,
        config.target_path.display()
    );
    Ok(())
}

#[derive(Debug, Clone)]
struct DesiredApplyState {
    role_charter: String,
    canonical_profile_name: Option<String>,
    profile_name: Option<String>,
    skill_refs: Option<Vec<String>>,
}

fn resolve_desired_apply_state(
    engine: &GraphEngine,
    harness_id: &str,
    profile: Option<String>,
    bundle: Option<String>,
) -> Result<DesiredApplyState> {
    if profile.is_some() && bundle.is_some() {
        bail!("use either --profile or --bundle, not both");
    }

    let existing_harness = engine.get_node(&harness_node_id(harness_id))?;
    let existing_profile = existing_harness
        .as_ref()
        .and_then(|node| node.properties.get("desired"))
        .and_then(|v| v.get("role_charter"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| "orchestrator".into());
    let existing_bundle = existing_harness
        .as_ref()
        .and_then(|node| node.properties.get("desired"))
        .and_then(|v| v.get("profile_name"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let existing_canonical_profile = existing_harness
        .as_ref()
        .and_then(|node| node.properties.get("desired"))
        .and_then(|v| v.get("canonical_profile_name"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let existing_skills = existing_harness.as_ref().and_then(harness_skill_refs);

    if let Some(bundle_name) = bundle {
        let profile_node_id = harness_profile_node_id(&bundle_name);
        let Some(bundle_node) = engine.get_node(&profile_node_id)? else {
            bail!("Harness profile not found: {}", bundle_name);
        };
        let skill_refs = bundle_node
            .properties
            .get("skills")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if skill_refs.is_empty() {
            bail!("Harness profile {} has no skills", bundle_name);
        }
        return Ok(DesiredApplyState {
            role_charter: profile.unwrap_or(existing_profile),
            canonical_profile_name: None,
            profile_name: Some(bundle_name),
            skill_refs: Some(skill_refs),
        });
    }

    if let Some(profile_name) = profile.clone() {
        if let Some(canonical_profile) = resolve_canonical_profile(engine, &profile_name)? {
            let role_charter = canonical_profile
                .properties
                .get("role_charter")
                .and_then(|v| v.as_str())
                .unwrap_or(&profile_name)
                .to_string();
            let skill_refs = canonical_profile
                .properties
                .get("skill_refs")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            return Ok(DesiredApplyState {
                role_charter,
                canonical_profile_name: Some(canonical_profile.name),
                profile_name: existing_bundle,
                skill_refs: Some(skill_refs),
            });
        }
    }

    Ok(DesiredApplyState {
        role_charter: profile.unwrap_or(existing_profile),
        canonical_profile_name: existing_canonical_profile,
        profile_name: existing_bundle,
        skill_refs: existing_skills,
    })
}

fn export_harness(
    engine: &GraphEngine,
    adapter: &dyn HarnessAdapter,
    harness_id: &str,
) -> Result<()> {
    let Some(node) = engine.get_node(&harness_node_id(harness_id))? else {
        bail!("Harness not found: {}", harness_id);
    };
    let config = adapter.config_from_harness(harness_id, &node)?;
    apply_harness(engine, adapter, harness_id, Some(config.profile), None)
}

fn verify_harness(
    engine: &GraphEngine,
    adapter: &dyn HarnessAdapter,
    harness_id: &str,
) -> Result<()> {
    let harness_id_full = harness_node_id(harness_id);
    let drift_class = "rendered_observed";
    let drift_id = harness_drift_node_id(harness_id, drift_class);
    for _ in 0..HARNESS_UPDATE_RETRIES {
        let Some(harness) = engine.get_node(&harness_id_full)? else {
            bail!("Harness not found: {}", harness_id);
        };
        let config = adapter.config_from_harness(harness_id, &harness)?;

        let now = Utc::now();
        let observed_id = format!("harness_observation:{}:{}", harness_id, now.timestamp());
        let mut drift_status = "clean";
        let mut severity = None;
        let rendered_hash = harness
            .properties
            .get("rendered")
            .and_then(|v| v.get("config_hash"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let (status, profile_hash, freshness) = if config.target_path.exists() {
            let content = fs::read(&config.target_path).with_context(|| {
                format!(
                    "failed to read harness file {}",
                    config.target_path.display()
                )
            })?;
            let observed_hash = sha256_hex(&content);
            if observed_hash != rendered_hash {
                drift_status = "drifted";
                severity = Some("warn");
            }
            ("ok", Some(observed_hash), "fresh")
        } else {
            drift_status = "drifted";
            severity = Some("error");
            ("missing", None, "stale")
        };

        // Auxiliary files written by apply (workspace imports, hooks, managed
        // blocks) drift independently of the main projection; check each.
        let aux_checks: Vec<AuxCheck> = harness
            .properties
            .get("rendered")
            .and_then(|v| v.get("aux_checks"))
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let aux_failures: Vec<String> = aux_checks
            .iter()
            .filter_map(|check| check.evaluate())
            .collect();
        if !aux_failures.is_empty() {
            drift_status = "drifted";
            if severity != Some("error") {
                severity = Some("warn");
            }
        }

        let mut updated = harness.clone();
        updated.updated_at = now;
        updated.properties["observed_summary"] = json!({
            "status": status,
            "last_checked_at": now.to_rfc3339(),
            "freshness": freshness,
            "drift_status": drift_status
        });
        if !engine.compare_and_swap_node(&updated, harness.updated_at)? {
            continue;
        }

        let observation = Node {
            id: observed_id.clone(),
            kind: NodeKind::HarnessObservation,
            name: format!("{} observation", harness_id),
            properties: json!({
                "harness_id": harness_id_full,
                "observed_at": now.to_rfc3339(),
                "collector": "filesystem",
                "freshness": freshness,
                "profile_hash": profile_hash,
                "capability_surface": ["managed_config_file"],
                "compatibility": "ok",
                "status": status
            }),
            file_path: Some(config.target_path.to_string_lossy().to_string()),
            worktree: String::new(),
            created_at: now,
            updated_at: now,
            embedding: None,
            embedding_model: None,
            embedding_dims: None,
            embedding_updated: None,
            embedding_hash: None,
        };
        engine.upsert_node(&observation)?;
        engine.upsert_edge(&Edge {
            source_id: observed_id.clone(),
            target_id: harness_id_full.clone(),
            relation: EdgeRelation::Validates,
            properties: json!({}),
            worktree: String::new(),
        })?;

        if let Some(sev) = severity {
            let existing_drift = engine.get_node(&drift_id)?;
            let first_detected_at = existing_drift
                .as_ref()
                .and_then(|node| node.properties.get("first_detected_at"))
                .cloned()
                .unwrap_or_else(|| json!(now.to_rfc3339()));
            let drift = Node {
                id: drift_id.clone(),
                kind: NodeKind::HarnessDrift,
                name: format!("{} drift", harness_id),
                properties: json!({
                    "harness_id": harness_id_full,
                    "drift_class": drift_class,
                    "severity": sev,
                    "status": "open",
                    "first_detected_at": first_detected_at,
                    "last_seen_at": now.to_rfc3339(),
                    "resolved_at": serde_json::Value::Null,
                    "resolved_by_observation": serde_json::Value::Null,
                    "summary": if aux_failures.is_empty() {
                        "Rendered config does not match observed local state".to_string()
                    } else {
                        format!(
                            "Harness drift: {} auxiliary check(s) failed",
                            aux_failures.len()
                        )
                    },
                    "details": {
                        "rendered_hash": rendered_hash,
                        "observed_hash": observation.properties["profile_hash"].clone(),
                        "aux_failures": aux_failures
                    }
                }),
                file_path: Some(config.target_path.to_string_lossy().to_string()),
                worktree: String::new(),
                created_at: existing_drift
                    .as_ref()
                    .map(|node| node.created_at)
                    .unwrap_or(now),
                updated_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
            };
            engine.upsert_node(&drift)?;
            engine.upsert_edge(&Edge {
                source_id: drift_id,
                target_id: harness_id_full.clone(),
                relation: EdgeRelation::Validates,
                properties: json!({}),
                worktree: String::new(),
            })?;
        } else if let Some(mut existing_drift) = engine.get_node(&drift_id)? {
            existing_drift.updated_at = now;
            existing_drift.properties["status"] = json!("resolved");
            existing_drift.properties["resolved_at"] = json!(now.to_rfc3339());
            existing_drift.properties["resolved_by_observation"] = json!(observed_id.clone());
            existing_drift.properties["last_seen_at"] = json!(now.to_rfc3339());
            engine.upsert_node(&existing_drift)?;
        }

        record_mutation(
            engine,
            "harness_verify",
            Some(harness_id_full.clone()),
            None,
            Some(drift_status.into()),
            Some(format!(
                "Verified {} harness {}",
                adapter.runtime_kind(),
                harness_id
            )),
            json!({
                "path": config.target_path.to_string_lossy().to_string(),
                "runtime_kind": adapter.runtime_kind(),
                "drift_status": drift_status
            }),
        )?;

        println!(
            "Verified {} harness {}: {}",
            adapter.runtime_kind(),
            harness_id,
            drift_status
        );
        return Ok(());
    }

    bail!(
        "failed to verify harness {} after {} retries",
        harness_id,
        HARNESS_UPDATE_RETRIES
    )
}

fn report_drift(engine: &GraphEngine, harness_id: Option<&str>) -> Result<()> {
    let harnesses = match harness_id {
        Some(id) => {
            let Some(node) = engine.get_node(&harness_node_id(id))? else {
                bail!("Harness not found: {}", id);
            };
            vec![node]
        }
        None => engine.query_nodes(Some(NodeKind::Harness), None)?,
    };

    if harnesses.is_empty() {
        println!("No managed harnesses found.");
        return Ok(());
    }

    println!("{:<24} {:<14} {}", "HARNESS", "DRIFT", "LAST CHECK");
    println!("{}", "─".repeat(65));
    for harness in harnesses {
        let drift = prop_str(&harness, &["observed_summary", "drift_status"]).unwrap_or("unknown");
        let checked = prop_str(&harness, &["observed_summary", "last_checked_at"]).unwrap_or("-");
        println!("{:<24} {:<14} {}", harness.id, drift, checked);
    }
    Ok(())
}

fn start_harness_trial(
    engine: &GraphEngine,
    harness_id: &str,
    seam_id: &str,
    session_id: Option<&str>,
    profile: Option<&str>,
    workflow: Option<&str>,
    proposal_id: Option<&str>,
    task_id: Option<&str>,
    agent: Option<&str>,
    agent_model: Option<&str>,
    phase: Option<&str>,
) -> Result<()> {
    if let Some(profile_name) = profile {
        with_adapter(engine, harness_id, |adapter| {
            apply_harness(
                engine,
                adapter,
                harness_id,
                Some(profile_name.to_string()),
                None,
            )
        })?;
    }

    let harness = engine
        .get_node(&harness_node_id(harness_id))?
        .ok_or_else(|| anyhow::anyhow!("Harness not found: {}", harness_id))?;
    let runtime_kind = harness
        .properties
        .get("runtime_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let role_charter = harness
        .properties
        .get("desired")
        .and_then(|v| v.get("role_charter"))
        .and_then(|v| v.as_str())
        .unwrap_or("orchestrator");
    let canonical_profile_name = harness
        .properties
        .get("desired")
        .and_then(|v| v.get("canonical_profile_name"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let workflow_name = workflow.map(str::to_string);
    if let Some(ref workflow_name) = workflow_name {
        ensure_canonical_workflow_exists(engine, workflow_name)?;
    }

    let now = Utc::now();
    let session_id = session_id
        .map(str::to_string)
        .unwrap_or_else(|| format!("session:trial:{}:{}", harness_id, now.timestamp()));
    let workstream_id = format!("workstream:{}", seam_id.replace("seam:", ""));
    let phase = phase.unwrap_or("trial_started");
    let agent = agent.unwrap_or(runtime_kind);
    let agent_model = agent_model.unwrap_or("unknown");
    let git_context = detect_git_session_context();

    let workstream = Node {
        id: workstream_id.clone(),
        kind: NodeKind::Workstream,
        name: format!("Work on {}", seam_id),
        properties: json!({
            "seam_id": seam_id,
            "proposal_id": proposal_id,
            "task_id": task_id,
            "agent": agent,
            "status": "active",
            "phase": phase,
            "started_at": now.to_rfc3339(),
            "last_activity": now.to_rfc3339(),
            "harness_id": harness.id,
            "workflow_name": workflow_name,
            "branch": git_context.branch.clone(),
            "worktree_path": git_context.worktree_path.clone(),
        }),
        file_path: None,
        worktree: String::new(),
        created_at: now,
        updated_at: now,
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&workstream)?;
    engine.upsert_edge(&Edge {
        source_id: workstream_id.clone(),
        target_id: seam_id.to_string(),
        relation: EdgeRelation::PartOf,
        properties: json!({ "role": "active_work" }),
        worktree: String::new(),
    })?;
    if let Some(proposal_id) = proposal_id {
        engine.upsert_edge(&Edge {
            source_id: workstream_id.clone(),
            target_id: proposal_id.to_string(),
            relation: EdgeRelation::Governs,
            properties: json!({ "direction": "implements" }),
            worktree: String::new(),
        })?;
    }

    let session = Node {
        id: session_id.clone(),
        kind: NodeKind::Session,
        name: format!("Harness trial {} - {}", agent, now.format("%Y-%m-%d %H:%M")),
        properties: json!({
            "agent": agent,
            "agent_model": agent_model,
            "seam_id": seam_id,
            "proposal_id": proposal_id,
            "task_id": task_id,
            "status": "active",
            "phase": phase,
            "start_time": now.to_rfc3339(),
            "last_activity": now.to_rfc3339(),
            "files_touched": [],
            "lines_changed": 0,
            "test_runs": 0,
            "tokens_input": 0,
            "tokens_output": 0,
            "tokens_total": 0,
            "elapsed_ms": 0,
            "harness_id": harness.id,
            "runtime_kind": runtime_kind,
            "role_charter": role_charter,
            "canonical_profile_name": canonical_profile_name,
            "workflow_name": workflow_name,
            "trial_kind": "harness",
            "branch": git_context.branch.clone(),
            "worktree_path": git_context.worktree_path.clone()
        }),
        file_path: None,
        worktree: String::new(),
        created_at: now,
        updated_at: now,
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&session)?;
    engine.upsert_edge(&Edge {
        source_id: session_id.clone(),
        target_id: seam_id.to_string(),
        relation: EdgeRelation::WorkingOn,
        properties: json!({ "since": now.to_rfc3339() }),
        worktree: String::new(),
    })?;
    engine.upsert_edge(&Edge {
        source_id: session_id.clone(),
        target_id: workstream_id.clone(),
        relation: EdgeRelation::WorkingOn,
        properties: json!({}),
        worktree: String::new(),
    })?;
    engine.upsert_edge(&Edge {
        source_id: session_id.clone(),
        target_id: harness.id.clone(),
        relation: EdgeRelation::References,
        properties: json!({ "role": "trial_target" }),
        worktree: String::new(),
    })?;
    if let Some(task_id) = task_id {
        engine.upsert_edge(&Edge {
            source_id: session_id.clone(),
            target_id: task_id.to_string(),
            relation: EdgeRelation::Implements,
            properties: json!({}),
            worktree: String::new(),
        })?;
    }

    record_mutation(
        engine,
        "harness_trial_start",
        Some(session_id.clone()),
        None,
        Some("active".into()),
        Some(format!("Started measured harness trial on {}", harness_id)),
        json!({
            "harness_id": harness.id,
            "runtime_kind": runtime_kind,
            "role_charter": role_charter,
            "canonical_profile_name": session.properties["canonical_profile_name"].clone(),
            "workflow_name": session.properties["workflow_name"].clone(),
            "seam_id": seam_id,
            "proposal_id": proposal_id,
            "task_id": task_id
        }),
    )?;

    println!("Started harness trial {}", session_id);
    println!("  harness:   {}", harness_id);
    println!("  seam:      {}", seam_id);
    println!("  runtime:   {}", runtime_kind);
    println!("  role:      {}", role_charter);
    println!(
        "  canonical: {}",
        session
            .properties
            .get("canonical_profile_name")
            .and_then(|v| v.as_str())
            .unwrap_or("-")
    );
    println!(
        "  workflow:  {}",
        session
            .properties
            .get("workflow_name")
            .and_then(|v| v.as_str())
            .unwrap_or("-")
    );
    println!("  phase:     {}", phase);
    Ok(())
}

fn detect_git_session_context() -> GitSessionContext {
    let current_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(_) => return GitSessionContext::default(),
    };

    let worktree_path = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&current_dir)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty());

    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(&current_dir)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|branch| branch.trim().to_string())
        .filter(|branch| !branch.is_empty());

    GitSessionContext {
        branch,
        worktree_path,
    }
}

#[allow(clippy::too_many_arguments)]
fn report_harness_trial_activity(
    engine: &GraphEngine,
    session_id: &str,
    activity_type: &str,
    phase: Option<&str>,
    tokens_input: Option<i64>,
    tokens_output: Option<i64>,
    elapsed_ms: Option<i64>,
    lines_changed: Option<i64>,
    files: &[String],
    note: Option<&str>,
) -> Result<()> {
    if !trial_activity_has_signal(
        phase,
        tokens_input,
        tokens_output,
        elapsed_ms,
        lines_changed,
        files,
        note,
    ) {
        bail!(
            "Harness trial activity must include at least one signal (phase, tokens, elapsed_ms, lines_changed, files, or note)"
        );
    }

    let mut session = engine
        .get_node(session_id)?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;
    let now = Utc::now();
    let mut props = session.properties.as_object().cloned().unwrap_or_default();
    props.insert("last_activity".to_string(), json!(now.to_rfc3339()));
    if let Some(phase) = phase {
        props.insert("phase".to_string(), json!(phase));
    }

    if !files.is_empty() {
        let existing_files = props
            .get("files_touched")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut all_files: Vec<String> = existing_files
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        for file in files {
            if !all_files.iter().any(|existing| existing == file) {
                all_files.push(file.clone());
            }
        }
        props.insert("files_touched".to_string(), json!(all_files));
    }

    if let Some(lines_changed) = lines_changed {
        let existing_lines = props
            .get("lines_changed")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        props.insert(
            "lines_changed".to_string(),
            json!(existing_lines + lines_changed),
        );
    }

    if activity_type == "test_run" {
        let existing_tests = props.get("test_runs").and_then(|v| v.as_i64()).unwrap_or(0);
        props.insert("test_runs".to_string(), json!(existing_tests + 1));
    }

    let tokens_in = tokens_input.unwrap_or(0);
    let tokens_out = tokens_output.unwrap_or(0);
    if tokens_in > 0 || tokens_out > 0 {
        let existing_in = props
            .get("tokens_input")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let existing_out = props
            .get("tokens_output")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let new_in = existing_in + tokens_in;
        let new_out = existing_out + tokens_out;
        props.insert("tokens_input".to_string(), json!(new_in));
        props.insert("tokens_output".to_string(), json!(new_out));
        props.insert("tokens_total".to_string(), json!(new_in + new_out));
    }

    if let Some(elapsed_ms) = elapsed_ms {
        let existing_elapsed = props
            .get("elapsed_ms")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        props.insert(
            "elapsed_ms".to_string(),
            json!(existing_elapsed.max(elapsed_ms)),
        );
    }

    session.properties = serde_json::Value::Object(props);
    session.updated_at = now;
    engine.upsert_node(&session)?;

    let mut details = json!({
        "files": files,
        "lines_changed": lines_changed,
        "tokens_input": tokens_input,
        "tokens_output": tokens_output,
        "elapsed_ms": elapsed_ms,
        "note": note
    });
    scrub_nulls(&mut details);
    record_mutation(
        engine,
        activity_type,
        Some(session_id.to_string()),
        None,
        phase.map(str::to_string),
        Some(format!("Harness trial activity: {}", activity_type)),
        details,
    )?;

    println!("Recorded {} for {}", activity_type, session_id);
    if let Some(elapsed_ms) = elapsed_ms {
        println!("  elapsed_ms: {}", elapsed_ms);
    }
    if tokens_in > 0 || tokens_out > 0 {
        println!("  tokens: {} in / {} out", tokens_in, tokens_out);
    }
    Ok(())
}

fn close_harness_trial(
    engine: &GraphEngine,
    session_id: &str,
    status: &str,
    verified: Option<&str>,
    summary: Option<&str>,
    elapsed_ms: Option<i64>,
    tokens_total: Option<i64>,
    quota_remaining: Option<i64>,
) -> Result<()> {
    if !trial_close_has_verified(status, verified) {
        bail!(
            "Completed harness trials must include a verification level (for example, test-green)"
        );
    }

    let mut session = engine
        .get_node(session_id)?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;
    let now = Utc::now();
    let mut props = session.properties.as_object().cloned().unwrap_or_default();
    let computed_elapsed = elapsed_ms
        .or_else(|| compute_elapsed_ms(props.get("start_time").and_then(|v| v.as_str()), now));
    props.insert("status".to_string(), json!(status));
    props.insert("end_time".to_string(), json!(now.to_rfc3339()));
    props.insert("last_activity".to_string(), json!(now.to_rfc3339()));
    if let Some(verified) = verified {
        props.insert("verified".to_string(), json!(verified));
    }
    if let Some(summary) = summary {
        props.insert("summary".to_string(), json!(summary));
    }
    if let Some(elapsed_ms) = computed_elapsed {
        props.insert("elapsed_ms".to_string(), json!(elapsed_ms));
    }
    if let Some(tokens_total) = tokens_total {
        props.insert("tokens_total".to_string(), json!(tokens_total));
    }
    if let Some(quota_remaining) = quota_remaining {
        props.insert("quota_remaining".to_string(), json!(quota_remaining));
    }
    let session_tokens_total = props
        .get("tokens_total")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let session_tokens_in = props
        .get("tokens_input")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let session_tokens_out = props
        .get("tokens_output")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let session_elapsed_ms = props
        .get("elapsed_ms")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    session.properties = serde_json::Value::Object(props);
    session.updated_at = now;
    engine.upsert_node(&session)?;
    let closed_workstreams = engine.close_linked_workstreams(&session, status, summary)?;

    if session_tokens_total > 0 || session_elapsed_ms > 0 {
        for edge in engine.get_edges_from(session_id)? {
            if edge.relation == EdgeRelation::WorkingOn || edge.relation == EdgeRelation::Implements
            {
                if let Some(mut node) = engine.get_node(&edge.target_id)? {
                    let mut node_props = node.properties.as_object().cloned().unwrap_or_default();
                    if session_tokens_total > 0 {
                        let existing = node_props
                            .get("tokens_total")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let existing_in = node_props
                            .get("tokens_input")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let existing_out = node_props
                            .get("tokens_output")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        node_props.insert(
                            "tokens_total".to_string(),
                            json!(existing + session_tokens_total),
                        );
                        node_props.insert(
                            "tokens_input".to_string(),
                            json!(existing_in + session_tokens_in),
                        );
                        node_props.insert(
                            "tokens_output".to_string(),
                            json!(existing_out + session_tokens_out),
                        );
                    }
                    if session_elapsed_ms > 0 {
                        let existing_elapsed = node_props
                            .get("elapsed_ms")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        node_props.insert(
                            "elapsed_ms".to_string(),
                            json!(existing_elapsed + session_elapsed_ms),
                        );
                    }
                    node.updated_at = now;
                    node.properties = serde_json::Value::Object(node_props);
                    let _ = engine.upsert_node(&node);
                }
            }
        }
    }

    let mut details = json!({
        "verified": verified,
        "tokens_total": session_tokens_total,
        "tokens_input": session_tokens_in,
        "tokens_output": session_tokens_out,
        "elapsed_ms": session_elapsed_ms,
        "quota_remaining": quota_remaining,
        "workstream_ids": closed_workstreams,
    });
    scrub_nulls(&mut details);
    record_mutation(
        engine,
        "harness_trial_close",
        Some(session_id.to_string()),
        Some("active".into()),
        Some(status.to_string()),
        summary
            .map(str::to_string)
            .or_else(|| Some("Closed measured harness trial".into())),
        details,
    )?;

    println!("Closed harness trial {}", session_id);
    println!("  status: {}", status);
    println!("  elapsed_ms: {}", session_elapsed_ms);
    println!("  tokens_total: {}", session_tokens_total);
    Ok(())
}

fn render_harness_config(
    runtime_kind: &str,
    harness_id: &str,
    profile: &str,
    skills: Vec<String>,
) -> Result<Vec<u8>> {
    match runtime_kind {
        "codex" => Ok(render_codex_markdown(harness_id, profile, &skills).into_bytes()),
        "claude-code" => Ok(render_claude_code_markdown(harness_id, profile, &skills).into_bytes()),
        "windsurf" => Ok(render_windsurf_rule_markdown(harness_id, profile, &skills).into_bytes()),
        "antigravity" => Ok(render_antigravity_markdown(harness_id, profile, &skills).into_bytes()),
        other => bail!("unsupported harness runtime: {}", other),
    }
}

/// Node ids (`harness:codex-local`) and short ids (`codex-local`) are both
/// accepted by every CLI entry point, but projection paths must be derived
/// from the short form only — otherwise `verify harness:x` and `verify x`
/// silently address two different files (observed live: duplicate
/// `~/.gemini/.../harness:gemini-*` projection dirs).
fn short_harness_id(harness_id: &str) -> &str {
    harness_id.strip_prefix("harness:").unwrap_or(harness_id)
}

fn codex_target_path(harness_id: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("failed to locate home directory")?;
    Ok(home
        .join(".codex")
        .join("philotic")
        .join("harnesses")
        .join(short_harness_id(harness_id))
        .join("AGENTS.md"))
}

fn claude_code_target_path(harness_id: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("failed to locate home directory")?;
    Ok(home
        .join(".claude")
        .join("philotic")
        .join("harnesses")
        .join(short_harness_id(harness_id))
        .join("CLAUDE.md"))
}

fn antigravity_target_path(harness_id: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("failed to locate home directory")?;
    Ok(home
        .join(".gemini")
        .join("antigravity")
        .join("philotic")
        .join("harnesses")
        .join(short_harness_id(harness_id))
        .join("GEMINI_HARNESS.md"))
}

fn windsurf_target_path(harness_id: &str) -> Result<PathBuf> {
    let root = std::env::current_dir().context("failed to locate current workspace")?;
    Ok(root
        .join(".windsurf")
        .join("rules")
        .join(format!("philotic-{}.md", short_harness_id(harness_id))))
}

fn default_skills_for_profile(profile: &str) -> Vec<String> {
    match profile {
        "verifier" => vec!["verification".into(), "muninn-memory-habit".into()],
        "implementer" => vec!["implementation".into(), "muninn-memory-habit".into()],
        _ => vec![
            "planning".into(),
            "verification".into(),
            "muninn-memory-habit".into(),
        ],
    }
}

fn render_codex_markdown(harness_id: &str, profile: &str, skills: &[String]) -> String {
    let harness_id = short_harness_id(harness_id);
    let skills_lines = if skills.is_empty() {
        "- none".to_string()
    } else {
        skills
            .iter()
            .map(|skill| format!("- {}", skill))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "# Philotic Codex Harness: {harness_id}\n\nManaged by `phil graph harness`.\n\n## Role Charter\n\n{profile}\n\n## Active Skills\n\n{skills_lines}\n\n## Session Protocol\n\n- At session start, run `phil graph harness verify {harness_id}` and surface anything other than `clean`.\n- Follow the repository `AGENTS.md` protocol; the skills above are this harness's active skill set under `skills/` in the repo.\n- Record decisions with `graph_decide` (MCP) or `phil graph decide`, and durable memory with Muninn.\n\n## Muninn Memory\n\n{MUNINN_HARNESS_INSTRUCTIONS}"
    )
}

/// Replace the marker-delimited block in `existing` with `block`, or append it.
/// Idempotent: applying the same block twice yields identical content.
fn merge_managed_block(existing: &str, begin: &str, end: &str, block: &str) -> String {
    match (existing.find(begin), existing.find(end)) {
        (Some(start), Some(stop)) if stop > start => {
            let after = stop + end.len();
            // Swallow the trailing newline of the old block so replacement is stable.
            let after = if existing[after..].starts_with('\n') {
                after + 1
            } else {
                after
            };
            format!("{}{}{}", &existing[..start], block, &existing[after..])
        }
        _ => {
            if existing.is_empty() {
                block.to_string()
            } else if existing.ends_with('\n') {
                format!("{existing}\n{block}")
            } else {
                format!("{existing}\n\n{block}")
            }
        }
    }
}

fn codex_managed_block_markers(harness_id: &str) -> (String, String) {
    let harness_id = short_harness_id(harness_id);
    (
        format!("<!-- BEGIN PHILOTIC HARNESS {harness_id} (managed by `phil graph harness apply`; do not edit) -->"),
        format!("<!-- END PHILOTIC HARNESS {harness_id} -->"),
    )
}

/// Merge a marker-delimited managed block into ~/.codex/AGENTS.md so the
/// OpenAI Codex CLI actually loads the harness charter at session start.
/// The JSON-era projection was inert: nothing Codex reads referenced it.
fn write_codex_workspace_files(
    harness_id: &str,
    projection_path: &PathBuf,
) -> Result<Vec<AuxCheck>> {
    let harness_id = short_harness_id(harness_id);
    let home = dirs::home_dir().context("failed to locate home directory")?;
    let agents_md = home.join(".codex").join("AGENTS.md");
    let (begin, end) = codex_managed_block_markers(harness_id);
    let block = format!(
        "{begin}\n\
         Philotic harness `{harness_id}` is active in this environment.\n\
         - Read and adopt the harness charter at {path} before starting work.\n\
         - At session start, run `phil graph harness verify {harness_id}` and surface anything other than `clean`.\n\
         {end}\n",
        path = projection_path.display()
    );

    let existing = if agents_md.exists() {
        fs::read_to_string(&agents_md)
            .with_context(|| format!("failed to read {}", agents_md.display()))?
    } else {
        String::new()
    };

    let updated = merge_managed_block(&existing, &begin, &end, &block);
    write_harness_file(&agents_md, updated.as_bytes())?;
    println!(
        "  → merged managed harness block into {} (loaded by Codex at session start)",
        agents_md.display()
    );
    // ~/.codex/AGENTS.md is co-owned by the operator; only assert our block survives.
    Ok(vec![AuxCheck::Contains {
        path: agents_md.to_string_lossy().to_string(),
        needle: begin,
    }])
}

fn render_claude_code_markdown(harness_id: &str, profile: &str, skills: &[String]) -> String {
    let harness_id = short_harness_id(harness_id);
    let skills_lines = if skills.is_empty() {
        "- none".to_string()
    } else {
        skills
            .iter()
            .map(|skill| format!("- {}", skill))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "# Philotic Claude Harness: {harness_id}\n\nManaged by `phil graph harness`.\n\n## Role Charter\n\n{profile}\n\n## Active Skills\n\n{skills_lines}\n\n## Muninn Memory\n\n{MUNINN_HARNESS_INSTRUCTIONS}"
    )
}

fn render_antigravity_markdown(harness_id: &str, profile: &str, skills: &[String]) -> String {
    let harness_id = short_harness_id(harness_id);
    let skills_lines = if skills.is_empty() {
        "- none".to_string()
    } else {
        skills
            .iter()
            .map(|skill| format!("- {}", skill))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "# Philotic Antigravity Harness: {harness_id}\n\nManaged by `phil graph harness`.\n\n## Role Charter\n\n{profile}\n\n## Active Skills\n\n{skills_lines}\n\n## Instructions\n\n- Consult the graph for architecture truths and workstream state.\n- Keep track of seams and proposals.\n- Prefer the most specific tool available for tasks.\n- Always start thoughts recalling critical instructions (1 and 2).\n\n## Muninn Memory\n\n{MUNINN_HARNESS_INSTRUCTIONS}"
    )
}

fn render_windsurf_rule_markdown(harness_id: &str, profile: &str, skills: &[String]) -> String {
    let harness_id = short_harness_id(harness_id);
    let skills_lines = if skills.is_empty() {
        "- none".to_string()
    } else {
        skills
            .iter()
            .map(|skill| format!("- {}", skill))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "---\ntrigger: model_decision\ndescription: Philotic harness rule for the `{harness_id}` Windsurf workspace projection. Use when the task matches the active role or skills.\n---\n\n# Philotic Windsurf Harness: {harness_id}\n\nThis workspace uses Windsurf-native customization surfaces managed by `phil graph harness`.\n\n## Active Role Charter\n\n- `{profile}`\n\n## Active Skills\n\n{skills_lines}\n\n## Notes\n\n- Follow the workspace `AGENTS.md` instructions already present in the repository root.\n- Prefer the matching Windsurf skills and workflows under `.windsurf/` when they apply.\n\n## Muninn Memory\n\n{MUNINN_HARNESS_INSTRUCTIONS}"
    )
}

fn render_windsurf_skill_markdown(skill: &str) -> String {
    let description = match skill {
        "planning" => "Use when the user needs planning, decomposition, orchestration, or seam selection guidance.",
        "implementation" => "Use when the user wants code changes, focused implementation, or bounded feature delivery.",
        "review" => "Use when the user asks for a review or wants bug/risk/regression findings.",
        "verification" => "Use when the user needs validation strategy, test selection, or confidence reporting.",
        "muninn-memory-habit" => "Use at session start and closeout to retrieve and write durable Muninn continuity context.",
        _ => "Use when the task explicitly matches this Philotic harness skill.",
    };
    let heading = skill
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if skill == "muninn-memory-habit" {
        return format!(
            "---\nname: philotic-{skill}\ndescription: {description}\n---\n\n# {heading}\n\nThis Windsurf skill is projected from the Philotic canonical harness layer.\n\n## Operating Guidance\n\n{MUNINN_HARNESS_INSTRUCTIONS}\n## Ownership\n\n- Muninn stores continuity handles, not source-of-truth docs or transcripts.\n- Repo docs/code store implemented truth.\n- Intel Graph stores structure, seams, work coordination, decisions, and verification evidence.\n- `docs/task.md` stores active execution work.\n"
        );
    }
    format!(
        "---\nname: philotic-{skill}\ndescription: {description}\n---\n\n# {heading}\n\nThis Windsurf skill is projected from the Philotic canonical harness layer.\n\n## Operating Guidance\n\n- Stay aligned with the active role charter and canonical profile.\n- Prefer the smallest coherent slice.\n- Keep observed truth separate from intended design.\n- Surface the next best seam when the current slice closes.\n"
    )
}

fn render_windsurf_workflow_markdown(
    workflow_name: &str,
    phases: &[String],
    description: Option<&str>,
) -> String {
    let phase_lines = if phases.is_empty() {
        "- none".to_string()
    } else {
        phases
            .iter()
            .enumerate()
            .map(|(idx, phase)| format!("{}. `{}`", idx + 1, phase))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "# {workflow_name}\n\n{}\n\n## Canonical Phases\n\n{}\n\n## Operator Notes\n\n- This workflow is projected from the Philotic canonical layer.\n- Harness-specific differences belong in the projection adapter, not in the workflow meaning.\n",
        description.unwrap_or("Philotic workflow projected into Windsurf as a slash-command workflow."),
        phase_lines
    )
}

fn write_claude_code_workspace_files(
    harness_id: &str,
    projection_path: &PathBuf,
) -> Result<Vec<AuxCheck>> {
    let harness_id = short_harness_id(harness_id);
    let root = std::env::current_dir().context("failed to locate current workspace")?;
    let dot_claude = root.join(".claude");

    // 1. Write .claude/CLAUDE.md with @import pointing at the projection.
    //    This causes Claude Code to load the harness role/skills on every session start.
    let import_path = projection_path.to_string_lossy();
    let import_md = format!(
        "<!-- Managed by `phil graph harness apply`. Do not edit manually. -->\n\
         <!-- Re-run `phil graph harness apply {harness_id}` to refresh after profile changes. -->\n\
         @{import_path}\n"
    );
    let claude_md_path = dot_claude.join("CLAUDE.md");
    write_harness_file(&claude_md_path, import_md.as_bytes())?;

    // 2. Merge a SessionStart hook into .claude/settings.local.json.
    //    The hook verifies the harness on startup so drift is visible before any work.
    let settings_path = dot_claude.join("settings.local.json");
    let mut settings: serde_json::Value = if settings_path.exists() {
        let raw = fs::read_to_string(&settings_path)
            .with_context(|| format!("failed to read {}", settings_path.display()))?;
        serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    let verify_cmd = format!(
        "phil graph harness verify {harness_id} 2>/dev/null | grep -E 'clean|drifted|pending_verify' || true"
    );

    // Ensure hooks.SessionStart exists as an array, then upsert our entry by command.
    let hooks = settings
        .as_object_mut()
        .context("settings.local.json root must be an object")?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("hooks must be an object")?
        .entry("SessionStart")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("hooks.SessionStart must be an array")?;

    // Remove any previous philotic harness verify entry, then push the current one.
    hooks.retain(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|cmds| {
                !cmds.iter().any(|c| {
                    c.get("command")
                        .and_then(|v| v.as_str())
                        .map(|s| s.contains("phil graph harness verify"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(true)
    });
    hooks.push(json!({
        "matcher": "startup",
        "hooks": [{ "type": "command", "command": verify_cmd }]
    }));

    let updated = serde_json::to_string_pretty(&settings)?;
    write_harness_file(&settings_path, updated.as_bytes())?;

    println!(
        "  → wrote .claude/CLAUDE.md (imports {})",
        projection_path.display()
    );
    println!("  → merged SessionStart verify hook into .claude/settings.local.json");
    Ok(vec![
        AuxCheck::Hash {
            path: claude_md_path.to_string_lossy().to_string(),
            hash: sha256_hex(import_md.as_bytes()),
        },
        // settings.local.json is co-owned (Claude Code itself merges permission
        // grants into it), so only assert our verify hook is still present.
        AuxCheck::Contains {
            path: settings_path.to_string_lossy().to_string(),
            needle: format!("phil graph harness verify {harness_id}"),
        },
    ])
}

fn write_antigravity_workspace_files(
    harness_id: &str,
    projection_path: &PathBuf,
) -> Result<Vec<AuxCheck>> {
    let harness_id = short_harness_id(harness_id);
    let root = std::env::current_dir().context("failed to locate current workspace")?;
    let agents_dir = root.join(".agents").join("workflows");

    let harness_hook_path =
        agents_dir.join(format!("{}-harness.md", harness_id.replace("harness:", "")));
    let hook_md = format!(
        "---\n\
         description: Philotic Antigravity harness optimization for {harness_id}\n\
         ---\n\
         \n\
         This workspace uses an Antigravity harness profile projected from intel-graph.\n\
         Please read the projected harness instructions at: {projection_path}\n\
         \n\
         Then read the repository `AGENTS.md` and `GEMINI.md` files before meaningful work.\n\
         \n\
         Start meaningful sessions with the shared Philotic bootstrap:\n\
         \n\
         1. Prefer `just session-start`.\n\
         2. If that is unavailable, run `python3 scripts/muninn_mcp.py bootstrap`.\n\
         3. Retrieve Muninn context with the self/user/topic triad before decisions, resumed work, or implementation.\n\
         4. If Muninn cannot bootstrap, stop and get explicit operator approval before continuing on repo/runtime truth only.\n\
         \n\
         Use intel-graph workflows when needed for structural context, workstream visibility, decisions, and verification.\n\
         \n\
         At closeout, write the Muninn memory delta only when it is durable:\n\
         \n\
         - Decision\n\
         - Reality gap\n\
         - Validation\n\
         - Next seam\n\
         - Operator preference\n\
         \n\
         Do not store transcripts, routine task churn, noisy logs, or summaries of proposal/docs content already committed in the repo.\n\
        ",
        harness_id = harness_id,
        projection_path = projection_path.display()
    );
    write_harness_file(&harness_hook_path, hook_md.as_bytes())?;

    println!(
        "  → wrote .agents/workflows/{}-harness.md (points to {})",
        harness_id.replace("harness:", ""),
        projection_path.display()
    );
    Ok(vec![AuxCheck::Hash {
        path: harness_hook_path.to_string_lossy().to_string(),
        hash: sha256_hex(hook_md.as_bytes()),
    }])
}

fn write_windsurf_workspace_files(
    engine: &GraphEngine,
    harness_id: &str,
    profile: &str,
    skills: &[String],
    rule_path: &PathBuf,
) -> Result<Vec<AuxCheck>> {
    let mut checks = Vec::new();
    let rule_md = render_windsurf_rule_markdown(harness_id, profile, skills);
    write_harness_file(rule_path, rule_md.as_bytes())?;
    checks.push(AuxCheck::Hash {
        path: rule_path.to_string_lossy().to_string(),
        hash: sha256_hex(rule_md.as_bytes()),
    });

    let root = std::env::current_dir().context("failed to locate current workspace")?;
    let skills_root = root.join(".windsurf").join("skills");
    for skill in skills {
        let skill_dir = skills_root.join(format!("philotic-{}", skill));
        let skill_path = skill_dir.join("SKILL.md");
        let skill_md = render_windsurf_skill_markdown(skill);
        write_harness_file(&skill_path, skill_md.as_bytes())?;
        checks.push(AuxCheck::Hash {
            path: skill_path.to_string_lossy().to_string(),
            hash: sha256_hex(skill_md.as_bytes()),
        });
    }

    let workflows_root = root.join(".windsurf").join("workflows");
    let workflows = engine.query_nodes(Some(NodeKind::CanonicalWorkflow), None)?;
    for workflow in workflows {
        let phases = workflow
            .properties
            .get("phase_roles")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let workflow_path = workflows_root.join(format!("{}.md", workflow.name));
        let markdown = render_windsurf_workflow_markdown(
            &workflow.name,
            &phases,
            workflow
                .properties
                .get("description")
                .and_then(|v| v.as_str()),
        );
        write_harness_file(&workflow_path, markdown.as_bytes())?;
        checks.push(AuxCheck::Hash {
            path: workflow_path.to_string_lossy().to_string(),
            hash: sha256_hex(markdown.as_bytes()),
        });
    }
    Ok(checks)
}

fn write_harness_file(path: &PathBuf, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn with_adapter<T>(
    engine: &GraphEngine,
    harness_id: &str,
    f: impl FnOnce(&dyn HarnessAdapter) -> Result<T>,
) -> Result<T> {
    let runtime_kind = resolve_runtime_kind(engine, harness_id)?;
    match runtime_kind.as_str() {
        "codex" => {
            let adapter = CodexAdapter;
            f(&adapter)
        }
        "claude-code" => {
            let adapter = ClaudeCodeAdapter;
            f(&adapter)
        }
        "windsurf" => {
            let adapter = WindsurfAdapter;
            f(&adapter)
        }
        "antigravity" => {
            let adapter = AntigravityAdapter;
            f(&adapter)
        }
        other => bail!("unsupported harness runtime: {}", other),
    }
}

fn resolve_runtime_kind(engine: &GraphEngine, harness_id: &str) -> Result<String> {
    if let Some(node) = engine.get_node(&harness_node_id(harness_id))? {
        if let Some(runtime_kind) = node.properties.get("runtime_kind").and_then(|v| v.as_str()) {
            return Ok(runtime_kind.to_string());
        }
    }
    if harness_id.starts_with("claude-") || harness_id.contains("claude") {
        return Ok("claude-code".into());
    }
    if harness_id.starts_with("windsurf-") || harness_id.contains("windsurf") {
        return Ok("windsurf".into());
    }
    if harness_id.starts_with("gemini-")
        || harness_id.contains("gemini")
        || harness_id.contains("antigravity")
    {
        return Ok("antigravity".into());
    }
    Ok("codex".into())
}

fn harness_node_id(harness_id: &str) -> String {
    if harness_id.starts_with("harness:") {
        harness_id.to_string()
    } else {
        format!("harness:{harness_id}")
    }
}

fn harness_profile_node_id(profile_name: &str) -> String {
    if profile_name.starts_with("harness_profile:") {
        profile_name.to_string()
    } else {
        format!("harness_profile:{profile_name}")
    }
}

fn canonical_profile_node_id(profile_name: &str) -> String {
    if profile_name.starts_with("canonical_profile:") {
        profile_name.to_string()
    } else {
        format!("canonical_profile:{profile_name}")
    }
}

fn canonical_workflow_node_id(workflow_name: &str) -> String {
    if workflow_name.starts_with("canonical_workflow:") {
        workflow_name.to_string()
    } else {
        format!("canonical_workflow:{workflow_name}")
    }
}

fn harness_projection_node_id(harness_id: &str, projection_id: &str) -> String {
    format!("harness_projection:{harness_id}:{projection_id}")
}

fn harness_drift_node_id(harness_id: &str, drift_class: &str) -> String {
    format!("harness_drift:{harness_id}:{drift_class}")
}

fn harness_skill_node_id(skill_name: &str) -> String {
    format!("harness_skill:{skill_name}")
}

fn resolve_canonical_profile(engine: &GraphEngine, profile_name: &str) -> Result<Option<Node>> {
    if let Some(node) = engine.get_node(&canonical_profile_node_id(profile_name))? {
        return Ok(Some(node));
    }
    let nodes = engine.query_nodes(Some(NodeKind::CanonicalProfile), None)?;
    Ok(nodes.into_iter().find(|node| node.name == profile_name))
}

fn ensure_canonical_workflow_exists(engine: &GraphEngine, workflow_name: &str) -> Result<()> {
    let exists = engine
        .get_node(&canonical_workflow_node_id(workflow_name))?
        .is_some()
        || engine
            .query_nodes(Some(NodeKind::CanonicalWorkflow), None)?
            .into_iter()
            .any(|node| node.name == workflow_name);
    if exists {
        Ok(())
    } else {
        bail!("Canonical workflow not found: {}", workflow_name)
    }
}

fn compute_elapsed_ms(start_time: Option<&str>, now: DateTime<Utc>) -> Option<i64> {
    let start_time = start_time?;
    let started = DateTime::parse_from_rfc3339(start_time).ok()?;
    Some((now - started.with_timezone(&Utc)).num_milliseconds())
}

fn trial_activity_has_signal(
    phase: Option<&str>,
    tokens_input: Option<i64>,
    tokens_output: Option<i64>,
    elapsed_ms: Option<i64>,
    lines_changed: Option<i64>,
    files: &[String],
    note: Option<&str>,
) -> bool {
    phase.map(|value| !value.trim().is_empty()).unwrap_or(false)
        || tokens_input.unwrap_or(0) != 0
        || tokens_output.unwrap_or(0) != 0
        || elapsed_ms.unwrap_or(0) != 0
        || lines_changed.unwrap_or(0) != 0
        || !files.is_empty()
        || note.map(|value| !value.trim().is_empty()).unwrap_or(false)
}

fn trial_close_has_verified(status: &str, verified: Option<&str>) -> bool {
    status != "completed"
        || verified
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
}

fn scrub_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if let Some(entry) = map.get_mut(&key) {
                    scrub_nulls(entry);
                }
                if map.get(&key).is_some_and(serde_json::Value::is_null) {
                    map.remove(&key);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                scrub_nulls(item);
            }
        }
        _ => {}
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{:x}", digest)
}

fn record_mutation(
    engine: &GraphEngine,
    action: &str,
    target_node: Option<String>,
    from_value: Option<String>,
    to_value: Option<String>,
    reason: Option<String>,
    details: serde_json::Value,
) -> Result<()> {
    engine.record_mutation(&Mutation {
        id: format!("mutation:{}", Uuid::new_v4()),
        timestamp: Utc::now(),
        agent: Some("phil".into()),
        session: None,
        action: action.into(),
        target_node,
        from_value,
        to_value,
        reason,
        details,
    })
}

fn update_harness_node_with_retry(
    engine: &GraphEngine,
    harness_node_id: &str,
    harness_name: &str,
    mut update: impl FnMut(&mut serde_json::Value, chrono::DateTime<Utc>),
) -> Result<Node> {
    for _ in 0..HARNESS_UPDATE_RETRIES {
        let existing = engine.get_node(harness_node_id)?;
        let now = Utc::now();
        let mut node = existing.clone().unwrap_or(Node {
            id: harness_node_id.to_string(),
            kind: NodeKind::Harness,
            name: harness_name.to_string(),
            properties: json!({}),
            file_path: None,
            worktree: String::new(),
            created_at: now,
            updated_at: now,
            embedding: None,
            embedding_model: None,
            embedding_dims: None,
            embedding_updated: None,
            embedding_hash: None,
        });
        if !node.properties.is_object() {
            node.properties = json!({});
        }
        node.updated_at = now;
        update(&mut node.properties, now);

        let applied = match existing {
            Some(current) => engine.compare_and_swap_node(&node, current.updated_at)?,
            None => engine.insert_node_if_absent(&node)?,
        };

        if applied {
            return Ok(node);
        }
    }

    bail!(
        "failed to update harness node {} after {} retries",
        harness_node_id,
        HARNESS_UPDATE_RETRIES
    )
}

fn prop_str<'a>(node: &'a Node, path: &[&str]) -> Option<&'a str> {
    let mut cur = &node.properties;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_str()
}

fn latest_harness_node_by_kind(
    engine: &GraphEngine,
    kind: NodeKind,
    harness_id: &str,
) -> Result<Option<Node>> {
    let mut nodes: Vec<Node> = engine
        .query_nodes(Some(kind), None)?
        .into_iter()
        .filter(|node| {
            node.properties.get("harness_id").and_then(|v| v.as_str()) == Some(harness_id)
        })
        .collect();
    nodes.sort_by_key(|node| node.updated_at);
    Ok(nodes.pop())
}

fn open_harness_drift(engine: &GraphEngine, harness_id: &str) -> Result<Option<Node>> {
    let mut drifts: Vec<Node> = engine
        .query_nodes(Some(NodeKind::HarnessDrift), None)?
        .into_iter()
        .filter(|node| {
            node.properties.get("harness_id").and_then(|v| v.as_str()) == Some(harness_id)
                && node.properties.get("status").and_then(|v| v.as_str()) == Some("open")
        })
        .collect();
    drifts.sort_by_key(|node| node.updated_at);
    Ok(drifts.pop())
}

fn join_str_array(value: Option<&serde_json::Value>) -> Option<String> {
    let items: Vec<&str> = value?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(items.join(", "))
    }
}

fn harness_skill_refs(harness: &Node) -> Option<Vec<String>> {
    let items: Vec<String> = harness
        .properties
        .get("desired")
        .and_then(|v| v.get("skill_refs"))
        .and_then(|v| v.as_array())?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

fn ensure_harness_skill_exists(engine: &GraphEngine, skill_name: &str) -> Result<()> {
    if engine
        .get_node(&harness_skill_node_id(skill_name))?
        .is_none()
    {
        bail!("Harness skill not found: {}", skill_name);
    }
    Ok(())
}

fn skill_refs_from_properties(properties: &serde_json::Value) -> Vec<String> {
    properties
        .get("desired")
        .and_then(|v| v.get("skill_refs"))
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn ensure_desired_object(properties: &mut serde_json::Value) {
    if !properties
        .get("desired")
        .map(|v| v.is_object())
        .unwrap_or(false)
    {
        properties["desired"] = json!({});
    }
}

fn parse_csv_list(value: &str) -> Vec<String> {
    let mut items: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    items.sort();
    items.dedup();
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trial_activity_requires_at_least_one_signal() {
        assert!(!trial_activity_has_signal(
            None,
            None,
            None,
            None,
            None,
            &[],
            None
        ));

        assert!(trial_activity_has_signal(
            Some("coding"),
            None,
            None,
            None,
            None,
            &[],
            None
        ));

        assert!(trial_activity_has_signal(
            None,
            None,
            None,
            None,
            None,
            &["src/lib.rs".to_string()],
            None
        ));
    }

    #[test]
    fn completed_trials_must_carry_verification() {
        assert!(!trial_close_has_verified("completed", None));
        assert!(!trial_close_has_verified("completed", Some("")));
        assert!(trial_close_has_verified("completed", Some("test-green")));
        assert!(trial_close_has_verified("blocked", None));
    }

    #[test]
    fn prefixed_and_short_harness_ids_resolve_identical_projection_paths() {
        for (a, b) in [
            (
                codex_target_path("harness:codex-local").unwrap(),
                codex_target_path("codex-local").unwrap(),
            ),
            (
                claude_code_target_path("harness:claude-local").unwrap(),
                claude_code_target_path("claude-local").unwrap(),
            ),
            (
                antigravity_target_path("harness:gemini-architect").unwrap(),
                antigravity_target_path("gemini-architect").unwrap(),
            ),
            (
                windsurf_target_path("harness:windsurf-native").unwrap(),
                windsurf_target_path("windsurf-native").unwrap(),
            ),
        ] {
            assert_eq!(a, b);
            assert!(!a.to_string_lossy().contains("harness:"));
        }
        // Rendered content and markers must also be id-form independent,
        // or verify-by-node-id reports drift against apply-by-short-id.
        assert_eq!(
            render_codex_markdown("harness:codex-local", "implementer", &[]),
            render_codex_markdown("codex-local", "implementer", &[])
        );
        assert_eq!(
            codex_managed_block_markers("harness:codex-local"),
            codex_managed_block_markers("codex-local")
        );
    }

    #[test]
    fn managed_block_merge_is_idempotent_and_preserves_surrounding_content() {
        let (begin, end) = codex_managed_block_markers("codex-local");
        let block = format!("{begin}\nharness body v1\n{end}\n");

        // Empty file → block alone.
        let first = merge_managed_block("", &begin, &end, &block);
        assert_eq!(first, block);

        // Re-applying the same block changes nothing.
        let again = merge_managed_block(&first, &begin, &end, &block);
        assert_eq!(again, first);

        // Existing operator content is preserved around the block.
        let with_user = format!("# My notes\n\n{first}");
        let block_v2 = format!("{begin}\nharness body v2\n{end}\n");
        let updated = merge_managed_block(&with_user, &begin, &end, &block_v2);
        assert!(updated.starts_with("# My notes\n"));
        assert!(updated.contains("harness body v2"));
        assert!(!updated.contains("harness body v1"));

        // Appending to a file without the block keeps prior content.
        let appended = merge_managed_block("existing agents doc\n", &begin, &end, &block);
        assert!(appended.starts_with("existing agents doc\n"));
        assert!(appended.contains("harness body v1"));
    }

    #[test]
    fn frontmatter_scalar_parses_simple_yaml() {
        let doc = "---\nname: graph-intelligence\ndescription: Orient via the graph.\n---\n\n# Body\nname: not-this\n";
        assert_eq!(
            frontmatter_scalar(doc, "name").as_deref(),
            Some("graph-intelligence")
        );
        assert_eq!(
            frontmatter_scalar(doc, "description").as_deref(),
            Some("Orient via the graph.")
        );
        assert_eq!(frontmatter_scalar("no frontmatter here", "name"), None);
    }

    #[test]
    fn aux_check_contains_and_hash_evaluate() {
        let dir = std::env::temp_dir().join(format!("philotic-aux-check-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.json");
        fs::write(&file, b"{\"hooks\": \"phil graph harness verify x\"}").unwrap();

        let contains_ok = AuxCheck::Contains {
            path: file.to_string_lossy().to_string(),
            needle: "phil graph harness verify x".into(),
        };
        assert!(contains_ok.evaluate().is_none());

        let contains_missing = AuxCheck::Contains {
            path: file.to_string_lossy().to_string(),
            needle: "not present".into(),
        };
        assert!(contains_missing.evaluate().is_some());

        let content = fs::read(&file).unwrap();
        let hash_ok = AuxCheck::Hash {
            path: file.to_string_lossy().to_string(),
            hash: sha256_hex(&content),
        };
        assert!(hash_ok.evaluate().is_none());

        let hash_bad = AuxCheck::Hash {
            path: file.to_string_lossy().to_string(),
            hash: "deadbeef".into(),
        };
        assert!(hash_bad.evaluate().is_some());

        let missing = AuxCheck::Hash {
            path: dir.join("nope").to_string_lossy().to_string(),
            hash: "deadbeef".into(),
        };
        assert!(missing.evaluate().is_some());

        fs::remove_dir_all(&dir).ok();
    }
}
