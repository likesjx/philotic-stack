//! Opt-in attachment to the hotel's existing materializer. No independent
//! supervisor, implicit credentials, schema creation or public listener.
use super::*;
use ansible_mesh_core::privacy_local::LaunchPrincipal;
use ansible_mesh_core::process_artifact::{Artifact, validate_artifact, verified_artifact};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
};
use tokio::net::UnixListener;
#[cfg(all(test, target_os = "linux"))]
mod composition;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    hotel: String,
    owner_id: String,
    membrane: String,
    runner: String,
    issuer_guest: String,
    control_socket: PathBuf,
    issuer_socket: PathBuf,
    journal: PathBuf,
    policy_file: PathBuf,
    binding: Binding,
    introspection_client: String,
    introspection_secret_file: PathBuf,
    #[serde(default)]
    introspection_ca_file: Option<PathBuf>,
    workers: Vec<Worker>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    pub guest: String,
    agent: String,
    roles: BTreeSet<String>,
    command: PathBuf,
    args: Vec<String>,
    sha256: String,
    environment: BTreeMap<String, String>,
    #[serde(default)]
    artifacts: Vec<Artifact>,
}
struct Pins {
    executable: std::fs::File,
    artifacts: Vec<(PathBuf, std::fs::File)>,
}
// Linux memfd seals prevent a same-UID artifact owner from changing verified
// bytes after admission. Never execute the mutable source inode.
#[cfg(target_os = "linux")]
fn pinned(a: &Artifact, executable: bool) -> Result<std::fs::File> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    let mut source = verified_artifact(a, executable, unsafe { libc::geteuid() })?;
    source.seek(SeekFrom::Start(0))?;
    let fd = unsafe {
        libc::memfd_create(
            c"native-artifact".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    ensure!(fd >= 0, "sealed artifact unavailable");
    let mut snapshot = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut hash = Sha256::new();
    let mut count = 0usize;
    let mut bytes = [0; 65536];
    loop {
        let n = source.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        count += n;
        ensure!(count <= 536870912, "artifact size exceeded");
        hash.update(&bytes[..n]);
        snapshot.write_all(&bytes[..n])?;
    }
    ensure!(
        format!("{:x}", hash.finalize()) == a.sha256,
        "artifact changed during snapshot"
    );
    snapshot.seek(SeekFrom::Start(0))?;
    ensure!(
        unsafe {
            libc::fcntl(
                snapshot.as_raw_fd(),
                libc::F_ADD_SEALS,
                libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
            )
        } == 0,
        "artifact sealing failed"
    );
    Ok(snapshot)
}
#[cfg(not(target_os = "linux"))]
fn pinned(a: &Artifact, executable: bool) -> Result<std::fs::File> {
    verified_artifact(a, executable, unsafe { libc::geteuid() })
}
struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.dev() == self.device && m.ino() == self.inode)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub struct Runtime {
    pub authority: Arc<NativeAuthority>,
    pub launches: Arc<LocalLaunchRegistry>,
    workers: BTreeMap<String, Worker>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    pins: BTreeMap<String, Pins>,
    _sockets: Vec<SocketGuard>,
    healthy: Arc<std::sync::atomic::AtomicBool>,
    issuer_guest: String,
    issuer_socket: PathBuf,
    owner_id: String,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn private(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        path.is_absolute()
            && meta.is_file()
            && meta.mode() & 0o077 == 0
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.len() <= 1048576,
        "invalid private native config"
    );
    let mut bytes = Vec::new();
    file.take(1048577).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1048576, "oversized native config");
    Ok(bytes)
}
struct HttpIssuer {
    client: reqwest::Client,
    url: String,
    client_id: String,
    secret: zeroize::Zeroizing<String>,
}
#[async_trait::async_trait]
impl NativeIntrospector for HttpIssuer {
    async fn inspect(&self, token: &str) -> Result<Introspection> {
        let mut response = self
            .client
            .post(&self.url)
            .basic_auth(&self.client_id, Some(self.secret.as_str()))
            .form(&[("token", token)])
            .send()
            .await
            .map_err(|_| anyhow!("issuer unavailable"))?;
        ensure!(response.status().is_success(), "issuer denied");
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("issuer unavailable"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= 16384,
                "oversized issuer response"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid issuer response"))?)
    }
}
fn listener(path: &Path) -> Result<(UnixListener, SocketGuard)> {
    let parent = std::fs::symlink_metadata(
        path.parent()
            .ok_or_else(|| anyhow!("missing native socket parent"))?,
    )?;
    ensure!(
        path.is_absolute()
            && parent.is_dir()
            && parent.uid() == unsafe { libc::geteuid() }
            && parent.mode() & 0o077 == 0,
        "invalid native socket directory"
    );
    // Never unlink a preexisting socket or take over another service.
    ensure!(!path.try_exists()?, "native socket already exists");
    let socket = UnixListener::bind(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    let guard = SocketGuard {
        path: path.into(),
        device: meta.dev(),
        inode: meta.ino(),
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok((socket, guard))
}
impl Runtime {
    pub async fn load(path: &Path, hotel: &str) -> Result<Arc<Self>> {
        ensure!(
            cfg!(target_os = "linux"),
            "native OAuth runtime requires Linux pinned ELF execution; portable contract tests remain available"
        );
        let c: Config = serde_json::from_slice(&private(path)?)?;
        validate_wiring(&c)?;
        let mut pins = BTreeMap::new();
        for worker in &c.workers {
            let executable = Artifact {
                path: worker.command.clone(),
                sha256: worker.sha256.clone(),
            };
            validate_artifact(&executable)?;
            let executable = pinned(&executable, true)?;
            let mut artifacts = Vec::new();
            for artifact in &worker.artifacts {
                validate_artifact(artifact)?;
                ensure!(
                    worker
                        .args
                        .iter()
                        .any(|a| Some(a.as_str()) == artifact.path.to_str()),
                    "pinned artifact must be an explicit argument"
                );
                artifacts.push((artifact.path.clone(), pinned(artifact, false)?));
            }
            pins.insert(
                worker.guest.clone(),
                Pins {
                    executable,
                    artifacts,
                },
            );
        }
        ensure!(
            c.hotel == hotel && c.control_socket != c.issuer_socket,
            "native hotel mismatch"
        );
        let issuer = reqwest::Url::parse(&c.binding.issuer)?;
        ensure!(
            issuer.scheme() == "https"
                && issuer.host_str().is_some()
                && issuer.username().is_empty()
                && issuer.password().is_none()
                && issuer.path() == "/"
                && issuer.query().is_none()
                && issuer.fragment().is_none()
                && !c.binding.issuer.ends_with('/'),
            "invalid issuer URL"
        );
        let secret = String::from_utf8(private(&c.introspection_secret_file)?)?;
        ensure!(
            secret.len() >= 32 && secret.len() <= 4096 && !secret.chars().any(char::is_control),
            "invalid introspection secret"
        );
        let mut client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(2));
        if let Some(path) = &c.introspection_ca_file {
            client = client.add_root_certificate(reqwest::Certificate::from_pem(&private(path)?)?);
        }
        let client = client.build()?;
        let inspect = Arc::new(HttpIssuer {
            client,
            url: format!("{}/oauth/introspect", c.binding.issuer),
            client_id: c.introspection_client,
            secret: zeroize::Zeroizing::new(secret),
        });
        let launches = Arc::new(LocalLaunchRegistry::default());
        let authority = NativeAuthority::new(
            c.hotel,
            c.membrane.clone(),
            c.runner.clone(),
            c.binding,
            launches.clone(),
            &c.journal,
            unsafe { libc::geteuid() },
            inspect,
        )?;
        ensure!(
            c.workers.len() == 3,
            "exactly three native protected workers required"
        );
        let workers: BTreeMap<_, _> = c
            .workers
            .into_iter()
            .map(|w| (w.guest.clone(), w))
            .collect();
        ensure!(
            workers.len() == 3
                && [&c.membrane, &c.runner, &c.issuer_guest]
                    .iter()
                    .all(|g| workers.contains_key(*g)),
            "native worker mapping mismatch"
        );
        let (control, control_guard) = listener(&c.control_socket)?;
        let (issuer, issuer_guard) = listener(&c.issuer_socket)?;
        let policy_path = c.policy_file;
        native_memory::Policy::load(&policy_path)?;
        let owner = authority.clone();
        let healthy = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let health = healthy.clone();
        let task = tokio::spawn(async move {
            let _ = owner
                .serve_control(
                    control,
                    Arc::new(move || Ok(native_memory::Policy::load(&policy_path)?)),
                )
                .await;
            health.store(false, std::sync::atomic::Ordering::SeqCst);
        });
        let owner = authority.clone();
        let health = healthy.clone();
        let issuer_guest = c.issuer_guest.clone();
        let owner_id = c.owner_id.clone();
        let issuer_task = tokio::spawn(async move {
            let _ = owner.serve_issuer(issuer, issuer_guest, owner_id).await;
            health.store(false, std::sync::atomic::Ordering::SeqCst);
        });
        Ok(Arc::new(Self {
            authority,
            launches,
            workers,
            tasks: vec![task, issuer_task],
            pins,
            _sockets: vec![control_guard, issuer_guard],
            healthy,
            issuer_guest: c.issuer_guest,
            issuer_socket: c.issuer_socket,
            owner_id: c.owner_id,
        }))
    }
    pub fn selected(&self, guest: &str) -> bool {
        self.workers.contains_key(guest)
    }
    pub fn healthy(&self) -> bool {
        self.healthy.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn prepare(
        &self,
        guest: &str,
        config: &serde_json::Value,
        command: &mut tokio::process::Command,
    ) -> Result<LaunchPrincipal> {
        let worker = self
            .workers
            .get(guest)
            .ok_or_else(|| anyhow!("unknown protected worker"))?;
        ensure!(
            worker.command.is_absolute()
                && config.get("command").and_then(|v| v.as_str()) == worker.command.to_str()
                && config.get("args") == Some(&serde_json::to_value(&worker.args)?),
            "protected launch command mismatch"
        );
        ensure!(self.healthy(), "native coordinator unavailable");
        use std::os::fd::AsRawFd;
        let pins = &self.pins[guest];
        *command =
            tokio::process::Command::new(format!("/proc/self/fd/{}", pins.executable.as_raw_fd()));
        let mut inherited = Vec::new();
        for arg in &worker.args {
            if let Some((_, file)) = pins
                .artifacts
                .iter()
                .find(|(path, _)| path.to_str() == Some(arg.as_str()))
            {
                inherited.push(file.as_raw_fd());
                command.arg(format!("/proc/self/fd/{}", file.as_raw_fd()));
            } else {
                command.arg(arg);
            }
        }
        command.env_clear();
        command.envs(&worker.environment);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if guest == self.issuer_guest {
            command
                // Keep the bundled CommonJS main script on its verified FD.
                .env("NODE_OPTIONS", "--preserve-symlinks-main")
                .env("PERSONAL_OAUTH_LIFEGRAPH_COORDINATION", "required")
                .env("PERSONAL_OAUTH_LIFEGRAPH_OWNER_SOCKET", &self.issuer_socket)
                .env("PERSONAL_OAUTH_LIFEGRAPH_OWNER_ID", &self.owner_id);
        }
        #[cfg(target_os = "linux")]
        unsafe {
            let parent = std::process::id() as libc::pid_t;
            command.pre_exec(move || {
                for fd in &inherited {
                    if libc::fcntl(*fd, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if libc::prctl(
                    libc::PR_SET_NO_NEW_PRIVS,
                    1 as libc::c_ulong,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong,
                ) != 0
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                    || libc::getppid() != parent
                {
                    return Err(std::io::Error::other("native parent unavailable"));
                }
                Ok(())
            });
        }
        command.kill_on_drop(true);
        Ok(LaunchPrincipal {
            stable_agent_id: worker.agent.clone(),
            roles: worker.roles.clone(),
        })
    }
}
fn argument<'a>(worker: &'a Worker, name: &str) -> Result<&'a str> {
    let positions: Vec<_> = worker
        .args
        .iter()
        .enumerate()
        .filter(|(_, v)| v.as_str() == name)
        .map(|(i, _)| i)
        .collect();
    ensure!(
        positions.len() == 1,
        "required protected argument absent or duplicated"
    );
    worker
        .args
        .get(positions[0] + 1)
        .map(|s| s.as_str())
        .ok_or_else(|| anyhow!("missing protected argument value"))
}
fn validate_wiring(c: &Config) -> Result<()> {
    ensure!(
        c.workers.len() == 3
            && c.membrane != c.runner
            && c.issuer_guest != c.membrane
            && c.issuer_guest != c.runner
            && !c.owner_id.is_empty()
            && c.owner_id.len() <= 128,
        "invalid native worker identities"
    );
    let workers: BTreeMap<_, _> = c.workers.iter().map(|w| (w.guest.as_str(), w)).collect();
    ensure!(workers.len() == 3, "duplicate protected guest");
    for worker in &c.workers {
        ensure!(
            !worker.agent.is_empty()
                && worker.args.len() <= 64
                && worker.environment.len() <= 64
                && worker.artifacts.len() <= 16,
            "protected worker bounds"
        );
        ensure!(
            worker.environment.keys().all(|k| !k.starts_with("LD_")
                && !k.starts_with("DYLD_")
                && !matches!(
                    k.as_str(),
                    "NODE_OPTIONS" | "NODE_PATH" | "BASH_ENV" | "ENV"
                )),
            "loader environment forbidden"
        );
    }
    let runner = workers
        .get(c.runner.as_str())
        .ok_or_else(|| anyhow!("runner missing"))?;
    ensure!(
        argument(runner, "--authority-socket")?
            == c.control_socket
                .to_str()
                .ok_or_else(|| anyhow!("invalid socket path"))?
            && argument(runner, "--policy")?
                == c.policy_file
                    .to_str()
                    .ok_or_else(|| anyhow!("invalid policy path"))?
            && argument(runner, "--node")? == c.hotel
            && argument(runner, "--guest")? == c.runner
            && !runner
                .args
                .iter()
                .any(|a| a.starts_with("--sqlite-fixture")),
        "runner enforcement mode mismatch"
    );
    let _ = argument(runner, "--memgraph-config")?;
    let membrane = workers
        .get(c.membrane.as_str())
        .ok_or_else(|| anyhow!("membrane missing"))?;
    ensure!(
        argument(membrane, "--node-id")? == c.hotel
            && argument(membrane, "--guest-id")? == c.membrane,
        "membrane identity mismatch"
    );
    let ingress = native_memory::IngressConfig::load(Path::new(argument(
        membrane,
        "--native-memory-config",
    )?))?;
    let resource = ingress
        .oauth
        .as_ref()
        .ok_or_else(|| anyhow!("membrane OAuth mode required"))?;
    ensure!(
        resource.authority_socket == c.control_socket
            && resource.resource == c.binding.resource
            && resource.issuer == c.binding.issuer,
        "membrane resource mismatch"
    );
    let grant = ingress
        .grants
        .get(&resource.grant_id)
        .ok_or_else(|| anyhow!("native grant missing"))?;
    let policy = native_memory::Policy::load(&c.policy_file)?;
    ensure!(
        policy
            .grants
            .get(&resource.grant_id)
            .is_some_and(|g| serde_json::to_value(g).ok() == serde_json::to_value(grant).ok())
            && grant.principal == c.binding.subject
            && grant.owner == c.binding.owner
            && grant.namespace == c.binding.namespace
            && grant.destination == c.hotel
            && grant.runner == c.runner
            && grant.reply_node == c.hotel
            && grant.reply_guest == c.membrane,
        "native authority mapping mismatch"
    );
    let issuer = workers
        .get(c.issuer_guest.as_str())
        .ok_or_else(|| anyhow!("issuer missing"))?;
    ensure!(
        issuer.artifacts.len() == 1
            && issuer
                .args
                .first()
                .is_some_and(
                    |arg| issuer.artifacts[0].path.to_str() == Some(arg.as_str())
                        && issuer.artifacts[0]
                            .path
                            .extension()
                            .is_some_and(|ext| ext == "cjs")
                ),
        "issuer requires a single pinned CommonJS bundle as its main argument"
    );
    ensure!(
        issuer
            .environment
            .get("PERSONAL_OAUTH_NATIVE_MEMORY_RESOURCE")
            == Some(&c.binding.resource),
        "issuer resource mismatch"
    );
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires explicit NODE_TEST_EXECUTABLE; executed by Linux qualification CI"]
    fn sealed_node_bundle_survives_source_replacement_and_cannot_be_written() {
        use sha2::{Digest, Sha256};
        use std::io::Write;
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let node = std::env::var("NODE_TEST_EXECUTABLE").expect("explicit Node test executable");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authority.cjs");
        let source = b"process.stdout.write('verified synthetic bundle')";
        std::fs::write(&path, source).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let mut snapshot = pinned(
            &Artifact {
                path: path.clone(),
                sha256: format!("{:x}", Sha256::digest(source)),
            },
            false,
        )
        .unwrap();
        assert!(snapshot.write_all(b"tamper").is_err());
        std::fs::rename(&path, dir.path().join("old.cjs")).unwrap();
        std::fs::write(&path, b"process.stdout.write('substituted')").unwrap();
        let fd = snapshot.as_raw_fd();
        let mut command = std::process::Command::new(node);
        command
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH")
            .arg("--preserve-symlinks-main")
            .arg(format!("/proc/self/fd/{fd}"));
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"verified synthetic bundle");
    }
}
