//! Credential pools — same-provider API-key rotation (Model Failover Layers, Layer 1).
//!
//! A pool holds an ordered ring of key members for one provider. When a request
//! fails with an auth or rate-limit error, the failing member enters a cooldown
//! and the request retries on the next eligible member — *before* the failure is
//! allowed to surface as a tier-worthy error to philote.
//!
//! Pool state (cooldowns, error counts) lives for the lifetime of the controller
//! process, outside the per-task config reload. Member plaintexts are re-resolved
//! on every `ProviderConfigs::load`, so members are identified by stable source
//! labels rather than by key value.
//!
//! Member order: env break-glass first (preserves the existing env-override
//! precedence), then the scalar `<provider>_api_key_ref` member, then each entry
//! of `<provider>_api_key_pool`. An env-sourced member never touches the vault
//! cipher, so it survives a master-key mismatch — the 2026-07-04 incident class.

use std::time::{Duration, Instant};

/// Why a member is being rotated away from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationTrigger {
    /// HTTP 429 / quota. Cooldown escalates 30s → 60s → 300s cap.
    RateLimit,
    /// HTTP 401/403, expired/invalid key, or vault decrypt failure.
    /// Rotate immediately; long cooldown (re-probe after 5 min).
    AuthFailure,
    /// Credit exhaustion. Member is disabled on an hours-scale lane.
    Billing,
}

impl RotationTrigger {
    /// Map a classified provider failure `sub_kind` to a rotation trigger.
    /// Only auth and rate-limit failures rotate; network/streaming/5xx errors
    /// are provider-side, not key-side, and follow the existing retry path.
    pub fn from_sub_kind(sub_kind: Option<&str>) -> Option<Self> {
        match sub_kind {
            Some("provider_auth") => Some(Self::AuthFailure),
            Some("rate_limit") => Some(Self::RateLimit),
            _ => None,
        }
    }
}

/// Reason a member is out of rotation indefinitely (not a timed cooldown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledReason {
    Billing,
}

/// Where a member's plaintext comes from on each refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberSource {
    /// Process env var (e.g. `PHILOTIC_GEMINI_API_KEY`). Break-glass: never
    /// touches the vault cipher.
    Env(String),
    /// Vault secret ref (`secret://…`), resolved via GetSecret IPC.
    SecretRef(String),
}

/// Cooldown escalation for rate limits, keyed by consecutive error count
/// (OpenClaw schedule): 30s → 60s → 300s cap.
const RATE_LIMIT_COOLDOWNS_SECS: [u64; 3] = [30, 60, 300];
/// Auth failures cool for 5 minutes — long enough to stop hammering a revoked
/// key, short enough that a rotated-back-in key is picked up without restart.
const AUTH_COOLDOWN_SECS: u64 = 300;
/// Consecutive-failure window. Errors older than this reset the escalation.
const FAILURE_WINDOW_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Default)]
pub struct CooldownState {
    pub until: Option<Instant>,
    pub error_count: u32,
    pub last_failure: Option<Instant>,
    pub disabled_reason: Option<DisabledReason>,
}

impl CooldownState {
    fn is_cooling(&self, now: Instant) -> bool {
        self.disabled_reason.is_some() || self.until.map(|t| t > now).unwrap_or(false)
    }

    fn record_failure(&mut self, trigger: RotationTrigger, now: Instant) {
        // Reset escalation if the last failure is outside the window.
        if let Some(last) = self.last_failure
            && now.duration_since(last) > Duration::from_secs(FAILURE_WINDOW_SECS)
        {
            self.error_count = 0;
        }
        self.error_count = self.error_count.saturating_add(1);
        self.last_failure = Some(now);
        match trigger {
            RotationTrigger::RateLimit => {
                let idx = (self.error_count.saturating_sub(1) as usize)
                    .min(RATE_LIMIT_COOLDOWNS_SECS.len() - 1);
                self.until = Some(now + Duration::from_secs(RATE_LIMIT_COOLDOWNS_SECS[idx]));
            }
            RotationTrigger::AuthFailure => {
                self.until = Some(now + Duration::from_secs(AUTH_COOLDOWN_SECS));
            }
            RotationTrigger::Billing => {
                self.disabled_reason = Some(DisabledReason::Billing);
                self.until = None;
            }
        }
    }

    fn record_success(&mut self) {
        self.until = None;
        self.error_count = 0;
        self.last_failure = None;
        // A successful request on a billing-disabled member means the operator
        // re-funded it — re-admit it to rotation.
        self.disabled_reason = None;
    }
}

#[derive(Debug, Clone)]
pub struct PoolMember {
    /// Stable identity across refreshes ("env", "primary", "pool[0]", …).
    pub label: String,
    pub source: MemberSource,
    /// Resolved key plaintext; `None` when the last refresh failed to resolve
    /// this member (missing config, vault decrypt failure, ACL denial).
    pub plaintext: Option<String>,
    pub cooldown: CooldownState,
}

/// Ordered key ring for one provider. `pinned` is the last-good member index —
/// requests stick to it (provider-side prompt caches stay warm) until it fails.
#[derive(Debug, Clone)]
pub struct CredentialPool {
    pub provider: String,
    pub members: Vec<PoolMember>,
    pub pinned: Option<usize>,
}

impl CredentialPool {
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            members: Vec::new(),
            pinned: None,
        }
    }

    /// Reconcile the member list against freshly-resolved sources, preserving
    /// cooldown state for members whose label survives the refresh.
    pub fn reconcile(&mut self, resolved: Vec<(String, MemberSource, Option<String>)>) {
        let mut next: Vec<PoolMember> = Vec::with_capacity(resolved.len());
        for (label, source, plaintext) in resolved {
            let cooldown = self
                .members
                .iter()
                .find(|m| m.label == label)
                .map(|m| m.cooldown.clone())
                .unwrap_or_default();
            next.push(PoolMember {
                label,
                source,
                plaintext,
                cooldown,
            });
        }
        // Re-anchor the pin by label; a vanished member unpins.
        self.pinned = self.pinned.and_then(|old_idx| {
            let old_label = self.members.get(old_idx).map(|m| m.label.clone())?;
            next.iter().position(|m| m.label == old_label)
        });
        self.members = next;
    }

    /// Record that a member failed to resolve at refresh time (vault decrypt
    /// failure, ACL denial). Counts as an auth failure so rotation skips it.
    pub fn mark_resolution_failed(&mut self, label: &str) {
        let now = Instant::now();
        if let Some(m) = self.members.iter_mut().find(|m| m.label == label) {
            m.cooldown.record_failure(RotationTrigger::AuthFailure, now);
        }
    }

    /// The member a new request should use: the pinned member if still
    /// eligible, otherwise the first eligible member in ring order.
    pub fn active_member(&self) -> Option<(usize, &str)> {
        let now = Instant::now();
        let eligible = |idx: usize| -> Option<(usize, &str)> {
            let m = self.members.get(idx)?;
            if m.cooldown.is_cooling(now) {
                return None;
            }
            m.plaintext.as_deref().map(|k| (idx, k))
        };
        if let Some(pinned) = self.pinned
            && let Some(hit) = eligible(pinned)
        {
            return Some(hit);
        }
        (0..self.members.len()).find_map(eligible)
    }

    /// Mark `idx` failed and return the next eligible member, pinning it.
    /// Returns `None` when the pool is exhausted — the caller must surface the
    /// original provider error exactly as before pools existed.
    pub fn rotate_on_failure(
        &mut self,
        idx: usize,
        trigger: RotationTrigger,
    ) -> Option<(usize, String)> {
        let now = Instant::now();
        if let Some(m) = self.members.get_mut(idx) {
            m.cooldown.record_failure(trigger, now);
        }
        let n = self.members.len();
        // Walk the ring starting after the failed member.
        for step in 1..=n {
            let candidate = (idx + step) % n;
            let m = &self.members[candidate];
            if m.cooldown.is_cooling(now) {
                continue;
            }
            if let Some(key) = m.plaintext.clone() {
                self.pinned = Some(candidate);
                return Some((candidate, key));
            }
        }
        None
    }

    /// Record a successful request on `idx`: clear its cooldown and pin it.
    pub fn note_success(&mut self, idx: usize) {
        if let Some(m) = self.members.get_mut(idx) {
            m.cooldown.record_success();
        }
        self.pinned = Some(idx);
    }

    /// True when more than one member resolved — rotation has somewhere to go.
    pub fn has_alternates(&self) -> bool {
        self.members
            .iter()
            .filter(|m| m.plaintext.is_some())
            .count()
            > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(label: &str, key: Option<&str>) -> (String, MemberSource, Option<String>) {
        (
            label.to_string(),
            MemberSource::SecretRef(format!("secret://hotel/default/api_key/{label}")),
            key.map(str::to_string),
        )
    }

    fn pool_with(members: Vec<(String, MemberSource, Option<String>)>) -> CredentialPool {
        let mut pool = CredentialPool::new("gemini");
        pool.reconcile(members);
        pool
    }

    #[test]
    fn active_member_prefers_first_eligible_in_ring_order() {
        let pool = pool_with(vec![
            member("env", Some("k-env")),
            member("primary", Some("k-prim")),
        ]);
        assert_eq!(pool.active_member(), Some((0, "k-env")));
    }

    #[test]
    fn active_member_skips_unresolved_members() {
        let pool = pool_with(vec![member("env", None), member("primary", Some("k-prim"))]);
        assert_eq!(pool.active_member(), Some((1, "k-prim")));
    }

    #[test]
    fn rotate_on_auth_failure_moves_to_next_member_and_pins_it() {
        let mut pool = pool_with(vec![
            member("primary", Some("k-prim")),
            member("pool[0]", Some("k-backup")),
        ]);
        let next = pool.rotate_on_failure(0, RotationTrigger::AuthFailure);
        assert_eq!(next, Some((1, "k-backup".to_string())));
        assert_eq!(pool.pinned, Some(1));
        // The failed member is cooling — a new request also lands on the backup.
        assert_eq!(pool.active_member(), Some((1, "k-backup")));
    }

    #[test]
    fn rotate_returns_none_when_pool_exhausted() {
        let mut pool = pool_with(vec![member("primary", Some("k-prim"))]);
        assert_eq!(
            pool.rotate_on_failure(0, RotationTrigger::AuthFailure),
            None
        );
        // And with a second member already cooling:
        let mut pool = pool_with(vec![
            member("primary", Some("k-prim")),
            member("pool[0]", Some("k-backup")),
        ]);
        assert!(
            pool.rotate_on_failure(0, RotationTrigger::AuthFailure)
                .is_some()
        );
        assert_eq!(
            pool.rotate_on_failure(1, RotationTrigger::AuthFailure),
            None
        );
    }

    #[test]
    fn rate_limit_cooldown_escalates_30_60_300() {
        let mut cd = CooldownState::default();
        let t0 = Instant::now();
        cd.record_failure(RotationTrigger::RateLimit, t0);
        assert_eq!(cd.until, Some(t0 + Duration::from_secs(30)));
        cd.record_failure(RotationTrigger::RateLimit, t0);
        assert_eq!(cd.until, Some(t0 + Duration::from_secs(60)));
        cd.record_failure(RotationTrigger::RateLimit, t0);
        assert_eq!(cd.until, Some(t0 + Duration::from_secs(300)));
        // Capped at 300.
        cd.record_failure(RotationTrigger::RateLimit, t0);
        assert_eq!(cd.until, Some(t0 + Duration::from_secs(300)));
    }

    #[test]
    fn billing_disables_member_until_manual_clear() {
        let mut pool = pool_with(vec![
            member("primary", Some("k-prim")),
            member("pool[0]", Some("k-backup")),
        ]);
        pool.rotate_on_failure(0, RotationTrigger::Billing);
        assert_eq!(
            pool.members[0].cooldown.disabled_reason,
            Some(DisabledReason::Billing)
        );
        // Success on the disabled member clears the lane (manual re-adoption).
        pool.note_success(0);
        assert_eq!(pool.members[0].cooldown.disabled_reason, None);
    }

    #[test]
    fn decrypt_failure_at_refresh_counts_as_auth_failure() {
        let mut pool = pool_with(vec![
            member("primary", Some("stale-key")),
            member("env", Some("k-env")),
        ]);
        // Refresh where the vault member fails to resolve (wrong master key):
        pool.reconcile(vec![member("primary", None), member("env", Some("k-env"))]);
        pool.mark_resolution_failed("primary");
        // The env break-glass member carries the provider.
        assert_eq!(pool.active_member(), Some((1, "k-env")));
        assert!(pool.members[0].cooldown.is_cooling(Instant::now()));
    }

    #[test]
    fn reconcile_preserves_cooldowns_by_label_and_reanchors_pin() {
        let mut pool = pool_with(vec![
            member("primary", Some("k-prim")),
            member("pool[0]", Some("k-backup")),
        ]);
        pool.rotate_on_failure(0, RotationTrigger::RateLimit);
        assert_eq!(pool.pinned, Some(1));
        // Refresh reorders members; cooldown and pin must follow labels.
        pool.reconcile(vec![
            member("pool[0]", Some("k-backup")),
            member("primary", Some("k-prim")),
        ]);
        assert_eq!(pool.pinned, Some(0)); // pool[0] moved to index 0
        assert!(pool.members[1].cooldown.is_cooling(Instant::now())); // primary still cooling
        assert_eq!(pool.active_member(), Some((0, "k-backup")));
    }

    #[test]
    fn success_clears_cooldown_and_pins() {
        let mut pool = pool_with(vec![
            member("primary", Some("k-prim")),
            member("pool[0]", Some("k-backup")),
        ]);
        pool.rotate_on_failure(0, RotationTrigger::RateLimit);
        pool.note_success(0);
        assert_eq!(pool.pinned, Some(0));
        assert_eq!(pool.active_member(), Some((0, "k-prim")));
    }

    #[test]
    fn rotation_trigger_maps_only_auth_and_rate_limit() {
        assert_eq!(
            RotationTrigger::from_sub_kind(Some("provider_auth")),
            Some(RotationTrigger::AuthFailure)
        );
        assert_eq!(
            RotationTrigger::from_sub_kind(Some("rate_limit")),
            Some(RotationTrigger::RateLimit)
        );
        assert_eq!(RotationTrigger::from_sub_kind(Some("network_error")), None);
        assert_eq!(
            RotationTrigger::from_sub_kind(Some("streaming_timeout")),
            None
        );
        assert_eq!(RotationTrigger::from_sub_kind(None), None);
    }
}
