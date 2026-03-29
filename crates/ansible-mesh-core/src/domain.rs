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

use crate::cron::CronJob;
use crate::graph::{
    AbstractSkillRecord, AbstractToolRecord, GraphNode, RoleIncarnationRecord, RuleRecord,
    ToolsetProfileRecord, WorkflowSkillRecord,
};
use crate::storage::{
    AgentIdentityRecord, GraphAdapter, GraphRunnerInstanceRecord, GuestRecord, HotelRecord,
    SecretRecord, SessionEventRecord, SessionParticipantRecord, SessionRecord, SessionTurnRecord,
    VaultRegistryEntry, CONFIG_GRAPH_RUNNER_REGISTRY, CONFIG_MUNINN_ENDPOINT,
    CONFIG_VAULT_REGISTRY,
};
use crate::NodeCapabilities;
use anyhow::{Context, Result};
use std::sync::Arc;

// ── Kind constants ────────────────────────────────────────────────────────────
//
// These are the shared data vocabulary for all graph stores in the system.
// When a new entity type is added, add its kind constant here first.

// Slice 1
pub const NODE_KIND_HOTEL: &str = "hotel";
pub const NODE_KIND_ABSTRACT_TOOL: &str = "abstract_tool";
pub const NODE_KIND_RULE: &str = "rule";

// Slice 2
pub const NODE_KIND_GUEST: &str = "guest";
pub const NODE_KIND_AGENT_IDENTITY: &str = "agent_identity";
pub const NODE_KIND_SESSION: &str = "session";
pub const NODE_KIND_SESSION_PARTICIPANT: &str = "session_participant";
pub const NODE_KIND_SESSION_TURN: &str = "session_turn";
pub const NODE_KIND_SESSION_EVENT: &str = "session_event";
pub const NODE_KIND_ROLE_INCARNATION: &str = "role_incarnation";
pub const NODE_KIND_SECRET: &str = "secret";
pub const NODE_KIND_ABSTRACT_SKILL: &str = "abstract_skill";
pub const NODE_KIND_WORKFLOW_SKILL: &str = "workflow_skill";
pub const NODE_KIND_TOOLSET_PROFILE: &str = "toolset_profile";
pub const NODE_KIND_NODE_CAPABILITIES: &str = "node_capabilities";
pub const NODE_KIND_CONFIG: &str = "config";
pub const NODE_KIND_APARTMENT: &str = "apartment";
pub const NODE_KIND_CRON_JOB: &str = "cron_job";

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

    fn rule_key(rule_id: &str) -> String {
        format!("{}:{}", NODE_KIND_RULE, rule_id)
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
                out.push(
                    serde_json::from_value(node.data).context(
                        "GraphDomain::list_session_turns: deserialize SessionTurnRecord",
                    )?,
                );
            }
        }
        if limit > 0 && out.len() > limit {
            out.drain(..out.len() - limit);
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
        self.adapter
            .list_nodes_by_kind(NODE_KIND_ABSTRACT_SKILL)?
            .into_iter()
            .map(|n| {
                serde_json::from_value(n.data)
                    .context("GraphDomain::list_abstract_skills: deserialize AbstractSkillRecord")
            })
            .collect()
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
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{AbstractSkillRecord, RoleIncarnationRecord, ToolsetProfileRecord};
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
            inactive_ttl_seconds: None,
            turn_loop_config: TurnLoopConfig::default(),
        };
        d.upsert_role_incarnation(&r).unwrap();
        let loaded = d.get_role_incarnation("bjork", "coder").unwrap().unwrap();
        assert_eq!(loaded.guest_id, "guest-1");

        assert_eq!(d.list_role_incarnations("bjork").unwrap().len(), 1);
        assert!(d.list_role_incarnations("nobody").unwrap().is_empty());

        let by_guest = d.list_role_incarnations_by_guest_id("guest-1").unwrap();
        assert_eq!(by_guest.len(), 1);
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
}
