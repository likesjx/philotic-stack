//! Explicit Linux supervisor for the existing read owner. No installation,
//! credential provisioning, identity resolver or graph write route is supplied.
use crate::read_owner::{OwnerRole, ReadOwner};
use ansible_mesh_core::privacy_local::LaunchPrincipal;
use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UnixListener;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorConfig {
    pub version: u32,
    pub hotel: String,
    pub owner: String,
    pub uid: u32,
    pub socket: PathBuf,
    pub journal: PathBuf,
    pub graph_connection_file: PathBuf,
    pub workers: Vec<WorkerConfig>,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
    Gateway,
    Issuer,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: PathBuf,
    pub sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerConfig {
    pub guest: String,
    pub agent: String,
    pub roles: BTreeSet<String>,
    pub role: WorkerRole,
    pub count: usize,
    pub executable: Artifact,
    pub artifacts: Vec<Artifact>,
    pub directory: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphConnection {
    uri: String,
    user: String,
    password: String,
}
fn text(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= 128 && !s.contains('\0')
}
fn absolute(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("supervisor path invalid");
    }
    Ok(())
}
fn private_parent(path: &Path, uid: u32) -> Result<()> {
    absolute(path)?;
    let m = std::fs::symlink_metadata(
        path.parent()
            .ok_or_else(|| anyhow!("private parent unavailable"))?,
    )?;
    if !m.is_dir() || m.uid() != uid || m.mode() & 0o077 != 0 {
        bail!("private parent unavailable");
    }
    Ok(())
}
fn private_bytes(path: &Path, uid: u32, maximum: u64) -> Result<Vec<u8>> {
    private_parent(path, uid)?;
    let before = std::fs::symlink_metadata(path)?;
    if !before.is_file()
        || before.uid() != uid
        || before.mode() & 0o077 != 0
        || before.len() > maximum
    {
        bail!("private file unavailable");
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let m = file.metadata()?;
    if m.dev() != before.dev() || m.ino() != before.ino() {
        bail!("private file changed");
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    let after = std::fs::symlink_metadata(path)?;
    if bytes.len() as u64 > maximum
        || after.dev() != m.dev()
        || after.ino() != m.ino()
        || after.mtime() != m.mtime()
        || after.mtime_nsec() != m.mtime_nsec()
        || after.ctime() != m.ctime()
        || after.ctime_nsec() != m.ctime_nsec()
    {
        bail!("private file changed");
    }
    Ok(bytes)
}
pub fn load_config(path: &Path) -> Result<SupervisorConfig> {
    // Error output never contains file contents, argument/environment values or
    // database credentials. Connection details are not part of this manifest.
    let uid = unsafe { libc::geteuid() };
    let config: SupervisorConfig = serde_json::from_slice(&private_bytes(path, uid, 65536)?)
        .map_err(|_| anyhow!("supervisor manifest invalid"))?;
    config.validate(uid)?;
    Ok(config)
}
impl SupervisorConfig {
    pub fn validate(&self, uid: u32) -> Result<()> {
        if self.version != 1
            || self.uid != uid
            || !text(&self.hotel)
            || !text(&self.owner)
            || self.workers.is_empty()
            || self.workers.len() > 16
            || !self.socket.to_string_lossy().ends_with(".sock")
            || self.socket.as_os_str().len() > 100
            || self.socket == self.journal
            || self.socket == self.graph_connection_file
            || self.journal == self.graph_connection_file
        {
            bail!("supervisor manifest invalid");
        }
        private_parent(&self.socket, uid)?;
        private_parent(&self.journal, uid)?;
        private_parent(&self.graph_connection_file, uid)?;
        let mut names = BTreeSet::new();
        let mut agents = BTreeSet::new();
        let mut total = 0usize;
        let mut issuer = false;
        let mut gateway = false;
        for w in &self.workers {
            if !text(&w.guest)
                || w.guest.len() > 120
                || w.guest.contains('#')
                || !text(&w.agent)
                || !names.insert(w.guest.clone())
                || !agents.insert(w.agent.clone())
                || w.count == 0
                || w.count > 16
                || w.roles.len() > 32
                || w.roles.iter().any(|r| !text(r))
                || w.arguments.len() > 64
                || w.arguments
                    .iter()
                    .any(|a| a.len() > 4096 || a.contains('\0'))
                || w.environment.len() > 64
                || w.environment.iter().any(|(k, v)| {
                    k.is_empty()
                        || k.len() > 128
                        || !k
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                        || k.starts_with("PERSONAL_OAUTH_LIFEGRAPH_")
                        || k.starts_with("LIFEGRAPH_SUPERVISOR_")
                        || v.len() > 4096
                        || v.contains('\0')
                })
                || w.artifacts.len() > 64
            {
                bail!("worker manifest invalid");
            }
            total += w.count;
            if total > 16 {
                bail!("worker count exceeded");
            }
            absolute(&w.directory)?;
            let m = std::fs::symlink_metadata(&w.directory)?;
            if !m.is_dir() || (m.uid() != uid && m.uid() != 0) || m.mode() & 0o022 != 0 {
                bail!("worker directory unavailable");
            }
            match w.role {
                WorkerRole::Gateway => gateway = true,
                WorkerRole::Issuer => issuer = true,
            }
            validate_artifact(&w.executable)?;
            for a in &w.artifacts {
                validate_artifact(a)?;
            }
        }
        if !issuer || !gateway {
            bail!("both owner roles required");
        }
        Ok(())
    }
}
fn validate_artifact(a: &Artifact) -> Result<()> {
    absolute(&a.path)?;
    if a.sha256.len() != 64
        || !a
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail!("artifact digest invalid");
    }
    Ok(())
}
fn verified_artifact(a: &Artifact, executable: bool, uid: u32) -> Result<File> {
    let before = std::fs::symlink_metadata(&a.path)?;
    if !before.is_file()
        || (before.uid() != uid && before.uid() != 0)
        || before.mode() & 0o022 != 0
        || (before.uid() == uid && before.mode() & 0o222 != 0)
        || before.len() > 536870912
        || (executable && (before.mode() & 0o111 == 0 || before.mode() & 0o6000 != 0))
    {
        bail!("artifact unavailable");
    }
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&a.path)?;
    let m = f.metadata()?;
    if m.dev() != before.dev() || m.ino() != before.ino() {
        bail!("artifact changed");
    }
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    let mut first = true;
    let mut read = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        if read > 536870912 {
            bail!("artifact size exceeded");
        }
        if first && executable && (n < 4 || &buf[..4] != b"\x7fELF") {
            bail!("ELF executable required");
        }
        first = false;
        hash.update(&buf[..n]);
    }
    let after = f.metadata()?;
    if first
        || format!("{:x}", hash.finalize()) != a.sha256
        || m.len() != after.len()
        || m.mtime() != after.mtime()
        || m.mtime_nsec() != after.mtime_nsec()
        || m.ctime() != after.ctime()
        || m.ctime_nsec() != after.ctime_nsec()
    {
        bail!("artifact verification failed");
    }
    Ok(f)
}
struct OwnedChildren(Vec<Arc<Mutex<Child>>>);
impl Drop for OwnedChildren {
    fn drop(&mut self) {
        for child in &self.0 {
            if let Ok(mut c) = child.lock() {
                let _ = c.kill();
            }
        }
    }
}
struct OwnerTask {
    owner: Arc<ReadOwner>,
    task: tokio::task::JoinHandle<Result<()>>,
}
impl Drop for OwnerTask {
    fn drop(&mut self) {
        // Cancellation/panic of the supervisor future cannot detach a live
        // listener. JoinSet drop aborts its connections, preserving dirty fences.
        self.owner.stop_admission();
        self.task.abort();
    }
}
async fn reap(children: &OwnedChildren) {
    for child in &children.0 {
        if let Ok(mut c) = child.lock() {
            let _ = c.kill();
        }
    }
    for _ in 0..100 {
        let done = children.0.iter().all(|c| {
            c.lock()
                .ok()
                .is_some_and(|mut c| c.try_wait().ok().flatten().is_some())
        });
        if done {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
struct SocketGuard {
    path: PathBuf,
    dev: u64,
    ino: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Ok(m) = std::fs::symlink_metadata(&self.path)
            && m.dev() == self.dev
            && m.ino() == self.ino
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
/// No worker restart: any required worker loss stops admission and the whole
/// generation. The existing journal decides whether a subsequent start is safe.
pub async fn run(
    config: SupervisorConfig,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    let uid = unsafe { libc::geteuid() };
    config.validate(uid)?;
    let secret: GraphConnection =
        serde_json::from_slice(&private_bytes(&config.graph_connection_file, uid, 8192)?)
            .map_err(|_| anyhow!("graph configuration unavailable"))?;
    if secret.uri.is_empty()
        || secret.user.is_empty()
        || secret.uri.len() > 2048
        || secret.user.len() > 128
        || secret.password.len() > 4096
    {
        bail!("graph configuration unavailable");
    }
    // Fail all artifact checks before opening admission or launching any worker.
    let mut executables = Vec::new();
    for w in &config.workers {
        executables.push(verified_artifact(&w.executable, true, uid)?);
        for a in &w.artifacts {
            let _ = verified_artifact(a, false, uid)?;
        }
    }
    let owner = ReadOwner::new(
        config.hotel.clone(),
        config.owner.clone(),
        &config.journal,
        uid,
        neo4rs::ConfigBuilder::default()
            .uri(secret.uri)
            .user(secret.user)
            .password(secret.password),
    )?;
    // bind() fails on an existing path; never unlink a previous generation.
    let listener = UnixListener::bind(&config.socket)?;
    let m = std::fs::symlink_metadata(&config.socket)?;
    let socket = SocketGuard {
        path: config.socket.clone(),
        dev: m.dev(),
        ino: m.ino(),
    };
    std::fs::set_permissions(&config.socket, std::fs::Permissions::from_mode(0o600))?;
    let mut children = OwnedChildren(Vec::new());
    let launch: Result<()> = async {
        for (w, executable) in config.workers.iter().zip(&executables) {
            for index in 0..w.count {
                // Execute the SHA-verified open ELF inode, never re-open its
                // mutable path between verification and exec. No shell added.
                let mut command = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
                command
                    .args(&w.arguments)
                    .current_dir(&w.directory)
                    .env_clear()
                    .envs(&w.environment)
                    .env("PERSONAL_OAUTH_LIFEGRAPH_COORDINATION", "required")
                    .env("PERSONAL_OAUTH_LIFEGRAPH_OWNER_SOCKET", &config.socket)
                    .env("PERSONAL_OAUTH_LIFEGRAPH_OWNER_ID", &config.owner)
                    .env("LIFEGRAPH_SUPERVISOR_WORKER_INDEX", index.to_string())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                let parent = std::process::id() as libc::pid_t;
                // Only async-signal-safe syscalls in the post-fork hook. A
                // supervisor crash kills its direct workers; close the race
                // where the parent died before the death signal was installed.
                unsafe {
                    command.pre_exec(move || {
                        if libc::prctl(
                            libc::PR_SET_NO_NEW_PRIVS,
                            1 as libc::c_ulong,
                            0 as libc::c_ulong,
                            0 as libc::c_ulong,
                            0 as libc::c_ulong,
                        ) != 0
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        if libc::getppid() != parent {
                            return Err(std::io::Error::other("supervisor exited"));
                        }
                        Ok(())
                    });
                }
                let child = command.spawn()?;
                let child = Arc::new(Mutex::new(child));
                children.0.push(child.clone());
                owner
                    .attach(
                        &format!("{}#{index}", w.guest),
                        uid,
                        LaunchPrincipal {
                            stable_agent_id: w.agent.clone(),
                            roles: w.roles.clone(),
                        },
                        child,
                        match w.role {
                            WorkerRole::Gateway => OwnerRole::Gateway,
                            WorkerRole::Issuer => OwnerRole::Issuer,
                        },
                    )
                    .await?;
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = launch {
        reap(&children).await;
        drop(socket);
        return Err(e);
    }
    let mut serving = OwnerTask {
        owner: owner.clone(),
        task: tokio::spawn(owner.clone().serve(listener)),
    };
    tokio::pin!(shutdown);
    let outcome = loop {
        tokio::select! {
            biased;
            _=&mut shutdown=>break Ok(()),
            result=&mut serving.task=>break result.unwrap_or_else(|_|Err(anyhow!("owner task failed"))),
            _=tokio::time::sleep(Duration::from_millis(50))=>{
                if children.0.iter().any(|c|c.lock().map_or(true,|mut c|c.try_wait().map_or(true,|s|s.is_some()))) {
                    break Err(anyhow!("required worker exited"));
                }
            }
        }
    };
    owner.stop_admission();
    if !serving.task.is_finished() {
        let _ = (&mut serving.task).await;
    }
    reap(&children).await;
    drop(socket);
    outcome
}
