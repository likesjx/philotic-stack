//! Local protected-delivery authority. Runtime installation is opt-in and absent.
//! Kernel peer credentials + a live supervisor-owned child authenticate a guest;
//! caller GuestIdentity, payload roles and opaque handles alone never do.
use crate::privacy::{
    authorize_processing, authorize_read, AuthenticatedAgent, PolicyAuthority, ProcessingOperation,
    ProviderBoundary, ServerAuthenticatedIdentity,
};
use crate::privacy_storage::{
    capture_payload_digest, PolicyCommitLease, PolicySnapshot, PolicyStore,
};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UnixStream;
pub use uuid::Uuid;

/// Only the supervisor implements this over a child it actually owns. A PID
/// string from the context DB, ps/kill existence or caller JSON is insufficient.
pub trait SupervisedProcess: Send + Sync {
    fn pid(&self) -> Result<u32>;
    fn alive(&self) -> Result<bool>;
}
impl SupervisedProcess for Mutex<std::process::Child> {
    fn pid(&self) -> Result<u32> {
        Ok(self
            .lock()
            .map_err(|_| anyhow!("child lock poisoned"))?
            .id())
    }
    fn alive(&self) -> Result<bool> {
        Ok(self
            .lock()
            .map_err(|_| anyhow!("child lock poisoned"))?
            .try_wait()?
            .is_none())
    }
}

/// Server-loaded launch mapping; never deserialize this from a registration.
pub struct LaunchPrincipal {
    pub stable_agent_id: String,
    pub roles: BTreeSet<String>,
}
impl ServerAuthenticatedIdentity for LaunchPrincipal {
    fn stable_agent_id(&self) -> &str {
        &self.stable_agent_id
    }
    fn roles(&self) -> BTreeSet<String> {
        self.roles.clone()
    }
}
struct Launch {
    generation: Uuid,
    uid: u32,
    principal: LaunchPrincipal,
    process: Arc<dyn SupervisedProcess>,
}
#[derive(Default)]
pub struct LocalLaunchRegistry {
    launches: Mutex<BTreeMap<String, Launch>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalConsumer {
    hotel: String,
    guest: String,
    generation: Uuid,
    agent: String,
}
/// No serde or caller-issued constructor. Each use rechecks owned child liveness
/// and generation, so stale PID reuse cannot revive a completed launch.
pub struct VerifiedLocalSession {
    registry: Arc<LocalLaunchRegistry>,
    scope: LocalConsumer,
    pid: u32,
    uid: u32,
}
impl LocalLaunchRegistry {
    pub fn attach(
        &self,
        guest: &str,
        uid: u32,
        principal: LaunchPrincipal,
        process: Arc<dyn SupervisedProcess>,
    ) -> Result<Uuid> {
        if guest.trim().is_empty() || guest.len() > 512 || !process.alive()? || process.pid()? == 0
        {
            bail!("invalid supervised launch");
        }
        AuthenticatedAgent::from_server(&principal).map_err(|_| anyhow!("invalid principal"))?;
        let generation = Uuid::new_v4();
        self.launches
            .lock()
            .map_err(|_| anyhow!("launch lock poisoned"))?
            .insert(
                guest.into(),
                Launch {
                    generation,
                    uid,
                    principal,
                    process,
                },
            );
        Ok(generation)
    }
    pub fn retire(&self, guest: &str, generation: Uuid) -> Result<()> {
        let mut launches = self
            .launches
            .lock()
            .map_err(|_| anyhow!("launch lock poisoned"))?;
        if launches
            .get(guest)
            .is_some_and(|l| l.generation == generation)
        {
            launches.remove(guest);
        }
        Ok(())
    }
    pub fn target(&self, hotel: &str, guest: &str) -> Result<LocalConsumer> {
        if hotel.trim().is_empty() {
            bail!("missing hotel");
        }
        let launches = self
            .launches
            .lock()
            .map_err(|_| anyhow!("launch lock poisoned"))?;
        let l = launches
            .get(guest)
            .ok_or_else(|| anyhow!("unknown supervised guest"))?;
        if !l.process.alive()? {
            bail!("supervised guest exited");
        }
        Ok(LocalConsumer {
            hotel: hotel.into(),
            guest: guest.into(),
            generation: l.generation,
            agent: l.principal.stable_agent_id.clone(),
        })
    }
    pub fn authenticate(
        self: &Arc<Self>,
        hotel: &str,
        stream: &UnixStream,
        claimed_guest: &str,
    ) -> Result<Arc<VerifiedLocalSession>> {
        let peer = stream.peer_cred()?;
        let pid = u32::try_from(
            peer.pid()
                .ok_or_else(|| anyhow!("kernel peer PID unavailable"))?,
        )
        .map_err(|_| anyhow!("invalid peer PID"))?;
        let scope = self.target(hotel, claimed_guest)?;
        let session = Arc::new(VerifiedLocalSession {
            registry: self.clone(),
            scope,
            pid,
            uid: peer.uid(),
        });
        session.principal()?; // Check kernel identity against that exact live child.
        Ok(session)
    }
    fn validate_target(&self, target: &LocalConsumer) -> Result<()> {
        let current = self.target(&target.hotel, &target.guest)?;
        if current != *target {
            bail!("consumer incarnation replaced");
        }
        Ok(())
    }
}
impl VerifiedLocalSession {
    pub fn consumer(&self) -> LocalConsumer {
        self.scope.clone()
    }
    pub fn principal(&self) -> Result<AuthenticatedAgent> {
        let launches = self
            .registry
            .launches
            .lock()
            .map_err(|_| anyhow!("launch lock poisoned"))?;
        let launch = launches
            .get(&self.scope.guest)
            .ok_or_else(|| anyhow!("retired launch"))?;
        if launch.generation != self.scope.generation
            || launch.uid != self.uid
            || launch.process.pid()? != self.pid
            || !launch.process.alive()?
            || launch.principal.stable_agent_id != self.scope.agent
        {
            bail!("unverified or replaced local session");
        }
        AuthenticatedAgent::from_server(&launch.principal).map_err(|_| anyhow!("invalid principal"))
    }
}

/// Implemented by the server's content/provenance owner. It must derive ALL
/// sources from the full payload/context, not just accept a client source list.
pub trait LocalManifestAuthority: Send + Sync {
    fn sources(
        &self,
        task_id: Uuid,
        payload: &str,
        actor: &AuthenticatedAgent,
    ) -> Option<Vec<String>>;
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalTaskEnvelope {
    pub task_id: Uuid,
    pub payload: String,
    pub authority_handle: Uuid,
}
/// Park/repark stores the identical envelope; it does not issue new authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParkedLocalTask {
    pub envelope: LocalTaskEnvelope,
}
struct Record {
    origin: Arc<VerifiedLocalSession>,
    consumer: LocalConsumer,
    task_id: Uuid,
    digest: String,
    sources: Vec<String>,
    revision: u64,
    expires: Instant,
    cancelled: bool,
}
struct Records {
    by_handle: BTreeMap<Uuid, Record>,
    by_task: BTreeMap<(Uuid, Uuid), Uuid>,
}
pub struct LocalTaskAuthority {
    hotel: String,
    launches: Arc<LocalLaunchRegistry>,
    policies: Arc<PolicyStore>,
    manifests: Arc<dyn LocalManifestAuthority>,
    records: Mutex<Records>,
    limit: usize,
}
pub struct ResolvedLocalAuthority {
    pub actor: AuthenticatedAgent,
    pub policies: PolicySnapshot,
    pub sources: Vec<String>,
    pub payload_digest: String,
    pub consumer_incarnation: Uuid,
}
impl LocalTaskAuthority {
    pub fn new(
        hotel: String,
        launches: Arc<LocalLaunchRegistry>,
        policies: Arc<PolicyStore>,
        manifests: Arc<dyn LocalManifestAuthority>,
        limit: usize,
    ) -> Result<Self> {
        if hotel.trim().is_empty() || limit == 0 || limit > 8192 {
            bail!("invalid local authority configuration");
        }
        Ok(Self {
            hotel,
            launches,
            policies,
            manifests,
            records: Mutex::new(Records {
                by_handle: BTreeMap::new(),
                by_task: BTreeMap::new(),
            }),
            limit,
        })
    }
    pub fn issue(
        &self,
        origin: Arc<VerifiedLocalSession>,
        consumer: LocalConsumer,
        task_id: Uuid,
        payload: String,
        ttl: Duration,
    ) -> Result<LocalTaskEnvelope> {
        if origin.scope.hotel != self.hotel
            || consumer.hotel != self.hotel
            || !Arc::ptr_eq(&origin.registry, &self.launches)
        {
            bail!("protected cross-hotel/foreign issuer delivery denied");
        }
        if payload.is_empty()
            || payload.len() > 1_048_576
            || ttl.is_zero()
            || ttl > Duration::from_secs(300)
        {
            bail!("invalid protected task");
        }
        let actor = origin.principal()?;
        self.launches.validate_target(&consumer)?;
        let mut sources = self
            .manifests
            .sources(task_id, &payload, &actor)
            .ok_or_else(|| anyhow!("missing authoritative source manifest"))?;
        sources.sort();
        sources.dedup();
        if sources.is_empty() || sources.len() > 256 {
            bail!("invalid source manifest");
        }
        let current = self.policies.snapshot()?;
        for source in &sources {
            authorize_read(&current, Some(&actor), source)
                .map_err(|_| anyhow!("source access denied"))?;
        }
        let digest = capture_payload_digest(&payload);
        let mut records = self
            .records
            .lock()
            .map_err(|_| anyhow!("authority lock poisoned"))?;
        let task_key = (origin.scope.generation, task_id);
        if let Some(handle) = records.by_task.get(&task_key) {
            let r = &records.by_handle[handle];
            if r.cancelled
                || Instant::now() >= r.expires
                || r.consumer != consumer
                || r.digest != digest
                || r.sources != sources
                || r.revision != current.revision()
            {
                bail!("protected task replay conflict or revoked authority");
            }
            return Ok(LocalTaskEnvelope {
                task_id,
                payload,
                authority_handle: *handle,
            });
        }
        // Tombstones are retained; capacity rejects, never evicts cancellation.
        if records.by_handle.len() >= self.limit {
            bail!("local authority capacity");
        }
        let handle = Uuid::new_v4();
        records.by_handle.insert(
            handle,
            Record {
                origin,
                consumer,
                task_id,
                digest,
                sources,
                revision: current.revision(),
                expires: Instant::now() + ttl,
                cancelled: false,
            },
        );
        records.by_task.insert(task_key, handle);
        Ok(LocalTaskEnvelope {
            task_id,
            payload,
            authority_handle: handle,
        })
    }
    fn check(
        &self,
        records: &Records,
        envelope: &LocalTaskEnvelope,
        consumer: &VerifiedLocalSession,
        policy: &impl PolicyAuthority,
        revision: u64,
        processing: (ProcessingOperation, ProviderBoundary),
    ) -> Result<AuthenticatedAgent> {
        let r = records
            .by_handle
            .get(&envelope.authority_handle)
            .ok_or_else(|| anyhow!("unknown protected authority"))?;
        if !Arc::ptr_eq(&consumer.registry, &self.launches) {
            bail!("foreign consumer registry");
        }
        consumer.principal()?;
        let actor = r.origin.principal()?;
        if consumer.scope.hotel != self.hotel
            || consumer.scope != r.consumer
            || r.task_id != envelope.task_id
            || r.digest != capture_payload_digest(&envelope.payload)
            || r.cancelled
            || Instant::now() >= r.expires
            || r.revision != revision
        {
            bail!("protected authority mismatch/stale/revoked");
        }
        self.launches.validate_target(&r.consumer)?;
        authorize_processing(policy, Some(&actor), &r.sources, processing.0, processing.1)
            .map_err(|_| anyhow!("privacy denied"))?;
        Ok(actor)
    }
    pub fn resolve(
        &self,
        envelope: &LocalTaskEnvelope,
        consumer: &VerifiedLocalSession,
        operation: ProcessingOperation,
        boundary: ProviderBoundary,
    ) -> Result<ResolvedLocalAuthority> {
        let policy = self.policies.snapshot()?;
        let records = self
            .records
            .lock()
            .map_err(|_| anyhow!("authority lock poisoned"))?;
        let actor = self.check(
            &records,
            envelope,
            consumer,
            &policy,
            policy.revision(),
            (operation, boundary),
        )?;
        let r = &records.by_handle[&envelope.authority_handle];
        Ok(ResolvedLocalAuthority {
            actor,
            policies: policy,
            sources: r.sources.clone(),
            payload_digest: r.digest.clone(),
            consumer_incarnation: r.consumer.generation,
        })
    }
    /// Graph reservation precedes checking the exact protected envelope. The
    /// caller keeps this lease through commit/rollback, including replay.
    pub fn pin_capture(
        &self,
        envelope: &LocalTaskEnvelope,
        consumer: &VerifiedLocalSession,
    ) -> Result<(ResolvedLocalAuthority, PolicyCommitLease)> {
        let lease = self.policies.pin_commit()?;
        let records = self
            .records
            .lock()
            .map_err(|_| anyhow!("authority lock poisoned"))?;
        let actor = self.check(
            &records,
            envelope,
            consumer,
            &lease,
            lease.revision(),
            (
                ProcessingOperation::SemanticResolution,
                ProviderBoundary::LocalTrusted,
            ),
        )?;
        let r = &records.by_handle[&envelope.authority_handle];
        let resolved = ResolvedLocalAuthority {
            actor,
            policies: lease.snapshot(),
            sources: r.sources.clone(),
            payload_digest: r.digest.clone(),
            consumer_incarnation: r.consumer.generation,
        };
        Ok((resolved, lease))
    }
    pub fn validate_pinned_capture(
        &self,
        envelope: &LocalTaskEnvelope,
        consumer: &VerifiedLocalSession,
        lease: &PolicyCommitLease,
    ) -> Result<()> {
        if !lease.belongs_to(&self.policies) {
            bail!("foreign policy reservation");
        }
        let records = self
            .records
            .lock()
            .map_err(|_| anyhow!("authority lock poisoned"))?;
        self.check(
            &records,
            envelope,
            consumer,
            lease,
            lease.revision(),
            (
                ProcessingOperation::SemanticResolution,
                ProviderBoundary::LocalTrusted,
            ),
        )?;
        Ok(())
    }
    pub fn park(&self, envelope: &LocalTaskEnvelope) -> Result<ParkedLocalTask> {
        let records = self
            .records
            .lock()
            .map_err(|_| anyhow!("authority lock poisoned"))?;
        let r = records
            .by_handle
            .get(&envelope.authority_handle)
            .ok_or_else(|| anyhow!("unknown protected authority"))?;
        if r.task_id != envelope.task_id
            || r.digest != capture_payload_digest(&envelope.payload)
            || r.cancelled
            || Instant::now() >= r.expires
        {
            bail!("cannot park modified/revoked authority");
        }
        Ok(ParkedLocalTask {
            envelope: envelope.clone(),
        })
    }
    pub fn flush(
        &self,
        parked: &ParkedLocalTask,
        consumer: &VerifiedLocalSession,
    ) -> Result<LocalTaskEnvelope> {
        self.resolve(
            &parked.envelope,
            consumer,
            ProcessingOperation::SemanticResolution,
            ProviderBoundary::LocalTrusted,
        )?;
        Ok(parked.envelope.clone())
    }
    pub fn cancel(&self, origin: &VerifiedLocalSession, handle: Uuid) -> Result<()> {
        origin.principal()?;
        let mut records = self
            .records
            .lock()
            .map_err(|_| anyhow!("authority lock poisoned"))?;
        let r = records
            .by_handle
            .get_mut(&handle)
            .ok_or_else(|| anyhow!("unknown protected authority"))?;
        if origin.scope != r.origin.scope {
            bail!("wrong cancellation principal");
        }
        r.cancelled = true;
        Ok(())
    }
}
