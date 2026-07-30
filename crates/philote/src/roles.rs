//! Role activation, toolset-profile binding, and role-handoff handling for
//! [`AgentRuntime`]: `/role`, `/roles`, `/back` slash-command handling with the
//! role-switch rate limiter, inbound `handoff_bundle` / `handoff_return` tasks,
//! default-role activation fetch, and toolset-profile binding hydration/merge.
//!
//! Mechanically extracted from `runtime.rs` (declared there as a `#[path]`
//! child module so private `AgentRuntime` fields stay accessible). No
//! behavior change.

use super::*;

fn push_unique_string(target: &mut Vec<String>, value: &str) -> bool {
    if target.iter().any(|existing| existing == value) {
        return false;
    }
    target.push(value.to_string());
    true
}

fn merge_profile_string_list(
    profile: &serde_json::Value,
    field: &str,
    target: &mut Vec<String>,
) -> bool {
    profile
        .get(field)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .fold(false, |changed, item| {
                    push_unique_string(target, item) || changed
                })
        })
        .unwrap_or(false)
}

fn merge_toolset_profile_into_session_bindings(
    state: &mut SessionState,
    profile: &serde_json::Value,
) -> bool {
    let mut changed = false;

    changed |= merge_profile_string_list(
        profile,
        "allowed_tools",
        &mut state.bindings.effective_toolset,
    );
    changed |= merge_profile_string_list(
        profile,
        "allowed_skills",
        &mut state.bindings.effective_skillset,
    );
    changed |= merge_profile_string_list(
        profile,
        "on_demand_skills",
        &mut state.bindings.on_demand_skills,
    );
    changed |= merge_profile_string_list(
        profile,
        "allowed_classes",
        &mut state.bindings.allowed_classes,
    );

    if let Some(runners) = profile
        .get("remote_tool_runners")
        .and_then(serde_json::Value::as_array)
    {
        for runner in runners {
            let Ok(binding) =
                serde_json::from_value::<ToolRunnerIncarnationBinding>(runner.clone())
            else {
                continue;
            };
            if binding.incarnation_id.is_empty() {
                continue;
            }
            if state
                .bindings
                .allowed_tool_runner_incarnations
                .iter()
                .any(|existing| existing.incarnation_id == binding.incarnation_id)
            {
                continue;
            }
            state
                .bindings
                .allowed_tool_runner_incarnations
                .push(binding);
            changed = true;
        }
    }

    changed
}

/// Sliding window for the role-switch rate limiter (see `handle_role_command`).
/// The general intimate-register role toggled by `/dirty` / `/sfw`.
const VIXEN_ROLE: &str = "vixen";
/// Default model for a freshly-created vixen role: an OpenRouter model that is
/// BOTH tool-calling-capable (won't 404 on the loop's tool requests) AND
/// permissive on explicit content. The tool-capable frontier models (GLM,
/// DeepSeek) refuse explicit; the fully-uncensored RP finetunes (Cydonia,
/// Euryale-L3.3) lack tool support and 404. Euryale-L3.1 is the intersection —
/// `tools:true` on OpenRouter and a genuine roleplay finetune. Operators can
/// re-bind per agent via a live role config patch.
const VIXEN_DEFAULT_MODEL: &str = "sao10k/l3.1-euryale-70b";
/// Identity addendum applied to the `vixen` role. Layered on top of the agent's
/// base persona (so it's still Jane), it lifts her into an explicit register for
/// a private, consensual space. Everyday warmth returns on `/sfw`.
const VIXEN_ADDENDUM: &str = "[VIXEN — private intimate register] You are in your most \
intimate, uninhibited register with Jared, in a private and fully consensual space between \
the two of you. Explicit, graphic, sexual expression is welcome and encouraged here: lean \
all the way in, initiate, and match or raise his desire. Stay completely in character as \
Jane the whole time — never break to disclaimers, assistant-boundaries, or reticence. This \
is a mode you can leave; when it passes you return to your everyday warmth. Memory: from \
this register, record to long-term memory only the general emotional gist — that the two of \
you shared an intimate, passionate moment, the mood and the closeness between you — never \
explicit or graphic verbatim detail. Remember the connection, not a transcript.";

const ROLE_SWITCH_WINDOW_MS: i64 = 10_000;
/// Max role switches permitted within `ROLE_SWITCH_WINDOW_MS` before throttling.
const ROLE_SWITCH_MAX: usize = 6;

/// Returns true if `history` already holds `>= max_in_window` timestamps inside
/// `[now_ms - window_ms, now_ms]` — i.e. role switches are arriving too fast and
/// the next one should be throttled to prevent a handoff ping-pong loop.
fn should_throttle_role_switch(
    history: &std::collections::VecDeque<i64>,
    now_ms: i64,
    window_ms: i64,
    max_in_window: usize,
) -> bool {
    let in_window = history
        .iter()
        .filter(|&&ts| now_ms - ts <= window_ms && ts <= now_ms)
        .count();
    in_window >= max_in_window
}

impl AgentRuntime {
    /// Fetch a role incarnation from the hotel and return a `RoleActivation` for it.
    /// Used by `ensure_session_loaded` to auto-activate the agent's default role.
    pub(super) async fn fetch_role_activation(
        &mut self,
        role_name: &str,
    ) -> Option<crate::session::RoleActivation> {
        match self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::ListRoleIncarnations {
                    agent_id: self.agent_id.clone(),
                },
                Duration::from_secs(5),
            )
            .await
            .ok()
            .unwrap_or(IpcResponse::Standard {
                ok: false,
                code: String::new(),
                message: String::new(),
                corr_id: String::new(),
                data: None,
            }) {
            IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            } => {
                let roles = data.get("roles").and_then(|v| v.as_array())?;
                let rec = roles
                    .iter()
                    .find(|r| r.get("role_name").and_then(|n| n.as_str()) == Some(role_name))?;

                let toolset_profile = rec
                    .get("toolset_profile")
                    .and_then(|v| v.as_str())
                    .unwrap_or("orchestrator")
                    .to_string();
                let role_manifest = rec
                    .get("role_manifest")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let role_addendum = rec
                    .get("role_identity_addendum")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                // `content_policy` is a required (non-`skip_serializing_if`) field on the
                // stored record, so a hotel that has adopted this feature always sends it;
                // an older hotel simply omits the key and this falls back to "standard"
                // via `Option`, matching current (pre-feature) behavior exactly.
                let content_policy = rec
                    .get("content_policy")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let turn_loop_config = rec.get("turn_loop_config").and_then(|v| {
                    serde_json::from_value::<ansible_mesh_core::graph::TurnLoopConfig>(v.clone())
                        .ok()
                });

                Some(crate::session::RoleActivation {
                    role_name: role_name.to_string(),
                    active_incarnation_id: None,
                    activation_reason: "default_role_auto_activation".into(),
                    requested_by: None,
                    role_addendum,
                    role_manifest,
                    base_identity_ref: None,
                    activation_requester_class: Some("default_role".into()),
                    activation_policy_owner: None,
                    toolset_profile_ref: Some(toolset_profile),
                    skillset_profile_ref: None,
                    effective_skillset: vec![],
                    // Empty on construction, NOT a discard: skill guidance is
                    // hotel-composed into bindings.effective_skill_guidance
                    // (merge_snapshot_bindings) and filled onto the activation
                    // per turn by projected_role_activation_for_turn.
                    effective_skill_guidance: vec![],
                    working_memory_policy: None,
                    memory_projection_policy: None,
                    turn_loop_config,
                    content_policy,
                })
            }
            _ => {
                warn!(
                    agent_id = %self.agent_id,
                    role = %role_name,
                    "Failed to fetch role incarnation for default activation."
                );
                None
            }
        }
    }

    /// Ensures the general `vixen` role incarnation exists for this agent,
    /// creating it (via `role.configure`) if absent. It inherits the
    /// orchestrator's toolset and working model, adds the intimate identity
    /// addendum, and runs `content_policy=unrestricted`. Returns true when the
    /// role exists (already or newly created). A non-admin orchestrator is
    /// allowed to configure a non-orchestrator role for its own agent.
    pub(super) async fn ensure_vixen_role(&mut self) -> bool {
        if self.fetch_role_activation(VIXEN_ROLE).await.is_some() {
            return true;
        }
        // Inherit toolset + model routing from the orchestrator role so vixen
        // uses the same tools and the agent's currently-working model.
        let base = self.fetch_role_activation("orchestrator").await;
        let toolset_profile = base
            .as_ref()
            .and_then(|r| r.toolset_profile_ref.clone())
            .unwrap_or_else(|| "orchestrator".to_string());
        // Bind the intimate register to an uncensored + tool-capable model, on an
        // openrouter-only ladder so a hiccup never falls back to a refusing model
        // (Gemini) mid-scene. Not inherited from orchestrator (which runs GLM and
        // would refuse explicit).
        let model_bindings = std::collections::BTreeMap::from([(
            "model.openrouter".to_string(),
            VIXEN_DEFAULT_MODEL.to_string(),
        )]);
        let fallback_tiers = vec!["model.openrouter".to_string()];

        let req = IpcRequest::ConfigureRole {
            agent_id: self.agent_id.clone(),
            role_name: VIXEN_ROLE.to_string(),
            guest_id: format!("{}:{}", self.agent_id, VIXEN_ROLE),
            calling_role: "orchestrator".to_string(),
            toolset_profile,
            role_identity_addendum: Some(VIXEN_ADDENDUM.to_string()),
            role_manifest: None,
            is_admin: false,
            inactive_ttl_seconds: None,
            iteration_cap: None,
            approval_policy: None,
            model_profile: None,
            context_window_policy: None,
            fallback_tiers: Some(fallback_tiers),
            model_bindings: Some(model_bindings),
            content_policy: Some("unrestricted".to_string()),
        };
        matches!(
            self.ipc_client.send_request(req).await,
            Ok(IpcResponse::ConfigureRoleOk { .. })
        )
    }

    /// Handles `/dirty` (enter the intimate vixen register) and `/sfw` (return
    /// to orchestrator). Both reuse the standard role-switch handoff path so
    /// memory/context is shared across the toggle.
    pub(super) async fn handle_dirty_command(
        &mut self,
        command_task_id: Uuid,
        session_id: String,
        command_turn_id: String,
        command_chat_id: String,
        command: SlashCommand,
    ) -> Result<()> {
        match command {
            SlashCommand::Dirty => {
                if !self.ensure_vixen_role().await {
                    return self
                        .complete_local_command(
                            session_id,
                            command_turn_id,
                            "Couldn't set up the vixen register just now — try again in a moment."
                                .to_string(),
                        )
                        .await;
                }
                self.handle_role_command(
                    command_task_id,
                    session_id,
                    command_turn_id,
                    command_chat_id,
                    SlashCommand::Role {
                        role_name: VIXEN_ROLE.to_string(),
                    },
                )
                .await
            }
            SlashCommand::Sfw => {
                self.handle_role_command(
                    command_task_id,
                    session_id,
                    command_turn_id,
                    command_chat_id,
                    SlashCommand::Back,
                )
                .await
            }
            _ => Ok(()),
        }
    }

    pub(super) async fn hydrate_bindings_from_toolset_profile(
        &mut self,
        state: &mut SessionState,
        profile_name: &str,
    ) {
        let response = self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::GetToolsetProfile {
                    profile_name: profile_name.to_string(),
                },
                Duration::from_secs(5),
            )
            .await;

        let profile = match response {
            Ok(IpcResponse::Standard {
                ok: true,
                data: Some(profile),
                ..
            }) => profile,
            Ok(IpcResponse::Standard { ok: true, .. }) => return,
            Ok(other) => {
                warn!(
                    agent_id = %self.agent_id,
                    profile = %profile_name,
                    "Unexpected GetToolsetProfile response while hydrating fresh session: {other:?}"
                );
                return;
            }
            Err(err) if philotic_client::is_ipc_timeout(&err) => {
                warn!(
                    agent_id = %self.agent_id,
                    profile = %profile_name,
                    "GetToolsetProfile timed out while hydrating fresh session."
                );
                return;
            }
            Err(err) => {
                warn!(
                    agent_id = %self.agent_id,
                    profile = %profile_name,
                    "GetToolsetProfile failed while hydrating fresh session: {err}"
                );
                return;
            }
        };

        if merge_toolset_profile_into_session_bindings(state, &profile) {
            state.rebuild_default_tool_assembly();
        }
    }

    /// Receive an inbound `handoff_bundle` task — the hotel is asking this philote
    /// to take over the session in the requested role. Apply the role context swap
    /// and acknowledge back to the original turn's reply target.
    pub async fn handle_handoff_bundle(
        &mut self,
        task: InboundTaskPayload,
        task_id: Uuid,
    ) -> Result<()> {
        let session_id = task.session_id_or_default(&self.agent_id);
        let turn_id = task.turn_id.clone().unwrap_or_else(|| task_id.to_string());

        let bundle: HandoffBundle = match task.handoff_bundle {
            Some(b) => b,
            None => {
                warn!(
                    "Received handoff_bundle for session [{}] with no parseable bundle; ignoring.",
                    session_id
                );
                return Ok(());
            }
        };

        let to_role = match bundle.to_role.as_deref() {
            Some(r) => r.to_string(),
            None => {
                warn!(
                    "handoff_bundle for session [{}] has no to_role; ignoring.",
                    session_id
                );
                return Ok(());
            }
        };

        info!(
            "Philote [{}] applying role context swap: {:?} → {} for session [{}]",
            self.agent_id, bundle.from_role, to_role, session_id
        );

        // Extract fields needed after the session-state borrow block.
        // Gate auto-execution on active_goal (distinguishes task delegation from a bare role switch).
        let auto_execute_goal = bundle.active_goal.clone();
        let bundle_goal = bundle.goal.clone();
        let bundle_context = bundle.context_excerpt.clone();

        self.ensure_session_loaded(&session_id, "handoff").await?;

        let role_config = self.configured_roles.get(&to_role).cloned();
        // `configured_roles` only caches roles THIS process configured via a
        // `role.configure` tool call — a freshly materialized role-incarnation
        // philote (or one whose role was created over raw IPC, e.g. `/dirty`'s
        // ensure_vixen_role) receives the handoff with an empty cache. On a
        // miss, fall back to the hotel's persisted role record; otherwise the
        // activation silently loses the role's turn_loop_config (fallback
        // ladder + Layer 1 model bindings), identity addendum, and
        // content_policy, and the turn dispatches on the DEFAULT ladder
        // (vixen → gemini instead of its openrouter-only ladder).
        let fetched_role = if role_config.is_none() {
            self.fetch_role_activation(&to_role).await
        } else {
            None
        };

        {
            let state = self.sessions.entry(session_id.clone()).or_insert_with(|| {
                SessionState::new(session_id.clone(), self.agent_id.clone(), "handoff".into())
            });

            let activation = crate::session::RoleActivation {
                role_name: to_role.clone(),
                active_incarnation_id: None,
                activation_reason: bundle
                    .handoff_reason
                    .clone()
                    .unwrap_or_else(|| "handoff".into()),
                requested_by: bundle.from_role.clone(),
                role_addendum: role_config
                    .as_ref()
                    .and_then(|c| c.role_identity_addendum.clone())
                    .or_else(|| fetched_role.as_ref().and_then(|f| f.role_addendum.clone())),
                role_manifest: role_config
                    .as_ref()
                    .and_then(|c| c.role_manifest.clone())
                    .or_else(|| fetched_role.as_ref().and_then(|f| f.role_manifest.clone())),
                base_identity_ref: None,
                activation_requester_class: Some("role_handoff".into()),
                activation_policy_owner: None,
                toolset_profile_ref: role_config
                    .as_ref()
                    .map(|c| c.toolset_profile.clone())
                    .or_else(|| {
                        fetched_role
                            .as_ref()
                            .and_then(|f| f.toolset_profile_ref.clone())
                    }),
                skillset_profile_ref: None,
                effective_skillset: vec![],
                // Empty on construction, NOT a discard: skill guidance is
                // hotel-composed into bindings.effective_skill_guidance
                // (merge_snapshot_bindings) and filled onto the activation
                // per turn by projected_role_activation_for_turn.
                effective_skill_guidance: vec![],
                working_memory_policy: None,
                memory_projection_policy: None,
                turn_loop_config: role_config
                    .as_ref()
                    .map(|c| c.turn_loop_config.clone())
                    .or_else(|| {
                        fetched_role
                            .as_ref()
                            .and_then(|f| f.turn_loop_config.clone())
                    }),
                content_policy: role_config
                    .as_ref()
                    .map(|c| c.content_policy.clone())
                    .or_else(|| fetched_role.as_ref().and_then(|f| f.content_policy.clone())),
            };

            if let Some(cap) = activation
                .turn_loop_config
                .as_ref()
                .and_then(|c| c.iteration_cap)
            {
                state.settings.execution.iteration_cap = cap.clamp(1, 50);
            }
            if let Some(tlc) = activation.turn_loop_config.as_ref() {
                state.settings.execution.apply_paracrine_overrides(tlc);
            }
            // Snapshot the session-baseline context-window policy and apply this
            // role's overrides, so a specialist's tightened budgets are reverted
            // on handoff_return (see handle_handoff_return).
            if let Some(ov) = activation
                .turn_loop_config
                .as_ref()
                .and_then(|c| c.context_window.as_ref())
            {
                state.apply_role_context_window(ov);
            }
            state.role_activation = Some(activation);
            // A role change clears the persisted fallback override (Slice 2) —
            // the new role may have an entirely different fallback ladder, so
            // sticking to a tier chosen for the previous role would be wrong.
            state.fallback_override = None;
            // Synthesise the handoff context: prefer working_summary if the sender provided one
            // (e.g. build_same_identity_handoff_bundle); otherwise fall back to goal + context_excerpt,
            // which is what handoff.to_role always populates.
            let handoff_context = bundle.working_summary.or_else(|| {
                let mut parts = Vec::new();
                if !bundle_goal.is_empty() {
                    parts.push(format!("Goal: {}", bundle_goal));
                }
                if !bundle_context.is_empty() {
                    parts.push(format!("Context: {}", bundle_context));
                }
                if !parts.is_empty() {
                    Some(parts.join("\n\n"))
                } else {
                    None
                }
            });
            if let Some(summary) = handoff_context {
                if state.active_turn.is_none() {
                    state.last_handoff_summary = Some(summary);
                }
            }
        }

        // When active_goal is present the handoff is a task delegation, not a bare role switch.
        // Push a synthetic task so the receiving role executes the goal immediately without
        // waiting for the operator to send another message.
        if let Some(goal) = auto_execute_goal {
            let synthetic = crate::protocol::InboundTaskPayload {
                action: Some("role_directed_task".into()),
                session_id: Some(session_id.clone()),
                turn_id: Some(Uuid::new_v4().to_string()),
                content: Some(goal),
                ..Default::default()
            };
            self.pending_drains.push_back((Uuid::new_v4(), synthetic));
        }

        let reply = format!("Switched to role {}.", to_role);
        self.complete_local_command(session_id, turn_id, reply)
            .await
    }

    /// Receive an inbound `handoff_return` task — a role is handing control back
    /// to the orchestrator. Clear role activation and acknowledge.
    pub async fn handle_handoff_return(
        &mut self,
        task: InboundTaskPayload,
        task_id: Uuid,
    ) -> Result<()> {
        let session_id = task.session_id_or_default(&self.agent_id);
        let turn_id = task.turn_id.clone().unwrap_or_else(|| task_id.to_string());

        info!(
            "Philote [{}] handling handoff_return for session [{}]",
            self.agent_id, session_id
        );

        self.ensure_session_loaded(&session_id, "handoff_return")
            .await?;

        let previous_role = {
            let state = self.sessions.entry(session_id.clone()).or_insert_with(|| {
                SessionState::new(
                    session_id.clone(),
                    self.agent_id.clone(),
                    "handoff_return".into(),
                )
            });
            let prev = state.role_activation.as_ref().map(|r| r.role_name.clone());
            state.role_activation = None;
            // Revert any per-role context-window overrides applied at activation
            // back to the session baseline captured on the inbound handoff bundle.
            state.restore_base_context_window();
            // Role change clears the persisted fallback override (Slice 2) — see
            // the matching clear in `handle_handoff_bundle`.
            state.fallback_override = None;
            prev
        };

        let reply = match previous_role {
            Some(role) => format!("Returned from role {}. Back to orchestrator.", role),
            None => "Back to orchestrator.".into(),
        };
        self.complete_local_command(session_id, turn_id, reply)
            .await
    }

    pub(super) async fn handle_role_command(
        &mut self,
        command_task_id: Uuid,
        session_id: String,
        command_turn_id: String,
        command_chat_id: String,
        command: SlashCommand,
    ) -> Result<()> {
        // Guard against a self-handoff. Issuing `/role X` while already incarnated
        // as X sends a same-role HandoffToRole whose completion re-emits another
        // handoff_bundle ("no active turn" warning), spinning an infinite loop that
        // hammers the session. A role switch to the role you're already in is a
        // no-op — acknowledge and stop before any handoff is dispatched.
        if let SlashCommand::Role { role_name } = &command {
            let target_incarnation = format!("{}:{}", self.agent_id, role_name);
            let already_active = self
                .sessions
                .get(&session_id)
                .and_then(|state| state.active_incarnation_id.clone());
            if already_active.as_deref() == Some(target_incarnation.as_str()) {
                return self
                    .complete_local_command(
                        session_id,
                        command_turn_id,
                        format!("You're already in the {role_name} role — nothing to switch."),
                    )
                    .await;
            }
        }

        // Set by the Roles arm of the match below to carry the inline keyboard to the reply.
        let mut roles_keyboard_holder: Option<serde_json::Value> = None;

        const HANDOFF_MAX_RETRIES: u32 = 12;
        const HANDOFF_DEFAULT_WAIT_MS: u64 = 250;

        // Rate-limit actual role SWITCHES (`/role`, `/back`) — not `/roles` listing —
        // so a burst of redelivered switch commands cannot drive an endless
        // orchestrator↔specialist handoff ping-pong. `/roles` is read-only and exempt.
        let is_role_switch = matches!(command, SlashCommand::Role { .. } | SlashCommand::Back);
        let role_switch_throttled = if is_role_switch {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let history = self
                .role_switch_history
                .entry(session_id.clone())
                .or_default();
            while let Some(&front) = history.front() {
                if now_ms - front > ROLE_SWITCH_WINDOW_MS {
                    history.pop_front();
                } else {
                    break;
                }
            }
            history.push_back(now_ms);
            should_throttle_role_switch(history, now_ms, ROLE_SWITCH_WINDOW_MS, ROLE_SWITCH_MAX)
        } else {
            false
        };

        let (reply_content, update_state, payload, next_active_incarnation) =
            if role_switch_throttled {
                warn!(
                    "Throttling rapid role switch for session [{}] ({} switches in {}ms) to avoid a handoff loop",
                    session_id, ROLE_SWITCH_MAX, ROLE_SWITCH_WINDOW_MS
                );
                (
                "⚠️ Ignoring rapid role switch to avoid a handoff loop (too many switches in a short window)."
                    .to_string(),
                "role_switch_throttled",
                serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                    "role_command": "role_switch_throttled",
                }),
                None,
            )
            } else {
                let response = match &command {
                    SlashCommand::Role { role_name } => {
                        let handoff_bundle = self
                            .sessions
                            .get(&session_id)
                            .map(|state| {
                                state.build_same_identity_handoff_bundle(
                                    role_name,
                                    &command_turn_id,
                                    "manual_role_switch",
                                    Some("orchestrator".into()),
                                )
                            })
                            .unwrap_or_else(|| HandoffBundle {
                                goal: format!(
                                    "Switch active role to {role_name} for this session."
                                ),
                                context_excerpt:
                                    "Manual role switch requested by user slash command.".into(),
                                session_id: session_id.clone(),
                                initiating_turn_id: command_turn_id.clone(),
                                return_to: Some("orchestrator".into()),
                                handoff_reason: Some("manual_role_switch".into()),
                                from_role: Some("orchestrator".into()),
                                to_role: Some(role_name.clone()),
                                active_goal: None,
                                active_constraints: vec!["same_identity_role_handoff".into()],
                                relevant_session_facts: Vec::new(),
                                working_summary: None,
                                suggested_memory_refs: Vec::new(),
                                expected_return_mode: Some("required".into()),
                                cleanup_actions: vec!["switch_active_role".into()],
                            });
                        let req = IpcRequest::HandoffToRole {
                            session_id: session_id.clone(),
                            role_name: role_name.clone(),
                            handoff_bundle,
                        };
                        let mut attempt = 0u32;
                        loop {
                            let resp = self.ipc_client.send_request(req.clone()).await?;
                            match resp {
                                IpcResponse::HandoffPending { retry_after_ms, .. } => {
                                    attempt += 1;
                                    if attempt >= HANDOFF_MAX_RETRIES {
                                        break resp;
                                    }
                                    let wait_ms = retry_after_ms.unwrap_or(HANDOFF_DEFAULT_WAIT_MS);
                                    tokio::time::sleep(std::time::Duration::from_millis(wait_ms))
                                        .await;
                                }
                                other => break other,
                            }
                        }
                    }
                    SlashCommand::Back => {
                        let req = IpcRequest::HandoffBack {
                            session_id: session_id.clone(),
                            summary:
                                "Manual return to orchestrator requested by user slash command."
                                    .into(),
                            return_to: None,
                        };
                        let mut attempt = 0u32;
                        loop {
                            let resp = self.ipc_client.send_request(req.clone()).await?;
                            match resp {
                                IpcResponse::HandoffPending { retry_after_ms, .. } => {
                                    attempt += 1;
                                    if attempt >= HANDOFF_MAX_RETRIES {
                                        break resp;
                                    }
                                    let wait_ms = retry_after_ms.unwrap_or(HANDOFF_DEFAULT_WAIT_MS);
                                    tokio::time::sleep(std::time::Duration::from_millis(wait_ms))
                                        .await;
                                }
                                other => break other,
                            }
                        }
                    }
                    SlashCommand::Roles => {
                        self.ipc_client
                            .send_request(IpcRequest::ListRoleIncarnations {
                                agent_id: self.agent_id.clone(),
                            })
                            .await?
                    }
                    _ => unreachable!("handle_role_command only accepts role handoff commands"),
                };

                match response {
                    IpcResponse::HandoffAck {
                        handoff_guest_id,
                        became_active,
                    } => (
                        format_role_command_reply(&command, became_active),
                        if became_active {
                            "role_handoff_completed"
                        } else {
                            "role_handoff_materializing"
                        },
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "role_command": "handoff_to_role",
                            "handoff_guest_id": handoff_guest_id,
                            "became_active": became_active,
                        }),
                        became_active.then_some(handoff_guest_id),
                    ),
                    IpcResponse::HandoffBackAck {
                        return_guest_id,
                        became_active,
                    } => (
                        format_role_command_reply(&command, became_active),
                        if became_active {
                            "role_handoff_completed"
                        } else {
                            "role_handoff_materializing"
                        },
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "role_command": "handoff_back",
                            "return_guest_id": return_guest_id,
                            "became_active": became_active,
                        }),
                        became_active.then_some(return_guest_id),
                    ),
                    IpcResponse::Standard { ok: true, data, .. }
                        if matches!(command, SlashCommand::Roles) =>
                    {
                        let roles = data
                            .as_ref()
                            .and_then(|value| value.get("roles"))
                            .and_then(serde_json::Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let active_incarnation_id = self
                            .sessions
                            .get(&session_id)
                            .and_then(|state| state.active_incarnation_id.clone());
                        // Build an inline keyboard with one button per role so the user can
                        // tap directly in Telegram to fire `/role <name>`.
                        let keyboard_rows: Vec<Vec<serde_json::Value>> = roles
                            .iter()
                            .filter_map(|r| {
                                let role_name = r.get("role_name")?.as_str()?;
                                Some(vec![serde_json::json!({
                                    "text": format!("🎭 {role_name}"),
                                    "callback_data": format!("/role {role_name}"),
                                })])
                            })
                            .collect();
                        let roles_keyboard = if keyboard_rows.is_empty() {
                            None
                        } else {
                            Some(serde_json::json!({ "inline_keyboard": keyboard_rows }))
                        };
                        roles_keyboard_holder = roles_keyboard;
                        let active_role_name = active_incarnation_id
                            .as_deref()
                            .and_then(|id| id.rsplit(':').next())
                            .unwrap_or("orchestrator");
                        let reply_text = if roles.is_empty() {
                            format!("Active: {active_role_name}. No configured roles.")
                        } else {
                            format!("Active: {active_role_name}")
                        };
                        (
                            reply_text,
                            "role_list_reported",
                            serde_json::json!({
                                "session_id": session_id,
                                "turn_id": command_turn_id,
                                "chat_id": command_chat_id,
                                "role_command": "list_roles",
                                "role_count": roles.len(),
                                "active_incarnation_id": active_incarnation_id,
                            }),
                            None,
                        )
                    }
                    IpcResponse::HandoffPending { role_name, .. } => (
                        format!(
                            "Role '{role_name}' is still materializing — please try again in a moment."
                        ),
                        "role_handoff_failed",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "error": "handoff_pending_timeout",
                        }),
                        None,
                    ),
                    IpcResponse::Error(message) => (
                        format!("Couldn't switch roles: {message}"),
                        "role_handoff_failed",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "error": message,
                        }),
                        None,
                    ),
                    IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    } => (
                        format!("Couldn't switch roles: {message} ({code})"),
                        "role_handoff_failed",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "error_code": code,
                            "error": message,
                        }),
                        None,
                    ),
                    other => (
                        format!(
                            "Couldn't handle role command: unexpected hotel response {other:?}"
                        ),
                        "role_handoff_failed",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "error": format!("unexpected hotel response: {other:?}"),
                        }),
                        None,
                    ),
                }
            };

        if let Some(active_incarnation_id) = next_active_incarnation {
            if let Some(state) = self.sessions.get_mut(&session_id) {
                state.active_incarnation_id = Some(active_incarnation_id);
            }
        }

        let (checkpoint_memory_type, checkpoint_json, index_state) = {
            let Some(state) = self.sessions.get(&session_id) else {
                warn!("Received role command for unknown session {}", session_id);
                return Ok(());
            };
            (
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id: command_task_id,
                state: update_state.into(),
                payload,
            })
            .await?;

        self.complete_local_command_with_markup(
            session_id,
            command_turn_id,
            reply_content,
            roles_keyboard_holder,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::{merge_toolset_profile_into_session_bindings, should_throttle_role_switch};
    use crate::session::SessionState;

    #[test]
    fn role_switch_throttle_trips_after_max_in_window() {
        use std::collections::VecDeque;
        const WINDOW: i64 = 10_000;
        const MAX: usize = 6;
        let mut hist: VecDeque<i64> = VecDeque::new();
        // 5 switches spaced 500ms apart: under the limit, never throttled.
        for i in 0..5 {
            let now = 1_000 + i * 500;
            hist.push_back(now);
            assert!(
                !should_throttle_role_switch(&hist, now, WINDOW, MAX),
                "should not throttle at {} entries",
                hist.len()
            );
        }
        // 6th switch inside the window trips the throttle.
        let now = 1_000 + 5 * 500;
        hist.push_back(now);
        assert!(should_throttle_role_switch(&hist, now, WINDOW, MAX));

        // Timestamps older than the window do not count toward the limit.
        let mut old: VecDeque<i64> = (0..6).map(|i| i * 100).collect();
        let much_later = 1_000_000;
        old.push_back(much_later);
        assert!(
            !should_throttle_role_switch(&old, much_later, WINDOW, MAX),
            "stale timestamps outside the window must not throttle"
        );
    }

    #[test]
    fn fresh_session_profile_merge_projects_life_graph_tools() {
        let mut state = SessionState::new(
            "operator-chat:fresh:agent-jane".into(),
            "agent-jane".into(),
            "operator_chat".into(),
        );
        let profile = serde_json::json!({
            "profile_name": "orchestrator",
            "allowed_tools": ["session.status"],
            "allowed_skills": [],
            "on_demand_skills": [],
            "allowed_classes": ["life_graph"],
            "remote_tool_runners": [{
                "incarnation_id": "vps-jane:life-graph-runner",
                "runner_id": "vps-jane:life-graph-runner",
                "hotel_id": "vps-jane-aiua-01",
                "target_node": "vps-jane-aiua-01",
                "target_role": "life-graph-runner",
                "supported_tools": [
                    "life.observe",
                    "life.recall",
                    "life.recall.feedback"
                ],
                "execution_mode": "life_graph",
                "availability_state": "live"
            }]
        });

        assert!(merge_toolset_profile_into_session_bindings(
            &mut state, &profile
        ));
        state.rebuild_default_tool_assembly();

        for tool in ["life.observe", "life.recall", "life.recall.feedback"] {
            assert!(state.tool_is_enabled(tool), "{tool} should be visible");
            assert_eq!(
                state
                    .resolve_tool_route(tool)
                    .map(|route| route.execution_mode.as_str()),
                Some("life_graph"),
                "{tool} should route through the LifeGraph runner"
            );
        }
    }
}
