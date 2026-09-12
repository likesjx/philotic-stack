//! Procedural graphs — plan attribution and the run ledger row
//! (doc:procedural-graphs, slice P1 `procedure-run-ledger`).
//!
//! The paper's self-evolution loop needs one thing Philotic threw away: a
//! per-episode score attached to the procedure that produced it.
//! [`crate::plan_eval::PlanEvalOutcome`] already carries the grounded verdict;
//! this module attributes it to a procedure and shapes the ledger row. Only a
//! **terminal** eval (complete, blocked, or stopped) ever becomes a row —
//! a `Continue` is mid-flight, not evidence.
//!
//! Attribution is deliberately conservative: a plan the harness seeded from a
//! procedure carries `ActivePlan::procedure_id` and resolves by id; any other
//! plan resolves only when its declared tools overlap a bound procedure's
//! tools by Jaccard ≥ [`MIN_PROCEDURE_TOOL_OVERLAP`]. A plan that matches
//! nothing produces no row, so the ledger never scores a procedure for work
//! it did not shape.

use std::collections::BTreeSet;

use ansible_mesh_core::procedure::{ProcedureGraphRecord, ProcedureRunRecord};
use uuid::Uuid;

use crate::plan_eval::PlanEvalOutcome;
use crate::session::{ActivePlan, WorkingTurn};

/// Minimum Jaccard overlap between a plan's declared tools and a procedure's
/// tool nodes for an un-stamped plan to be attributed to that procedure.
pub const MIN_PROCEDURE_TOOL_OVERLAP: f32 = 0.5;
/// Plan goal excerpt kept on a ledger row.
pub const RUN_GOAL_MAX_CHARS: usize = 200;

/// How a plan's lifetime ended, as the ledger records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTerminal {
    /// Every step settled (grounded or model-reported — `basis` says which).
    Complete,
    /// The eval blocked it: a failed step or a stall past the ceiling.
    Blocked,
    /// The loop stopped it: budget or lifetime cap exhausted, or continuation
    /// disabled with work outstanding.
    Stopped,
}

impl RunTerminal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Blocked => "blocked",
            Self::Stopped => "stopped",
        }
    }
}

/// Distinct tool names a plan declares on its steps, in first-seen order.
pub fn plan_tool_names(plan: &ActivePlan) -> Vec<String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    plan.steps
        .iter()
        .filter_map(|s| s.tool_name.as_deref())
        .filter(|t| !t.is_empty())
        .filter(|t| seen.insert(t))
        .map(str::to_string)
        .collect()
}

/// The procedure a plan belongs to: its stamped id when the harness seeded
/// it, otherwise the best tool-overlap match at or above the threshold.
/// Ties break on the lexically smaller id so attribution is deterministic.
pub fn resolve_procedure_for_plan<'a>(
    procedures: &'a [ProcedureGraphRecord],
    plan: &ActivePlan,
) -> Option<&'a ProcedureGraphRecord> {
    if let Some(id) = plan.procedure_id.as_deref() {
        return procedures.iter().find(|p| p.procedure_id == id);
    }
    let tools = plan_tool_names(plan);
    if tools.is_empty() {
        return None;
    }
    procedures
        .iter()
        .map(|p| (p, p.tool_overlap(&tools)))
        .filter(|(_, score)| *score >= MIN_PROCEDURE_TOOL_OVERLAP)
        .max_by(|(a, sa), (b, sb)| {
            sa.partial_cmp(sb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.procedure_id.cmp(&a.procedure_id))
        })
        .map(|(p, _)| p)
}

/// Shape the ledger row for a terminal plan eval, or `None` when the plan is
/// not attributable to any bound procedure. `recorded_at` is left at zero for
/// the hotel to stamp; `score` is derived here and re-derived there.
pub fn build_procedure_run(
    procedures: &[ProcedureGraphRecord],
    agent_id: &str,
    session_id: &str,
    turn: &WorkingTurn,
    plan: &ActivePlan,
    outcome: &PlanEvalOutcome,
    terminal: RunTerminal,
) -> Option<ProcedureRunRecord> {
    let procedure = resolve_procedure_for_plan(procedures, plan)?;
    let verdict = terminal.as_str();
    let basis = outcome.basis.as_str();
    let mut goal: String = plan.goal.chars().take(RUN_GOAL_MAX_CHARS).collect();
    if plan.goal.chars().count() > RUN_GOAL_MAX_CHARS {
        goal.push('…');
    }
    Some(ProcedureRunRecord {
        run_id: Uuid::new_v4().to_string(),
        procedure_id: procedure.procedure_id.clone(),
        graph_version: procedure.version,
        agent_id: agent_id.to_string(),
        session_id: session_id.to_string(),
        turn_id: turn.turn_id.clone(),
        goal,
        tool_sequence: turn
            .working_tool_history
            .iter()
            .map(|(call, _)| call.tool_name.clone())
            .collect(),
        verdict: verdict.to_string(),
        basis: basis.to_string(),
        steps_total: outcome.steps_total,
        steps_verified: outcome.steps_verified,
        steps_done: outcome.steps_done,
        stalls: outcome.stalled_continuations,
        non_atomic: outcome.non_atomic_step_ids.len(),
        contradicted: outcome.contradicted_step_ids.len(),
        guidance_rendered: turn.procedure_guidance_rendered,
        score: ProcedureRunRecord::score_for(verdict, basis),
        recorded_at: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::r#loop::{ToolCall, ToolResult};
    use crate::plan_eval::{PlanEvalBasis, PlanEvalVerdict};
    use crate::session::PlanStep;
    use ansible_mesh_core::procedure::{
        ProcedureNode, ProcedureNodeKind, outcome_reflex_procedure,
    };

    fn plan(goal: &str, tools: &[Option<&str>]) -> ActivePlan {
        ActivePlan {
            goal: goal.into(),
            status: "executing".into(),
            steps: tools
                .iter()
                .enumerate()
                .map(|(i, t)| PlanStep {
                    id: i as u32 + 1,
                    description: format!("step {}", i + 1),
                    tool_name: t.map(str::to_string),
                    status: "pending".into(),
                })
                .collect(),
            context_1_advisory: None,
            procedure_id: None,
        }
    }

    fn chain(id: &str, tools: &[&str]) -> ProcedureGraphRecord {
        ProcedureGraphRecord {
            procedure_id: id.into(),
            description: "test".into(),
            entry: "n0".into(),
            nodes: tools
                .iter()
                .enumerate()
                .map(|(i, t)| ProcedureNode {
                    id: format!("n{i}"),
                    label: format!("do {t}"),
                    kind: ProcedureNodeKind::Tool,
                    tool_name: Some((*t).to_string()),
                })
                .collect(),
            version: 3,
            ..Default::default()
        }
    }

    fn outcome(verdict: PlanEvalVerdict, basis: PlanEvalBasis) -> PlanEvalOutcome {
        PlanEvalOutcome {
            steps_total: 3,
            steps_done: 2,
            steps_verified: 2,
            steps_done_flags: vec![true, true, false],
            verified_step_ids: vec![1, 2],
            uncertain_step_ids: vec![],
            contradicted_step_ids: vec![3],
            non_atomic_step_ids: vec![],
            outstanding_step_ids: vec![3],
            stalled_continuations: 1,
            verdict,
            basis,
        }
    }

    fn turn(history: &[&str]) -> WorkingTurn {
        let mut turn = WorkingTurn::for_plan_tests();
        turn.turn_id = "turn-7".into();
        turn.working_tool_history = history
            .iter()
            .map(|t| {
                (
                    ToolCall {
                        tool_name: (*t).to_string(),
                        arguments: serde_json::json!({}),
                    },
                    ToolResult {
                        tool_name: (*t).to_string(),
                        content: "ok".into(),
                    },
                )
            })
            .collect();
        turn
    }

    #[test]
    fn stamped_plan_resolves_by_id_regardless_of_tools() {
        let procedures = vec![
            outcome_reflex_procedure(),
            chain("web.digest", &["web.fetch"]),
        ];
        let mut p = plan("x", &[Some("web.fetch")]);
        p.procedure_id = Some("outcome-reflex".into());
        assert_eq!(
            resolve_procedure_for_plan(&procedures, &p).map(|r| r.procedure_id.as_str()),
            Some("outcome-reflex")
        );
        // A stamped id that is not bound resolves to nothing — never a fallback.
        p.procedure_id = Some("gone".into());
        assert!(resolve_procedure_for_plan(&procedures, &p).is_none());
    }

    #[test]
    fn unstamped_plan_resolves_by_tool_overlap_above_threshold() {
        let procedures = vec![
            outcome_reflex_procedure(),
            chain("web.digest", &["web.fetch", "memory.remember"]),
        ];
        // {observe, commit} vs {recall, observe, commit} = 2/3 ≥ 0.5.
        let p = plan("x", &[Some("life.observe"), Some("life.commit"), None]);
        assert_eq!(
            resolve_procedure_for_plan(&procedures, &p).map(|r| r.procedure_id.as_str()),
            Some("outcome-reflex")
        );
        // {observe} vs {recall, observe, commit} = 1/3 < 0.5.
        let p = plan("x", &[Some("life.observe")]);
        assert!(resolve_procedure_for_plan(&procedures, &p).is_none());
        // No declared tools: nothing to attribute.
        let p = plan("x", &[None, None]);
        assert!(resolve_procedure_for_plan(&procedures, &p).is_none());
        // Ties break on the lexically smaller id.
        let procedures = vec![chain("b.same", &["t.a"]), chain("a.same", &["t.a"])];
        let p = plan("x", &[Some("t.a")]);
        assert_eq!(
            resolve_procedure_for_plan(&procedures, &p).map(|r| r.procedure_id.as_str()),
            Some("a.same")
        );
    }

    #[test]
    fn run_row_carries_terminal_basis_tools_and_derived_score() {
        let procedures = vec![outcome_reflex_procedure()];
        let mut p = plan(
            &"g".repeat(300),
            &[Some("life.observe"), Some("life.commit")],
        );
        p.procedure_id = Some("outcome-reflex".into());
        let t = turn(&["life.recall", "life.observe", "life.commit"]);

        let run = build_procedure_run(
            &procedures,
            "agent-a",
            "sess-1",
            &t,
            &p,
            &outcome(PlanEvalVerdict::Complete, PlanEvalBasis::Grounded),
            RunTerminal::Complete,
        )
        .expect("attributed");
        assert_eq!(run.procedure_id, "outcome-reflex");
        assert_eq!(run.graph_version, 1);
        assert_eq!(run.agent_id, "agent-a");
        assert_eq!(run.session_id, "sess-1");
        assert_eq!(run.turn_id, "turn-7");
        assert_eq!(run.verdict, "complete");
        assert_eq!(run.basis, "grounded");
        assert_eq!(run.score, 1.0);
        assert_eq!(
            run.tool_sequence,
            vec!["life.recall", "life.observe", "life.commit"]
        );
        assert_eq!(run.steps_total, 3);
        assert_eq!(run.steps_verified, 2);
        assert_eq!(run.stalls, 1);
        assert_eq!(run.contradicted, 1);
        assert_eq!(run.goal.chars().count(), RUN_GOAL_MAX_CHARS + 1);
        assert!(run.goal.ends_with('…'));
        assert_eq!(run.recorded_at, 0);
        assert!(!run.run_id.is_empty());

        let stopped = build_procedure_run(
            &procedures,
            "agent-a",
            "sess-1",
            &t,
            &p,
            &outcome(PlanEvalVerdict::Continue, PlanEvalBasis::Grounded),
            RunTerminal::Stopped,
        )
        .expect("attributed");
        assert_eq!(stopped.verdict, "stopped");
        assert_eq!(stopped.score, 0.0);

        let claimed = build_procedure_run(
            &procedures,
            "agent-a",
            "sess-1",
            &t,
            &p,
            &outcome(PlanEvalVerdict::Complete, PlanEvalBasis::ModelReported),
            RunTerminal::Complete,
        )
        .expect("attributed");
        assert_eq!(claimed.score, 0.5);
    }

    #[test]
    fn unattributed_plan_produces_no_row() {
        let procedures = vec![outcome_reflex_procedure()];
        let p = plan("x", &[Some("web.fetch")]);
        assert!(
            build_procedure_run(
                &procedures,
                "a",
                "s",
                &turn(&["web.fetch"]),
                &p,
                &outcome(PlanEvalVerdict::Complete, PlanEvalBasis::Grounded),
                RunTerminal::Complete,
            )
            .is_none()
        );
    }
}
