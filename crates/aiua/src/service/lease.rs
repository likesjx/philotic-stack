use philotic_client::{LeaseEnvelope, LeaseStatus};
use std::collections::HashMap;
use uuid::Uuid;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseObserverEventKind {
    Granted,
    Renewed,
    Released,
    Revoked,
    Expired,
    StaleOwnerDropped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseObserverEvent {
    pub kind: LeaseObserverEventKind,
    pub lease: LeaseEnvelope,
}

pub trait LeaseObserver {
    fn on_event(&mut self, event: &LeaseObserverEvent);
}

#[allow(dead_code)]
#[derive(Default)]
pub struct NoopLeaseObserver;

impl LeaseObserver for NoopLeaseObserver {
    fn on_event(&mut self, _event: &LeaseObserverEvent) {}
}

pub trait LeaseProvider {
    fn acquire(
        &mut self,
        owner_conn_id: Uuid,
        candidate: LeaseEnvelope,
        ttl_secs: u64,
        now: u64,
        observer: &mut dyn LeaseObserver,
    ) -> LeaseAcquireOutcome;

    fn renew(
        &mut self,
        lease_scope: &str,
        owner_conn_id: Uuid,
        lease_epoch: u64,
        ttl_secs: u64,
        now: u64,
        observer: &mut dyn LeaseObserver,
    ) -> LeaseRenewOutcome;

    fn release(
        &mut self,
        lease_scope: &str,
        owner_conn_id: Uuid,
        observer: &mut dyn LeaseObserver,
    ) -> Option<LeaseEnvelope>;

    fn inspect(&self, lease_scope: &str) -> Option<LeaseEnvelope>;
}

#[derive(Debug, Clone)]
struct ActiveLease {
    conn_id: Uuid,
    envelope: LeaseEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseAcquireOutcome {
    Granted(LeaseEnvelope),
    Denied(LeaseEnvelope),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseRenewOutcome {
    Renewed(LeaseEnvelope),
    Lost(Option<LeaseEnvelope>),
}

#[derive(Debug, Default)]
pub struct RuntimeLeaseRegistry {
    active: HashMap<String, ActiveLease>,
    next_epoch: HashMap<String, u64>,
}

impl RuntimeLeaseRegistry {
    pub fn active_leases_for_connection(&self, conn_id: Uuid) -> Vec<LeaseEnvelope> {
        self.active
            .values()
            .filter(|lease| lease.conn_id == conn_id)
            .map(|lease| lease.envelope.clone())
            .collect()
    }

    pub fn drop_if_stale<F>(
        &mut self,
        lease_scope: &str,
        mut is_stale: F,
        observer: &mut dyn LeaseObserver,
    ) -> Option<LeaseEnvelope>
    where
        F: FnMut(&LeaseEnvelope) -> bool,
    {
        let should_remove = self
            .active
            .get(lease_scope)
            .is_some_and(|lease| is_stale(&lease.envelope));
        if !should_remove {
            return None;
        }
        let removed = self.active.remove(lease_scope).map(|lease| lease.envelope);
        if let Some(ref envelope) = removed {
            let kind = if envelope.lease_expires_at > 0 && !envelope.is_active() {
                LeaseObserverEventKind::Expired
            } else {
                LeaseObserverEventKind::StaleOwnerDropped
            };
            observer.on_event(&LeaseObserverEvent {
                kind,
                lease: envelope.clone(),
            });
        }
        removed
    }

    #[allow(dead_code)]
    pub fn mark_expired(
        &mut self,
        lease_scope: &str,
        observer: &mut dyn LeaseObserver,
    ) -> Option<LeaseEnvelope> {
        let mut removed = self.active.remove(lease_scope)?.envelope;
        removed.status = LeaseStatus::Expired;
        observer.on_event(&LeaseObserverEvent {
            kind: LeaseObserverEventKind::Expired,
            lease: removed.clone(),
        });
        Some(removed)
    }
}

impl LeaseProvider for RuntimeLeaseRegistry {
    fn acquire(
        &mut self,
        owner_conn_id: Uuid,
        mut candidate: LeaseEnvelope,
        ttl_secs: u64,
        now: u64,
        observer: &mut dyn LeaseObserver,
    ) -> LeaseAcquireOutcome {
        if let Some(existing) = self.active.get(&candidate.lease_scope) {
            if existing.conn_id == owner_conn_id {
                return LeaseAcquireOutcome::Granted(existing.envelope.clone());
            }
            return LeaseAcquireOutcome::Denied(existing.envelope.clone());
        }

        let next_epoch = self
            .next_epoch
            .entry(candidate.lease_scope.clone())
            .or_insert(0);
        *next_epoch += 1;
        candidate.lease_epoch = *next_epoch;
        candidate.lease_expires_at = now + ttl_secs;
        candidate.last_heartbeat_at = now;
        candidate.status = LeaseStatus::Active;
        self.active.insert(
            candidate.lease_scope.clone(),
            ActiveLease {
                conn_id: owner_conn_id,
                envelope: candidate.clone(),
            },
        );
        observer.on_event(&LeaseObserverEvent {
            kind: LeaseObserverEventKind::Granted,
            lease: candidate.clone(),
        });
        LeaseAcquireOutcome::Granted(candidate)
    }

    fn renew(
        &mut self,
        lease_scope: &str,
        owner_conn_id: Uuid,
        lease_epoch: u64,
        ttl_secs: u64,
        now: u64,
        observer: &mut dyn LeaseObserver,
    ) -> LeaseRenewOutcome {
        let Some(existing) = self.active.get_mut(lease_scope) else {
            return LeaseRenewOutcome::Lost(None);
        };
        if existing.conn_id != owner_conn_id || existing.envelope.lease_epoch != lease_epoch {
            return LeaseRenewOutcome::Lost(Some(existing.envelope.clone()));
        }
        existing.envelope.lease_expires_at = now + ttl_secs;
        existing.envelope.last_heartbeat_at = now;
        existing.envelope.status = LeaseStatus::Active;
        observer.on_event(&LeaseObserverEvent {
            kind: LeaseObserverEventKind::Renewed,
            lease: existing.envelope.clone(),
        });
        LeaseRenewOutcome::Renewed(existing.envelope.clone())
    }

    fn release(
        &mut self,
        lease_scope: &str,
        owner_conn_id: Uuid,
        observer: &mut dyn LeaseObserver,
    ) -> Option<LeaseEnvelope> {
        let existing = self.active.get(lease_scope)?;
        if existing.conn_id != owner_conn_id {
            return None;
        }
        let mut removed = self.active.remove(lease_scope)?.envelope;
        removed.status = LeaseStatus::Releasing;
        observer.on_event(&LeaseObserverEvent {
            kind: LeaseObserverEventKind::Released,
            lease: removed.clone(),
        });
        Some(removed)
    }

    fn inspect(&self, lease_scope: &str) -> Option<LeaseEnvelope> {
        self.active
            .get(lease_scope)
            .map(|lease| lease.envelope.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(scope: &str, owner: &str) -> LeaseEnvelope {
        LeaseEnvelope {
            lease_type: "telegram_poll".into(),
            lease_scope: scope.into(),
            authority_hotel: "hotel-a".into(),
            authority_component: Some("aiua".into()),
            owner_guest_id: owner.into(),
            owner_hotel: Some("hotel-a".into()),
            owner_component_type: Some("membrane".into()),
            lease_epoch: 0,
            lease_expires_at: 0,
            last_heartbeat_at: 0,
            status: LeaseStatus::Releasing,
            delegated_from: None,
            metadata: serde_json::json!({ "agent_id": "agent-a" }),
        }
    }

    #[test]
    fn acquire_renew_release_share_common_envelope() {
        let mut registry = RuntimeLeaseRegistry::default();
        let mut observer = NoopLeaseObserver;
        let conn = Uuid::new_v4();
        let granted = match registry.acquire(
            conn,
            candidate("scope-a", "guest-a"),
            30,
            100,
            &mut observer,
        ) {
            LeaseAcquireOutcome::Granted(lease) => lease,
            other => panic!("unexpected outcome: {other:?}"),
        };
        assert_eq!(granted.lease_epoch, 1);
        assert_eq!(granted.lease_expires_at, 130);
        assert!(granted.is_active());

        let renewed = match registry.renew("scope-a", conn, 1, 30, 110, &mut observer) {
            LeaseRenewOutcome::Renewed(lease) => lease,
            other => panic!("unexpected renew outcome: {other:?}"),
        };
        assert_eq!(renewed.lease_expires_at, 140);
        assert_eq!(renewed.last_heartbeat_at, 110);

        let released = registry
            .release("scope-a", conn, &mut observer)
            .expect("release should succeed");
        assert_eq!(released.status, LeaseStatus::Releasing);
        assert!(registry.inspect("scope-a").is_none());
    }
}
