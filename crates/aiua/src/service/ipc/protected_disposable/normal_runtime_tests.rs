use super::inspection::Inspection;
use super::*;
use ansible_mesh_core::privacy_local::LaunchPrincipal;
use graph_intelligence::local_work::{Authority, LocalWork, Phase, Scope, Work};
use philotic_client::development_inspection::{ACTION, Snapshot};
use std::{
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};
const TASK: &str = "synthetic:task:normal-inspection";
const AUTH: &str = "synthetic:charter:normal-inspection";
fn snapshot() -> Snapshot {
    Snapshot {
        cargo: include_bytes!("../../../../../../Cargo.toml").to_vec(),
        readme: include_bytes!("../../../../../../README.md").to_vec(),
    }
}
fn scope() -> Scope {
    Scope {
        action: ACTION.into(),
        resource: snapshot().digest().unwrap(),
        destination: format!("{TASK}:receipt"),
    }
}
fn authority(revoked: bool) -> Authority {
    Authority {
        authenticated_authorizer_ref: "synthetic:authorizer:local-fixture".into(),
        scopes: vec![scope()],
        valid_from: 0,
        expires_at: 10_000_000,
        revoked,
        may_delegate: false,
        parent: None,
        revision: 0,
    }
}
fn seed(path: &Path, max_attempts: u64, budget_ms: u64) -> LocalWork {
    let mut l = LocalWork::open(path.to_str().unwrap()).unwrap();
    l.set_authority(AUTH, authority(false)).unwrap();
    l.create(
        TASK,
        Work {
            checkpoint: None,
            slice: "synthetic:slice:normal-inspection".into(),
            role_charter: AUTH.into(),
            revision: std::env::var("PHILOTIC_INSPECTION_REVISION")
                .expect("exact source revision required"),
            environment: format!(
                "{}:{}:normal-runtime",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            fence: None,
            operation: None,
            attempts: 0,
            max_attempts,
            active_ms: 0,
            budget_ms,
            stopped: None,
            implemented: true,
            tested: false,
            merged: false,
            deployed: false,
            live_verified: false,
        },
    )
    .unwrap();
    l
}
fn adapter(mut l: LocalWork, now: u64) -> Arc<Inspection> {
    let f = l
        .claim(TASK, &format!("synthetic:session:{now}"), now, 100_000)
        .unwrap();
    Arc::new(Inspection::new(l, f, now, TASK.into(), snapshot()).unwrap())
}
async fn launch(
    binary: &str,
    dir: &Path,
    label: &str,
    i: Arc<Inspection>,
) -> (
    crate::service::protected_local_guest::ProtectedLocalGuest,
    tokio::task::JoinHandle<()>,
    PathBuf,
) {
    let socket = dir.join(format!("{label}.sock"));
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let registry = Arc::new(LocalLaunchRegistry::default());
    let contract = DisposableContract {
        hotel: "fixture".into(),
        guest: "agent".into(),
        agent: "agent".into(),
        role: "agent".into(),
        destination_role: ACTION.into(),
        destination_guest: TASK.into(),
        operation_json: "{}".into(),
        checkpoint_key: "unused".into(),
        receipt_key: "unused-receipt".into(),
        expires_at: Instant::now() + Duration::from_secs(60),
    };
    let endpoint = ProtectedDisposable::new(registry.clone(), contract)
        .unwrap()
        .with_inspection(i);
    let output = dir.join(format!("{label}.stdout"));
    let mut cmd = tokio::process::Command::new(binary);
    cmd.env_clear()
        .env("PHILOTIC_AGENT_ID", "agent")
        .env("PHILOTIC_HOTEL_SOCKET", &socket)
        .env("PHILOTIC_LOCAL_INSPECTION", "1")
        .env(
            "PHILOTIC_INSPECTION_PARENT_PID",
            std::process::id().to_string(),
        )
        .env(
            "PHILOTIC_INSPECTION_PARENT_UID",
            unsafe { libc::geteuid() }.to_string(),
        )
        .stdout(std::fs::File::create(&output).unwrap())
        .stderr(std::fs::File::create(dir.join(format!("{label}.stderr"))).unwrap());
    let child = crate::service::protected_local_guest::ProtectedLocalGuest::spawn(
        &mut cmd,
        registry,
        "agent",
        unsafe { libc::geteuid() },
        LaunchPrincipal {
            stable_agent_id: "agent".into(),
            roles: ["agent".into()].into_iter().collect(),
        },
    )
    .unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let store =
            ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(":memory:").unwrap();
        let graph = Arc::new(GraphDomain::new(Arc::new(store.adapter())));
        let _ = endpoint.serve(stream, graph).await;
    });
    (child, server, output)
}
async fn marker(path: &Path, text: &str, count: usize) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if std::fs::read_to_string(path)
                .unwrap_or_default()
                .matches(text)
                .count()
                >= count
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "missing {text}: {}",
            std::fs::read_to_string(path).unwrap_or_default()
        )
    });
}
fn receipt_count(db: &Path) -> u64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM local_inspection_receipts", [], |r| {
            r.get(0)
        })
        .unwrap()
}
async fn stop(
    child: crate::service::protected_local_guest::ProtectedLocalGuest,
    server: tokio::task::JoinHandle<()>,
    dir: &Path,
    label: &str,
) {
    child.stop().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_file(dir.join(format!("{label}.sock"))).unwrap();
}

#[tokio::test]
#[ignore = "requires actual default-path PHILOTIC_INSPECTION_BINARY and exact revision"]
async fn actual_normal_runtime_inspection_recovery() {
    let binary = std::env::var("PHILOTIC_INSPECTION_BINARY").unwrap();
    let dir = PathBuf::from(format!("/tmp/nr-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&dir).unwrap();
    println!("NORMAL_FIXTURE_DIR {}", dir.display());
    let db = dir.join("main.db");
    let i = adapter(seed(&db, 3, 100), 0);
    i.hold.store(true, Ordering::SeqCst);
    let (child, server, out) = launch(&binary, &dir, "interrupted", i.clone()).await;
    tokio::time::timeout(Duration::from_secs(15), i.reached.notified())
        .await
        .unwrap();
    marker(&out, "NORMAL_INSPECTION_COMPUTED", 1).await;
    child.stop().await.unwrap();
    i.release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_file(dir.join("interrupted.sock")).unwrap();
    drop(i);
    let i = adapter(LocalWork::open(db.to_str().unwrap()).unwrap(), 100_000);
    let (child, server, out) = launch(&binary, &dir, "resumed", i.clone()).await;
    marker(&out, "NORMAL_INSPECTION_COMPLETE", 2).await;
    assert!(
        !std::fs::read_to_string(&out)
            .unwrap()
            .contains("NORMAL_INSPECTION_COMPUTED")
    );
    let w = i.state.lock().unwrap().ledger.inspect(TASK).unwrap();
    assert_eq!(w.attempts, 1);
    assert_eq!(w.operation.unwrap().phase, Phase::Succeeded);
    assert_eq!(w.checkpoint.unwrap()["phase"], "complete");
    assert!(w.active_ms >= 5);
    stop(child, server, &dir, "resumed").await;
    assert_eq!(receipt_count(&db), 1);

    for mode in ["revoked", "changed", "expired", "stale", "unknown"] {
        let db = dir.join(format!("{mode}.db"));
        let i = adapter(seed(&db, 2, 100), 0);
        i.hold_dispatch.store(true, Ordering::SeqCst);
        let (child, server, out) = launch(&binary, &dir, mode, i.clone()).await;
        tokio::time::timeout(Duration::from_secs(15), i.reached.notified())
            .await
            .unwrap();
        if mode == "unknown" {
            child.stop().await.unwrap();
            i.release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap();
            std::fs::remove_file(dir.join(format!("{mode}.sock"))).unwrap();
            drop(i);
            let i = adapter(LocalWork::open(db.to_str().unwrap()).unwrap(), 100_000);
            let (child, server, out) = launch(&binary, &dir, "unknown-resume", i.clone()).await;
            marker(&out, "NORMAL_INSPECTION_BLOCKED", 2).await;
            assert_eq!(
                i.state
                    .lock()
                    .unwrap()
                    .ledger
                    .inspect(TASK)
                    .unwrap()
                    .operation
                    .unwrap()
                    .phase,
                Phase::Unknown
            );
            assert!(
                !std::fs::read_to_string(out)
                    .unwrap()
                    .contains("NORMAL_INSPECTION_COMPUTED")
            );
            stop(child, server, &dir, "unknown-resume").await;
        } else {
            if mode == "expired" {
                i.state.lock().unwrap().now = 100_000;
            } else if mode == "changed" {
                i.state
                    .lock()
                    .unwrap()
                    .ledger
                    .set_authority(AUTH, authority(false))
                    .unwrap();
            } else if mode == "revoked" {
                i.state
                    .lock()
                    .unwrap()
                    .ledger
                    .set_authority(AUTH, authority(true))
                    .unwrap();
            } else {
                LocalWork::open(db.to_str().unwrap())
                    .unwrap()
                    .claim(TASK, "synthetic:session:takeover", 100_000, 100_000)
                    .unwrap();
            }
            i.hold_dispatch.store(false, Ordering::SeqCst);
            i.release.notify_one();
            marker(&out, "NORMAL_INSPECTION_BLOCKED", 2).await;
            stop(child, server, &dir, mode).await;
        }
        assert_eq!(receipt_count(&db), 0);
    }
    for (label, failures, attempts, budget, success) in [
        ("retry", 1, 2, 100, true),
        ("retry-limit", 3, 2, 100, false),
        ("budget", 0, 2, 1, false),
        ("quota", 0, 2, 100, false),
    ] {
        let db = dir.join(format!("{label}.db"));
        let i = adapter(seed(&db, attempts, budget), 0);
        i.failures.store(failures, Ordering::SeqCst);
        if label == "quota" {
            let mut s = i.state.lock().unwrap();
            let f = s.fence.clone();
            assert!(!s.ledger.reserve(TASK, &f, 0, 0, false).unwrap());
        }
        let i = if label == "retry" {
            i.hold_bootstrap.store(true, Ordering::SeqCst);
            let (child, server, _) = launch(&binary, &dir, "retry-interrupted", i.clone()).await;
            tokio::time::timeout(Duration::from_secs(15), i.reached.notified())
                .await
                .unwrap();
            assert_eq!(
                i.state
                    .lock()
                    .unwrap()
                    .ledger
                    .inspect(TASK)
                    .unwrap()
                    .checkpoint
                    .unwrap()["retry_required"],
                true
            );
            child.stop().await.unwrap();
            i.release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap();
            std::fs::remove_file(dir.join("retry-interrupted.sock")).unwrap();
            drop(i);
            adapter(LocalWork::open(db.to_str().unwrap()).unwrap(), 100_000)
        } else {
            i
        };
        let (child, server, out) = launch(&binary, &dir, label, i.clone()).await;
        marker(
            &out,
            if success {
                "NORMAL_INSPECTION_COMPLETE"
            } else {
                "NORMAL_INSPECTION_BLOCKED"
            },
            2,
        )
        .await;
        let w = i.state.lock().unwrap().ledger.inspect(TASK).unwrap();
        if success {
            assert_eq!(w.attempts, 2);
        } else {
            assert!(w.stopped.is_some());
        }
        stop(child, server, &dir, label).await;
        let w2 = LocalWork::open(db.to_str().unwrap())
            .unwrap()
            .inspect(TASK)
            .unwrap();
        assert_eq!(w.active_ms, w2.active_ms);
        assert_eq!(w.attempts, w2.attempts);
        assert_eq!(w.stopped, w2.stopped);
        assert_eq!(receipt_count(&db), if success { 1 } else { 0 });
    }
    assert!(
        !std::fs::read_dir(&dir).unwrap().any(|p| p
            .unwrap()
            .path()
            .extension()
            .is_some_and(|e| e == "sock"))
    );
    println!("NORMAL_RUNTIME_ACCEPTANCE_COMPLETE all_owned_children_reaped=true");
}
