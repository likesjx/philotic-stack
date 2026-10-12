//! Concrete local authenticated-delivery bridge to the canonical adapter.
//! Server root/evidence authority must be supplied; no runtime issuer installed.
use super::capture_commit::{CommitAuthority, CommitLease, CommitPayload};
use super::resolve_plan::*;
use ansible_mesh_core::privacy::{ProcessingOperation, ProviderBoundary};
use ansible_mesh_core::privacy_local::{
    LocalTaskAuthority, LocalTaskEnvelope, VerifiedLocalSession,
};
use ansible_mesh_core::privacy_storage::PolicyStore;
use anyhow::{Result, bail};
use async_trait::async_trait;
use std::sync::Arc;

pub struct LocalCaptureAuthority {
    pub tasks: Arc<LocalTaskAuthority>,
    pub consumer: Arc<VerifiedLocalSession>,
    pub envelope: LocalTaskEnvelope,
    pub inbox: Arc<PolicyStore>,
    /// Existing server root catalog/write authority, never caller root claims.
    pub roots: Arc<dyn PlanningAuthority + Send + Sync>,
}
impl PlanningAuthority for LocalCaptureAuthority {
    fn authorize_capture(&self, capture: &Capture) -> std::result::Result<String, Denial> {
        let resolved = self
            .tasks
            .resolve(
                &self.envelope,
                &self.consumer,
                ProcessingOperation::SemanticResolution,
                ProviderBoundary::LocalTrusted,
            )
            .map_err(|_| Denial::AccessDenied)?;
        if resolved.payload_digest != capture.payload_digest {
            return Err(Denial::InvalidInput);
        }
        self.roots.authorize_capture(capture)?;
        Ok(resolved.policies.revision().to_string())
    }
    fn resolve(&self, anchor: &RootAnchor) -> std::result::Result<Lookup, Denial> {
        self.roots.resolve(anchor)
    }
    fn can_read_root(&self, id: &RootId) -> bool {
        self.roots.can_read_root(id)
    }
    fn can_write(&self, target: &WriteTarget) -> bool {
        self.roots.can_write(target)
    }
}
#[async_trait]
impl CommitAuthority for LocalCaptureAuthority {
    async fn acquire(&self, capture: &Capture, payload: &CommitPayload) -> Result<CommitLease> {
        let (resolved, lease) = self.tasks.pin_capture(&self.envelope, &self.consumer)?;
        if !lease.belongs_to(&self.inbox) {
            bail!("capture inbox and policy authority differ");
        }
        let stored = lease.load_pending(
            &resolved.actor,
            &capture.event.producer,
            &capture.event.event_id,
        )?;
        let supplied = payload.pending();
        if stored.payload != self.envelope.payload
            || supplied.payload != stored.payload
            || supplied.sources != stored.sources
            || stored.sources != resolved.sources
            || supplied.recorded_by != stored.recorded_by
            || stored.recorded_by != resolved.actor.stable_agent_id()
            || resolved.payload_digest != capture.payload_digest
        {
            bail!("capture payload/provenance binding mismatch");
        }
        Ok(CommitLease {
            actor: resolved.actor,
            revision: lease.revision().to_string(),
            hold: lease,
        })
    }
    fn validate_lease(&self, lease: &CommitLease) -> Result<()> {
        self.tasks
            .validate_pinned_capture(&self.envelope, &self.consumer, &lease.hold)?;
        Ok(())
    }
}
