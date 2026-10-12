//! Admission seam for authenticated voice cancellation.
//!
//! The ledger is installed for correlated edge submissions, but its runtime
//! cancellation adapter is absent pending trusted hotel authority. Never advertise
//! `turn_cancel_v1` merely because this ledger can suppress outgoing audio.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::{future::Future, pin::Pin};

/// Install only from the server-owned runtime composition root after verified
/// IPC authority exists. Resolving this binding must not trust GuestIdentity or
/// task JSON. Success means generation/retry/publication cancellation completed;
/// committed tool effects are outside this interface.
pub(super) trait RuntimeTurnCancellation: Send + Sync {
    fn cancel<'a>(
        &'a self,
        binding: &'a TurnBinding,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

/// Construct only from the edge's verified device session, never payload JSON.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TurnBinding {
    pub device: String,
    pub request: String,
    pub target_node: String,
    pub target_agent: String,
    pub conversation: String,
    pub turn: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdmissionError {
    UnknownOrWrongScope,
    DuplicateRequest,
    Capacity,
    Revoked,
}

/// Unknown managed turns always deny, even after their terminal metadata expires.
/// Legacy turns retain their existing behavior. This namespace is server-issued.
pub(super) const MANAGED_TURN_PREFIX: &str = "operator-chat-edge-turn-";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Pending,
    Accepted,
    Revoked,
    Finished,
}

struct Entry {
    binding: TurnBinding,
    phase: Phase,
    expires_at: Option<u64>,
    sealed_seq: Option<u64>,
}

/// Bounded metadata only; contains neither content nor credentials. Active turns
/// never expire. Terminal metadata expires/evicts separately from compact spent
/// request fingerprints, which prevent reuse for this process lifetime. Hash
/// collisions reject safely; fingerprints confer no authentication/authority.
pub(super) struct TurnBindings {
    entries: HashMap<(String, String), Entry>,
    spent: HashMap<String, HashSet<u64>>,
    fingerprint_hasher: RandomState,
    active_limit: usize,
    terminal_limit: usize,
    spent_limit: usize,
    ttl_ms: u64,
    now_ms: u64,
}

impl TurnBindings {
    pub fn new(active_limit: usize) -> Self {
        Self::with_limits(active_limit, active_limit, 4096, 30 * 60 * 1000)
    }

    pub fn with_limits(
        active_limit: usize,
        terminal_limit: usize,
        spent_limit: usize,
        ttl_ms: u64,
    ) -> Self {
        Self {
            entries: HashMap::new(),
            spent: HashMap::new(),
            fingerprint_hasher: RandomState::new(),
            active_limit,
            terminal_limit,
            spent_limit,
            ttl_ms,
            now_ms: 0,
        }
    }

    /// Input is server monotonic elapsed time, never client timestamps or wall
    /// clock. Rollback cannot extend/reopen leases; jumps remove terminals only.
    pub fn advance(&mut self, now_ms: u64) {
        self.now_ms = self.now_ms.max(now_ms);
        let now = self.now_ms;
        self.entries
            .retain(|_, entry| entry.expires_at.is_none_or(|deadline| deadline > now));
    }

    pub fn reserve(&mut self, binding: TurnBinding) -> Result<(), AdmissionError> {
        if !binding.turn.starts_with(MANAGED_TURN_PREFIX) {
            return Err(AdmissionError::UnknownOrWrongScope);
        }
        if self
            .entry_for_turn(&binding.device, &binding.turn)
            .is_some()
        {
            return Err(AdmissionError::DuplicateRequest);
        }
        let fingerprint = self
            .fingerprint_hasher
            .hash_one((&binding.device, &binding.request));
        let spent = self.spent.entry(binding.device.clone()).or_default();
        if spent.contains(&fingerprint) {
            return Err(AdmissionError::DuplicateRequest);
        }
        if spent.len() >= self.spent_limit
            || self
                .entries
                .values()
                .filter(|entry| {
                    entry.binding.device == binding.device && entry.expires_at.is_none()
                })
                .count()
                >= self.active_limit
        {
            return Err(AdmissionError::Capacity);
        }
        spent.insert(fingerprint);
        self.entries.insert(
            (binding.device.clone(), binding.request.clone()),
            Entry {
                binding,
                phase: Phase::Pending,
                expires_at: None,
                sealed_seq: None,
            },
        );
        Ok(())
    }

    pub fn accept(&mut self, binding: &TurnBinding) -> Result<(), AdmissionError> {
        let entry = self.exact_mut(binding)?;
        if entry.phase != Phase::Pending {
            return Err(AdmissionError::Revoked);
        }
        entry.phase = Phase::Accepted;
        Ok(())
    }

    /// Omitting turn supports cancellation before acceptance reaches the client.
    pub fn cancel(
        &mut self,
        authenticated_device: &str,
        request: &str,
        target_node: &str,
        target_agent: &str,
        conversation: &str,
        turn: Option<&str>,
    ) -> Result<TurnBinding, AdmissionError> {
        let entry = self
            .entries
            .get_mut(&(authenticated_device.into(), request.into()))
            .ok_or(AdmissionError::UnknownOrWrongScope)?;
        let binding = &entry.binding;
        if binding.target_node != target_node
            || binding.target_agent != target_agent
            || binding.conversation != conversation
            || turn.is_some_and(|id| id != binding.turn)
        {
            return Err(AdmissionError::UnknownOrWrongScope);
        }
        entry.phase = Phase::Revoked;
        // Repeating cancellation never renews an existing terminal expiry.
        Ok(binding.clone())
    }

    pub fn may_publish(&self, binding: &TurnBinding) -> bool {
        self.entries
            .get(&(binding.device.clone(), binding.request.clone()))
            .is_some_and(|entry| entry.binding == *binding && entry.phase == Phase::Accepted)
    }

    /// Admission for new events/attempts. Missing managed entries fail closed.
    pub fn may_publish_turn(&self, device: &str, turn: &str) -> bool {
        self.entry_for_turn(device, turn)
            .map_or(!turn.starts_with(MANAGED_TURN_PREFIX), |entry| {
                entry.phase == Phase::Accepted
            })
    }

    /// Already-admitted frames may finish delivery/replay up to the terminal
    /// cutoff. Revocation denies even those frames. Expired managed IDs deny.
    pub fn may_deliver_turn(&self, device: &str, turn: &str, seq: u64) -> bool {
        self.entry_for_turn(device, turn)
            .map_or(!turn.starts_with(MANAGED_TURN_PREFIX), |entry| {
                entry.phase == Phase::Accepted
                    || (entry.phase == Phase::Finished
                        && entry.sealed_seq.is_some_and(|cutoff| seq <= cutoff))
            })
    }

    /// Call only after the relay has terminated, or cancellation has been
    /// confirmed by the runtime adapter. Failed/hanging cancellation is active
    /// revoked metadata and must not be evicted by a timer.
    pub fn finish(&mut self, binding: &TurnBinding) -> Result<(), AdmissionError> {
        let expiry = self.now_ms.saturating_add(self.ttl_ms);
        let entry = self.exact_mut(binding)?;
        if entry.phase != Phase::Revoked {
            entry.phase = Phase::Finished;
        }
        entry.expires_at.get_or_insert(expiry);
        self.trim_terminal(&binding.device);
        Ok(())
    }

    /// Seal only after retaining/sending the complete terminal event batch.
    pub fn seal(&mut self, device: &str, turn: &str, cutoff: u64) {
        let binding = self
            .entry_for_turn(device, turn)
            .map(|entry| entry.binding.clone());
        if let Some(binding) = binding {
            if let Ok(entry) = self.exact_mut(&binding) {
                entry.sealed_seq.get_or_insert(cutoff);
            }
            let _ = self.finish(&binding);
        }
    }

    fn entry_for_turn(&self, device: &str, turn: &str) -> Option<&Entry> {
        self.entries
            .values()
            .find(|entry| entry.binding.device == device && entry.binding.turn == turn)
    }
    fn exact_mut(&mut self, binding: &TurnBinding) -> Result<&mut Entry, AdmissionError> {
        self.entries
            .get_mut(&(binding.device.clone(), binding.request.clone()))
            .filter(|entry| entry.binding == *binding)
            .ok_or(AdmissionError::UnknownOrWrongScope)
    }
    fn trim_terminal(&mut self, device: &str) {
        let mut terminals: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.binding.device == device && entry.expires_at.is_some())
            .map(|(key, entry)| (entry.expires_at.unwrap(), key.clone()))
            .collect();
        terminals.sort();
        let excess = terminals.len().saturating_sub(self.terminal_limit);
        for (_, key) in terminals.into_iter().take(excess) {
            self.entries.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding(request: &str) -> TurnBinding {
        TurnBinding {
            device: "verified-device".into(),
            request: request.into(),
            target_node: "hotel".into(),
            target_agent: "philote".into(),
            conversation: "conversation".into(),
            turn: format!("{MANAGED_TURN_PREFIX}{request}"),
        }
    }
    fn cancel(ledger: &mut TurnBindings, b: &TurnBinding) -> Result<TurnBinding, AdmissionError> {
        ledger.cancel(
            &b.device,
            &b.request,
            &b.target_node,
            &b.target_agent,
            &b.conversation,
            None,
        )
    }
    #[test]
    fn cancellation_before_acceptance_never_resurrects() {
        let mut ledger = TurnBindings::new(8);
        let b = binding("one");
        ledger.reserve(b.clone()).unwrap();
        cancel(&mut ledger, &b).unwrap();
        assert_eq!(ledger.accept(&b), Err(AdmissionError::Revoked));
        ledger.finish(&b).unwrap();
        assert!(!ledger.may_publish(&b));
    }
    #[test]
    fn wrong_device_target_conversation_and_turn_are_denied() {
        let mut ledger = TurnBindings::new(8);
        let b = binding("one");
        ledger.reserve(b.clone()).unwrap();
        ledger.accept(&b).unwrap();
        for args in [
            ("other", "hotel", "philote", "conversation", None),
            ("verified-device", "other", "philote", "conversation", None),
            ("verified-device", "hotel", "other", "conversation", None),
            ("verified-device", "hotel", "philote", "other", None),
            (
                "verified-device",
                "hotel",
                "philote",
                "conversation",
                Some("wrong"),
            ),
        ] {
            assert_eq!(
                ledger.cancel(args.0, "one", args.1, args.2, args.3, args.4),
                Err(AdmissionError::UnknownOrWrongScope)
            );
        }
        assert!(ledger.may_publish(&b));
    }
    #[test]
    fn stale_chunks_and_late_final_cannot_reopen_an_interrupted_turn() {
        let mut ledger = TurnBindings::new(8);
        let old = binding("old");
        let new = binding("new");
        ledger.reserve(old.clone()).unwrap();
        ledger.accept(&old).unwrap();
        cancel(&mut ledger, &old).unwrap();
        ledger.reserve(new.clone()).unwrap();
        ledger.accept(&new).unwrap();
        ledger.finish(&old).unwrap();
        assert!(!ledger.may_publish(&old));
        assert!(ledger.may_publish(&new));
    }
    #[test]
    fn duplicate_and_capacity_cannot_replace_tombstones() {
        let mut ledger = TurnBindings::new(1);
        let b = binding("one");
        ledger.reserve(b.clone()).unwrap();
        cancel(&mut ledger, &b).unwrap();
        assert_eq!(ledger.reserve(b), Err(AdmissionError::DuplicateRequest));
        assert_eq!(
            ledger.reserve(binding("two")),
            Err(AdmissionError::Capacity)
        );
    }
    #[test]
    fn cancellation_does_not_undo_committed_tools() {
        let committed_tools = vec!["synthetic committed tool"];
        let mut ledger = TurnBindings::new(8);
        let b = binding("one");
        ledger.reserve(b.clone()).unwrap();
        ledger.accept(&b).unwrap();
        cancel(&mut ledger, &b).unwrap();
        assert_eq!(committed_tools, vec!["synthetic committed tool"]);
        assert!(!ledger.may_publish(&b));
    }
    #[test]
    fn more_than_256_completed_and_failed_submissions_release_active_capacity() {
        let mut ledger = TurnBindings::with_limits(2, 3, 1024, 10);
        let active = binding("still-active");
        ledger.reserve(active.clone()).unwrap();
        ledger.accept(&active).unwrap();
        for i in 0..600 {
            let completed = binding(&format!("completed-{i}"));
            ledger.reserve(completed.clone()).unwrap();
            if i % 2 == 0 {
                ledger.accept(&completed).unwrap();
                ledger.seal(&completed.device, &completed.turn, i + 1);
            } else {
                ledger.finish(&completed).unwrap();
            }
            assert!(ledger.entries.len() <= 4); // one active plus three terminals
        }
        assert!(ledger.may_publish(&active));
        assert!(!ledger.may_publish_turn(&active.device, &binding("completed-0").turn));
        assert_eq!(
            ledger.reserve(binding("completed-0")),
            Err(AdmissionError::DuplicateRequest)
        );
    }

    #[test]
    fn completed_replay_is_sealed_and_expired_managed_chunks_never_become_legacy() {
        let mut ledger = TurnBindings::with_limits(2, 2, 8, 10);
        let b = binding("completed");
        ledger.reserve(b.clone()).unwrap();
        ledger.accept(&b).unwrap();
        ledger.seal(&b.device, &b.turn, 15);
        assert!(!ledger.may_publish_turn(&b.device, &b.turn));
        assert!(ledger.may_deliver_turn(&b.device, &b.turn, 15));
        assert!(!ledger.may_deliver_turn(&b.device, &b.turn, 16));
        ledger.advance(10);
        assert!(!ledger.may_deliver_turn(&b.device, &b.turn, 15));
        assert!(!ledger.may_publish_turn(&b.device, &b.turn));
        assert_eq!(
            ledger.reserve(b.clone()),
            Err(AdmissionError::DuplicateRequest)
        );
        assert!(ledger.may_publish_turn(&b.device, "operator-chat-turn-legacy"));
    }

    #[test]
    fn clock_rollback_forward_jump_and_failed_cancel_never_evict_active_turns() {
        let mut ledger = TurnBindings::with_limits(2, 2, 8, 10);
        ledger.advance(100);
        let active = binding("active");
        let done = binding("done");
        ledger.reserve(active.clone()).unwrap();
        ledger.accept(&active).unwrap();
        cancel(&mut ledger, &active).unwrap(); // adapter has not confirmed stop
        ledger.reserve(done.clone()).unwrap();
        ledger.accept(&done).unwrap();
        ledger.seal(&done.device, &done.turn, 1);
        ledger.advance(90);
        assert_eq!(ledger.now_ms, 100);
        ledger.advance(109);
        assert!(ledger.may_deliver_turn(&done.device, &done.turn, 1));
        ledger.advance(u64::MAX);
        assert!(!ledger.may_deliver_turn(&done.device, &done.turn, 1));
        assert!(ledger
            .entries
            .contains_key(&(active.device.clone(), active.request.clone())));
        assert!(ledger.accept(&active).is_err());
    }

    #[test]
    fn repeated_cancel_does_not_extend_confirmed_terminal_expiry() {
        let mut ledger = TurnBindings::with_limits(2, 2, 8, 10);
        let b = binding("cancelled");
        ledger.reserve(b.clone()).unwrap();
        ledger.accept(&b).unwrap();
        cancel(&mut ledger, &b).unwrap();
        ledger.finish(&b).unwrap();
        ledger.advance(9);
        cancel(&mut ledger, &b).unwrap();
        ledger.finish(&b).unwrap();
        ledger.advance(10);
        assert_eq!(
            cancel(&mut ledger, &b),
            Err(AdmissionError::UnknownOrWrongScope)
        );
        assert!(!ledger.may_deliver_turn(&b.device, &b.turn, 0));
    }

    #[test]
    fn exhausted_compact_tombstones_fail_closed_without_eviction() {
        let mut ledger = TurnBindings::with_limits(2, 1, 2, 1);
        for id in ["one", "two"] {
            let b = binding(id);
            ledger.reserve(b.clone()).unwrap();
            ledger.finish(&b).unwrap();
        }
        ledger.advance(100);
        assert!(ledger.entries.is_empty());
        assert_eq!(
            ledger.reserve(binding("three")),
            Err(AdmissionError::Capacity)
        );
        assert_eq!(
            ledger.reserve(binding("one")),
            Err(AdmissionError::DuplicateRequest)
        );
    }
}
