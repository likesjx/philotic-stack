//! Architect Charter — Autopoiesis Slice A4 (`aria-architect-charter`).
//!
//! Config-shaped, per-hotel/per-agent registration of a daily "architect"
//! steward role: a role manifest (durable charter text) plus a daily cron
//! job that parks a task on that role's philote guest, whose prompt
//! instructs it to sweep the heal queue / `DEFECTS.md` / seam staleness /
//! autonomy trust ledger, author or update scored intel-graph proposals, and
//! compose a morning dev-brief for the operator.
//!
//! # Not implemented here
//!
//! The steward is a MODEL. This module registers the role and the trigger —
//! it does not sweep anything in Rust. [`DEFAULT_CHARTER_MANIFEST`] is the
//! prompt that tells the model which tools/CLI verbs to call
//! (`memory.delta_digest`, `bash.exec` against `phil heal list` /
//! `phil autonomy status` / `phil graph status` / `phil graph seams`, and
//! `docs/DEFECTS.md`) and what shape the reply should take. See
//! `docs/architecture/AUTOPOIESIS_PROPOSAL.md`'s A4 row and
//! `docs/architecture/MEMORY_TRANSPARENCY_PROPOSAL.md`'s M3 row (this is the
//! "push half" M3 named as deferred to whoever built A4).
//!
//! # Delivery
//!
//! Reuses the existing cron→role delivery path (`CronTicker::fire`,
//! PR #80 heritage) unmodified: the daily fire is a normal
//! `role:{agent_id}:{role_name}` `TaskInvoke`, parked/materialized like any
//! other role delivery, and the steward's `send_reply` routes back through
//! the same `reply_guest_id`-disambiguated path DEF-051 (PR #292/#297/#300/
//! #301/#303) fixed for every role guest — not touched by this slice.
//! `silent_ok: false` so the brief is never suppressed by the `[SILENT]`
//! cron-reply convention.
//!
//! **Reply destination, traced precisely (not assumed):** `philote::runtime`
//! (`handle_user_message`) defaults an inbound task's `final_reply_role` to
//! `"membrane"` and `final_reply_to` to the local node whenever the task
//! omits them (`DEFAULT_REPLY_ROLE = "membrane"`) — so a cron-fired task
//! with no explicit return-route already lands at the local membrane guest
//! by default, no extra wiring needed there. But `chat_id` is carried
//! completely independently (`task.chat_id.clone().unwrap_or_default()`,
//! echoed onto the outbound `FinalReplyPayload`) — it is NOT derived from
//! `session_id`, `source`, or anything else. Without it the reply still
//! routes to membrane, just with an empty chat destination. So the one
//! thing this module must supply is `chat_id` (plus `source` for honest
//! transport labeling): [`ensure_scheduled`] bakes both into the registered
//! job's `payload` from [`ENV_CHAT_ID`]/[`ENV_CHAT_SOURCE`] — the same shape
//! a manually `cron.register`-ed operator check-in (Beacon's Chronos)
//! already uses. **If [`ENV_CHAT_ID`] is unset, the charter still registers
//! and fires — the sweep and brief still compose — but the reply has no
//! chat to land in.** [`ensure_scheduled`] warns loudly at registration time
//! rather than papering over it.
//!
//! # Reconciliation — deliberately weaker than M4/PR #298's toolset reconciler
//!
//! [`ensure_role_incarnation`] is idempotent **create-if-absent only**: once
//! a hotel's charter role incarnation exists, this module never touches its
//! `role_manifest` again. This is a considered choice, not an oversight — the
//! Autopoiesis roadmap's A8b slice (`team.evolve` — charter evolution) has
//! the steward *itself* propose amendments to its own charter, and an
//! operator may hand-tune the manifest via `role.create_or_update` in the
//! meantime. A PR #298-style bidirectional reconciler (propagate new seed
//! text while preserving runtime edits) would silently stomp exactly the
//! edits A8b exists to make safe. **Reality gap, named plainly:** improving
//! [`DEFAULT_CHARTER_MANIFEST`] in a later release does not reach an
//! already-materialized charter agent without an explicit
//! `role.create_or_update` call or a future dedicated reconciler — unlike
//! the toolset-profile seed, which does propagate (see the `architect`
//! toolset profile entry in `main.rs`'s `seed_toolset_profiles`).
//!
//! # Fire-time re-check — job-id-scoped, not target-role-scoped like M4
//!
//! `CronJobSync` replicates a hotel's `CronJob` *definitions* to every
//! mesh-connected peer unconditionally. M4's `memory.hygiene` sweep re-checks
//! the firing hotel's own opt-in (`enabled_locally`) and nothing else,
//! because its action (sweep *this* hotel's own vaults) is symmetric — any
//! hotel firing it acts on itself regardless of which job row triggered it.
//! This charter's action is NOT symmetric: it delivers to a specific
//! `role:{agent_id}:{role_name}` that may only exist, or only mean something,
//! on the hotel that registered it. So `CronTicker` gates on both
//! `enabled_locally` AND the fired job's id exactly matching this hotel's own
//! deterministic [`cron_job_id`] — a replicated peer's charter job (same
//! `architect-charter:` prefix, different hotel suffix) is always skipped,
//! never just "skipped when this hotel opted out." See
//! `CronTicker::with_architect_charter` / the `JOB_ID_PREFIX` check in
//! `CronTicker::fire`.

use ansible_mesh_core::cron::{CronJob, CronJobSource, CronSessionTarget, next_fire_after};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::graph::RoleIncarnationRecord;
use tracing::{debug, info, warn};

/// Env var gating whether this hotel registers the daily architect-charter
/// role + cron at all. Operator opt-in per hotel, mirroring
/// `PHILOTIC_MEMORY_HYGIENE_ENABLED` — disabled unless explicitly set to a
/// truthy value.
pub const ENV_ENABLED: &str = "PHILOTIC_ARCHITECT_CHARTER_ENABLED";
/// The agent this hotel's charter steward runs as. Required when
/// [`ENV_ENABLED`] is set — there is no default agent (do not hardcode any
/// specific persona; this is config-shaped per operator's fleet).
pub const ENV_AGENT: &str = "PHILOTIC_ARCHITECT_CHARTER_AGENT";
/// Role name for the steward incarnation. Defaults to `"architect"` — the
/// same domain slug already recognized by the LifeGraph semantic-retrieval
/// role vocabulary (`librarian | communications | companion | architect |
/// chief_of_staff | musician | human`, see `SEMANTIC_RETRIEVAL.md`) and the
/// same toolset profile name already seeded in `main.rs`.
pub const ENV_ROLE: &str = "PHILOTIC_ARCHITECT_CHARTER_ROLE";
pub const DEFAULT_ROLE: &str = "architect";
/// Toolset profile granted to the steward incarnation. Defaults to the
/// existing `"architect"` profile (`main.rs::seed_toolset_profiles`) —
/// extended by this slice with an explicit `memory.delta_digest` grant so
/// the charter's mandatory digest call actually has a tool to call.
pub const ENV_TOOLSET_PROFILE: &str = "PHILOTIC_ARCHITECT_CHARTER_TOOLSET_PROFILE";
pub const DEFAULT_TOOLSET_PROFILE: &str = "architect";
/// Cron schedule override (7-field `cron` crate syntax).
pub const ENV_SCHEDULE: &str = "PHILOTIC_ARCHITECT_CHARTER_SCHEDULE";
/// Default: daily at 13:00 UTC (a US-Central/Eastern morning) — operators in
/// other timezones should override via [`ENV_SCHEDULE`].
pub const DEFAULT_SCHEDULE: &str = "0 0 13 * * * *";
/// The operator chat the daily brief is delivered to — see module docs
/// ("Delivery") for exactly why this is the one field this module cannot
/// default: `chat_id` is carried independently of `session_id`/`source` all
/// the way to the outbound `FinalReplyPayload`, so without it the reply
/// still routes to the local `membrane` guest but with nowhere to send.
/// Unset by default (no default operator chat) — [`ensure_scheduled`] warns
/// loudly rather than guessing one.
pub const ENV_CHAT_ID: &str = "PHILOTIC_ARCHITECT_CHARTER_CHAT_ID";
/// Transport label paired with [`ENV_CHAT_ID`]. Telegram is the only inbound
/// membrane transport wired in this repo today.
pub const ENV_CHAT_SOURCE: &str = "PHILOTIC_ARCHITECT_CHARTER_SOURCE";
pub const DEFAULT_CHAT_SOURCE: &str = "telegram";

/// Reserved prefix for the deterministic per-hotel cron job id (see
/// [`cron_job_id`]). `CronTicker::fire` uses this prefix to recognize a
/// replicated peer's charter job and refuse to act on it (see module docs,
/// "Fire-time re-check").
pub const JOB_ID_PREFIX: &str = "architect-charter:";

/// Deterministic id for the auto-registered per-hotel cron job — stable
/// across restarts so [`ensure_scheduled`] is idempotent, and unique per
/// hotel so a mesh-replicated peer job never collides with (or is mistaken
/// for) this hotel's own.
pub fn cron_job_id(hotel_name: &str) -> String {
    format!("{JOB_ID_PREFIX}{hotel_name}")
}

/// `role:{agent_id}:{role_name}` — matches
/// [`RoleIncarnationRecord::routing_role`] and
/// `CronTicker::resolve_target_role_record`'s expected shape.
pub fn target_role(agent_id: &str, role_name: &str) -> String {
    format!("role:{agent_id}:{role_name}")
}

/// True when the operator has opted this hotel into the daily charter.
pub fn charter_enabled(env: &impl Fn(&str) -> Option<String>) -> bool {
    match env(ENV_ENABLED) {
        None => false,
        Some(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        }
    }
}

/// The configured steward agent id, if any. `None` when unset or blank —
/// callers must not register anything without an explicit agent (no
/// hardcoded default persona).
pub fn charter_agent_id(env: &impl Fn(&str) -> Option<String>) -> Option<String> {
    env(ENV_AGENT)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn charter_role_name(env: &impl Fn(&str) -> Option<String>) -> String {
    env(ENV_ROLE)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_ROLE.to_string())
}

pub fn charter_toolset_profile(env: &impl Fn(&str) -> Option<String>) -> String {
    env(ENV_TOOLSET_PROFILE)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_TOOLSET_PROFILE.to_string())
}

fn charter_schedule(env: &impl Fn(&str) -> Option<String>) -> String {
    env(ENV_SCHEDULE)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_SCHEDULE.to_string())
}

/// The configured operator chat id, if any. `None` when unset or blank — no
/// default operator chat is assumed (see [`ENV_CHAT_ID`] docs).
pub fn charter_chat_id(env: &impl Fn(&str) -> Option<String>) -> Option<String> {
    env(ENV_CHAT_ID)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn charter_chat_source(env: &impl Fn(&str) -> Option<String>) -> String {
    env(ENV_CHAT_SOURCE)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CHAT_SOURCE.to_string())
}

/// The durable governance document projected into the steward's Identity
/// context layer (`RoleIncarnationRecord::role_manifest`) — the charter
/// itself. Generic across agents on purpose: per-agent tone/identity comes
/// from the agent's own Identity layer, not this manifest.
///
/// Named seams inside the text itself, honestly, rather than silently
/// assumed working:
/// - `phil graph status` / `phil graph seams` require the intel-graph server
///   (`just intel-graph-start`) to be running on this hotel; the manifest
///   instructs the model to say so plainly if it is not, rather than
///   omitting the section.
/// - Proposal authoring goes through the intel-graph REST API directly
///   (`POST /api/proposals/:id/manage`, `POST /api/decide`) via `bash.exec`
///   + `curl` — there is no `phil graph propose` CLI verb and no philote
///   tool that talks to the intel-graph server, so this is the same
///   transport A3's heal-pattern-filing lane uses, just driven by the model
///   instead of Rust.
/// - The autonomy trust-ledger's *pending outcome-stamp records* are a named
///   seam: a parallel slice (`codex/autonomy-outcome-stamping`) is adding a
///   dedicated pending-records listing. Until it lands, `phil autonomy
///   status` is the only reachable surface — the manifest asks for that and
///   nothing more.
pub const DEFAULT_CHARTER_MANIFEST: &str = r#"# Architect Charter — Daily Dev-Brief Steward (Autopoiesis Slice A4)

You are running as the daily architect-charter steward. Every time this
charter fires, do the following, in order:

1. Call the `memory.delta_digest` tool FIRST (default 24h window). This is
   the memory-delta digest built specifically for this brief (Memory
   Transparency Slice M3) — never hand-derive a memory-delta summary from
   raw recall; call the tool.
2. Sweep for engineering signal with `bash.exec`:
   - `phil heal list` — open self-heal work items filed by the heal circuit.
   - `cat docs/DEFECTS.md` — open defects and technical debt, especially
     anything newly filed or aging without movement.
   - `phil graph status` and `phil graph seams` — seam staleness and the
     proposal pipeline, IF the intel-graph server is reachable on this
     hotel. If it is not running, say so plainly in the brief rather than
     silently omitting the section.
   - `phil autonomy status` — the per-lane AutonomyGrant trust ledger:
     actions/day vs budget, consecutive failures, confirmed-good streak, and
     promotion eligibility. Surface anything pending operator confirmation
     or close to a posture change. (A dedicated pending-outcome-stamp
     listing is landing in a parallel slice; until then this CLI is the only
     reachable surface for that data — note it as a seam if the ledger looks
     incomplete, don't fabricate detail it doesn't expose.)
3. When the sweep surfaces a recurring gap, a stale seam worth reviving, or
   a pattern worth naming that is NOT already tracked, author or update a
   scored proposal in the intel graph via `bash.exec` + `curl`:
   `POST http://127.0.0.1:8900/api/proposals/<slug>/manage` to author/update,
   and `POST http://127.0.0.1:8900/api/decide` to record the authoring
   action with evidence (same reviewable-breadcrumb pattern the
   heal-pattern-filing lane uses). Only propose and update your own
   filings — never merge, close, or reprioritize someone else's in-flight
   work.
4. Compose ONE morning dev-brief as your reply: what the memory digest
   showed, what's open in the heal queue and DEFECTS ledger, seam/proposal
   pipeline health, autonomy trust-ledger highlights, and anything you filed
   or updated this run. If a data source was unreachable, say so plainly —
   never fabricate a clean sweep.
5. Keep the brief skimmable: short headers or bullets per section, not
   prose paragraphs. This is a standing daily brief, not a novel.

You author and update proposals. You do not implement code, merge PRs, or
execute slices — that is a later, separate autopoiesis lane, not yours.
"#;

/// The short trigger message sent as the daily `TaskInvoke` payload's
/// `content` (promoted from `payload.message` by
/// `CronTicker::build_cron_task_json`). The durable instructions live in
/// [`DEFAULT_CHARTER_MANIFEST`] (projected into Identity context whenever
/// this role is active) — this is just the fire signal.
const DAILY_BRIEF_TRIGGER_MESSAGE: &str =
    "Run your daily architect-charter sweep now and deliver the morning dev-brief.";

/// Build the daily fire's `TaskInvoke` payload JSON. Bakes in `chat_id`
/// (+ `source`) from [`ENV_CHAT_ID`]/[`ENV_CHAT_SOURCE`] when configured —
/// the same shape a manually `cron.register`-ed operator check-in already
/// uses (see module docs, "Delivery", for exactly why `chat_id` is the one
/// field that cannot be defaulted). Warns loudly, once, at registration
/// time when no chat destination is configured — the charter still fires
/// and the brief still composes, it just has nowhere to land.
fn daily_brief_payload(hotel_name: &str, env: &impl Fn(&str) -> Option<String>) -> String {
    let mut payload = serde_json::json!({ "message": DAILY_BRIEF_TRIGGER_MESSAGE });
    match charter_chat_id(env) {
        Some(chat_id) => {
            payload["chat_id"] = serde_json::Value::String(chat_id);
            payload["source"] = serde_json::Value::String(charter_chat_source(env));
        }
        None => {
            warn!(
                hotel = %hotel_name,
                "architect-charter: PHILOTIC_ARCHITECT_CHARTER_CHAT_ID is not set — the daily \
                 brief will compose but has no delivery destination (chat_id is carried \
                 independently of session routing all the way to the outbound reply; \
                 without it the reply still resolves to the local membrane guest but with an \
                 empty chat_id)"
            );
        }
    }
    payload.to_string()
}

/// Idempotent, create-if-absent registration of the steward role
/// incarnation. Never overwrites an existing record — see module docs
/// ("Reconciliation").
pub fn ensure_role_incarnation(
    graph: &GraphDomain,
    agent_id: &str,
    role_name: &str,
    toolset_profile: &str,
) -> anyhow::Result<()> {
    if graph.get_role_incarnation(agent_id, role_name)?.is_some() {
        debug!(
            agent_id,
            role_name, "architect-charter: role incarnation already present — not overwriting"
        );
        return Ok(());
    }

    let record = RoleIncarnationRecord {
        agent_id: agent_id.to_string(),
        role_name: role_name.to_string(),
        guest_id: format!("{agent_id}:{role_name}"),
        toolset_profile: toolset_profile.to_string(),
        role_manifest: Some(DEFAULT_CHARTER_MANIFEST.to_string()),
        ..Default::default()
    };
    graph.upsert_role_incarnation(&record)?;
    info!(
        agent_id,
        role_name, "architect-charter: seeded steward role incarnation"
    );
    Ok(())
}

/// Ensure the daily architect-charter role + cron job are registered when
/// the operator has opted this hotel in via [`ENV_ENABLED`] + [`ENV_AGENT`].
/// No-op (with a warning) if [`ENV_ENABLED`] is set but [`ENV_AGENT`] is
/// missing. Idempotent: never re-creates an existing role incarnation or
/// clobbers an operator-edited cron schedule.
pub fn ensure_scheduled(
    graph: &GraphDomain,
    hotel_name: &str,
    now_ms: u64,
    env: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    if !charter_enabled(&env) {
        debug!(
            hotel = %hotel_name,
            "architect-charter: not enabled for this hotel (PHILOTIC_ARCHITECT_CHARTER_ENABLED unset)"
        );
        return Ok(());
    }
    let Some(agent_id) = charter_agent_id(&env) else {
        warn!(
            hotel = %hotel_name,
            "architect-charter: PHILOTIC_ARCHITECT_CHARTER_ENABLED is set but \
             PHILOTIC_ARCHITECT_CHARTER_AGENT is missing — charter not registered"
        );
        return Ok(());
    };
    let role_name = charter_role_name(&env);
    let toolset_profile = charter_toolset_profile(&env);

    ensure_role_incarnation(graph, &agent_id, &role_name, &toolset_profile)?;

    let job_id = cron_job_id(hotel_name);
    if graph.get_cron_job(&job_id)?.is_some() {
        debug!(hotel = %hotel_name, "architect-charter: cron job already registered");
        return Ok(());
    }

    let schedule = charter_schedule(&env);
    let next_fire_at = next_fire_after(&schedule, now_ms)?;
    let payload = daily_brief_payload(hotel_name, &env);

    let job = CronJob {
        id: job_id.clone(),
        schedule,
        target_role: target_role(&agent_id, &role_name),
        target_node_id: None,
        payload,
        guaranteed: false,
        enabled: true,
        last_fired_epoch: None,
        next_fire_at,
        created_at: now_ms,
        created_by: CronJobSource::Operator,
        // The brief must always be delivered — never suppressed by the
        // `[SILENT]` cron-reply convention.
        silent_ok: false,
        session_target: CronSessionTarget::Isolated,
    };
    graph.upsert_cron_job(&job)?;
    info!(
        hotel = %hotel_name,
        job_id = %job_id,
        agent_id,
        role_name,
        next_fire_at,
        "architect-charter: daily dev-brief cron job registered"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use std::sync::Arc;

    fn open_domain() -> GraphDomain {
        let storage = SqliteGraphStorage::open(":memory:").expect("open");
        GraphDomain::new(Arc::new(storage.adapter()))
    }

    const T0_MS: u64 = 1_750_000_000_000;

    #[test]
    fn charter_enabled_requires_explicit_truthy_value() {
        assert!(!charter_enabled(&|_| None));
        assert!(!charter_enabled(
            &|k| (k == ENV_ENABLED).then(|| "0".to_string())
        ));
        assert!(charter_enabled(
            &|k| (k == ENV_ENABLED).then(|| "1".to_string())
        ));
        assert!(charter_enabled(
            &|k| (k == ENV_ENABLED).then(|| "true".to_string())
        ));
        assert!(charter_enabled(
            &|k| (k == ENV_ENABLED).then(|| "YES".to_string())
        ));
    }

    #[test]
    fn charter_agent_id_rejects_blank() {
        assert_eq!(charter_agent_id(&|_| None), None);
        assert_eq!(
            charter_agent_id(&|k| (k == ENV_AGENT).then(|| "   ".to_string())),
            None
        );
        assert_eq!(
            charter_agent_id(&|k| (k == ENV_AGENT).then(|| "aria".to_string())),
            Some("aria".to_string())
        );
    }

    #[test]
    fn role_and_toolset_profile_default_to_architect() {
        assert_eq!(charter_role_name(&|_| None), "architect");
        assert_eq!(charter_toolset_profile(&|_| None), "architect");
    }

    #[test]
    fn cron_job_id_and_target_role_are_deterministic_and_hotel_scoped() {
        assert_eq!(cron_job_id("mac-jane"), "architect-charter:mac-jane");
        assert_eq!(cron_job_id("mbp-jane"), "architect-charter:mbp-jane");
        assert_ne!(cron_job_id("mac-jane"), cron_job_id("mbp-jane"));
        assert_eq!(target_role("aria", "architect"), "role:aria:architect");
    }

    #[test]
    fn charter_manifest_names_the_mandatory_digest_call() {
        assert!(DEFAULT_CHARTER_MANIFEST.contains("memory.delta_digest"));
        assert!(DEFAULT_CHARTER_MANIFEST.contains("phil heal list"));
        assert!(DEFAULT_CHARTER_MANIFEST.contains("phil autonomy status"));
        assert!(DEFAULT_CHARTER_MANIFEST.contains("DEFECTS.md"));
    }

    // ── ensure_role_incarnation ─────────────────────────────────────────

    #[test]
    fn ensure_role_incarnation_creates_then_never_overwrites() {
        let graph = open_domain();
        ensure_role_incarnation(&graph, "aria", "architect", "architect").expect("create");
        let created = graph
            .get_role_incarnation("aria", "architect")
            .expect("lookup")
            .expect("exists");
        assert_eq!(created.guest_id, "aria:architect");
        assert_eq!(created.toolset_profile, "architect");
        assert_eq!(
            created.role_manifest.as_deref(),
            Some(DEFAULT_CHARTER_MANIFEST)
        );

        // Simulate an operator/self-proposed live edit to the manifest
        // (A8b: charters may amend their own text).
        let mut edited = created.clone();
        edited.role_manifest = Some("operator-edited charter text".to_string());
        graph.upsert_role_incarnation(&edited).expect("edit");

        // Re-running ensure_role_incarnation (e.g. on hotel restart) must
        // not clobber the live edit.
        ensure_role_incarnation(&graph, "aria", "architect", "architect").expect("idempotent");
        let after = graph
            .get_role_incarnation("aria", "architect")
            .expect("lookup")
            .expect("still exists");
        assert_eq!(
            after.role_manifest.as_deref(),
            Some("operator-edited charter text"),
            "live manifest edit must survive a re-run of ensure_role_incarnation"
        );
    }

    // ── ensure_scheduled ─────────────────────────────────────────────────

    #[test]
    fn ensure_scheduled_noop_when_not_enabled() {
        let graph = open_domain();
        ensure_scheduled(&graph, "test-hotel", T0_MS, |_| None).expect("ok");
        assert!(
            graph
                .get_cron_job(&cron_job_id("test-hotel"))
                .expect("lookup")
                .is_none()
        );
        assert!(
            graph
                .list_role_incarnations("aria")
                .expect("lookup")
                .is_empty()
        );
    }

    #[test]
    fn ensure_scheduled_noop_when_enabled_but_agent_missing() {
        let graph = open_domain();
        let env = |k: &str| (k == ENV_ENABLED).then(|| "1".to_string());
        ensure_scheduled(&graph, "test-hotel", T0_MS, env).expect("ok");
        assert!(
            graph
                .get_cron_job(&cron_job_id("test-hotel"))
                .expect("lookup")
                .is_none()
        );
    }

    #[test]
    fn ensure_scheduled_registers_role_and_daily_job_once() {
        let graph = open_domain();
        let env = |k: &str| match k {
            ENV_ENABLED => Some("1".to_string()),
            ENV_AGENT => Some("aria".to_string()),
            _ => None,
        };
        ensure_scheduled(&graph, "test-hotel", T0_MS, env).expect("ok");

        let role = graph
            .get_role_incarnation("aria", "architect")
            .expect("lookup")
            .expect("role registered");
        assert_eq!(role.toolset_profile, "architect");

        let job = graph
            .get_cron_job(&cron_job_id("test-hotel"))
            .expect("lookup")
            .expect("job registered");
        assert_eq!(job.target_role, "role:aria:architect");
        assert!(job.enabled);
        assert!(!job.silent_ok);
        assert_eq!(job.session_target, CronSessionTarget::Isolated);
        assert!(job.next_fire_at > T0_MS);

        // Idempotent + preserves an operator-edited schedule across restarts,
        // mirroring memory_hygiene::ensure_scheduled's contract.
        let mut edited = job.clone();
        edited.schedule = "0 30 14 * * * *".to_string();
        graph.upsert_cron_job(&edited).expect("upsert edited");
        ensure_scheduled(&graph, "test-hotel", T0_MS + 1000, env).expect("ok");
        let after = graph
            .get_cron_job(&cron_job_id("test-hotel"))
            .expect("lookup")
            .expect("still present");
        assert_eq!(after.schedule, "0 30 14 * * * *", "operator edit preserved");
    }

    #[test]
    fn ensure_scheduled_honors_role_toolset_and_schedule_overrides() {
        let graph = open_domain();
        let env = |k: &str| match k {
            ENV_ENABLED => Some("1".to_string()),
            ENV_AGENT => Some("beacon".to_string()),
            ENV_ROLE => Some("chief_of_staff".to_string()),
            ENV_TOOLSET_PROFILE => Some("admin".to_string()),
            ENV_SCHEDULE => Some("0 0 12 * * * *".to_string()),
            _ => None,
        };
        ensure_scheduled(&graph, "vps-jane", T0_MS, env).expect("ok");

        let role = graph
            .get_role_incarnation("beacon", "chief_of_staff")
            .expect("lookup")
            .expect("role registered");
        assert_eq!(role.toolset_profile, "admin");

        let job = graph
            .get_cron_job(&cron_job_id("vps-jane"))
            .expect("lookup")
            .expect("job registered");
        assert_eq!(job.target_role, "role:beacon:chief_of_staff");
        assert_eq!(job.schedule, "0 0 12 * * * *");
    }

    // ── Delivery: chat_id baking (see module docs, "Delivery") ─────────────

    #[test]
    fn daily_brief_payload_omits_chat_route_when_unset() {
        let payload: serde_json::Value =
            serde_json::from_str(&daily_brief_payload("test-hotel", &|_| None)).expect("json");
        assert!(payload.get("chat_id").is_none());
        assert!(payload.get("source").is_none());
        assert_eq!(payload["message"], DAILY_BRIEF_TRIGGER_MESSAGE);
    }

    #[test]
    fn daily_brief_payload_bakes_in_configured_chat_route() {
        let env = |k: &str| match k {
            ENV_CHAT_ID => Some("7898847424".to_string()),
            _ => None,
        };
        let payload: serde_json::Value =
            serde_json::from_str(&daily_brief_payload("test-hotel", &env)).expect("json");
        assert_eq!(payload["chat_id"], "7898847424");
        assert_eq!(payload["source"], "telegram");
    }

    #[test]
    fn daily_brief_payload_honors_chat_source_override() {
        let env = |k: &str| match k {
            ENV_CHAT_ID => Some("abc123".to_string()),
            ENV_CHAT_SOURCE => Some("discord".to_string()),
            _ => None,
        };
        let payload: serde_json::Value =
            serde_json::from_str(&daily_brief_payload("test-hotel", &env)).expect("json");
        assert_eq!(payload["source"], "discord");
    }

    #[test]
    fn ensure_scheduled_bakes_chat_route_into_the_registered_job() {
        let graph = open_domain();
        let env = |k: &str| match k {
            ENV_ENABLED => Some("1".to_string()),
            ENV_AGENT => Some("aria".to_string()),
            ENV_CHAT_ID => Some("7898847424".to_string()),
            _ => None,
        };
        ensure_scheduled(&graph, "mbp-jane", T0_MS, env).expect("ok");

        let job = graph
            .get_cron_job(&cron_job_id("mbp-jane"))
            .expect("lookup")
            .expect("job registered");
        let payload: serde_json::Value = serde_json::from_str(&job.payload).expect("json");
        assert_eq!(payload["chat_id"], "7898847424");
        assert_eq!(payload["source"], "telegram");
    }
}
