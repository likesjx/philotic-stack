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

/// Intent marker carried in the exosome's `context.intent`, prefixing the
/// trigger name (`skills.distill:tool_count`). Recognised by the tool layer.
pub(super) const INTENT: &str = "skills.distill";

/// The only tools a distill lookaside turn may call. Anything else is
/// refused at dispatch with a tool-result denial, regardless of the role's
/// default toolset. `skill.assign`/`skill.set_state` are deliberately absent:
/// a distilled skill is a proposal and must not be able to grant itself.
pub(super) const TOOL_ALLOWLIST: &[&str] = &[
    "skill.register",
    "skill.list",
    "memory.remember",
    "memory.recall",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DistillTrigger {
    ToolCount,
    ErrorRecovered,
    UserCorrection,
}

impl DistillTrigger {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            DistillTrigger::ToolCount => "tool_count",
            DistillTrigger::ErrorRecovered => "error_recovered",
            DistillTrigger::UserCorrection => "user_correction",
        }
    }
}

/// Is this turn a distill lookaside (serving a whisper with our intent)?
pub(super) fn turn_is_distill(turn: &WorkingTurn) -> bool {
    turn.paracrine_intent
        .as_deref()
        .is_some_and(|i| i == INTENT || i.starts_with(&format!("{INTENT}:")))
}

pub(super) fn tool_allowed(tool_name: &str) -> bool {
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
pub(super) fn tool_result_is_error(content: &str) -> bool {
    let head: String = content.trim_start().chars().take(64).collect();
    let head_l = head.to_ascii_lowercase();
    if head_l.starts_with("error") || head_l.starts_with("failed") || head_l.starts_with("denied") {
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
    };
    let prompt = format!(
        "DISTILL REVIEW — silent lookaside. Nothing you write here reaches the operator; only your tool calls matter.\n\
         A turn just completed and {trigger_line}.\n\n\
         User asked: «{user}»\n\
         Tools used, in order:\n{tools}\
         Final reply: «{reply}»\n\n\
         Decide whether this was a reusable procedure worth naming.\n\
         - If YES: call skill.register ONCE with skill_name (lowercase dotted, e.g. research.github-digest), \
         description (one sentence, when to use it), subagent_kind \"philote-worker\", goal (the procedure as a \
         template with {{{{placeholders}}}} for the parts that vary), and allowed_tools = exactly the tools used above. \
         It lands as a Draft for the operator to review; do not assign it, do not register a second one.\n\
         - If a durable fact about the environment or the operator was learned (a path, a preference, a \
         convention), record ONE atomic memory with memory.remember.\n\
         - If neither applies, reply exactly: DISTILL: nothing\n\
         Do not repeat the task. Do not call any other tool.",
        user = excerpt(&turn.user_content, USER_EXCERPT_CHARS),
        reply = excerpt(reply, REPLY_EXCERPT_CHARS),
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

    /// The role a self-whisper targets: the operator override, else this
    /// philote's own role (`agent` for a default philote, the role name for a
    /// role incarnation — the hotel resolves names to routing keys).
    fn distill_target_role(&self) -> String {
        if let Ok(role) = std::env::var(ENV_DISTILL_ROLE) {
            let role = role.trim().to_string();
            if !role.is_empty() {
                return role;
            }
        }
        self.role_name
            .clone()
            .unwrap_or_else(|| "agent".to_string())
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
        use ansible_mesh_core::autonomy::{AutonomyLane, LANE_SKILLS_DISTILL, lane_enabled};

        let Some(trigger) = evaluate_turn(turn) else {
            return;
        };
        let lane = AutonomyLane::new(LANE_SKILLS_DISTILL);
        if !lane_enabled(&lane, |k| std::env::var(k).ok()) {
            debug!(
                session_id = %session_id,
                trigger = trigger.as_str(),
                "skills.distill: predicate fired but lane kill switch is set"
            );
            return;
        }

        let tool_names: Vec<&str> = turn
            .working_tool_history
            .iter()
            .map(|(c, _)| c.tool_name.as_str())
            .collect();
        let consume = self
            .ipc_client
            .send_request(IpcRequest::ConsumeAutonomyAction {
                lane: LANE_SKILLS_DISTILL.into(),
                action_summary: format!(
                    "distill whisper after turn {} ({})",
                    turn.turn_id,
                    trigger.as_str()
                ),
                evidence: format!(
                    "agent={} session={} trigger={} tool_calls={} tools=[{}]",
                    self.agent_id,
                    session_id,
                    trigger.as_str(),
                    turn.working_tool_history.len(),
                    tool_names.join(",")
                ),
                reversal_hint: "if the resulting Draft skill is unwanted, `skill.set_state <name> \
                                deprecated`; an operator reversal demotes lane skills.distill"
                    .into(),
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
                    response = ?other,
                    "skills.distill: unexpected ConsumeAutonomyAction response"
                );
                (false, "unexpected_response".into(), None)
            }
            Err(e) => {
                warn!(
                    session_id = %session_id,
                    error = %e,
                    "skills.distill: ConsumeAutonomyAction IPC failed"
                );
                (false, "ipc_error".into(), None)
            }
        };
        if !allowed {
            info!(
                session_id = %session_id,
                trigger = trigger.as_str(),
                reason = %reason,
                "skills.distill: predicate fired; lane refused the whisper"
            );
            return;
        }

        let role = self.distill_target_role();
        let prompt = build_distill_prompt(turn, trigger, reply);
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
                "source_turn_id": turn.turn_id,
                "audit_id": audit_id,
            })),
            paracrine_id: Some(paracrine_id.clone()),
            response_routing: Some(ParacrineRouting::Discard),
            source_session_id: Some(session_id.to_string()),
            source_chat_id: (!turn.chat_id.is_empty()).then(|| turn.chat_id.clone()),
        };

        if let Some(state) = self.sessions.get_mut(session_id) {
            state.open_paracrine_thread(
                paracrine_id.clone(),
                role.clone(),
                format!("distill review ({})", trigger.as_str()),
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
                    role = %role,
                    code = %code,
                    message = %message,
                    "skills.distill: hotel refused the whisper"
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
                    role = %role,
                    trigger = trigger.as_str(),
                    paracrine_id = %paracrine_id,
                    audit_id = ?audit_id,
                    "skills.distill: whisper emitted"
                );
            }
            Err(e) => {
                warn!(
                    session_id = %session_id,
                    error = %e,
                    "skills.distill: ParacrineEmit IPC failed"
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
}
