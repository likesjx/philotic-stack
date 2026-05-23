use ansible_mesh_core::{ExposureTier, PerimeterSnapshot};
use tokio::sync::broadcast;

/// Emitted on the broadcast channel when the hotel's network posture changes.
#[derive(Debug, Clone)]
pub enum PerimeterEvent {
    /// The ceiling tier changed (e.g., machine gained or lost a public IP).
    /// Downgrade: affected endpoints may be suspended. Upgrade: no auto-widening.
    Shift {
        previous: ExposureTier,
        current: ExposureTier,
    },
    /// A full re-derivation completed (routine refresh or after a Shift).
    Refreshed(PerimeterSnapshot),
}

/// Trait implemented by the hotel's PerimeterService.
/// Consumers (membrane-mcp, philotic-client, aiua policy gates) hold Arc<dyn PerimeterService>.
pub trait PerimeterService: Send + Sync {
    /// Current point-in-time snapshot. Cheap — returns a clone of the cached state.
    fn snapshot(&self) -> PerimeterSnapshot;

    /// The hotel's current exposure ceiling. Equivalent to `snapshot().ceiling` but cheaper.
    fn ceiling(&self) -> ExposureTier;

    /// Subscribe to perimeter change events.
    fn subscribe(&self) -> broadcast::Receiver<PerimeterEvent>;

    /// Force a re-derivation of the perimeter state (e.g., after operator `hotel.perimeter.refresh`).
    /// Implementors should re-scan interfaces, update the snapshot, and broadcast a Refreshed event.
    fn refresh(&self);
}
