#![cfg(target_os = "linux")]
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

struct Fixture {
    dir: PathBuf,
    manifest: Value,
    contention: Arc<AtomicUsize>,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("synthetic-supervisor-{}", ulid::Ulid::new()));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join("uncertainty"), b"clean\n").unwrap();
        fs::write(
            dir.join("connection.json"),
            br#"{"uri":"127.0.0.1:1","user":"synthetic","password":""}"#,
        )
        .unwrap();
        for name in ["uncertainty", "connection.json"] {
            fs::set_permissions(dir.join(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
        let executable = dir.join("worker");
        fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o500)).unwrap();
        let digest = format!("{:x}", Sha256::digest(fs::read(&executable).unwrap()));
        let worker = |role: &str| {
            json!({"guest":role,"agent":format!("synthetic-{role}"),"roles":[],"role":role,"count":1,
            "executable":{"path":executable,"sha256":digest},"artifacts":[],"directory":dir,
            "arguments":["--ignored","--exact","supervised_fixture"],
            "environment":{"SYNTHETIC_SUPERVISOR_DIR":dir,"SYNTHETIC_SUPERVISOR_ROLE":role}})
        };
        let manifest = json!({"version":1,"hotel":"synthetic-hotel","owner":"synthetic-owner","uid":fs::metadata(&dir).unwrap().uid(),
            "socket":dir.join("owner.sock"),"journal":dir.join("uncertainty"),"graph_connection_file":dir.join("connection.json"),
            "workers":[worker("gateway"),worker("issuer")]});
        Self {
            dir,
            manifest,
            contention: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn save(&self) {
        fs::write(
            self.dir.join("manifest.json"),
            serde_json::to_vec(&self.manifest).unwrap(),
        )
        .unwrap();
        fs::set_permissions(
            self.dir.join("manifest.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    fn spawn(&self) -> Child {
        self.save();
        let mut child = Command::new(env!("CARGO_BIN_EXE_lifegraph-read-supervisor"))
            .env_clear()
            .arg("--config")
            .arg(self.dir.join("manifest.json"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let count = self.contention.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if matches!(line.as_deref(), Ok("LifeGraph owner fence contended")) {
                    count.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        child
    }
    fn wait_contention(&self, before: usize) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.contention.load(Ordering::SeqCst) <= before {
            assert!(
                Instant::now() < deadline,
                "owner did not register contending request"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn assert_blocked(&self, marker: &str) {
        // Arrival was already proved by the owner's registered waiter event.
        // Observe exclusion long enough to catch a wrongly granted response;
        // this interval is not used to infer that a request arrived.
        let until = Instant::now() + Duration::from_millis(250);
        while Instant::now() < until {
            assert!(
                !self.dir.join(marker).exists(),
                "contending lease was admitted: {marker}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn trigger(&self, name: &str) {
        fs::write(self.dir.join(name), b"synthetic").unwrap();
    }
    fn wait(&self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !self.dir.join(name).exists() {
            assert!(
                Instant::now() < deadline,
                "synthetic deadline waiting for {name}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn journal(&self) -> Vec<u8> {
        fs::read(self.dir.join("uncertainty")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(9);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("supervisor exit deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn terminate(child: &mut Child) {
    unsafe {
        assert_eq!(libc::kill(child.id() as i32, libc::SIGTERM), 0);
    }
    assert!(exit(child).success());
}
fn request(stream: &mut UnixStream, sequence: u64, operation: &str) -> Value {
    let body=serde_json::to_vec(&json!({"version":1,"owner":"synthetic-owner","session":"00000000-0000-0000-0000-000000000001","sequence":sequence,"operation":operation,"parameters":{}})).unwrap();
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&body).unwrap();
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let len = u32::from_be_bytes(header) as usize;
    assert!(len < 4096);
    let mut reply = vec![0; len];
    stream.read_exact(&mut reply).unwrap();
    serde_json::from_slice(&reply).unwrap()
}
#[test]
#[ignore = "only launched as an actual supervisor-owned synthetic child"]
fn supervised_fixture() {
    unsafe {
        assert_eq!(libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0), 1);
    }
    assert_eq!(
        std::env::var("PERSONAL_OAUTH_LIFEGRAPH_COORDINATION").unwrap(),
        "required"
    );
    assert_eq!(
        std::env::var("PERSONAL_OAUTH_LIFEGRAPH_OWNER_ID").unwrap(),
        "synthetic-owner"
    );
    let dir = PathBuf::from(std::env::var("SYNTHETIC_SUPERVISOR_DIR").unwrap());
    let role = std::env::var("SYNTHETIC_SUPERVISOR_ROLE").unwrap();
    fs::write(
        dir.join(format!("ready-{role}")),
        std::process::id().to_string(),
    )
    .unwrap();
    let wait = |name: &str| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !dir.join(name).exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    wait(&format!("start-{role}"));
    let mut socket =
        UnixStream::connect(std::env::var("PERSONAL_OAUTH_LIFEGRAPH_OWNER_SOCKET").unwrap())
            .unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let begin = if role == "issuer" {
        "begin_issuer_change"
    } else {
        "begin_release"
    };
    assert_eq!(request(&mut socket, 1, begin)["result"], true);
    fs::write(dir.join(format!("held-{role}")), b"synthetic").unwrap();
    wait(&format!("finish-{role}"));
    if dir.join(format!("abort-{role}")).exists() {
        return;
    }
    let end = if role == "issuer" {
        "end_issuer_change"
    } else {
        "end_release"
    };
    assert_eq!(request(&mut socket, 2, end)["result"], true);
    fs::write(dir.join(format!("completed-{role}")), b"synthetic").unwrap();
    // Required workers remain live until the supervisor terminates them.
    wait("never-finish");
}
#[test]
fn configuration_and_digest_fail_before_workers_or_socket() {
    for mutation in 0..6 {
        let mut f = Fixture::new();
        match mutation {
            0 => f.manifest["unknown"] = json!(true),
            1 => f.manifest["workers"][0]["count"] = json!(17),
            2 => f.manifest["workers"][0]["executable"]["sha256"] = json!("0".repeat(64)),
            3 => {
                f.manifest["workers"][0]["environment"]["PERSONAL_OAUTH_LIFEGRAPH_COORDINATION"] =
                    json!("disabled")
            }
            4 => fs::set_permissions(&f.dir, fs::Permissions::from_mode(0o755)).unwrap(),
            _ => fs::set_permissions(f.dir.join("worker"), fs::Permissions::from_mode(0o6500))
                .unwrap(),
        }
        let mut c = f.spawn();
        assert!(!exit(&mut c).success());
        assert!(!f.dir.join("ready-gateway").exists());
        assert!(!f.dir.join("owner.sock").exists());
        assert_eq!(f.journal(), b"clean\n");
    }
}
#[test]
fn partial_spawn_failure_reaps_generation_and_removes_only_its_socket() {
    let mut f = Fixture::new();
    let broken = f.dir.join("broken-worker");
    fs::write(&broken, b"\x7fELFsynthetic-invalid-executable").unwrap();
    fs::set_permissions(&broken, fs::Permissions::from_mode(0o500)).unwrap();
    f.manifest["workers"][1]["executable"] =
        json!({"path":broken,"sha256":format!("{:x}",Sha256::digest(fs::read(&broken).unwrap()))});
    let mut c = f.spawn();
    assert!(!exit(&mut c).success());
    assert!(!f.dir.join("owner.sock").exists());
    assert_eq!(f.journal(), b"clean\n");
}
#[test]
fn real_children_serialize_and_clean_shutdown_can_restart() {
    let f = Fixture::new();
    let mut c = f.spawn();
    f.wait("ready-gateway");
    f.wait("ready-issuer");
    f.trigger("start-gateway");
    f.wait("held-gateway");
    let before = f.contention.load(Ordering::SeqCst);
    f.trigger("start-issuer");
    f.wait_contention(before);
    f.assert_blocked("held-issuer");
    f.trigger("finish-gateway");
    f.wait("completed-gateway");
    f.wait("held-issuer");
    f.trigger("finish-issuer");
    f.wait("completed-issuer");
    assert_eq!(f.journal(), b"clean\n");
    terminate(&mut c);
    assert!(!f.dir.join("owner.sock").exists());
    for name in [
        "start-gateway",
        "start-issuer",
        "finish-gateway",
        "finish-issuer",
        "held-gateway",
        "held-issuer",
        "completed-gateway",
        "completed-issuer",
        "ready-gateway",
        "ready-issuer",
    ] {
        fs::remove_file(f.dir.join(name)).unwrap();
    }
    let mut restarted = f.spawn();
    f.wait("ready-gateway");
    f.wait("ready-issuer");
    terminate(&mut restarted);
    assert_eq!(f.journal(), b"clean\n");
}
#[test]
fn fence_only_serializes_without_graph_credentials() {
    let mut f = Fixture::new();
    f.manifest["version"] = json!(2);
    f.manifest["profile"] = json!("fence_only");
    f.manifest
        .as_object_mut()
        .unwrap()
        .remove("graph_connection_file");
    fs::remove_file(f.dir.join("connection.json")).unwrap();
    let mut c = f.spawn();
    f.wait("ready-gateway");
    f.wait("ready-issuer");
    f.trigger("start-gateway");
    f.wait("held-gateway");
    let before = f.contention.load(Ordering::SeqCst);
    f.trigger("start-issuer");
    f.wait_contention(before);
    f.assert_blocked("held-issuer");
    f.trigger("finish-gateway");
    f.wait("completed-gateway");
    f.wait("held-issuer");
    f.trigger("finish-issuer");
    f.wait("completed-issuer");
    terminate(&mut c);
    assert_eq!(f.journal(), b"clean\n");
}

#[test]
fn fence_only_refuses_mixed_profiles_before_spawn() {
    for version in [1, 2] {
        let mut f = Fixture::new();
        f.manifest["version"] = json!(version);
        f.manifest["profile"] = json!("fence_only");
        let mut c = f.spawn();
        assert!(!exit(&mut c).success());
        assert!(!f.dir.join("ready-gateway").exists());
        assert!(!f.dir.join("owner.sock").exists());
    }
}
#[test]
fn fence_only_unknown_mutation_denies_restart() {
    let mut f = Fixture::new();
    f.manifest["version"] = json!(2);
    f.manifest["profile"] = json!("fence_only");
    f.manifest
        .as_object_mut()
        .unwrap()
        .remove("graph_connection_file");
    fs::remove_file(f.dir.join("connection.json")).unwrap();
    let mut c = f.spawn();
    f.wait("ready-gateway");
    f.wait("ready-issuer");
    f.trigger("start-issuer");
    f.wait("held-issuer");
    f.trigger("abort-issuer");
    f.trigger("finish-issuer");
    assert!(!exit(&mut c).success());
    assert_eq!(f.journal(), b"dirty\n");
    let mut restarted = f.spawn();
    assert!(!exit(&mut restarted).success());
    assert_eq!(f.journal(), b"dirty\n");
}
#[test]
fn required_worker_exit_with_unknown_mutation_denies_restart() {
    let f = Fixture::new();
    let mut c = f.spawn();
    f.wait("ready-gateway");
    f.wait("ready-issuer");
    f.trigger("start-issuer");
    f.wait("held-issuer");
    f.trigger("abort-issuer");
    f.trigger("finish-issuer");
    assert!(!exit(&mut c).success());
    assert_eq!(f.journal(), b"dirty\n");
    let mut restarted = f.spawn();
    assert!(!exit(&mut restarted).success());
    assert_eq!(f.journal(), b"dirty\n");
}
#[test]
fn existing_socket_and_second_owner_are_never_replaced() {
    let f = Fixture::new();
    let _socket = std::os::unix::net::UnixListener::bind(f.dir.join("owner.sock")).unwrap();
    let inode = fs::metadata(f.dir.join("owner.sock")).unwrap().ino();
    let mut c = f.spawn();
    assert!(!exit(&mut c).success());
    assert_eq!(fs::metadata(f.dir.join("owner.sock")).unwrap().ino(), inode);
    drop(_socket);
    fs::remove_file(f.dir.join("owner.sock")).unwrap();
    let mut first = f.spawn();
    f.wait("ready-gateway");
    f.wait("ready-issuer");
    let mut second = f.spawn();
    assert!(!exit(&mut second).success());
    terminate(&mut first);
}
#[test]
fn unenrolled_peer_cannot_begin_a_mutation() {
    let f = Fixture::new();
    let mut c = f.spawn();
    f.wait("ready-gateway");
    f.wait("ready-issuer");
    let mut socket = UnixStream::connect(f.dir.join("owner.sock")).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    // Valid protocol input must be rejected specifically by kernel enrollment,
    // not JSON parsing, missing fields, owner mismatch or a timeout.
    let body = serde_json::to_vec(&json!({"version":1,"owner":"synthetic-owner",
        "session":"00000000-0000-0000-0000-000000000001","sequence":1,
        "operation":"begin_issuer_change","parameters":{}}))
    .unwrap();
    // Authentication may close before the write; read must still prove closure.
    let _ = socket.write_all(&(body.len() as u32).to_be_bytes());
    let _ = socket.write_all(&body);
    let mut b = [0; 1];
    match socket.read(&mut b) {
        Ok(0) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ) => {}
        other => panic!("unenrolled peer was not explicitly rejected: {other:?}"),
    }
    assert_eq!(f.journal(), b"clean\n");
    // Same-generation positive control: the identical valid operation from a
    // supervisor-enrolled child must succeed and complete its lease cleanly.
    f.trigger("start-issuer");
    f.wait("held-issuer");
    f.trigger("finish-issuer");
    f.wait("completed-issuer");
    assert_eq!(f.journal(), b"clean\n");
    terminate(&mut c);
}
#[test]
fn shutdown_during_unacknowledged_lease_preserves_dirty_marker() {
    let f = Fixture::new();
    let mut c = f.spawn();
    f.wait("ready-gateway");
    f.wait("ready-issuer");
    f.trigger("start-gateway");
    f.wait("held-gateway");
    terminate(&mut c);
    assert_eq!(f.journal(), b"dirty\n");
    assert!(!f.dir.join("owner.sock").exists());
}
#[test]
fn supervisor_crash_kills_direct_workers_and_never_cleans_uncertainty() {
    let f = Fixture::new();
    let mut c = f.spawn();
    f.wait("ready-gateway");
    f.wait("ready-issuer");
    let pids: [u32; 2] = ["gateway", "issuer"].map(|role| {
        fs::read_to_string(f.dir.join(format!("ready-{role}")))
            .unwrap()
            .parse()
            .unwrap()
    });
    f.trigger("start-issuer");
    f.wait("held-issuer");
    c.kill().unwrap();
    assert!(!exit(&mut c).success());
    let deadline = Instant::now() + Duration::from_secs(3);
    for pid in pids {
        loop {
            match fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(_) => break,
                Ok(stat)
                    if stat.rsplit_once(')').unwrap().1.split_whitespace().next() == Some("Z") =>
                {
                    break;
                }
                _ => {
                    assert!(
                        Instant::now() < deadline,
                        "direct worker survived supervisor crash"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }
    assert_eq!(f.journal(), b"dirty\n");
    assert!(f.dir.join("owner.sock").exists());
    // Explicit fixture-only stale socket removal demonstrates restart denial
    // comes from durable uncertainty, rather than merely an occupied path.
    fs::remove_file(f.dir.join("owner.sock")).unwrap();
    let mut restarted = f.spawn();
    assert!(!exit(&mut restarted).success());
    assert_eq!(f.journal(), b"dirty\n");
}

#[test]
fn issuer_lease_also_blocks_gateway_release() {
    let f = Fixture::new();
    let mut c = f.spawn();
    f.wait("ready-gateway");
    f.wait("ready-issuer");
    f.trigger("start-issuer");
    f.wait("held-issuer");
    let before = f.contention.load(Ordering::SeqCst);
    f.trigger("start-gateway");
    f.wait_contention(before);
    f.assert_blocked("held-gateway");
    f.trigger("finish-issuer");
    f.wait("completed-issuer");
    f.wait("held-gateway");
    f.trigger("finish-gateway");
    f.wait("completed-gateway");
    terminate(&mut c);
    assert_eq!(f.journal(), b"clean\n");
}

#[test]
fn shutdown_allows_existing_lease_to_acknowledge_clean_completion() {
    let f = Fixture::new();
    let mut c = f.spawn();
    f.wait("ready-gateway");
    f.wait("ready-issuer");
    f.trigger("start-issuer");
    f.wait("held-issuer");
    unsafe {
        assert_eq!(libc::kill(c.id() as i32, libc::SIGTERM), 0);
    }
    f.trigger("finish-issuer");
    assert!(exit(&mut c).success());
    assert!(f.dir.join("completed-issuer").exists());
    assert_eq!(f.journal(), b"clean\n");
}
