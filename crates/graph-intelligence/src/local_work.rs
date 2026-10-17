//! Opt-in synthetic local work protocol. No runtime executor or access grants.
//! The caller supplies a trusted monotonic logical clock (milliseconds) and prepays
//! active-work quanta before work. SQLite transactions serialize authority and work
//! transitions; transaction conflicts fail closed and may be retried by the caller.
use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::{GraphEngine, NodeKind};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Scope {
    pub action: String,
    pub resource: String,
    pub destination: String,
}

/// A synthetic authorization fixture, distinct from a work ownership lease.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Authority {
    pub authenticated_authorizer_ref: String,
    pub scopes: Vec<Scope>,
    pub valid_from: u64,
    pub expires_at: u64,
    pub revoked: bool,
    pub may_delegate: bool,
    pub parent: Option<String>,
    pub revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fence {
    pub session: String,
    pub epoch: u64,
    pub expires_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    Prepared,
    Dispatched,
    Succeeded,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub event: String,
    pub scope: Scope,
    pub authority: String,
    pub authority_chain: Vec<(String, u64)>,
    pub phase: Phase,
    pub dispatch_epoch: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub revision: String,
    pub environment: String,
    pub command: String,
    pub exit_code: i32,
    pub result_ref: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Work {
    #[serde(default)]
    pub checkpoint: Option<serde_json::Value>,
    pub slice: String,
    pub role_charter: String,
    pub revision: String,
    pub environment: String,
    pub fence: Option<Fence>,
    pub operation: Option<Operation>,
    pub attempts: u64,
    pub max_attempts: u64,
    pub active_ms: u64,
    pub budget_ms: u64,
    pub stopped: Option<String>,
    pub implemented: bool,
    pub tested: bool,
    pub merged: bool,
    pub deployed: bool,
    pub live_verified: bool,
}

/// Uses only namespaced synthetic nodes in an explicitly supplied graph database.
pub struct LocalWork {
    graph: GraphEngine,
    // An execution permit is process-local. Reopening a checkpoint can only
    // reconcile a prior dispatch, even while its original fence is unexpired.
    dispatches: HashSet<(String, u64)>,
}

fn read<T: serde::de::DeserializeOwned>(
    tx: &Transaction<'_>,
    id: &str,
    kind: NodeKind,
) -> Result<T> {
    let json: String = tx
        .query_row(
            "SELECT properties FROM nodes WHERE id=?1 AND kind=?2",
            params![id, kind.as_str()],
            |r| r.get(0),
        )
        .context("missing synthetic record")?;
    Ok(serde_json::from_str(&json)?)
}

fn write<T: Serialize>(tx: &Transaction<'_>, id: &str, kind: NodeKind, value: &T) -> Result<()> {
    ensure!(id.starts_with("synthetic:"), "synthetic records only");
    tx.execute(
        "INSERT INTO nodes(id,kind,name,properties,created_at,updated_at)
         VALUES (?1,?2,?1,?3,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         ON CONFLICT(id) DO UPDATE SET properties=excluded.properties,updated_at=excluded.updated_at
         WHERE nodes.kind=excluded.kind",
        params![id, kind.as_str(), serde_json::to_string(value)?],
    )?;
    ensure!(tx.changes() == 1, "node kind collision");
    // Keep the existing graph's search projection coherent.
    tx.execute(
        "INSERT OR IGNORE INTO nodes_fts(rowid,name) SELECT rowid,name FROM nodes WHERE id=?1",
        [id],
    )?;
    Ok(())
}

fn valid_fence(work: &Work, fence: &Fence, now: u64) -> Result<()> {
    ensure!(
        work.fence.as_ref() == Some(fence) && now < fence.expires_at,
        "expired or stale owner"
    );
    Ok(())
}

fn valid_inspection_receipt(work: &Work, receipt: &serde_json::Value) -> Result<()> {
    let op = work.operation.as_ref().context("not prepared")?;
    ensure!(
        receipt["schema_version"] == 1
            && receipt["operation_id"] == op.id
            && receipt["revision"] == work.revision
            && receipt["environment"] == work.environment
            && receipt["result"]["input_digest"] == op.scope.resource,
        "stale inspection receipt"
    );
    Ok(())
}

fn authority_chain(
    tx: &Transaction<'_>,
    id: &str,
    scope: &Scope,
    now: u64,
) -> Result<Vec<(String, u64)>> {
    let mut chain = Vec::new();
    let mut current = id.to_owned();
    let mut child: Option<Authority> = None;
    loop {
        ensure!(
            chain.len() < 16 && !chain.iter().any(|(id, _)| id == &current),
            "authority cycle/depth"
        );
        let auth: Authority = read(tx, &current, NodeKind::RoleCharter)?;
        ensure!(
            auth.authenticated_authorizer_ref
                .starts_with("synthetic:authorizer:")
                && !auth.revoked
                && auth.valid_from <= now
                && now < auth.expires_at
                && auth.scopes.contains(scope),
            "authority refused"
        );
        if let Some(child) = &child {
            ensure!(
                auth.may_delegate
                    && child.valid_from >= auth.valid_from
                    && child.expires_at <= auth.expires_at
                    && child.scopes.iter().all(|s| auth.scopes.contains(s)),
                "delegation broadens or is forbidden"
            );
        }
        chain.push((current.clone(), auth.revision));
        match &auth.parent {
            None => return Ok(chain),
            Some(parent) => {
                current = parent.clone();
                child = Some(auth);
            }
        }
    }
}

impl LocalWork {
    pub fn open(path: &str) -> Result<Self> {
        let mut graph = GraphEngine::open(path)?;
        graph.transaction(|tx| {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS synthetic_work_effects (
                operation_id TEXT PRIMARY KEY, task_id TEXT NOT NULL UNIQUE, scope TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS local_inspection_receipts (
                operation_id TEXT PRIMARY KEY, task_id TEXT NOT NULL UNIQUE, scope TEXT NOT NULL, receipt TEXT NOT NULL);",
            )?;
            Ok(())
        })?;
        Ok(Self {
            graph,
            dispatches: HashSet::new(),
        })
    }

    /// Fixture administration only; no credential issuance or production authority.
    pub fn set_authority(&mut self, id: &str, mut auth: Authority) -> Result<()> {
        self.graph.transaction(|tx| {
            let previous: Option<String> = tx
                .query_row("SELECT properties FROM nodes WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional()?;
            auth.revision = match previous {
                Some(json) => serde_json::from_str::<Authority>(&json)?
                    .revision
                    .checked_add(1)
                    .context("epoch overflow")?,
                None => 1,
            };
            write(tx, id, NodeKind::RoleCharter, &auth)
        })
    }

    pub fn create(&mut self, id: &str, work: Work) -> Result<()> {
        ensure!(
            work.checkpoint.is_none()
                && work.fence.is_none()
                && work.operation.is_none()
                && work.attempts == 0
                && work.active_ms == 0
                && work.stopped.is_none()
                && !work.tested
                && !work.merged
                && !work.deployed
                && !work.live_verified,
            "invalid initial work"
        );
        ensure!(
            !work.revision.is_empty() && !work.environment.is_empty(),
            "candidate identity required"
        );
        self.graph.transaction(|tx| {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE id=?1)",
                [id],
                |r| r.get(0),
            )?;
            ensure!(!exists, "work already exists");
            write(
                tx,
                &work.slice,
                NodeKind::Slice,
                &serde_json::json!({"local_only": true}),
            )?;
            let _: Authority = read(tx, &work.role_charter, NodeKind::RoleCharter)?;
            write(tx, id, NodeKind::Task, &work)
        })
    }

    pub fn inspect(&mut self, id: &str) -> Result<Work> {
        self.graph.transaction(|tx| read(tx, id, NodeKind::Task))
    }

    /// Persist the compact runtime checkpoint in the same fenced task transaction.
    /// This does not create another execution ledger or authorize an effect.
    pub fn checkpoint(
        &mut self,
        id: &str,
        fence: &Fence,
        now: u64,
        content: serde_json::Value,
    ) -> Result<()> {
        ensure!(
            serde_json::to_vec(&content)?.len() <= 65536,
            "checkpoint too large"
        );
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx, id, NodeKind::Task)?;
            valid_fence(&work, fence, now)?;
            ensure!(work.stopped.is_none(), "work stopped");
            let op = work.operation.as_ref().context("not prepared")?;
            ensure!(
                authority_chain(tx, &op.authority, &op.scope, now)? == op.authority_chain,
                "authority changed"
            );
            work.checkpoint = Some(content);
            write(tx, id, NodeKind::Task, &work)
        })
    }

    pub fn claim(&mut self, id: &str, session: &str, now: u64, ttl: u64) -> Result<Fence> {
        ensure!(ttl > 0, "positive lease required");
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx, id, NodeKind::Task)?;
            ensure!(
                work.fence.as_ref().is_none_or(|f| now >= f.expires_at),
                "already claimed"
            );
            let fence = Fence {
                session: session.into(),
                epoch: work
                    .fence
                    .as_ref()
                    .map_or(0, |f| f.epoch)
                    .checked_add(1)
                    .context("epoch overflow")?,
                expires_at: now.checked_add(ttl).context("expiry overflow")?,
            };
            write(tx, session, NodeKind::Session, &fence)?;
            work.fence = Some(fence.clone());
            write(tx, id, NodeKind::Task, &work)?;
            Ok(fence)
        })
    }

    /// Reserve active milliseconds BEFORE performing a bounded work quantum.
    /// Reservations are never refunded on crash; external waits reserve zero.
    /// False is a committed stop checkpoint, not a rolled-back error.
    pub fn reserve(
        &mut self,
        id: &str,
        fence: &Fence,
        now: u64,
        active_ms: u64,
        quota_available: bool,
    ) -> Result<bool> {
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx, id, NodeKind::Task)?;
            valid_fence(&work, fence, now)?;
            if work.stopped.is_some() {
                return Ok(false);
            }
            if !quota_available {
                work.stopped = Some("quota exhausted".into());
            } else if active_ms > work.budget_ms.saturating_sub(work.active_ms) {
                work.stopped = Some("active-work budget exhausted".into());
            } else {
                work.active_ms += active_ms;
            }
            write(tx, id, NodeKind::Task, &work)?;
            Ok(work.stopped.is_none())
        })
    }

    pub fn prepare(
        &mut self,
        id: &str,
        fence: &Fence,
        now: u64,
        event: &str,
        scope: Scope,
    ) -> Result<bool> {
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx, id, NodeKind::Task)?;
            valid_fence(&work, fence, now)?;
            ensure!(work.stopped.is_none(), "work stopped");
            if let Some(op) = &work.operation {
                ensure!(
                    op.event == event && op.scope == scope,
                    "operation identity conflict"
                );
                return Ok(false);
            }
            if work.attempts >= work.max_attempts {
                work.stopped = Some("attempt budget exhausted".into());
                write(tx, id, NodeKind::Task, &work)?;
                return Ok(false);
            }
            let chain = authority_chain(tx, &work.role_charter, &scope, now)?;
            work.attempts += 1;
            work.operation = Some(Operation {
                id: format!("{id}:operation"),
                event: event.into(),
                scope,
                authority: work.role_charter.clone(),
                authority_chain: chain,
                phase: Phase::Prepared,
                dispatch_epoch: None,
            });
            write(tx, id, NodeKind::Task, &work)?;
            Ok(true)
        })
    }

    /// Durable intent precedes even the synthetic effect transaction.
    pub fn dispatch(&mut self, id: &str, fence: &Fence, now: u64) -> Result<()> {
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx, id, NodeKind::Task)?;
            valid_fence(&work, fence, now)?;
            ensure!(work.stopped.is_none(), "work stopped");
            let op = work.operation.as_mut().context("not prepared")?;
            ensure!(
                op.phase == Phase::Prepared,
                "already dispatched; reconcile only"
            );
            ensure!(
                authority_chain(tx, &op.authority, &op.scope, now)? == op.authority_chain,
                "authority changed"
            );
            op.phase = Phase::Dispatched;
            op.dispatch_epoch = Some(fence.epoch);
            write(tx, id, NodeKind::Task, &work)
        })?;
        self.dispatches.insert((id.to_owned(), fence.epoch));
        Ok(())
    }

    /// Retry only a known pre-dispatch failure. Dispatched/unknown operations
    /// never return to Prepared. The attempt ceiling commits a stop checkpoint.
    pub fn retry_prepared(&mut self, id: &str, fence: &Fence, now: u64) -> Result<bool> {
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx, id, NodeKind::Task)?;
            valid_fence(&work, fence, now)?;
            ensure!(work.stopped.is_none(), "work stopped");
            ensure!(
                work.operation
                    .as_ref()
                    .is_some_and(|op| op.phase == Phase::Prepared),
                "retry outcome not known safe"
            );
            if work.attempts >= work.max_attempts {
                work.stopped = Some("attempt budget exhausted".into());
            } else {
                work.attempts += 1;
            }
            write(tx, id, NodeKind::Task, &work)?;
            Ok(work.stopped.is_none())
        })
    }

    /// This ledger IS the synthetic side effect. No external operation is executed.
    /// Repeat delivery is idempotent; authority and fencing are rechecked inside
    /// the same transaction as the effect, including changes after dispatch.
    pub fn simulate_effect(&mut self, id: &str, fence: &Fence, now: u64) -> Result<bool> {
        ensure!(
            self.dispatches.contains(&(id.to_owned(), fence.epoch)),
            "resumed dispatch must reconcile"
        );
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx, id, NodeKind::Task)?;
            valid_fence(&work, fence, now)?;
            ensure!(work.stopped.is_none(), "work stopped");
            let op = work.operation.as_ref().context("not prepared")?;
            ensure!(op.phase == Phase::Dispatched, "not dispatched");
            ensure!(op.dispatch_epoch == Some(fence.epoch), "takeover must reconcile prior dispatch");
            ensure!(authority_chain(tx, &op.authority, &op.scope, now)? == op.authority_chain, "authority changed");
            let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM synthetic_work_effects WHERE operation_id=?1)", [&op.id], |r| r.get(0))?;
            if exists { return Ok(true); }
            // One millisecond is the deterministic simulated operation cost.
            // Enforce a budget even if the caller omitted extra work reservations.
            if work.active_ms >= work.budget_ms {
                work.stopped = Some("active-work budget exhausted".into());
                write(tx, id, NodeKind::Task, &work)?;
                return Ok(false);
            }
            tx.execute("INSERT INTO synthetic_work_effects VALUES (?1,?2,?3) ON CONFLICT(operation_id) DO NOTHING",
                params![op.id, id, serde_json::to_string(&op.scope)?])?;
            work.active_ms += 1;
            write(tx, id, NodeKind::Task, &work)?;
            Ok(true)
        })
    }

    pub fn reconcile(&mut self, id: &str, fence: &Fence, now: u64) -> Result<Phase> {
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx, id, NodeKind::Task)?;
            valid_fence(&work, fence, now)?;
            let op = work.operation.as_mut().context("not prepared")?;
            ensure!(op.phase != Phase::Prepared, "not dispatched");
            let receipt: Option<String> = tx
                .query_row(
                    "SELECT scope FROM synthetic_work_effects WHERE operation_id=?1 AND task_id=?2",
                    params![op.id, id],
                    |r| r.get(0),
                )
                .optional()?;
            op.phase = if receipt.as_deref() == Some(&serde_json::to_string(&op.scope)?) {
                Phase::Succeeded
            } else {
                Phase::Unknown
            };
            let phase = op.phase.clone();
            write(tx, id, NodeKind::Task, &work)?;
            Ok(phase)
        })
    }

    /// Commit one bounded inspection result. Caller verifies the result against
    /// its immutable snapshot; the ledger owns atomic authorization and fencing.
    pub fn commit_inspection(
        &mut self,
        id: &str,
        fence: &Fence,
        now: u64,
        receipt: &serde_json::Value,
    ) -> Result<bool> {
        let encoded = serde_json::to_string(receipt)?;
        ensure!(encoded.len() <= 4096, "receipt too large");
        ensure!(
            self.dispatches.contains(&(id.to_owned(), fence.epoch)),
            "reconcile only"
        );
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx, id, NodeKind::Task)?;
            valid_fence(&work, fence, now)?;
            ensure!(receipt["task_id"] == id, "receipt task mismatch");
            valid_inspection_receipt(&work, receipt)?;
            ensure!(work.stopped.is_none(), "work stopped");
            let op = work.operation.as_ref().context("not prepared")?;
            ensure!(
                op.scope.action == "development.inspect_manifest"
                    && op.phase == Phase::Dispatched
                    && op.dispatch_epoch == Some(fence.epoch),
                "inspection dispatch mismatch"
            );
            ensure!(
                authority_chain(tx, &op.authority, &op.scope, now)? == op.authority_chain,
                "authority changed"
            );
            let existing: Option<String> = tx
                .query_row(
                    "SELECT receipt FROM local_inspection_receipts WHERE operation_id=?1",
                    [&op.id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(existing) = existing {
                ensure!(existing == encoded, "conflicting receipt");
                return Ok(true);
            }
            if work.active_ms >= work.budget_ms {
                work.stopped = Some("active-work budget exhausted".into());
                write(tx, id, NodeKind::Task, &work)?;
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO local_inspection_receipts VALUES (?1,?2,?3,?4)",
                params![op.id, id, serde_json::to_string(&op.scope)?, encoded],
            )?;
            work.active_ms += 1;
            write(tx, id, NodeKind::Task, &work)?;
            Ok(true)
        })
    }

    pub fn reconcile_inspection(
        &mut self,
        id: &str,
        fence: &Fence,
        now: u64,
    ) -> Result<Option<serde_json::Value>> {
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx,id,NodeKind::Task)?;
            valid_fence(&work,fence,now)?;
            ensure!(work.stopped.is_none(), "work stopped");
            let op = work.operation.as_mut().context("not prepared")?;
            ensure!(op.scope.action == "development.inspect_manifest" && op.phase != Phase::Prepared, "not dispatched inspection");
            ensure!(authority_chain(tx,&op.authority,&op.scope,now)? == op.authority_chain,"authority changed");
            let receipt: Option<String> = tx.query_row("SELECT receipt FROM local_inspection_receipts WHERE operation_id=?1 AND task_id=?2 AND scope=?3",params![op.id,id,serde_json::to_string(&op.scope)?],|r|r.get(0)).optional()?;
            if let Some(encoded)=&receipt {
                let value:serde_json::Value=serde_json::from_str(encoded)?;
                ensure!(value["task_id"]==id,"receipt task mismatch");
                valid_inspection_receipt(&work,&value)?;
            }
            work.operation.as_mut().context("not prepared")?.phase = if receipt.is_some() { Phase::Succeeded } else { Phase::Unknown };
            write(tx,id,NodeKind::Task,&work)?;
            receipt.map(|r|serde_json::from_str(&r).map_err(Into::into)).transpose()
        })
    }

    pub fn candidate(
        &mut self,
        id: &str,
        fence: &Fence,
        now: u64,
        revision: &str,
        environment: &str,
    ) -> Result<()> {
        ensure!(
            !revision.is_empty() && !environment.is_empty(),
            "candidate identity required"
        );
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx, id, NodeKind::Task)?;
            valid_fence(&work, fence, now)?;
            work.revision = revision.into();
            work.environment = environment.into();
            work.tested = false;
            write(tx, id, NodeKind::Task, &work)
        })
    }

    pub fn verify(
        &mut self,
        id: &str,
        fence: &Fence,
        now: u64,
        evidence_id: &str,
        evidence: Evidence,
    ) -> Result<()> {
        self.graph.transaction(|tx| {
            let mut work: Work = read(tx, id, NodeKind::Task)?;
            valid_fence(&work, fence, now)?;
            ensure!(
                work.stopped.is_none()
                    && work
                        .operation
                        .as_ref()
                        .is_some_and(|op| op.phase == Phase::Succeeded),
                "work incomplete/stopped"
            );
            ensure!(
                evidence.revision == work.revision
                    && evidence.environment == work.environment
                    && evidence.exit_code == 0
                    && !evidence.command.is_empty()
                    && !evidence.result_ref.is_empty(),
                "stale or invalid evidence"
            );
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE id=?1)",
                [evidence_id],
                |r| r.get(0),
            )?;
            ensure!(!exists, "immutable evidence already exists");
            write(tx, evidence_id, NodeKind::TestRun, &evidence)?;
            tx.execute(
                "INSERT INTO edges(source_id,target_id,relation) VALUES (?1,?2,'tests')",
                params![evidence_id, id],
            )?;
            work.tested = true;
            write(tx, id, NodeKind::Task, &work)
        })
    }

    pub fn effect_count(&mut self) -> Result<u64> {
        self.graph.transaction(|tx| {
            Ok(
                tx.query_row("SELECT count(*) FROM synthetic_work_effects", [], |r| {
                    r.get(0)
                })?,
            )
        })
    }
}
