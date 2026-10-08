//! Admission seam for authenticated voice cancellation.
//!
//! The ledger is installed for correlated edge submissions, but its runtime
//! cancellation adapter is absent pending trusted hotel authority. Never advertise
//! `turn_cancel_v1` merely because this ledger can suppress outgoing audio.

use std::collections::HashMap;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Pending,
    Accepted,
    Revoked,
    Finished,
}

/// Bounded metadata only; contains neither content nor credentials.
pub(super) struct TurnBindings {
    entries: HashMap<(String, String), (TurnBinding, Phase)>,
    per_device_limit: usize,
}

impl TurnBindings {
    /// Untracked legacy events remain compatible. A known revoked/finished turn
    /// cannot publish even when it was already queued before cancellation.
    pub fn may_publish_turn(&self, device: &str, turn: &str) -> bool {
        self.entries
            .values()
            .find(|(binding, _)| binding.device == device && binding.turn == turn)
            .is_none_or(|(_, phase)| *phase == Phase::Accepted)
    }
    pub fn new(per_device_limit: usize) -> Self {
        Self {
            entries: HashMap::new(),
            per_device_limit,
        }
    }

    /// Reserve before dispatch. Request IDs remain tombstoned for the session;
    /// capacity rejects rather than evicting a revoked ID and permitting reuse.
    pub fn reserve(&mut self, binding: TurnBinding) -> Result<(), AdmissionError> {
        let key = (binding.device.clone(), binding.request.clone());
        if self.entries.contains_key(&key) {
            return Err(AdmissionError::DuplicateRequest);
        }
        if self
            .entries
            .keys()
            .filter(|(device, _)| device == &binding.device)
            .count()
            >= self.per_device_limit
        {
            return Err(AdmissionError::Capacity);
        }
        self.entries.insert(key, (binding, Phase::Pending));
        Ok(())
    }

    /// Queue the correlated accepted event before starting the provider job.
    pub fn accept(&mut self, binding: &TurnBinding) -> Result<(), AdmissionError> {
        let (_, phase) = self.exact_mut(binding)?;
        match phase {
            Phase::Pending => {
                *phase = Phase::Accepted;
                Ok(())
            }
            _ => Err(AdmissionError::Revoked),
        }
    }

    /// Authenticate the caller independently and resolve its immutable binding.
    /// Omitting turn supports a cancel sent before acceptance reaches the client.
    pub fn cancel(
        &mut self,
        authenticated_device: &str,
        request: &str,
        target_node: &str,
        target_agent: &str,
        conversation: &str,
        turn: Option<&str>,
    ) -> Result<TurnBinding, AdmissionError> {
        let (binding, phase) = self
            .entries
            .get_mut(&(authenticated_device.into(), request.into()))
            .ok_or(AdmissionError::UnknownOrWrongScope)?;
        if binding.target_node != target_node
            || binding.target_agent != target_agent
            || binding.conversation != conversation
            || turn.is_some_and(|id| id != binding.turn)
        {
            return Err(AdmissionError::UnknownOrWrongScope);
        }
        // Revoke before asking the runtime adapter, including when that adapter
        // fails. Retrying cancellation may repeat the immutable binding safely.
        *phase = Phase::Revoked;
        Ok(binding.clone())
    }

    /// Check immediately before every provider attempt and outgoing chunk.
    pub fn may_publish(&self, binding: &TurnBinding) -> bool {
        self.entries
            .get(&(binding.device.clone(), binding.request.clone()))
            .is_some_and(|(actual, phase)| actual == binding && *phase == Phase::Accepted)
    }

    pub fn finish(&mut self, binding: &TurnBinding) -> Result<(), AdmissionError> {
        let (_, phase) = self.exact_mut(binding)?;
        if *phase != Phase::Revoked {
            *phase = Phase::Finished;
        }
        Ok(())
    }

    fn exact_mut(
        &mut self,
        binding: &TurnBinding,
    ) -> Result<&mut (TurnBinding, Phase), AdmissionError> {
        self.entries
            .get_mut(&(binding.device.clone(), binding.request.clone()))
            .filter(|(actual, _)| actual == binding)
            .ok_or(AdmissionError::UnknownOrWrongScope)
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
            turn: format!("turn-{request}"),
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
}
