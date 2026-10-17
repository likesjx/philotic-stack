//! Test-only hotel adapter to the existing LocalWork ledger, never production code.
use super::*;
use ansible_mesh_core::privacy_local::{LaunchPrincipal, LocalConsumer};
use graph_intelligence::local_work::{Authority, Fence, LocalWork, Phase, Scope, Work};
use serde_json::json;
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
const TASK: &str = "synthetic:task:actual-philote";
const AUTH: &str = "synthetic:charter:actual-philote";
fn scope() -> Scope {
    Scope {
        action: "simulate".into(),
        resource: "fixture".into(),
        destination: "fixture/synthetic/sink".into(),
    }
}
fn authority(revoked: bool) -> Authority {
    Authority {
        authenticated_authorizer_ref: "synthetic:authorizer:local-harness".into(),
        scopes: vec![scope()],
        valid_from: 0,
        expires_at: 1000,
        revoked,
        may_delegate: false,
        parent: None,
        revision: 0,
    }
}
fn script_checkpoint() -> serde_json::Value {
    json!({"phase":"prepared","script":{"script":{"variant":"disposable","steps":[{"id":"effect","type":"tool_sequence","input":"plan","config":{}}]},"step_cursor":0,"tool_sequence_cursor":0,"step_outputs":{"plan":[{"tool_name":"synthetic.receipt","arguments":{"operation_id":"one"}}]}}})
}
fn seed(path: &str) -> LocalWork {
    let mut ledger = LocalWork::open(path).unwrap();
    ledger.set_authority(AUTH, authority(false)).unwrap();
    ledger
        .create(
            TASK,
            Work {
                checkpoint: None,
                slice: "synthetic:slice:actual-philote".into(),
                role_charter: AUTH.into(),
                revision: std::env::var("PHILOTIC_DISPOSABLE_REVISION")
                    .unwrap_or_else(|_| "uncommitted-integration".into()),
                environment: "mbp-jane:disposable-philote".into(),
                fence: None,
                operation: None,
                attempts: 0,
                max_attempts: 2,
                active_ms: 0,
                budget_ms: 20,
                stopped: None,
                implemented: true,
                tested: false,
                merged: false,
                deployed: false,
                live_verified: false,
            },
        )
        .unwrap();
    ledger
}
struct State {
    ledger: LocalWork,
    fence: Fence,
    consumer: Option<LocalConsumer>,
    now: u64,
}
pub(super) struct RecoveryFixture {
    state: Mutex<State>,
    interrupt_after_effect: bool,
    pub(super) hold_response: AtomicBool,
    pub(super) release_response: tokio::sync::Notify,
    effect_committed: tokio::sync::Notify,
}
impl RecoveryFixture {
    fn new(mut ledger: LocalWork, now: u64, interrupt: bool) -> Self {
        let fence = ledger
            .claim(TASK, &format!("synthetic:session:epoch-{now}"), now, 10)
            .unwrap();
        Self {
            state: Mutex::new(State {
                ledger,
                fence,
                consumer: None,
                now,
            }),
            interrupt_after_effect: interrupt,
            hold_response: AtomicBool::new(false),
            release_response: tokio::sync::Notify::new(),
            effect_committed: tokio::sync::Notify::new(),
        }
    }
    pub(super) fn handle(
        &self,
        actor: &str,
        consumer: &LocalConsumer,
        req: IpcRequest,
        c: &DisposableContract,
    ) -> Result<IpcResponse> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("fixture poisoned"))?;
        ensure!(
            state.consumer.as_ref() == Some(consumer),
            "stale fixture incarnation"
        );
        let fence = state.fence.clone();
        let now = state.now;
        match req {
            IpcRequest::GetConfig { key } => {
                ensure!(key == "__disposable_recovery__", "bootstrap scope");
                ensure!(
                    state.ledger.reserve(TASK, &fence, now, 2, true)?,
                    "budget stopped"
                );
                let work = state.ledger.inspect(TASK)?;
                let phase = match work.operation {
                    None => {
                        ensure!(
                            state
                                .ledger
                                .prepare(TASK, &fence, now, "event-one", scope())?,
                            "prepare refused"
                        );
                        state
                            .ledger
                            .checkpoint(TASK, &fence, now, script_checkpoint())?;
                        Phase::Prepared
                    }
                    Some(op) if op.phase == Phase::Prepared => Phase::Prepared,
                    Some(_) => state.ledger.reconcile(TASK, &fence, now)?,
                };
                ensure!(phase != Phase::Unknown, "unknown outcome blocked");
                let checkpoint = state
                    .ledger
                    .inspect(TASK)?
                    .checkpoint
                    .ok_or_else(|| anyhow::anyhow!("missing checkpoint"))?;
                // Fresh authority + fence validation, even for a reconciled receipt.
                state
                    .ledger
                    .checkpoint(TASK, &fence, now, checkpoint.clone())?;
                Ok(IpcResponse::ConfigData {
                    key,
                    value_json: Some(json!({"phase":phase,"checkpoint":checkpoint}).to_string()),
                })
            }
            IpcRequest::SyncApartment {
                agent_id,
                memory_type,
                content_json,
            } => {
                ensure!(
                    agent_id == actor && memory_type == c.checkpoint_key,
                    "checkpoint scope"
                );
                state.ledger.checkpoint(TASK, &fence, now, content_json)?;
                Ok(IpcResponse::success("fixture_checkpoint", None))
            }
            IpcRequest::EmitTask {
                target_node,
                target_role,
                target_guest_id,
                task_json,
            } => {
                ensure!(
                    target_node == c.hotel
                        && target_role == c.destination_role
                        && target_guest_id.as_deref() == Some(c.destination_guest.as_str())
                        && task_json == c.operation_json,
                    "effect scope"
                );
                state.ledger.dispatch(TASK, &fence, now)?;
                ensure!(
                    state.ledger.simulate_effect(TASK, &fence, now)?,
                    "effect budget stopped"
                );
                if self.interrupt_after_effect {
                    self.hold_response.store(true, Ordering::SeqCst);
                    self.effect_committed.notify_one();
                }
                Ok(IpcResponse::success("fixture_effect", None))
            }
            _ => anyhow::bail!("unsupported fixture request"),
        }
    }
}
fn contract() -> DisposableContract {
    DisposableContract {
        hotel: "fixture".into(),
        guest: "agent".into(),
        agent: "agent".into(),
        role: "agent".into(),
        destination_role: "synthetic".into(),
        destination_guest: "sink".into(),
        operation_json: "{\"operation_id\":\"one\"}".into(),
        checkpoint_key: "fixture-checkpoint".into(),
        receipt_key: "unused-localwork-owns-receipt".into(),
        expires_at: Instant::now() + Duration::from_secs(60),
    }
}
async fn run_child(
    binary: &str,
    directory: &Path,
    phase: &str,
    fixture: Arc<RecoveryFixture>,
) -> (
    crate::service::protected_local_guest::ProtectedLocalGuest,
    tokio::task::JoinHandle<()>,
) {
    let socket = directory.join(format!("{phase}.sock"));
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let registry = Arc::new(LocalLaunchRegistry::default());
    let mut adapter = ProtectedDisposable::new(registry.clone(), contract()).unwrap();
    adapter.recovery = Some(fixture.clone());
    let mut command = tokio::process::Command::new(binary);
    command
        .env_clear()
        .env("PHILOTIC_AGENT_ID", "agent")
        .env("PHILOTIC_HOTEL_SOCKET", &socket)
        .env("PHILOTIC_DISPOSABLE_RECOVERY", "1")
        .env(
            "PHILOTIC_DISPOSABLE_PARENT_PID",
            std::process::id().to_string(),
        )
        .env(
            "PHILOTIC_DISPOSABLE_PARENT_UID",
            unsafe { libc::geteuid() }.to_string(),
        )
        .stdout(std::fs::File::create(directory.join(format!("{phase}.stdout"))).unwrap())
        .stderr(std::fs::File::create(directory.join(format!("{phase}.stderr"))).unwrap());
    let child = crate::service::protected_local_guest::ProtectedLocalGuest::spawn(
        &mut command,
        registry.clone(),
        "agent",
        unsafe { libc::geteuid() },
        LaunchPrincipal {
            stable_agent_id: "agent".into(),
            roles: ["agent".into()].into_iter().collect(),
        },
    )
    .unwrap();
    fixture.state.lock().unwrap().consumer = Some(registry.target("fixture", "agent").unwrap());
    // Accept only AFTER supervisor attachment: no launch registration race/fallback.
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let store =
            ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(":memory:").unwrap();
        let graph = Arc::new(GraphDomain::new(Arc::new(store.adapter())));
        let _ = adapter.serve(stream, graph).await;
    });
    (child, server)
}
async fn await_exit(child: &crate::service::protected_local_guest::ProtectedLocalGuest) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if child.exited().await.unwrap() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("owned Philote did not exit");
}

#[tokio::test]
#[ignore = "explicit actual executable integration; requires PHILOTIC_DISPOSABLE_BINARY"]
async fn actual_philote_interruption_reconciles_existing_ledger() {
    let binary = std::env::var("PHILOTIC_DISPOSABLE_BINARY")
        .expect("build actual philote with disposable-recovery feature");
    assert_eq!(
        Path::new(&binary).file_name().unwrap(),
        "philote",
        "actual executable required"
    );
    let directory =
        std::path::PathBuf::from("/tmp").join(format!("pr-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&directory).unwrap();
    let db = directory.join("work.db");
    let db = db.to_str().unwrap();
    let first = Arc::new(RecoveryFixture::new(seed(db), 0, true));
    let old_fence = first.state.lock().unwrap().fence.clone();
    let (child, server) = run_child(&binary, &directory, "interrupted", first.clone()).await;
    tokio::time::timeout(Duration::from_secs(10), first.effect_committed.notified())
        .await
        .expect("actual child did not commit effect");
    {
        let mut state = first.state.lock().unwrap();
        assert_eq!(state.ledger.effect_count().unwrap(), 1);
        assert_eq!(
            state.ledger.inspect(TASK).unwrap().operation.unwrap().phase,
            Phase::Dispatched
        );
        assert_eq!(
            state.ledger.inspect(TASK).unwrap().checkpoint.unwrap()["phase"],
            "prepared"
        );
    }
    child.stop().await.unwrap(); // retire first, kill/reap ONLY this owned actual binary
    first.release_response.notify_one();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    drop(child);
    drop(first);
    let mut reopened = LocalWork::open(db).unwrap();
    assert_eq!(reopened.inspect(TASK).unwrap().active_ms, 3);
    assert_eq!(reopened.inspect(TASK).unwrap().attempts, 1);
    let second = Arc::new(RecoveryFixture::new(reopened, 10, false));
    {
        let mut state = second.state.lock().unwrap();
        assert!(
            state
                .ledger
                .checkpoint(TASK, &old_fence, 10, json!({"stale":true}))
                .is_err()
        );
        assert!(state.ledger.simulate_effect(TASK, &old_fence, 10).is_err());
    }
    let (child, server) = run_child(&binary, &directory, "resumed", second.clone()).await;
    await_exit(&child).await;
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        std::fs::read_to_string(directory.join("resumed.stdout"))
            .unwrap()
            .contains("DISPOSABLE_RECOVERY_COMPLETE reconciled=true")
    );
    {
        let mut state = second.state.lock().unwrap();
        let work = state.ledger.inspect(TASK).unwrap();
        assert_eq!(work.checkpoint.unwrap()["phase"], "complete");
        assert_eq!(work.operation.unwrap().phase, Phase::Succeeded);
        assert_eq!(work.attempts, 1);
        assert_eq!(work.active_ms, 5);
        assert_eq!(state.ledger.effect_count().unwrap(), 1);
    }
    child.stop().await.unwrap();
    drop(child);
    drop(second);
    // A third actual process is denied after durable authority revocation.
    let mut ledger = LocalWork::open(db).unwrap();
    ledger.set_authority(AUTH, authority(true)).unwrap();
    let revoked = Arc::new(RecoveryFixture::new(ledger, 20, false));
    let (child, server) = run_child(&binary, &directory, "revoked", revoked.clone()).await;
    await_exit(&child).await;
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !std::fs::read_to_string(directory.join("revoked.stdout"))
            .unwrap()
            .contains("DISPOSABLE_RECOVERY_COMPLETE")
    );
    assert!(
        std::fs::read_to_string(directory.join("revoked.stderr"))
            .unwrap()
            .contains("recovery bootstrap refused")
    );
    child.stop().await.unwrap();
    drop(child);
    {
        let mut state = revoked.state.lock().unwrap();
        let fence = state.fence.clone();
        assert!(!state.ledger.reserve(TASK, &fence, 20, 100, true).unwrap());
    }
    drop(revoked);
    let mut ledger = LocalWork::open(db).unwrap();
    assert_eq!(ledger.effect_count().unwrap(), 1);
    assert_eq!(
        ledger.inspect(TASK).unwrap().stopped.as_deref(),
        Some("active-work budget exhausted")
    );
    // A still-live actual child loses its epoch before the result checkpoint.
    let stale_db = directory.join("stale.db");
    let stale_db = stale_db.to_str().unwrap();
    let stale = Arc::new(RecoveryFixture::new(seed(stale_db), 0, true));
    let (child, server) = run_child(&binary, &directory, "stale", stale.clone()).await;
    tokio::time::timeout(Duration::from_secs(10), stale.effect_committed.notified())
        .await
        .unwrap();
    let mut takeover = LocalWork::open(stale_db).unwrap();
    takeover
        .claim(TASK, "synthetic:session:takeover", 10, 10)
        .unwrap();
    stale.release_response.notify_one();
    await_exit(&child).await;
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        std::fs::read_to_string(directory.join("stale.stderr"))
            .unwrap()
            .contains("disposable request refused")
    );
    assert_eq!(
        takeover.inspect(TASK).unwrap().checkpoint.unwrap()["phase"],
        "prepared"
    );
    assert_eq!(takeover.effect_count().unwrap(), 1);
    child.stop().await.unwrap();
    drop(child);
    drop(stale);
    drop(takeover);

    // An unknown dispatch has no receipt: deny bootstrap without emitting again.
    let unknown_db = directory.join("unknown.db");
    let unknown_db = unknown_db.to_str().unwrap();
    let mut unknown_ledger = seed(unknown_db);
    let fence = unknown_ledger
        .claim(TASK, "synthetic:session:unknown-origin", 0, 10)
        .unwrap();
    unknown_ledger
        .prepare(TASK, &fence, 0, "event-one", scope())
        .unwrap();
    unknown_ledger
        .checkpoint(TASK, &fence, 0, script_checkpoint())
        .unwrap();
    unknown_ledger.dispatch(TASK, &fence, 0).unwrap();
    drop(unknown_ledger);
    let unknown = Arc::new(RecoveryFixture::new(
        LocalWork::open(unknown_db).unwrap(),
        10,
        false,
    ));
    let (child, server) = run_child(&binary, &directory, "unknown", unknown.clone()).await;
    await_exit(&child).await;
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        std::fs::read_to_string(directory.join("unknown.stderr"))
            .unwrap()
            .contains("recovery bootstrap refused")
    );
    assert_eq!(
        unknown.state.lock().unwrap().ledger.effect_count().unwrap(),
        0
    );
    assert_eq!(
        unknown
            .state
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
    child.stop().await.unwrap();
    drop(child);
    drop(unknown);
    for phase in ["interrupted", "resumed", "revoked", "stale", "unknown"] {
        std::fs::remove_file(directory.join(format!("{phase}.sock"))).unwrap();
        let stdout = std::fs::read_to_string(directory.join(format!("{phase}.stdout"))).unwrap();
        let start = stdout
            .lines()
            .find(|line| line.starts_with("DISPOSABLE_RECOVERY_START"))
            .expect("actual executable start receipt");
        println!("ACTUAL_CHILD phase={phase} {start}");
    }
    println!(
        "ACTUAL_PHILOTE_RECEIPT binary={binary} artifacts={} effect_count=1 attempts=1 epochs=1,2,3 all_children_reaped=true",
        directory.display()
    );
    // Keep DB/stdout/stderr evidence; owned children reaped and sockets removed.
}
