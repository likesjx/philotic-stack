//! Relocation Ceremony R5: continuity transfer.
//!
//! A relocated role must resume its conversations on the target hotel, not
//! start them fresh. Session checkpoints (apartments), the session rows a
//! snapshot is composed from, and the agent identity live only in the
//! origin hotel's graph — nothing replicates them — so the ceremony carries
//! them itself: the origin exports a [`ContinuityBundle`] as the last read
//! before SWITCH, the target imports it and acks, and only then do the role
//! home and transport home flip. The target philote loads sessions lazily
//! on first touch, and no traffic reaches it before SWITCH, so its first
//! read of each session lands on the imported state.
//!
//! The bundle rides the HMAC-signed execution plane inline, like
//! `materialize.request`. It is signed, not encrypted — so it carries
//! checkpoints and session rows, never vault entries.

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::relocation_ceremony::RelocationCeremonyRecord;
use ansible_mesh_core::storage::{AgentIdentityRecord, SessionRecord};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Refuse to ship a bundle larger than this inline; the ceremony rolls back
/// rather than flood the execution plane (a frame is buffered whole).
pub(crate) const CONTINUITY_MAX_BYTES: usize = 8 * 1024 * 1024;

/// The session index apartment — which sessions an agent has open.
const SESSION_INDEX_MEMORY_TYPE: &str = "short";

/// The role whose process is the agent's front door: it owns the base
/// (role-less) checkpoint key and the session index, so they move with it.
const FRONT_DOOR_ROLE: &str = "orchestrator";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ContinuityApartment {
    pub memory_type: String,
    pub content: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ContinuityBundle {
    pub ceremony_id: String,
    pub agent_id: String,
    pub role_name: String,
    pub origin_node_id: String,
    #[serde(default)]
    pub origin_hotel_name: Option<String>,
    pub target_node_id: String,
    #[serde(default)]
    pub target_hotel_name: Option<String>,
    /// The transport moves with the role, so a checkpointed turn's reply
    /// must follow it to the target's membrane.
    #[serde(default)]
    pub include_transport: bool,
    pub exported_at_unix: u64,
    #[serde(default)]
    pub agent_identity: Option<AgentIdentityRecord>,
    #[serde(default)]
    pub sessions: Vec<SessionRecord>,
    #[serde(default)]
    pub apartments: Vec<ContinuityApartment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ContinuityImportSummary {
    pub sessions: usize,
    pub apartments: usize,
    pub identity_seeded: bool,
    /// This ceremony's bundle was already imported — a retransmitted event.
    /// Nothing was rewritten, so turns the target has taken since survive.
    pub already_imported: bool,
}

/// Does `memory_type` hold a checkpoint that moves with `role_name`?
///
/// Each process keeps its own key for a session: a role process writes
/// `short_session:{sid}:{role}`, the front-door process writes
/// `short_session:{sid}`. Session ids themselves contain colons, so the
/// checkpoint's own `session_id` is what disambiguates the suffix.
fn checkpoint_moves_with_role(memory_type: &str, content: &Value, role_name: &str) -> bool {
    let Some(session_id) = content.get("session_id").and_then(Value::as_str) else {
        return false;
    };
    let base = format!("short_session:{session_id}");
    memory_type == format!("{base}:{role_name}")
        || (role_name == FRONT_DOOR_ROLE && memory_type == base)
}

/// Origin side: collect everything the target needs for `ceremony.role_name`
/// to resume its sessions — its checkpoints, their session rows, and (for
/// the front door) the session index — plus the agent identity, without
/// which the target hotel cannot compose a profile or seat a membrane.
pub(crate) fn export_continuity_bundle(
    graph: &GraphDomain,
    ceremony: &RelocationCeremonyRecord,
    origin_hotel_name: Option<String>,
    target_hotel_name: Option<String>,
    now_unix: u64,
) -> anyhow::Result<ContinuityBundle> {
    let agent_id = ceremony.agent_id.as_str();
    let role_name = ceremony.role_name.as_str();
    let mut apartments = Vec::new();
    let mut session_ids = BTreeSet::new();
    for memory_type in graph.list_apartments(agent_id)? {
        let Some(content) = graph.get_apartment(agent_id, &memory_type)? else {
            continue;
        };
        if memory_type == SESSION_INDEX_MEMORY_TYPE {
            if role_name == FRONT_DOOR_ROLE {
                apartments.push(ContinuityApartment {
                    memory_type,
                    content,
                });
            }
            continue;
        }
        if !memory_type.starts_with("short_session:")
            || !checkpoint_moves_with_role(&memory_type, &content, role_name)
        {
            continue;
        }
        if let Some(session_id) = content.get("session_id").and_then(Value::as_str) {
            session_ids.insert(session_id.to_string());
        }
        apartments.push(ContinuityApartment {
            memory_type,
            content,
        });
    }

    let mut sessions = Vec::new();
    for session_id in &session_ids {
        if let Some(session) = graph.get_session(session_id)? {
            sessions.push(session);
        }
    }

    Ok(ContinuityBundle {
        ceremony_id: ceremony.ceremony_id.clone(),
        agent_id: agent_id.to_string(),
        role_name: role_name.to_string(),
        origin_node_id: ceremony.origin_hotel.clone(),
        origin_hotel_name,
        target_node_id: ceremony.target_hotel.clone(),
        target_hotel_name,
        include_transport: ceremony.include_transport,
        exported_at_unix: now_unix,
        agent_identity: graph.get_agent_identity(agent_id)?,
        sessions,
        apartments,
    })
}

fn continuity_import_marker_key(ceremony_id: &str) -> String {
    format!("continuity_import:{ceremony_id}")
}

/// Swap a value naming the origin (node id or hotel name) for the target's.
fn rewrite_origin_value(value: &mut Value, bundle: &ContinuityBundle) {
    let Some(text) = value.as_str() else {
        return;
    };
    if text == bundle.origin_node_id {
        *value = Value::String(bundle.target_node_id.clone());
    } else if let (Some(origin), Some(target)) = (
        bundle.origin_hotel_name.as_deref(),
        bundle.target_hotel_name.as_deref(),
    ) && text == origin
    {
        *value = Value::String(target.to_string());
    }
}

/// Re-point every hotel-bound field in a checkpoint or session summary at
/// the target. `delivery_*` says where the agent lives, so it always moves.
/// `final_reply_to` says where the membrane that owes the reply lives, so
/// it moves only when the transport moves with the role.
fn rewrite_hotel_fields(value: &mut Value, bundle: &ContinuityBundle) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                match key.as_str() {
                    "delivery_hotel" | "delivery_node_id" => rewrite_origin_value(child, bundle),
                    "final_reply_to" if bundle.include_transport => {
                        rewrite_origin_value(child, bundle)
                    }
                    _ => rewrite_hotel_fields(child, bundle),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_hotel_fields(item, bundle);
            }
        }
        _ => {}
    }
}

/// A guest id minted on the origin (`{hotel}:philote-{key}`) names the
/// origin's process; the target's process for the same agent carries the
/// target's prefix.
fn rewrite_guest_prefix(guest_id: &str, bundle: &ContinuityBundle) -> String {
    if let (Some(origin), Some(target)) = (
        bundle.origin_hotel_name.as_deref(),
        bundle.target_hotel_name.as_deref(),
    ) && let Some(rest) = guest_id.strip_prefix(&format!("{origin}:"))
    {
        return format!("{target}:{rest}");
    }
    guest_id.to_string()
}

/// Most sessions a session index keeps — mirrors philote's
/// `merge_session_index` cap.
const SESSION_INDEX_CAP: usize = 32;

/// The session index is merged everywhere it is written, never replaced:
/// the target may already hold sessions this agent opened there. Imported
/// entries win on a `session_id` collision; the newest `updated_at` survive
/// the cap.
fn merge_session_indexes(existing: &Value, mut imported: Value) -> Value {
    let entries = |index: &Value| -> Vec<Value> {
        index
            .get("active_sessions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut merged = entries(&imported);
    for entry in entries(existing) {
        let session_id = entry.get("session_id").and_then(Value::as_str);
        if !merged
            .iter()
            .any(|m| m.get("session_id").and_then(Value::as_str) == session_id)
        {
            merged.push(entry);
        }
    }
    merged.sort_by_key(|entry| {
        std::cmp::Reverse(entry.get("updated_at").and_then(Value::as_u64).unwrap_or(0))
    });
    merged.truncate(SESSION_INDEX_CAP);
    if let Some(map) = imported.as_object_mut() {
        map.insert("active_sessions".into(), Value::Array(merged));
    }
    imported
}

/// Target side: write the bundle into this hotel's graph. Idempotent per
/// ceremony — mesh events can be redelivered, and a redelivery that landed
/// after the target started taking turns must not roll them back.
pub(crate) fn import_continuity_bundle(
    graph: &GraphDomain,
    bundle: &ContinuityBundle,
) -> anyhow::Result<ContinuityImportSummary> {
    let marker_key = continuity_import_marker_key(&bundle.ceremony_id);
    if let Some(previous) = graph.get_config_value(&marker_key)? {
        let mut summary: ContinuityImportSummary =
            serde_json::from_str(&previous).unwrap_or(ContinuityImportSummary {
                sessions: 0,
                apartments: 0,
                identity_seeded: false,
                already_imported: true,
            });
        summary.already_imported = true;
        return Ok(summary);
    }

    // Seed, never overwrite: an identity already on this hotel may carry
    // local edits, and the origin's copy is only needed where there is none.
    let identity_seeded = match &bundle.agent_identity {
        Some(identity) if graph.get_agent_identity(&identity.agent_id)?.is_none() => {
            graph.upsert_agent_identity(identity)?;
            true
        }
        _ => false,
    };

    for session in &bundle.sessions {
        let mut session = session.clone();
        session.active_incarnation_id = session
            .active_incarnation_id
            .as_deref()
            .map(|guest_id| rewrite_guest_prefix(guest_id, bundle));
        rewrite_hotel_fields(&mut session.summary_json, bundle);
        graph.upsert_session(&session)?;
    }

    for apartment in &bundle.apartments {
        let mut content = apartment.content.clone();
        rewrite_hotel_fields(&mut content, bundle);
        if let Some(Value::String(incarnation)) = content.get_mut("active_incarnation_id") {
            *incarnation = rewrite_guest_prefix(incarnation, bundle);
        }
        if apartment.memory_type == SESSION_INDEX_MEMORY_TYPE
            && let Some(existing) =
                graph.get_apartment(&bundle.agent_id, SESSION_INDEX_MEMORY_TYPE)?
        {
            content = merge_session_indexes(&existing, content);
        }
        graph.sync_apartment(&bundle.agent_id, &apartment.memory_type, &content)?;
    }

    let summary = ContinuityImportSummary {
        sessions: bundle.sessions.len(),
        apartments: bundle.apartments.len(),
        identity_seeded,
        already_imported: false,
    };
    graph.set_config_value(&marker_key, &serde_json::to_string(&summary)?)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use std::sync::Arc;

    fn graph() -> GraphDomain {
        let store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        GraphDomain::new(Arc::new(store.adapter()))
    }

    fn session(session_id: &str, incarnation: &str, summary: Value) -> SessionRecord {
        SessionRecord {
            session_id: session_id.into(),
            session_kind: "conversation".into(),
            primary_agent_id: Some("agent-bjork-01".into()),
            active_incarnation_id: Some(incarnation.into()),
            channel_kind: Some("telegram".into()),
            channel_session_key: Some("7".into()),
            status: "active".into(),
            lease_owner_component_id: None,
            lease_expires_at: None,
            summary_json: summary,
            created_at: 1,
            updated_at: 2,
        }
    }

    fn ceremony(role_name: &str, include_transport: bool) -> RelocationCeremonyRecord {
        RelocationCeremonyRecord::new(
            "relocate-1".into(),
            "agent-bjork-01".into(),
            role_name.into(),
            "mac-jane-aiua-01".into(),
            "vps-jane-aiua-01".into(),
            include_transport,
            include_transport.then(|| "telegram".into()),
            include_transport.then(|| "telegram_bot_token_bjork".into()),
            "orchestrator".into(),
            "test".into(),
            10,
        )
    }

    fn export(g: &GraphDomain, role_name: &str, include_transport: bool) -> ContinuityBundle {
        export_continuity_bundle(
            g,
            &ceremony(role_name, include_transport),
            Some("mac-jane".into()),
            Some("vps-jane".into()),
            20,
        )
        .expect("export")
    }

    const SID: &str = "telegram:7:agent-bjork-01";

    /// An origin graph with Björk's front-door checkpoint, a virtuosa
    /// checkpoint for the same session, and the session index.
    fn seeded_origin() -> GraphDomain {
        let g = graph();
        g.upsert_agent_identity(&AgentIdentityRecord {
            agent_id: "agent-bjork-01".into(),
            persona_name: "Björk".into(),
            authority_hotel: "mac-jane".into(),
            bundle_json: serde_json::json!({"soul_text": "origin soul"}),
        })
        .unwrap();
        g.upsert_session(&session(
            SID,
            "mac-jane:philote-bjork",
            serde_json::json!({"agent_runtime_provenance": {
                "delivery_hotel": "mac-jane", "delivery_node_id": "mac-jane-aiua-01"}}),
        ))
        .unwrap();
        g.sync_apartment(
            "agent-bjork-01",
            &format!("short_session:{SID}"),
            &serde_json::json!({
                "session_id": SID,
                "carryover_plan": {"goal": "log practice"},
                "active_turn": {"final_reply_to": "mac-jane-aiua-01"},
            }),
        )
        .unwrap();
        g.sync_apartment(
            "agent-bjork-01",
            &format!("short_session:{SID}:virtuosa"),
            &serde_json::json!({"session_id": SID, "parked_plan_turn": {"turn_id": "v1"}}),
        )
        .unwrap();
        g.sync_apartment(
            "agent-bjork-01",
            "short",
            &serde_json::json!({"active_sessions": [{"session_id": SID}]}),
        )
        .unwrap();
        g
    }

    fn memory_types(bundle: &ContinuityBundle) -> Vec<&str> {
        let mut types: Vec<&str> = bundle
            .apartments
            .iter()
            .map(|a| a.memory_type.as_str())
            .collect();
        types.sort();
        types
    }

    #[test]
    fn orchestrator_move_carries_the_front_door_checkpoint_and_index_but_not_other_roles() {
        let bundle = export(&seeded_origin(), "orchestrator", false);
        assert_eq!(
            memory_types(&bundle),
            vec!["short", "short_session:telegram:7:agent-bjork-01"]
        );
        assert_eq!(bundle.sessions.len(), 1);
        assert!(bundle.agent_identity.is_some());
    }

    #[test]
    fn a_role_move_carries_only_that_roles_checkpoints() {
        let bundle = export(&seeded_origin(), "virtuosa", false);
        assert_eq!(
            memory_types(&bundle),
            vec!["short_session:telegram:7:agent-bjork-01:virtuosa"]
        );
    }

    #[test]
    fn import_repoints_the_session_at_the_target_and_keeps_the_checkpoint_whole() {
        let bundle = export(&seeded_origin(), "orchestrator", true);
        let target = graph();
        let summary = import_continuity_bundle(&target, &bundle).unwrap();
        assert_eq!(summary.sessions, 1);
        assert_eq!(summary.apartments, 2);
        assert!(summary.identity_seeded);

        let session = target.get_session(SID).unwrap().expect("session imported");
        assert_eq!(
            session.active_incarnation_id.as_deref(),
            Some("vps-jane:philote-bjork")
        );
        let provenance = &session.summary_json["agent_runtime_provenance"];
        assert_eq!(provenance["delivery_hotel"], "vps-jane");
        assert_eq!(provenance["delivery_node_id"], "vps-jane-aiua-01");

        let checkpoint = target
            .get_apartment("agent-bjork-01", &format!("short_session:{SID}"))
            .unwrap()
            .expect("checkpoint imported");
        assert_eq!(checkpoint["carryover_plan"]["goal"], "log practice");
        assert_eq!(
            checkpoint["active_turn"]["final_reply_to"], "vps-jane-aiua-01",
            "the transport moved, so the owed reply follows it"
        );
    }

    #[test]
    fn a_role_only_move_leaves_the_reply_with_the_membrane_that_owes_it() {
        let bundle = export(&seeded_origin(), "orchestrator", false);
        let target = graph();
        import_continuity_bundle(&target, &bundle).unwrap();
        let checkpoint = target
            .get_apartment("agent-bjork-01", &format!("short_session:{SID}"))
            .unwrap()
            .unwrap();
        assert_eq!(
            checkpoint["active_turn"]["final_reply_to"],
            "mac-jane-aiua-01"
        );
    }

    #[test]
    fn a_redelivered_import_never_rolls_back_turns_taken_since() {
        let bundle = export(&seeded_origin(), "orchestrator", false);
        let target = graph();
        import_continuity_bundle(&target, &bundle).unwrap();
        let key = format!("short_session:{SID}");
        target
            .sync_apartment(
                "agent-bjork-01",
                &key,
                &serde_json::json!({"session_id": SID, "carryover_plan": null}),
            )
            .unwrap();

        let again = import_continuity_bundle(&target, &bundle).unwrap();
        assert!(again.already_imported);
        let checkpoint = target
            .get_apartment("agent-bjork-01", &key)
            .unwrap()
            .unwrap();
        assert!(
            checkpoint["carryover_plan"].is_null(),
            "the target's newer checkpoint must survive a redelivery"
        );
    }

    #[test]
    fn import_merges_the_session_index_instead_of_replacing_it() {
        let bundle = export(&seeded_origin(), "orchestrator", false);
        let target = graph();
        target
            .sync_apartment(
                "agent-bjork-01",
                "short",
                &serde_json::json!({"active_sessions": [
                    {"session_id": "smoke:bjork:ping", "updated_at": 5},
                    {"session_id": SID, "updated_at": 1, "stale": true},
                ]}),
            )
            .unwrap();
        import_continuity_bundle(&target, &bundle).unwrap();

        let index = target
            .get_apartment("agent-bjork-01", "short")
            .unwrap()
            .unwrap();
        let sessions = index["active_sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2, "both sessions survive: {index}");
        let moved = sessions
            .iter()
            .find(|s| s["session_id"] == SID)
            .expect("moved session indexed");
        assert!(moved.get("stale").is_none(), "the imported entry wins");
        assert!(
            sessions
                .iter()
                .any(|s| s["session_id"] == "smoke:bjork:ping")
        );
    }

    #[test]
    fn import_never_overwrites_an_identity_the_target_already_has() {
        let bundle = export(&seeded_origin(), "orchestrator", false);
        let target = graph();
        target
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-bjork-01".into(),
                persona_name: "Björk".into(),
                authority_hotel: "vps-jane".into(),
                bundle_json: serde_json::json!({"soul_text": "target soul"}),
            })
            .unwrap();
        let summary = import_continuity_bundle(&target, &bundle).unwrap();
        assert!(!summary.identity_seeded);
        let identity = target
            .get_agent_identity("agent-bjork-01")
            .unwrap()
            .unwrap();
        assert_eq!(identity.bundle_json["soul_text"], "target soul");
    }
}
