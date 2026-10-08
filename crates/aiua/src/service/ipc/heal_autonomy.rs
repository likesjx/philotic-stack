//! Heal queue and autonomy lanes: heal entries/events, work items, autonomy actions/outcomes, model-route oracle.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

impl IpcServer {
    /// Handle [`IpcRequest::QueryModelRoute`] — the routing oracle's IPC face.
    ///
    /// Ranks the local hotel's live model profiles against the caller's need
    /// (pure `model_oracle::rank_models_with`), maps providers onto controller
    /// roles, and keeps only roles backed by a live guest — the same
    /// reachability rule `validate_fallback_ladders` enforces at config time.
    /// When the query carries `exclude_providers` (a failure-driven reroute),
    /// the switch is logged and pushed to the heal queue as an
    /// `oracle_reroute` info entry so reroute patterns surface in dev briefs.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle_query_model_route(
        graph: &GraphDomain,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        local_node_id: &str,
        request_class: &str,
        needs_tools: bool,
        needs_structured: bool,
        approx_context_tokens: u32,
        latency_class: &str,
        trust_ceiling: &str,
        exclude_providers: &[String],
        now_secs: u64,
    ) -> IpcResponse {
        use ansible_mesh_core::model_oracle as oracle;

        const CORR: &str = "query_model_route";

        if oracle::routing_oracle_disabled() {
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({ "ranked": [], "disabled": true })),
            );
        }

        let profiles = match graph.list_model_profiles() {
            Ok(p) => p,
            Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
        };
        // Prefer profiles observed on this node; fall back to the full mesh
        // view when the local node has no profiles yet (fresh hotel).
        let local: Vec<_> = profiles
            .iter()
            .filter(|p| p.node_id == local_node_id)
            .cloned()
            .collect();
        let candidates = if local.is_empty() { profiles } else { local };

        let need = oracle::RouteNeed {
            request_class: request_class.to_string(),
            needs_tools,
            needs_structured,
            approx_context_tokens,
            latency_class: oracle::LatencyClass::parse(latency_class),
            trust_ceiling: trust_ceiling.to_string(),
        };
        let ranked = oracle::rank_models_with(
            &candidates,
            &need,
            now_secs,
            oracle::degrade_cooloff_secs_from_env(),
        );

        // A tier is reachable iff a live guest serves its role — same rule as
        // validate_fallback_ladders.
        let active_roles: std::collections::BTreeSet<String> =
            Self::local_hotel_name(graph, local_node_id)
                .and_then(|hotel| graph.list_guests(&hotel, true).ok())
                .map(|guests| guests.into_iter().map(|g| g.role).collect())
                .unwrap_or_default();

        let mut seen_roles: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let entries: Vec<serde_json::Value> = ranked
            .iter()
            .filter(|r| !exclude_providers.iter().any(|x| x == &r.provider))
            .filter_map(|r| {
                let role = oracle::controller_role_for_provider(&r.provider);
                if !active_roles.contains(&role) || !seen_roles.insert(role.clone()) {
                    return None;
                }
                Some(serde_json::json!({
                    "role": role,
                    "provider": r.provider,
                    "model_ref": r.model_ref,
                    "score": r.score,
                    "reasons": r.reasons,
                }))
            })
            .take(3)
            .collect();

        // Observability: a non-empty exclude list means a provider just
        // failed and the oracle is rerouting around it.
        if !exclude_providers.is_empty() {
            if let Some(first) = entries.first() {
                let provider_from = exclude_providers.join(",");
                let provider_to = first
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                info!(
                    provider_from = %provider_from,
                    provider_to = %provider_to,
                    reason = "fallback_ladder_exhausted",
                    "Routing oracle reroute"
                );
                if let Some(hq) = heal_queue {
                    let raw_text = format!(
                        "[model-oracle] reroute {provider_from} -> {provider_to} (fallback ladder exhausted, request_class={request_class})"
                    );
                    match hq.push_error("model-oracle", &raw_text) {
                        Ok(id) => {
                            if let Err(e) =
                                hq.update_triage(&id, "info", "oracle_reroute", "oracle_reroute")
                            {
                                warn!("oracle_reroute heal entry triage failed: {e}");
                            }
                        }
                        Err(e) => warn!("oracle_reroute heal entry push failed: {e}"),
                    }
                }
            }
        }

        IpcResponse::success(
            CORR,
            Some(serde_json::json!({ "ranked": entries, "disabled": false })),
        )
    }

    /// Turn-failure heal intake (self-heal): classify a FailTask's
    /// `error_code`/`reason` and, when it carries provider/model failure
    /// markers (`kind=provider_failure | component=model-router | provider=X`
    /// or a bare `MODEL_EMPTY_RESPONSE`), push a pre-triaged heal-queue entry
    /// so the heal-dispatcher and A3 recurrence counter see turn-level
    /// failures. Best-effort: never affects the FailTask response.
    ///
    /// Entry shape:
    /// - `guest_id`: `model-controller-{provider}` when the provider marker is
    ///   present (the model-controller guest naming convention), else
    ///   `turn:{caller_guest_id}`.
    /// - `severity`/`pattern_tag`: from the shared classifier
    ///   (`provider_4xx:{provider}`, `provider_timeout:{provider}`,
    ///   `model_empty_response`, …) so the dispatcher aggregates without
    ///   re-classifying.
    /// - `raw_text`: `[{error_code}] {reason}` capped to 2 KB.
    ///
    /// Flood control lives in `push_classified`: the same
    /// `(guest_id, pattern_tag)` within the flood window collapses.
    pub(crate) fn push_turn_failure_heal_entry(
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        caller_guest_id: Option<&str>,
        error_code: &str,
        reason: &str,
    ) {
        use ansible_mesh_core::heal_queue::{cap_turn_failure_text, classify_turn_failure};

        let Some(hq) = heal_queue else {
            return;
        };
        // Classify on the full line (markers may sit at the end), then cap
        // what gets stored.
        let full_line = format!("[{error_code}] {reason}");
        let Some(class) = classify_turn_failure(&full_line) else {
            return;
        };
        let line = cap_turn_failure_text(&full_line);
        let guest_id = match class.provider.as_deref() {
            Some(provider) => format!("model-controller-{provider}"),
            None => format!("turn:{}", caller_guest_id.unwrap_or("unknown")),
        };
        match hq.push_classified(&guest_id, &line, &class.severity, &class.pattern_tag) {
            Ok(Some(id)) => info!(
                id = %id,
                guest_id = %guest_id,
                pattern_tag = %class.pattern_tag,
                "turn failure pushed to heal queue"
            ),
            Ok(None) => debug!(
                guest_id = %guest_id,
                pattern_tag = %class.pattern_tag,
                "turn failure collapsed into recent heal entry (flood window)"
            ),
            Err(e) => warn!("turn failure heal push failed: {e}"),
        }
    }

    /// Handle [`IpcRequest::PushHealEvent`] — a guest-reported, pre-classified
    /// turn-level failure (philote watchdog evictions, fallback-ladder
    /// exhaustion, paracrine budget breaches). Stored pre-triaged; flood
    /// control collapses the same `(guest_id, pattern_tag)` within the window.
    pub(crate) fn handle_push_heal_event(
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        guest_id: &str,
        severity: &str,
        pattern_tag: &str,
        detail: &str,
    ) -> IpcResponse {
        use ansible_mesh_core::heal_queue::cap_turn_failure_text;

        const CORR: &str = "push_heal_event";
        let Some(hq) = heal_queue else {
            return IpcResponse::error(
                CORR,
                "UNAVAILABLE",
                "heal_queue not configured".to_string(),
            );
        };
        let detail = cap_turn_failure_text(detail);
        match hq.push_classified(guest_id, &detail, severity, pattern_tag) {
            Ok(Some(id)) => {
                info!(
                    id = %id,
                    guest_id = %guest_id,
                    pattern_tag = %pattern_tag,
                    "guest heal event pushed to heal queue"
                );
                IpcResponse::success(
                    CORR,
                    Some(serde_json::json!({ "collapsed": false, "id": id })),
                )
            }
            Ok(None) => IpcResponse::success(CORR, Some(serde_json::json!({ "collapsed": true }))),
            Err(e) => IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e}")),
        }
    }

    pub(crate) fn handle_file_heal_work_item(
        graph: &GraphDomain,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        pattern_tag: &str,
        guest_id: &str,
        occurrence_count: u32,
        window_secs: u64,
        evidence_lines: &[String],
        now: u64,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> IpcResponse {
        use ansible_mesh_core::autonomy::{
            AutonomyAuditRecord, AutonomyLane, LANE_FLEET_HEAL_SLICES, lane_enabled,
            try_consume_daily_action,
        };
        use ansible_mesh_core::heal_queue::{
            HEAL_WORK_ITEM_STATUS_OPEN, HealWorkItemRecord, cap_evidence_lines,
        };
        use ansible_mesh_core::provenance::{ProvenanceEnvelope, TrustTier};

        const CORR: &str = "file_heal_work_item";
        let lane = AutonomyLane::new(LANE_FLEET_HEAL_SLICES);

        // 1. Kill switch overrides everything, always (Autonomy Contract rule 3).
        if !lane_enabled(&lane, env) {
            debug!(
                pattern_tag,
                guest_id, "heal work item filing skipped: lane kill switch set"
            );
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "filed": false, "deduped": false, "reason": "lane_disabled"
                })),
            );
        }

        // 2. Dedup: one OPEN work item per (pattern_tag, guest_id). A re-breach
        // while open bumps count + last_seen — no budget consumed, no new audit.
        match graph.find_open_heal_work_item(pattern_tag, guest_id) {
            Ok(Some(mut item)) => {
                item.count = item.count.saturating_add(occurrence_count);
                item.last_seen = now;
                if let Err(e) = graph.upsert_heal_work_item(&item) {
                    return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
                }
                info!(
                    pattern_tag,
                    guest_id,
                    work_item_id = %item.work_item_id,
                    count = item.count,
                    "heal work item re-breach: bumped open item"
                );
                return IpcResponse::success(
                    CORR,
                    Some(serde_json::json!({
                        "filed": false, "deduped": true,
                        "work_item_id": item.work_item_id,
                    })),
                );
            }
            Ok(None) => {}
            Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
        }

        // 3. Grant + budget: frozen lanes and exhausted daily budgets refuse.
        let mut grant = match graph.get_or_create_autonomy_grant(LANE_FLEET_HEAL_SLICES, now) {
            Ok(grant) => grant,
            Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
        };
        if !try_consume_daily_action(&mut grant, now) {
            let reason = if grant.frozen_until_operator_review {
                "lane_frozen"
            } else {
                "daily_budget_exhausted"
            };
            debug!(
                pattern_tag,
                guest_id, reason, "heal work item filing refused by autonomy grant"
            );
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "filed": false, "deduped": false, "reason": reason
                })),
            );
        }
        if let Err(e) = graph.upsert_autonomy_grant(&grant) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }

        // 4. File: audit record first (the ledger), then the work item node.
        let evidence = cap_evidence_lines(evidence_lines);
        let work_item_id = Uuid::new_v4().to_string();
        let audit_id = format!("heal_filing:{work_item_id}");
        // Memory Transparency Slice M1: component-authored provenance for
        // the A3 heal filing — evidence pointers are the same evidence
        // lines already captured on the work item, so no new plumbing.
        let provenance = ProvenanceEnvelope::from_component("heal-dispatcher")
            .with_source(pattern_tag)
            .with_trust(TrustTier::Observed)
            .with_evidence(evidence.clone())
            .with_reversal(format!(
                "close_heal_work_item({work_item_id}) via GraphDomain::close_heal_work_item"
            ));
        let audit = AutonomyAuditRecord::new(
            audit_id.clone(),
            lane,
            format!(
                "filed heal work item {work_item_id}: pattern '{pattern_tag}' on guest \
                 '{guest_id}' recurred {occurrence_count}x within {window_secs}s"
            ),
            &evidence.join("\n"),
            "close the work item (GraphDomain::close_heal_work_item)",
            grant.posture,
            now,
        )
        .with_provenance(provenance);
        if let Err(e) = graph.record_autonomy_audit(&audit) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }
        let item = HealWorkItemRecord {
            work_item_id: work_item_id.clone(),
            pattern_tag: pattern_tag.to_string(),
            guest_id: guest_id.to_string(),
            count: occurrence_count,
            window_secs,
            evidence,
            status: HEAL_WORK_ITEM_STATUS_OPEN.to_string(),
            filed_by: "heal-dispatcher".to_string(),
            audit_id: Some(audit_id.clone()),
            created_at: now,
            last_seen: now,
        };
        if let Err(e) = graph.upsert_heal_work_item(&item) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }

        // 5. Operator visibility: one resolved info entry in the heal queue so
        // the filing surfaces in existing monitoring. Best-effort — the graph
        // nodes above are the durable record.
        if let Some(hq) = heal_queue {
            match hq.push_error(
                guest_id,
                &format!(
                    "work_item_filed: recurring pattern '{pattern_tag}' on {guest_id} \
                     ({occurrence_count}x/{window_secs}s) -> heal_work_item {work_item_id}"
                ),
            ) {
                Ok(entry_id) => {
                    if let Err(e) =
                        hq.update_triage(&entry_id, "info", pattern_tag, "work_item_filed")
                    {
                        warn!("heal work item info entry triage failed: {e:#}");
                    }
                    if let Err(e) = hq.resolve(&entry_id, "work_item_filed") {
                        warn!("heal work item info entry resolve failed: {e:#}");
                    }
                }
                Err(e) => warn!("heal work item info entry push failed: {e:#}"),
            }

            // 6. A9 Piece 3: an UNRESOLVED, throttled pending-outcome
            // notice — deliberately distinct from the `work_item_filed`
            // entry above, which is immediately `.resolve()`d and would
            // make a poor "still awaiting review" breadcrumb. Best-effort;
            // the audit record above is the durable one. Throttled per
            // (lane, pattern_tag) by `push_classified`'s own flood window.
            let notice = ansible_mesh_core::autonomy::pending_outcome_notice(
                &audit_id,
                LANE_FLEET_HEAL_SLICES,
                &audit.action_summary,
            );
            match hq.push_classified(
                LANE_FLEET_HEAL_SLICES,
                &notice,
                "info",
                "autonomy_outcome_pending",
            ) {
                Ok(Some(id)) => info!(
                    id,
                    audit_id = %audit_id,
                    "heal work item filing: pending-outcome notice pushed to heal queue"
                ),
                Ok(None) => debug!(
                    audit_id = %audit_id,
                    "heal work item filing: pending-outcome notice collapsed (flood window)"
                ),
                Err(e) => warn!("heal work item filing: pending-outcome notice push failed: {e:#}"),
            }
        }

        info!(
            pattern_tag,
            guest_id,
            work_item_id = %work_item_id,
            occurrence_count,
            window_secs,
            "heal work item filed via fleet.heal_slices lane"
        );
        IpcResponse::success(
            CORR,
            Some(serde_json::json!({
                "filed": true, "deduped": false,
                "work_item_id": work_item_id,
                "audit_id": audit_id,
            })),
        )
    }

    /// Handle [`IpcRequest::ConsumeAutonomyAction`] — a guest asking to take
    /// one autonomous action on `lane` (first consumer: the life-graph
    /// runner's feedback-to-action loop, lane `graph.bridge_edges`).
    ///
    /// Pipeline mirrors A3's `handle_file_heal_work_item`: lane kill switch →
    /// grant posture → daily budget (`try_consume_daily_action`, which also
    /// enforces the freeze flag) → Pending `autonomy_audit` record.
    ///
    /// Decision table (`data` in the Standard response):
    /// - kill switch set → `{allowed:false, reason:"lane_disabled"}`
    /// - posture ProposalOnly → `{allowed:false, posture:"proposal_only",
    ///   reason:"posture_proposal_only"}` — no budget, no audit; the caller
    ///   stays prose-only. This is every fresh lane's day-one answer.
    /// - frozen / budget exhausted → `{allowed:false, reason:...}`
    /// - posture ConfirmFirst → `{allowed:false, posture:"confirm_first",
    ///   audit_id}` — the caller files a ready-to-apply spec awaiting
    ///   operator confirmation.
    /// - posture AutoWithAudit → `{allowed:true, posture:"auto_with_audit",
    ///   audit_id}` — the caller acts now; the audit record is the ledger.
    ///
    /// Clock (`now`) and env reader are injected so tests run without
    /// wall-clock time or process environment.
    #[cfg(test)]
    pub(crate) fn handle_consume_autonomy_action(
        graph: &GraphDomain,
        lane: &str,
        action_summary: &str,
        evidence: &str,
        reversal_hint: &str,
        now: u64,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> IpcResponse {
        Self::handle_consume_autonomy_action_ext(
            graph,
            lane,
            action_summary,
            evidence,
            reversal_hint,
            false,
            now,
            env,
        )
    }

    /// [`Self::handle_consume_autonomy_action`] with the `filing` flag.
    ///
    /// A filing (a Draft skill, a proposal record) is what `ProposalOnly`
    /// *means* a lane may do, so `filing = true` is permitted at every
    /// posture — still kill-switch-gated, still budgeted, still audited
    /// `Pending` so the operator's outcome stamp trains the lane. `filing =
    /// false` keeps the original decision table (ProposalOnly refuses).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle_consume_autonomy_action_ext(
        graph: &GraphDomain,
        lane: &str,
        action_summary: &str,
        evidence: &str,
        reversal_hint: &str,
        filing: bool,
        now: u64,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> IpcResponse {
        use ansible_mesh_core::autonomy::{
            AutonomyAuditRecord, AutonomyLane, AutonomyPosture, lane_enabled,
            try_consume_daily_action,
        };

        const CORR: &str = "consume_autonomy_action";
        if lane.trim().is_empty() {
            return IpcResponse::error(CORR, "INVALID_LANE", "lane must not be empty");
        }
        let lane_id = AutonomyLane::new(lane);

        // 1. Kill switch overrides everything, always (Autonomy Contract rule 3).
        if !lane_enabled(&lane_id, env) {
            debug!(lane, "autonomy action refused: lane kill switch set");
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "allowed": false, "reason": "lane_disabled"
                })),
            );
        }

        // 2. Grant posture. ProposalOnly consumes nothing — the lane has not
        // earned filing actions yet, so the caller stays prose-only.
        let mut grant = match graph.get_or_create_autonomy_grant(lane, now) {
            Ok(grant) => grant,
            Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
        };
        if grant.posture == AutonomyPosture::ProposalOnly && !filing {
            debug!(lane, "autonomy action refused: posture proposal_only");
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "allowed": false,
                    "posture": "proposal_only",
                    "reason": "posture_proposal_only",
                })),
            );
        }

        // 3. Budget: frozen lanes and exhausted daily budgets refuse.
        if !try_consume_daily_action(&mut grant, now) {
            let reason = if grant.frozen_until_operator_review {
                "lane_frozen"
            } else {
                "daily_budget_exhausted"
            };
            debug!(lane, reason, "autonomy action refused by grant");
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "allowed": false,
                    "posture": posture_str(grant.posture),
                    "reason": reason,
                })),
            );
        }
        if let Err(e) = graph.upsert_autonomy_grant(&grant) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }

        // 4. Audit record first (the ledger) — Pending until the operator
        // confirms or reverses via RecordAutonomyOutcome.
        let audit_id = format!("autonomy:{}:{}", lane, Uuid::new_v4());
        let audit = AutonomyAuditRecord::new(
            audit_id.clone(),
            lane_id,
            action_summary,
            evidence,
            reversal_hint,
            grant.posture,
            now,
        );
        if let Err(e) = graph.record_autonomy_audit(&audit) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }

        let allowed = filing || grant.posture == AutonomyPosture::AutoWithAudit;
        info!(
            lane,
            audit_id = %audit_id,
            posture = posture_str(grant.posture),
            allowed,
            filing,
            "autonomy action consulted"
        );
        IpcResponse::success(
            CORR,
            Some(serde_json::json!({
                "allowed": allowed,
                "posture": posture_str(grant.posture),
                "audit_id": audit_id,
            })),
        )
    }

    /// Handle [`IpcRequest::RecordAutonomyOutcome`] — the operator/steward
    /// reporting the reviewed outcome of an audited autonomous action
    /// (Autopoiesis Slice A9 — `trust-ledger`).
    ///
    /// `outcome`: `"confirmed_good"` → audit `ConfirmedGood` + grant
    /// `Outcome::ConfirmedGood` (counts toward promotion); `"reversed"` →
    /// audit `Reversed` + grant `Outcome::OperatorReversal` (demotes one
    /// posture level); `"neutral"` → audit `Neutral` only — the grant's
    /// earn/demote counters are untouched (a wash, not a signal). Idempotent
    /// per audit id: an already-reviewed audit refuses with
    /// `reason:"already_recorded"` so a double-confirm never double-counts
    /// toward promotion.
    pub(crate) fn handle_record_autonomy_outcome(
        graph: &GraphDomain,
        audit_id: &str,
        outcome: &str,
        now: u64,
    ) -> IpcResponse {
        use ansible_mesh_core::autonomy::{AuditOutcome, Outcome, Transition};

        const CORR: &str = "record_autonomy_outcome";
        let (audit_outcome, grant_outcome): (AuditOutcome, Option<Outcome>) = match outcome {
            "confirmed_good" => (AuditOutcome::ConfirmedGood, Some(Outcome::ConfirmedGood)),
            "reversed" => (AuditOutcome::Reversed, Some(Outcome::OperatorReversal)),
            "neutral" => (AuditOutcome::Neutral, None),
            other => {
                return IpcResponse::error(
                    CORR,
                    "INVALID_OUTCOME",
                    format!(
                        "unknown outcome '{other}' (expected confirmed_good | reversed | neutral)"
                    ),
                );
            }
        };

        let audit = match graph.get_autonomy_audit(audit_id) {
            Ok(Some(audit)) => audit,
            Ok(None) => {
                return IpcResponse::error(
                    CORR,
                    "AUDIT_NOT_FOUND",
                    format!("no autonomy_audit record with id '{audit_id}'"),
                );
            }
            Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
        };
        if audit.outcome != AuditOutcome::Pending {
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "recorded": false,
                    "reason": "already_recorded",
                    "lane": audit.lane.as_str(),
                })),
            );
        }

        if let Err(e) = graph.set_autonomy_audit_outcome(audit_id, audit_outcome, now) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }
        // Neutral carries no grant_outcome — it stamps the audit record and
        // stops there (see AuditOutcome::Neutral doc).
        let transition = match grant_outcome {
            Some(grant_outcome) => {
                match graph.record_autonomy_outcome(audit.lane.as_str(), grant_outcome, now) {
                    Ok(transition) => transition,
                    Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
                }
            }
            None => Transition::NoChange,
        };
        let transition_str = match transition {
            Transition::NoChange => "no_change",
            Transition::Promoted { .. } => "promoted",
            Transition::Demoted { .. } => "demoted",
            Transition::Frozen => "frozen",
        };
        let posture = graph
            .get_autonomy_grant(audit.lane.as_str())
            .ok()
            .flatten()
            .map(|g| posture_str(g.posture));

        info!(
            audit_id,
            lane = audit.lane.as_str(),
            outcome,
            transition = transition_str,
            "autonomy outcome recorded"
        );
        IpcResponse::success(
            CORR,
            Some(serde_json::json!({
                "recorded": true,
                "lane": audit.lane.as_str(),
                "transition": transition_str,
                "posture": posture,
            })),
        )
    }

    /// Handle `GetConfig("__autonomy_status__")` /
    /// `GetConfig("__autonomy_status__:{lane}")` — the per-lane trust-ledger
    /// report `phil autonomy status` reads (Autopoiesis Slice A9). Read-only:
    /// computed straight from the persisted [`AutonomyGrant`](ansible_mesh_core::autonomy::AutonomyGrant)s
    /// via [`ansible_mesh_core::autonomy::lane_status_report`], no new state.
    ///
    /// `lane = None` → JSON array of every granted lane's report (lanes
    /// never consulted have no grant yet and are omitted — there is nothing
    /// to report). `lane = Some(l)` → JSON of that lane's report, or JSON
    /// `null` if lane `l` has no grant yet.
    pub(crate) fn handle_query_autonomy_status(
        graph: &GraphDomain,
        lane: Option<&str>,
        now: u64,
    ) -> IpcResponse {
        use ansible_mesh_core::autonomy::lane_status_report;

        let key = match lane {
            Some(lane) => format!("__autonomy_status__:{lane}"),
            None => "__autonomy_status__".to_string(),
        };
        let value_json = match lane {
            Some(lane) => {
                let report = graph
                    .get_autonomy_grant(lane)
                    .unwrap_or(None)
                    .map(|g| lane_status_report(&g, now));
                serde_json::to_string(&report).ok()
            }
            None => {
                let reports: Vec<_> = graph
                    .list_autonomy_grants()
                    .unwrap_or_default()
                    .iter()
                    .map(|g| lane_status_report(g, now))
                    .collect();
                serde_json::to_string(&reports).ok()
            }
        };
        IpcResponse::ConfigData { key, value_json }
    }

    /// Handle `GetConfig("__autonomy_pending__")` — the A9 outcome-stamping
    /// follow-up slice's `phil autonomy pending` surface: every
    /// `autonomy_audit` record across all lanes still `Pending` an operator
    /// outcome, oldest first. Read-only, computed straight from
    /// [`ansible_mesh_core::domain::GraphDomain::list_all_autonomy_audits`] —
    /// no new state, and no autonomy grant is consulted (a read, not an
    /// action). Each entry carries `audit_id`, `lane`, `action_summary`,
    /// `created_at`, and `age_secs` (as of `now`) so an operator can eyeball
    /// how stale the backlog is before the timeout-to-Neutral sweep
    /// (`crate::autonomy_sweep`) catches up to it.
    pub(crate) fn handle_query_autonomy_pending(graph: &GraphDomain, now: u64) -> IpcResponse {
        use ansible_mesh_core::autonomy::AuditOutcome;

        const KEY: &str = "__autonomy_pending__";
        let records = graph.list_all_autonomy_audits().unwrap_or_default();
        let pending: Vec<_> = records
            .into_iter()
            .filter(|r| r.outcome == AuditOutcome::Pending)
            .map(|r| {
                serde_json::json!({
                    "audit_id": r.audit_id,
                    "lane": r.lane.as_str(),
                    "action_summary": r.action_summary,
                    "created_at": r.created_at,
                    "age_secs": now.saturating_sub(r.created_at),
                })
            })
            .collect();
        let value_json = serde_json::to_string(&pending).ok();
        IpcResponse::ConfigData {
            key: KEY.to_string(),
            value_json,
        }
    }
}

impl IpcServer {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_push_heal_entry(
        guest_id: String,
        raw_text: String,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
    ) -> IpcResponse {
        match heal_queue.as_deref() {
            Some(hq) => match hq.push_error(&guest_id, &raw_text) {
                Ok(id) => IpcResponse::HealEntryPushed { id },
                Err(e) => IpcResponse::error("push_heal_entry", "STORAGE_ERROR", format!("{e}")),
            },
            None => IpcResponse::error(
                "push_heal_entry",
                "UNAVAILABLE",
                "heal_queue not configured".to_string(),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_heal_queue_pending(
        limit: usize,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
    ) -> IpcResponse {
        match heal_queue.as_deref() {
            Some(hq) => match hq.pending_errors(limit) {
                Ok(rows) => IpcResponse::HealQueuePending { rows },
                Err(e) => {
                    IpcResponse::error("get_heal_queue_pending", "STORAGE_ERROR", format!("{e}"))
                }
            },
            None => IpcResponse::HealQueuePending { rows: vec![] },
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_triage_heal_entry(
        id: String,
        severity: String,
        pattern_tag: String,
        heal_action: String,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
    ) -> IpcResponse {
        match heal_queue.as_deref() {
            Some(hq) => match hq.update_triage(&id, &severity, &pattern_tag, &heal_action) {
                Ok(()) => IpcResponse::success("triage_heal_entry", None),
                Err(e) => IpcResponse::error("triage_heal_entry", "STORAGE_ERROR", format!("{e}")),
            },
            None => IpcResponse::error(
                "triage_heal_entry",
                "UNAVAILABLE",
                "heal_queue not configured".to_string(),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_resolve_heal_entry(
        id: String,
        outcome: String,
        graph: &GraphDomain,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        // Agent-originated calls (heal.resolve steward tool) require
        // operational admin authority; the heal-dispatcher and CLI
        // paths pass through unchanged.
        if let Err(refusal) =
            steward_agent_admin_gate(graph, current_identity.as_ref(), "resolve_heal_entry")
        {
            return refusal;
        }
        match heal_queue.as_deref() {
            Some(hq) => match hq.resolve(&id, &outcome) {
                Ok(()) => IpcResponse::success("resolve_heal_entry", None),
                Err(e) => IpcResponse::error("resolve_heal_entry", "STORAGE_ERROR", format!("{e}")),
            },
            None => IpcResponse::error(
                "resolve_heal_entry",
                "UNAVAILABLE",
                "heal_queue not configured".to_string(),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_close_heal_work_item(
        work_item_id: String,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        // Agent-originated calls (heal.close_work_item steward tool)
        // require operational admin authority; the autonomy-lane loop
        // and `phil heal close` CLI paths pass through unchanged.
        if let Err(refusal) =
            steward_agent_admin_gate(graph, current_identity.as_ref(), "close_heal_work_item")
        {
            return refusal;
        }
        // Closure path for a filed heal work item (finding F8). Wired
        // straight to the unit-tested domain method; closing a missing
        // id returns closed=false, and closing an already-closed item
        // returns closed=true (idempotent) so the autonomy-lane loop can
        // retry safely.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        match graph.close_heal_work_item(&work_item_id, now) {
            Ok(closed) => IpcResponse::success(
                "close_heal_work_item",
                Some(serde_json::json!({
                    "closed": closed,
                    "work_item_id": work_item_id,
                })),
            ),
            Err(e) => IpcResponse::error("close_heal_work_item", "STORAGE_ERROR", format!("{e:#}")),
        }
    }
}
