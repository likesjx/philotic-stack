//! Last-writer-wins application of gossiped placement records.
//!
//! Placement (`RoleIncarnationRecord::home_node`, `MembraneTransportHomeRecord`)
//! is graph truth that every hotel must agree on, otherwise two hotels can
//! disagree about who is home for a role or who may poll a Telegram token
//! (DEF-107). Both record kinds carry a unix-seconds stamp of their last
//! placement change; a hotel applies a gossiped record only when the stamp is
//! strictly newer than its own copy, so a stale hotel re-gossiping an old
//! home can never flip a relocation back. A stamp of `0` means "never placed
//! at runtime" and is never applied — legacy or seed-only records stay local.
//!
//! Role homes are applied only onto role records the receiver already holds:
//! the gossip entry is a small `(agent, role, home, stamp)` tuple, not the
//! full record. A hotel that has never seen the role learns it from the next
//! `session.handoff` push, which carries the whole record including its home.

use crate::domain::GraphDomain;
use crate::graph::MembraneTransportHomeRecord;
use crate::heartbeat::HotelStateSyncRoleHome;
use tracing::{debug, warn};

/// Apply gossiped role homes and transport homes from `from_node`.
///
/// Returns `(role_homes_applied, transport_homes_applied)`.
pub fn apply_remote_placement(
    graph: &GraphDomain,
    from_node: &str,
    role_homes: &[HotelStateSyncRoleHome],
    transport_homes: &[MembraneTransportHomeRecord],
) -> (usize, usize) {
    let mut roles_applied = 0usize;
    for home in role_homes {
        if home.placement_updated_unix == 0 {
            continue;
        }
        match graph.get_role_incarnation(&home.agent_id, &home.role_name) {
            Ok(Some(mut local)) => {
                if home.placement_updated_unix > local.placement_updated_unix {
                    local.home_node = home.home_node.clone();
                    local.placement_updated_unix = home.placement_updated_unix;
                    match graph.upsert_role_incarnation(&local) {
                        Ok(()) => roles_applied += 1,
                        Err(err) => warn!(
                            "placement sync from {}: failed to apply home for {}:{}: {}",
                            from_node, home.agent_id, home.role_name, err
                        ),
                    }
                }
            }
            Ok(None) => debug!(
                "placement sync from {}: no local role record for {}:{}; a handoff push will carry it",
                from_node, home.agent_id, home.role_name
            ),
            Err(err) => warn!(
                "placement sync from {}: failed to read role {}:{}: {}",
                from_node, home.agent_id, home.role_name, err
            ),
        }
    }

    let mut transports_applied = 0usize;
    for home in transport_homes {
        if home.updated_unix == 0 {
            continue;
        }
        let apply = match graph.get_membrane_transport_home(
            &home.agent_id,
            &home.transport,
            &home.resource_ref,
        ) {
            Ok(Some(local)) => home.updated_unix > local.updated_unix,
            Ok(None) => true,
            Err(err) => {
                warn!(
                    "placement sync from {}: failed to read transport home {}:{}:{}: {}",
                    from_node, home.agent_id, home.transport, home.resource_ref, err
                );
                false
            }
        };
        if apply {
            match graph.upsert_membrane_transport_home(home) {
                Ok(()) => transports_applied += 1,
                Err(err) => warn!(
                    "placement sync from {}: failed to apply transport home {}:{}:{}: {}",
                    from_node, home.agent_id, home.transport, home.resource_ref, err
                ),
            }
        }
    }

    (roles_applied, transports_applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{MembraneTransportHomeStatus, RoleIncarnationRecord};
    use crate::sqlite_storage::SqliteGraphStorage;
    use std::sync::Arc;

    fn make_domain() -> GraphDomain {
        let storage = SqliteGraphStorage::open_in_memory().expect("in-memory graph");
        GraphDomain::new(Arc::new(storage.adapter()))
    }

    fn role(home: Option<&str>, stamp: u64) -> RoleIncarnationRecord {
        RoleIncarnationRecord {
            agent_id: "agent-bjork-01".into(),
            role_name: "orchestrator".into(),
            guest_id: "agent-bjork-01:orchestrator".into(),
            toolset_profile: "orchestrator".into(),
            home_node: home.map(str::to_string),
            placement_updated_unix: stamp,
            ..Default::default()
        }
    }

    fn role_home(home: Option<&str>, stamp: u64) -> HotelStateSyncRoleHome {
        HotelStateSyncRoleHome {
            agent_id: "agent-bjork-01".into(),
            role_name: "orchestrator".into(),
            home_node: home.map(str::to_string),
            placement_updated_unix: stamp,
        }
    }

    fn transport_home(active: &str, stamp: u64) -> MembraneTransportHomeRecord {
        MembraneTransportHomeRecord {
            agent_id: "agent-bjork-01".into(),
            transport: "telegram".into(),
            resource_ref: "telegram_bot_token_bjork".into(),
            active_home_hotel: active.into(),
            standby_hotels: vec!["mac-jane".into()],
            managed_by_role: "orchestrator".into(),
            lease_type: "telegram_poll".into(),
            failover_policy: "manual-or-explicit-delegation".into(),
            status: MembraneTransportHomeStatus::Active,
            updated_unix: stamp,
        }
    }

    #[test]
    fn newer_role_home_wins_and_older_or_equal_is_ignored() {
        let d = make_domain();
        d.upsert_role_incarnation(&role(None, 100)).unwrap();

        let (applied, _) =
            apply_remote_placement(&d, "vps", &[role_home(Some("vps-jane-aiua-01"), 200)], &[]);
        assert_eq!(applied, 1);
        let local = d
            .get_role_incarnation("agent-bjork-01", "orchestrator")
            .unwrap()
            .unwrap();
        assert_eq!(local.home_node.as_deref(), Some("vps-jane-aiua-01"));
        assert_eq!(local.placement_updated_unix, 200);

        // A stale hotel re-gossiping the old (unset) home must not flip it back.
        let (applied, _) = apply_remote_placement(&d, "mac", &[role_home(None, 150)], &[]);
        assert_eq!(applied, 0);
        let (applied, _) = apply_remote_placement(&d, "mac", &[role_home(None, 200)], &[]);
        assert_eq!(applied, 0);
        let local = d
            .get_role_incarnation("agent-bjork-01", "orchestrator")
            .unwrap()
            .unwrap();
        assert_eq!(local.home_node.as_deref(), Some("vps-jane-aiua-01"));

        // A newer explicit un-home does apply.
        let (applied, _) = apply_remote_placement(&d, "vps", &[role_home(None, 300)], &[]);
        assert_eq!(applied, 1);
        assert_eq!(
            d.get_role_incarnation("agent-bjork-01", "orchestrator")
                .unwrap()
                .unwrap()
                .home_node,
            None
        );
    }

    #[test]
    fn zero_stamp_and_unknown_role_are_ignored() {
        let d = make_domain();
        // Unknown role: nothing to apply onto.
        let (applied, _) =
            apply_remote_placement(&d, "vps", &[role_home(Some("vps-jane-aiua-01"), 200)], &[]);
        assert_eq!(applied, 0);
        assert!(d
            .get_role_incarnation("agent-bjork-01", "orchestrator")
            .unwrap()
            .is_none());

        // Legacy zero stamp: never applied even onto an existing record.
        d.upsert_role_incarnation(&role(None, 0)).unwrap();
        let (applied, _) =
            apply_remote_placement(&d, "vps", &[role_home(Some("vps-jane-aiua-01"), 0)], &[]);
        assert_eq!(applied, 0);
    }

    #[test]
    fn transport_home_is_created_when_missing_and_replaced_only_by_newer() {
        let d = make_domain();
        let (_, applied) =
            apply_remote_placement(&d, "vps", &[], &[transport_home("vps-jane", 100)]);
        assert_eq!(applied, 1);

        let (_, applied) =
            apply_remote_placement(&d, "mac", &[], &[transport_home("mac-jane", 90)]);
        assert_eq!(applied, 0);
        let (_, applied) =
            apply_remote_placement(&d, "mac", &[], &[transport_home("mac-jane", 100)]);
        assert_eq!(applied, 0);
        assert_eq!(
            d.get_membrane_transport_home("agent-bjork-01", "telegram", "telegram_bot_token_bjork")
                .unwrap()
                .unwrap()
                .active_home_hotel,
            "vps-jane"
        );

        let (_, applied) =
            apply_remote_placement(&d, "mac", &[], &[transport_home("mac-jane", 101)]);
        assert_eq!(applied, 1);
        assert_eq!(
            d.get_membrane_transport_home("agent-bjork-01", "telegram", "telegram_bot_token_bjork")
                .unwrap()
                .unwrap()
                .active_home_hotel,
            "mac-jane"
        );

        // Legacy zero-stamp records never travel.
        let (_, applied) = apply_remote_placement(&d, "mbp", &[], &[transport_home("mbp-jane", 0)]);
        assert_eq!(applied, 0);
    }
}
