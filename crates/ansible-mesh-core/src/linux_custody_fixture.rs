//! Explicitly ignored Linux-only synthetic custody test; no production entrypoint.
//! Fresh disposable hosted VM and root are external preconditions, not proven
//! by environment strings. It does not test systemd, ACLs or production custody.
use anyhow::{bail, ensure, Context, Result};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

const PREFIX: &str = "philotic-custody-fixture-";
const CHILD_ENV: &str = "PHILOTIC_CUSTODY_FIXTURE_CHILD";
const ROOT_ENV: &str = "PHILOTIC_CUSTODY_FIXTURE_ROOT";
const UID_ENV: &str = "PHILOTIC_CUSTODY_FIXTURE_UID";

fn fixture_path(path: &Path) -> Result<()> {
    ensure!(
        path.parent() == Some(Path::new("/run")),
        "fixture parent mismatch"
    );
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .context("fixture name")?;
    ensure!(
        name.starts_with(PREFIX) && name.len() > PREFIX.len(),
        "fixture name mismatch"
    );
    Ok(())
}

fn cleanup_target_matches(
    parent: &Path,
    root: &Path,
    parent_before: &fs::Metadata,
    root_before: &fs::Metadata,
    owner: u32,
    children_reaped: bool,
) -> bool {
    children_reaped
        && fs::symlink_metadata(parent).ok().is_some_and(|m| {
            m.file_type().is_dir()
                && m.uid() == owner
                && m.mode() & 0o022 == 0
                && m.dev() == parent_before.dev()
                && m.ino() == parent_before.ino()
        })
        && fs::symlink_metadata(root).ok().is_some_and(|m| {
            m.file_type().is_dir()
                && m.uid() == owner
                && m.dev() == root_before.dev()
                && m.ino() == root_before.ino()
        })
}

#[test]
fn fixture_path_guard_accepts_only_scoped_synthetic_names() {
    assert!(fixture_path(Path::new("/run/philotic-custody-fixture-abc123")).is_ok());
    for path in [
        "/run",
        "/etc/philotic-custody-fixture-x",
        "/run/philotic-custody-fixture-",
        "/run/../etc/passwd",
        "/run/philotic-custody-fixture-x/child",
        "relative",
    ] {
        assert!(fixture_path(Path::new(path)).is_err());
    }
}

#[test]
fn fixture_cleanup_refuses_changed_identity_or_unreaped_child() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let parent = dir.path();
    let root = parent.join("owned");
    fs::create_dir(&root)?;
    let parent_before = fs::symlink_metadata(parent)?;
    let root_before = fs::symlink_metadata(&root)?;
    let owner = root_before.uid();
    assert!(cleanup_target_matches(
        parent,
        &root,
        &parent_before,
        &root_before,
        owner,
        true
    ));
    assert!(!cleanup_target_matches(
        parent,
        &root,
        &parent_before,
        &root_before,
        owner,
        false
    ));
    fs::rename(&root, parent.join("original"))?;
    fs::create_dir(&root)?;
    assert!(!cleanup_target_matches(
        parent,
        &root,
        &parent_before,
        &root_before,
        owner,
        true
    ));
    assert!(root.exists());
    Ok(())
}

fn fixture_child_path() -> Result<PathBuf> {
    let p = PathBuf::from(std::env::var_os(ROOT_ENV).context("fixture path absent")?);
    fixture_path(&p)?;
    let meta = fs::symlink_metadata(&p).context("fixture directory unavailable")?;
    ensure!(
        meta.file_type().is_dir() && meta.uid() == 0 && meta.mode() & 0o7777 == 0o755,
        "fixture directory identity or mode invalid"
    );
    Ok(p)
}

struct OwnedChild(Child, Arc<AtomicBool>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        match self.0.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                let _ = self.0.kill();
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match self.0.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(20))
                        }
                        _ => {
                            self.1.store(false, Ordering::SeqCst);
                            break;
                        }
                    }
                }
            }
            Err(_) => self.1.store(false, Ordering::SeqCst),
        }
    }
}

fn start_child(
    test: &str,
    root: &Path,
    role: &str,
    uid: Option<u32>,
    cleanup_ok: &Arc<AtomicBool>,
) -> Result<OwnedChild> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", test, "--ignored", "--nocapture"])
        .env_clear()
        .env(CHILD_ENV, role)
        .env(ROOT_ENV, root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(uid) = uid {
        command.env(UID_ENV, uid.to_string());
    }
    Ok(OwnedChild(
        command.spawn().context("synthetic child spawn failed")?,
        Arc::clone(cleanup_ok),
    ))
}

fn await_ready(child: &mut OwnedChild, marker: &'static str) -> Result<()> {
    let stdout = child.0.stdout.take().context("child stdout unavailable")?;
    let (send, recv) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout.take(4096));
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    let _ = send.send(false);
                    break;
                }
                Ok(_) if line.trim_end().ends_with(marker) => {
                    let _ = send.send(true);
                    let _ = std::io::copy(&mut reader, &mut std::io::sink());
                    break;
                }
                Ok(_) => {}
            }
        }
    });
    ensure!(
        recv.recv_timeout(Duration::from_secs(15))
            .context("child readiness timeout")?,
        "child readiness missing"
    );
    Ok(())
}

fn await_exit(child: &mut OwnedChild) -> Result<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.0.try_wait()? {
            return Ok(status);
        }
        ensure!(Instant::now() < deadline, "synthetic child timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn await_denial_result(child: &mut OwnedChild) -> Result<()> {
    let status = await_exit(child)?;
    let mut output = String::new();
    child
        .0
        .stdout
        .take()
        .context("child stdout unavailable")?
        .take(16_384)
        .read_to_string(&mut output)?;
    ensure!(
        status.success() && output.contains("test result: ok. 1 passed;"),
        "untrusted writer result uncertain"
    );
    Ok(())
}

fn drop_to_fixture_uid() -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "fixture must start as root"
    );
    let uid: u32 = std::env::var(UID_ENV)?.parse()?;
    ensure!(
        uid == 65534 || uid == 65533,
        "synthetic unprivileged uid required"
    );
    // The disposable child drops all supplementary groups before its gid and
    // uid. Failure is inconclusive; it is never weakened to continue.
    ensure!(
        unsafe { libc::setgroups(0, std::ptr::null()) } == 0,
        "group drop failed"
    );
    ensure!(unsafe { libc::setgid(uid) } == 0, "gid drop failed");
    ensure!(unsafe { libc::setuid(uid) } == 0, "uid drop failed");
    ensure!(unsafe { libc::geteuid() } == uid, "uid drop unconfirmed");
    ensure!(
        unsafe { libc::getgroups(0, std::ptr::null_mut()) } == 0,
        "supplemental groups remain"
    );
    Ok(())
}

#[test]
#[ignore = "owned child only; launched by the disposable Linux test"]
fn untrusted_writer() -> Result<()> {
    ensure!(cfg!(target_os = "linux"), "Linux fixture required");
    ensure!(
        std::env::var(CHILD_ENV).as_deref() == Ok("untrusted"),
        "child role required"
    );
    let root = fixture_child_path()?;
    drop_to_fixture_uid()?;
    let db = root.join("state.db");
    let wal = root.join("state.db-wal");
    for result in [
        OpenOptions::new().write(true).open(&db).map(|_| ()),
        fs::rename(&db, root.join("replaced.db")),
        fs::hard_link(&db, root.join("linked.db")),
        OpenOptions::new().write(true).open(&wal).map(|_| ()),
        OpenOptions::new()
            .write(true)
            .open(root.join("retained.sidecar"))
            .map(|_| ()),
        fs::rename(&wal, root.join("replaced-wal")),
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join("state.db-shm"))
            .map(|_| ()),
    ] {
        match result {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
            _ => bail!("expected permission denial absent"),
        }
    }
    Ok(())
}

#[test]
#[ignore = "owned child only; launched by the disposable Linux test"]
fn retained_writer() -> Result<()> {
    ensure!(cfg!(target_os = "linux"), "Linux fixture required");
    ensure!(
        std::env::var(CHILD_ENV).as_deref() == Ok("retained"),
        "child role required"
    );
    let root = fixture_child_path()?;
    drop_to_fixture_uid()?;
    let mut held = OpenOptions::new()
        .write(true)
        .open(root.join("retained.sidecar"))?;
    println!("SYNTHETIC_DESCRIPTOR_READY");
    std::io::stdout().flush()?;
    let mut proceed = [0u8; 1];
    std::io::stdin().read_exact(&mut proceed)?;
    ensure!(proceed == [1], "fixture signal invalid");
    held.write_all(b"after!")?;
    held.sync_all()?;
    Ok(())
}

#[test]
#[ignore = "owned child only; launched by the disposable Linux test"]
fn interrupted_writer() -> Result<()> {
    ensure!(cfg!(target_os = "linux"), "Linux fixture required");
    ensure!(
        std::env::var(CHILD_ENV).as_deref() == Ok("writer"),
        "child role required"
    );
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "synthetic root writer required"
    );
    let root = fixture_child_path()?;
    let conn = rusqlite::Connection::open(root.join("state.db"))?;
    conn.execute_batch("BEGIN IMMEDIATE; UPDATE fixture_state SET value=2 WHERE id=1;")?;
    println!("SYNTHETIC_TRANSACTION_READY");
    std::io::stdout().flush()?;
    std::thread::sleep(Duration::from_secs(60));
    bail!("parent failed to interrupt synthetic writer")
}

/// An isolated root-owned SQLite fixture. The runner must be a fresh disposable
/// Linux VM; environment strings are an accidental-execution guard only. No
/// account, systemd unit, socket, network resource or production data is used.
#[test]
#[ignore = "requires separately reviewed disposable Linux root execution"]
fn disposable_linux_filesystem_and_interruption() -> Result<()> {
    ensure!(cfg!(target_os = "linux"), "Linux fixture required");
    ensure!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
            && std::env::var("PHILOTIC_DISPOSABLE_LINUX_CUSTODY").as_deref() == Ok("1")
            && unsafe { libc::geteuid() } == 0,
        "fresh hosted disposable root runner required"
    );
    println!("synthetic_sqlite_version={}", rusqlite::version());
    let parent = fs::symlink_metadata("/run").context("synthetic runtime parent unavailable")?;
    ensure!(
        parent.file_type().is_dir() && parent.uid() == 0 && parent.mode() & 0o022 == 0,
        "synthetic runtime parent unsafe"
    );
    // Disarm automatic recursive cleanup immediately. Every later deletion
    // goes through explicit identity and child-reaping checks.
    let root = tempfile::Builder::new()
        .prefix(PREFIX)
        .tempdir_in("/run")?
        .keep();
    let created = fs::symlink_metadata(&root).context("synthetic fixture identity unavailable")?;
    ensure!(
        created.file_type().is_dir() && created.uid() == 0,
        "synthetic fixture identity invalid"
    );
    let children_reaped = Arc::new(AtomicBool::new(true));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        fixture_path(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755))?;
        let db = root.join("state.db");
        let conn = rusqlite::Connection::open(&db)?;
        conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; CREATE TABLE fixture_state(id INTEGER PRIMARY KEY, value INTEGER NOT NULL); INSERT INTO fixture_state VALUES(1,1);")?;
        drop(conn);
        fs::set_permissions(&db, fs::Permissions::from_mode(0o600))?;
        let wal = root.join("state.db-wal");
        fs::write(&wal, b"synthetic sidecar")?;
        fs::set_permissions(&wal, fs::Permissions::from_mode(0o600))?;
        // This deliberately unsafe transition demonstrates why changing a
        // mode cannot evict a writer that already has a writable descriptor.
        let retained = root.join("retained.sidecar");
        fs::write(&retained, b"before")?;
        fs::set_permissions(&retained, fs::Permissions::from_mode(0o666))?;
        let mut prior_writer = start_child(
            "linux_custody_fixture::retained_writer", &root, "retained", Some(65534), &children_reaped,
        )?;
        await_ready(&mut prior_writer, "SYNTHETIC_DESCRIPTOR_READY")?;
        fs::set_permissions(&retained, fs::Permissions::from_mode(0o600))?;
        let mut signal = prior_writer.0.stdin.take().context("retained writer stdin unavailable")?;
        signal.write_all(&[1])?;
        drop(signal);
        ensure!(await_exit(&mut prior_writer)?.success(), "retained writer did not exit cleanly");
        drop(prior_writer);
        ensure!(fs::read(&retained)? == b"after!", "retained descriptor behavior not confirmed");
        // Only after the known pre-existing writer has been reaped do we run
        // the denied-new-writer and interrupted maintenance checks.
        for uid in [65534, 65533] {
            let mut child = start_child(
                "linux_custody_fixture::untrusted_writer",
                &root,
                "untrusted",
                Some(uid),
                &children_reaped,
            )?;
            await_denial_result(&mut child)?;
        }
        ensure!(
            fs::read(&wal)? == b"synthetic sidecar",
            "synthetic sidecar changed"
        );
        fs::remove_file(&wal)?;
        let mut writer = start_child(
            "linux_custody_fixture::interrupted_writer",
            &root,
            "writer",
            None,
            &children_reaped,
        )?;
        await_ready(&mut writer, "SYNTHETIC_TRANSACTION_READY")?;
        writer.0.kill().context("synthetic writer stop failed")?;
        ensure!(await_exit(&mut writer)?.signal() == Some(libc::SIGKILL),
            "synthetic writer was not killed as expected");
        let conn = rusqlite::Connection::open(&db)?;
        let value: i64 =
            conn.query_row("SELECT value FROM fixture_state WHERE id=1", [], |row| {
                row.get(0)
            })?;
        ensure!(value == 1, "uncommitted synthetic write survived");
        drop(conn);
        Ok(())
    })).map_err(|_| anyhow::anyhow!("synthetic fixture panicked")).and_then(|r| r);
    if !cleanup_target_matches(
        Path::new("/run"),
        &root,
        &parent,
        &created,
        0,
        children_reaped.load(Ordering::SeqCst),
    ) {
        bail!("synthetic fixture identity or child teardown uncertain; cleanup not attempted");
    }
    let cleanup = fs::remove_dir_all(&root)
        .context("synthetic fixture cleanup uncertain")
        .and_then(|()| match fs::symlink_metadata(&root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            _ => bail!("synthetic fixture removal unconfirmed"),
        });
    result.and(cleanup)
}
