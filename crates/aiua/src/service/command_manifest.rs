//! Agent command manifests, replicated to peer hotels (DEF-180).
//!
//! A Telegram seat builds its bot menu and `/help` from the agent's command
//! manifest, read from its OWN hotel's graph (`__apartment__:{agent}:
//! command_manifest`). The philote writes that apartment on the hotel that
//! runs it — so a seat polling on a different hotel (a transport home is
//! independent of its agent's home) found nothing and registered only the
//! four built-in commands, replacing the bot's real menu.
//!
//! The hotel that hosts an agent pushes its manifest to every peer whenever
//! the philote publishes it, and re-broadcasts periodically so a peer that
//! restarted or joined later catches up. A peer caches it as the same
//! apartment, so the seat's existing local read just works. Commands themselves
//! were never affected: the seat forwards any command it does not handle
//! itself to the agent, wherever it runs.
//!
//! The event rides `SessionControl` with its own `action`; hotels that
//! predate this ignore an action they do not know, so a mixed-version mesh
//! stays safe.

use crate::LedgerCommand;
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use ansible_mesh_core::registry::NodeRegistry;
use philotic_client::CommandManifestEntry;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// The `SessionControl` action that carries a manifest.
pub(crate) const SYNC_ACTION: &str = "agent.command_manifest.sync";

/// The apartment a philote publishes its manifest to.
const MANIFEST_MEMORY_TYPE: &str = "command_manifest";

/// Most entries a manifest may carry — Telegram itself menus at most 100.
const MAX_MANIFEST_ENTRIES: usize = 200;

/// Config key marking a cached manifest as a peer's copy. A manifest the local
/// philote wrote has no marker — the hotel hosts that agent and never
/// overwrites its own with a peer's, and never re-broadcasts a peer's.
fn origin_marker_key(agent_id: &str) -> String {
    format!("command_manifest_origin:{agent_id}")
}

/// True when this hotel runs `agent_id` right now: an ACTIVE guest here is the
/// agent or one of its role incarnations (`agent-x`, `agent-x:role`).
///
/// This is the one definition of "hosts" both directions use (DEF-189). A
/// leftover `command_manifest` apartment is NOT hosting — mac-jane kept
/// Beacon's from when it ran her, re-broadcast it every 2 minutes for an agent
/// it no longer served, and got refused `NotTheAgentsHost` each time, while the
/// vps refused the mirror image (`LocallyHosted`) on a stale apartment of its own.
fn hosts_agent(graph: &GraphDomain, hotel_name: &str, agent_id: &str) -> anyhow::Result<bool> {
    Ok(graph.list_guests(hotel_name, true)?.iter().any(|guest| {
        guest.guest_id == agent_id
            || guest
                .guest_id
                .strip_prefix(agent_id)
                .is_some_and(|rest| rest.starts_with(':'))
    }))
}

/// First sighting of a (peer, agent, reason) refusal? Expected refusals recur
/// every re-broadcast, so they are logged loudly once and quietly after — a
/// stale peer must be visible without drowning the log (271 WARNs in 18 min).
fn first_refusal(source: &str, agent_id: &str, reason: &str) -> bool {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    SEEN.get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert(format!("{source}|{agent_id}|{reason}")))
        .unwrap_or(true)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The philote on this hotel just published `agent_id`'s manifest: it is now
/// the local truth, so drop any peer-copy marker and tell the peers.
pub(crate) async fn on_local_manifest_written(
    graph: &GraphDomain,
    dispatcher_tx: &mpsc::Sender<LedgerCommand>,
    local_node_id: &str,
    agent_id: &str,
    commands: &Value,
) {
    let _ = graph.remove_config_value(&origin_marker_key(agent_id));
    broadcast_manifest(graph, dispatcher_tx, local_node_id, agent_id, commands).await;
}

async fn broadcast_manifest(
    graph: &GraphDomain,
    dispatcher_tx: &mpsc::Sender<LedgerCommand>,
    local_node_id: &str,
    agent_id: &str,
    commands: &Value,
) {
    let Ok(peers) = graph.list_hotels() else {
        return;
    };
    let data = serde_json::json!({
        "action": SYNC_ACTION,
        "agent_id": agent_id,
        "commands": commands,
    })
    .to_string();
    for peer in peers
        .into_iter()
        .map(|hotel| hotel.capabilities.node_id)
        .filter(|node_id| node_id != local_node_id)
    {
        let event = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: local_node_id.to_string(),
            target_node_id: Some(peer),
            source_agent_id: local_node_id.to_string(),
            target_agent_id: None,
            kind: EventKind::SessionControl,
            corr_id: String::new(),
            attempt: 0,
            created_at: now_secs(),
            expires_at: None,
            payload: EventPayload::Inline { data: data.clone() },
            trace: vec![],
        };
        let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(event)).await;
    }
}

/// Re-broadcast every manifest a philote on THIS hotel published (not the
/// peer copies it caches), so a peer that restarted or joined since the last
/// publish catches up.
pub(crate) async fn rebroadcast_local_manifests(
    graph: &GraphDomain,
    dispatcher_tx: &mpsc::Sender<LedgerCommand>,
    local_node_id: &str,
    hotel_name: &str,
) {
    let Ok(nodes) = graph.list_agents_with_apartment(MANIFEST_MEMORY_TYPE) else {
        return;
    };
    for agent_id in nodes {
        if graph
            .get_config_value(&origin_marker_key(&agent_id))
            .ok()
            .flatten()
            .is_some()
        {
            continue;
        }
        // Only an agent this hotel is actually running: a stale apartment for
        // one that moved away must not be presented to peers as current. A
        // lookup failure skips the pass (it repeats in 2 minutes).
        if !hosts_agent(graph, hotel_name, &agent_id).unwrap_or(false) {
            continue;
        }
        if let Ok(Some(commands)) = graph.get_apartment(&agent_id, MANIFEST_MEMORY_TYPE) {
            broadcast_manifest(graph, dispatcher_tx, local_node_id, &agent_id, &commands).await;
        }
    }
}

/// Why a peer's manifest was not cached.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ManifestRefused {
    Malformed(String),
    /// The sender is not the hotel the roster says runs the agent.
    NotTheAgentsHost,
    /// This hotel's own philote published a manifest for the agent.
    LocallyHosted,
}

/// Cache a peer's manifest as the apartment the seat reads.
///
/// Only the hotel the roster says runs the agent may publish for it: a menu
/// is what the operator sees and what `/help` prints, so any peer must not
/// be able to write another agent's.
pub(crate) fn apply_remote_manifest(
    graph: &GraphDomain,
    registry: &NodeRegistry,
    hotel_name: &str,
    source_node_id: &str,
    data: &str,
) -> Result<String, ManifestRefused> {
    let payload: Value =
        serde_json::from_str(data).map_err(|e| ManifestRefused::Malformed(e.to_string()))?;
    let agent_id = payload
        .get("agent_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ManifestRefused::Malformed("no agent_id".into()))?;
    let commands = payload
        .get("commands")
        .ok_or_else(|| ManifestRefused::Malformed("no commands".into()))?;
    let entries: Vec<CommandManifestEntry> = serde_json::from_value(commands.clone())
        .map_err(|e| ManifestRefused::Malformed(e.to_string()))?;
    if entries.len() > MAX_MANIFEST_ENTRIES {
        return Err(ManifestRefused::Malformed(format!(
            "{} entries (max {MAX_MANIFEST_ENTRIES})",
            entries.len()
        )));
    }
    if registry.find_node_id_for_agent(agent_id).as_deref() != Some(source_node_id) {
        return Err(ManifestRefused::NotTheAgentsHost);
    }
    let marker = origin_marker_key(agent_id);
    // "Ours" means this hotel runs the agent AND its philote wrote the menu
    // (no peer-copy marker). An unmarked apartment with no active guest behind
    // it is a leftover from an agent that moved away: the peer that the roster
    // says runs it now supersedes it. A lookup failure keeps what we have.
    let has_own = graph
        .get_apartment(agent_id, MANIFEST_MEMORY_TYPE)
        .ok()
        .flatten()
        .is_some()
        && graph.get_config_value(&marker).ok().flatten().is_none()
        && hosts_agent(graph, hotel_name, agent_id).unwrap_or(true);
    if has_own {
        return Err(ManifestRefused::LocallyHosted);
    }
    graph
        .sync_apartment(agent_id, MANIFEST_MEMORY_TYPE, commands)
        .map_err(|e| ManifestRefused::Malformed(e.to_string()))?;
    let _ = graph.set_config_value(&marker, &serde_json::json!(source_node_id).to_string());
    Ok(agent_id.to_string())
}

/// Mesh entry point for a received manifest event.
pub(crate) fn handle_remote_manifest_sync(
    graph: &GraphDomain,
    registry: &NodeRegistry,
    hotel_name: &str,
    source_node_id: &str,
    data: &str,
) {
    match apply_remote_manifest(graph, registry, hotel_name, source_node_id, data) {
        Ok(agent_id) => info!(
            agent_id = %agent_id,
            source = source_node_id,
            "Cached a peer hotel's command manifest for a seat on this hotel"
        ),
        // A malformed manifest is always worth a warning. The other two are
        // steady-state disagreements that repeat with every re-broadcast: say so
        // once per (peer, agent, reason), then drop to debug.
        Err(refused) => {
            let agent_id = serde_json::from_str::<Value>(data)
                .ok()
                .and_then(|v| {
                    v.get("agent_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            let repeat = !matches!(refused, ManifestRefused::Malformed(_))
                && !first_refusal(source_node_id, &agent_id, &format!("{refused:?}"));
            if repeat {
                debug!(
                    source = source_node_id,
                    agent_id = %agent_id,
                    "Refused a peer's command manifest again: {refused:?}"
                );
            } else {
                warn!(
                    source = source_node_id,
                    agent_id = %agent_id,
                    "Refused a peer's command manifest: {refused:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::heartbeat::{HotelStateSyncAgent, HotelStateSyncGuest};
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::storage::{GuestRecord, HotelRecord};
    use std::sync::Arc;

    /// The hotel under test (the receiver / re-broadcaster).
    const LOCAL: &str = "mac-jane";

    fn graph() -> GraphDomain {
        let store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        GraphDomain::new(Arc::new(store.adapter()))
    }

    /// `agent` is running on the hotel under test (its orchestrator guest is active).
    fn host_locally(g: &GraphDomain, agent: &str, active: bool) {
        g.upsert_guest(&GuestRecord {
            hotel_name: LOCAL.into(),
            guest_id: format!("{agent}:orchestrator"),
            role: "agent".into(),
            config_json: "{}".into(),
            is_active: active,
            active_pid: None,
            last_active_at: None,
        })
        .unwrap();
    }

    fn seed_peers(g: &GraphDomain) {
        for (name, node) in [
            ("vps-jane", "vps-jane-aiua-01"),
            ("mac-jane", "mac-jane-aiua-01"),
            ("mbp-jane", "mbp-jane-aiua-01"),
        ] {
            g.upsert_hotel(&HotelRecord {
                hotel_name: name.into(),
                capabilities: ansible_mesh_core::NodeCapabilities {
                    node_id: node.into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: String::new(),
                active_pid: None,
                mesh_host: None,
            })
            .unwrap();
        }
    }

    /// The vps runs Beacon; mac-jane (the caller) has her roster entry.
    fn roster() -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        registry.observe_hotel_state(
            "vps-jane-aiua-01".into(),
            "vps-jane".into(),
            vec![HotelStateSyncGuest {
                guest_id: "agent-beacon:orchestrator".into(),
                role: "agent".into(),
                active: true,
            }],
            vec![HotelStateSyncAgent {
                agent_id: "agent-beacon".into(),
                persona_name: "Beacon".into(),
            }],
        );
        registry
    }

    fn payload(agent: &str) -> String {
        serde_json::json!({
            "action": SYNC_ACTION,
            "agent_id": agent,
            "commands": [
                {"command": "role", "description": "Switch role", "usage_hint": "/role <name>"},
                {"command": "status", "description": "Agent status"}
            ]
        })
        .to_string()
    }

    #[test]
    fn a_seat_reads_the_peer_hotels_manifest_from_its_own_graph() {
        let g = graph();
        let cached = apply_remote_manifest(
            &g,
            &roster(),
            LOCAL,
            "vps-jane-aiua-01",
            &payload("agent-beacon"),
        )
        .expect("the agent's host may publish its manifest");
        assert_eq!(cached, "agent-beacon");

        // Exactly the read `fetch_agent_command_manifest` does.
        let stored = g
            .get_apartment("agent-beacon", "command_manifest")
            .unwrap()
            .expect("cached as the apartment the seat reads");
        let entries: Vec<CommandManifestEntry> = serde_json::from_value(stored).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].command, "role");
        assert!(
            g.get_config_value(&origin_marker_key("agent-beacon"))
                .unwrap()
                .is_some(),
            "marked as a peer's copy, so it is never re-broadcast"
        );
    }

    #[test]
    fn only_the_hotel_that_runs_the_agent_may_publish_for_it() {
        let g = graph();
        assert_eq!(
            apply_remote_manifest(
                &g,
                &roster(),
                LOCAL,
                "mbp-jane-aiua-01",
                &payload("agent-beacon")
            ),
            Err(ManifestRefused::NotTheAgentsHost),
            "a peer that does not run Beacon cannot write her menu"
        );
        assert_eq!(
            apply_remote_manifest(
                &g,
                &roster(),
                LOCAL,
                "vps-jane-aiua-01",
                &payload("agent-unknown")
            ),
            Err(ManifestRefused::NotTheAgentsHost)
        );
        assert!(
            g.get_apartment("agent-beacon", "command_manifest")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_hotel_never_overwrites_its_own_agents_manifest_with_a_peers() {
        let g = graph();
        host_locally(&g, "agent-beacon", true);
        g.sync_apartment(
            "agent-beacon",
            "command_manifest",
            &serde_json::json!([{"command": "mine", "description": "local"}]),
        )
        .unwrap();
        assert_eq!(
            apply_remote_manifest(
                &g,
                &roster(),
                LOCAL,
                "vps-jane-aiua-01",
                &payload("agent-beacon")
            ),
            Err(ManifestRefused::LocallyHosted)
        );
        let stored = g
            .get_apartment("agent-beacon", "command_manifest")
            .unwrap()
            .unwrap();
        assert_eq!(stored[0]["command"], "mine");
    }

    /// DEF-189: an unmarked apartment with no ACTIVE guest behind it is a
    /// leftover from an agent that moved away. The vps refused mac-jane's
    /// Beacon manifest as `LocallyHosted` on exactly such a leftover, forever.
    #[test]
    fn a_leftover_apartment_is_superseded_by_the_host_the_roster_names() {
        for guest in [None, Some(false)] {
            let g = graph();
            if let Some(active) = guest {
                host_locally(&g, "agent-beacon", active); // stopped, not running
            }
            g.sync_apartment(
                "agent-beacon",
                "command_manifest",
                &serde_json::json!([{"command": "stale", "description": "left behind"}]),
            )
            .unwrap();
            apply_remote_manifest(
                &g,
                &roster(),
                LOCAL,
                "vps-jane-aiua-01",
                &payload("agent-beacon"),
            )
            .expect("a stale local copy must not block the agent's real host");
            let stored = g
                .get_apartment("agent-beacon", "command_manifest")
                .unwrap()
                .unwrap();
            assert_eq!(stored[0]["command"], "role", "replaced by the host's menu");
            assert!(
                g.get_config_value(&origin_marker_key("agent-beacon"))
                    .unwrap()
                    .is_some(),
                "now marked as a peer's copy"
            );
        }
    }

    /// DEF-189: only an agent this hotel is actually running is presented to
    /// peers; a stale apartment for one that moved away is not (it drew
    /// `NotTheAgentsHost` from every peer every 2 minutes).
    #[tokio::test]
    async fn rebroadcast_only_covers_agents_this_hotel_runs() {
        let g = graph();
        seed_peers(&g);
        let menu = serde_json::json!([{"command": "status", "description": "s"}]);
        // Running here.
        host_locally(&g, "agent-jane", true);
        g.sync_apartment("agent-jane", "command_manifest", &menu)
            .unwrap();
        // Left behind: an apartment, but no guest of this hotel serves it.
        g.sync_apartment("agent-beacon", "command_manifest", &menu)
            .unwrap();
        // Stopped guest is not running either.
        host_locally(&g, "agent-astrid", false);
        g.sync_apartment("agent-astrid", "command_manifest", &menu)
            .unwrap();

        let (tx, mut rx) = mpsc::channel(16);
        rebroadcast_local_manifests(&g, &tx, "mac-jane-aiua-01", LOCAL).await;
        drop(tx);
        let mut agents = std::collections::BTreeSet::new();
        while let Some(LedgerCommand::AppendLocal(event)) = rx.recv().await {
            let EventPayload::Inline { data } = event.payload else {
                panic!("manifest events are inline");
            };
            let v: Value = serde_json::from_str(&data).unwrap();
            agents.insert(v["agent_id"].as_str().unwrap().to_string());
        }
        assert_eq!(agents, ["agent-jane".to_string()].into());
    }

    #[test]
    fn a_repeated_refusal_is_reported_once() {
        assert!(first_refusal("mac-x", "agent-y", "NotTheAgentsHost"));
        assert!(!first_refusal("mac-x", "agent-y", "NotTheAgentsHost"));
        // A different peer, agent or reason is a new fact worth a line.
        assert!(first_refusal("mbp-x", "agent-y", "NotTheAgentsHost"));
        assert!(first_refusal("mac-x", "agent-z", "NotTheAgentsHost"));
        assert!(first_refusal("mac-x", "agent-y", "LocallyHosted"));
    }

    #[test]
    fn a_newer_peer_copy_replaces_an_older_one() {
        let g = graph();
        apply_remote_manifest(
            &g,
            &roster(),
            LOCAL,
            "vps-jane-aiua-01",
            &payload("agent-beacon"),
        )
        .unwrap();
        let newer = serde_json::json!({
            "action": SYNC_ACTION, "agent_id": "agent-beacon",
            "commands": [{"command": "only", "description": "one"}]
        })
        .to_string();
        apply_remote_manifest(&g, &roster(), LOCAL, "vps-jane-aiua-01", &newer)
            .expect("a peer's copy is replaced by the peer's newer one");
        let stored = g
            .get_apartment("agent-beacon", "command_manifest")
            .unwrap()
            .unwrap();
        assert_eq!(stored.as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_malformed_or_oversized_manifest_is_refused() {
        let g = graph();
        assert!(matches!(
            apply_remote_manifest(&g, &roster(), LOCAL, "vps-jane-aiua-01", "not json"),
            Err(ManifestRefused::Malformed(_))
        ));
        let too_many: Vec<Value> = (0..=MAX_MANIFEST_ENTRIES)
            .map(|i| serde_json::json!({"command": format!("c{i}"), "description": "d"}))
            .collect();
        let big = serde_json::json!({"agent_id": "agent-beacon", "commands": too_many}).to_string();
        assert!(matches!(
            apply_remote_manifest(&g, &roster(), LOCAL, "vps-jane-aiua-01", &big),
            Err(ManifestRefused::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn the_hosting_hotel_publishes_to_every_peer_and_not_to_itself() {
        let g = graph();
        seed_peers(&g);
        let (tx, mut rx) = mpsc::channel(8);
        let commands = serde_json::json!([{"command": "status", "description": "s"}]);
        on_local_manifest_written(&g, &tx, "vps-jane-aiua-01", "agent-beacon", &commands).await;
        drop(tx);
        let mut targets = Vec::new();
        while let Some(LedgerCommand::AppendLocal(event)) = rx.recv().await {
            assert!(matches!(event.kind, EventKind::SessionControl));
            targets.push(event.target_node_id.unwrap());
        }
        targets.sort();
        assert_eq!(targets, vec!["mac-jane-aiua-01", "mbp-jane-aiua-01"]);
    }
}
