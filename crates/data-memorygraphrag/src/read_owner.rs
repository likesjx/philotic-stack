//! Supervised cross-process read owner. No listener, credential, grant or writer
//! is installed implicitly. All admitted clients must be direct owned children;
//! remote/independent peers and unenrolled writers keep production disabled.
use ansible_mesh_core::privacy_local::{
    LaunchPrincipal, LocalLaunchRegistry, Uuid, VerifiedLocalSession,
};
use anyhow::{Result, anyhow, bail};
use neo4rs::{ConfigBuilder, Graph, Txn, query};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, OwnedMutexGuard, Semaphore};

const NODES: &str = "MATCH (n) WHERE n.id IN $ids RETURN n.id AS id, labels(n) AS labels, n.claim_summary AS summary, n.source_policy_manifest AS sources LIMIT 2049";
const EDGE: &str = "MATCH (a {id:$from})-[r]->(b {id:$to}) WHERE type(r)=$relation RETURN a.id AS from, b.id AS to, type(r) AS relation, r.source_policy_manifest AS sources LIMIT 2";
const ROOT: &str = "MATCH (b:LifeRootBinding {key:$key}) RETURN b.root_id AS root LIMIT 2";
const ALIAS: &str =
    "MATCH (b:LifeRootAlias {key:$key}) WHERE b.approved=true RETURN b.root_id AS root LIMIT 2";
const MAX_FRAME: usize = 262144;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerRole {
    Gateway,
    Issuer,
}

// Durable uncertainty marker, not a policy/grant database. The operator must
// preprovision a private clean marker and explicitly reconcile dirty recovery.
// File lock prevents two daemons from owning this journal concurrently.
struct Journal {
    file: File,
    path: PathBuf,
    uid: u32,
    device: u64,
    inode: u64,
    poisoned: bool,
}
impl Journal {
    fn open(path: &Path, uid: u32) -> Result<Self> {
        let before = std::fs::symlink_metadata(path)?;
        let parent =
            std::fs::symlink_metadata(path.parent().ok_or_else(|| anyhow!("owner unavailable"))?)?;
        if !path.is_absolute()
            || !before.is_file()
            || before.uid() != uid
            || before.mode() & 0o077 != 0
            || !parent.is_dir()
            || parent.uid() != uid
            || parent.mode() & 0o077 != 0
        {
            bail!("owner unavailable");
        }
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        file.try_lock().map_err(|_| anyhow!("owner unavailable"))?;
        let after = file.metadata()?;
        if after.dev() != before.dev() || after.ino() != before.ino() {
            bail!("owner unavailable");
        }
        let mut value = Vec::new();
        (&mut file).take(7).read_to_end(&mut value)?;
        if value != b"clean\n" {
            bail!("owner recovery required");
        }
        Ok(Self {
            file,
            path: path.into(),
            uid,
            device: after.dev(),
            inode: after.ino(),
            poisoned: false,
        })
    }
    fn validate(&self) -> Result<()> {
        let s = std::fs::symlink_metadata(&self.path)?;
        if self.poisoned
            || !s.is_file()
            || s.uid() != self.uid
            || s.mode() & 0o077 != 0
            || s.dev() != self.device
            || s.ino() != self.inode
        {
            bail!("owner unavailable");
        }
        Ok(())
    }
    fn mark(&mut self, clean: bool) -> Result<()> {
        self.validate()?;
        let result = (|| -> Result<()> {
            self.file.seek(SeekFrom::Start(0))?;
            self.file
                .write_all(if clean { b"clean\n" } else { b"dirty\n" })?;
            self.file.set_len(6)?;
            self.file.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            self.poison();
        }
        result
    }
    fn poison(&mut self) {
        self.poisoned = true;
    } // dirty marker remains durable
}

// An aborted/panicking task cannot bypass async cleanup and silently reopen
// admission in the same daemon. Drop is deliberately conservative; only an
// acknowledged clean completion can disarm the durable uncertainty guard.
struct Fence {
    journal: OwnedMutexGuard<Journal>,
    complete: bool,
}
impl std::ops::Deref for Fence {
    type Target = Journal;
    fn deref(&self) -> &Journal {
        &self.journal
    }
}
impl std::ops::DerefMut for Fence {
    fn deref_mut(&mut self) -> &mut Journal {
        &mut self.journal
    }
}
impl Drop for Fence {
    fn drop(&mut self) {
        if !self.complete {
            self.journal.poison();
        }
    }
}

pub struct ReadOwner {
    hotel: String,
    identity: String,
    launches: Arc<LocalLaunchRegistry>,
    roles: StdMutex<BTreeMap<String, OwnerRole>>,
    journal: Arc<Mutex<Journal>>,
    graph: Graph,
    connections: Arc<Semaphore>,
}
impl ReadOwner {
    /// Own the private one-connection pool; no caller can retain a Graph clone.
    /// Session setup/check immediately precedes begin on that same connection.
    pub fn new(
        hotel: String,
        identity: String,
        journal: &Path,
        uid: u32,
        connection: ConfigBuilder,
    ) -> Result<Arc<Self>> {
        if !bounded(&hotel) || !bounded(&identity) {
            bail!("owner unavailable");
        }
        Ok(Arc::new(Self {
            hotel,
            identity,
            launches: Arc::new(LocalLaunchRegistry::default()),
            roles: StdMutex::new(BTreeMap::new()),
            journal: Arc::new(Mutex::new(Journal::open(journal, uid)?)),
            graph: Graph::connect(connection.max_connections(1).fetch_size(32).build()?)?,
            connections: Arc::new(Semaphore::new(32)),
        }))
    }
    /// Concrete child handles only; a claimed PID is never an enrollment.
    pub async fn attach(
        &self,
        guest: &str,
        uid: u32,
        principal: LaunchPrincipal,
        child: Arc<StdMutex<Child>>,
        role: OwnerRole,
    ) -> Result<Uuid> {
        let gate = self.journal.lock().await;
        gate.validate()?;
        let mut roles = self
            .roles
            .lock()
            .map_err(|_| anyhow!("owner unavailable"))?;
        if !bounded(guest) || (roles.len() >= 16 && !roles.contains_key(guest)) {
            bail!("owner unavailable");
        }
        let generation = self.launches.attach(guest, uid, principal, child)?;
        roles.insert(guest.into(), role);
        Ok(generation)
    }
    pub async fn retire(&self, guest: &str, generation: Uuid) -> Result<()> {
        let gate = self.journal.lock().await;
        gate.validate()?;
        self.launches.retire(guest, generation)
    }
    fn authenticate(&self, stream: &UnixStream) -> Result<(Arc<VerifiedLocalSession>, OwnerRole)> {
        let roles = self
            .roles
            .lock()
            .map_err(|_| anyhow!("owner unavailable"))?;
        for (guest, role) in roles.iter() {
            if let Ok(session) = self.launches.authenticate(&self.hotel, stream, guest) {
                return Ok((session, *role));
            }
        }
        bail!("owner unavailable")
    }
    /// Listener creation/permissions remain explicit installation actions.
    /// Existing listener and private durable journal must be operator reviewed.
    pub async fn serve(self: Arc<Self>, listener: UnixListener) -> Result<()> {
        loop {
            let permit = self.connections.clone().acquire_owned().await?;
            let (stream, _) = listener.accept().await?;
            let owner = self.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let outcome = owner.connection(stream).await;
                #[cfg(test)]
                if std::env::var("LIFEGRAPH_APPROVED_DISPOSABLE_BOLT").as_deref()
                    == Ok("memgraph-3.10.1")
                {
                    if let Err(error) = &outcome {
                        eprintln!("synthetic owner failure: {error}");
                    }
                }
                let _ = outcome;
            });
        }
    }
    async fn begin_read(&self) -> Result<Txn> {
        // Setup, SHOW and BEGIN hold one physical driver checkout. Pool
        // replacement happens before setup; no retries/recycle occur between.
        Ok(self
            .graph
            .start_txn_with_verified_session(
                query("SET SESSION TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION"),
                query("SHOW STORAGE INFO"),
                verify_isolation,
            )
            .await?)
    }

    async fn connection(&self, mut stream: UnixStream) -> Result<()> {
        let (peer, role) = self.authenticate(&stream)?;
        let mut gate: Option<Fence> = None;
        let mut tx: Option<Txn> = None;
        let mut session: Option<String> = None;
        let mut sequence = 0;
        let mut mode = "";
        let mut unknown = false;
        let mut lease_deadline: Option<Instant> = None;
        let outcome: Result<()> = async {
            loop {
                let request = tokio::time::timeout(
                    lease_deadline
                        .map_or(Some(Duration::from_secs(5)), |d| {
                            d.checked_duration_since(Instant::now())
                        })
                        .ok_or_else(|| anyhow!("owner unavailable"))?,
                    read_frame(&mut stream),
                )
                .await??;
                peer.principal()?;
                if request.version != 1
                    || request.owner != self.identity
                    || Uuid::parse_str(&request.session).is_err()
                    || session.as_ref().is_some_and(|s| s != &request.session)
                    || request.sequence != sequence + 1
                    || request.sequence > 6150
                {
                    bail!("owner unavailable");
                }
                session.get_or_insert_with(|| request.session.clone());
                sequence = request.sequence;
                let result = match request.operation.as_str() {
                    "begin_snapshot" | "begin_release" | "begin_issuer_change" => {
                        if gate.is_some()
                            || !empty(&request.parameters)
                            || (request.operation == "begin_issuer_change")
                                != (role == OwnerRole::Issuer)
                        {
                            bail!("owner unavailable");
                        }
                        let lock = tokio::time::timeout(
                            Duration::from_secs(5),
                            self.journal.clone().lock_owned(),
                        )
                        .await?;
                        let mut lock = Fence {
                            journal: lock,
                            complete: false,
                        };
                        lock.mark(false)?;
                        lease_deadline = Some(Instant::now() + Duration::from_secs(5));
                        gate = Some(lock);
                        mode = match request.operation.as_str() {
                            "begin_release" => "release",
                            "begin_issuer_change" => "issuer",
                            _ => "snapshot",
                        };
                        json!(true)
                    }
                    "start_read" => {
                        if role != OwnerRole::Gateway
                            || gate.is_none()
                            || tx.is_some()
                            || !empty(&request.parameters)
                        {
                            bail!("owner unavailable");
                        }
                        unknown = true;
                        tx = Some(self.begin_read().await?);
                        unknown = false;
                        json!(true)
                    }
                    "nodes" | "edge" | "verified_root" | "approved_alias" => {
                        if role != OwnerRole::Gateway || gate.is_none() {
                            bail!("owner unavailable");
                        }
                        if tx.is_none() {
                            unknown = true;
                            tx = Some(self.begin_read().await?);
                            unknown = false;
                        }
                        fixed_read(
                            tx.as_mut().unwrap(),
                            &request.operation,
                            &request.parameters,
                        )
                        .await?
                    }
                    "finish_read" => {
                        if role != OwnerRole::Gateway || !empty(&request.parameters) {
                            bail!("owner unavailable");
                        }
                        unknown = true;
                        tx.take()
                            .ok_or_else(|| anyhow!("owner unavailable"))?
                            .rollback()
                            .await?;
                        unknown = false;
                        json!(true)
                    }
                    "finish_snapshot" | "end_release" | "end_issuer_change" => {
                        if !empty(&request.parameters)
                            || gate.is_none()
                            || !matches!(
                                (request.operation.as_str(), mode),
                                ("finish_snapshot", "snapshot")
                                    | ("end_release", "release")
                                    | ("end_issuer_change", "issuer")
                            )
                        {
                            bail!("owner unavailable");
                        }
                        if let Some(transaction) = tx.take() {
                            unknown = true;
                            transaction.rollback().await?;
                            unknown = false;
                        }
                        peer.principal()?;
                        gate.as_mut().unwrap().mark(true)?;
                        gate.as_mut().unwrap().complete = true;
                        mode = "";
                        lease_deadline = None;
                        drop(gate.take());
                        json!(true)
                    }
                    _ => bail!("owner unavailable"),
                };
                if lease_deadline.is_some_and(|d| Instant::now() >= d) {
                    bail!("owner unavailable");
                }
                peer.principal()?;
                let reply = Reply {
                    owner: &self.identity,
                    session: session.as_ref().unwrap(),
                    sequence,
                    ok: true,
                    result,
                };
                tokio::time::timeout(Duration::from_secs(5), write_frame(&mut stream, &reply))
                    .await??;
            }
        }
        .await;
        // Never cancel a driver future mid-query: owned task retains the gate
        // until it finishes and this explicit rollback acknowledges quiescence.
        let rollback = if let Some(transaction) = tx.take() {
            transaction.rollback().await
        } else {
            Ok(())
        };
        if let Some(mut lock) = gate.take() {
            if mode != "snapshot" || rollback.is_err() || unknown || lock.mark(true).is_err() {
                lock.poison();
            } else {
                lock.complete = true;
            }
        }
        outcome
    }
}
fn bounded(v: &str) -> bool {
    !v.trim().is_empty() && v.len() <= 128
}
fn empty(v: &Value) -> bool {
    v.as_object().is_some_and(|o| o.is_empty())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u8,
    owner: String,
    session: String,
    sequence: u32,
    operation: String,
    parameters: Value,
}
#[derive(Serialize)]
struct Reply<'a> {
    owner: &'a str,
    session: &'a str,
    sequence: u32,
    ok: bool,
    result: Value,
}
async fn read_frame(stream: &mut UnixStream) -> Result<Request> {
    let size = stream.read_u32().await? as usize;
    if size == 0 || size > MAX_FRAME {
        bail!("owner unavailable");
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
async fn write_frame(stream: &mut UnixStream, reply: &Reply<'_>) -> Result<()> {
    let bytes = serde_json::to_vec(reply)?;
    if bytes.len() > 1048576 {
        bail!("owner unavailable");
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    Ok(())
}
fn verify_isolation(rows: &[neo4rs::Row]) -> neo4rs::Result<()> {
    let mut mode = None;
    let mut session = None;
    for row in rows {
        let key: String = row.get("storage info")?;
        // Other metadata includes integers; never decode or disclose values
        // outside these exact allowlisted fields.
        if key == "storage_mode" {
            if mode.is_some() {
                return Err(neo4rs::Error::InvalidConfig);
            }
            let value: String = row.get("value")?;
            mode = Some(
                ["IN_MEMORY_TRANSACTIONAL", "ON_DISK_TRANSACTIONAL"].contains(&value.as_str()),
            );
        }
        if key == "session_isolation_level" {
            if session.is_some() {
                return Err(neo4rs::Error::InvalidConfig);
            }
            let value: String = row.get("value")?;
            session = Some(value == "SNAPSHOT_ISOLATION");
        }
    }
    if mode != Some(true) || session != Some(true) {
        return Err(neo4rs::Error::InvalidConfig);
    }
    Ok(())
}

async fn fixed_read(tx: &mut Txn, op: &str, parameters: &Value) -> Result<Value> {
    let keys: Vec<&str> = match op {
        "nodes" => vec!["ids"],
        "edge" => vec!["from", "to", "relation"],
        "verified_root" | "approved_alias" => vec!["key"],
        _ => bail!("owner unavailable"),
    };
    let object = parameters
        .as_object()
        .ok_or_else(|| anyhow!("owner unavailable"))?;
    if object.len() != keys.len() || object.keys().any(|k| !keys.contains(&k.as_str())) {
        bail!("owner unavailable");
    }
    let source = match op {
        "nodes" => NODES,
        "edge" => EDGE,
        "verified_root" => ROOT,
        _ => ALIAS,
    };
    let mut q = query(source);
    if op == "nodes" {
        let ids = parameters["ids"]
            .as_array()
            .ok_or_else(|| anyhow!("owner unavailable"))?;
        let values: Vec<String> = ids
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| bounded(s))
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("owner unavailable"))
            })
            .collect::<Result<_>>()?;
        if values.len() > 2048 || values.iter().collect::<BTreeSet<_>>().len() != values.len() {
            bail!("owner unavailable");
        }
        q = q.param("ids", values);
    } else {
        for key in keys {
            q = q.param(
                key,
                parameters[key]
                    .as_str()
                    .filter(|s| bounded(s))
                    .ok_or_else(|| anyhow!("owner unavailable"))?
                    .to_owned(),
            );
        }
    }
    let mut stream = tx.execute(q).await?;
    let mut rows = Vec::new();
    let mut bytes = 0;
    while let Some(row) = stream.next(&mut *tx).await? {
        if rows.len() >= if op == "nodes" { 2049 } else { 2 } {
            bail!("owner unavailable");
        }
        let value = match op {
            "nodes" => {
                json!({"id":row.get::<String>("id")?,"labels":row.get::<Vec<String>>("labels")?,"summary":row.get::<String>("summary")?,"sources":row.get::<String>("sources")?})
            }
            "edge" => {
                json!({"from":row.get::<String>("from")?,"to":row.get::<String>("to")?,"relation":row.get::<String>("relation")?,"sources":row.get::<String>("sources")?})
            }
            _ => json!({"root":row.get::<String>("root")?}),
        };
        let size = serde_json::to_vec(&value)?.len();
        bytes += size;
        if size > 65536 || bytes > 900000 {
            bail!("owner unavailable");
        }
        rows.push(value);
    }
    Ok(json!(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    struct Fixture {
        dir: PathBuf,
        journal: PathBuf,
        uid: u32,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("synthetic-read-owner-{}", Uuid::new_v4()));
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let journal = dir.join("uncertainty");
            std::fs::write(&journal, b"clean\n").unwrap();
            std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o600)).unwrap();
            let uid = std::fs::metadata(&journal).unwrap().uid();
            Self { dir, journal, uid }
        }
        fn graph(&self) -> ConfigBuilder {
            ConfigBuilder::default()
                .uri("127.0.0.1:1")
                .user("synthetic")
                .password("")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    #[test]
    fn durable_journal_excludes_second_owner_and_refuses_uncertain_restart() {
        let f = Fixture::new();
        let mut j = Journal::open(&f.journal, f.uid).unwrap();
        assert!(Journal::open(&f.journal, f.uid).is_err());
        j.mark(false).unwrap();
        j.poison();
        assert!(j.mark(true).is_err());
        drop(j);
        assert!(Journal::open(&f.journal, f.uid).is_err());
        assert_eq!(std::fs::read(&f.journal).unwrap(), b"dirty\n");
    }

    #[tokio::test]
    async fn aborted_task_cannot_reopen_admission_in_same_process() {
        let f = Fixture::new();
        let journal = Arc::new(Mutex::new(Journal::open(&f.journal, f.uid).unwrap()));
        let ready = Arc::new(tokio::sync::Notify::new());
        let task_journal = journal.clone();
        let task_ready = ready.clone();
        let task = tokio::spawn(async move {
            let mut fence = Fence {
                journal: task_journal.lock_owned().await,
                complete: false,
            };
            fence.mark(false).unwrap();
            task_ready.notify_one();
            std::future::pending::<()>().await;
        });
        ready.notified().await;
        task.abort();
        assert!(task.await.is_err());
        assert!(journal.lock().await.validate().is_err());
        drop(journal);
        assert!(Journal::open(&f.journal, f.uid).is_err());
    }
    #[test]
    fn isolation_metadata_is_allowlisted_and_fails_closed() {
        fn row(key: &str, value: neo4rs::BoltType) -> neo4rs::Row {
            neo4rs::Row::new(
                neo4rs::BoltList {
                    value: vec!["storage info".into(), "value".into()],
                },
                neo4rs::BoltList {
                    value: vec![key.into(), value],
                },
            )
        }
        let good = || {
            vec![
                row("storage_mode", "IN_MEMORY_TRANSACTIONAL".into()),
                row("session_isolation_level", "SNAPSHOT_ISOLATION".into()),
                row("vertex_count", 11_i64.into()),
            ]
        };
        assert!(verify_isolation(&good()).is_ok());
        assert!(verify_isolation(&[]).is_err());
        assert!(verify_isolation(&good()[..1]).is_err());
        let mut duplicate = good();
        duplicate.push(row("session_isolation_level", "SNAPSHOT_ISOLATION".into()));
        assert!(verify_isolation(&duplicate).is_err());
        assert!(
            verify_isolation(&[
                row("storage_mode", "IN_MEMORY_TRANSACTIONAL".into()),
                row("session_isolation_level", "READ_COMMITTED".into())
            ])
            .is_err()
        );
    }
    #[test]
    fn journal_replacement_and_permissions_deny() {
        let f = Fixture::new();
        let j = Journal::open(&f.journal, f.uid).unwrap();
        std::fs::set_permissions(&f.journal, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(j.validate().is_err());
        std::fs::set_permissions(&f.journal, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(j.validate().is_ok());
        std::fs::rename(&f.journal, f.dir.join("original")).unwrap();
        std::fs::write(&f.journal, b"clean\n").unwrap();
        assert!(j.validate().is_err());
    }
    #[tokio::test]
    async fn frame_bounds_and_unknown_fields_are_enforced_before_dispatch() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        assert!(read_frame(&mut server).await.is_err());
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let bytes=serde_json::to_vec(&json!({"version":1,"owner":"synthetic","session":Uuid::new_v4().to_string(),"sequence":1,"operation":"begin_snapshot","parameters":{},"role":"issuer"})).unwrap();
        client.write_u32(bytes.len() as u32).await.unwrap();
        client.write_all(&bytes).await.unwrap();
        assert!(read_frame(&mut server).await.is_err());
    }
    async fn wait_file(path: &Path) {
        for _ in 0..300 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("synthetic fixture deadline");
    }
    fn child(f: &Fixture, name: &str, operation: &str) -> Arc<StdMutex<Child>> {
        Arc::new(StdMutex::new(
            Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .arg("--ignored")
                .arg("--exact")
                .arg("read_owner::tests::fixture_child")
                .env("SYNTHETIC_OWNER_DIR", &f.dir)
                .env("SYNTHETIC_OWNER_NAME", name)
                .env("SYNTHETIC_OWNER_OPERATION", operation)
                .spawn()
                .unwrap(),
        ))
    }
    async fn wait_child(child: &Arc<StdMutex<Child>>) {
        for _ in 0..500 {
            if let Some(status) = child.lock().unwrap().try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("synthetic child deadline");
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_owned_children_serialize_cross_process_issuer_changes() {
        let f = Fixture::new();
        let owner = ReadOwner::new(
            "synthetic-hotel".into(),
            "synthetic-owner".into(),
            &f.journal,
            f.uid,
            f.graph(),
        )
        .unwrap();
        let socket = f.dir.join("owner.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let c1 = child(&f, "one", "issuer");
        let c2 = child(&f, "two", "issuer");
        for (name, c) in [("one", c1.clone()), ("two", c2.clone())] {
            owner
                .attach(
                    name,
                    f.uid,
                    LaunchPrincipal {
                        stable_agent_id: format!("synthetic-{name}"),
                        roles: BTreeSet::new(),
                    },
                    c,
                    OwnerRole::Issuer,
                )
                .await
                .unwrap();
        }
        let server = tokio::spawn(owner.clone().serve(listener));
        std::fs::write(f.dir.join("start-one"), b"synthetic").unwrap();
        wait_file(&f.dir.join("held-one")).await;
        std::fs::write(f.dir.join("start-two"), b"synthetic").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!f.dir.join("held-two").exists());
        std::fs::write(f.dir.join("finish-one"), b"synthetic").unwrap();
        wait_child(&c1).await;
        wait_file(&f.dir.join("held-two")).await;
        std::fs::write(f.dir.join("finish-two"), b"synthetic").unwrap();
        wait_child(&c2).await;
        assert_eq!(std::fs::read(&f.journal).unwrap(), b"clean\n");
        server.abort();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn release_disconnect_poison_survives_daemon_restart() {
        let f = Fixture::new();
        let owner = ReadOwner::new(
            "synthetic-hotel".into(),
            "synthetic-owner".into(),
            &f.journal,
            f.uid,
            f.graph(),
        )
        .unwrap();
        let socket = f.dir.join("owner.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let c = child(&f, "abort", "release_abort");
        owner
            .attach(
                "abort",
                f.uid,
                LaunchPrincipal {
                    stable_agent_id: "synthetic-gateway".into(),
                    roles: BTreeSet::new(),
                },
                c.clone(),
                OwnerRole::Gateway,
            )
            .await
            .unwrap();
        let server = tokio::spawn(owner.clone().serve(listener));
        std::fs::write(f.dir.join("start-abort"), b"synthetic").unwrap();
        wait_child(&c).await;
        for _ in 0..100 {
            if owner.journal.lock().await.poisoned {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(owner.journal.lock().await.validate().is_err());
        server.abort();
        let _ = server.await;
        drop(owner);
        assert!(Journal::open(&f.journal, f.uid).is_err());
    }
    #[tokio::test]
    async fn unregistered_peer_is_denied_without_dirtying_authority() {
        let f = Fixture::new();
        let owner = ReadOwner::new(
            "synthetic-hotel".into(),
            "synthetic-owner".into(),
            &f.journal,
            f.uid,
            f.graph(),
        )
        .unwrap();
        let (_, stream) = UnixStream::pair().unwrap();
        assert!(owner.authenticate(&stream).is_err());
        assert_eq!(std::fs::read(&f.journal).unwrap(), b"clean\n");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "explicit approved disposable Memgraph fixture only"]
    async fn real_disposable_memgraph_snapshot_holds_release_fence() {
        if std::env::var("LIFEGRAPH_APPROVED_DISPOSABLE_BOLT").as_deref() != Ok("memgraph-3.10.1") {
            return;
        }
        let f = Fixture::new();
        let graph = ConfigBuilder::default()
            .uri("127.0.0.1:7687")
            .user("synthetic")
            .password("");
        let owner = ReadOwner::new(
            "synthetic-hotel".into(),
            "synthetic-owner".into(),
            &f.journal,
            f.uid,
            graph,
        )
        .unwrap();
        let socket = f.dir.join("owner.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let gateway = child(&f, "gateway", "gateway_read");
        let issuer = child(&f, "issuer", "issuer");
        for (name, c, role) in [
            ("gateway", gateway.clone(), OwnerRole::Gateway),
            ("issuer", issuer.clone(), OwnerRole::Issuer),
        ] {
            owner
                .attach(
                    name,
                    f.uid,
                    LaunchPrincipal {
                        stable_agent_id: format!("synthetic-{name}"),
                        roles: BTreeSet::new(),
                    },
                    c,
                    role,
                )
                .await
                .unwrap();
        }
        let server = tokio::spawn(owner.clone().serve(listener));
        std::fs::write(f.dir.join("start-gateway"), b"synthetic").unwrap();
        wait_file(&f.dir.join("held-gateway")).await;
        std::fs::write(f.dir.join("start-issuer"), b"synthetic").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!f.dir.join("held-issuer").exists());
        std::fs::write(f.dir.join("finish-gateway"), b"synthetic").unwrap();
        wait_child(&gateway).await;
        wait_file(&f.dir.join("held-issuer")).await;
        std::fs::write(f.dir.join("finish-issuer"), b"synthetic").unwrap();
        wait_child(&issuer).await;
        assert_eq!(std::fs::read(&f.journal).unwrap(), b"clean\n");
        server.abort();
    }
    // Executed only as the real supervisor-owned child of the synthetic tests.
    #[test]
    #[ignore]
    fn fixture_child() {
        let Ok(dir) = std::env::var("SYNTHETIC_OWNER_DIR") else {
            return;
        };
        let name = std::env::var("SYNTHETIC_OWNER_NAME").unwrap();
        let operation = std::env::var("SYNTHETIC_OWNER_OPERATION").unwrap();
        let dir = PathBuf::from(dir);
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            wait_file(&dir.join(format!("start-{name}"))).await;
            let mut stream=UnixStream::connect(dir.join("owner.sock")).await.unwrap();
            let session=Uuid::new_v4().to_string();
            async fn send(stream:&mut UnixStream,session:&str,sequence:u32,op:&str,parameters:Value)->Value {
                let bytes=serde_json::to_vec(&json!({"version":1,"owner":"synthetic-owner","session":session,"sequence":sequence,"operation":op,"parameters":parameters})).unwrap();
                stream.write_u32(bytes.len() as u32).await.unwrap();stream.write_all(&bytes).await.unwrap();
                let size=stream.read_u32().await.unwrap();assert!(size<=1048576);
                let mut body=vec![0;size as usize];stream.read_exact(&mut body).await.unwrap();
                let reply:Value=serde_json::from_slice(&body).unwrap();assert_eq!(reply["sequence"],sequence);assert_eq!(reply["ok"],true);reply["result"].clone()
            }
            assert_eq!(send(&mut stream,&session,1,if operation=="issuer"{"begin_issuer_change"}else{"begin_release"},json!({})).await,true);
            let final_sequence=if operation=="gateway_read" {
                assert_eq!(send(&mut stream,&session,2,"start_read",json!({})).await,true);
                let rows=send(&mut stream,&session,3,"nodes",json!({"ids":["goal:synthetic-root"]})).await;
                assert_eq!(rows.as_array().unwrap().len(),1);assert_eq!(rows[0]["summary"],"synthetic goal");assert_eq!(rows[0]["id"],"goal:synthetic-root");
                assert_eq!(send(&mut stream,&session,4,"finish_read",json!({})).await,true);5
            }else{2};
            std::fs::write(dir.join(format!("held-{name}")),b"synthetic").unwrap();
            if operation=="release_abort"{return;}
            wait_file(&dir.join(format!("finish-{name}"))).await;
            assert_eq!(send(&mut stream,&session,final_sequence,if operation=="issuer"{"end_issuer_change"}else{"end_release"},json!({})).await,true);
        });
    }
}
