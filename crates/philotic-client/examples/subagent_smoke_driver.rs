//! Subagent spawn/lease/assign/hook smoke driver.
//!
//! Validates the full subagent round-trip using two IPC connections in a single
//! process — no model runner needed:
//!
//! ```text
//! parent (agent-jane-01 / "agent")
//!   → SpawnSubagent            → SpawnSubagentOk { subagent_guest_id }
//!
//! worker (<subagent_guest_id> / "agent-worker")  [tokio task]
//!   → Register                 → (inbox subscription created)
//!   → AcceptSubagentLease      → success
//!
//! parent
//!   → AssignSubagentTask       → success (routes to worker inbox)
//!
//! worker
//!   ← InboundTask (delegation)
//!   → FireSubagentHook(TurnCompleted, { result_summary: "smoke ok" })
//!
//! parent
//!   ← InboundTask { kind: "subagent_hook", hook_kind: "turn_completed" }
//!   → ReleaseSubagent          → success
//! ```

use anyhow::{Context, Result, bail};
use philotic_client::{
    GuestIdentity, HookKind, HookRoute, HookSubscription, IpcRequest, IpcResponse,
    PhiloticClient, SubagentContextPacket, SubagentDelegation, SubagentLeaseTerms,
};
use tokio::time::{Duration, sleep, timeout};

const PARENT_GUEST_ID: &str = "agent-jane-01";
const PARENT_ROLE: &str = "agent";
const WORKER_ROLE: &str = "agent-worker";
const EXPECTED_RESULT: &str = "smoke ok";

#[tokio::main]
async fn main() -> Result<()> {
    let _hotel_node = std::env::var("PHILOTIC_TARGET_NODE")
        .unwrap_or_else(|_| "local-ansible-01".to_string());

    // ── Phase 1: parent connects ───────────────────────────────────────────────
    println!("[parent] connecting…");
    let mut parent = PhiloticClient::connect(GuestIdentity {
        guest_id: PARENT_GUEST_ID.into(),
        role: PARENT_ROLE.into(),
        supported_tools: Vec::new(),
    })
    .await
    .context("parent failed to connect")?;

    // ── Phase 2: SpawnSubagent ─────────────────────────────────────────────────
    let delegation = SubagentDelegation {
        parent_agent_id: PARENT_GUEST_ID.into(),
        parent_role: PARENT_ROLE.into(),
        subagent_kind: WORKER_ROLE.into(),
        goal: "smoke test task".into(),
        context_packet: SubagentContextPacket {
            summary: "smoke context".into(),
            ..Default::default()
        },
        lease_terms: SubagentLeaseTerms {
            ttl_seconds: 30,
            ..Default::default()
        },
        hook_subscriptions: vec![HookSubscription {
            hook_kind: HookKind::TurnCompleted,
            route: HookRoute::PersonaAgent,
            handler_skill: None,
        }],
        completion_route: HookRoute::PersonaAgent,
        failure_route: HookRoute::PersonaAgent,
        ..Default::default()
    };

    println!("[parent] sending SpawnSubagent…");
    let spawn_resp = parent
        .send_request(IpcRequest::SpawnSubagent {
            session_id: "smoke-subagent-session-1".into(),
            delegation: delegation.clone(),
        })
        .await
        .context("SpawnSubagent IPC failed")?;

    let (subagent_guest_id, lease_epoch) = match spawn_resp {
        IpcResponse::SpawnSubagentOk {
            subagent_guest_id,
            confirmed_lease,
        } => {
            println!(
                "[parent] SpawnSubagentOk: guest_id={} epoch={}",
                subagent_guest_id, confirmed_lease.lease_epoch
            );
            (subagent_guest_id, confirmed_lease.lease_epoch)
        }
        other => bail!("unexpected SpawnSubagent response: {other:?}"),
    };

    // ── Phase 3: worker connects and accepts lease ─────────────────────────────
    let worker_id = subagent_guest_id.clone();
    let worker_handle = tokio::spawn(async move {
        run_worker(worker_id).await
    });

    // Give the worker task time to connect and subscribe before AssignSubagentTask.
    sleep(Duration::from_millis(200)).await;

    // ── Phase 4: parent assigns the task ──────────────────────────────────────
    println!("[parent] sending AssignSubagentTask…");
    let assign_resp = parent
        .send_request(IpcRequest::AssignSubagentTask {
            subagent_guest_id: subagent_guest_id.clone(),
            lease_epoch,
            delegation,
        })
        .await
        .context("AssignSubagentTask IPC failed")?;

    match assign_resp {
        IpcResponse::Standard { ok: true, .. } => {
            println!("[parent] AssignSubagentTask ok");
        }
        other => bail!("unexpected AssignSubagentTask response: {other:?}"),
    }

    // ── Phase 5: parent waits for the completion hook ─────────────────────────
    // SpawnSubagent eagerly delivers a `subagent_spawned` notification to the
    // parent inbox. Drain any non-hook messages until we get `subagent_hook`.
    println!("[parent] waiting for subagent hook…");
    let hook: serde_json::Value = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                bail!("timed out waiting for subagent_hook");
            }
            let msg = timeout(remaining, parent.recv_task())
                .await
                .context("timed out waiting for subagent hook")??;

            let IpcResponse::InboundTask { task_json, .. } = msg else {
                continue;
            };
            let v: serde_json::Value =
                serde_json::from_str(&task_json).context("failed to decode inbound task json")?;
            let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            if kind == "subagent_hook" {
                break v;
            }
            println!("[parent] skipping notification kind={kind:?}");
        }
    };
    let result_summary = hook
        .get("payload")
        .and_then(|p| p.get("result_summary"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if result_summary != EXPECTED_RESULT {
        bail!(
            "expected result_summary={:?}, got {:?}",
            EXPECTED_RESULT,
            result_summary
        );
    }
    println!("[parent] hook received: result_summary={result_summary:?} ✓");

    // ── Phase 6: parent releases the subagent ─────────────────────────────────
    println!("[parent] releasing subagent…");
    let release_resp = parent
        .send_request(IpcRequest::ReleaseSubagent {
            subagent_guest_id: subagent_guest_id.clone(),
        })
        .await
        .context("ReleaseSubagent IPC failed")?;

    match release_resp {
        IpcResponse::Standard { ok: true, .. } => {
            println!("[parent] ReleaseSubagent ok");
        }
        other => bail!("unexpected ReleaseSubagent response: {other:?}"),
    }

    // Wait for the worker task to finish cleanly.
    worker_handle
        .await
        .context("worker task panicked")?
        .context("worker task returned error")?;

    println!("subagent smoke: PASSED");
    Ok(())
}

/// Worker side: connect, accept lease, receive delegation, fire completion hook.
async fn run_worker(worker_guest_id: String) -> Result<()> {
    println!("[worker] connecting as {worker_guest_id}…");
    let mut worker = PhiloticClient::connect(GuestIdentity {
        guest_id: worker_guest_id.clone(),
        role: WORKER_ROLE.into(),
        supported_tools: Vec::new(),
    })
    .await
    .context("worker failed to connect")?;

    // Accept the lease — signals to the hotel that the worker is alive.
    println!("[worker] accepting lease…");
    let accept_resp = worker
        .send_request(IpcRequest::AcceptSubagentLease {
            subagent_guest_id: worker_guest_id.clone(),
        })
        .await
        .context("AcceptSubagentLease IPC failed")?;

    match accept_resp {
        IpcResponse::Standard { ok: true, .. } => {
            println!("[worker] lease accepted");
        }
        other => bail!("AcceptSubagentLease failed: {other:?}"),
    }

    // Wait for the delegation task.
    println!("[worker] waiting for InboundTask…");
    let task_msg = timeout(Duration::from_secs(10), worker.recv_task())
        .await
        .context("timed out waiting for InboundTask")??;

    let IpcResponse::InboundTask { task_json, .. } = task_msg else {
        bail!("expected InboundTask, got: {task_msg:?}");
    };

    let _delegation: SubagentDelegation =
        serde_json::from_str(&task_json).context("failed to decode SubagentDelegation")?;
    println!("[worker] delegation received, firing completion hook…");

    // Fire the completion hook back to the parent.
    let hook_resp = worker
        .send_request(IpcRequest::FireSubagentHook {
            subagent_guest_id: worker_guest_id.clone(),
            hook_kind: HookKind::TurnCompleted,
            payload: serde_json::json!({
                "completed": true,
                "result_summary": EXPECTED_RESULT,
            }),
        })
        .await
        .context("FireSubagentHook IPC failed")?;

    match hook_resp {
        IpcResponse::Standard { ok: true, .. } => {
            println!("[worker] completion hook fired");
        }
        other => bail!("FireSubagentHook failed: {other:?}"),
    }

    Ok(())
}
