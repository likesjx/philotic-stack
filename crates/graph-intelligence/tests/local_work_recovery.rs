use graph_intelligence::{local_work::*, GraphEngine, NodeKind};
use std::{
    process::Command,
    sync::{Arc, Barrier},
};

const TASK: &str = "synthetic:task:recovery";
const AUTH: &str = "synthetic:charter:builder";

fn scope() -> Scope {
    Scope {
        action: "simulate".into(),
        resource: "fixture".into(),
        destination: "local-ledger".into(),
    }
}

fn authority() -> Authority {
    Authority {
        authenticated_authorizer_ref: "synthetic:authorizer:jared".into(),
        scopes: vec![scope()],
        valid_from: 0,
        expires_at: 1000,
        revoked: false,
        may_delegate: false,
        parent: None,
        revision: 0,
    }
}

fn seed(path: &str, max_attempts: u64) -> LocalWork {
    let mut store = LocalWork::open(path).unwrap();
    store.set_authority(AUTH, authority()).unwrap();
    store
        .create(
            TASK,
            Work {
                checkpoint: None,
                slice: "synthetic:slice:recovery".into(),
                role_charter: AUTH.into(),
                revision: "sha256:candidate-1".into(),
                environment: "mbp-jane:synthetic-v1".into(),
                fence: None,
                operation: None,
                attempts: 0,
                max_attempts,
                active_ms: 0,
                budget_ms: 100,
                stopped: None,
                implemented: true,
                tested: false,
                merged: false,
                deployed: false,
                live_verified: false,
            },
        )
        .unwrap();
    store
}

fn evidence() -> Evidence {
    Evidence {
        revision: "sha256:candidate-1".into(),
        environment: "mbp-jane:synthetic-v1".into(),
        command: "local_work_recovery".into(),
        exit_code: 0,
        result_ref: "sha256:test-result-1".into(),
    }
}

#[test]
fn inspection_receipt_is_fenced_authorized_and_reconciled_after_reopen() {
    for mode in [
        "success", "revoked", "changed", "expired", "takeover", "unknown",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inspection.db");
        let path = path.to_str().unwrap();
        let mut l = seed(path, 2);
        let mut s = scope();
        s.action = "development.inspect_manifest".into();
        let mut auth = authority();
        auth.scopes = vec![s.clone()];
        l.set_authority(AUTH, auth.clone()).unwrap();
        let f = l.claim(TASK, "synthetic:session:one", 0, 10).unwrap();
        l.prepare(TASK, &f, 0, "one", s).unwrap();
        l.dispatch(TASK, &f, 0).unwrap();
        let receipt = serde_json::json!({"input_digest":"pinned","files":[]});
        match mode {
            "revoked" => {
                auth.revoked = true;
                l.set_authority(AUTH, auth).unwrap();
            }
            "changed" => l.set_authority(AUTH, auth).unwrap(),
            "takeover" => {
                LocalWork::open(path)
                    .unwrap()
                    .claim(TASK, "synthetic:session:two", 10, 10)
                    .unwrap();
            }
            _ => {}
        }
        let now = if mode == "expired" { 10 } else { 0 };
        if mode == "success" {
            assert!(l.commit_inspection(TASK, &f, now, &receipt).unwrap());
            assert!(l.commit_inspection(TASK, &f, now, &receipt).unwrap());
            assert!(l
                .commit_inspection(TASK, &f, now, &serde_json::json!({"different":true}))
                .is_err());
        } else if mode != "unknown" {
            assert!(l.commit_inspection(TASK, &f, now, &receipt).is_err());
        }
        drop(l);
        if mode == "success" || mode == "unknown" {
            let mut l = LocalWork::open(path).unwrap();
            let f = l.claim(TASK, "synthetic:session:resume", 10, 10).unwrap();
            let result = l.reconcile_inspection(TASK, &f, 10).unwrap();
            assert_eq!(
                result,
                if mode == "success" {
                    Some(receipt.clone())
                } else {
                    None
                }
            );
            assert!(l.commit_inspection(TASK, &f, 10, &receipt).is_err());
            assert!(l.retry_prepared(TASK, &f, 10).is_err());
        }
    }
}

#[test]
fn atomic_claim_expiry_and_takeover_fence_every_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let path = path.to_str().unwrap();
    drop(seed(path, 2));
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|i| {
            let mut store = LocalWork::open(path).unwrap();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.claim(TASK, &format!("synthetic:session:{i}"), 0, 10)
            })
        })
        .collect();
    let winners: Vec<_> = handles
        .into_iter()
        .filter_map(|h| h.join().unwrap().ok())
        .collect();
    assert_eq!(winners.len(), 1);
    let old = &winners[0];
    let mut store = LocalWork::open(path).unwrap();
    assert!(store.prepare(TASK, old, 10, "event-1", scope()).is_err());
    let new = store
        .claim(TASK, "synthetic:session:next", 10, 100)
        .unwrap();
    assert_eq!(new.epoch, old.epoch + 1);
    assert!(store.reserve(TASK, old, 10, 1, true).is_err());
    assert!(store.dispatch(TASK, old, 10).is_err());
    assert!(store.simulate_effect(TASK, old, 10).is_err());
    assert!(store.reconcile(TASK, old, 10).is_err());
    assert!(store.candidate(TASK, old, 10, "x", "y").is_err());
    assert!(store
        .verify(TASK, old, 10, "synthetic:evidence:bad", evidence())
        .is_err());
    assert!(store.prepare(TASK, &new, 11, "event-1", scope()).unwrap());
}

// The child intentionally exits only its disposable synthetic test process.
#[test]
fn crash_worker() {
    let Ok(path) = std::env::var("PHILOTIC_SYNTHETIC_CRASH_DB") else {
        return;
    };
    let mut store = LocalWork::open(&path).unwrap();
    let fence = store.claim(TASK, "synthetic:session:crash", 0, 10).unwrap();
    assert!(store.reserve(TASK, &fence, 1, 20, true).unwrap());
    store.prepare(TASK, &fence, 1, "event-1", scope()).unwrap();
    store.dispatch(TASK, &fence, 2).unwrap();
    if std::env::var("PHILOTIC_SYNTHETIC_UNKNOWN").is_err() {
        store.simulate_effect(TASK, &fence, 3).unwrap();
        store.simulate_effect(TASK, &fence, 3).unwrap();
    }
    std::process::exit(73); // no result checkpoint, no destructor-based persistence
}

#[test]
fn crash_after_effect_reconciles_without_repeating_and_unknown_stays_blocked() {
    for unknown in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let path = path.to_str().unwrap();
        drop(seed(path, 2));
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "crash_worker", "--nocapture"])
            .env("PHILOTIC_SYNTHETIC_CRASH_DB", path);
        if unknown {
            cmd.env("PHILOTIC_SYNTHETIC_UNKNOWN", "1");
        }
        assert_eq!(cmd.status().unwrap().code(), Some(73));
        let mut store = LocalWork::open(path).unwrap();
        let saved = store.inspect(TASK).unwrap();
        assert_eq!(saved.operation.unwrap().phase, Phase::Dispatched);
        assert_eq!(
            (saved.attempts, saved.active_ms),
            (1, if unknown { 20 } else { 21 })
        );
        let fence = store
            .claim(TASK, "synthetic:session:resume", 10, 100)
            .unwrap();
        assert!(store.simulate_effect(TASK, &fence, 11).is_err());
        assert!(!store.prepare(TASK, &fence, 11, "event-1", scope()).unwrap());
        assert!(store.dispatch(TASK, &fence, 11).is_err());
        assert_eq!(
            store.reconcile(TASK, &fence, 11).unwrap(),
            if unknown {
                Phase::Unknown
            } else {
                Phase::Succeeded
            }
        );
        assert!(store.simulate_effect(TASK, &fence, 12).is_err());
        assert_eq!(store.effect_count().unwrap(), u64::from(!unknown));
        if unknown {
            assert!(store
                .verify(TASK, &fence, 12, "synthetic:evidence:unknown", evidence())
                .is_err());
        } else {
            store
                .verify(TASK, &fence, 12, "synthetic:evidence:1", evidence())
                .unwrap();
            assert!(store.inspect(TASK).unwrap().tested);
            store
                .candidate(
                    TASK,
                    &fence,
                    13,
                    "sha256:candidate-2",
                    "mbp-jane:synthetic-v1",
                )
                .unwrap();
            assert!(store
                .verify(TASK, &fence, 14, "synthetic:evidence:2", evidence())
                .is_err());
            let facts = store.inspect(TASK).unwrap();
            assert!(
                facts.implemented
                    && !facts.tested
                    && !facts.merged
                    && !facts.deployed
                    && !facts.live_verified
            );
            let graph = GraphEngine::open(path).unwrap();
            assert_eq!(
                graph
                    .get_node("synthetic:evidence:1")
                    .unwrap()
                    .unwrap()
                    .kind,
                NodeKind::TestRun
            );
            assert_eq!(
                graph.search_nodes("synthetic:task:recovery").unwrap().len(),
                1
            );
        }
    }
}

#[test]
fn quota_budget_and_attempt_stop_checkpoints_survive_restart() {
    for reason in ["quota", "budget", "attempt"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let path = path.to_str().unwrap();
        let mut store = seed(path, if reason == "attempt" { 0 } else { 2 });
        let fence = store
            .claim(TASK, "synthetic:session:budget", 0, 10)
            .unwrap();
        assert!(store.reserve(TASK, &fence, 1, 60, true).unwrap());
        if reason == "attempt" {
            assert!(!store.prepare(TASK, &fence, 2, "event", scope()).unwrap());
        } else {
            assert!(!store
                .reserve(TASK, &fence, 2, 41, reason != "quota")
                .unwrap());
        }
        drop(store);
        let mut store = LocalWork::open(path).unwrap();
        let fence = store
            .claim(TASK, "synthetic:session:restart", 10, 10)
            .unwrap();
        let work = store.inspect(TASK).unwrap();
        assert_eq!(work.active_ms, 60);
        assert!(work.stopped.unwrap().contains(reason));
        assert!(store.prepare(TASK, &fence, 11, "event", scope()).is_err());
        assert!(!store.reserve(TASK, &fence, 11, 0, true).unwrap());
        assert_eq!(store.effect_count().unwrap(), 0);
    }
}

#[test]
fn retries_and_effect_budget_are_durable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let path = path.to_str().unwrap();
    let mut store = seed(path, 2);
    let fence = store
        .claim(TASK, "synthetic:session:retry", 0, 100)
        .unwrap();
    store.prepare(TASK, &fence, 1, "event", scope()).unwrap();
    assert!(store.retry_prepared(TASK, &fence, 2).unwrap());
    drop(store);
    let mut store = LocalWork::open(path).unwrap();
    assert_eq!(store.inspect(TASK).unwrap().attempts, 2);
    assert!(!store.retry_prepared(TASK, &fence, 3).unwrap());
    assert!(store.dispatch(TASK, &fence, 4).is_err());
    let mut store = seed(":memory:", 2);
    let fence = store
        .claim(TASK, "synthetic:session:budget", 0, 100)
        .unwrap();
    store.prepare(TASK, &fence, 1, "event", scope()).unwrap();
    store.dispatch(TASK, &fence, 2).unwrap();
    assert!(store.retry_prepared(TASK, &fence, 2).is_err());
    assert!(store.reserve(TASK, &fence, 3, 100, true).unwrap());
    assert!(!store.simulate_effect(TASK, &fence, 4).unwrap());
    assert_eq!(store.effect_count().unwrap(), 0);
    assert!(store.inspect(TASK).unwrap().stopped.is_some());
}

#[test]
fn reopening_with_unexpired_fence_cannot_reexecute_prior_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let path = path.to_str().unwrap();
    let mut store = seed(path, 2);
    let fence = store
        .claim(TASK, "synthetic:session:reopen", 0, 100)
        .unwrap();
    store.prepare(TASK, &fence, 1, "event", scope()).unwrap();
    store.dispatch(TASK, &fence, 2).unwrap();
    drop(store);
    let mut store = LocalWork::open(path).unwrap();
    assert!(store.simulate_effect(TASK, &fence, 3).is_err());
    assert_eq!(store.effect_count().unwrap(), 0);
    assert_eq!(store.reconcile(TASK, &fence, 4).unwrap(), Phase::Unknown);
}

#[test]
fn evidence_requires_environment_command_result_and_success() {
    let mut store = seed(":memory:", 2);
    let fence = store
        .claim(TASK, "synthetic:session:evidence", 0, 100)
        .unwrap();
    store.prepare(TASK, &fence, 1, "event", scope()).unwrap();
    store.dispatch(TASK, &fence, 2).unwrap();
    assert!(store.simulate_effect(TASK, &fence, 3).unwrap());
    store.reconcile(TASK, &fence, 4).unwrap();
    for case in ["environment", "command", "result", "failed"] {
        let mut bad = evidence();
        match case {
            "environment" => bad.environment = "other-host".into(),
            "command" => bad.command.clear(),
            "result" => bad.result_ref.clear(),
            _ => bad.exit_code = 1,
        }
        assert!(store
            .verify(TASK, &fence, 5, "synthetic:evidence:bad", bad)
            .is_err());
        assert!(!store.inspect(TASK).unwrap().tested);
    }
    store
        .verify(TASK, &fence, 6, "synthetic:evidence:good", evidence())
        .unwrap();
    assert!(store
        .verify(TASK, &fence, 7, "synthetic:evidence:good", evidence())
        .is_err());
}

#[test]
fn authority_constraints_and_changes_are_refused_at_execution() {
    for case in [
        "action",
        "resource",
        "destination",
        "expired",
        "revoked",
        "authorizer",
        "changed-before-dispatch",
        "changed-before-effect",
    ] {
        let mut store = seed(":memory:", 2);
        let fence = store
            .claim(TASK, "synthetic:session:auth", 0, 2000)
            .unwrap();
        let mut requested = scope();
        match case {
            "action" => requested.action = "deploy".into(),
            "resource" => requested.resource = "production".into(),
            "destination" => requested.destination = "remote".into(),
            _ => {}
        }
        if ["revoked", "authorizer"].contains(&case) {
            let mut auth = authority();
            if case == "revoked" {
                auth.revoked = true;
            } else {
                auth.authenticated_authorizer_ref.clear();
            }
            store.set_authority(AUTH, auth).unwrap();
        }
        let now = if case == "expired" { 1000 } else { 1 };
        if case.starts_with("changed") {
            store
                .prepare(TASK, &fence, now, "event", requested)
                .unwrap();
            if case == "changed-before-effect" {
                store.dispatch(TASK, &fence, 2).unwrap();
            }
            store.set_authority(AUTH, authority()).unwrap();
            if case == "changed-before-effect" {
                assert!(store.simulate_effect(TASK, &fence, 3).is_err());
            } else {
                assert!(store.dispatch(TASK, &fence, 3).is_err());
            }
        } else {
            assert!(store
                .prepare(TASK, &fence, now, "event", requested)
                .is_err());
        }
        assert_eq!(store.effect_count().unwrap(), 0);
    }
}

#[test]
fn delegation_can_only_narrow_and_parent_revocation_is_live() {
    for case in [
        "allowed",
        "forbidden",
        "broaden",
        "expiry",
        "revoked",
        "cycle",
    ] {
        let mut store = seed(":memory:", 2);
        let mut parent = authority();
        parent.may_delegate = case != "forbidden";
        store
            .set_authority("synthetic:charter:parent", parent.clone())
            .unwrap();
        let mut child = authority();
        child.parent = Some("synthetic:charter:parent".into());
        child.expires_at = 900;
        if case == "broaden" {
            child.scopes.push(Scope {
                action: "deploy".into(),
                ..scope()
            });
        }
        if case == "expiry" {
            child.expires_at = 1001;
        }
        if case == "cycle" {
            child.parent = Some(AUTH.into());
        }
        store.set_authority(AUTH, child).unwrap();
        let fence = store
            .claim(TASK, "synthetic:session:delegation", 0, 100)
            .unwrap();
        if ["allowed", "revoked"].contains(&case) {
            store.prepare(TASK, &fence, 1, "event", scope()).unwrap();
            store.dispatch(TASK, &fence, 2).unwrap();
            if case == "revoked" {
                parent.revoked = true;
                store
                    .set_authority("synthetic:charter:parent", parent)
                    .unwrap();
                assert!(store.simulate_effect(TASK, &fence, 3).is_err());
            } else {
                store.simulate_effect(TASK, &fence, 3).unwrap();
            }
        } else {
            assert!(store.prepare(TASK, &fence, 1, "event", scope()).is_err());
        }
        assert_eq!(store.effect_count().unwrap(), u64::from(case == "allowed"));
    }
}

#[test]
fn runtime_checkpoint_survives_reopen_and_fences_stale_revoked_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.db");
    let path = path.to_str().unwrap();
    let mut store = seed(path, 2);
    let old = store.claim(TASK, "synthetic:session:first", 0, 10).unwrap();
    store.prepare(TASK, &old, 0, "one", scope()).unwrap();
    let saved = serde_json::json!({"phase":"prepared","cursor":0});
    store.checkpoint(TASK, &old, 0, saved.clone()).unwrap();
    drop(store);
    let mut store = LocalWork::open(path).unwrap();
    assert_eq!(store.inspect(TASK).unwrap().checkpoint, Some(saved.clone()));
    let current = store
        .claim(TASK, "synthetic:session:replacement", 10, 10)
        .unwrap();
    assert!(store
        .checkpoint(TASK, &old, 10, serde_json::json!({"stale":true}))
        .is_err());
    let mut revoked = authority();
    revoked.revoked = true;
    store.set_authority(AUTH, revoked).unwrap();
    assert!(store
        .checkpoint(TASK, &current, 10, serde_json::json!({"revoked":true}))
        .is_err());
    assert_eq!(store.inspect(TASK).unwrap().checkpoint, Some(saved));
}
