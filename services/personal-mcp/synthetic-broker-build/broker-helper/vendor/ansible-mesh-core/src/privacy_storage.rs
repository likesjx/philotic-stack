//! Dedicated privacy store and durable capture inbox, not a LifeGraph migration.
//! Never open a hotel/LifeGraph DB here: initialization requires an empty database.
//! Authentication is supplied by the verified server-session owner, not this store.

use crate::privacy::{authorize_read, AuthenticatedAgent, PolicyAuthority, ResourcePolicy};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

pub struct PolicyStore {
    connection: Mutex<Connection>,
    path: std::path::PathBuf,
}

/// SQLite write reservation pins this authority revision until graph commit
/// finishes. Every writer to this same database, including other processes,
/// waits or fails busy. No authority is inferred from a captured revision.
pub struct PolicyCommitLease {
    connection: Mutex<Connection>,
    snapshot: PolicySnapshot,
    path: std::path::PathBuf,
}
impl PolicyCommitLease {
    pub fn revision(&self) -> u64 {
        self.snapshot.revision()
    }
    pub fn snapshot(&self) -> PolicySnapshot {
        self.snapshot.clone()
    }
    pub fn belongs_to(&self, store: &PolicyStore) -> bool {
        self.path == store.path
    }
    pub(crate) fn admit_local_authority(
        &self,
        hotel: &str,
        origin: &str,
        task: &str,
        handle: &str,
        binding: &str,
    ) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow!("policy lease lock poisoned"))?;
        let count: u64 = conn.query_row(
            "SELECT count(*) FROM local_authority_receipt WHERE hotel=?1 AND origin=?2",
            params![hotel, origin],
            |r| r.get(0),
        )?;
        if count >= 4096 {
            bail!("durable authority capacity; tombstones cannot be evicted");
        }
        // No upsert: an old event may not be silently regranted after restart.
        conn.execute("INSERT INTO local_authority_receipt(hotel,origin,task,handle,binding,cancelled) VALUES (?1,?2,?3,?4,?5,0)", params![hotel, origin, task, handle, binding])?;
        conn.execute_batch("COMMIT")?;
        Ok(())
    }
    pub(crate) fn local_authority_active(&self, handle: &str) -> Result<bool> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow!("policy lease lock poisoned"))?;
        authority_active(&conn, handle)
    }
    pub fn load_pending(
        &self,
        actor: &AuthenticatedAgent,
        producer: &str,
        event_id: &str,
    ) -> Result<CapturedRecord> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow!("policy lease lock poisoned"))?;
        captured_record(&conn, &self.snapshot, actor, producer, event_id)
    }
}
impl PolicyAuthority for PolicyCommitLease {
    fn policy(&self, resource: &str) -> Option<&ResourcePolicy> {
        self.snapshot.policy(resource)
    }
}
impl Drop for PolicyCommitLease {
    fn drop(&mut self) {
        if let Ok(conn) = self.connection.get_mut() {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
}

#[derive(Clone)]
pub struct PolicySnapshot {
    revision: u64,
    policies: BTreeMap<String, ResourcePolicy>,
}
/// Implement only over a current snapshot from an authenticated server transport.
/// Raw deserialized policies and client assertions must never implement this.
pub trait ServerPolicySnapshotAuthority {
    fn revision(&self) -> u64;
    fn policies(&self) -> BTreeMap<String, ResourcePolicy>;
}
impl PolicySnapshot {
    pub fn from_server(authority: &impl ServerPolicySnapshotAuthority) -> Result<Self> {
        if authority.revision() == 0 {
            bail!("missing policy revision");
        }
        let policies = authority.policies();
        if policies.is_empty() || policies.len() > 4096 {
            bail!("invalid policy snapshot size");
        }
        Ok(Self {
            revision: authority.revision(),
            policies,
        })
    }
    /// Export just the already-authorized manifest and its inherited ancestors.
    pub(crate) fn source_closure(
        &self,
        sources: &[String],
    ) -> Result<BTreeMap<String, ResourcePolicy>> {
        let mut pending = sources.to_vec();
        let mut out = BTreeMap::new();
        while let Some(source) = pending.pop() {
            if out.contains_key(&source) {
                continue;
            }
            if out.len() >= 4096 {
                bail!("source closure bound");
            }
            let policy = self
                .policies
                .get(&source)
                .ok_or_else(|| anyhow!("missing source policy"))?;
            pending.extend(policy.sources.iter().cloned());
            out.insert(source, policy.clone());
        }
        Ok(out)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
}
impl PolicyAuthority for PolicySnapshot {
    fn policy(&self, resource: &str) -> Option<&ResourcePolicy> {
        self.policies.get(resource)
    }
}

/// Capture inbox identity and opaque payload are server-owned. A capture is
/// pending work, not a graph root or a committed graph receipt. Its manifest
/// must include all inherited sources; the server must bind it to the payload.
pub struct PendingCapture<'a> {
    pub producer: &'a str,
    pub event_id: &'a str,
    pub payload: &'a str,
    pub sources: &'a [String],
    pub expected_revision: u64,
}

#[derive(Clone)]
pub struct CapturedRecord {
    pub producer: String,
    pub event_id: String,
    pub recorded_by: String,
    pub payload: String,
    pub sources: Vec<String>,
    pub captured_policy_revision: u64,
}

pub fn capture_payload_digest(payload: &str) -> String {
    hex::encode(Sha256::digest(payload.as_bytes()))
}

#[derive(Debug, PartialEq, Eq)]
pub enum CaptureResult {
    Inserted,
    Replay,
}

impl PolicyStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        // Durable commit before returning; no WAL files or external graph access.
        connection.pragma_update(None, "synchronous", "FULL")?;
        let version: u64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            let tables: u64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |r| r.get(0))?;
            if tables != 0 {
                bail!("privacy store requires a dedicated empty database");
            }
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch("CREATE TABLE privacy_revision (singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision INTEGER NOT NULL);
                INSERT INTO privacy_revision VALUES (1, 1);
                CREATE TABLE privacy_policy (resource TEXT PRIMARY KEY, policy_json TEXT NOT NULL);
                CREATE TABLE capture_inbox (producer TEXT NOT NULL, event_id TEXT NOT NULL, actor TEXT NOT NULL, payload TEXT NOT NULL, sources_json TEXT NOT NULL, policy_revision INTEGER NOT NULL, PRIMARY KEY(producer,event_id));
                CREATE TABLE local_authority_receipt (hotel TEXT NOT NULL, origin TEXT NOT NULL, task TEXT NOT NULL, handle TEXT NOT NULL UNIQUE, binding TEXT NOT NULL, cancelled INTEGER NOT NULL CHECK(cancelled IN (0,1)), PRIMARY KEY(hotel,origin,task));
                PRAGMA user_version=2;")?;
            tx.commit()?;
        } else if version != 2 {
            bail!("unsupported privacy store version");
        }
        let store = Self {
            connection: Mutex::new(connection),
            path: std::fs::canonicalize(path)?,
        };
        store.snapshot()?; // Missing/corrupt authority is an error, never defaults.
        Ok(store)
    }

    pub(crate) fn local_authority_active(&self, handle: &str) -> Result<bool> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow!("policy store lock poisoned"))?;
        authority_active(&conn, handle)
    }
    pub(crate) fn cancel_local_authority(&self, handle: &str) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow!("policy store lock poisoned"))?;
        if conn.execute(
            "UPDATE local_authority_receipt SET cancelled=1 WHERE handle=?1",
            [handle],
        )? != 1
        {
            bail!("unknown durable authority receipt");
        }
        Ok(())
    }

    /// Acquire before authorization and hold through the canonical graph commit
    /// or rollback. This is not a distributed graph/SQLite atomic transaction:
    /// graph receipt replay handles a crash after graph commit before inbox ack.
    pub fn pin_commit(&self) -> Result<PolicyCommitLease> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let current = snapshot(&conn)?;
        Ok(PolicyCommitLease {
            connection: Mutex::new(conn),
            snapshot: current,
            path: self.path.clone(),
        })
    }

    pub fn snapshot(&self) -> Result<PolicySnapshot> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow!("privacy store lock poisoned"))?;
        let tx = conn.transaction()?;
        let snapshot = snapshot(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// Only the authenticated owner inserts policies. Source ancestry becomes
    /// immutable: there is deliberately no replace/upsert/edit-source method.
    pub fn insert_policy(
        &self,
        actor: &AuthenticatedAgent,
        resource: &str,
        policy: &ResourcePolicy,
    ) -> Result<()> {
        if resource.trim().is_empty()
            || resource.len() > 512
            || policy.owner != actor.stable_agent_id()
            || policy.creator.trim().is_empty()
        {
            bail!("owner authority and valid policy required");
        }
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow!("privacy store lock poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = snapshot(&tx)?;
        for source in &policy.sources {
            if source == resource {
                bail!("source cycle");
            }
            authorize_read(&current, Some(actor), source)
                .map_err(|_| anyhow!("source access denied"))?;
        }
        tx.execute(
            "INSERT INTO privacy_policy VALUES (?1,?2)",
            params![resource, serde_json::to_string(policy)?],
        )?;
        tx.execute(
            "UPDATE privacy_revision SET revision=revision+1 WHERE singleton=1",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn revoke_creator(&self, actor: &AuthenticatedAgent, resource: &str) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow!("privacy store lock poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let json: String = tx.query_row(
            "SELECT policy_json FROM privacy_policy WHERE resource=?1",
            [resource],
            |r| r.get(0),
        )?;
        let mut policy: ResourcePolicy = serde_json::from_str(&json)?;
        policy
            .revoke_creator(actor)
            .map_err(|_| anyhow!("owner authority required"))?;
        tx.execute(
            "UPDATE privacy_policy SET policy_json=?1 WHERE resource=?2",
            params![serde_json::to_string(&policy)?, resource],
        )?;
        tx.execute(
            "UPDATE privacy_revision SET revision=revision+1 WHERE singleton=1",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// One real SQLite transaction checks current source read ACLs, revision,
    /// and event replay binding, then persists the durable pending capture.
    /// It performs no semantic resolution or graph write.
    pub fn enqueue(
        &self,
        actor: &AuthenticatedAgent,
        capture: PendingCapture<'_>,
    ) -> Result<CaptureResult> {
        if capture.producer.trim().is_empty()
            || capture.event_id.trim().is_empty()
            || capture.producer.len() > 256
            || capture.event_id.len() > 256
            || capture.payload.is_empty()
            || capture.payload.len() > 1_048_576
            || capture.sources.is_empty()
            || capture.sources.len() > 256
        {
            bail!("invalid capture");
        }
        let mut sources = capture.sources.to_vec();
        sources.sort();
        sources.dedup();
        let sources_json = serde_json::to_string(&sources)?;
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow!("privacy store lock poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = snapshot(&tx)?;
        if current.revision != capture.expected_revision {
            bail!("stale policy revision");
        }
        for source in &sources {
            authorize_read(&current, Some(actor), source)
                .map_err(|_| anyhow!("source access denied"))?;
        }
        let existing: Option<(String,String,String)> = tx.query_row("SELECT actor,payload,sources_json FROM capture_inbox WHERE producer=?1 AND event_id=?2", params![capture.producer,capture.event_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((prior_actor, payload, prior_sources)) = existing {
            if prior_actor != actor.stable_agent_id()
                || payload != capture.payload
                || prior_sources != sources_json
            {
                bail!("event replay conflict");
            }
            tx.commit()?;
            return Ok(CaptureResult::Replay);
        }
        tx.execute(
            "INSERT INTO capture_inbox VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                capture.producer,
                capture.event_id,
                actor.stable_agent_id(),
                capture.payload,
                sources_json,
                current.revision
            ],
        )?;
        tx.commit()?;
        Ok(CaptureResult::Inserted)
    }

    pub fn pending_count(&self) -> Result<u64> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow!("privacy store lock poisoned"))?;
        Ok(conn.query_row("SELECT count(*) FROM capture_inbox", [], |r| r.get(0))?)
    }

    /// Stored actor is provenance, never a principal to impersonate. No graph
    /// acknowledgement/deletion here: redelivery relies on canonical receipts.
    pub fn load_pending(
        &self,
        actor: &AuthenticatedAgent,
        producer: &str,
        event_id: &str,
    ) -> Result<CapturedRecord> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow!("privacy store lock poisoned"))?;
        let tx = conn.transaction()?;
        let current = snapshot(&tx)?;
        let record = captured_record(&tx, &current, actor, producer, event_id)?;
        tx.commit()?;
        Ok(record)
    }
}

fn captured_record(
    conn: &Connection,
    policy: &PolicySnapshot,
    actor: &AuthenticatedAgent,
    producer: &str,
    event_id: &str,
) -> Result<CapturedRecord> {
    let (recorded_by,payload,sources_json,revision):(String,String,String,u64)=conn.query_row(
        "SELECT actor,payload,sources_json,policy_revision FROM capture_inbox WHERE producer=?1 AND event_id=?2",
        params![producer,event_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let sources: Vec<String> = serde_json::from_str(&sources_json)?;
    if sources.is_empty() {
        bail!("missing source manifest");
    }
    for source in &sources {
        authorize_read(policy, Some(actor), source).map_err(|_| anyhow!("source access denied"))?;
    }
    Ok(CapturedRecord {
        producer: producer.into(),
        event_id: event_id.into(),
        recorded_by,
        payload,
        sources,
        captured_policy_revision: revision,
    })
}

fn snapshot(conn: &Connection) -> Result<PolicySnapshot> {
    let revision = conn.query_row(
        "SELECT revision FROM privacy_revision WHERE singleton=1",
        [],
        |r| r.get::<_, u64>(0),
    )?;
    if revision == 0 {
        bail!("missing authoritative revision");
    }
    let mut query = conn.prepare("SELECT resource,policy_json FROM privacy_policy")?;
    let mut policies = BTreeMap::new();
    for row in query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (resource, json) = row?;
        policies.insert(
            resource,
            serde_json::from_str(&json).context("invalid canonical policy")?,
        );
    }
    Ok(PolicySnapshot { revision, policies })
}

fn authority_active(conn: &Connection, handle: &str) -> Result<bool> {
    let state: Option<i64> = conn
        .query_row(
            "SELECT cancelled FROM local_authority_receipt WHERE handle=?1",
            [handle],
            |r| r.get(0),
        )
        .optional()?;
    Ok(state == Some(0))
}
