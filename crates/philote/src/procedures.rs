//! Procedural graphs — plan attribution, the run ledger row, and localized
//! guidance (doc:procedural-graphs, slices P1 `procedure-run-ledger` and
//! P2 `procedure-localized-guidance`).
//!
//! **P1.** The paper's self-evolution loop needs one thing Philotic threw
//! away: a per-episode score attached to the procedure that produced it.
//! [`crate::plan_eval::PlanEvalOutcome`] already carries the grounded
//! verdict; this module attributes it to a procedure and shapes the ledger
//! row. Only a **terminal** eval (complete, blocked, or stopped) ever becomes
//! a row — a `Continue` is mid-flight, not evidence.
//!
//! Attribution is deliberately conservative: a plan the harness seeded from a
//! procedure carries `ActivePlan::procedure_id` and resolves by id; any other
//! plan resolves only when its declared tools overlap a bound procedure's
//! tools by Jaccard ≥ [`MIN_PROCEDURE_TOOL_OVERLAP`]. A plan that matches
//! nothing produces no row, so the ledger never scores a procedure for work
//! it did not shape.
//!
//! **P2.** The paper's guidance step, reduced to what needs no model: locate
//! the active node by exact-matching the last tool call, then render that
//! node's outgoing edges as `Next / when / do / avoid` lines. It is appended
//! after the grounded re-entry hint and to the continuation brief, capped at
//! [`MAX_PROCEDURE_GUIDANCE_CHARS`], and it is advisory in exactly the sense
//! the paper means: it never adds a tool, never overrides the one-step-one-
//! outcome rule, and approval policy still decides what runs. Kill switch:
//! `PHILOTIC_DISABLE_PROCEDURE_GUIDANCE`.

use std::collections::BTreeSet;

use ansible_mesh_core::procedure::{ProcedureGraphRecord, ProcedureNode, ProcedureRunRecord};
use uuid::Uuid;

use crate::r#loop::{ToolCall, ToolResult};
use crate::plan_eval::PlanEvalOutcome;
use crate::session::{ActivePlan, CarryoverPlan, WorkingTurn};

/// Minimum Jaccard overlap between a plan's declared tools and a procedure's
/// tool nodes for an un-stamped plan to be attributed to that procedure.
pub const MIN_PROCEDURE_TOOL_OVERLAP: f32 = 0.5;
/// Plan goal excerpt kept on a ledger row.
pub const RUN_GOAL_MAX_CHARS: usize = 200;
/// Hard cap on a rendered guidance block. The paper's localized subgraph cut
/// tokens ~71% against full-graph injection; the cap makes that structural.
pub const MAX_PROCEDURE_GUIDANCE_CHARS: usize = 900;

/// Operator kill switch for localized guidance. Attribution and the run
/// ledger keep working with it set — only the prompt text goes away.
pub fn procedure_guidance_disabled() -> bool {
    std::env::var("PHILOTIC_DISABLE_PROCEDURE_GUIDANCE")
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

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

/// Whether localized guidance is in play for this plan: a procedure resolves
/// and the kill switch is off. Recorded on the ledger row so the trial gate
/// can tell guided runs from unguided ones.
pub fn guidance_applies(procedures: &[ProcedureGraphRecord], plan: &ActivePlan) -> bool {
    !procedure_guidance_disabled() && resolve_procedure_for_plan(procedures, plan).is_some()
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
        guidance_rendered: !procedure_guidance_disabled(),
        score: ProcedureRunRecord::score_for(verdict, basis),
        recorded_at: 0,
    })
}

// ── P2: localized guidance ───────────────────────────────────────────────────

/// Where the agent is in the procedure, and how it got there.
struct Localized<'a> {
    node: &'a ProcedureNode,
    /// The last tool call read as an error, so the way *into* `node` matters.
    last_failed: bool,
}

/// Exact-match localization over the working tool history: the last call
/// whose tool is a node of the procedure, disambiguated by the call before
/// it. With no matching call at all, the entry node — the agent has not
/// started the procedure yet.
fn localize<'a>(
    procedure: &'a ProcedureGraphRecord,
    history: &[(ToolCall, ToolResult)],
) -> Option<Localized<'a>> {
    let tools = procedure.tool_names();
    let last_failed = history
        .last()
        .is_some_and(|(_, r)| crate::runtime::distill::tool_result_is_error(&r.content));
    let mut idx = history.len();
    while idx > 0 {
        idx -= 1;
        let tool = history[idx].0.tool_name.as_str();
        if !tools.contains(tool) {
            continue;
        }
        let previous = idx.checked_sub(1).map(|p| history[p].0.tool_name.as_str());
        let node = procedure.locate(tool, previous)?;
        return Some(Localized { node, last_failed });
    }
    procedure.node(&procedure.entry).map(|node| Localized {
        node,
        last_failed: false,
    })
}

/// Localize by the plan's own evidence instead of a tool history: the tool
/// of the last verified step in plan order, else the entry node. Used for the
/// continuation brief, which opens a fresh turn with no history yet.
fn localize_from_carryover<'a>(
    procedure: &'a ProcedureGraphRecord,
    carry: &CarryoverPlan,
) -> Option<Localized<'a>> {
    let mut last_verified_tool: Option<&str> = None;
    let mut previous_tool: Option<&str> = None;
    for step in &carry.plan.steps {
        if carry.verified_step_ids.contains(&step.id) {
            if let Some(tool) = step.tool_name.as_deref() {
                previous_tool = last_verified_tool;
                last_verified_tool = Some(tool);
            }
        }
    }
    match last_verified_tool {
        Some(tool) => procedure.locate(tool, previous_tool).map(|node| Localized {
            node,
            last_failed: carry.stalled_continuations > 0,
        }),
        None => procedure.node(&procedure.entry).map(|node| Localized {
            node,
            last_failed: false,
        }),
    }
}

fn node_handle(node: &ProcedureNode) -> String {
    match node.tool_name.as_deref() {
        Some(tool) => format!("{} [{tool}]", node.id),
        None => node.id.clone(),
    }
}

/// Collapse whitespace and read the seeder's `{target}` placeholder as
/// prose — the render has no recalled id in hand.
fn joined(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(TARGET_PLACEHOLDER, "the recalled loop")
}

/// The neighbourhood render. One line per outgoing edge in declaration
/// order; the incoming edges' pitfalls only when the last call failed.
fn render_at(procedure: &ProcedureGraphRecord, at: &Localized<'_>) -> String {
    let mut out = format!(
        "[Procedure guidance: {} @ {}]\n",
        procedure.procedure_id,
        node_handle(at.node)
    );
    if at.last_failed {
        let pitfalls: Vec<String> = procedure
            .incoming(&at.node.id)
            .iter()
            .map(|e| joined(&e.pitfalls))
            .filter(|p| !p.is_empty())
            .collect();
        if !pitfalls.is_empty() {
            out.push_str(&format!(
                "The last call failed. On the way into {}, avoid: {}.\n",
                at.node.id,
                pitfalls.join("; ")
            ));
        }
    }
    let outgoing = procedure.outgoing(&at.node.id);
    if outgoing.is_empty() {
        out.push_str(&format!(
            "{} is the procedure's last step. Finish it, then report what was actually done.\n",
            at.node.id
        ));
    }
    for edge in outgoing {
        let Some(to) = procedure.node(&edge.to) else {
            continue;
        };
        let mut line = format!("Next: {} ({})", node_handle(to), edge.relation.as_str());
        let condition = joined(&edge.condition);
        if !condition.is_empty() {
            line.push_str(&format!(" — when: {condition}"));
        }
        let guidance = joined(&edge.guidance);
        if !guidance.is_empty() {
            line.push_str(&format!("; do: {guidance}"));
        }
        let pitfalls = joined(&edge.pitfalls);
        if !pitfalls.is_empty() {
            line.push_str(&format!("; avoid: {pitfalls}"));
        }
        line.push('\n');
        out.push_str(&line);
    }
    out.push_str(
        "This is advice from the procedure's graph, not a new instruction: one step per tool \
         call, and a step is done only when its tool call succeeded.",
    );
    if out.chars().count() > MAX_PROCEDURE_GUIDANCE_CHARS {
        let mut truncated: String = out.chars().take(MAX_PROCEDURE_GUIDANCE_CHARS - 1).collect();
        truncated.push('…');
        truncated
    } else {
        out
    }
}

/// Guidance for an in-turn re-entry: localize on the working tool history
/// of the turn. `None` when no procedure resolves for the plan, when the
/// agent has left the procedure (calls made but none of them a node), or
/// when the kill switch is set.
pub fn render_procedure_guidance(
    procedures: &[ProcedureGraphRecord],
    plan: &ActivePlan,
    history: &[(ToolCall, ToolResult)],
) -> Option<String> {
    if procedure_guidance_disabled() {
        return None;
    }
    render_procedure_guidance_unchecked(procedures, plan, history)
}

/// [`render_procedure_guidance`] without the environment check.
pub fn render_procedure_guidance_unchecked(
    procedures: &[ProcedureGraphRecord],
    plan: &ActivePlan,
    history: &[(ToolCall, ToolResult)],
) -> Option<String> {
    let procedure = resolve_procedure_for_plan(procedures, plan)?;
    let at = localize(procedure, history)?;
    Some(render_at(procedure, &at))
}

/// Guidance for a synthesized continuation turn: localize on the carryover's
/// verified steps, since the new turn has no history yet.
pub fn render_carryover_guidance(
    procedures: &[ProcedureGraphRecord],
    carry: &CarryoverPlan,
) -> Option<String> {
    if procedure_guidance_disabled() {
        return None;
    }
    render_carryover_guidance_unchecked(procedures, carry)
}

/// [`render_carryover_guidance`] without the environment check.
pub fn render_carryover_guidance_unchecked(
    procedures: &[ProcedureGraphRecord],
    carry: &CarryoverPlan,
) -> Option<String> {
    let procedure = resolve_procedure_for_plan(procedures, &carry.plan)?;
    let at = localize_from_carryover(procedure, carry)?;
    Some(render_at(procedure, &at))
}

// ── P3: procedure-seeded plans ───────────────────────────────────────────────

/// Placeholder in node labels and edge guidance that the seeder replaces with
/// the recalled target id (or a description of how to find it).
pub const TARGET_PLACEHOLDER: &str = "{target}";

/// What the trigger context already knows when a plan is seeded.
#[derive(Debug, Clone, Copy, Default)]
pub struct SeedContext<'a> {
    /// Branch tag to follow out of the entry node (`ProcedureEdge::branch`).
    /// `None` or no tagged match: the first sequencing edge.
    pub branch: Option<&'a str>,
    /// The recalled node the outcome settles, when already in context.
    pub target: Option<&'a str>,
    /// Operator excerpt appended to the first step.
    pub excerpt: &'a str,
}

/// The bound procedure whose `trigger` names `trigger`, if any is projectable.
pub fn triggered_procedure<'a>(
    procedures: &'a [ProcedureGraphRecord],
    trigger: &str,
) -> Option<&'a ProcedureGraphRecord> {
    procedures
        .iter()
        .find(|p| p.trigger.as_deref() == Some(trigger) && p.validation_state.is_projectable())
}

/// Project a procedure's backbone into an `ActivePlan`: one step per tool
/// node from the chosen branch, description = node label + the incoming
/// edge's guidance, `{target}` substituted, the operator excerpt on the first
/// step, and `procedure_id` stamped so the ledger attributes the result.
/// `None` when the branch yields no tool step.
pub fn seed_plan_from_procedure(
    procedure: &ProcedureGraphRecord,
    ctx: &SeedContext<'_>,
) -> Option<ActivePlan> {
    use ansible_mesh_core::procedure::ProcedureNodeKind;
    let entry = procedure.node(&procedure.entry)?;
    let start: &str = if entry.kind == ProcedureNodeKind::Tool {
        entry.id.as_str()
    } else {
        let out = procedure.outgoing(&entry.id);
        let chosen = ctx
            .branch
            .and_then(|tag| out.iter().find(|e| e.branch.as_deref() == Some(tag)))
            .or_else(|| out.first())
            .copied()?;
        chosen.to.as_str()
    };
    let target_text = ctx
        .target
        .map(str::to_string)
        .unwrap_or_else(|| "the loop found in the recall step (never invent an id)".to_string());
    let mut steps = Vec::new();
    let mut previous: Option<&str> = Some(entry.id.as_str()).filter(|e| *e != start);
    for node in procedure.backbone_from(start) {
        if node.kind != ProcedureNodeKind::Tool {
            previous = Some(node.id.as_str());
            continue;
        }
        let mut description = node.label.clone();
        if let Some(prev) = previous {
            if let Some(edge) = procedure
                .edges
                .iter()
                .find(|e| e.from == prev && e.to == node.id)
            {
                let guidance = edge
                    .guidance
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !guidance.is_empty() {
                    description.push_str(" — ");
                    description.push_str(&guidance);
                }
            }
        }
        if steps.is_empty() && !ctx.excerpt.is_empty() {
            description.push_str(&format!(" Operator said: \"{}\"", ctx.excerpt));
        }
        steps.push(crate::session::PlanStep {
            id: steps.len() as u32 + 1,
            description: description.replace(TARGET_PLACEHOLDER, &target_text),
            tool_name: node.tool_name.clone(),
            status: "pending".into(),
        });
        previous = Some(node.id.as_str());
    }
    if steps.is_empty() {
        return None;
    }
    let headline = procedure
        .description
        .split(['.', '\n'])
        .next()
        .unwrap_or(&procedure.description)
        .trim();
    Some(ActivePlan {
        goal: format!(
            "{} — {}",
            headline.replace(TARGET_PLACEHOLDER, &target_text),
            ctx.target.unwrap_or("resolve the loop it settles")
        ),
        status: "executing".into(),
        steps,
        context_1_advisory: None,
        procedure_id: Some(procedure.procedure_id.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_eval::{PlanEvalBasis, PlanEvalVerdict};
    use crate::session::PlanStep;
    use ansible_mesh_core::procedure::{
        ProcedureEdge, ProcedureNodeKind, ProcedureRelation, outcome_reflex_procedure,
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
        let nodes: Vec<ProcedureNode> = tools
            .iter()
            .enumerate()
            .map(|(i, t)| ProcedureNode {
                id: format!("n{i}"),
                label: format!("do {t}"),
                kind: ProcedureNodeKind::Tool,
                tool_name: Some((*t).to_string()),
            })
            .collect();
        let edges = (1..nodes.len())
            .map(|i| ProcedureEdge {
                from: format!("n{}", i - 1),
                to: format!("n{i}"),
                relation: ProcedureRelation::LeadsTo,
                condition: format!("c{i}"),
                guidance: format!("g{i}"),
                pitfalls: format!("p{i}"),
                branch: None,
            })
            .collect();
        ProcedureGraphRecord {
            procedure_id: id.into(),
            description: "test".into(),
            entry: "n0".into(),
            nodes,
            edges,
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
            outstanding_step_briefs: Vec::new(),
            stalled_continuations: 1,
            verdict,
            basis,
        }
    }

    fn history(calls: &[(&str, &str)]) -> Vec<(ToolCall, ToolResult)> {
        calls
            .iter()
            .map(|(t, content)| {
                (
                    ToolCall {
                        tool_name: (*t).to_string(),
                        arguments: serde_json::json!({}),
                    },
                    ToolResult {
                        tool_name: (*t).to_string(),
                        content: (*content).to_string(),
                    },
                )
            })
            .collect()
    }

    fn turn(calls: &[&str]) -> WorkingTurn {
        let mut turn = WorkingTurn::for_plan_tests();
        turn.turn_id = "turn-7".into();
        turn.working_tool_history = history(&calls.iter().map(|c| (*c, "ok")).collect::<Vec<_>>());
        turn
    }

    // ── P1 ────────────────────────────────────────────────────────────────

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

    // ── P2 ────────────────────────────────────────────────────────────────

    #[test]
    fn guidance_names_only_the_active_nodes_out_edges() {
        let procedures = vec![outcome_reflex_procedure()];
        let mut p = plan("x", &[Some("life.observe"), Some("life.commit")]);
        p.procedure_id = Some("outcome-reflex".into());
        let g = render_procedure_guidance_unchecked(
            &procedures,
            &p,
            &history(&[("life.recall", "ok"), ("life.observe", "ok")]),
        )
        .expect("guidance");
        assert!(
            g.starts_with("[Procedure guidance: outcome-reflex @ observe [life.observe]]"),
            "{g}"
        );
        assert!(g.contains("Next: commit [life.commit] (LEADS_TO)"), "{g}");
        assert!(g.contains("when: the outcome Event is recorded"), "{g}");
        assert!(g.contains("avoid: inventing an id"), "{g}");
        // Not the recall edge, not the start branches.
        assert!(!g.contains("Next: observe"), "{g}");
        assert!(!g.contains("Next: recall"), "{g}");
        assert!(!g.contains("The last call failed"), "{g}");
        assert!(g.chars().count() <= MAX_PROCEDURE_GUIDANCE_CHARS);
    }

    #[test]
    fn guidance_starts_at_entry_with_no_history_and_flags_a_failed_call() {
        let procedures = vec![outcome_reflex_procedure()];
        let mut p = plan("x", &[Some("life.observe")]);
        p.procedure_id = Some("outcome-reflex".into());
        let g = render_procedure_guidance_unchecked(&procedures, &p, &[]).expect("entry");
        assert!(g.contains("@ start]"), "{g}");
        assert!(g.contains("Next: recall [life.recall] (TRIGGERS)"), "{g}");
        assert!(g.contains("Next: observe [life.observe] (TRIGGERS)"), "{g}");

        // A failed commit: pitfalls on the way in, and it is the last step.
        let g = render_procedure_guidance_unchecked(
            &procedures,
            &p,
            &history(&[
                ("life.observe", "ok"),
                ("life.commit", "Error: no such node"),
            ]),
        )
        .expect("failed");
        assert!(g.contains("@ commit [life.commit]"), "{g}");
        assert!(
            g.contains("The last call failed. On the way into commit, avoid: inventing an id"),
            "{g}"
        );
        assert!(g.contains("commit is the procedure's last step"), "{g}");
    }

    #[test]
    fn guidance_is_absent_when_unattributed_or_off_the_procedure() {
        let procedures = vec![outcome_reflex_procedure()];
        // No procedure for this plan.
        let p = plan("x", &[Some("web.fetch")]);
        assert!(render_procedure_guidance_unchecked(&procedures, &p, &[]).is_none());
        // Attributed, but every call so far is off the graph: the agent has
        // not started the procedure, so the entry node's branches apply.
        let mut p = plan("x", &[Some("life.observe")]);
        p.procedure_id = Some("outcome-reflex".into());
        let g =
            render_procedure_guidance_unchecked(&procedures, &p, &history(&[("web.fetch", "ok")]))
                .expect("entry");
        assert!(g.contains("@ start]"), "{g}");
        // Calls after the procedure step do not lose the localization.
        let g = render_procedure_guidance_unchecked(
            &procedures,
            &p,
            &history(&[("life.observe", "ok"), ("memory.recall", "ok")]),
        )
        .expect("still localized");
        assert!(g.contains("@ observe"), "{g}");
    }

    #[test]
    fn guidance_disambiguates_shared_tools_and_is_capped() {
        // Same tool twice; the predecessor decides which node we are on.
        let mut p = chain("x.long", &["t.a", "t.shared", "t.b", "t.shared"]);
        p.edges[0].guidance = "w".repeat(270);
        p.edges[1].guidance = "w".repeat(270);
        p.edges[2].guidance = "w".repeat(270);
        assert_eq!(p.validate(), Ok(()));
        let procedures = vec![p];
        let mut plan = plan("x", &[Some("t.a"), Some("t.shared"), Some("t.b")]);
        plan.procedure_id = Some("x.long".into());
        let g = render_procedure_guidance_unchecked(
            &procedures,
            &plan,
            &history(&[("t.b", "ok"), ("t.shared", "ok")]),
        )
        .expect("localized");
        assert!(g.contains("@ n3 [t.shared]"), "{g}");
        let g = render_procedure_guidance_unchecked(
            &procedures,
            &plan,
            &history(&[("t.a", "ok"), ("t.shared", "ok")]),
        )
        .expect("localized");
        assert!(g.contains("@ n1 [t.shared]"), "{g}");
        // Entry render carries one 270-char guidance line plus the footer; a
        // node with three long lines would blow the cap, so build one.
        let mut wide = chain("x.wide", &["t.a", "t.b", "t.c", "t.d"]);
        for i in 1..4 {
            wide.edges.push(ProcedureEdge {
                from: "n0".into(),
                to: format!("n{i}"),
                relation: ProcedureRelation::Triggers,
                guidance: "w".repeat(270),
                ..Default::default()
            });
        }
        assert_eq!(wide.validate(), Ok(()));
        let mut plan = plan;
        plan.procedure_id = Some("x.wide".into());
        let g = render_procedure_guidance_unchecked(&[wide], &plan, &[]).expect("capped");
        assert_eq!(g.chars().count(), MAX_PROCEDURE_GUIDANCE_CHARS);
        assert!(g.ends_with('…'));
    }

    #[test]
    fn carryover_guidance_localizes_on_the_last_verified_step() {
        let procedures = vec![outcome_reflex_procedure()];
        let mut p = plan(
            "x",
            &[
                Some("life.recall"),
                Some("life.observe"),
                Some("life.commit"),
            ],
        );
        p.procedure_id = Some("outcome-reflex".into());
        let mut carry = CarryoverPlan {
            plan: p,
            steps_done: vec![true, true, false],
            verified_step_ids: vec![1, 2],
            stalled_continuations: 0,
            continuations_used: 1,
            lifetime_continuations: 1,
            created_turn_id: "t0".into(),
        };
        let g = render_carryover_guidance_unchecked(&procedures, &carry).expect("carry");
        assert!(g.contains("@ observe [life.observe]"), "{g}");
        assert!(g.contains("Next: commit [life.commit]"), "{g}");
        assert!(!g.contains("The last call failed"), "{g}");
        // A stalled continuation reads as a failed way in.
        carry.stalled_continuations = 1;
        let g = render_carryover_guidance_unchecked(&procedures, &carry).expect("stalled");
        assert!(
            g.contains("The last call failed. On the way into observe"),
            "{g}"
        );
        // Nothing verified yet: start at the entry.
        carry.verified_step_ids.clear();
        let g = render_carryover_guidance_unchecked(&procedures, &carry).expect("entry");
        assert!(g.contains("@ start]"), "{g}");
    }

    // ── P3 ────────────────────────────────────────────────────────────────

    #[test]
    fn seeded_plan_follows_the_tagged_branch_and_substitutes_the_target() {
        let p = outcome_reflex_procedure();
        // Target known: observe → commit, the id in both steps.
        let plan = seed_plan_from_procedure(
            &p,
            &SeedContext {
                branch: Some("target_known"),
                target: Some("life:open_loop:speech"),
                excerpt: "I gave my speech",
            },
        )
        .expect("seeded");
        assert_eq!(plan.procedure_id.as_deref(), Some("outcome-reflex"));
        assert_eq!(
            plan.steps
                .iter()
                .map(|s| s.tool_name.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("life.observe"), Some("life.commit")]
        );
        assert!(
            plan.steps[0]
                .description
                .contains("linked to life:open_loop:speech"),
            "{}",
            plan.steps[0].description
        );
        assert!(
            plan.steps[0]
                .description
                .contains("Operator said: \"I gave my speech\"")
        );
        assert!(
            plan.steps[1]
                .description
                .contains("life.commit life:open_loop:speech by its exact id"),
            "{}",
            plan.steps[1].description
        );
        assert!(!plan.steps[1].description.contains("Operator said"));
        assert!(plan.goal.contains("life:open_loop:speech"), "{}", plan.goal);
        assert_eq!(plan.status, "executing");
        assert_eq!(plan.steps[0].id, 1);
        assert_eq!(plan.steps[1].id, 2);

        // Target unknown: recall → observe → commit, placeholder spelled out.
        let plan = seed_plan_from_procedure(
            &p,
            &SeedContext {
                branch: Some("target_unknown"),
                target: None,
                excerpt: "tickets are booked",
            },
        )
        .expect("seeded");
        assert_eq!(
            plan.steps
                .iter()
                .map(|s| s.tool_name.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some("life.recall"),
                Some("life.observe"),
                Some("life.commit")
            ]
        );
        assert!(plan.steps[0].description.contains("open_loops_by_context"));
        assert!(
            plan.steps[2]
                .description
                .contains("the loop found in the recall step"),
            "{}",
            plan.steps[2].description
        );
        assert!(!plan.steps[2].description.contains(TARGET_PLACEHOLDER));

        // Unknown branch tag: first sequencing edge out of the entry.
        let plan = seed_plan_from_procedure(
            &p,
            &SeedContext {
                branch: Some("nope"),
                target: None,
                excerpt: "",
            },
        )
        .expect("seeded");
        assert_eq!(plan.steps[0].tool_name.as_deref(), Some("life.recall"));
        assert!(!plan.steps[0].description.contains("Operator said"));

        // A procedure whose entry is itself a tool node starts there.
        let c = chain("x.chain", &["t.a", "t.b"]);
        let plan = seed_plan_from_procedure(&c, &SeedContext::default()).expect("seeded");
        assert_eq!(
            plan.steps
                .iter()
                .map(|s| s.tool_name.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("t.a"), Some("t.b")]
        );
        assert!(
            plan.steps[1].description.contains("— g1"),
            "{}",
            plan.steps[1].description
        );
        assert_eq!(
            triggered_procedure(&[c, p], "reports_an_outcome").map(|r| r.procedure_id.as_str()),
            Some("outcome-reflex")
        );
    }
}
