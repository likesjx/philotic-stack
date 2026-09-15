//! Self-Improvement Loop Slice L1 — `skills.distill`.
//!
//! The one mechanical trigger Philotic's learning loop never had. When a
//! turn closes having done something hard-won, the philote whispers a bounded
//! *distill review* to itself through the ordinary paracrine path. The review
//! runs as a separate lookaside session on a fixed minimal tool surface
//! ([`TOOL_ALLOWLIST`]); its only legal outputs are a `skill.register` that the
//! hotel forces to `Draft` (see `handle_register_skill_with_origin`) and a
//! Muninn write. Its final text is routed [`ParacrineRouting::Discard`] — the
//! operator never sees it, the model never re-reads it.
//!
//! Three predicates, evaluated in order at turn close (`deliver_text_reply`):
//!
//! 1. [`DistillTrigger::ToolCount`] — `working_tool_history.len() >=`
//!    [`DISTILL_TOOL_COUNT_THRESHOLD`].
//! 2. [`DistillTrigger::ErrorRecovered`] — some earlier tool result in the
//!    turn read as an error and the final one did not.
//! 3. [`DistillTrigger::UserCorrection`] — the user's message opened with a
//!    correction and the turn still used at least one tool.
//!
//! Guard rails, all mechanical:
//! - never fires for a paracrine-origin or intent-carrying turn (so the
//!   distill turn itself, and every other whisper, can never re-trigger);
//! - lane kill switch `PHILOTIC_AUTONOMY_DISABLE_SKILLS_DISTILL` checked
//!   locally *and* by the hotel;
//! - budgeted by the hotel's `skills.distill` `AutonomyGrant` (3/day per
//!   hotel by default) via `ConsumeAutonomyAction { filing: true }`, which
//!   also writes the `Pending` audit record the operator later stamps;
//! - the whisper prompt is bounded by `PARACRINE_WHISPER_PROMPT_MAX_CHARS`.

use super::*;
use ansible_mesh_core::procedure::{
    ProcedureGraphRecord, ProcedurePatchRecord, ProcedureRunRecord,
};

/// Intent marker carried in the exosome's `context.intent`, prefixing the
/// trigger name (`skills.distill:tool_count`). Recognised by the tool layer.
pub(crate) const INTENT: &str = "skills.distill";

/// The only tools a distill lookaside turn may call. Anything else is
/// refused at dispatch with a tool-result denial, regardless of the role's
/// default toolset. `skill.assign`/`skill.set_state` are deliberately absent:
/// a distilled skill is a proposal and must not be able to grant itself.
pub(crate) const TOOL_ALLOWLIST: &[&str] = &[
    "skill.register",
    "skill.list",
    "memory.remember",
    "memory.recall",
    // Procedural graphs P4: a distilled skill may arrive with its procedure,
    // and the contrast whisper may read a graph and file one patch. Both
    // land Draft / Pending — filings, gated at promotion.
    "procedure.register",
    "procedure.get",
    "procedure.patch",
];

/// Predicate 1 threshold.
pub(super) const DISTILL_TOOL_COUNT_THRESHOLD: usize = 5;

/// Per-tool summary length inside the whisper prompt.
const TOOL_SUMMARY_CHARS: usize = 140;
/// Reply excerpt length inside the whisper prompt.
const REPLY_EXCERPT_CHARS: usize = 400;
/// User message excerpt length inside the whisper prompt.
const USER_EXCERPT_CHARS: usize = 400;

/// Optional operator override of the role that receives distill whispers.
/// Default: this philote's own role (a self-lookaside).
const ENV_DISTILL_ROLE: &str = "PHILOTIC_SKILLS_DISTILL_ROLE";

/// Role name a distill whisper falls back to when the current role cannot
/// register skills. Targeted by agent-scoped routing key, never by bare name.
const DISTILL_FALLBACK_ROLE: &str = "orchestrator";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DistillTrigger {
    ToolCount,
    ErrorRecovered,
    UserCorrection,
    /// Procedural graphs P4: the run ledger holds both a failed and a
    /// successful run of one procedure at its current version.
    ProcedureContrast,
}

impl DistillTrigger {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            DistillTrigger::ToolCount => "tool_count",
            DistillTrigger::ErrorRecovered => "error_recovered",
            DistillTrigger::UserCorrection => "user_correction",
            DistillTrigger::ProcedureContrast => "procedure_contrast",
        }
    }
}

/// Is this turn a distill lookaside (serving a whisper with our intent)?
pub(crate) fn turn_is_distill(turn: &WorkingTurn) -> bool {
    turn.paracrine_intent
        .as_deref()
        .is_some_and(|i| i == INTENT || i.starts_with(&format!("{INTENT}:")))
}

pub(crate) fn tool_allowed(tool_name: &str) -> bool {
    TOOL_ALLOWLIST.contains(&tool_name)
}

/// Map a turn's intent (`skills.distill:<trigger>`) to the `origin` the
/// hotel expects on `RegisterSkill` (`distill:<trigger>`).
pub(super) fn origin_from_intent(intent: &str) -> Option<String> {
    if intent == INTENT {
        return Some("distill".to_string());
    }
    intent
        .strip_prefix(&format!("{INTENT}:"))
        .map(|trigger| format!("distill:{trigger}"))
}

/// Heuristic: does a tool result read as a failure? Tool results are free
/// text; the common shapes are a leading `Error`/`error:` line, a JSON
/// envelope with `"success": false` / `"ok": false`, or an exit code line.
pub(crate) fn tool_result_is_error(content: &str) -> bool {
    let head: String = content.trim_start().chars().take(64).collect();
    let head_l = head.to_ascii_lowercase();
    if head_l.starts_with("error") || head_l.starts_with("failed") || head_l.starts_with("denied") {
        return true;
    }
    // Every `TaskErrorPayload::display_message` renders as
    // "<message> | kind=<kind> | code=<code> …" — the hotel's own refusal
    // shape. Live 2026-09-14 18:43 UTC "only agent guests may request
    // subagent delegation | kind=ipc_failure | code=SUBAGENT_FORBIDDEN"
    // was labelled "ok" by this heuristic because it opens with prose.
    if content.contains(" | kind=") {
        return true;
    }
    // life.observe.batch reports validation rejections inside an ok
    // envelope: nothing written, every item rejected.
    if content.contains("failed validation and were never written") {
        return true;
    }
    let compact: String = content
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(4096)
        .collect();
    compact.contains("\"success\":false")
        || compact.contains("\"ok\":false")
        || compact.contains("\"error\":\"")
        || compact.contains("\"exit_code\":1")
        || compact.contains("\"exit_code\":2")
        || compact.contains("\"exit_code\":126")
        || compact.contains("\"exit_code\":127")
}

/// Heuristic: did the user open with a correction of the previous turn?
/// Lexical on the first ~80 characters, lowercase.
pub(super) fn is_corrective_message(text: &str) -> bool {
    let head: String = text
        .trim_start()
        .chars()
        .take(80)
        .collect::<String>()
        .to_ascii_lowercase();
    const OPENERS: &[&str] = &[
        "no,",
        "no.",
        "no ",
        "nope",
        "not that",
        "that's wrong",
        "thats wrong",
        "that is wrong",
        "wrong",
        "incorrect",
        "i meant",
        "i said",
        "not what i asked",
        "not what i meant",
        "that's not what",
        "thats not what",
        "undo",
        "revert",
        "try again",
        "redo",
    ];
    OPENERS.iter().any(|o| head.starts_with(o))
        || head.contains("that's not what i")
        || head.contains("thats not what i")
}

/// Evaluate the three predicates on a completed turn. `None` = no whisper.
pub(super) fn evaluate_turn(turn: &WorkingTurn) -> Option<DistillTrigger> {
    // Never for whispers — this is what makes the loop terminate.
    if turn.paracrine_origin.is_some() || turn.paracrine_intent.is_some() {
        return None;
    }
    let history = &turn.working_tool_history;
    if history.is_empty() {
        return None;
    }
    if history.len() >= DISTILL_TOOL_COUNT_THRESHOLD {
        return Some(DistillTrigger::ToolCount);
    }
    let last_is_error = history
        .last()
        .is_some_and(|(_, r)| tool_result_is_error(&r.content));
    let earlier_error = history[..history.len() - 1]
        .iter()
        .any(|(_, r)| tool_result_is_error(&r.content));
    if earlier_error && !last_is_error {
        return Some(DistillTrigger::ErrorRecovered);
    }
    if is_corrective_message(&turn.user_content) {
        return Some(DistillTrigger::UserCorrection);
    }
    None
}

fn excerpt(text: &str, max_chars: usize) -> String {
    let mut s: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        s.push('…');
    }
    s.replace('\n', " ")
}

/// Build the distill review brief. Bounded; names the trigger; spells out
/// the only two legal outputs and the exact no-op reply.
pub(super) fn build_distill_prompt(
    turn: &WorkingTurn,
    trigger: DistillTrigger,
    reply: &str,
) -> String {
    let mut tools = String::new();
    for (i, (call, result)) in turn.working_tool_history.iter().enumerate() {
        let args = excerpt(&call.arguments.to_string(), TOOL_SUMMARY_CHARS);
        let outcome = if tool_result_is_error(&result.content) {
            "ERR"
        } else {
            "ok"
        };
        let res = excerpt(&result.content, TOOL_SUMMARY_CHARS);
        tools.push_str(&format!(
            "{}. {} {} → {} {}\n",
            i + 1,
            call.tool_name,
            args,
            outcome,
            res
        ));
    }
    let trigger_line = match trigger {
        DistillTrigger::ToolCount => {
            format!("it took {} tool calls", turn.working_tool_history.len())
        }
        DistillTrigger::ErrorRecovered => {
            "an earlier step failed and a later path worked".to_string()
        }
        DistillTrigger::UserCorrection => "the user corrected the previous attempt".to_string(),
        DistillTrigger::ProcedureContrast => {
            "a procedure run was contrasted with an earlier one".to_string()
        }
    };
    let prompt = format!(
        "DISTILL REVIEW — silent lookaside. Nothing you write here reaches the operator; only your tool calls matter.\n\
         A turn just completed and {trigger_line}.\n\n\
         User asked: «{user}»\n\
         {goal_line}\
         Tools used, in order:\n{tools}\
         Final reply: «{reply}»\n\n\
         Decide whether this was a reusable procedure worth naming. Reaching this review already means the \
         turn met the pattern threshold — you do not need to have seen it three times; the operator reviews \
         every Draft, so a plausible procedure is worth drafting and a wrong one costs nothing. Say YES when \
         the turn followed an ordered, repeatable sequence of tool steps that someone could ask for again \
         (a review, a digest, a check, a report). Say nothing only when the turn was a one-tool lookup, \
         pure conversation, or a failure with no working path.\n\
         - If YES: call skill.register ONCE with skill_name (lowercase dotted, e.g. research.github-digest), \
         description (one sentence, when to use it), subagent_kind \"philote-worker\", goal (the procedure as a \
         template with {{{{placeholders}}}} for the parts that vary), and allowed_tools = exactly the tools used above. \
         It lands as a Draft for the operator to review; do not assign it, do not register a second one. \
         Then, if the sequence had two or more tool steps, call procedure.register ONCE with procedure_id = the \
         skill_name, skill_name = the skill_name, one tool node per tool call in order (kind \"tool\", tool_name \
         exact), leads_to edges between consecutive nodes with condition/guidance/pitfalls drawn from what you saw, \
         and entry = the first node. It also lands as a Draft.\n\
         - If a durable fact about the environment or the operator was learned (a path, a preference, a \
         convention), record ONE atomic memory with memory.remember.\n\
         - If neither applies, reply exactly: DISTILL: nothing\n\
         Do not repeat the task. Do not call any other tool.",
        user = excerpt(&turn.user_content, USER_EXCERPT_CHARS),
        // A plan-continuation turn's user_content is the synthesized
        // continuation brief; the plan's own goal is the request the
        // operator actually made.
        goal_line = turn
            .active_plan
            .as_ref()
            .map(|p| format!(
                "Original goal: «{}»\n",
                excerpt(&p.goal, USER_EXCERPT_CHARS)
            ))
            .unwrap_or_default(),
        reply = excerpt(reply, REPLY_EXCERPT_CHARS),
    );
    truncate_for_wire(&prompt, PARACRINE_WHISPER_PROMPT_MAX_CHARS)
}

/// Newest runs the contrast hook reads from the ledger.
const CONTRAST_LEDGER_WINDOW: usize = 20;
/// Rejected patches rendered into the contrast prompt as negative evidence.
const CONTRAST_REJECTED_LIMIT: usize = 5;

/// The paper's contrast pair over a newest-first ledger window at one graph
/// version: the newest failed run (score 0) and the newest fully successful
/// one (score 1). A model-reported completion (0.5) is neither.
pub(super) fn contrast_pair(
    runs: &[ProcedureRunRecord],
) -> Option<(&ProcedureRunRecord, &ProcedureRunRecord)> {
    let failed = runs.iter().find(|r| r.score <= 0.0)?;
    let success = runs.iter().find(|r| r.score >= 1.0)?;
    Some((failed, success))
}

fn run_line(label: &str, run: &ProcedureRunRecord) -> String {
    format!(
        "{label} run {}: verdict {} ({}), {}/{} steps verified, stalls {}, contradicted {}, non-atomic {}; \
         tools in order: {}; goal: «{}»\n",
        run.run_id,
        run.verdict,
        run.basis,
        run.steps_verified,
        run.steps_total,
        run.stalls,
        run.contradicted,
        run.non_atomic,
        if run.tool_sequence.is_empty() {
            "(none)".to_string()
        } else {
            run.tool_sequence.join(" → ")
        },
        excerpt(&run.goal, 160)
    )
}

/// Build the contrast review brief: the graph as triplets, the failed and
/// successful trajectories, the rejection memory, and the two legal outputs.
pub(super) fn build_contrast_prompt(
    procedure: &ProcedureGraphRecord,
    failed: &ProcedureRunRecord,
    success: &ProcedureRunRecord,
    rejected: &[ProcedurePatchRecord],
) -> String {
    let mut rejected_block = String::new();
    for patch in rejected.iter().take(CONTRAST_REJECTED_LIMIT) {
        rejected_block.push_str(&format!(
            "- {} — {}\n",
            patch.summary(),
            patch.rejection_reason.as_deref().unwrap_or("rejected")
        ));
    }
    let rejected_section = if rejected_block.is_empty() {
        String::new()
    } else {
        format!("Previously rejected edits — do NOT propose these again:\n{rejected_block}\n")
    };
    let prompt = format!(
        "PROCEDURE REFINE — silent lookaside. Nothing you write here reaches the operator; only your tool calls matter.\n\
         Procedure {id} v{version}: {description}\n\
         Graph as (from, RELATION, to) triplets with attributes:\n{triplets}\n\
         A run of this procedure FAILED and another SUCCEEDED at this same version.\n\
         {failed_line}{success_line}\n\
         {rejected_section}\
         Decide whether ONE small edit to the graph would have steered the failed run onto the successful path: \
         a missing verification node, a missing or wrong edge, a condition/guidance/pitfalls attribute that \
         invites the failure. Edits must be about the procedure, never about the operator's data.\n\
         - If YES: call procedure.patch ONCE with procedure_id \"{id}\", ops (at most 4: add_node, delete_node, \
         add_edge, delete_edge, set_edge_attrs, set_node_label), rationale (one or two sentences: what the failed \
         run did that the successful one did not, and how the edit prevents it), evidence_run_ids \
         [\"{failed_id}\", \"{success_id}\"]. It lands Pending for the operator; do not call it twice.\n\
         - If no single edit is justified, reply exactly: PROCEDURE: nothing\n\
         Do not call any other tool.",
        id = procedure.procedure_id,
        version = procedure.version,
        description = excerpt(&procedure.description, 400),
        triplets = procedure.render_triplets(),
        failed_line = run_line("FAILED", failed),
        success_line = run_line("SUCCEEDED", success),
        failed_id = failed.run_id,
        success_id = success.run_id,
    );
    truncate_for_wire(&prompt, PARACRINE_WHISPER_PROMPT_MAX_CHARS)
}

impl AgentRuntime {
    /// Deliver a tool-result denial for the active turn without failing the
    /// turn: the model sees `content` as the tool's output and continues.
    pub(super) async fn deliver_tool_denial(
        &mut self,
        session_id: String,
        turn_id: String,
        tool_name: String,
        content: String,
    ) -> Result<()> {
        let (chat_id, final_reply_to, final_reply_role, final_reply_guest_id) = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| {
                (
                    t.chat_id.clone(),
                    t.final_reply_to.clone(),
                    t.final_reply_role.clone(),
                    t.final_reply_guest_id.clone(),
                )
            })
            .unwrap_or_default();
        self.handle_tool_result(InboundTaskPayload {
            action: Some("tool_result".into()),
            source: Some("agent".into()),
            session_id: Some(session_id),
            turn_id: Some(turn_id),
            chat_id: Some(chat_id),
            content: Some(content),
            tool_name: Some(tool_name),
            final_reply_to: Some(final_reply_to),
            final_reply_role: Some(final_reply_role),
            final_reply_guest_id,
            ..Default::default()
        })
        .await
    }

    /// The role a distill whisper targets, with the reason for the log.
    ///
    /// 1. Operator override `PHILOTIC_SKILLS_DISTILL_ROLE`.
    /// 2. This philote's own role, when the session's bindings already hold
    ///    `skill.register` — a self-lookaside.
    /// 3. Otherwise this agent's `orchestrator` incarnation by its
    ///    agent-scoped routing key (`role:{agent_id}:orchestrator`), so a
    ///    hotel with several agents' orchestrators cannot deliver Bjork's
    ///    distill to someone else's. Live 2026-09-04: Bjork was incarnated as
    ///    `architect`, whose toolset profile has no `skill.register`; a
    ///    self-whisper could only ever decline.
    fn distill_target_role(&self, session_id: &str) -> (String, &'static str) {
        if let Ok(role) = std::env::var(ENV_DISTILL_ROLE) {
            let role = role.trim().to_string();
            if !role.is_empty() {
                return (role, "env_override");
            }
        }
        let self_can_register = self.sessions.get(session_id).is_some_and(|s| {
            s.bindings
                .effective_toolset
                .iter()
                .any(|t| t == "skill.register")
        });
        if self_can_register {
            return (
                self.role_name
                    .clone()
                    .unwrap_or_else(|| "agent".to_string()),
                "self_holds_skill_register",
            );
        }
        (
            format!("role:{}:{}", self.agent_id, DISTILL_FALLBACK_ROLE),
            "fallback_to_agent_orchestrator",
        )
    }

    /// Turn-close hook. Evaluates the predicates, consults the lane, and
    /// emits the whisper. Logs and returns on every refusal; never errors
    /// into the caller — the user's reply is already out.
    pub(super) async fn maybe_distill_after_turn(
        &mut self,
        session_id: &str,
        turn: &WorkingTurn,
        reply: &str,
    ) {
        use ansible_mesh_core::autonomy::LANE_SKILLS_DISTILL;

        let Some(trigger) = evaluate_turn(turn) else {
            return;
        };
        let tool_names: Vec<&str> = turn
            .working_tool_history
            .iter()
            .map(|(c, _)| c.tool_name.as_str())
            .collect();
        let prompt = build_distill_prompt(turn, trigger, reply);
        self.emit_distill_whisper(
            session_id,
            &turn.turn_id,
            &turn.chat_id,
            LANE_SKILLS_DISTILL,
            trigger,
            prompt,
            format!(
                "distill whisper after turn {} ({})",
                turn.turn_id,
                trigger.as_str()
            ),
            format!(
                "agent={} session={} trigger={} tool_calls={} tools=[{}]",
                self.agent_id,
                session_id,
                trigger.as_str(),
                turn.working_tool_history.len(),
                tool_names.join(",")
            ),
            "if the resulting Draft skill is unwanted, `skill.set_state <name> deprecated`; an \
             operator reversal demotes lane skills.distill",
            "distill review",
        )
        .await;
    }

    /// Procedural graphs P4: after a terminal plan eval landed on the run
    /// ledger, whisper a contrast review when that procedure now has both a
    /// failed and a successful run at its current version, and this run is
    /// one of the pair (so a pair fires once, not on every later run).
    /// Never for whispers, never for a version under trial.
    pub(super) async fn maybe_procedure_contrast_after_run(
        &mut self,
        session_id: &str,
        turn: &WorkingTurn,
        run: &ProcedureRunRecord,
    ) {
        use ansible_mesh_core::autonomy::LANE_PROCEDURES_REFINE;

        if turn.paracrine_origin.is_some() || turn.paracrine_intent.is_some() {
            return;
        }
        let runs = match self
            .ipc_client
            .send_request(IpcRequest::ListProcedureRuns {
                procedure_id: run.procedure_id.clone(),
                graph_version: Some(run.graph_version),
                limit: Some(CONTRAST_LEDGER_WINDOW),
            })
            .await
        {
            Ok(IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            }) => data
                .get("runs")
                .cloned()
                .and_then(|v| serde_json::from_value::<Vec<ProcedureRunRecord>>(v).ok())
                .unwrap_or_default(),
            other => {
                debug!(
                    session_id = %session_id,
                    procedure_id = %run.procedure_id,
                    response = ?other.as_ref().map(|_| "non-standard").unwrap_or("ipc error"),
                    "procedures.refine: ledger unavailable, no contrast"
                );
                return;
            }
        };
        let Some((failed, success)) = contrast_pair(&runs) else {
            return;
        };
        if failed.run_id != run.run_id && success.run_id != run.run_id {
            return;
        }
        let procedure = match self
            .ipc_client
            .send_request(IpcRequest::GetProcedure {
                procedure_id: run.procedure_id.clone(),
            })
            .await
        {
            Ok(IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            }) => match serde_json::from_value::<ProcedureGraphRecord>(data) {
                Ok(p) => p,
                Err(_) => return,
            },
            _ => return,
        };
        if procedure.trial_of.is_some() || procedure.version != run.graph_version {
            debug!(
                session_id = %session_id,
                procedure_id = %run.procedure_id,
                "procedures.refine: procedure on trial or moved on, no contrast"
            );
            return;
        }
        let rejected: Vec<ProcedurePatchRecord> = match self
            .ipc_client
            .send_request(IpcRequest::ListProcedurePatches {
                procedure_id: Some(run.procedure_id.clone()),
                status: Some("rejected".into()),
            })
            .await
        {
            Ok(IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            }) => data
                .get("patches")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let prompt = build_contrast_prompt(&procedure, failed, success, &rejected);
        let trigger = DistillTrigger::ProcedureContrast;
        self.emit_distill_whisper(
            session_id,
            &turn.turn_id,
            &turn.chat_id,
            LANE_PROCEDURES_REFINE,
            trigger,
            prompt,
            format!(
                "procedure contrast whisper for {} v{} (failed {} vs success {})",
                run.procedure_id, run.graph_version, failed.run_id, success.run_id
            ),
            format!(
                "agent={} session={} procedure={} version={} failed_run={} success_run={} rejected_patches={}",
                self.agent_id,
                session_id,
                run.procedure_id,
                run.graph_version,
                failed.run_id,
                success.run_id,
                rejected.len()
            ),
            "reject the Pending patch with `phil procedure reject <patch_id>`; an operator reversal \
             demotes lane procedures.refine",
            "procedure refine",
        )
        .await;
    }

    /// Shared whisper plumbing for every distill-family trigger: lane kill
    /// switch, `ConsumeAutonomyAction { filing: true }`, target role, the
    /// paracrine thread, and `ParacrineEmit` with `Discard` routing. The
    /// intent is `skills.distill:<trigger>` for every lane, so the allowlist
    /// and the clean-context rules apply uniformly.
    #[allow(clippy::too_many_arguments)]
    async fn emit_distill_whisper(
        &mut self,
        session_id: &str,
        turn_id: &str,
        chat_id: &str,
        lane: &str,
        trigger: DistillTrigger,
        prompt: String,
        action_summary: String,
        evidence: String,
        reversal_hint: &str,
        thread_kind: &str,
    ) {
        use ansible_mesh_core::autonomy::{AutonomyLane, lane_enabled};

        let lane_handle = AutonomyLane::new(lane);
        if !lane_enabled(&lane_handle, |k| std::env::var(k).ok()) {
            debug!(
                session_id = %session_id,
                lane,
                trigger = trigger.as_str(),
                "distill-family predicate fired but lane kill switch is set"
            );
            return;
        }
        let consume = self
            .ipc_client
            .send_request(IpcRequest::ConsumeAutonomyAction {
                lane: lane.into(),
                action_summary,
                evidence,
                reversal_hint: reversal_hint.into(),
                filing: true,
            })
            .await;
        let (allowed, reason, audit_id) = match &consume {
            Ok(IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            }) => (
                data.get("allowed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                data.get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                data.get("audit_id")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string()),
            ),
            Ok(other) => {
                warn!(
                    session_id = %session_id,
                    lane,
                    response = ?other,
                    "unexpected ConsumeAutonomyAction response"
                );
                (false, "unexpected_response".into(), None)
            }
            Err(e) => {
                warn!(
                    session_id = %session_id,
                    lane,
                    error = %e,
                    "ConsumeAutonomyAction IPC failed"
                );
                (false, "ipc_error".into(), None)
            }
        };
        if !allowed {
            info!(
                session_id = %session_id,
                lane,
                trigger = trigger.as_str(),
                reason = %reason,
                "predicate fired; lane refused the whisper"
            );
            return;
        }

        let (role, role_reason) = self.distill_target_role(session_id);
        info!(
            session_id = %session_id,
            lane,
            role = %role,
            reason = role_reason,
            "whisper target role selected"
        );
        let paracrine_id = Uuid::new_v4().to_string();
        let node_id = local_node_id();
        let reply_guest_id = self
            .role_name
            .as_ref()
            .map(|rn| format!("{}:{}", self.agent_id, rn))
            .unwrap_or_else(|| self.agent_id.clone());
        let exosome = Exosome {
            prompt: prompt.clone(),
            context: Some(serde_json::json!({
                "intent": format!("{INTENT}:{}", trigger.as_str()),
                "trigger": trigger.as_str(),
                "lane": lane,
                "source_turn_id": turn_id,
                "audit_id": audit_id,
            })),
            paracrine_id: Some(paracrine_id.clone()),
            response_routing: Some(ParacrineRouting::Discard),
            source_session_id: Some(session_id.to_string()),
            source_chat_id: (!chat_id.is_empty()).then(|| chat_id.to_string()),
        };

        if let Some(state) = self.sessions.get_mut(session_id) {
            state.open_paracrine_thread(
                paracrine_id.clone(),
                role.clone(),
                format!("{thread_kind} ({})", trigger.as_str()),
                ParacrineRouting::Discard,
                "advice_only".into(),
                "distill".into(),
                "originating_session".into(),
            );
        }

        let emit = self
            .ipc_client
            .send_request(IpcRequest::ParacrineEmit {
                role: role.clone(),
                exosome,
                reply_to_node: node_id,
                reply_to_role: "agent".to_string(),
                reply_to_guest_id: Some(reply_guest_id),
                timeout_secs: None,
            })
            .await;
        match emit {
            Ok(IpcResponse::Standard {
                ok: false,
                code,
                message,
                ..
            }) => {
                warn!(
                    session_id = %session_id,
                    lane,
                    role = %role,
                    code = %code,
                    message = %message,
                    "hotel refused the whisper"
                );
                if let Some(state) = self.sessions.get_mut(session_id) {
                    state.close_paracrine_thread(
                        &paracrine_id,
                        ParacrineThreadStatus::Cancelled,
                        None,
                        Some(format!("hotel refused: {code}")),
                    );
                }
            }
            Ok(_) => {
                info!(
                    session_id = %session_id,
                    lane,
                    role = %role,
                    trigger = trigger.as_str(),
                    paracrine_id = %paracrine_id,
                    audit_id = ?audit_id,
                    "whisper emitted"
                );
            }
            Err(e) => {
                warn!(
                    session_id = %session_id,
                    lane,
                    error = %e,
                    "ParacrineEmit IPC failed"
                );
                if let Some(state) = self.sessions.get_mut(session_id) {
                    state.close_paracrine_thread(
                        &paracrine_id,
                        ParacrineThreadStatus::Cancelled,
                        None,
                        Some(format!("ipc error: {e}")),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::r#loop::{ToolCall, ToolResult};

    fn turn_with(history: Vec<(&str, &str)>, user: &str) -> WorkingTurn {
        let mut turn = WorkingTurn::test_turn("t-1", user);
        turn.working_tool_history = history
            .into_iter()
            .map(|(name, res)| {
                (
                    ToolCall {
                        tool_name: name.into(),
                        arguments: serde_json::json!({}),
                    },
                    ToolResult {
                        tool_name: name.into(),
                        content: res.into(),
                    },
                )
            })
            .collect();
        turn
    }

    #[test]
    fn tool_count_predicate_fires_at_threshold() {
        let five = vec![("a", "ok"); 5];
        assert_eq!(
            evaluate_turn(&turn_with(five, "do the thing")),
            Some(DistillTrigger::ToolCount)
        );
        let four = vec![("a", "ok"); 4];
        assert_eq!(evaluate_turn(&turn_with(four, "do the thing")), None);
    }

    #[test]
    fn error_recovered_predicate() {
        let t = turn_with(
            vec![("bash.exec", "Error: no such file"), ("bash.exec", "done")],
            "list it",
        );
        assert_eq!(evaluate_turn(&t), Some(DistillTrigger::ErrorRecovered));
        // Ending in error is not a recovery.
        let t = turn_with(
            vec![("bash.exec", "ok"), ("bash.exec", "{\"success\": false}")],
            "list it",
        );
        assert_eq!(evaluate_turn(&t), None);
    }

    #[test]
    fn user_correction_predicate_needs_a_tool() {
        let t = turn_with(
            vec![("life.observe", "recorded")],
            "No, I meant the other one",
        );
        assert_eq!(evaluate_turn(&t), Some(DistillTrigger::UserCorrection));
        let t = turn_with(vec![], "No, I meant the other one");
        assert_eq!(evaluate_turn(&t), None);
        let t = turn_with(vec![("life.observe", "recorded")], "Nothing to do today");
        assert_eq!(evaluate_turn(&t), None);
    }

    #[test]
    fn whisper_turns_never_trigger() {
        let mut t = turn_with(vec![("a", "ok"); 9], "distill review");
        t.paracrine_origin = Some("pid".into());
        assert_eq!(evaluate_turn(&t), None);
        let mut t = turn_with(vec![("a", "ok"); 9], "x");
        t.paracrine_intent = Some("skills.distill:tool_count".into());
        assert_eq!(evaluate_turn(&t), None);
        assert!(turn_is_distill(&t));
    }

    #[test]
    fn prompt_is_bounded_and_names_the_no_op() {
        let big = "x".repeat(20_000);
        let t = turn_with(vec![("a", big.as_str()); 6], &big);
        let p = build_distill_prompt(&t, DistillTrigger::ToolCount, &big);
        assert!(p.chars().count() <= PARACRINE_WHISPER_PROMPT_MAX_CHARS);
        assert!(p.contains("DISTILL: nothing"));
    }

    #[test]
    fn origin_maps_from_intent() {
        assert_eq!(
            origin_from_intent("skills.distill").as_deref(),
            Some("distill")
        );
        assert_eq!(
            origin_from_intent("skills.distill:error_recovered").as_deref(),
            Some("distill:error_recovered")
        );
        assert_eq!(origin_from_intent("steward.checkin"), None);
    }

    #[test]
    fn allowlist_is_narrow() {
        assert!(tool_allowed("skill.register"));
        assert!(tool_allowed("memory.remember"));
        assert!(!tool_allowed("skill.assign"));
        assert!(!tool_allowed("bash.exec"));
    }

    // ── Procedural graphs P4 ──────────────────────────────────────────────

    fn run(id: &str, score: f32, tools: &[&str]) -> ProcedureRunRecord {
        ProcedureRunRecord {
            run_id: id.into(),
            procedure_id: "outcome-reflex".into(),
            graph_version: 1,
            verdict: if score >= 1.0 { "complete" } else { "blocked" }.into(),
            basis: "grounded".into(),
            steps_total: 3,
            steps_verified: if score >= 1.0 { 3 } else { 1 },
            tool_sequence: tools.iter().map(|t| t.to_string()).collect(),
            goal: "record the outcome".into(),
            score,
            ..Default::default()
        }
    }

    #[test]
    fn contrast_pair_needs_a_failure_and_a_full_success() {
        let runs = vec![
            run("r3", 0.5, &["life.observe"]),
            run("r2", 0.0, &["life.observe"]),
            run("r1", 1.0, &["life.recall", "life.observe", "life.commit"]),
        ];
        let (f, s) = contrast_pair(&runs).expect("pair");
        assert_eq!(f.run_id, "r2");
        assert_eq!(s.run_id, "r1");
        assert!(contrast_pair(&[run("a", 1.0, &[]), run("b", 0.5, &[])]).is_none());
        assert!(contrast_pair(&[run("a", 0.0, &[])]).is_none());
    }

    #[test]
    fn contrast_prompt_is_bounded_and_carries_graph_runs_and_rejections() {
        use ansible_mesh_core::procedure::{
            ProcedurePatchOp, ProcedurePatchRecord, outcome_reflex_procedure,
        };
        let procedure = outcome_reflex_procedure();
        let failed = run("r2", 0.0, &["life.observe"]);
        let success = run("r1", 1.0, &["life.recall", "life.observe", "life.commit"]);
        let rejected = vec![ProcedurePatchRecord {
            patch_id: "p0".into(),
            procedure_id: "outcome-reflex".into(),
            ops: vec![ProcedurePatchOp::DeleteNode {
                id: "recall".into(),
            }],
            rejection_reason: Some(
                "trial: candidate v2 mean 0.00 over 2 run(s) < baseline v1 mean 0.50 over 2 run(s)"
                    .into(),
            ),
            ..Default::default()
        }];
        let p = build_contrast_prompt(&procedure, &failed, &success, &rejected);
        assert!(p.starts_with("PROCEDURE REFINE"));
        assert!(p.contains("(observe, LEADS_TO, commit)"), "{p}");
        assert!(p.contains("FAILED run r2"), "{p}");
        assert!(p.contains("SUCCEEDED run r1"), "{p}");
        assert!(
            p.contains("life.recall → life.observe → life.commit"),
            "{p}"
        );
        assert!(p.contains("do NOT propose these again"), "{p}");
        assert!(p.contains("delete_node recall"), "{p}");
        assert!(p.contains("PROCEDURE: nothing"), "{p}");
        assert!(p.contains("evidence_run_ids [\"r2\", \"r1\"]"), "{p}");
        assert!(p.chars().count() <= PARACRINE_WHISPER_PROMPT_MAX_CHARS);
        let none = build_contrast_prompt(&procedure, &failed, &success, &[]);
        assert!(!none.contains("do NOT propose"));
        assert!(tool_allowed("procedure.patch"));
        assert!(tool_allowed("procedure.get"));
        assert!(tool_allowed("procedure.register"));
        assert_eq!(
            DistillTrigger::ProcedureContrast.as_str(),
            "procedure_contrast"
        );
        assert_eq!(
            origin_from_intent("skills.distill:procedure_contrast").as_deref(),
            Some("distill:procedure_contrast")
        );
    }
}

#[cfg(test)]
mod tool_result_is_error_tests {
    use super::tool_result_is_error;

    #[test]
    fn hotel_refusal_payload_text_is_an_error() {
        assert!(tool_result_is_error(
            "only agent guests may request subagent delegation | kind=ipc_failure | \
             code=SUBAGENT_FORBIDDEN | component=aiua | retryable=true"
        ));
    }

    #[test]
    fn wholesale_batch_rejection_is_an_error() {
        assert!(tool_result_is_error(
            r#"{"data":{"evaluation":{"next_action":["8 item(s) failed validation and were never written — fix the payload"],"written":0}}}"#
        ));
    }

    #[test]
    fn successful_batch_and_plain_ok_text_are_not_errors() {
        assert!(!tool_result_is_error(
            r#"{"data":{"evaluation":{"next_action":["all observations landed durably"],"written":8},"failed":0}}"#
        ));
        assert!(!tool_result_is_error(
            "Skill 'music.repertoire-gardener' registered (state: validated)."
        ));
    }
}
