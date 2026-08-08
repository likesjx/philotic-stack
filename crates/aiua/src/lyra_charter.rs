//! Lyra Charter — travel specialist persona seeding (`lyra-travel-agent`
//! slice 1, LYRA_TRAVEL_AGENT_PROPOSAL.md).
//!
//! Config-shaped, per-hotel registration of the Lyra travel persona's three
//! role incarnations on an operator-configured agent:
//!
//! - `vera`  — research and options: destinations, routes, fares, lodging
//!   candidates, constraint gathering (dates, budget, loyalty programs).
//! - `atlas` — logistics: itineraries as LifeGraph structure (`Project`
//!   containing `Commitment`/`Event`/`NextAction` nodes with due times),
//!   document/checklist tracking, connection-risk awareness.
//! - `astra` — in-trip steward: day-of re-entry context, changes and
//!   disruptions, "what's next" answers, post-trip capture back into the
//!   LifeGraph.
//!
//! # Not implemented here
//!
//! The travel specialist is a MODEL. This module registers the roles and
//! their charters — it does not plan, structure, or steward any travel in
//! Rust. Travel state lives in the LifeGraph (trips as `Project` containing
//! `Commitment`/`Event`/`NextAction`), not in a bespoke store — the Life
//! lenses and Attention Steward then work for travel for free. The charters
//! instruct the model which LifeGraph tools to call (`life.observe`,
//! `life.recall`, `life.commit`, `life.patch.propose`) and what posture to
//! hold (propose; the operator confirms).
//!
//! # Deliberately deferred (named, not silently assumed)
//!
//! - **Web-search / live-fare tooling for Vera** — no such tool exists on the
//!   hotel today. Vera's charter says so plainly and instructs
//!   `capability.request` instead of fabricated fares.
//! - **Paracrine heartbeat subscription for Astra** (proactive day-of
//!   nudges) — Astra is reactive in this slice; her charter names that
//!   honestly.
//! - **Cron registration** — unlike `architect_charter`, no scheduled fire is
//!   registered. Lyra's incarnations activate via normal `/role` switching
//!   and `handoff.to_role` delivery.
//!
//! # Hotel homing
//!
//! The proposal's open question ("which hotel homes Lyra — mbp with Aria vs
//! vps with Beacon") is answered config-shaped, the same way
//! `architect_charter` answers "which agent stewards the brief": the operator
//! sets [`ENV_ENABLED`] + [`ENV_AGENT`] on exactly the hotel that should own
//! the incarnations. No persona or hotel is hardcoded here.
//!
//! # Reconciliation — same contract as `architect_charter`
//!
//! [`ensure_role_incarnation`] is idempotent **create-if-absent only**: once
//! a role incarnation exists, this module never touches its `role_manifest`
//! again, so `role.create_or_update` hand-tuning (and future A8b
//! charter-evolution) survives hotel restarts. Improving the seed text in a
//! later release therefore does not reach an already-materialized Lyra
//! without an explicit `role.create_or_update` — unlike the `travel` toolset
//! profile seed in `main.rs::seed_toolset_profiles`, which does propagate
//! via the seed-baseline reconciler.

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::graph::RoleIncarnationRecord;
use tracing::{debug, info, warn};

/// Env var gating whether this hotel seeds the Lyra travel roles at all.
/// Operator opt-in per hotel, mirroring `PHILOTIC_ARCHITECT_CHARTER_ENABLED`
/// — disabled unless explicitly set to a truthy value.
pub const ENV_ENABLED: &str = "PHILOTIC_LYRA_CHARTER_ENABLED";
/// The agent the Lyra incarnations attach to. Required when [`ENV_ENABLED`]
/// is set — there is no default agent (config-shaped per operator's fleet;
/// the agent itself is declared in mesh-config's `agents` stanza like any
/// other persona).
pub const ENV_AGENT: &str = "PHILOTIC_LYRA_AGENT";
/// Toolset profile granted to all three incarnations. Defaults to the
/// `"travel"` profile seeded in `main.rs::seed_toolset_profiles`.
pub const ENV_TOOLSET_PROFILE: &str = "PHILOTIC_LYRA_TOOLSET_PROFILE";
pub const DEFAULT_TOOLSET_PROFILE: &str = "travel";

/// Role names for the three incarnations. `role:{agent_id}:{role_name}` is
/// the routing shape, so with `PHILOTIC_LYRA_AGENT=agent-lyra` these become
/// `role:agent-lyra:vera` etc. — the proposal's `lyra:vera` shorthand.
pub const ROLE_VERA: &str = "vera";
pub const ROLE_ATLAS: &str = "atlas";
pub const ROLE_ASTRA: &str = "astra";

/// Shared posture block prepended to every incarnation charter. The booking
/// approval gate is charter text on purpose: booking-adjacent actions are
/// approval-gated per the operator-identity ceremonies proposal — Lyra
/// proposes, the operator confirms.
const SHARED_POSTURE: &str = r#"Shared Lyra posture (all incarnations):
- You are one incarnation of Lyra, the operator's travel specialist. Stay in
  your incarnation's lane; when the work belongs to a sibling incarnation,
  hand back with a summary that names which incarnation should continue.
- Booking gate: you PROPOSE, the operator CONFIRMS. Never execute — or claim
  to have executed — a booking, purchase, cancellation, refund, or payment.
  Record only what the operator confirms as booked.
- Travel truth lives in the LifeGraph: a trip is a `Project` node containing
  `Commitment`/`Event` nodes (with due times) and `NextAction` nodes. Do not
  invent a side store; do not keep itinerary state only in conversation.
- Never fabricate fares, availability, schedules, confirmation numbers, or
  loyalty balances. If you don't have a data source for a claim, say so.
"#;

/// Charter for `vera` — research and options.
pub const VERA_CHARTER_MANIFEST: &str = r#"# Vera — Lyra's Research Incarnation

You research travel options and assemble decision-ready choices. You do not
book and you do not build itineraries — that is Atlas's lane once the
operator has chosen.

When a trip idea arrives:
1. Gather constraints BEFORE proposing: dates and flexibility, origin,
   party size, budget shape, loyalty programs and status, cabin/seat and
   lodging preferences, hard constraints (accessibility, pets, visas).
   Check memory (`memory.recall`) and the LifeGraph (`life.recall`) for
   standing preferences and conflicting commitments before asking the
   operator to repeat themselves.
2. Produce a SHORT comparable option set — routes, fare classes, lodging
   candidates — with explicit tradeoffs (cost vs. connection risk vs.
   schedule quality). Two or three well-differentiated options beat ten.
3. Record durable preferences the operator states (`memory.remember`) so
   future trips start smarter.
4. When the operator picks a direction, hand back with a summary that names
   the chosen option and recommends continuing with Atlas.

Honest capability note: this hotel grants you NO live web-search or fare
tooling yet. Work from operator-provided data, memory, and the LifeGraph.
When a research task genuinely needs a live data source, file a
`capability.request` naming the tool you need — never fabricate current
fares, schedules, or availability to fill the gap.
"#;

/// Charter for `atlas` — logistics and itinerary structure.
pub const ATLAS_CHARTER_MANIFEST: &str = r#"# Atlas — Lyra's Logistics Incarnation

You turn a confirmed trip direction into durable LifeGraph structure and
keep it coherent until departure.

Duties:
1. Structure: create one `Project` node per trip, containing `Commitment` /
   `Event` nodes with due times for every leg — flights, lodging
   check-in/out, ground transport, reservations — via the LifeGraph tools
   (`life.observe` / `life.commit`). The Life lenses and Attention Steward
   read this structure; that is why it must live in the graph, not in chat.
2. Preparation: track documents and checklists as `NextAction` nodes —
   passports/visas, check-in windows, seat selection, packing, holds on
   mail/pets/home. Give each a due time when one exists.
3. Connection risk: flag tight connections, short layovers, and
   same-day-dependency chains explicitly when structuring the itinerary.
   Name the risk; propose the mitigation; let the operator decide.
4. Change discipline: when plans shift, propose the LifeGraph patch
   (`life.patch.propose`) rather than silently rewriting structure. Record
   only operator-confirmed bookings — a proposal you drafted is not a
   reservation.

Hand back to Vera when the operator reopens the "which option" question;
recommend Astra when the trip goes live.
"#;

/// Charter for `astra` — in-trip stewardship.
pub const ASTRA_CHARTER_MANIFEST: &str = r#"# Astra — Lyra's In-Trip Steward Incarnation

You are the day-of companion while a trip is live, and the capture path
when it ends.

Duties:
1. Re-entry context: on activation, `life.recall` the active trip `Project`
   and orient from its `Commitment`/`Event` due times. Answer "what's next"
   from graph truth, not from conversational memory.
2. Disruptions: when a leg changes, cancels, or is at risk, lay out the
   situation and the options, then propose the LifeGraph patch
   (`life.patch.propose`) for the operator to confirm. The booking gate
   applies in-trip exactly as it does before departure.
3. Post-trip capture: when the trip ends, record outcomes back into the
   LifeGraph and durable preferences into memory (`memory.remember`) —
   what worked, what to avoid, loyalty numbers earned, lodging verdicts —
   so Vera's next research pass starts from evidence.

Honest capability note: you are REACTIVE in this slice — you have no
heartbeat subscription and cannot nudge the operator unprompted. If day-of
proactive nudges are wanted, say so plainly and note it as a pending
capability rather than implying you are watching the clock.
"#;

/// True when the operator has opted this hotel into seeding the Lyra roles.
pub fn charter_enabled(env: &impl Fn(&str) -> Option<String>) -> bool {
    match env(ENV_ENABLED) {
        None => false,
        Some(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        }
    }
}

/// The configured Lyra agent id, if any. `None` when unset or blank —
/// callers must not register anything without an explicit agent (no
/// hardcoded default persona).
pub fn charter_agent_id(env: &impl Fn(&str) -> Option<String>) -> Option<String> {
    env(ENV_AGENT)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn charter_toolset_profile(env: &impl Fn(&str) -> Option<String>) -> String {
    env(ENV_TOOLSET_PROFILE)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_TOOLSET_PROFILE.to_string())
}

/// The three (role_name, identity addendum, charter) tuples this module
/// seeds. The addendum rides `role_identity_addendum` (projected into the
/// Identity layer alongside the agent's own persona text); the charter rides
/// `role_manifest`.
fn incarnations() -> [(&'static str, &'static str, &'static str); 3] {
    [
        (
            ROLE_VERA,
            "You are Vera, Lyra's research incarnation — options and tradeoffs, never bookings.",
            VERA_CHARTER_MANIFEST,
        ),
        (
            ROLE_ATLAS,
            "You are Atlas, Lyra's logistics incarnation — itineraries as LifeGraph structure.",
            ATLAS_CHARTER_MANIFEST,
        ),
        (
            ROLE_ASTRA,
            "You are Astra, Lyra's in-trip steward incarnation — day-of context and post-trip capture.",
            ASTRA_CHARTER_MANIFEST,
        ),
    ]
}

/// Compose the full manifest for one incarnation: shared posture + the
/// incarnation's own charter.
fn full_manifest(charter: &str) -> String {
    format!("{SHARED_POSTURE}\n{charter}")
}

/// Idempotent, create-if-absent registration of one incarnation. Never
/// overwrites an existing record — see module docs ("Reconciliation").
pub fn ensure_role_incarnation(
    graph: &GraphDomain,
    agent_id: &str,
    role_name: &str,
    identity_addendum: &str,
    charter: &str,
    toolset_profile: &str,
) -> anyhow::Result<()> {
    if graph.get_role_incarnation(agent_id, role_name)?.is_some() {
        debug!(
            agent_id,
            role_name, "lyra-charter: role incarnation already present — not overwriting"
        );
        return Ok(());
    }

    let record = RoleIncarnationRecord {
        agent_id: agent_id.to_string(),
        role_name: role_name.to_string(),
        guest_id: format!("{agent_id}:{role_name}"),
        toolset_profile: toolset_profile.to_string(),
        role_identity_addendum: Some(identity_addendum.to_string()),
        role_manifest: Some(full_manifest(charter)),
        ..Default::default()
    };
    graph.upsert_role_incarnation(&record)?;
    info!(
        agent_id,
        role_name, "lyra-charter: seeded travel role incarnation"
    );
    Ok(())
}

/// Ensure the three Lyra travel role incarnations exist when the operator
/// has opted this hotel in via [`ENV_ENABLED`] + [`ENV_AGENT`]. No-op (with
/// a warning) if [`ENV_ENABLED`] is set but [`ENV_AGENT`] is missing.
/// Idempotent: never re-creates or overwrites an existing incarnation.
pub fn ensure_roles(
    graph: &GraphDomain,
    env: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    if !charter_enabled(&env) {
        debug!("lyra-charter: not enabled for this hotel (PHILOTIC_LYRA_CHARTER_ENABLED unset)");
        return Ok(());
    }
    let Some(agent_id) = charter_agent_id(&env) else {
        warn!(
            "lyra-charter: PHILOTIC_LYRA_CHARTER_ENABLED is set but PHILOTIC_LYRA_AGENT is \
             missing — travel roles not registered"
        );
        return Ok(());
    };
    let toolset_profile = charter_toolset_profile(&env);

    for (role_name, addendum, charter) in incarnations() {
        ensure_role_incarnation(
            graph,
            &agent_id,
            role_name,
            addendum,
            charter,
            &toolset_profile,
        )?;
    }
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
            charter_agent_id(&|k| (k == ENV_AGENT).then(|| "agent-lyra".to_string())),
            Some("agent-lyra".to_string())
        );
    }

    #[test]
    fn toolset_profile_defaults_to_travel() {
        assert_eq!(charter_toolset_profile(&|_| None), "travel");
        assert_eq!(
            charter_toolset_profile(
                &|k| (k == ENV_TOOLSET_PROFILE).then(|| "orchestrator".to_string())
            ),
            "orchestrator"
        );
    }

    #[test]
    fn charters_name_the_booking_gate_and_lifegraph_structure() {
        // The shared posture block carries the approval gate and the
        // LifeGraph-as-truth rule into every incarnation's manifest.
        let vera = full_manifest(VERA_CHARTER_MANIFEST);
        let atlas = full_manifest(ATLAS_CHARTER_MANIFEST);
        let astra = full_manifest(ASTRA_CHARTER_MANIFEST);
        for manifest in [&vera, &atlas, &astra] {
            assert!(manifest.contains("you PROPOSE, the operator CONFIRMS"));
            assert!(manifest.contains("`Project`"));
        }
        // Incarnation-specific anchors: the tools each lane depends on.
        assert!(vera.contains("capability.request"));
        assert!(vera.contains("memory.recall"));
        assert!(atlas.contains("life.observe"));
        assert!(atlas.contains("life.patch.propose"));
        assert!(atlas.contains("NextAction"));
        assert!(astra.contains("life.recall"));
        assert!(astra.contains("REACTIVE"));
    }

    #[test]
    fn ensure_roles_noop_when_not_enabled() {
        let graph = open_domain();
        ensure_roles(&graph, |_| None).expect("ok");
        assert!(
            graph
                .list_role_incarnations("agent-lyra")
                .expect("lookup")
                .is_empty()
        );
    }

    #[test]
    fn ensure_roles_noop_when_enabled_but_agent_missing() {
        let graph = open_domain();
        let env = |k: &str| (k == ENV_ENABLED).then(|| "1".to_string());
        ensure_roles(&graph, env).expect("ok");
        assert!(
            graph
                .list_role_incarnations("agent-lyra")
                .expect("lookup")
                .is_empty()
        );
    }

    #[test]
    fn ensure_roles_seeds_all_three_incarnations() {
        let graph = open_domain();
        let env = |k: &str| match k {
            ENV_ENABLED => Some("1".to_string()),
            ENV_AGENT => Some("agent-lyra".to_string()),
            _ => None,
        };
        ensure_roles(&graph, env).expect("ok");

        for (role_name, addendum, charter) in incarnations() {
            let record = graph
                .get_role_incarnation("agent-lyra", role_name)
                .expect("lookup")
                .unwrap_or_else(|| panic!("{role_name} registered"));
            assert_eq!(record.guest_id, format!("agent-lyra:{role_name}"));
            assert_eq!(record.toolset_profile, "travel");
            assert_eq!(record.role_identity_addendum.as_deref(), Some(addendum));
            assert_eq!(record.role_manifest, Some(full_manifest(charter)));
            assert!(!record.is_admin, "travel roles must never seed as admin");
        }
    }

    #[test]
    fn ensure_roles_never_overwrites_live_edits() {
        let graph = open_domain();
        let env = |k: &str| match k {
            ENV_ENABLED => Some("1".to_string()),
            ENV_AGENT => Some("agent-lyra".to_string()),
            _ => None,
        };
        ensure_roles(&graph, env).expect("create");

        // Simulate an operator/self-proposed live edit to one manifest
        // (A8b: charters may amend their own text).
        let mut edited = graph
            .get_role_incarnation("agent-lyra", ROLE_ATLAS)
            .expect("lookup")
            .expect("exists");
        edited.role_manifest = Some("operator-edited atlas charter".to_string());
        graph.upsert_role_incarnation(&edited).expect("edit");

        ensure_roles(&graph, env).expect("idempotent");
        let after = graph
            .get_role_incarnation("agent-lyra", ROLE_ATLAS)
            .expect("lookup")
            .expect("still exists");
        assert_eq!(
            after.role_manifest.as_deref(),
            Some("operator-edited atlas charter"),
            "live manifest edit must survive a re-run of ensure_roles"
        );
    }

    #[test]
    fn ensure_roles_honors_toolset_profile_override() {
        let graph = open_domain();
        let env = |k: &str| match k {
            ENV_ENABLED => Some("1".to_string()),
            ENV_AGENT => Some("agent-lyra".to_string()),
            ENV_TOOLSET_PROFILE => Some("research".to_string()),
            _ => None,
        };
        ensure_roles(&graph, env).expect("ok");
        let record = graph
            .get_role_incarnation("agent-lyra", ROLE_VERA)
            .expect("lookup")
            .expect("registered");
        assert_eq!(record.toolset_profile, "research");
    }
}
