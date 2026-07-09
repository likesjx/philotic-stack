//! `phil explain` — decision-chain diagnostics for agent-facing actions.
//!
//! Slice 1 implements only `explain exec`: it evaluates a command through the
//! exec-guard crate's public API (the L0 compiled-in hardline floor shipped
//! in PR #186) and prints which layer decided, the matched pattern/reason
//! (if any), and operator-facing guidance. `explain route` and `explain
//! lease` are stubs — both need a trace ledger that doesn't exist yet and
//! are explicitly out of scope for this slice.

use anyhow::Result;
use clap::Subcommand;
use serde::Serialize;
use serde_json::json;

#[derive(Subcommand, Debug)]
pub enum ExplainAction {
    /// Explain how a shell command would be evaluated by the execution
    /// safety floor: which layer decides, what matched, and why.
    Exec {
        /// The command exactly as an agent would submit it for execution.
        command: String,

        /// Emit machine-readable JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Explain how a turn was routed to an agent (requires a trace ledger —
    /// not implemented in slice 1).
    Route {
        /// Agent/persona name to explain routing for.
        agent: String,

        /// Emit machine-readable JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Explain a membrane lease's current state (requires a trace ledger —
    /// not implemented in slice 1).
    Lease {
        /// Membrane name to explain the lease for.
        membrane: String,

        /// Emit machine-readable JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(action: ExplainAction) -> Result<()> {
    match action {
        ExplainAction::Exec { command, json } => explain_exec(&command, json),
        ExplainAction::Route { agent, json } => explain_stub("route", &agent, json),
        ExplainAction::Lease { membrane, json } => explain_stub("lease", &membrane, json),
    }
}

// ── explain exec ────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ExecDecision {
    command: String,
    normalized: String,
    layer: &'static str,
    /// "block" or "allow-through-floor" — the latter is deliberately not
    /// named "allow" or "safe": L0 having no opinion is not an approval,
    /// since L1-L4 (not implemented yet) still apply above it.
    decision: &'static str,
    matched_pattern: Option<&'static str>,
    reason: Option<&'static str>,
    guidance: String,
}

/// Pure decision-building step, kept separate from printing so it can be
/// exercised directly in tests without capturing stdout.
fn build_exec_decision(command: &str) -> ExecDecision {
    let normalized = exec_guard::normalize_command(command);
    match exec_guard::detect_hardline(command) {
        Some(m) => ExecDecision {
            command: command.to_string(),
            normalized,
            layer: "L0 (compiled-in hardline floor)",
            decision: "block",
            matched_pattern: Some(m.description),
            reason: Some(m.description),
            guidance: m.denial_message(),
        },
        None => ExecDecision {
            command: command.to_string(),
            normalized,
            layer: "L0 (compiled-in hardline floor)",
            decision: "allow-through-floor",
            matched_pattern: None,
            reason: None,
            guidance: "no L0 hardline rule matched. This is not an approval — it only means \
                       the compiled-in floor has no opinion on this command. L1 (operator deny \
                       globs), L2 (allowlist + approval modes), and L3 (approval context \
                       binding) are later slices and are not implemented yet, so no higher \
                       layer can block or approve this command either right now."
                .to_string(),
        },
    }
}

fn explain_exec(command: &str, json_out: bool) -> Result<()> {
    let decision = build_exec_decision(command);

    if json_out {
        println!("{}", serde_json::to_string_pretty(&decision)?);
        return Ok(());
    }

    println!("phil explain exec — command: {}", decision.command);
    println!("  normalized: {}", decision.normalized);
    println!("  layer:      {}", decision.layer);
    println!("  decision:   {}", decision.decision.to_uppercase());
    if let Some(pattern) = decision.matched_pattern {
        println!("  matched:    {pattern}");
    }
    println!("  guidance:   {}", decision.guidance);
    Ok(())
}

// ── explain route / lease (stubs) ───────────────────────────────────────

fn explain_stub(kind: &str, target: &str, json_out: bool) -> Result<()> {
    let message = format!(
        "phil explain {kind} {target} — requires trace ledger — not in slice 1 \
         (planned for a later slice of PHIL_DOCTOR_EXPLAIN_PROPOSAL.md)"
    );

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "kind": kind,
                "target": target,
                "status": "not_implemented",
                "message": message,
            }))?
        );
    } else {
        println!("{message}");
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardline_command_prints_block_decision() {
        let decision = build_exec_decision("rm -rf /");
        assert_eq!(decision.decision, "block");
        assert_eq!(
            decision.matched_pattern,
            Some("recursive delete of root filesystem")
        );
        assert!(decision.guidance.contains("Do not retry"));
    }

    #[test]
    fn safe_command_is_allowed_through_the_floor() {
        let decision = build_exec_decision("ls -la");
        assert_eq!(decision.decision, "allow-through-floor");
        assert!(decision.matched_pattern.is_none());
        // Must not claim the command is "safe" or "approved" — L0 having no
        // opinion is not an approval; higher layers still apply.
        assert!(!decision.guidance.to_lowercase().contains("safe"));
        assert!(!decision.guidance.to_lowercase().contains("approved"));
        assert!(decision.guidance.contains("not an approval"));
    }

    #[test]
    fn another_hardline_variant_is_also_blocked() {
        let decision = build_exec_decision("sudo rm -rf /*");
        assert_eq!(decision.decision, "block");
        assert!(decision.matched_pattern.is_some());
    }

    #[test]
    fn explain_exec_runs_without_error_json_and_human() {
        explain_exec("rm -rf /", true).expect("json block must not error");
        explain_exec("rm -rf /", false).expect("human block must not error");
        explain_exec("ls -la", true).expect("json allow must not error");
        explain_exec("ls -la", false).expect("human allow must not error");
    }

    #[test]
    fn explain_stub_reports_not_in_slice_1() {
        explain_stub("route", "astrid", true).expect("json stub must not error");
        explain_stub("lease", "membrane-telegram", false).expect("human stub must not error");
    }
}
