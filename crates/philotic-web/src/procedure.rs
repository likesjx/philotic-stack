//! `phil procedure` — operator surface for procedural graphs
//! (doc:procedural-graphs, slice P0/P1).
//!
//! - `list`: every procedure with version, validation state, provenance,
//!   node/edge counts, and the skill it rides on.
//! - `show <id>`: the graph as the paper's triplet list plus its backbone.
//! - `runs <id>`: the newest terminal plan evaluations attributed to it —
//!   the refiner's evidence and the trial gate's score source.
//! - `patches [--procedure <id>] [--status <s>]`: refiner patches and where
//!   the gate left them (pending / trial / accepted / rejected).
//! - `approve <patch_id>` / `reject <patch_id> [--reason ..]`: the operator
//!   decision that opens a live trial window, or keeps the patch as negative
//!   evidence.
//!
//! Everything reads the daemon's live GraphDomain over IPC, like `phil
//! autonomy`; nothing here re-derives state from a raw DB read.

use ansible_mesh_core::procedure::{
    ProcedureGraphRecord, ProcedurePatchRecord, ProcedureRunRecord,
};
use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::Value;

use crate::start::socket_path;

#[derive(Subcommand, Debug)]
pub enum ProcedureAction {
    /// List every procedural graph on this hotel.
    List,

    /// Show one procedure: triplets with condition/guidance/pitfalls, and the
    /// linear backbone a seeded plan would follow.
    Show {
        /// The `procedure_id` printed by `phil procedure list`.
        procedure_id: String,
    },

    /// Newest-first run ledger for one procedure.
    Runs {
        procedure_id: String,
        /// Only runs recorded against this graph version.
        #[arg(long)]
        version: Option<u32>,
        /// Maximum rows (default 20, max 200).
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },

    /// Refiner patches, newest first, with their gate status.
    Patches {
        /// Only patches against this procedure.
        #[arg(long)]
        procedure: Option<String>,
        /// One of pending | trial | accepted | rejected.
        #[arg(long)]
        status: Option<String>,
    },

    /// Approve a pending patch: applies it as a candidate version that runs
    /// a live trial and is accepted only if it scores at least as well as
    /// the version it replaces.
    Approve { patch_id: String },

    /// Reject a pending patch. It is kept as negative evidence for the
    /// refiner, never deleted.
    Reject {
        patch_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
}

pub async fn run(action: ProcedureAction) -> Result<()> {
    match action {
        ProcedureAction::List => list().await,
        ProcedureAction::Show { procedure_id } => show(procedure_id).await,
        ProcedureAction::Runs {
            procedure_id,
            version,
            limit,
        } => runs(procedure_id, version, limit).await,
        ProcedureAction::Patches { procedure, status } => patches(procedure, status).await,
        ProcedureAction::Approve { patch_id } => decide(patch_id, "approve", None).await,
        ProcedureAction::Reject { patch_id, reason } => decide(patch_id, "reject", reason).await,
    }
}

async fn ipc_client() -> Result<PhiloticClient> {
    let socket = socket_path("aiua");
    let identity = GuestIdentity {
        guest_id: "phil-procedure".into(),
        role: "management".into(),
        supported_tools: vec![],
    };
    PhiloticClient::connect_at(&socket, identity)
        .await
        .with_context(|| format!("connect to aiua at {socket}"))
}

fn expect_data(resp: IpcResponse, what: &str) -> Result<Value> {
    match resp {
        IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        } => Ok(data),
        IpcResponse::Standard { ok: true, .. } => Ok(Value::Null),
        IpcResponse::Standard { code, message, .. } => Err(anyhow!("{what}: {code}: {message}")),
        other => Err(anyhow!("{what}: unexpected response {other:?}")),
    }
}

fn state_label(p: &ProcedureGraphRecord) -> String {
    match &p.validation_state {
        ansible_mesh_core::graph::SkillValidationState::Suspended { reason } => {
            format!("suspended ({reason})")
        }
        ansible_mesh_core::graph::SkillValidationState::Invalid { errors } => {
            format!("invalid ({})", errors.len())
        }
        other => format!("{other:?}").to_lowercase(),
    }
}

fn provenance_label(p: &ProcedureGraphRecord) -> String {
    match &p.provenance {
        ansible_mesh_core::procedure::ProcedureProvenance::Agent { agent_id } => {
            format!("agent:{agent_id}")
        }
        other => format!("{other:?}").to_lowercase(),
    }
}

async fn list() -> Result<()> {
    let mut client = ipc_client().await?;
    let data = expect_data(
        client.send_request(IpcRequest::ListProcedures {}).await?,
        "list_procedures",
    )?;
    let procedures: Vec<ProcedureGraphRecord> = data
        .get("procedures")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    if procedures.is_empty() {
        println!("no procedures registered on this hotel");
        return Ok(());
    }
    println!(
        "{:<32} {:>3}  {:<12} {:<22} {:>5} {:>5}  {}",
        "procedure_id", "ver", "state", "provenance", "nodes", "edges", "skill / trigger"
    );
    for p in &procedures {
        let binding = match (&p.skill_name, &p.trigger) {
            (Some(s), Some(t)) => format!("{s} / {t}"),
            (Some(s), None) => s.clone(),
            (None, Some(t)) => format!("- / {t}"),
            (None, None) => "-".into(),
        };
        let trial = p
            .trial_of
            .as_deref()
            .map(|id| format!(" [trial of {id}]"))
            .unwrap_or_default();
        println!(
            "{:<32} {:>3}  {:<12} {:<22} {:>5} {:>5}  {}{}",
            p.procedure_id,
            p.version,
            state_label(p),
            provenance_label(p),
            p.nodes.len(),
            p.edges.len(),
            binding,
            trial
        );
    }
    Ok(())
}

async fn show(procedure_id: String) -> Result<()> {
    let mut client = ipc_client().await?;
    let data = expect_data(
        client
            .send_request(IpcRequest::GetProcedure {
                procedure_id: procedure_id.clone(),
            })
            .await?,
        "get_procedure",
    )?;
    let p: ProcedureGraphRecord =
        serde_json::from_value(data).with_context(|| format!("decode procedure {procedure_id}"))?;
    println!("{} v{} — {}", p.procedure_id, p.version, state_label(&p));
    println!("provenance: {}", provenance_label(&p));
    if let Some(skill) = &p.skill_name {
        println!("skill: {skill}");
    }
    if let Some(trigger) = &p.trigger {
        println!("trigger: {trigger}");
    }
    if let Some(patch) = &p.trial_of {
        println!("trial of patch: {patch}");
    }
    println!("\n{}\n", p.description);
    println!("nodes ({}):", p.nodes.len());
    for n in &p.nodes {
        let tool = n
            .tool_name
            .as_deref()
            .map(|t| format!(" [{t}]"))
            .unwrap_or_default();
        println!("  {:<16} {:?}{} — {}", n.id, n.kind, tool, n.label);
    }
    println!("\ntriplets ({}):", p.edges.len());
    print!("{}", p.render_triplets());
    let backbone: Vec<&str> = p.linear_backbone().iter().map(|n| n.id.as_str()).collect();
    println!("\nbackbone from `{}`: {}", p.entry, backbone.join(" → "));
    if let Err(errors) = p.validate() {
        println!("\nVALIDATION ERRORS:");
        for e in errors {
            println!("  - {e}");
        }
    }
    Ok(())
}

async fn runs(procedure_id: String, version: Option<u32>, limit: usize) -> Result<()> {
    let mut client = ipc_client().await?;
    let data = expect_data(
        client
            .send_request(IpcRequest::ListProcedureRuns {
                procedure_id: procedure_id.clone(),
                graph_version: version,
                limit: Some(limit),
            })
            .await?,
        "list_procedure_runs",
    )?;
    let runs: Vec<ProcedureRunRecord> = data
        .get("runs")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    if runs.is_empty() {
        println!("no runs recorded for {procedure_id}");
        return Ok(());
    }
    println!(
        "{:<12} {:>3} {:<9} {:<15} {:>5} {:>7} {:>6}  {:<22} tools",
        "recorded", "ver", "verdict", "basis", "score", "steps", "stalls", "agent"
    );
    for r in &runs {
        println!(
            "{:<12} {:>3} {:<9} {:<15} {:>5.2} {:>3}/{:<3} {:>6}  {:<22} {}",
            r.recorded_at,
            r.graph_version,
            r.verdict,
            r.basis,
            r.score,
            r.steps_verified,
            r.steps_total,
            r.stalls,
            r.agent_id,
            r.tool_sequence.join(" → ")
        );
    }
    let n = runs.len() as f32;
    let mean = runs.iter().map(|r| r.score).sum::<f32>() / n;
    println!("\n{} run(s), mean score {:.2}", runs.len(), mean);
    Ok(())
}

async fn patches(procedure: Option<String>, status: Option<String>) -> Result<()> {
    let mut client = ipc_client().await?;
    let data = expect_data(
        client
            .send_request(IpcRequest::ListProcedurePatches {
                procedure_id: procedure,
                status,
            })
            .await?,
        "list_procedure_patches",
    )?;
    let patches: Vec<ProcedurePatchRecord> = data
        .get("patches")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    if patches.is_empty() {
        println!("no procedure patches");
        return Ok(());
    }
    for p in &patches {
        let trial = p
            .trial
            .as_ref()
            .map(|t| {
                format!(
                    " trial v{}: candidate {:.2}/{} vs baseline {:.2}/{} (needs {})",
                    t.candidate_version,
                    t.candidate_mean,
                    t.candidate_n,
                    t.baseline_mean,
                    t.baseline_n,
                    t.required_runs
                )
            })
            .unwrap_or_default();
        println!(
            "{}  {:<9} {} v{}→{}  by {}  {}",
            p.patch_id,
            p.status.as_str(),
            p.procedure_id,
            p.base_version,
            p.candidate_version
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".into()),
            p.proposed_by,
            p.created_at
        );
        println!("    ops: {}", p.summary());
        if !p.rationale.is_empty() {
            println!("    why: {}", p.rationale);
        }
        if let Some(reason) = &p.rejection_reason {
            println!("    rejected: {reason}");
        }
        if !trial.is_empty() {
            println!("   {trial}");
        }
    }
    Ok(())
}

async fn decide(patch_id: String, decision: &str, reason: Option<String>) -> Result<()> {
    let mut client = ipc_client().await?;
    let data = expect_data(
        client
            .send_request(IpcRequest::DecideProcedurePatch {
                patch_id: patch_id.clone(),
                decision: decision.to_string(),
                reason,
            })
            .await?,
        "decide_procedure_patch",
    )?;
    let status = data.get("status").and_then(Value::as_str).unwrap_or("?");
    match status {
        "trial" => println!(
            "patch {patch_id} approved: {} is now v{} on trial for {} run(s)",
            data.get("procedure_id")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            data.get("candidate_version")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            data.get("required_runs")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ),
        other => println!("patch {patch_id}: {other}"),
    }
    Ok(())
}
