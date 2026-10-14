//! Scripted Bolt peers prove sequencing only, not real Memgraph compatibility.
use neo4rs::{ConfigBuilder, Error, Graph, Operation, query};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

const LIMIT: Duration = Duration::from_secs(5);

async fn receive(socket: &mut TcpStream) -> Vec<u8> {
    let mut message = Vec::new();
    loop {
        let size = socket.read_u16().await.unwrap() as usize;
        if size == 0 {
            return message;
        }
        let start = message.len();
        message.resize(start + size, 0);
        socket.read_exact(&mut message[start..]).await.unwrap();
    }
}

async fn send(socket: &mut TcpStream, message: &[u8]) {
    socket.write_u16(message.len() as u16).await.unwrap();
    socket.write_all(message).await.unwrap();
    socket.write_u16(0).await.unwrap();
}

async fn expect(socket: &mut TcpStream, signature: u8) -> Vec<u8> {
    let message = receive(socket).await;
    assert_eq!(
        message.get(1),
        Some(&signature),
        "unexpected Bolt message: {message:?}"
    );
    message
}

fn contains(message: &[u8], value: &[u8]) -> bool {
    message.windows(value.len()).any(|part| part == value)
}

async fn success(socket: &mut TcpStream) {
    send(socket, &[0xb1, 0x70, 0xa0]).await;
}

async fn run(socket: &mut TcpStream, statement: &str) -> Vec<u8> {
    let message = expect(socket, 0x10).await;
    assert!(
        contains(&message, statement.as_bytes()),
        "wrong RUN: {message:?}"
    );
    // SUCCESS {fields: ["value"]}
    send(socket, b"\xb1\x70\xa1\x86fields\x91\x85value").await;
    message
}

async fn metadata(socket: &mut TcpStream, statement: &str, count: usize) {
    run(socket, statement).await;
    let mut remaining = count;
    loop {
        expect(socket, 0x3f).await; // PULL
        let batch = remaining.min(32);
        for _ in 0..batch {
            send(socket, &[0xb1, 0x71, 0x91, 0x01]).await;
        }
        remaining -= batch;
        if remaining == 0 {
            success(socket).await;
            break;
        }
        send(socket, b"\xb1\x70\xa1\x88has_more\xc3").await;
    }
}

#[derive(Clone, Copy)]
enum Case {
    Read,
    Write,
    LegacyRead,
    ValidatorFailure,
    TooManyRows,
    SecondQueryFailure,
}

async fn exercise(case: Case) {
    timeout(LIMIT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut handshake = [0; 20];
            socket.read_exact(&mut handshake).await.unwrap();
            assert_eq!(&handshake[..4], &[0x60, 0x60, 0xb0, 0x17]);
            socket.write_all(&[0, 0, 1, 4]).await.unwrap(); // Bolt 4.1
            expect(&mut socket, 0x01).await; // HELLO
            success(&mut socket).await;
            run(&mut socket, "SET SESSION").await;
            expect(&mut socket, 0x2f).await; // setup stream discarded
            success(&mut socket).await;
            metadata(&mut socket, "STORAGE", if matches!(case, Case::TooManyRows) {129} else {33}).await;
            if !matches!(case, Case::LegacyRead | Case::TooManyRows) {
                if matches!(case, Case::SecondQueryFailure) {
                    let message = expect(&mut socket, 0x10).await;
                    assert!(contains(&message, b"CONSTRAINTS"));
                    // A retriable server error must still not cause a retry here.
                    let code = b"Neo.TransientError.General.DatabaseUnavailable";
                    let mut failure = b"\xb1\x7f\xa2\x84code\xd0".to_vec();
                    failure.push(code.len() as u8);
                    failure.extend_from_slice(code);
                    failure.extend_from_slice(b"\x87message\x84boom");
                    send(&mut socket, &failure).await;
                } else { metadata(&mut socket, "CONSTRAINTS", 33).await; }
            }
            if matches!(case, Case::Read | Case::Write | Case::LegacyRead) {
                expect(&mut socket, 0x11).await; // BEGIN only after both streams finish
                success(&mut socket).await;
                let message = run(&mut socket, "INSIDE").await;
                let mode = if matches!(case, Case::Write) { b"\x84mode\x81w" } else { b"\x84mode\x81r" };
                assert!(contains(&message, mode), "transaction lost operation mode");
                expect(&mut socket, 0x2f).await;
                success(&mut socket).await;
                expect(&mut socket, 0x13).await; // ROLLBACK
                success(&mut socket).await;
            }
            // Pool cleanup may RESET, but must never retry RUN or issue BEGIN.
            timeout(Duration::from_millis(150), async {
                loop {
                    tokio::select! {
                        connection = listener.accept() => panic!("second physical connection: {connection:?}"),
                        message = receive(&mut socket) => {
                            assert_eq!(message.get(1), Some(&0x0f), "unexpected request after completion: {message:?}");
                            success(&mut socket).await;
                        }
                    }
                }
            }).await.unwrap_err();
        });
        let config = ConfigBuilder::default().uri(format!("bolt://{address}")).user("test").password("test").max_connections(2).build().unwrap();
        let graph = Graph::connect(config).unwrap();
        if matches!(case, Case::LegacyRead) {
            let mut txn = graph.start_txn_with_verified_session(query("SET SESSION"), query("STORAGE"), |rows| { assert_eq!(rows.len(), 33); Ok(()) }).await.unwrap();
            txn.run(query("INSIDE")).await.unwrap();
            txn.rollback().await.unwrap();
        } else {
            let result = graph.start_txn_with_verified_metadata(
                if matches!(case, Case::Write) { Operation::Write } else { Operation::Read },
                query("SET SESSION"), [query("STORAGE"), query("CONSTRAINTS")],
                |rows| {
                    assert_eq!(rows[0].len(), 33);
                    assert_eq!(rows[1].len(), 33);
                    assert_eq!(rows[1][32].get::<i64>("value").unwrap(), 1);
                    if matches!(case, Case::ValidatorFailure) { Err(Error::InvalidConfig) } else { Ok("validated constraints") }
                },
            ).await;
            match case {
                Case::Read | Case::Write => {
                    let (mut txn, metadata) = result.unwrap();
                    assert_eq!(metadata, "validated constraints");
                    txn.run(query("INSIDE")).await.unwrap();
                    txn.rollback().await.unwrap();
                }
                Case::ValidatorFailure => assert!(matches!(result, Err(Error::InvalidConfig))),
                Case::TooManyRows => assert!(matches!(result, Err(Error::ListTooLong))),
                Case::SecondQueryFailure => assert!(matches!(result, Err(Error::Neo4j(_)))),
                Case::LegacyRead => unreachable!(),
            }
        }
        server.await.unwrap();
        drop(graph);
    }).await.expect("Bolt sequencing test timed out");
}

#[tokio::test]
async fn read_metadata_stays_on_one_connection_through_rollback() {
    exercise(Case::Read).await;
}
#[tokio::test]
async fn write_metadata_stays_on_one_connection_through_rollback() {
    exercise(Case::Write).await;
}
#[tokio::test]
async fn legacy_verified_session_keeps_read_mode() {
    exercise(Case::LegacyRead).await;
}
#[tokio::test]
async fn validator_rejection_never_begins() {
    exercise(Case::ValidatorFailure).await;
}
#[tokio::test]
async fn metadata_over_128_rows_never_begins() {
    exercise(Case::TooManyRows).await;
}
#[tokio::test]
async fn second_metadata_failure_never_retries_or_begins() {
    exercise(Case::SecondQueryFailure).await;
}

#[tokio::test]
async fn stale_loop_action_releases_single_connection_before_replay_lookup() {
    use data_memorygraphrag::loop_action::{Action, FIELDS, LoopAction, apply};
    timeout(LIMIT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut handshake = [0; 20];
            socket.read_exact(&mut handshake).await.unwrap();
            socket.write_all(&[0, 0, 1, 4]).await.unwrap();
            expect(&mut socket, 0x01).await;
            success(&mut socket).await;
            for (index, statement) in [
                "MATCH (a:LifeLoopAction",
                "MATCH (n {id:",
                "MATCH (a:LifeLoopAction",
            ]
            .into_iter()
            .enumerate()
            {
                if index > 0 {
                    expect(&mut socket, 0x0f).await; // pool checkout recycles the released connection
                    success(&mut socket).await;
                }
                metadata(&mut socket, statement, 0).await;
            }
        });
        let graph = Graph::connect(
            ConfigBuilder::default()
                .uri(format!("bolt://{address}"))
                .user("test")
                .password("test")
                .max_connections(1)
                .build()
                .unwrap(),
        )
        .unwrap();
        let input = LoopAction {
            id: "life:open-loop:synthetic".into(),
            actor: "synthetic-operator".into(),
            request_id: "a".repeat(32),
            action: Action::Close,
            before: FIELDS
                .iter()
                .map(|field| ((*field).into(), serde_json::Value::Null))
                .collect(),
            note: "synthetic close".into(),
        };
        assert_eq!(
            apply(&graph, &input, "2026-10-14T00:00:00Z").await.unwrap()["status"],
            "conflict"
        );
        server.await.unwrap();
    })
    .await
    .expect("stale action retained the sole pooled connection");
}
