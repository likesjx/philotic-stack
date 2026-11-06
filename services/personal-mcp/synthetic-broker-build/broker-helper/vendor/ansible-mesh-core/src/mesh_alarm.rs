//! Throttled alarms for mesh traffic this hotel could not decode
//! (MESH_DELIVERY_GUARANTEES L1, DEF-182).
//!
//! A peer running a newer or broken build can send gossip this binary cannot
//! parse. Those parses used to fail silently. Each failure now `warn!`s and
//! pushes a heal-queue row, at most once per minute per `(peer, kind)` so a
//! peer that gossips every few seconds cannot flood the log or the queue.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tracing::warn;

use crate::heal_queue::HealQueueStorage;

/// Heal-queue pattern tag for a gossip message (heartbeat, capability sync,
/// catalog sync, hotel state, ack) that did not decode.
pub const MESH_GOSSIP_UNDECODABLE_TAG: &str = "mesh_gossip_undecodable";
/// Heal-queue pattern tag for a mesh event batch element that did not decode
/// as an `EventEnvelope`.
pub const MESH_EVENT_UNDECODABLE_TAG: &str = "mesh_event_undecodable";
/// Heal-queue pattern tag for a cross-hotel task this hotel accepted but
/// could neither deliver, park nor rescue.
pub const MESH_TASK_DROPPED_TAG: &str = "mesh_task_dropped";

/// Minimum interval between reports for the same `(peer, kind)`.
pub const GOSSIP_ALARM_INTERVAL: Duration = Duration::from_secs(60);

/// Heal-queue `guest_id` the mesh alarms are filed under.
pub const MESH_ALARM_SOURCE: &str = "aiua.mesh_inbound";

/// Per-`(peer, kind)` throttle for undecodable-gossip reports.
#[derive(Debug, Default)]
pub struct GossipParseAlarm {
    last_reported: HashMap<(String, String), Instant>,
}

impl GossipParseAlarm {
    /// True when `(peer, key)` has not been reported within
    /// [`GOSSIP_ALARM_INTERVAL`] of `now`; records `now` when it returns true.
    /// Entries older than the interval are pruned so the map stays bounded.
    pub fn should_report(&mut self, peer: &str, key: &str, now: Instant) -> bool {
        self.last_reported
            .retain(|_, last| now.saturating_duration_since(*last) < GOSSIP_ALARM_INTERVAL);
        let key = (peer.to_string(), key.to_string());
        if self.last_reported.contains_key(&key) {
            return false;
        }
        self.last_reported.insert(key, now);
        true
    }

    /// Report an undecodable `kind` message from `peer`, throttled. Returns
    /// whether a report was emitted.
    pub fn report(
        &mut self,
        heal_queue: Option<&dyn HealQueueStorage>,
        peer: &str,
        kind: &'static str,
        error: &str,
    ) -> bool {
        if !self.should_report(peer, kind, Instant::now()) {
            return false;
        }
        warn!(
            peer,
            kind,
            error,
            "Undecodable mesh gossip from peer (reported at most once per minute per peer and kind)"
        );
        if let Some(hq) = heal_queue {
            let message = format!(
                "[{MESH_GOSSIP_UNDECODABLE_TAG}] {kind} from peer {peer} did not decode: {error}. \
                 The peer may run an incompatible build."
            );
            if let Err(err) = hq.push_classified(
                MESH_ALARM_SOURCE,
                &message,
                "medium",
                MESH_GOSSIP_UNDECODABLE_TAG,
            ) {
                warn!(error = %err, "Failed to push undecodable-gossip alarm to heal queue");
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gossip_alarm_reports_once_per_minute_per_peer_and_kind() {
        let mut alarm = GossipParseAlarm::default();
        let t0 = Instant::now();
        assert!(alarm.should_report("peer-a", "heartbeat", t0));
        assert!(!alarm.should_report("peer-a", "heartbeat", t0 + Duration::from_secs(30)));
        // A different kind or peer is its own throttle window.
        assert!(alarm.should_report("peer-a", "capability_sync", t0 + Duration::from_secs(30)));
        assert!(alarm.should_report("peer-b", "heartbeat", t0 + Duration::from_secs(30)));
        // The window reopens after the interval.
        assert!(alarm.should_report("peer-a", "heartbeat", t0 + GOSSIP_ALARM_INTERVAL));
    }
}
