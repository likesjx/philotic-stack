use ed25519_dalek::SigningKey;
use native_memory::*;
use std::{collections::BTreeMap, sync::atomic::AtomicBool};
fn setup() -> (Ingress, Policy) {
    let key = SigningKey::from_bytes(&[7; 32]);
    let grant = Grant {
        issuer: "ingress-hotel".into(),
        reply_node: "ingress-hotel".into(),
        reply_guest: "membrane-1".into(),
        principal: "alice".into(),
        owner: "alice".into(),
        namespace: "notes".into(),
        destination: "memory-hotel".into(),
        runner: "memory-1".into(),
        operations: vec!["remember".into(), "recall".into()],
        revision: 1,
        expires_at: 1000,
        revoked: false,
    };
    let grants = BTreeMap::from([("caller-1".into(), grant)]);
    let policy = Policy {
        issuers: BTreeMap::from([(
            "ingress-hotel".into(),
            TrustedIssuer {
                key_id: "test-key".into(),
                public_key: key.verifying_key().to_bytes(),
            },
        )]),
        grants: grants.clone(),
    };
    (
        Ingress::new("ingress-hotel".into(), "test-key".into(), key, grants),
        policy,
    )
}
fn command() -> Command {
    Command::Remember {
        id: "note-1".into(),
        subject: "practice".into(),
        content: "Harmless synthetic blue notebook".into(),
        source: "synthetic://client".into(),
        source_version: "1".into(),
        lineage: "source-1".into(),
    }
}
fn issue(i: &Ingress, event: &str, nonce: &str, command: Command) -> Envelope {
    i.issue("caller-1", command, event.into(), nonce.into(), 100)
        .unwrap()
}
fn open(path: &std::path::Path) -> Runner {
    Runner::open(path, "memory-hotel".into(), "memory-1".into()).unwrap()
}
#[test]
fn signed_two_hotel_remember_recall_and_restart_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    let (i, p) = setup();
    let e = issue(&i, "e1", "n1", command());
    let mut r = open(&path);
    let first = r
        .execute_wire(
            &serde_json::to_vec(&e).unwrap(),
            &p,
            100,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(first.receipt.status, "committed");
    drop(r);
    let mut r = open(&path);
    assert_eq!(
        r.execute(&e, &p, 101, &AtomicBool::new(false)).unwrap(),
        first
    );
    let retry = issue(&i, "e1", "n2", command());
    assert_eq!(
        r.execute(&retry, &p, 101, &AtomicBool::new(false)).unwrap(),
        first
    );
    let q = issue(
        &i,
        "q1",
        "n3",
        Command::Recall {
            query: "notebook".into(),
            limit: 10,
        },
    );
    let response = r.execute(&q, &p, 101, &AtomicBool::new(false)).unwrap();
    assert_eq!(response.observations.len(), 1);
    assert_eq!(response.observations[0].authenticated_writer, "alice");
    assert_eq!(response.observations[0].kind, "observation");
    assert_eq!(response.observations[0].lineage, "source-1");
}
#[test]
fn tampering_with_payload_operation_destination_or_reply_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = open(&dir.path().join("db"));
    let (i, p) = setup();
    let original = issue(&i, "e", "n", command());
    for n in 0..6 {
        let mut e = original.clone();
        match n {
            0 => {
                e.request.command = Command::Recall {
                    query: "all".into(),
                    limit: 1,
                }
            }
            1 => e.claims.destination = "other".into(),
            2 => e.request.owner = "bob".into(),
            3 => e.claims.reply_guest = "attacker".into(),
            4 => e.request.provider = Some("paid-provider".into()),
            _ => e.signature[0] ^= 1,
        };
        assert_eq!(
            r.execute(&e, &p, 100, &AtomicBool::new(false)),
            Err(Error::Denied)
        );
    }
}
#[test]
fn expiry_revocation_policy_change_and_untrusted_issuer_reject() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = open(&dir.path().join("db"));
    let (i, p) = setup();
    let e = issue(&i, "e", "n", command());
    assert_eq!(
        r.execute(&e, &p, 160, &AtomicBool::new(false)),
        Err(Error::Expired)
    );
    for n in 0..4 {
        let mut p = p.clone();
        match n {
            0 => p.grants.get_mut("caller-1").unwrap().revoked = true,
            1 => p.grants.get_mut("caller-1").unwrap().revision = 2,
            2 => p.grants.get_mut("caller-1").unwrap().operations = vec!["recall".into()],
            _ => p.issuers.clear(),
        };
        assert_eq!(
            r.execute(&e, &p, 100, &AtomicBool::new(false)),
            Err(Error::Denied)
        );
    }
}
#[test]
fn nonce_replay_survives_restart_and_event_conflicts_fail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let (i, p) = setup();
    let mut r = open(&path);
    r.execute(
        &issue(&i, "e1", "nonce", command()),
        &p,
        100,
        &AtomicBool::new(false),
    )
    .unwrap();
    drop(r);
    let mut r = open(&path);
    assert_eq!(
        r.execute(
            &issue(&i, "e2", "nonce", command()),
            &p,
            100,
            &AtomicBool::new(false)
        ),
        Err(Error::Replay)
    );
    assert_eq!(
        r.execute(
            &issue(
                &i,
                "e1",
                "different",
                Command::Recall {
                    query: "blue".into(),
                    limit: 1
                }
            ),
            &p,
            100,
            &AtomicBool::new(false)
        ),
        Err(Error::Conflict)
    );
}
#[test]
fn unauthorized_ingress_no_identity_from_arguments() {
    let (i, _) = setup();
    assert!(matches!(
        i.issue("forged-token-id", command(), "e".into(), "n".into(), 100),
        Err(Error::Denied)
    ));
    let raw = r#"{"operation":"recall","query":"blue","limit":1,"owner":"alice","role":"owner"}"#;
    assert!(serde_json::from_str::<Command>(raw).is_err());
}
#[test]
fn cross_user_reads_are_empty_and_writes_do_not_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = open(&dir.path().join("db"));
    let (i, mut p) = setup();
    r.execute(
        &issue(&i, "e1", "n1", command()),
        &p,
        100,
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut bob = p.grants["caller-1"].clone();
    bob.owner = "bob".into();
    bob.principal = "bob".into();
    p.grants.insert("caller-2".into(), bob);
    let bi = Ingress::new(
        "ingress-hotel".into(),
        "test-key".into(),
        SigningKey::from_bytes(&[7; 32]),
        p.grants.clone(),
    );
    let q = bi
        .issue(
            "caller-2",
            Command::Recall {
                query: "blue".into(),
                limit: 5,
            },
            "q".into(),
            "n2".into(),
            100,
        )
        .unwrap();
    assert!(
        r.execute(&q, &p, 100, &AtomicBool::new(false))
            .unwrap()
            .observations
            .is_empty()
    );
    assert_eq!(
        r.execute(
            &issue(&i, "e2", "n3", command()),
            &p,
            100,
            &AtomicBool::new(false)
        ),
        Err(Error::Conflict)
    );
}
#[test]
fn malformed_cancelled_and_unavailable_fail_without_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut r = open(&path);
    let (i, p) = setup();
    let e = issue(&i, "e", "n", command());
    assert_eq!(
        r.execute_wire(b"not-json", &p, 100, &AtomicBool::new(false)),
        Err(Error::Invalid)
    );
    assert_eq!(
        r.execute(&e, &p, 100, &AtomicBool::new(true)),
        Err(Error::Cancelled)
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    db.execute_batch("DROP TABLE receipts").unwrap();
    assert_eq!(
        r.execute(&e, &p, 100, &AtomicBool::new(false)),
        Err(Error::Unavailable)
    );
    let count: i64 = db
        .query_row("SELECT count(*) FROM observations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
#[test]
fn reusable_secret_canary_absent_from_wire_debug_and_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut r = open(&path);
    let (_, mut p) = setup();
    let canary = "KKKKKKKKKKKKKKKKKKKKKKKKKKKKKKKK";
    let key = SigningKey::from_bytes(canary.as_bytes().try_into().unwrap());
    p.issuers.get_mut("ingress-hotel").unwrap().public_key = key.verifying_key().to_bytes();
    let i = Ingress::new(
        "ingress-hotel".into(),
        "test-key".into(),
        key,
        p.grants.clone(),
    );
    let e = issue(&i, "e", "n", command());
    let wire = serde_json::to_vec(&e).unwrap();
    assert!(!String::from_utf8_lossy(&wire).contains(canary));
    assert_eq!(format!("{e:?}"), "Envelope([REDACTED])");
    r.execute(&e, &p, 100, &AtomicBool::new(false)).unwrap();
    drop(r);
    let db = std::fs::read(path).unwrap();
    assert!(!db.windows(canary.len()).any(|w| w == canary.as_bytes()));
    assert!(!db.windows(e.signature.len()).any(|w| w == e.signature));
}

#[test]
fn delayed_revocation_or_expiry_rolls_back_before_acknowledgement() {
    for expires in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut r = open(&path);
        let (i, p) = setup();
        let e = issue(&i, "e", "n", command());
        let mut calls = 0;
        let result = r.execute_current(
            &e,
            || {
                calls += 1;
                let mut p = p.clone();
                if calls == 2 && !expires {
                    p.grants.get_mut("caller-1").unwrap().revoked = true;
                }
                Ok((p, if calls == 2 && expires { 160 } else { 100 }))
            },
            &AtomicBool::new(false),
        );
        assert_eq!(
            result,
            Err(if expires {
                Error::Expired
            } else {
                Error::Denied
            })
        );
        let db = rusqlite::Connection::open(path).unwrap();
        for table in ["observations", "receipts", "nonces"] {
            let count: i64 = db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
    }
}

#[cfg(unix)]
#[test]
fn native_runner_binary_uses_real_sdk_ipc_and_durable_receipt() {
    use philotic_client::{IpcRequest, IpcResponse};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn send(s: &mut std::os::unix::net::UnixStream, r: &IpcResponse) {
        let b = serde_json::to_vec(r).unwrap();
        s.write_all(&(b.len() as u32).to_be_bytes()).unwrap();
        s.write_all(&b).unwrap();
    }
    fn receive(s: &mut std::os::unix::net::UnixStream) -> IpcRequest {
        let mut header = [0; 4];
        s.read_exact(&mut header).unwrap();
        let n = u32::from_be_bytes(header) as usize;
        assert!(n < 65536);
        let mut b = vec![0; n];
        s.read_exact(&mut b).unwrap();
        serde_json::from_slice(&b).unwrap()
    }
    let dir = tempfile::Builder::new()
        .prefix("nm-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("ipc");
    let database = dir.path().join("db");
    let config = dir.path().join("policy.json");
    let (_, mut policy) = setup();
    policy.grants.get_mut("caller-1").unwrap().expires_at = u64::MAX;
    std::fs::write(&config, serde_json::to_vec(&policy).unwrap()).unwrap();
    let ingress = Ingress::new(
        "ingress-hotel".into(),
        "test-key".into(),
        SigningKey::from_bytes(&[7; 32]),
        policy.grants.clone(),
    );
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = Child(
        std::process::Command::new(env!("CARGO_BIN_EXE_native-memory"))
            .args([
                "--socket",
                socket.to_str().unwrap(),
                "--node",
                "memory-hotel",
                "--guest",
                "memory-1",
                "--sqlite-fixture",
                database.to_str().unwrap(),
                "--policy",
                config.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let (mut peer, _) = loop {
        match listener.accept() {
            Ok(pair) => break pair,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(std::time::Instant::now() < deadline);
                assert!(child.0.try_wait().unwrap().is_none());
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    };
    peer.set_nonblocking(false).unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    assert!(matches!(receive(&mut peer),IpcRequest::Register(identity) if identity.role==ROLE));
    send(&mut peer, &IpcResponse::success("registered", None));
    for (event, cmd) in [
        ("remember", command()),
        (
            "recall",
            Command::Recall {
                query: "notebook".into(),
                limit: 5,
            },
        ),
    ] {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let envelope = ingress
            .issue("caller-1", cmd, event.into(), event.into(), now)
            .unwrap();
        send(
            &mut peer,
            &IpcResponse::InboundTask {
                source_node: "ingress-hotel".into(),
                task_id: uuid::Uuid::new_v4(),
                task_json: serde_json::to_string(&envelope).unwrap(),
            },
        );
        let IpcRequest::EmitTask {
            target_node,
            target_guest_id,
            task_json,
            ..
        } = receive(&mut peer)
        else {
            panic!("missing result")
        };
        assert_eq!(target_node, "ingress-hotel");
        assert_eq!(target_guest_id.as_deref(), Some("membrane-1"));
        let payload: serde_json::Value = serde_json::from_str(&task_json).unwrap();
        let result: Response = serde_json::from_value(payload["result"].clone()).unwrap();
        assert_eq!(result.receipt.status, "committed");
        if event == "recall" {
            assert_eq!(result.observations.len(), 1);
        }
        // Receipt must be visible to another SQLite connection before ACK is sent.
        let db = rusqlite::Connection::open(&database).unwrap();
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM receipts WHERE event=?1",
                [event],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        send(&mut peer, &IpcResponse::success("routed", None));
    }
}

#[test]
fn cancellation_during_final_policy_reload_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut runner = open(&path);
    let (ingress, policy) = setup();
    let envelope = issue(&ingress, "event", "nonce", command());
    let cancelled = AtomicBool::new(false);
    let mut calls = 0;
    let result = runner.execute_current(
        &envelope,
        || {
            calls += 1;
            if calls == 2 {
                cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok((policy.clone(), 100))
        },
        &cancelled,
    );
    assert_eq!(result, Err(Error::Cancelled));
    let db = rusqlite::Connection::open(path).unwrap();
    for table in ["observations", "receipts", "nonces"] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[test]
fn typed_json_normalizes_order_but_rejects_duplicates_and_unknown_fields() {
    let dir = tempfile::tempdir().unwrap();
    let mut runner = open(&dir.path().join("db"));
    let (ingress, policy) = setup();
    let envelope = issue(&ingress, "event", "nonce", command());
    // Value maps reorder struct fields; pretty JSON changes whitespace only.
    let value = serde_json::to_value(&envelope).unwrap();
    let reordered = serde_json::to_vec_pretty(&value).unwrap();
    assert!(
        runner
            .execute_wire(&reordered, &policy, 100, &AtomicBool::new(false))
            .is_ok()
    );
    let original = serde_json::to_string(&envelope).unwrap();
    let duplicate = original.replacen(
        "\"event\":\"event\"",
        "\"event\":\"event\",\"event\":\"event\"",
        1,
    );
    assert_ne!(original, duplicate);
    assert_eq!(
        runner.execute_wire(duplicate.as_bytes(), &policy, 100, &AtomicBool::new(false)),
        Err(Error::Invalid)
    );
    let mut unknown = value;
    unknown["request"]["role"] = serde_json::json!("owner");
    assert_eq!(
        runner.execute_wire(
            &serde_json::to_vec(&unknown).unwrap(),
            &policy,
            100,
            &AtomicBool::new(false)
        ),
        Err(Error::Invalid)
    );
}

#[test]
fn every_signed_authority_pin_rejects_tampering() {
    let dir = tempfile::tempdir().unwrap();
    let mut runner = open(&dir.path().join("db"));
    let (ingress, policy) = setup();
    let original = serde_json::to_value(issue(&ingress, "event", "nonce", command())).unwrap();
    for field in [
        "issuer",
        "key_id",
        "caller",
        "delegation",
        "destination",
        "runner",
        "reply_node",
        "reply_guest",
        "owner",
        "namespace",
        "operation",
        "nonce",
    ] {
        let mut value = original.clone();
        value["claims"][field] = serde_json::json!("tampered");
        assert!(
            runner
                .execute_wire(
                    &serde_json::to_vec(&value).unwrap(),
                    &policy,
                    100,
                    &AtomicBool::new(false)
                )
                .is_err(),
            "{field}"
        );
    }
    for field in ["version", "policy_revision", "issued_at", "expires_at"] {
        let mut value = original.clone();
        value["claims"][field] = serde_json::json!(999);
        assert!(
            runner
                .execute_wire(
                    &serde_json::to_vec(&value).unwrap(),
                    &policy,
                    100,
                    &AtomicBool::new(false)
                )
                .is_err(),
            "{field}"
        );
    }
    for field in [
        "id",
        "subject",
        "content",
        "source",
        "source_version",
        "lineage",
    ] {
        let mut value = original.clone();
        value["request"]["command"][field] = serde_json::json!("tampered");
        assert_eq!(
            runner.execute_wire(
                &serde_json::to_vec(&value).unwrap(),
                &policy,
                100,
                &AtomicBool::new(false)
            ),
            Err(Error::Denied),
            "{field}"
        );
    }
}

#[test]
fn trusted_issuer_cannot_use_another_issuers_grant() {
    let dir = tempfile::tempdir().unwrap();
    let mut runner = open(&dir.path().join("db"));
    let (_, mut policy) = setup();
    let key = SigningKey::from_bytes(&[8; 32]);
    policy.issuers.insert(
        "foreign-issuer".into(),
        TrustedIssuer {
            key_id: "foreign-key".into(),
            public_key: key.verifying_key().to_bytes(),
        },
    );
    let mut foreign_grants = policy.grants.clone();
    foreign_grants.get_mut("caller-1").unwrap().issuer = "foreign-issuer".into();
    let foreign = Ingress::new(
        "foreign-issuer".into(),
        "foreign-key".into(),
        key,
        foreign_grants,
    );
    let envelope = issue(&foreign, "event", "nonce", command());
    assert_eq!(
        runner.execute(&envelope, &policy, 100, &AtomicBool::new(false)),
        Err(Error::Denied)
    );
}

#[cfg(unix)]
#[test]
fn signing_config_rejects_public_permissions_and_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("issuer.json");
    let (_, policy) = setup();
    std::fs::write(&path,serde_json::to_vec(&serde_json::json!({"issuer":"ingress-hotel","key_id":"test-key","signing_seed":vec![7;32],"grants":policy.grants})).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(IngressConfig::load(&path), Err(Error::Denied)));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(IngressConfig::load(&path).is_ok());
    let link = dir.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(matches!(IngressConfig::load(&link), Err(Error::Denied)));
}
