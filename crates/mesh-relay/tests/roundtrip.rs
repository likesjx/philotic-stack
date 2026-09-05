//! End-to-end relay round-trip over in-process duplex streams (no real socket):
//! two hotels authenticate to one relay and a frame from A is delivered to B.
//! This is the "server + client handshake + frame round-trip between two
//! simulated node_ids" milestone for the vps-relayed mesh fallback.

use ansible_mesh_core::membership::verifying_key_to_base64url;
use ed25519_dalek::SigningKey;
use mesh_relay::StaticKeyResolver;
use mesh_relay::protocol::{Plane, ServerMsg};
use mesh_relay::transport::{RelayClient, RelayServer, decode_inner};
use rand::rngs::OsRng;
use std::sync::Arc;
use std::time::Duration;

fn node(resolver: &mut StaticKeyResolver, id: &str) -> SigningKey {
    let sk = SigningKey::generate(&mut OsRng);
    resolver.insert(id, verifying_key_to_base64url(&sk.verifying_key()));
    sk
}

#[tokio::test]
async fn frame_relays_from_a_to_b() {
    let mut resolver = StaticKeyResolver::new();
    let sk_a = node(&mut resolver, "mac-jane-aiua-01");
    let sk_b = node(&mut resolver, "mbp-jane-aiua-01");
    let resolver = Arc::new(resolver);
    let server = RelayServer::new();

    // Wire each client to the server through an in-memory duplex pipe.
    let (a_client_io, a_server_io) = tokio::io::duplex(64 * 1024);
    let (b_client_io, b_server_io) = tokio::io::duplex(64 * 1024);

    for io in [a_server_io, b_server_io] {
        let server = server.clone();
        let resolver = resolver.clone();
        tokio::spawn(async move {
            let _ = server.handle_connection(io, resolver.as_ref()).await;
        });
    }

    let mut a = RelayClient::connect(a_client_io, "mac-jane-aiua-01".into(), &sk_a)
        .await
        .expect("A authenticates");
    let mut b = RelayClient::connect(b_client_io, "mbp-jane-aiua-01".into(), &sk_b)
        .await
        .expect("B authenticates");

    // Wait until both are registered so the relay can route A -> B.
    for _ in 0..50 {
        if server.connected_node_count().await == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(server.connected_node_count().await, 2);

    let payload = b"opaque-hmac-signed-mesh-frame";
    a.send("mbp-jane-aiua-01", Plane::Execution, payload)
        .await
        .expect("A sends");

    let delivered = tokio::time::timeout(Duration::from_secs(2), b.recv())
        .await
        .expect("no timeout")
        .expect("recv ok")
        .expect("a message");

    match delivered {
        ServerMsg::Deliver {
            from_node_id,
            plane,
            inner_b64,
        } => {
            assert_eq!(from_node_id, "mac-jane-aiua-01");
            assert_eq!(plane, Plane::Execution);
            assert_eq!(decode_inner(&inner_b64).unwrap(), payload);
        }
        other => panic!("expected Deliver, got {other:?}"),
    }
}

#[tokio::test]
async fn forged_signature_is_rejected() {
    // A connection claiming to be mac but signing with the WRONG key must not
    // authenticate — the impersonation-to-receive guard, end to end.
    let mut resolver = StaticKeyResolver::new();
    let _real_mac = node(&mut resolver, "mac-jane-aiua-01");
    let attacker = SigningKey::generate(&mut OsRng); // not mac's key
    let resolver = Arc::new(resolver);
    let server = RelayServer::new();

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    {
        let server = server.clone();
        let resolver = resolver.clone();
        tokio::spawn(async move {
            let _ = server.handle_connection(server_io, resolver.as_ref()).await;
        });
    }

    let result = RelayClient::connect(client_io, "mac-jane-aiua-01".into(), &attacker).await;
    assert!(result.is_err(), "forged identity must be rejected");
    assert_eq!(server.connected_node_count().await, 0);
}

#[tokio::test]
async fn unknown_node_is_rejected() {
    let resolver = Arc::new(StaticKeyResolver::new()); // knows nobody
    let server = RelayServer::new();
    let stranger = SigningKey::generate(&mut OsRng);

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    {
        let server = server.clone();
        let resolver = resolver.clone();
        tokio::spawn(async move {
            let _ = server.handle_connection(server_io, resolver.as_ref()).await;
        });
    }

    let result = RelayClient::connect(client_io, "ghost-node".into(), &stranger).await;
    assert!(result.is_err(), "unknown node must be rejected");
}

#[tokio::test]
async fn relay_to_absent_peer_reports_undeliverable() {
    let mut resolver = StaticKeyResolver::new();
    let sk_a = node(&mut resolver, "mac-jane-aiua-01");
    let resolver = Arc::new(resolver);
    let server = RelayServer::new();

    let (a_client_io, a_server_io) = tokio::io::duplex(64 * 1024);
    {
        let server = server.clone();
        let resolver = resolver.clone();
        tokio::spawn(async move {
            let _ = server
                .handle_connection(a_server_io, resolver.as_ref())
                .await;
        });
    }

    let mut a = RelayClient::connect(a_client_io, "mac-jane-aiua-01".into(), &sk_a)
        .await
        .expect("A authenticates");

    a.send("nobody-home-aiua-01", Plane::Beacon, b"x")
        .await
        .expect("A sends");

    let reply = tokio::time::timeout(Duration::from_secs(2), a.recv())
        .await
        .expect("no timeout")
        .expect("recv ok")
        .expect("a message");
    match reply {
        ServerMsg::Undeliverable { to_node_id } => assert_eq!(to_node_id, "nobody-home-aiua-01"),
        other => panic!("expected Undeliverable, got {other:?}"),
    }
}
