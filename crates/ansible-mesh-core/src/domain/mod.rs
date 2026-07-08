//! GraphDomain — reusable middle layer between callers and the graph adapter backend.
//!
//! `GraphDomain` expresses domain operations purely in terms of [`GraphAdapter`]
//! node/edge primitives. No SQL, no direct backend access, no schema duplication.
//!
//! All graph stores in the system (hotel CG, agent graph, trace store) hold a
//! `GraphDomain` over their own `GraphAdapter` instance. Adding a new entity type
//! means adding kind constants and domain methods here — the backend never changes.
//!
//! # Node key convention
//!
//! Node keys follow `"{kind}:{id}"` — e.g. `"hotel:default"`,
//! `"abstract_tool:bash.exec"`, `"rule:rule-001"`. This keeps keys globally
//! unique within a store and makes kind-scoped lookups fast.

use crate::autonomy::{
    record_outcome, AuditOutcome, AutonomyAuditRecord, AutonomyGrant, AutonomyLane, Outcome,
    Transition,
};
use crate::cron::CronJob;
use crate::graph::{
    AbstractModelRecord, AbstractRightRecord, AbstractSkillRecord, AbstractToolRecord, GraphNode,
    MembraneTransportHomeRecord, ModelProfileRecord, RoleIncarnationRecord, RoleReadinessState,
    RoutingPolicyEvaluationRecord, RoutingPolicyRecord, RuleRecord, SkillRegistrationAuditRecord,
    ToolsetProfileRecord, WorkflowSkillRecord,
};
use crate::heal_queue::{
    HealWorkItemRecord, HEAL_WORK_ITEM_STATUS_CLOSED, HEAL_WORK_ITEM_STATUS_OPEN,
};
use crate::storage::{
    AgentIdentityRecord, GraphAdapter, GraphRunnerInstanceRecord, GuestRecord, HotelRecord,
    ProjectedUserIdentityRecord, SecretRecord, SessionEventRecord, SessionParticipantRecord,
    SessionRecord, SessionTurnRecord, UserProfile, VaultRegistryEntry,
    CONFIG_GRAPH_RUNNER_REGISTRY, CONFIG_MUNINN_ENDPOINT, CONFIG_VAULT_REGISTRY,
};
use crate::NodeCapabilities;
use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::warn;

mod kinds;
pub use kinds::*;
// ── GraphDomain ───────────────────────────────────────────────────────────────

/// Domain-operation layer over a generic [`GraphAdapter`].
///
/// All persistence is expressed via `GraphNode` upserts and queries on the
/// adapter. Callers hold `Arc<GraphDomain>` and never interact with the adapter
/// directly.
pub struct GraphDomain {
    adapter: Arc<dyn GraphAdapter>,
}

impl GraphDomain {
    /// Construct a domain layer backed by `adapter`.
    pub fn new(adapter: Arc<dyn GraphAdapter>) -> Self {
        Self { adapter }
    }

    // ── Node key helpers ──────────────────────────────────────────────────────

    fn hotel_key(hotel_name: &str) -> String {
        format!("{}:{}", NODE_KIND_HOTEL, hotel_name)
    }

    fn abstract_tool_key(tool_name: &str) -> String {
        format!("{}:{}", NODE_KIND_ABSTRACT_TOOL, tool_name)
    }

    fn abstract_model_key(model_ref: &str) -> String {
        format!("{}:{}", NODE_KIND_ABSTRACT_MODEL, model_ref)
    }

    fn abstract_right_key(right_name: &str) -> String {
        format!("{}:{}", NODE_KIND_ABSTRACT_RIGHT, right_name)
    }

    fn rule_key(rule_id: &str) -> String {
        format!("{}:{}", NODE_KIND_RULE, rule_id)
    }

    fn routing_policy_key(proposal_id: &str) -> String {
        format!("{}:{}", NODE_KIND_ROUTING_POLICY, proposal_id)
    }

    // ── Hotel methods ─────────────────────────────────────────────────────────

    /// Upsert a hotel record as a graph node.
    pub fn upsert_hotel(&self, hotel: &HotelRecord) -> Result<()> {
        let data = serde_json::to_value(hotel)
            .context("GraphDomain::upsert_hotel: serialize HotelRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::hotel_key(&hotel.hotel_name),
            kind: NODE_KIND_HOTEL.to_string(),
            label: Some(hotel.hotel_name.clone()),
            data,
        })
    }

    /// Load a hotel record by name.
    pub fn get_hotel(&self, hotel_name: &str) -> Result<Option<HotelRecord>> {
        match self.adapter.get_node(&Self::hotel_key(hotel_name))? {
            None => Ok(None),
            Some(node) => {
                let record = serde_json::from_value(node.data)
                    .context("GraphDomain::get_hotel: deserialize HotelRecord")?;
                Ok(Some(record))
            }
        }
    }

    /// List all hotel records.
    pub fn list_hotels(&self) -> Result<Vec<HotelRecord>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_HOTEL)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_hotels: deserialize HotelRecord")
            })
            .collect()
    }

    // ── Abstract tool methods ─────────────────────────────────────────────────

    /// Upsert an abstract tool record as a graph node.
    pub fn upsert_abstract_tool(&self, tool: &AbstractToolRecord) -> Result<()> {
        let data = serde_json::to_value(tool)
            .context("GraphDomain::upsert_abstract_tool: serialize AbstractToolRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::abstract_tool_key(&tool.tool_name),
            kind: NODE_KIND_ABSTRACT_TOOL.to_string(),
            label: Some(tool.tool_name.clone()),
            data,
        })
    }

    /// Load an abstract tool record by tool name.
    pub fn get_abstract_tool(&self, tool_name: &str) -> Result<Option<AbstractToolRecord>> {
        match self.adapter.get_node(&Self::abstract_tool_key(tool_name))? {
            None => Ok(None),
            Some(node) => {
                let record = serde_json::from_value(node.data)
                    .context("GraphDomain::get_abstract_tool: deserialize AbstractToolRecord")?;
                Ok(Some(record))
            }
        }
    }

    /// List all abstract tool records.
    pub fn list_abstract_tools(&self) -> Result<Vec<AbstractToolRecord>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_ABSTRACT_TOOL)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_abstract_tools: deserialize AbstractToolRecord")
            })
            .collect()
    }

    /// Upsert an abstract model record as a graph node.
    pub fn upsert_abstract_model(&self, model: &AbstractModelRecord) -> Result<()> {
        let data = serde_json::to_value(model)
            .context("GraphDomain::upsert_abstract_model: serialize AbstractModelRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::abstract_model_key(&model.model_ref),
            kind: NODE_KIND_ABSTRACT_MODEL.to_string(),
            label: Some(model.model_ref.clone()),
            data,
        })
    }

    /// Load an abstract model record by model_ref.
    pub fn get_abstract_model(&self, model_ref: &str) -> Result<Option<AbstractModelRecord>> {
        match self
            .adapter
            .get_node(&Self::abstract_model_key(model_ref))?
        {
            None => Ok(None),
            Some(node) => {
                let record = serde_json::from_value(node.data)
                    .context("GraphDomain::get_abstract_model: deserialize AbstractModelRecord")?;
                Ok(Some(record))
            }
        }
    }

    /// List all abstract model records.
    pub fn list_abstract_models(&self) -> Result<Vec<AbstractModelRecord>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_ABSTRACT_MODEL)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_abstract_models: deserialize AbstractModelRecord")
            })
            .collect()
    }

    /// Upsert an abstract right record as a graph node.
    pub fn upsert_abstract_right(&self, right: &AbstractRightRecord) -> Result<()> {
        let data = serde_json::to_value(right)
            .context("GraphDomain::upsert_abstract_right: serialize AbstractRightRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::abstract_right_key(&right.right_name),
            kind: NODE_KIND_ABSTRACT_RIGHT.to_string(),
            label: Some(right.right_name.clone()),
            data,
        })
    }

    /// Load an abstract right record by right name.
    pub fn get_abstract_right(&self, right_name: &str) -> Result<Option<AbstractRightRecord>> {
        match self
            .adapter
            .get_node(&Self::abstract_right_key(right_name))?
        {
            None => Ok(None),
            Some(node) => {
                let record = serde_json::from_value(node.data)
                    .context("GraphDomain::get_abstract_right: deserialize AbstractRightRecord")?;
                Ok(Some(record))
            }
        }
    }

    /// List all abstract right records.
    pub fn list_abstract_rights(&self) -> Result<Vec<AbstractRightRecord>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_ABSTRACT_RIGHT)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_abstract_rights: deserialize AbstractRightRecord")
            })
            .collect()
    }

    // ── Rule methods ──────────────────────────────────────────────────────────

    /// Upsert a rule record as a graph node.
    pub fn upsert_rule(&self, rule: &RuleRecord) -> Result<()> {
        let data =
            serde_json::to_value(rule).context("GraphDomain::upsert_rule: serialize RuleRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::rule_key(&rule.rule_id),
            kind: NODE_KIND_RULE.to_string(),
            label: Some(rule.rule_id.clone()),
            data,
        })
    }

    /// Load a rule record by rule_id.
    pub fn get_rule(&self, rule_id: &str) -> Result<Option<RuleRecord>> {
        match self.adapter.get_node(&Self::rule_key(rule_id))? {
            None => Ok(None),
            Some(node) => {
                let record = serde_json::from_value(node.data)
                    .context("GraphDomain::get_rule: deserialize RuleRecord")?;
                Ok(Some(record))
            }
        }
    }

    /// List all rules owned by `agent_id`.
    ///
    /// Loads all rule nodes and filters in Rust — no SQL predicate required.
    pub fn list_rules(&self, agent_id: &str) -> Result<Vec<RuleRecord>> {
        let mut rules = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_RULE)? {
            let record: RuleRecord = serde_json::from_value(node.data)
                .context("GraphDomain::list_rules: deserialize RuleRecord")?;
            if record.agent_id == agent_id {
                rules.push(record);
            }
        }
        Ok(rules)
    }

    pub fn upsert_routing_policy(&self, policy: &RoutingPolicyRecord) -> Result<()> {
        let data = serde_json::to_value(policy)
            .context("GraphDomain::upsert_routing_policy: serialize RoutingPolicyRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::routing_policy_key(&policy.proposal_id),
            kind: NODE_KIND_ROUTING_POLICY.to_string(),
            label: Some(policy.proposal_id.clone()),
            data,
        })
    }

    pub fn get_routing_policy(&self, proposal_id: &str) -> Result<Option<RoutingPolicyRecord>> {
        match self
            .adapter
            .get_node(&Self::routing_policy_key(proposal_id))?
        {
            None => Ok(None),
            Some(node) => {
                let record = serde_json::from_value(node.data)
                    .context("GraphDomain::get_routing_policy: deserialize RoutingPolicyRecord")?;
                Ok(Some(record))
            }
        }
    }

    pub fn list_routing_policies(&self, agent_id: &str) -> Result<Vec<RoutingPolicyRecord>> {
        let mut policies = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_ROUTING_POLICY)? {
            let record: RoutingPolicyRecord = serde_json::from_value(node.data)
                .context("GraphDomain::list_routing_policies: deserialize RoutingPolicyRecord")?;
            if record.agent_id == agent_id {
                policies.push(record);
            }
        }
        Ok(policies)
    }

    pub fn append_routing_policy_evaluation(
        &self,
        proposal_id: &str,
        evaluation: RoutingPolicyEvaluationRecord,
    ) -> Result<bool> {
        let Some(mut record) = self.get_routing_policy(proposal_id)? else {
            return Ok(false);
        };
        record.evaluations.push(evaluation);
        self.upsert_routing_policy(&record)?;
        Ok(true)
    }

    pub fn set_routing_policy_disposition(
        &self,
        proposal_id: &str,
        state: String,
        reason: String,
        decided_at: u64,
        source_tool: Option<String>,
    ) -> Result<bool> {
        let Some(mut record) = self.get_routing_policy(proposal_id)? else {
            return Ok(false);
        };
        record.operator_disposition = crate::graph::RoutingPolicyDispositionRecord {
            state: state.clone(),
            reason: reason.clone(),
            decided_at,
        };
        record.evaluations.push(RoutingPolicyEvaluationRecord {
            evaluation_kind: "operator_disposition".to_string(),
            decision: state,
            reason,
            created_at: decided_at,
            source_tool,
        });
        self.upsert_routing_policy(&record)?;
        Ok(true)
    }

    // ── Guest methods ─────────────────────────────────────────────────────────

    fn guest_key(hotel_name: &str, guest_id: &str) -> String {
        format!("{}:{}:{}", NODE_KIND_GUEST, hotel_name, guest_id)
    }

    pub fn upsert_guest(&self, guest: &GuestRecord) -> Result<()> {
        let data = serde_json::to_value(guest)
            .context("GraphDomain::upsert_guest: serialize GuestRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::guest_key(&guest.hotel_name, &guest.guest_id),
            kind: NODE_KIND_GUEST.to_string(),
            label: Some(guest.guest_id.clone()),
            data,
        })
    }

    pub fn get_guest(&self, hotel_name: &str, guest_id: &str) -> Result<Option<GuestRecord>> {
        match self
            .adapter
            .get_node(&Self::guest_key(hotel_name, guest_id))?
        {
            None => Ok(None),
            Some(node) => Ok(Some(
                serde_json::from_value(node.data)
                    .context("GraphDomain::get_guest: deserialize GuestRecord")?,
            )),
        }
    }

    /// List all guests for `hotel_name`. If `active_only`, filter to `is_active == true`.
    pub fn list_guests(&self, hotel_name: &str, active_only: bool) -> Result<Vec<GuestRecord>> {
        let prefix = format!("{}:{}:", NODE_KIND_GUEST, hotel_name);
        let mut out = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_GUEST)? {
            if !node.node_key.starts_with(&prefix) {
                continue;
            }
            let record: GuestRecord = serde_json::from_value(node.data)
                .context("GraphDomain::list_guests: deserialize GuestRecord")?;
            if !active_only || record.is_active {
                out.push(record);
            }
        }
        Ok(out)
    }

    /// Delete a guest record entirely.
    pub fn remove_guest(&self, hotel_name: &str, guest_id: &str) -> Result<()> {
        self.adapter
            .delete_node(&Self::guest_key(hotel_name, guest_id))
    }

    /// Bulk upsert guest rows (used during initial seeding).
    pub fn seed_guests(&self, hotel_name: &str, guests: &[GuestRecord]) -> Result<()> {
        for g in guests {
            debug_assert_eq!(g.hotel_name, hotel_name);
            self.upsert_guest(g)?;
        }
        Ok(())
    }

    /// Update the `active_pid` on a guest record (get → mutate → upsert).
    pub fn set_guest_pid(&self, hotel_name: &str, guest_id: &str, pid: Option<&str>) -> Result<()> {
        if let Some(mut rec) = self.get_guest(hotel_name, guest_id)? {
            rec.active_pid = pid.map(str::to_string);
            self.upsert_guest(&rec)?;
        }
        Ok(())
    }

    /// Update the `is_active` flag on a guest record.
    pub fn set_guest_active(&self, hotel_name: &str, guest_id: &str, active: bool) -> Result<()> {
        if let Some(mut rec) = self.get_guest(hotel_name, guest_id)? {
            rec.is_active = active;
            self.upsert_guest(&rec)?;
        }
        Ok(())
    }

    /// Stamp the `last_active_at` time on a guest record.
    pub fn set_guest_last_active(
        &self,
        hotel_name: &str,
        guest_id: &str,
        epoch: u64,
    ) -> Result<()> {
        if let Some(mut rec) = self.get_guest(hotel_name, guest_id)? {
            rec.last_active_at = Some(epoch);
            self.upsert_guest(&rec)?;
        }
        Ok(())
    }

    // ── Hotel PID update ──────────────────────────────────────────────────────

    /// Update the `active_pid` on a hotel record.
    pub fn set_hotel_pid(&self, hotel_name: &str, pid: Option<&str>) -> Result<()> {
        if let Some(mut rec) = self.get_hotel(hotel_name)? {
            rec.active_pid = pid.map(str::to_string);
            self.upsert_hotel(&rec)?;
        }
        Ok(())
    }

    // ── Agent identity methods ────────────────────────────────────────────────

    fn agent_identity_key(agent_id: &str) -> String {
        format!("{}:{}", NODE_KIND_AGENT_IDENTITY, agent_id)
    }

    pub fn upsert_agent_identity(&self, identity: &AgentIdentityRecord) -> Result<()> {
        let data = serde_json::to_value(identity)
            .context("GraphDomain::upsert_agent_identity: serialize AgentIdentityRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::agent_identity_key(&identity.agent_id),
            kind: NODE_KIND_AGENT_IDENTITY.to_string(),
            label: Some(identity.persona_name.clone()),
            data,
        })
    }

    pub fn get_agent_identity(&self, agent_id: &str) -> Result<Option<AgentIdentityRecord>> {
        match self.adapter.get_node(&Self::agent_identity_key(agent_id))? {
            None => Ok(None),
            Some(node) => Ok(Some(serde_json::from_value(node.data).context(
                "GraphDomain::get_agent_identity: deserialize AgentIdentityRecord",
            )?)),
        }
    }

    pub fn list_agent_identities(&self) -> Result<Vec<AgentIdentityRecord>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_AGENT_IDENTITY)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_agent_identities: deserialize AgentIdentityRecord")
            })
            .collect()
    }

    // ── Session methods ───────────────────────────────────────────────────────

    fn session_key(session_id: &str) -> String {
        format!("{}:{}", NODE_KIND_SESSION, session_id)
    }

    pub fn upsert_session(&self, session: &SessionRecord) -> Result<()> {
        let data = serde_json::to_value(session)
            .context("GraphDomain::upsert_session: serialize SessionRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::session_key(&session.session_id),
            kind: NODE_KIND_SESSION.to_string(),
            label: Some(session.session_id.clone()),
            data,
        })
    }

    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        match self.adapter.get_node(&Self::session_key(session_id))? {
            None => Ok(None),
            Some(node) => Ok(Some(
                serde_json::from_value(node.data)
                    .context("GraphDomain::get_session: deserialize SessionRecord")?,
            )),
        }
    }

    /// List all session records. Malformed rows are skipped with a warning
    /// instead of failing the whole listing (same hardening as
    /// [`Self::list_session_turns`]).
    pub fn list_sessions(&self) -> Result<Vec<SessionRecord>> {
        let mut out: Vec<SessionRecord> = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_SESSION)? {
            match serde_json::from_value::<SessionRecord>(node.data) {
                Ok(record) => out.push(record),
                Err(e) => warn!(
                    node_key = %node.node_key,
                    error = %e,
                    "list_sessions: skipping malformed record"
                ),
            }
        }
        Ok(out)
    }

    // ── Session participant methods ────────────────────────────────────────────

    fn session_participant_key(session_id: &str, component_id: &str) -> String {
        format!(
            "{}:{}:{}",
            NODE_KIND_SESSION_PARTICIPANT, session_id, component_id
        )
    }

    pub fn upsert_session_participant(&self, p: &SessionParticipantRecord) -> Result<()> {
        let data = serde_json::to_value(p).context(
            "GraphDomain::upsert_session_participant: serialize SessionParticipantRecord",
        )?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::session_participant_key(&p.session_id, &p.component_id),
            kind: NODE_KIND_SESSION_PARTICIPANT.to_string(),
            label: Some(p.component_id.clone()),
            data,
        })
    }

    pub fn list_session_participants(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionParticipantRecord>> {
        let prefix = format!("{}:{}:", NODE_KIND_SESSION_PARTICIPANT, session_id);
        let mut out = Vec::new();
        for node in self
            .adapter
            .list_nodes_by_kind(NODE_KIND_SESSION_PARTICIPANT)?
        {
            if node.node_key.starts_with(&prefix) {
                out.push(serde_json::from_value(node.data).context(
                    "GraphDomain::list_session_participants: deserialize SessionParticipantRecord",
                )?);
            }
        }
        Ok(out)
    }

    // ── Session turn methods ──────────────────────────────────────────────────

    fn session_turn_key(session_id: &str, turn_id: &str) -> String {
        format!("{}:{}:{}", NODE_KIND_SESSION_TURN, session_id, turn_id)
    }

    pub fn upsert_session_turn(&self, turn: &SessionTurnRecord) -> Result<()> {
        let data = serde_json::to_value(turn)
            .context("GraphDomain::upsert_session_turn: serialize SessionTurnRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::session_turn_key(&turn.session_id, &turn.turn_id),
            kind: NODE_KIND_SESSION_TURN.to_string(),
            label: Some(turn.turn_id.clone()),
            data,
        })
    }

    pub fn get_session_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Option<SessionTurnRecord>> {
        match self
            .adapter
            .get_node(&Self::session_turn_key(session_id, turn_id))?
        {
            None => Ok(None),
            Some(node) => Ok(Some(serde_json::from_value(node.data).context(
                "GraphDomain::get_session_turn: deserialize SessionTurnRecord",
            )?)),
        }
    }

    /// List turns for `session_id`, most recent `limit` entries.
    pub fn list_session_turns(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionTurnRecord>> {
        let prefix = format!("{}:{}:", NODE_KIND_SESSION_TURN, session_id);
        let mut out: Vec<SessionTurnRecord> = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_SESSION_TURN)? {
            if node.node_key.starts_with(&prefix) {
                match serde_json::from_value::<SessionTurnRecord>(node.data) {
                    Ok(record) => out.push(record),
                    Err(e) => warn!(
                        node_key = %node.node_key,
                        error = %e,
                        "list_session_turns: skipping malformed record"
                    ),
                }
            }
        }
        if limit > 0 && out.len() > limit {
            out.drain(..out.len() - limit);
        }
        Ok(out)
    }

    /// Returns all session_turn records with status="running" and started_at <= max_started_at.
    /// Used by the hotel's zombie-turn repair sweep.
    pub fn list_zombie_session_turns(&self, max_started_at: u64) -> Result<Vec<SessionTurnRecord>> {
        let mut out = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_SESSION_TURN)? {
            let record = match serde_json::from_value::<SessionTurnRecord>(node.data) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if record.status == "running" {
                if let Some(started_at) = record.started_at {
                    if started_at <= max_started_at {
                        out.push(record);
                    }
                }
            }
        }
        Ok(out)
    }

    // ── Session event methods ─────────────────────────────────────────────────

    fn session_event_key(event_id: &str) -> String {
        format!("{}:{}", NODE_KIND_SESSION_EVENT, event_id)
    }

    pub fn append_session_event(&self, event: &SessionEventRecord) -> Result<()> {
        let data = serde_json::to_value(event)
            .context("GraphDomain::append_session_event: serialize SessionEventRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::session_event_key(&event.event_id),
            kind: NODE_KIND_SESSION_EVENT.to_string(),
            label: None,
            data,
        })
    }

    /// List events for `session_id`, most recent `limit` entries.
    pub fn list_session_events(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionEventRecord>> {
        let mut out: Vec<SessionEventRecord> = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_SESSION_EVENT)? {
            let record: SessionEventRecord = serde_json::from_value(node.data)
                .context("GraphDomain::list_session_events: deserialize SessionEventRecord")?;
            if record.session_id == session_id {
                out.push(record);
            }
        }
        if limit > 0 && out.len() > limit {
            out.drain(..out.len() - limit);
        }
        Ok(out)
    }

    // ── Role incarnation methods ──────────────────────────────────────────────

    fn role_incarnation_key(agent_id: &str, role_name: &str) -> String {
        format!("{}:{}:{}", NODE_KIND_ROLE_INCARNATION, agent_id, role_name)
    }

    pub fn upsert_role_incarnation(&self, role: &RoleIncarnationRecord) -> Result<()> {
        let data = serde_json::to_value(role)
            .context("GraphDomain::upsert_role_incarnation: serialize RoleIncarnationRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::role_incarnation_key(&role.agent_id, &role.role_name),
            kind: NODE_KIND_ROLE_INCARNATION.to_string(),
            label: Some(role.role_name.clone()),
            data,
        })
    }

    pub fn get_role_incarnation(
        &self,
        agent_id: &str,
        role_name: &str,
    ) -> Result<Option<RoleIncarnationRecord>> {
        match self
            .adapter
            .get_node(&Self::role_incarnation_key(agent_id, role_name))?
        {
            None => Ok(None),
            Some(node) => Ok(Some(serde_json::from_value(node.data).context(
                "GraphDomain::get_role_incarnation: deserialize RoleIncarnationRecord",
            )?)),
        }
    }

    pub fn list_role_incarnations(&self, agent_id: &str) -> Result<Vec<RoleIncarnationRecord>> {
        let prefix = format!("{}:{}:", NODE_KIND_ROLE_INCARNATION, agent_id);
        let mut out = Vec::new();
        for node in self
            .adapter
            .list_nodes_by_kind(NODE_KIND_ROLE_INCARNATION)?
        {
            if node.node_key.starts_with(&prefix) {
                out.push(serde_json::from_value(node.data).context(
                    "GraphDomain::list_role_incarnations: deserialize RoleIncarnationRecord",
                )?);
            }
        }
        Ok(out)
    }

    /// Find all role incarnation records whose `guest_id` matches.
    pub fn list_role_incarnations_by_guest_id(
        &self,
        guest_id: &str,
    ) -> Result<Vec<RoleIncarnationRecord>> {
        let mut out = Vec::new();
        for node in self
            .adapter
            .list_nodes_by_kind(NODE_KIND_ROLE_INCARNATION)?
        {
            let record: RoleIncarnationRecord = serde_json::from_value(node.data).context(
                "GraphDomain::list_role_incarnations_by_guest_id: deserialize RoleIncarnationRecord",
            )?;
            if record.guest_id == guest_id {
                out.push(record);
            }
        }
        Ok(out)
    }

    pub fn list_role_incarnations_by_routing_role(
        &self,
        routing_role: &str,
    ) -> Result<Vec<RoleIncarnationRecord>> {
        let mut out = Vec::new();
        for node in self
            .adapter
            .list_nodes_by_kind(NODE_KIND_ROLE_INCARNATION)?
        {
            let record: RoleIncarnationRecord = serde_json::from_value(node.data).context(
                "GraphDomain::list_role_incarnations_by_routing_role: deserialize RoleIncarnationRecord",
            )?;
            if record.routing_role() == routing_role {
                out.push(record);
            }
        }
        Ok(out)
    }

    /// List every role incarnation record across all agents. Used by the
    /// role-handoff-loop self-heal detector to find agents with more than one
    /// `ActiveInSession` incarnation.
    pub fn list_all_role_incarnations(&self) -> Result<Vec<RoleIncarnationRecord>> {
        let mut out = Vec::new();
        for node in self
            .adapter
            .list_nodes_by_kind(NODE_KIND_ROLE_INCARNATION)?
        {
            out.push(serde_json::from_value(node.data).context(
                "GraphDomain::list_all_role_incarnations: deserialize RoleIncarnationRecord",
            )?);
        }
        Ok(out)
    }

    /// List every session record. Used by the role-handoff-loop self-heal
    /// detector to clear pins that point at a demoted incarnation.
    pub fn list_all_sessions(&self) -> Result<Vec<SessionRecord>> {
        let mut out = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_SESSION)? {
            out.push(
                serde_json::from_value(node.data)
                    .context("GraphDomain::list_all_sessions: deserialize SessionRecord")?,
            );
        }
        Ok(out)
    }

    pub fn set_role_incarnation_readiness(
        &self,
        agent_id: &str,
        role_name: &str,
        readiness_state: RoleReadinessState,
    ) -> Result<()> {
        if let Some(mut rec) = self.get_role_incarnation(agent_id, role_name)? {
            rec.readiness_state = readiness_state;
            self.upsert_role_incarnation(&rec)?;
        }
        Ok(())
    }

    /// Promote a single role incarnation to `ActiveInSession`, enforcing the
    /// single-active invariant: at most ONE incarnation per agent may be
    /// `ActiveInSession` at a time. Any sibling incarnation of the same agent
    /// currently `ActiveInSession` is demoted to `Routable`.
    ///
    /// Without this invariant, distinct role handoffs (e.g. orchestrator then
    /// Chronos) each set their own target active without clearing the previous
    /// one, leaving two incarnations active simultaneously — the corrupt state
    /// that seeds the role-handoff ping-pong loop.
    pub fn promote_role_incarnation_active(&self, agent_id: &str, role_name: &str) -> Result<()> {
        // Demote any OTHER incarnation of this agent that is currently active.
        for sibling in self.list_role_incarnations(agent_id)? {
            if sibling.role_name != role_name
                && matches!(sibling.readiness_state, RoleReadinessState::ActiveInSession)
            {
                self.set_role_incarnation_readiness(
                    agent_id,
                    &sibling.role_name,
                    RoleReadinessState::Routable,
                )?;
            }
        }
        // Promote the target.
        self.set_role_incarnation_readiness(
            agent_id,
            role_name,
            RoleReadinessState::ActiveInSession,
        )
    }

    /// Find the first role incarnation record with the given role_name, across all agents.
    pub fn find_role_incarnation_by_name(
        &self,
        role_name: &str,
    ) -> Result<Option<RoleIncarnationRecord>> {
        for node in self
            .adapter
            .list_nodes_by_kind(NODE_KIND_ROLE_INCARNATION)?
        {
            let record: RoleIncarnationRecord = serde_json::from_value(node.data).context(
                "GraphDomain::find_role_incarnation_by_name: deserialize RoleIncarnationRecord",
            )?;
            if record.role_name == role_name {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    // ── Membrane transport home methods ──────────────────────────────────────

    fn membrane_transport_home_key(agent_id: &str, transport: &str, resource_ref: &str) -> String {
        format!(
            "{}:{}:{}:{}",
            NODE_KIND_MEMBRANE_TRANSPORT_HOME, agent_id, transport, resource_ref
        )
    }

    pub fn upsert_membrane_transport_home(&self, home: &MembraneTransportHomeRecord) -> Result<()> {
        let data = serde_json::to_value(home).context(
            "GraphDomain::upsert_membrane_transport_home: serialize MembraneTransportHomeRecord",
        )?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::membrane_transport_home_key(
                &home.agent_id,
                &home.transport,
                &home.resource_ref,
            ),
            kind: NODE_KIND_MEMBRANE_TRANSPORT_HOME.to_string(),
            label: Some(format!(
                "{}:{}:{}",
                home.agent_id, home.transport, home.resource_ref
            )),
            data,
        })
    }

    pub fn get_membrane_transport_home(
        &self,
        agent_id: &str,
        transport: &str,
        resource_ref: &str,
    ) -> Result<Option<MembraneTransportHomeRecord>> {
        match self.adapter.get_node(&Self::membrane_transport_home_key(
            agent_id,
            transport,
            resource_ref,
        ))? {
            None => Ok(None),
            Some(node) => Ok(Some(serde_json::from_value(node.data).context(
                "GraphDomain::get_membrane_transport_home: deserialize MembraneTransportHomeRecord",
            )?)),
        }
    }

    pub fn list_membrane_transport_homes(
        &self,
        agent_id: Option<&str>,
    ) -> Result<Vec<MembraneTransportHomeRecord>> {
        let prefix =
            agent_id.map(|agent_id| format!("{}:{}:", NODE_KIND_MEMBRANE_TRANSPORT_HOME, agent_id));
        let mut out = Vec::new();
        for node in self
            .adapter
            .list_nodes_by_kind(NODE_KIND_MEMBRANE_TRANSPORT_HOME)?
        {
            if prefix
                .as_ref()
                .is_none_or(|prefix| node.node_key.starts_with(prefix))
            {
                out.push(serde_json::from_value(node.data).context(
                    "GraphDomain::list_membrane_transport_homes: deserialize MembraneTransportHomeRecord",
                )?);
            }
        }
        Ok(out)
    }

    /// Resolve explicit graph-owned placement for a membrane transport.
    ///
    /// Missing records intentionally resolve to `None`; callers that still have
    /// transitional config fallbacks must name that fallback at their boundary.
    pub fn resolve_membrane_transport_home(
        &self,
        agent_id: &str,
        transport: &str,
        resource_ref: &str,
    ) -> Result<Option<MembraneTransportHomeRecord>> {
        self.get_membrane_transport_home(agent_id, transport, resource_ref)
    }

    // ── Secret methods ────────────────────────────────────────────────────────

    fn secret_key(secret_ref: &str) -> String {
        format!("{}:{}", NODE_KIND_SECRET, secret_ref)
    }

    pub fn upsert_secret(&self, secret: &SecretRecord) -> Result<()> {
        let data = serde_json::to_value(secret)
            .context("GraphDomain::upsert_secret: serialize SecretRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::secret_key(&secret.secret_ref),
            kind: NODE_KIND_SECRET.to_string(),
            label: Some(secret.secret_ref.clone()),
            data,
        })
    }

    pub fn get_secret(&self, secret_ref: &str) -> Result<Option<SecretRecord>> {
        match self.adapter.get_node(&Self::secret_key(secret_ref))? {
            None => Ok(None),
            Some(node) => Ok(Some(
                serde_json::from_value(node.data)
                    .context("GraphDomain::get_secret: deserialize SecretRecord")?,
            )),
        }
    }

    // ── Abstract skill methods ────────────────────────────────────────────────

    fn abstract_skill_key(skill_name: &str) -> String {
        format!("{}:{}", NODE_KIND_ABSTRACT_SKILL, skill_name)
    }

    pub fn upsert_abstract_skill(&self, skill: &AbstractSkillRecord) -> Result<()> {
        let data = serde_json::to_value(skill)
            .context("GraphDomain::upsert_abstract_skill: serialize AbstractSkillRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::abstract_skill_key(&skill.skill_name),
            kind: NODE_KIND_ABSTRACT_SKILL.to_string(),
            label: Some(skill.skill_name.clone()),
            data,
        })
    }

    pub fn get_abstract_skill(&self, skill_name: &str) -> Result<Option<AbstractSkillRecord>> {
        match self
            .adapter
            .get_node(&Self::abstract_skill_key(skill_name))?
        {
            None => Ok(None),
            Some(node) => Ok(Some(serde_json::from_value(node.data).context(
                "GraphDomain::get_abstract_skill: deserialize AbstractSkillRecord",
            )?)),
        }
    }

    pub fn list_abstract_skills(&self) -> Result<Vec<AbstractSkillRecord>> {
        let mut skills = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_ABSTRACT_SKILL)? {
            match serde_json::from_value::<AbstractSkillRecord>(node.data.clone()) {
                Ok(skill) => skills.push(skill),
                Err(err) => {
                    warn!(
                        node_key = %node.node_key,
                        "Skipping incompatible abstract_skill record during list_abstract_skills: {}",
                        err
                    );
                }
            }
        }
        Ok(skills)
    }

    // ── Skill registration audit methods ──────────────────────────────────────

    fn skill_registration_audit_key(audit_id: &str) -> String {
        format!("{}:{}", NODE_KIND_SKILL_REGISTRATION_AUDIT, audit_id)
    }

    /// Append an audit entry for an accepted skill registration. Entries are
    /// keyed by a unique `audit_id`, so this never overwrites prior entries —
    /// the collection behaves as an append-only trail.
    pub fn record_skill_registration_audit(
        &self,
        record: &SkillRegistrationAuditRecord,
    ) -> Result<()> {
        let data = serde_json::to_value(record).context(
            "GraphDomain::record_skill_registration_audit: serialize SkillRegistrationAuditRecord",
        )?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::skill_registration_audit_key(&record.audit_id),
            kind: NODE_KIND_SKILL_REGISTRATION_AUDIT.to_string(),
            label: Some(record.skill_name.clone()),
            data,
        })
    }

    /// List all recorded skill-registration audit entries. Malformed records are
    /// skipped with a warning rather than failing the whole listing.
    pub fn list_skill_registration_audits(&self) -> Result<Vec<SkillRegistrationAuditRecord>> {
        let mut audits = Vec::new();
        for node in self
            .adapter
            .list_nodes_by_kind(NODE_KIND_SKILL_REGISTRATION_AUDIT)?
        {
            match serde_json::from_value::<SkillRegistrationAuditRecord>(node.data.clone()) {
                Ok(rec) => audits.push(rec),
                Err(err) => {
                    warn!(
                        node_key = %node.node_key,
                        "Skipping incompatible skill_registration_audit record during list: {}",
                        err
                    );
                }
            }
        }
        Ok(audits)
    }

    // ── Workflow skill methods ────────────────────────────────────────────────

    fn workflow_skill_key(workflow_name: &str) -> String {
        format!("{}:{}", NODE_KIND_WORKFLOW_SKILL, workflow_name)
    }

    pub fn upsert_workflow_skill(&self, skill: &WorkflowSkillRecord) -> Result<()> {
        let data = serde_json::to_value(skill)
            .context("GraphDomain::upsert_workflow_skill: serialize WorkflowSkillRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::workflow_skill_key(&skill.workflow_name),
            kind: NODE_KIND_WORKFLOW_SKILL.to_string(),
            label: Some(skill.workflow_name.clone()),
            data,
        })
    }

    pub fn get_workflow_skill(&self, workflow_name: &str) -> Result<Option<WorkflowSkillRecord>> {
        match self
            .adapter
            .get_node(&Self::workflow_skill_key(workflow_name))?
        {
            None => Ok(None),
            Some(node) => Ok(Some(serde_json::from_value(node.data).context(
                "GraphDomain::get_workflow_skill: deserialize WorkflowSkillRecord",
            )?)),
        }
    }

    pub fn list_workflow_skills(&self) -> Result<Vec<WorkflowSkillRecord>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_WORKFLOW_SKILL)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_workflow_skills: deserialize WorkflowSkillRecord")
            })
            .collect()
    }

    // ── Toolset profile methods ───────────────────────────────────────────────

    fn toolset_profile_key(profile_name: &str) -> String {
        format!("{}:{}", NODE_KIND_TOOLSET_PROFILE, profile_name)
    }

    pub fn upsert_toolset_profile(&self, profile: &ToolsetProfileRecord) -> Result<()> {
        let data = serde_json::to_value(profile)
            .context("GraphDomain::upsert_toolset_profile: serialize ToolsetProfileRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::toolset_profile_key(&profile.profile_name),
            kind: NODE_KIND_TOOLSET_PROFILE.to_string(),
            label: Some(profile.profile_name.clone()),
            data,
        })
    }

    pub fn get_toolset_profile(&self, profile_name: &str) -> Result<Option<ToolsetProfileRecord>> {
        match self
            .adapter
            .get_node(&Self::toolset_profile_key(profile_name))?
        {
            None => Ok(None),
            Some(node) => Ok(Some(serde_json::from_value(node.data).context(
                "GraphDomain::get_toolset_profile: deserialize ToolsetProfileRecord",
            )?)),
        }
    }

    pub fn list_toolset_profiles(&self) -> Result<Vec<ToolsetProfileRecord>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_TOOLSET_PROFILE)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_toolset_profiles: deserialize ToolsetProfileRecord")
            })
            .collect()
    }

    // ── User profile ──────────────────────────────────────────────────────────

    fn user_profile_key(hotel_name: &str) -> String {
        format!("{}:{}", NODE_KIND_USER_PROFILE, hotel_name)
    }

    fn projected_user_identity_key(principal_id: &str) -> String {
        format!("{}:{}", NODE_KIND_PROJECTED_USER_IDENTITY, principal_id)
    }

    pub fn upsert_user_profile(&self, hotel_name: &str, profile: &UserProfile) -> Result<()> {
        let data = serde_json::to_value(profile)
            .context("GraphDomain::upsert_user_profile: serialize UserProfile")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::user_profile_key(hotel_name),
            kind: NODE_KIND_USER_PROFILE.to_string(),
            label: Some(hotel_name.to_string()),
            data,
        })
    }

    pub fn get_user_profile(&self, hotel_name: &str) -> Result<Option<UserProfile>> {
        self.adapter
            .get_node(&Self::user_profile_key(hotel_name))?
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::get_user_profile: deserialize UserProfile")
            })
            .transpose()
    }

    pub fn upsert_projected_user_identity(
        &self,
        identity: &ProjectedUserIdentityRecord,
    ) -> Result<()> {
        let data = serde_json::to_value(identity).context(
            "GraphDomain::upsert_projected_user_identity: serialize ProjectedUserIdentityRecord",
        )?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::projected_user_identity_key(&identity.principal_id),
            kind: NODE_KIND_PROJECTED_USER_IDENTITY.to_string(),
            label: Some(identity.display_name.clone()),
            data,
        })
    }

    pub fn get_projected_user_identity(
        &self,
        principal_id: &str,
    ) -> Result<Option<ProjectedUserIdentityRecord>> {
        self.adapter
            .get_node(&Self::projected_user_identity_key(principal_id))?
            .map(|n| {
                serde_json::from_value(n.data).context(
                    "GraphDomain::get_projected_user_identity: deserialize ProjectedUserIdentityRecord",
                )
            })
            .transpose()
    }

    pub fn list_projected_user_identities(&self) -> Result<Vec<ProjectedUserIdentityRecord>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_PROJECTED_USER_IDENTITY)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data).context(
                    "GraphDomain::list_projected_user_identities: deserialize ProjectedUserIdentityRecord",
                )
            })
            .collect()
    }

    pub fn find_projected_user_identity_for_local_user(
        &self,
        local_user_id: &str,
    ) -> Result<Option<ProjectedUserIdentityRecord>> {
        let mut matches = self
            .list_projected_user_identities()?
            .into_iter()
            .filter(|identity| identity.local_user_id == local_user_id);
        let Some(first) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Ok(None);
        }
        Ok(Some(first))
    }

    // ── Node capabilities ─────────────────────────────────────────────────────

    const NODE_CAPABILITIES_KEY: &'static str = "node_capabilities:local";

    pub fn save_node_capabilities(&self, caps: &NodeCapabilities) -> Result<()> {
        let data = serde_json::to_value(caps)
            .context("GraphDomain::save_node_capabilities: serialize NodeCapabilities")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::NODE_CAPABILITIES_KEY.to_string(),
            kind: NODE_KIND_NODE_CAPABILITIES.to_string(),
            label: None,
            data,
        })
    }

    pub fn load_node_capabilities(&self) -> Result<Option<NodeCapabilities>> {
        match self.adapter.get_node(Self::NODE_CAPABILITIES_KEY)? {
            None => Ok(None),
            Some(node) => Ok(Some(serde_json::from_value(node.data).context(
                "GraphDomain::load_node_capabilities: deserialize NodeCapabilities",
            )?)),
        }
    }

    // ── Config values ─────────────────────────────────────────────────────────

    fn config_key(key: &str) -> String {
        format!("{}:{}", NODE_KIND_CONFIG, key)
    }

    /// Store an arbitrary JSON config value by key.
    pub fn set_config_value(&self, key: &str, value_json: &str) -> Result<()> {
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::config_key(key),
            kind: NODE_KIND_CONFIG.to_string(),
            label: Some(key.to_string()),
            data: serde_json::json!({ "value": value_json }),
        })
    }

    /// Load an arbitrary JSON config value by key.
    pub fn get_config_value(&self, key: &str) -> Result<Option<String>> {
        match self.adapter.get_node(&Self::config_key(key))? {
            None => Ok(None),
            Some(node) => Ok(node
                .data
                .get("value")
                .and_then(|v| v.as_str())
                .map(str::to_string)),
        }
    }

    /// Delete an arbitrary JSON config value by key.
    pub fn remove_config_value(&self, key: &str) -> Result<()> {
        self.adapter.delete_node(&Self::config_key(key))
    }

    // ── Vault registry (stored as a config value) ─────────────────────────────

    pub fn get_vault_registry(&self) -> Result<Vec<VaultRegistryEntry>> {
        Ok(self
            .get_config_value(CONFIG_VAULT_REGISTRY)?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default())
    }

    pub fn upsert_vault_registry_entry(&self, entry: &VaultRegistryEntry) -> Result<()> {
        let mut entries = self.get_vault_registry()?;
        match entries
            .iter()
            .position(|e| e.vault_name == entry.vault_name)
        {
            Some(pos) => entries[pos] = entry.clone(),
            None => entries.push(entry.clone()),
        }
        self.set_config_value(CONFIG_VAULT_REGISTRY, &serde_json::to_string(&entries)?)
    }

    pub fn remove_vault_registry_entry(&self, vault_name: &str) -> Result<()> {
        let entries: Vec<VaultRegistryEntry> = self
            .get_vault_registry()?
            .into_iter()
            .filter(|e| e.vault_name != vault_name)
            .collect();
        self.set_config_value(CONFIG_VAULT_REGISTRY, &serde_json::to_string(&entries)?)
    }

    // ── Muninn endpoint (stored as a config value) ────────────────────────────

    pub fn get_muninn_endpoint(&self) -> Result<Option<String>> {
        self.get_config_value(CONFIG_MUNINN_ENDPOINT)?
            .map(|raw| serde_json::from_str::<String>(&raw).map_err(anyhow::Error::from))
            .transpose()
    }

    pub fn set_muninn_endpoint(&self, url: &str) -> Result<()> {
        self.set_config_value(CONFIG_MUNINN_ENDPOINT, &serde_json::to_string(url)?)
    }

    // ── Graph runner registry (stored as a config value) ──────────────────────

    pub fn get_graph_runner_registry(&self) -> Result<Vec<GraphRunnerInstanceRecord>> {
        Ok(self
            .get_config_value(CONFIG_GRAPH_RUNNER_REGISTRY)?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default())
    }

    pub fn upsert_graph_runner_instance(&self, record: &GraphRunnerInstanceRecord) -> Result<()> {
        let mut entries = self.get_graph_runner_registry()?;
        match entries.iter().position(|e| e.graph_id == record.graph_id) {
            Some(pos) => entries[pos] = record.clone(),
            None => entries.push(record.clone()),
        }
        self.set_config_value(
            CONFIG_GRAPH_RUNNER_REGISTRY,
            &serde_json::to_string(&entries)?,
        )
    }

    pub fn get_graph_runner_instance(
        &self,
        graph_id: &str,
    ) -> Result<Option<GraphRunnerInstanceRecord>> {
        Ok(self
            .get_graph_runner_registry()?
            .into_iter()
            .find(|e| e.graph_id == graph_id))
    }

    // ── Memory apartments ─────────────────────────────────────────────────────

    fn apartment_key(agent_id: &str, memory_type: &str) -> String {
        format!("{}:{}:{}", NODE_KIND_APARTMENT, agent_id, memory_type)
    }

    /// Upsert a memory apartment using Last-Writer-Wins semantics.
    pub fn sync_apartment(
        &self,
        agent_id: &str,
        memory_type: &str,
        content_json: &serde_json::Value,
    ) -> Result<()> {
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::apartment_key(agent_id, memory_type),
            kind: NODE_KIND_APARTMENT.to_string(),
            label: Some(format!("{}:{}", agent_id, memory_type)),
            data: serde_json::json!({
                "agent_id": agent_id,
                "memory_type": memory_type,
                "content": content_json,
            }),
        })
    }

    pub fn get_apartment(
        &self,
        agent_id: &str,
        memory_type: &str,
    ) -> Result<Option<serde_json::Value>> {
        match self
            .adapter
            .get_node(&Self::apartment_key(agent_id, memory_type))?
        {
            None => Ok(None),
            Some(node) => Ok(node.data.get("content").cloned()),
        }
    }

    /// List apartment memory types for an agent.
    pub fn list_apartments(&self, agent_id: &str) -> Result<Vec<String>> {
        let prefix = format!("{}:{}:", NODE_KIND_APARTMENT, agent_id);
        let mut out = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_APARTMENT)? {
            if !node.node_key.starts_with(&prefix) {
                continue;
            }
            if let Some(memory_type) = node.node_key.rsplit(':').next() {
                if !memory_type.is_empty() {
                    out.push(memory_type.to_string());
                }
            }
        }
        Ok(out)
    }

    // ── Cron job methods ──────────────────────────────────────────────────────

    fn cron_job_key(id: &str) -> String {
        format!("{}:{}", NODE_KIND_CRON_JOB, id)
    }

    /// Upsert a cron job record as a graph node.
    pub fn upsert_cron_job(&self, job: &CronJob) -> Result<()> {
        let data =
            serde_json::to_value(job).context("GraphDomain::upsert_cron_job: serialize CronJob")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::cron_job_key(&job.id),
            kind: NODE_KIND_CRON_JOB.to_string(),
            label: Some(job.target_role.clone()),
            data,
        })
    }

    /// Load a cron job by id.
    pub fn get_cron_job(&self, id: &str) -> Result<Option<CronJob>> {
        match self.adapter.get_node(&Self::cron_job_key(id))? {
            None => Ok(None),
            Some(node) => {
                let job = serde_json::from_value(node.data)
                    .context("GraphDomain::get_cron_job: deserialize CronJob")?;
                Ok(Some(job))
            }
        }
    }

    /// Remove a cron job by id. No-op if not present.
    pub fn remove_cron_job(&self, id: &str) -> Result<()> {
        self.adapter.delete_node(&Self::cron_job_key(id))
    }

    /// List all cron job records.
    pub fn list_cron_jobs(&self) -> Result<Vec<CronJob>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_CRON_JOB)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_cron_jobs: deserialize CronJob")
            })
            .collect()
    }

    /// List cron jobs that are due to fire.
    ///
    /// Returns all jobs where `enabled = true` AND
    /// `next_fire_at + offset_ms <= now_ms`.
    pub fn list_due_cron_jobs(&self, now_ms: u64, offset_ms: u64) -> Result<Vec<CronJob>> {
        Ok(self
            .list_cron_jobs()?
            .into_iter()
            .filter(|j| j.enabled && j.next_fire_at.saturating_add(offset_ms) <= now_ms)
            .collect())
    }

    // ── User task methods ─────────────────────────────────────────────────────

    pub fn user_task_key(task_id: &str) -> String {
        format!("{}:{}", NODE_KIND_USER_TASK, task_id)
    }

    /// Upsert a user task. `task_json` is the full task document as a JSON Value.
    pub fn upsert_user_task(&self, task_json: serde_json::Value, task_id: &str) -> Result<()> {
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::user_task_key(task_id),
            kind: NODE_KIND_USER_TASK.to_string(),
            label: Some(task_id.to_string()),
            data: task_json,
        })
    }

    /// Retrieve a user task by ID, returning its data as a JSON Value.
    pub fn get_user_task(&self, task_id: &str) -> Result<Option<serde_json::Value>> {
        match self.adapter.get_node(&Self::user_task_key(task_id))? {
            None => Ok(None),
            Some(node) => Ok(Some(node.data)),
        }
    }

    /// List user tasks, optionally filtered by session_id and/or agent_id.
    pub fn list_user_tasks(
        &self,
        session_id: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>> {
        let mut out = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_USER_TASK)? {
            if let Some(sid) = session_id {
                if node.data.get("session_id").and_then(|v| v.as_str()) != Some(sid) {
                    continue;
                }
            }
            if let Some(aid) = agent_id {
                if node.data.get("agent_id").and_then(|v| v.as_str()) != Some(aid) {
                    continue;
                }
            }
            out.push(node.data);
        }
        Ok(out)
    }
}

// ── Model profile ─────────────────────────────────────────────────────────────

impl GraphDomain {
    fn model_profile_key(model_ref: &str, node_id: &str) -> String {
        format!("{}:{}:{}", NODE_KIND_MODEL_PROFILE, model_ref, node_id)
    }

    /// Upsert a model provider's operational profile.
    pub fn upsert_model_profile(&self, profile: &ModelProfileRecord) -> Result<()> {
        let data = serde_json::to_value(profile)
            .context("GraphDomain::upsert_model_profile: serialize")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::model_profile_key(&profile.model_ref, &profile.node_id),
            kind: NODE_KIND_MODEL_PROFILE.to_string(),
            label: Some(format!("{}@{}", profile.model_ref, profile.node_id)),
            data,
        })
    }

    pub fn get_model_profile(
        &self,
        model_ref: &str,
        node_id: &str,
    ) -> Result<Option<ModelProfileRecord>> {
        let key = Self::model_profile_key(model_ref, node_id);
        match self.adapter.get_node(&key)? {
            Some(node) => Ok(Some(
                serde_json::from_value(node.data)
                    .context("GraphDomain::get_model_profile: deserialize")?,
            )),
            None => Ok(None),
        }
    }

    pub fn list_model_profiles(&self) -> Result<Vec<ModelProfileRecord>> {
        let mut out = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_MODEL_PROFILE)? {
            out.push(
                serde_json::from_value(node.data)
                    .context("GraphDomain::list_model_profiles: deserialize")?,
            );
        }
        Ok(out)
    }

    /// Record the outcome of a single dispatch attempt, updating the profile's
    /// latency_p50_ms (EMA α=0.25) and error_rate (EMA α=0.1), degrading after
    /// N consecutive failures and recovering on the first success (the shared
    /// state machine in [`crate::model_oracle::apply_model_outcome`]).
    pub fn observe_model_outcome(
        &self,
        model_ref: &str,
        node_id: &str,
        latency_ms: u64,
        success: bool,
    ) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut profile = self
            .get_model_profile(model_ref, node_id)?
            .unwrap_or_else(|| {
                // The provider id doubles as the model_ref at this granularity;
                // seed capability flags honestly from the provider name.
                let (supports_tools, supports_structured) =
                    crate::model_oracle::seed_capabilities_for_provider(model_ref);
                ModelProfileRecord {
                    model_ref: model_ref.to_string(),
                    node_id: node_id.to_string(),
                    provider: model_ref.to_string(),
                    latency_p50_ms: latency_ms,
                    updated_secs: now,
                    supports_tools,
                    supports_structured,
                    ..Default::default()
                }
            });

        crate::model_oracle::apply_model_outcome(
            &mut profile,
            latency_ms,
            success,
            now,
            crate::model_oracle::degrade_threshold_from_env(),
        );

        self.upsert_model_profile(&profile)
    }

    /// Return model profiles for `task_kind` on `node_id`, sorted by health then latency.
    /// Degraded or unavailable models are included but sorted last.
    pub fn best_model_for(
        &self,
        task_kind: &str,
        node_id: &str,
    ) -> Result<Vec<ModelProfileRecord>> {
        let mut profiles: Vec<ModelProfileRecord> = self
            .list_model_profiles()?
            .into_iter()
            .filter(|p| p.node_id == node_id)
            .filter(|p| p.task_kinds.is_empty() || p.task_kinds.iter().any(|t| t == task_kind))
            .collect();

        profiles.sort_by(|a, b| {
            let a_degraded = a.status != "healthy";
            let b_degraded = b.status != "healthy";
            match (a_degraded, b_degraded) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => a.latency_p50_ms.cmp(&b.latency_p50_ms),
            }
        });

        Ok(profiles)
    }

    // ── Autonomy grant methods (Autopoiesis Slice A1) ─────────────────────────

    fn autonomy_grant_key(lane: &str) -> String {
        format!("{}:{}", NODE_KIND_AUTONOMY_GRANT, lane)
    }

    fn autonomy_audit_key(audit_id: &str) -> String {
        format!("{}:{}", NODE_KIND_AUTONOMY_AUDIT, audit_id)
    }

    /// Upsert a per-lane autonomy grant as a graph node.
    pub fn upsert_autonomy_grant(&self, grant: &AutonomyGrant) -> Result<()> {
        let data = serde_json::to_value(grant)
            .context("GraphDomain::upsert_autonomy_grant: serialize AutonomyGrant")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::autonomy_grant_key(grant.lane.as_str()),
            kind: NODE_KIND_AUTONOMY_GRANT.to_string(),
            label: Some(grant.lane.as_str().to_string()),
            data,
        })
    }

    /// Load the autonomy grant for `lane`.
    pub fn get_autonomy_grant(&self, lane: &str) -> Result<Option<AutonomyGrant>> {
        match self.adapter.get_node(&Self::autonomy_grant_key(lane))? {
            None => Ok(None),
            Some(node) => Ok(Some(
                serde_json::from_value(node.data)
                    .context("GraphDomain::get_autonomy_grant: deserialize AutonomyGrant")?,
            )),
        }
    }

    /// Load the grant for `lane`, creating it at the safest posture
    /// ([`crate::autonomy::AutonomyPosture::ProposalOnly`]) if absent.
    ///
    /// Grants are never created above ProposalOnly — trust is earned per-lane
    /// via operator-confirmed outcomes (Autonomy Contract rule 2).
    pub fn get_or_create_autonomy_grant(&self, lane: &str, now: u64) -> Result<AutonomyGrant> {
        if let Some(grant) = self.get_autonomy_grant(lane)? {
            return Ok(grant);
        }
        let grant = AutonomyGrant::new(AutonomyLane::new(lane), now);
        self.upsert_autonomy_grant(&grant)?;
        Ok(grant)
    }

    /// List all autonomy grants.
    pub fn list_autonomy_grants(&self) -> Result<Vec<AutonomyGrant>> {
        self.adapter
            .list_nodes_by_kind(NODE_KIND_AUTONOMY_GRANT)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_autonomy_grants: deserialize AutonomyGrant")
            })
            .collect()
    }

    /// Apply an [`Outcome`] to the grant for `lane` and persist the result.
    ///
    /// When the transition freezes the lane (consecutive-failure ceiling),
    /// this also writes an `autonomy_audit` record describing the freeze so
    /// operator review has an anchor. Returns the transition.
    pub fn record_autonomy_outcome(
        &self,
        lane: &str,
        outcome: Outcome,
        now: u64,
    ) -> Result<Transition> {
        let mut grant = self.get_or_create_autonomy_grant(lane, now)?;
        let transition = record_outcome(&mut grant, outcome, now);
        self.upsert_autonomy_grant(&grant)?;
        if transition == Transition::Frozen {
            let audit = AutonomyAuditRecord::new(
                format!("freeze:{}:{}", lane, now),
                grant.lane.clone(),
                format!(
                    "lane frozen: {} consecutive failures reached ceiling {}",
                    grant.earned.consecutive_failures, grant.budget.max_consecutive_failures
                ),
                &format!(
                    "consecutive_failures={} max_consecutive_failures={} posture retained at {:?}",
                    grant.earned.consecutive_failures,
                    grant.budget.max_consecutive_failures,
                    grant.posture
                ),
                "operator review: clear via GraphDomain::clear_autonomy_freeze",
                grant.posture,
                now,
            );
            self.record_autonomy_audit(&audit)?;
        }
        Ok(transition)
    }

    /// Explicit operator action: clear a lane's freeze flag and reset its
    /// consecutive-failure streak. Returns `false` when no grant exists.
    pub fn clear_autonomy_freeze(&self, lane: &str, now: u64) -> Result<bool> {
        let Some(mut grant) = self.get_autonomy_grant(lane)? else {
            return Ok(false);
        };
        grant.frozen_until_operator_review = false;
        grant.earned.consecutive_failures = 0;
        grant.updated_at = now;
        self.upsert_autonomy_grant(&grant)?;
        Ok(true)
    }

    /// Record an autonomy audit entry. Distinct `audit_id`s never overwrite —
    /// the trail is append-only by construction.
    pub fn record_autonomy_audit(&self, audit: &AutonomyAuditRecord) -> Result<()> {
        let data = serde_json::to_value(audit)
            .context("GraphDomain::record_autonomy_audit: serialize AutonomyAuditRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::autonomy_audit_key(&audit.audit_id),
            kind: NODE_KIND_AUTONOMY_AUDIT.to_string(),
            label: Some(audit.lane.as_str().to_string()),
            data,
        })
    }

    /// Load one audit record by id.
    pub fn get_autonomy_audit(&self, audit_id: &str) -> Result<Option<AutonomyAuditRecord>> {
        match self.adapter.get_node(&Self::autonomy_audit_key(audit_id))? {
            None => Ok(None),
            Some(node) => Ok(Some(serde_json::from_value(node.data).context(
                "GraphDomain::get_autonomy_audit: deserialize AutonomyAuditRecord",
            )?)),
        }
    }

    /// List all audit records for `lane`, oldest first.
    pub fn list_autonomy_audits_by_lane(&self, lane: &str) -> Result<Vec<AutonomyAuditRecord>> {
        let mut out = Vec::new();
        for node in self.adapter.list_nodes_by_kind(NODE_KIND_AUTONOMY_AUDIT)? {
            let record: AutonomyAuditRecord = serde_json::from_value(node.data).context(
                "GraphDomain::list_autonomy_audits_by_lane: deserialize AutonomyAuditRecord",
            )?;
            if record.lane.as_str() == lane {
                out.push(record);
            }
        }
        out.sort_by_key(|r| r.created_at);
        Ok(out)
    }

    /// Update the review outcome on an existing audit record. Returns `false`
    /// when the record does not exist.
    pub fn set_autonomy_audit_outcome(
        &self,
        audit_id: &str,
        outcome: AuditOutcome,
        now: u64,
    ) -> Result<bool> {
        let Some(mut record) = self.get_autonomy_audit(audit_id)? else {
            return Ok(false);
        };
        record.outcome = outcome;
        record.updated_at = now;
        self.record_autonomy_audit(&record)?;
        Ok(true)
    }

    // ── Heal work items (Autopoiesis Slice A3) ────────────────────────────────

    fn heal_work_item_key(work_item_id: &str) -> String {
        format!("{}:{}", NODE_KIND_HEAL_WORK_ITEM, work_item_id)
    }

    /// Upsert a heal work item as a graph node.
    pub fn upsert_heal_work_item(&self, item: &HealWorkItemRecord) -> Result<()> {
        let data = serde_json::to_value(item)
            .context("GraphDomain::upsert_heal_work_item: serialize HealWorkItemRecord")?;
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::heal_work_item_key(&item.work_item_id),
            kind: NODE_KIND_HEAL_WORK_ITEM.to_string(),
            label: Some(format!("{}@{}", item.pattern_tag, item.guest_id)),
            data,
        })
    }

    /// Load one heal work item by id.
    pub fn get_heal_work_item(&self, work_item_id: &str) -> Result<Option<HealWorkItemRecord>> {
        match self
            .adapter
            .get_node(&Self::heal_work_item_key(work_item_id))?
        {
            None => Ok(None),
            Some(node) => Ok(Some(serde_json::from_value(node.data).context(
                "GraphDomain::get_heal_work_item: deserialize HealWorkItemRecord",
            )?)),
        }
    }

    /// List all heal work items, oldest first.
    pub fn list_heal_work_items(&self) -> Result<Vec<HealWorkItemRecord>> {
        let mut out: Vec<HealWorkItemRecord> = self
            .adapter
            .list_nodes_by_kind(NODE_KIND_HEAL_WORK_ITEM)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_heal_work_items: deserialize HealWorkItemRecord")
            })
            .collect::<Result<_>>()?;
        out.sort_by_key(|item| item.created_at);
        Ok(out)
    }

    /// Find the OPEN heal work item for `(pattern_tag, guest_id)`, if any.
    ///
    /// This is the dedup lookup: the filing path bumps the open item instead
    /// of creating a second one for the same recurring pattern.
    pub fn find_open_heal_work_item(
        &self,
        pattern_tag: &str,
        guest_id: &str,
    ) -> Result<Option<HealWorkItemRecord>> {
        Ok(self.list_heal_work_items()?.into_iter().find(|item| {
            item.status == HEAL_WORK_ITEM_STATUS_OPEN
                && item.pattern_tag == pattern_tag
                && item.guest_id == guest_id
        }))
    }

    /// Close a heal work item (the reversal path named in the filing's audit
    /// record). Returns `false` when the item does not exist.
    pub fn close_heal_work_item(&self, work_item_id: &str, now: u64) -> Result<bool> {
        let Some(mut item) = self.get_heal_work_item(work_item_id)? else {
            return Ok(false);
        };
        item.status = HEAL_WORK_ITEM_STATUS_CLOSED.to_string();
        item.last_seen = now;
        self.upsert_heal_work_item(&item)?;
        Ok(true)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{
        AbstractModelRecord, AbstractRightRecord, AbstractSkillRecord, RoleIncarnationRecord,
        SkillRegistrationAuditRecord, ToolsetProfileRecord,
    };
    use crate::sqlite_storage::SqliteGraphStorage;
    use crate::storage::{
        AgentIdentityRecord, GuestRecord, SecretRecord, SessionRecord, VaultRegistryEntry,
    };
    use crate::{NodeCapabilities, NodeConstraints};

    fn make_domain() -> GraphDomain {
        let storage =
            SqliteGraphStorage::open_in_memory().expect("in-memory SqliteGraphStorage failed");
        GraphDomain::new(Arc::new(storage.adapter()))
    }

    #[test]
    fn autonomy_grant_storage_round_trip() {
        let domain = make_domain();
        let lane = crate::autonomy::LANE_GRAPH_BRIDGE_EDGES;
        assert!(domain.get_autonomy_grant(lane).expect("get").is_none());

        // First creation always starts at ProposalOnly — the safest level.
        let created = domain
            .get_or_create_autonomy_grant(lane, 1_000)
            .expect("create");
        assert_eq!(
            created.posture,
            crate::autonomy::AutonomyPosture::ProposalOnly
        );
        assert_eq!(created.earned.required_for_promotion, 5);

        // Round-trip: what we stored is what we load.
        let loaded = domain
            .get_autonomy_grant(lane)
            .expect("get")
            .expect("grant exists");
        assert_eq!(loaded, created);

        // get_or_create is idempotent — no reset of an existing grant.
        let mut mutated = loaded.clone();
        mutated.posture = crate::autonomy::AutonomyPosture::ConfirmFirst;
        mutated.earned.confirmed_good_outcomes = 3;
        mutated.updated_at = 2_000;
        domain.upsert_autonomy_grant(&mutated).expect("upsert");
        let again = domain
            .get_or_create_autonomy_grant(lane, 9_999)
            .expect("get_or_create existing");
        assert_eq!(again, mutated);

        // Second lane shows up alongside the first in list.
        domain
            .get_or_create_autonomy_grant(crate::autonomy::LANE_FLEET_HEAL_SLICES, 3_000)
            .expect("create second");
        let mut grants = domain.list_autonomy_grants().expect("list");
        grants.sort_by(|a, b| a.lane.as_str().cmp(b.lane.as_str()));
        assert_eq!(grants.len(), 2);
        assert_eq!(
            grants[0].lane.as_str(),
            crate::autonomy::LANE_FLEET_HEAL_SLICES
        );
        assert_eq!(grants[1].lane.as_str(), lane);
    }

    #[test]
    fn autonomy_outcome_persists_and_freeze_writes_audit() {
        let domain = make_domain();
        let lane = crate::autonomy::LANE_WORK_FILE_PROPOSALS;

        // Failures up to the ceiling freeze the lane and write an audit record.
        let mut last = Transition::NoChange;
        let cap = crate::autonomy::AutonomyBudget::default().max_consecutive_failures;
        for i in 0..cap {
            last = domain
                .record_autonomy_outcome(lane, Outcome::Failure, 1_000 + u64::from(i))
                .expect("record failure");
        }
        assert_eq!(last, Transition::Frozen);
        let grant = domain
            .get_autonomy_grant(lane)
            .expect("get")
            .expect("grant exists");
        assert!(grant.frozen_until_operator_review);
        let audits = domain
            .list_autonomy_audits_by_lane(lane)
            .expect("list audits");
        assert_eq!(audits.len(), 1);
        assert!(audits[0].audit_id.starts_with("freeze:"));
        assert_eq!(audits[0].outcome, AuditOutcome::Pending);

        // Unfreeze semantics: explicit operator action clears the flag and
        // resets the failure streak.
        assert!(domain.clear_autonomy_freeze(lane, 5_000).expect("clear"));
        let grant = domain
            .get_autonomy_grant(lane)
            .expect("get")
            .expect("grant exists");
        assert!(!grant.frozen_until_operator_review);
        assert_eq!(grant.earned.consecutive_failures, 0);
        assert_eq!(grant.updated_at, 5_000);

        // Clearing an unknown lane reports false.
        assert!(!domain
            .clear_autonomy_freeze("no.such_lane", 5_001)
            .expect("clear unknown"));
    }

    #[test]
    fn autonomy_audit_round_trip_and_list_by_lane() {
        let domain = make_domain();
        let lane_a = crate::autonomy::LANE_STEWARD_ACTIVE_CHECKINS;
        let lane_b = crate::autonomy::LANE_WORK_EXECUTE_SLICES;

        let newer = AutonomyAuditRecord::new(
            "audit-2",
            AutonomyLane::new(lane_a),
            "sent active check-in",
            "5 confirmed SIL entries",
            "retract the check-in message",
            crate::autonomy::AutonomyPosture::AutoWithAudit,
            2_000,
        );
        let older = AutonomyAuditRecord::new(
            "audit-1",
            AutonomyLane::new(lane_a),
            "sent active check-in",
            "operator confirmed entry",
            "retract the check-in message",
            crate::autonomy::AutonomyPosture::ConfirmFirst,
            1_000,
        );
        let other_lane = AutonomyAuditRecord::new(
            "audit-3",
            AutonomyLane::new(lane_b),
            "drafted slice plan",
            "top scored proposal",
            "close the draft PR",
            crate::autonomy::AutonomyPosture::ProposalOnly,
            1_500,
        );
        // Insert out of order to prove created_at sorting.
        domain.record_autonomy_audit(&newer).expect("record newer");
        domain.record_autonomy_audit(&older).expect("record older");
        domain
            .record_autonomy_audit(&other_lane)
            .expect("record other lane");

        let audits = domain.list_autonomy_audits_by_lane(lane_a).expect("list");
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0], older);
        assert_eq!(audits[1], newer);

        // Outcome update round-trips.
        assert!(domain
            .set_autonomy_audit_outcome("audit-1", AuditOutcome::Confirmed, 3_000)
            .expect("set outcome"));
        let updated = domain
            .get_autonomy_audit("audit-1")
            .expect("get")
            .expect("exists");
        assert_eq!(updated.outcome, AuditOutcome::Confirmed);
        assert_eq!(updated.updated_at, 3_000);
        assert_eq!(updated.created_at, 1_000);
        assert!(!domain
            .set_autonomy_audit_outcome("missing", AuditOutcome::Reversed, 3_001)
            .expect("set missing"));
    }

    #[test]
    fn autonomy_state_machine_round_trips_through_storage() {
        // Pure-function transitions survive persistence: promote a lane to
        // ConfirmFirst through stored outcomes, then reverse it back down.
        let domain = make_domain();
        let lane = crate::autonomy::LANE_GRAPH_BRIDGE_EDGES;
        let mut last = Transition::NoChange;
        for i in 0..5u64 {
            last = domain
                .record_autonomy_outcome(lane, Outcome::ConfirmedGood, 100 + i)
                .expect("confirm");
        }
        assert_eq!(
            last,
            Transition::Promoted {
                from: crate::autonomy::AutonomyPosture::ProposalOnly,
                to: crate::autonomy::AutonomyPosture::ConfirmFirst,
            }
        );
        let t = domain
            .record_autonomy_outcome(lane, Outcome::OperatorReversal, 200)
            .expect("reversal");
        assert_eq!(
            t,
            Transition::Demoted {
                from: crate::autonomy::AutonomyPosture::ConfirmFirst,
                to: crate::autonomy::AutonomyPosture::ProposalOnly,
            }
        );
        let grant = domain
            .get_autonomy_grant(lane)
            .expect("get")
            .expect("exists");
        assert_eq!(grant.earned.confirmed_good_outcomes, 0);
    }

    #[test]
    fn skill_registration_audits_are_append_only() {
        let domain = make_domain();
        assert!(domain
            .list_skill_registration_audits()
            .expect("list")
            .is_empty());

        let first = SkillRegistrationAuditRecord {
            audit_id: "audit-1".into(),
            skill_name: "research".into(),
            registered_by: "agent-jane-01:orchestrator".into(),
            registered_by_role: "orchestrator".into(),
            validation_state: "validated".into(),
            registered_at: 1000,
        };
        domain
            .record_skill_registration_audit(&first)
            .expect("record first audit");

        let second = SkillRegistrationAuditRecord {
            audit_id: "audit-2".into(),
            skill_name: "summarize".into(),
            registered_by: "mgmt-01".into(),
            registered_by_role: "management".into(),
            validation_state: "draft".into(),
            registered_at: 2000,
        };
        domain
            .record_skill_registration_audit(&second)
            .expect("record second audit");

        // Distinct audit_ids never overwrite — both entries survive.
        let mut audits = domain.list_skill_registration_audits().expect("list");
        audits.sort_by(|a, b| a.audit_id.cmp(&b.audit_id));
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0], first);
        assert_eq!(audits[1], second);
    }

    fn caps() -> NodeCapabilities {
        NodeCapabilities {
            node_id: "test-node".to_string(),
            roles: vec![],
            models: vec![],
            tools: vec![],
            constraints: NodeConstraints::default(),
        }
    }

    fn hotel(name: &str) -> HotelRecord {
        HotelRecord {
            hotel_name: name.to_string(),
            capabilities: caps(),
            mesh_host: None,
            mesh_port: 8999,
            blob_port: 9001,
            execution_port: 9002,
            ipc_socket_path: "/tmp/philotic-aiua.sock".to_string(),
            active_pid: None,
        }
    }

    fn tool(name: &str) -> AbstractToolRecord {
        AbstractToolRecord {
            tool_name: name.to_string(),
            description: format!("Description for {}", name),
            input_schema: serde_json::json!({"type": "object"}),
            class: "utility".to_string(),
            tool_markers: Vec::new(),
        }
    }

    fn model(
        model_ref: &str,
        provider_hint: &str,
        capability_markers: &[&str],
    ) -> AbstractModelRecord {
        AbstractModelRecord {
            model_ref: model_ref.to_string(),
            provider_hint: provider_hint.to_string(),
            description: format!("Description for {}", model_ref),
            capability_markers: capability_markers
                .iter()
                .map(|v| (*v).to_string())
                .collect(),
            endpoint_stem: None,
            speed_marker: 80,
            thinking_marker: 70,
            tool_use_marker: 60,
            audio_native_marker: 0,
        }
    }

    fn right(name: &str, target_kind: &str, target_ref: &str) -> AbstractRightRecord {
        AbstractRightRecord {
            right_name: name.to_string(),
            description: format!("Description for {}", name),
            target_kind: target_kind.to_string(),
            target_ref: target_ref.to_string(),
        }
    }

    fn rule(id: &str, agent: &str) -> RuleRecord {
        RuleRecord {
            rule_id: id.to_string(),
            agent_id: agent.to_string(),
            description: "Always ask before deleting files.".to_string(),
            rationale: "Prevents accidental data loss.".to_string(),
            created_at: 1_700_000_000,
        }
    }

    fn routing_policy(id: &str, agent: &str) -> RoutingPolicyRecord {
        RoutingPolicyRecord {
            proposal_id: id.to_string(),
            agent_id: agent.to_string(),
            problem: "Voice ingress keeps surfacing remote tools too early.".to_string(),
            proposed_change: "Dampen remote tool reflex during receptor ingress.".to_string(),
            evidence: "Observed low-intent voice ingress requesting remote tools.".to_string(),
            affected_stage: Some("ingress".to_string()),
            affected_capability: Some("voice.transcribe".to_string()),
            learned_reflex_preference_key: Some("operator-mesh-trust".to_string()),
            operator_disposition: crate::graph::RoutingPolicyDispositionRecord {
                state: "approved".to_string(),
                reason: "Approved via operator-gated routing.policy.propose.".to_string(),
                decided_at: 1_700_000_001,
            },
            evaluations: vec![crate::graph::RoutingPolicyEvaluationRecord {
                evaluation_kind: "operator_disposition".to_string(),
                decision: "approved".to_string(),
                reason: "Approved via operator-gated tool execution.".to_string(),
                created_at: 1_700_000_001,
                source_tool: Some("routing.policy.propose".to_string()),
            }],
            created_at: 1_700_000_000,
        }
    }

    // ── Hotel ─────────────────────────────────────────────────────────────────

    #[test]
    fn hotel_roundtrip() {
        let d = make_domain();
        d.upsert_hotel(&hotel("default")).unwrap();
        let h = d.get_hotel("default").unwrap().unwrap();
        assert_eq!(h.hotel_name, "default");
        assert_eq!(h.mesh_port, 8999);
    }

    #[test]
    fn hotel_missing_returns_none() {
        assert!(make_domain().get_hotel("ghost").unwrap().is_none());
    }

    #[test]
    fn hotel_list() {
        let d = make_domain();
        for name in ["alpha", "beta", "gamma"] {
            d.upsert_hotel(&hotel(name)).unwrap();
        }
        let names: Vec<_> = d
            .list_hotels()
            .unwrap()
            .into_iter()
            .map(|h| h.hotel_name)
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"alpha".to_string()));
        assert!(names.contains(&"beta".to_string()));
        assert!(names.contains(&"gamma".to_string()));
    }

    #[test]
    fn hotel_upsert_overwrites() {
        let d = make_domain();
        d.upsert_hotel(&hotel("default")).unwrap();
        let mut h2 = hotel("default");
        h2.mesh_port = 9100;
        d.upsert_hotel(&h2).unwrap();
        assert_eq!(d.get_hotel("default").unwrap().unwrap().mesh_port, 9100);
        assert_eq!(d.list_hotels().unwrap().len(), 1);
    }

    // ── AbstractTool ──────────────────────────────────────────────────────────

    #[test]
    fn abstract_tool_roundtrip() {
        let d = make_domain();
        d.upsert_abstract_tool(&tool("bash.exec")).unwrap();
        let t = d.get_abstract_tool("bash.exec").unwrap().unwrap();
        assert_eq!(t.tool_name, "bash.exec");
        assert_eq!(t.class, "utility");
        assert!(t.tool_markers.is_empty());
    }

    #[test]
    fn abstract_tool_missing_returns_none() {
        assert!(make_domain()
            .get_abstract_tool("no.such.tool")
            .unwrap()
            .is_none());
    }

    #[test]
    fn abstract_tool_list() {
        let d = make_domain();
        for name in ["tool.a", "tool.b", "tool.c"] {
            d.upsert_abstract_tool(&tool(name)).unwrap();
        }
        assert_eq!(d.list_abstract_tools().unwrap().len(), 3);
    }

    #[test]
    fn abstract_model_roundtrip() {
        let d = make_domain();
        d.upsert_abstract_model(&model(
            "gemini-3.1-flash",
            "gemini",
            &["text.generate", "media.analyze"],
        ))
        .unwrap();
        let record = d.get_abstract_model("gemini-3.1-flash").unwrap().unwrap();
        assert_eq!(record.provider_hint, "gemini");
        assert_eq!(
            record.capability_markers,
            vec!["text.generate", "media.analyze"]
        );
    }

    #[test]
    fn abstract_model_list() {
        let d = make_domain();
        d.upsert_abstract_model(&model("gemini-3.1-flash", "gemini", &["text.generate"]))
            .unwrap();
        d.upsert_abstract_model(&model("scribe_v1", "elevenlabs", &["voice.transcribe"]))
            .unwrap();
        let models = d.list_abstract_models().unwrap();
        assert_eq!(models.len(), 2);
        assert!(models
            .iter()
            .any(|record| record.model_ref == "gemini-3.1-flash"));
        assert!(models.iter().any(|record| record.model_ref == "scribe_v1"));
    }

    #[test]
    fn abstract_right_roundtrip() {
        let d = make_domain();
        d.upsert_abstract_right(&right("tool.echo", "tool", "echo"))
            .unwrap();
        let r = d.get_abstract_right("tool.echo").unwrap().unwrap();
        assert_eq!(r.right_name, "tool.echo");
        assert_eq!(r.target_kind, "tool");
        assert_eq!(r.target_ref, "echo");
    }

    #[test]
    fn abstract_right_list() {
        let d = make_domain();
        for (name, kind, target) in [
            ("tool.echo", "tool", "echo"),
            ("skill.handoff.back", "skill", "handoff.back"),
            ("component.text.generate", "component", "text.generate"),
        ] {
            d.upsert_abstract_right(&right(name, kind, target)).unwrap();
        }
        assert_eq!(d.list_abstract_rights().unwrap().len(), 3);
    }

    // ── Guest ─────────────────────────────────────────────────────────────────

    fn guest(hotel: &str, id: &str) -> GuestRecord {
        GuestRecord {
            hotel_name: hotel.to_string(),
            guest_id: id.to_string(),
            role: "philote".to_string(),
            config_json: "{}".to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        }
    }

    #[test]
    fn guest_roundtrip() {
        let d = make_domain();
        d.upsert_guest(&guest("default", "g1")).unwrap();
        let g = d.get_guest("default", "g1").unwrap().unwrap();
        assert_eq!(g.guest_id, "g1");
        assert!(g.is_active);
    }

    #[test]
    fn guest_list_filters_by_hotel_and_active() {
        let d = make_domain();
        let mut g2 = guest("default", "g2");
        g2.is_active = false;
        d.upsert_guest(&guest("default", "g1")).unwrap();
        d.upsert_guest(&g2).unwrap();
        d.upsert_guest(&guest("other", "g3")).unwrap();

        assert_eq!(d.list_guests("default", false).unwrap().len(), 2);
        assert_eq!(d.list_guests("default", true).unwrap().len(), 1);
        assert_eq!(d.list_guests("other", false).unwrap().len(), 1);
    }

    #[test]
    fn set_guest_pid_updates() {
        let d = make_domain();
        d.upsert_guest(&guest("default", "g1")).unwrap();
        d.set_guest_pid("default", "g1", Some("12345")).unwrap();
        assert_eq!(
            d.get_guest("default", "g1").unwrap().unwrap().active_pid,
            Some("12345".to_string())
        );
        d.set_guest_pid("default", "g1", None).unwrap();
        assert!(d
            .get_guest("default", "g1")
            .unwrap()
            .unwrap()
            .active_pid
            .is_none());
    }

    // ── Agent identity ────────────────────────────────────────────────────────

    #[test]
    fn agent_identity_roundtrip() {
        let d = make_domain();
        let id = AgentIdentityRecord {
            agent_id: "bjork".to_string(),
            persona_name: "Björk".to_string(),
            authority_hotel: "default".to_string(),
            bundle_json: serde_json::json!({}),
        };
        d.upsert_agent_identity(&id).unwrap();
        let loaded = d.get_agent_identity("bjork").unwrap().unwrap();
        assert_eq!(loaded.persona_name, "Björk");
        assert_eq!(d.list_agent_identities().unwrap().len(), 1);
    }

    // ── Session ───────────────────────────────────────────────────────────────

    #[test]
    fn session_roundtrip() {
        let d = make_domain();
        let s = SessionRecord {
            session_id: "sess-1".to_string(),
            session_kind: "telegram".to_string(),
            primary_agent_id: Some("bjork".to_string()),
            active_incarnation_id: None,
            channel_kind: None,
            channel_session_key: None,
            status: "active".to_string(),
            lease_owner_component_id: None,
            lease_expires_at: None,
            summary_json: serde_json::Value::Null,
            created_at: 0,
            updated_at: 0,
        };
        d.upsert_session(&s).unwrap();
        let loaded = d.get_session("sess-1").unwrap().unwrap();
        assert_eq!(loaded.session_kind, "telegram");
    }

    // ── Role incarnation ──────────────────────────────────────────────────────

    #[test]
    fn role_incarnation_roundtrip_and_list() {
        use crate::graph::TurnLoopConfig;
        let d = make_domain();
        let r = RoleIncarnationRecord {
            agent_id: "bjork".to_string(),
            role_name: "coder".to_string(),
            guest_id: "guest-1".to_string(),
            toolset_profile: "default".to_string(),
            role_identity_addendum: None,
            role_manifest: None,
            is_admin: false,
            readiness_state: RoleReadinessState::Configured,
            inactive_ttl_seconds: None,
            turn_loop_config: TurnLoopConfig::default(),
            home_node: None,
        };
        d.upsert_role_incarnation(&r).unwrap();
        let loaded = d.get_role_incarnation("bjork", "coder").unwrap().unwrap();
        assert_eq!(loaded.guest_id, "guest-1");

        assert_eq!(d.list_role_incarnations("bjork").unwrap().len(), 1);
        assert!(d.list_role_incarnations("nobody").unwrap().is_empty());

        let by_guest = d.list_role_incarnations_by_guest_id("guest-1").unwrap();
        assert_eq!(by_guest.len(), 1);
    }

    #[test]
    fn promote_role_incarnation_active_enforces_single_active() {
        use crate::graph::TurnLoopConfig;
        let d = make_domain();
        let mk = |role: &str| RoleIncarnationRecord {
            agent_id: "agent-beacon".to_string(),
            role_name: role.to_string(),
            guest_id: format!("agent-beacon:{role}"),
            toolset_profile: "default".to_string(),
            role_identity_addendum: None,
            role_manifest: None,
            is_admin: false,
            readiness_state: RoleReadinessState::Configured,
            inactive_ttl_seconds: None,
            turn_loop_config: TurnLoopConfig::default(),
            home_node: None,
        };
        d.upsert_role_incarnation(&mk("orchestrator")).unwrap();
        d.upsert_role_incarnation(&mk("Chronos")).unwrap();

        // Promote orchestrator → it is active, Chronos untouched.
        d.promote_role_incarnation_active("agent-beacon", "orchestrator")
            .unwrap();
        assert!(matches!(
            d.get_role_incarnation("agent-beacon", "orchestrator")
                .unwrap()
                .unwrap()
                .readiness_state,
            RoleReadinessState::ActiveInSession
        ));

        // Promote Chronos → orchestrator MUST be demoted (single-active invariant).
        d.promote_role_incarnation_active("agent-beacon", "Chronos")
            .unwrap();
        assert!(matches!(
            d.get_role_incarnation("agent-beacon", "Chronos")
                .unwrap()
                .unwrap()
                .readiness_state,
            RoleReadinessState::ActiveInSession
        ));
        assert!(
            matches!(
                d.get_role_incarnation("agent-beacon", "orchestrator")
                    .unwrap()
                    .unwrap()
                    .readiness_state,
                RoleReadinessState::Routable
            ),
            "previous active incarnation must be demoted, never two active at once"
        );

        // Exactly one incarnation is ActiveInSession.
        let active = d
            .list_role_incarnations("agent-beacon")
            .unwrap()
            .into_iter()
            .filter(|r| matches!(r.readiness_state, RoleReadinessState::ActiveInSession))
            .count();
        assert_eq!(active, 1);
    }

    #[test]
    fn membrane_transport_home_roundtrip_list_and_resolve() {
        let d = make_domain();
        let home = MembraneTransportHomeRecord {
            agent_id: "agent-beacon".to_string(),
            transport: "telegram".to_string(),
            resource_ref: "telegram_bot_token_beacon".to_string(),
            active_home_hotel: "vps-jane".to_string(),
            standby_hotels: vec!["mbp-jane".to_string(), "mac-jane".to_string()],
            managed_by_role: "orchestrator".to_string(),
            lease_type: "telegram_poll".to_string(),
            failover_policy: "manual-or-explicit-delegation".to_string(),
            status: crate::graph::MembraneTransportHomeStatus::Active,
        };

        d.upsert_membrane_transport_home(&home).unwrap();

        let loaded = d
            .get_membrane_transport_home("agent-beacon", "telegram", "telegram_bot_token_beacon")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.active_home_hotel, "vps-jane");
        assert!(loaded.is_active_home("vps-jane"));
        assert!(loaded.is_standby_home("mbp-jane"));
        assert!(!loaded.is_active_home("mbp-jane"));

        let resolved = d
            .resolve_membrane_transport_home(
                "agent-beacon",
                "telegram",
                "telegram_bot_token_beacon",
            )
            .unwrap()
            .unwrap();
        assert_eq!(resolved, home);

        assert_eq!(d.list_membrane_transport_homes(None).unwrap().len(), 1);
        assert_eq!(
            d.list_membrane_transport_homes(Some("agent-beacon"))
                .unwrap()
                .len(),
            1
        );
        assert!(d
            .list_membrane_transport_homes(Some("agent-bjork"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn membrane_transport_home_is_distinct_from_role_home() {
        use crate::graph::TurnLoopConfig;
        let d = make_domain();
        d.upsert_role_incarnation(&RoleIncarnationRecord {
            agent_id: "agent-beacon".to_string(),
            role_name: "orchestrator".to_string(),
            guest_id: "guest-orchestrator".to_string(),
            toolset_profile: "default".to_string(),
            role_identity_addendum: None,
            role_manifest: None,
            is_admin: true,
            readiness_state: RoleReadinessState::Configured,
            inactive_ttl_seconds: None,
            turn_loop_config: TurnLoopConfig::default(),
            home_node: Some("mac-jane".to_string()),
        })
        .unwrap();
        d.upsert_membrane_transport_home(&MembraneTransportHomeRecord {
            agent_id: "agent-beacon".to_string(),
            transport: "telegram".to_string(),
            resource_ref: "telegram_bot_token_beacon".to_string(),
            active_home_hotel: "vps-jane".to_string(),
            standby_hotels: vec!["mac-jane".to_string()],
            managed_by_role: "orchestrator".to_string(),
            lease_type: "telegram_poll".to_string(),
            failover_policy: "manual-or-explicit-delegation".to_string(),
            status: crate::graph::MembraneTransportHomeStatus::Active,
        })
        .unwrap();

        let role = d
            .get_role_incarnation("agent-beacon", "orchestrator")
            .unwrap()
            .unwrap();
        let transport_home = d
            .resolve_membrane_transport_home(
                "agent-beacon",
                "telegram",
                "telegram_bot_token_beacon",
            )
            .unwrap()
            .unwrap();

        assert_eq!(role.home_node.as_deref(), Some("mac-jane"));
        assert_eq!(transport_home.active_home_hotel, "vps-jane");
    }

    // ── Secret ────────────────────────────────────────────────────────────────

    #[test]
    fn secret_roundtrip() {
        let d = make_domain();
        let s = SecretRecord {
            secret_ref: "secret://hotel/default/api-key".to_string(),
            secret_kind: "api_key".to_string(),
            scope: "hotel".to_string(),
            allowed_roles: vec![],
            allowed_guests: vec![],
            ciphertext_b64: "abc123".to_string(),
            nonce_b64: "xyz".to_string(),
            created_at: 0,
            updated_at: 0,
        };
        d.upsert_secret(&s).unwrap();
        let loaded = d
            .get_secret("secret://hotel/default/api-key")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.ciphertext_b64, "abc123");
    }

    // ── Skills and toolset profiles ───────────────────────────────────────────

    #[test]
    fn abstract_skill_roundtrip() {
        let d = make_domain();
        let sk = AbstractSkillRecord {
            skill_name: "code-review".to_string(),
            description: "Reviews code.".to_string(),
            skill_markers: vec!["governed".to_string()],
            ..Default::default()
        };
        d.upsert_abstract_skill(&sk).unwrap();
        assert_eq!(
            d.get_abstract_skill("code-review")
                .unwrap()
                .unwrap()
                .skill_name,
            "code-review"
        );
        assert_eq!(
            d.get_abstract_skill("code-review")
                .unwrap()
                .unwrap()
                .skill_markers,
            vec!["governed".to_string()]
        );
        assert_eq!(d.list_abstract_skills().unwrap().len(), 1);
    }

    #[test]
    fn toolset_profile_roundtrip() {
        let d = make_domain();
        let p = ToolsetProfileRecord {
            profile_name: "dev".to_string(),
            allowed_tools: vec!["bash.exec".to_string()],
            ..Default::default()
        };
        d.upsert_toolset_profile(&p).unwrap();
        let loaded = d.get_toolset_profile("dev").unwrap().unwrap();
        assert_eq!(loaded.allowed_tools, vec!["bash.exec"]);
    }

    #[test]
    fn workflow_skill_roundtrip() {
        let d = make_domain();
        let wf = WorkflowSkillRecord {
            workflow_name: "role.create_or_update".to_string(),
            workflow_kind: "role.configure".to_string(),
            owner_scope: "orchestrator".to_string(),
            target_class: "same_identity_role_definition".to_string(),
            description: "Governed role creation workflow.".to_string(),
            target_selection_policy: serde_json::json!({"selection_mode": "same_agent_role_record"}),
            context_requirements: serde_json::json!({"required_fields": ["role_name", "toolset_profile"]}),
            return_contract: serde_json::json!({"ack": "ConfigureRoleOk"}),
            governance: serde_json::json!({"execution_surface": "role.configure"}),
            rollout_state: "active".to_string(),
        };
        d.upsert_workflow_skill(&wf).unwrap();
        let loaded = d
            .get_workflow_skill("role.create_or_update")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.workflow_kind, "role.configure");
        assert_eq!(loaded.owner_scope, "orchestrator");
        assert_eq!(d.list_workflow_skills().unwrap().len(), 1);
    }

    // ── Config, vault registry, muninn endpoint ───────────────────────────────

    #[test]
    fn config_value_roundtrip() {
        let d = make_domain();
        d.set_config_value("my_key", "\"hello\"").unwrap();
        assert_eq!(d.get_config_value("my_key").unwrap().unwrap(), "\"hello\"");
        assert!(d.get_config_value("missing").unwrap().is_none());
    }

    #[test]
    fn vault_registry_upsert_and_remove() {
        let d = make_domain();
        assert!(d.get_vault_registry().unwrap().is_empty());
        d.upsert_vault_registry_entry(&VaultRegistryEntry {
            vault_name: "self_bjork".to_string(),
            secret_ref: "secret://x".to_string(),
        })
        .unwrap();
        assert_eq!(d.get_vault_registry().unwrap().len(), 1);
        d.remove_vault_registry_entry("self_bjork").unwrap();
        assert!(d.get_vault_registry().unwrap().is_empty());
    }

    #[test]
    fn muninn_endpoint_roundtrip() {
        let d = make_domain();
        assert!(d.get_muninn_endpoint().unwrap().is_none());
        d.set_muninn_endpoint("http://localhost:8750").unwrap();
        assert_eq!(
            d.get_muninn_endpoint().unwrap().unwrap(),
            "http://localhost:8750"
        );
    }

    // ── Node capabilities ─────────────────────────────────────────────────────

    #[test]
    fn node_capabilities_roundtrip() {
        let d = make_domain();
        assert!(d.load_node_capabilities().unwrap().is_none());
        d.save_node_capabilities(&caps()).unwrap();
        let loaded = d.load_node_capabilities().unwrap().unwrap();
        assert_eq!(loaded.node_id, "test-node");
    }

    // ── Memory apartments ─────────────────────────────────────────────────────

    #[test]
    fn apartment_sync_and_get() {
        let d = make_domain();
        let content = serde_json::json!({"summary": "I worked on Philotic today."});
        d.sync_apartment("bjork", "episodic", &content).unwrap();
        let loaded = d.get_apartment("bjork", "episodic").unwrap().unwrap();
        assert_eq!(loaded["summary"], "I worked on Philotic today.");
        assert!(d.get_apartment("bjork", "semantic").unwrap().is_none());
    }

    // ── Rule ──────────────────────────────────────────────────────────────────

    #[test]
    fn rule_roundtrip() {
        let d = make_domain();
        d.upsert_rule(&rule("rule-001", "agent-alice")).unwrap();
        let r = d.get_rule("rule-001").unwrap().unwrap();
        assert_eq!(r.rule_id, "rule-001");
        assert_eq!(r.agent_id, "agent-alice");
    }

    #[test]
    fn rule_missing_returns_none() {
        assert!(make_domain().get_rule("rule-999").unwrap().is_none());
    }

    #[test]
    fn rule_list_filters_by_agent() {
        let d = make_domain();
        d.upsert_rule(&rule("r1", "agent-alice")).unwrap();
        d.upsert_rule(&rule("r2", "agent-bob")).unwrap();
        d.upsert_rule(&rule("r3", "agent-alice")).unwrap();
        d.upsert_rule(&rule("r4", "agent-alice")).unwrap();

        let alice = d.list_rules("agent-alice").unwrap();
        assert_eq!(alice.len(), 3);
        assert!(alice.iter().all(|r| r.agent_id == "agent-alice"));

        assert_eq!(d.list_rules("agent-bob").unwrap().len(), 1);
        assert!(d.list_rules("agent-nobody").unwrap().is_empty());
    }

    #[test]
    fn routing_policy_roundtrip() {
        let d = make_domain();
        d.upsert_routing_policy(&routing_policy("routing-001", "agent-alice"))
            .unwrap();
        let record = d
            .get_routing_policy("routing-001")
            .unwrap()
            .expect("stored routing policy");
        assert_eq!(record.agent_id, "agent-alice");
        assert_eq!(record.operator_disposition.state, "approved");
        assert_eq!(record.evaluations.len(), 1);
    }

    #[test]
    fn routing_policy_list_filters_by_agent() {
        let d = make_domain();
        d.upsert_routing_policy(&routing_policy("routing-001", "agent-alice"))
            .unwrap();
        d.upsert_routing_policy(&routing_policy("routing-002", "agent-bob"))
            .unwrap();
        d.upsert_routing_policy(&routing_policy("routing-003", "agent-alice"))
            .unwrap();

        let alice = d.list_routing_policies("agent-alice").unwrap();
        assert_eq!(alice.len(), 2);
        assert_eq!(d.list_routing_policies("agent-bob").unwrap().len(), 1);
        assert!(d.list_routing_policies("agent-nobody").unwrap().is_empty());
    }

    #[test]
    fn append_routing_policy_evaluation_updates_history() {
        let d = make_domain();
        d.upsert_routing_policy(&routing_policy("routing-001", "agent-alice"))
            .unwrap();
        let appended = d
            .append_routing_policy_evaluation(
                "routing-001",
                crate::graph::RoutingPolicyEvaluationRecord {
                    evaluation_kind: "learned_reflex_writeback".to_string(),
                    decision: "approved_writeback".to_string(),
                    reason: "Learned reflex was persisted into the agent graph.".to_string(),
                    created_at: 1_700_000_002,
                    source_tool: Some("routing.policy.propose".to_string()),
                },
            )
            .unwrap();
        assert!(appended);
        let record = d
            .get_routing_policy("routing-001")
            .unwrap()
            .expect("stored routing policy");
        assert_eq!(record.evaluations.len(), 2);
        assert_eq!(
            record.evaluations[1].evaluation_kind,
            "learned_reflex_writeback"
        );
    }

    #[test]
    fn set_routing_policy_disposition_updates_record_and_appends_history() {
        let d = make_domain();
        d.upsert_routing_policy(&routing_policy("routing-001", "agent-alice"))
            .unwrap();
        let updated = d
            .set_routing_policy_disposition(
                "routing-001",
                "rejected".to_string(),
                "Operator rejected after later review.".to_string(),
                1_700_000_003,
                Some("operator.control".to_string()),
            )
            .unwrap();
        assert!(updated);
        let record = d
            .get_routing_policy("routing-001")
            .unwrap()
            .expect("stored routing policy");
        assert_eq!(record.operator_disposition.state, "rejected");
        assert_eq!(record.evaluations.len(), 2);
        assert_eq!(record.evaluations[1].decision, "rejected");
    }

    #[test]
    fn projected_user_identity_round_trip() {
        let d = make_domain();
        let identity = ProjectedUserIdentityRecord {
            principal_id: "user:google:subject-123".into(),
            local_user_id: "root-user:mac-jane".into(),
            home_hotel: "mac-jane".into(),
            display_name: "Jared Likes".into(),
            preferred_name: Some("Jared".into()),
            primary_email: Some("jared@example.com".into()),
            linked_identities: vec![crate::storage::ProjectedExternalIdentityRecord {
                provider: "google".into(),
                provider_subject: "subject-123".into(),
                email: Some("jared@example.com".into()),
                login: None,
                display_name: Some("Jared Likes".into()),
                verified_at: 123,
                last_seen_at: 456,
            }],
            updated_at: 789,
        };
        d.upsert_projected_user_identity(&identity).unwrap();
        let loaded = d
            .get_projected_user_identity("user:google:subject-123")
            .unwrap()
            .expect("stored projected user identity");
        assert_eq!(loaded, identity);
    }

    #[test]
    fn find_projected_user_identity_for_local_user_returns_unique_match() {
        let d = make_domain();
        let identity = ProjectedUserIdentityRecord {
            principal_id: "user:google:subject-123".into(),
            local_user_id: "root-user:mac-jane".into(),
            home_hotel: "mac-jane".into(),
            display_name: "Jared Likes".into(),
            preferred_name: Some("Jared".into()),
            primary_email: Some("jared@example.com".into()),
            linked_identities: Vec::new(),
            updated_at: 789,
        };
        d.upsert_projected_user_identity(&identity).unwrap();
        let loaded = d
            .find_projected_user_identity_for_local_user("root-user:mac-jane")
            .unwrap()
            .expect("stored projected user identity");
        assert_eq!(loaded.principal_id, "user:google:subject-123");
    }

    #[test]
    fn user_task_round_trip_create_and_get() {
        let d = make_domain();
        let task_id = "task-001";
        let task = serde_json::json!({
            "task_id": task_id,
            "session_id": "session-abc",
            "agent_id": "agent-01",
            "chat_id": "chat-42",
            "goal": "refactor auth module",
            "steps": [],
            "status": "planning",
            "approved_risk_ceiling": "moderate",
            "planning_model_tier": 0,
            "quiet": false,
            "created_at": 1_000_000u64,
            "updated_at": 1_000_000u64,
            "completed_at": null,
            "next_step_idx": 0,
            "approval_note": null,
        });
        d.upsert_user_task(task.clone(), task_id).unwrap();
        let fetched = d.get_user_task(task_id).unwrap().expect("stored task");
        assert_eq!(fetched["goal"].as_str(), Some("refactor auth module"));
        assert_eq!(fetched["status"].as_str(), Some("planning"));
    }

    #[test]
    fn user_task_update_preserves_existing_fields() {
        let d = make_domain();
        let task_id = "task-002";
        let task = serde_json::json!({
            "task_id": task_id,
            "session_id": "session-abc",
            "agent_id": "agent-01",
            "chat_id": "chat-42",
            "goal": "write tests",
            "steps": [],
            "status": "planning",
            "approved_risk_ceiling": "safe",
            "planning_model_tier": 0,
            "quiet": true,
            "created_at": 1_000_000u64,
            "updated_at": 1_000_000u64,
            "completed_at": null,
            "next_step_idx": 0,
            "approval_note": null,
        });
        d.upsert_user_task(task, task_id).unwrap();

        let mut data = d.get_user_task(task_id).unwrap().unwrap();
        data["status"] = serde_json::Value::String("running".into());
        data["updated_at"] = serde_json::json!(2_000_000u64);
        d.upsert_user_task(data, task_id).unwrap();

        let updated = d.get_user_task(task_id).unwrap().unwrap();
        assert_eq!(updated["status"].as_str(), Some("running"));
        assert_eq!(updated["goal"].as_str(), Some("write tests")); // preserved
    }

    #[test]
    fn list_user_tasks_filters_by_session() {
        let d = make_domain();
        let make_task = |id: &str, session: &str| {
            serde_json::json!({
                "task_id": id,
                "session_id": session,
                "agent_id": "agent-01",
                "chat_id": "chat-1",
                "goal": "do something",
                "steps": [],
                "status": "planning",
                "approved_risk_ceiling": "safe",
                "planning_model_tier": 0,
                "quiet": false,
                "created_at": 1u64,
                "updated_at": 1u64,
                "completed_at": null,
                "next_step_idx": 0,
                "approval_note": null,
            })
        };
        d.upsert_user_task(make_task("t1", "session-A"), "t1")
            .unwrap();
        d.upsert_user_task(make_task("t2", "session-B"), "t2")
            .unwrap();
        d.upsert_user_task(make_task("t3", "session-A"), "t3")
            .unwrap();

        let all = d.list_user_tasks(None, None).unwrap();
        assert_eq!(all.len(), 3);

        let session_a = d.list_user_tasks(Some("session-A"), None).unwrap();
        assert_eq!(session_a.len(), 2);

        let session_b = d.list_user_tasks(Some("session-B"), None).unwrap();
        assert_eq!(session_b.len(), 1);
    }

    #[test]
    fn get_user_task_returns_none_when_absent() {
        let d = make_domain();
        assert!(d.get_user_task("nonexistent").unwrap().is_none());
    }

    fn work_item(id: &str, pattern: &str, guest: &str, status: &str) -> HealWorkItemRecord {
        HealWorkItemRecord {
            work_item_id: id.to_string(),
            pattern_tag: pattern.to_string(),
            guest_id: guest.to_string(),
            count: 5,
            window_secs: 1800,
            evidence: vec!["connection refused".to_string()],
            status: status.to_string(),
            filed_by: "heal-dispatcher".to_string(),
            audit_id: Some(format!("heal_filing:{id}")),
            created_at: 1_000,
            last_seen: 1_000,
        }
    }

    #[test]
    fn heal_work_item_round_trip_and_open_lookup() {
        let d = make_domain();
        assert!(d.get_heal_work_item("wi-1").expect("get").is_none());
        assert!(d
            .find_open_heal_work_item("connection_refused", "membrane-01")
            .expect("find")
            .is_none());

        d.upsert_heal_work_item(&work_item(
            "wi-1",
            "connection_refused",
            "membrane-01",
            HEAL_WORK_ITEM_STATUS_OPEN,
        ))
        .expect("upsert");
        // Closed items and different keys never match the open lookup.
        d.upsert_heal_work_item(&work_item(
            "wi-2",
            "connection_refused",
            "membrane-01",
            HEAL_WORK_ITEM_STATUS_CLOSED,
        ))
        .expect("upsert closed");
        d.upsert_heal_work_item(&work_item(
            "wi-3",
            "panic",
            "membrane-01",
            HEAL_WORK_ITEM_STATUS_OPEN,
        ))
        .expect("upsert other pattern");

        let found = d
            .find_open_heal_work_item("connection_refused", "membrane-01")
            .expect("find")
            .expect("open item");
        assert_eq!(found.work_item_id, "wi-1");
        assert!(d
            .find_open_heal_work_item("connection_refused", "other-guest")
            .expect("find")
            .is_none());
        assert_eq!(d.list_heal_work_items().expect("list").len(), 3);
    }

    #[test]
    fn close_heal_work_item_flips_status_and_frees_dedup_slot() {
        let d = make_domain();
        d.upsert_heal_work_item(&work_item(
            "wi-1",
            "oom",
            "philote-01",
            HEAL_WORK_ITEM_STATUS_OPEN,
        ))
        .expect("upsert");

        assert!(d.close_heal_work_item("wi-1", 2_000).expect("close"));
        let item = d.get_heal_work_item("wi-1").expect("get").expect("present");
        assert_eq!(item.status, HEAL_WORK_ITEM_STATUS_CLOSED);
        assert_eq!(item.last_seen, 2_000);
        // Dedup slot is free again after closure.
        assert!(d
            .find_open_heal_work_item("oom", "philote-01")
            .expect("find")
            .is_none());
        // Closing a missing item reports false.
        assert!(!d.close_heal_work_item("missing", 2_001).expect("close"));
    }
}
