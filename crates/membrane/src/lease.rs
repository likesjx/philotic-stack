//! Lease lifecycle types shared across all membrane variants.

use serde::{Deserialize, Serialize};

/// Outcome of a lease renewal attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum LeaseRenewResult {
    /// Renewal succeeded. The new epoch is returned.
    Ok { epoch: u64 },
    /// The lease was lost and needs to be re-acquired from scratch.
    NeedsReacquire,
    /// Another holder took the lease. `owner` is their guest_id.
    Lost { owner: Option<String> },
}

impl LeaseRenewResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }
}
