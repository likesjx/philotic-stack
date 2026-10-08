//! Unexported integration with the EXISTING canonical LifeGraph transaction.
//! No production issuer, schema installation or runtime route is enabled here.

use super::resolve_plan::*;
use ansible_mesh_core::privacy::AuthenticatedAgent;
use ansible_mesh_core::privacy_storage::{
    CapturedRecord, PolicyCommitLease, capture_payload_digest,
};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureBody {
    pub summary: String,
    pub details: Option<String>,
    pub source_ids: Vec<String>,
    pub evidence_ids: BTreeSet<String>,
}

pub struct CommitPayload {
    pending: CapturedRecord,
    body: CaptureBody,
}
impl CommitPayload {
    pub fn from_pending(pending: CapturedRecord) -> Result<Self> {
        let mut body: CaptureBody = serde_json::from_str(&pending.payload)?;
        body.source_ids.sort();
        body.source_ids.dedup();
        if body.summary.trim().is_empty()
            || body.summary.len() > 16_384
            || body
                .details
                .as_ref()
                .is_some_and(|v| v.trim().is_empty() || v.len() > 16_384)
            || body.source_ids != pending.sources
            || body.evidence_ids.is_empty()
            || body.evidence_ids.len() > 256
        {
            bail!("invalid or incomplete capture body/manifest");
        }
        Ok(Self { pending, body })
    }
    pub fn pending(&self) -> &CapturedRecord {
        &self.pending
    }
}

/// Trusted server holds session/current policy and schema authority through
/// graph commit. A JSON assertion cannot issue this lease. Production issuance
/// is not installed. The concrete policy reservation must come from the same
/// SQLite authority database used for authorization; it survives through commit.
pub struct CommitLease {
    pub actor: AuthenticatedAgent,
    pub revision: String,
    pub hold: PolicyCommitLease,
}
#[async_trait]
pub trait CommitAuthority: PlanningAuthority + Send + Sync {
    async fn acquire(&self, capture: &Capture, payload: &CommitPayload) -> Result<CommitLease>;
    fn validate_lease(&self, lease: &CommitLease) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct SchemaState {
    pub transactional: bool,
    pub catalog_ready: bool,
    pub unique: BTreeSet<(String, String)>,
}
pub fn required_constraints(root_type: RootType) -> BTreeSet<(String, String)> {
    [
        (root_type.ontology_label(), "id"),
        ("LifeRootBinding", "key"),
        ("LifeCaptureReceipt", "key"),
        ("LifeCaptureEvidence", "key"),
        ("LifeCaptureExtension", "key"),
    ]
    .into_iter()
    .map(|(a, b)| (a.into(), b.into()))
    .collect()
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRecord {
    pub id: RootId,
    pub label: String,
}
#[derive(Debug, Clone)]
pub struct StoredReceipt {
    pub identity: String,
    pub root: Option<RootId>,
    pub kind: String,
}
#[derive(Debug, Clone)]
pub struct GraphWrite {
    pub event_key: String,
    pub identity: String,
    pub actor: String,
    pub revision: String,
    pub summary: String,
    pub details: Option<String>,
    pub sources_json: String,
    pub digest: String,
    pub evidence_ids: Vec<String>,
    pub disposition: Disposition,
    pub root_label: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitResult {
    pub root: Option<RootId>,
    pub kind: String,
    pub replay: bool,
}

/// Every read/check/write belongs to this same canonical graph transaction.
#[async_trait]
pub trait GraphTransaction: Send {
    async fn schema(&mut self) -> Result<SchemaState>;
    async fn receipt(&mut self, event_key: &str) -> Result<Option<StoredReceipt>>;
    async fn resolve(&mut self, anchor: &RootAnchor) -> Result<Lookup>;
    async fn roots(&mut self, id: &RootId) -> Result<Vec<RootRecord>>;
    async fn write(&mut self, write: &GraphWrite) -> Result<CommitResult>;
    async fn commit(self: Box<Self>) -> Result<()>;
    async fn rollback(self: Box<Self>) -> Result<()>;
}
#[async_trait]
pub trait AtomicLifeGraph: Send + Sync {
    async fn begin(&self) -> Result<Box<dyn GraphTransaction>>;
}
pub fn opaque_key(namespace: &str, value: &str) -> String {
    let hex = |s: &str| {
        s.as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    format!("{}.{}", hex(namespace), hex(value))
}

pub async fn commit_capture(
    graph: &dyn AtomicLifeGraph,
    authority: Option<&dyn CommitAuthority>,
    capture: &Capture,
    planned: &EventReceipt,
    payload: &CommitPayload,
) -> Result<CommitResult> {
    let authority = authority.ok_or_else(|| anyhow!("missing commit authority"))?;
    if payload.pending.producer != capture.event.producer
        || payload.pending.event_id != capture.event.event_id
        || capture_payload_digest(&payload.pending.payload) != capture.payload_digest
        || payload.body.evidence_ids != capture.evidence_ids
    {
        bail!("capture/event/digest binding mismatch");
    }
    let lease = authority.acquire(capture, payload).await?;
    if lease.revision != planned.policy_revision
        || lease.revision != lease.hold.revision().to_string()
    {
        bail!("stale policy revision");
    }
    if plan(capture, Some(authority)).map_err(|e| anyhow!("planning denied: {e:?}"))? != *planned {
        bail!("stale or forged plan");
    }
    let anchor = match &capture.anchor {
        RootAnchor::ExactId(id) => json!({"id":id.as_str()}),
        RootAnchor::VerifiedKey { namespace, value } => json!({"key":[namespace,value]}),
        RootAnchor::ApprovedAlias { namespace, value } => json!({"alias":[namespace,value]}),
        RootAnchor::Missing => json!(null),
    };
    // Immutable replay identity; decision/revision may change on re-resolution.
    let identity = json!({"digest":capture.payload_digest,"evidence":capture.evidence_ids,"anchor":anchor,
        "type":capture.root_type.ontology_label(),"actor":lease.actor.stable_agent_id(),"sources":payload.pending.sources}).to_string();
    let write = GraphWrite {
        event_key: opaque_key(&capture.event.producer, &capture.event.event_id),
        identity,
        actor: lease.actor.stable_agent_id().into(),
        revision: lease.revision.clone(),
        summary: payload.body.summary.clone(),
        details: payload.body.details.clone(),
        sources_json: serde_json::to_string(&payload.pending.sources)?,
        digest: capture.payload_digest.clone(),
        evidence_ids: capture.evidence_ids.iter().cloned().collect(),
        disposition: planned.disposition.clone(),
        root_label: capture.root_type.ontology_label().into(),
    };
    let mut tx = graph.begin().await?;
    let result = within_transaction(tx.as_mut(), capture, &write)
        .await
        .and_then(|result| authority.validate_lease(&lease).map(|()| result));
    match result {
        Ok(result) => {
            tx.commit().await?;
            drop(lease.hold);
            Ok(result)
        }
        Err(error) => {
            let rollback = tx.rollback().await;
            drop(lease.hold);
            rollback?;
            Err(error)
        }
    }
}

async fn within_transaction(
    tx: &mut dyn GraphTransaction,
    capture: &Capture,
    write: &GraphWrite,
) -> Result<CommitResult> {
    let schema = tx.schema().await?;
    let mut required = required_constraints(capture.root_type);
    if matches!(capture.anchor, RootAnchor::ApprovedAlias { .. }) {
        required.insert(("LifeRootAlias".into(), "key".into()));
    }
    if !schema.transactional || !schema.catalog_ready || !required.is_subset(&schema.unique) {
        bail!("missing graph transaction/unique-key/backfill prerequisites");
    }
    if let Some(existing) = tx.receipt(&write.event_key).await? {
        if existing.identity != write.identity {
            bail!("event replay conflict");
        }
        if let Some(root) = &existing.root {
            let roots = tx.roots(root).await?;
            if roots.len() != 1 || roots[0].label != write.root_label {
                bail!("receipt root missing/ambiguous");
            }
        }
        return Ok(CommitResult {
            root: existing.root,
            kind: existing.kind,
            replay: true,
        });
    }
    let resolved = tx.resolve(&capture.anchor).await?;
    match &write.disposition {
        Disposition::NewRoot {
            proposed_id,
            binding,
            root_type,
        } => {
            if resolved != Lookup::Absent
                || capture.anchor
                    != (RootAnchor::VerifiedKey {
                        namespace: binding.namespace.clone(),
                        value: binding.value.clone(),
                    })
                || root_type != &capture.root_type
                || !tx.roots(proposed_id).await?.is_empty()
            {
                bail!("stale graph resolution; replan required");
            }
        }
        Disposition::Same { root } | Disposition::Extend { root } => {
            if resolved != Lookup::Unique(root.clone()) {
                bail!("stale graph resolution; replan required");
            }
            let roots = tx.roots(root).await?;
            if roots.len() != 1 || roots[0].label != write.root_label {
                bail!("root missing/ambiguous/type mismatch");
            }
            if matches!(write.disposition, Disposition::Extend { .. }) && write.details.is_none() {
                bail!("extension requires details");
            }
        }
        Disposition::Review { .. } => {}
    }
    tx.write(write).await
}
