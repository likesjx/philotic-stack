//! RelayLink lifecycle: an always-on hotel link connects, sends and receives
//! over real QUIC, and reconnects on its own after the relay drops.

use ansible_mesh_core::membership::verifying_key_to_base64url;
use ed25519_dalek::{SigningKey, VerifyingKey};
use mesh_relay::link::{RelayLink, RelayLinkConfig};
use mesh_relay::protocol::{Plane, ServerMsg};
use mesh_relay::quic::{RelayIdentity, bind_relay, connect_relay, serve_relay};
use mesh_relay::transport::decode_inner;
use mesh_relay::{KeyResolver, StaticKeyResolver};
use rand::rngs::OsRng;
use std::sync::Arc;
use std::time::Duration;

/// Build a resolver from (node_id, public key) pairs — public keys captured
/// before any signing key is moved into a link.
fn resolver(pairs: &[(&str, &VerifyingKey)]) -> Arc<dyn KeyResolver> {
    let mut r = StaticKeyResolver::new();
    for (id, vk) in pairs {
        r.insert(*id, verifying_key_to_base64url(vk));
    }
    Arc::new(r)
}

#[tokio::test]
async fn link_delivers_inbound_and_outbound() {
    let sk_a = SigningKey::generate(&mut OsRng);
    let sk_b = SigningKey::generate(&mut OsRng);
    let vk_a = sk_a.verifying_key();
    let vk_b = sk_b.verifying_key();

    let identity = RelayIdentity::generate().unwrap();
    let pinned = identity.cert_der.clone();
    let (endpoint, server) = bind_relay("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let relay_addr = endpoint.local_addr().unwrap();
    tokio::spawn(serve_relay(
        endpoint,
        server,
        resolver(&[("mac-jane-aiua-01", &vk_a), ("mbp-jane-aiua-01", &vk_b)]),
    ));

    let cfg = RelayLinkConfig::new(
        relay_addr,
        pinned.clone(),
        "mac-jane-aiua-01".into(),
        Arc::new(sk_a),
    );
    let (link, mut inbound) = RelayLink::spawn(cfg);
    let mut b = connect_relay(relay_addr, pinned.clone(), "mbp-jane-aiua-01".into(), &sk_b)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Outbound: A's link -> B.
    link.try_send("mbp-jane-aiua-01", Plane::Execution, b"a-to-b".to_vec())
        .expect("enqueue");
    let got = tokio::time::timeout(Duration::from_secs(3), b.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match got {
        ServerMsg::Deliver {
            from_node_id,
            inner_b64,
            ..
        } => {
            assert_eq!(from_node_id, "mac-jane-aiua-01");
            assert_eq!(decode_inner(&inner_b64).unwrap(), b"a-to-b");
        }
        other => panic!("expected Deliver, got {other:?}"),
    }

    // Inbound: B -> A's link (surfaced on the link's inbound receiver).
    b.send("mac-jane-aiua-01", Plane::Beacon, b"b-to-a")
        .await
        .unwrap();
    let delivered = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.from_node_id, "mbp-jane-aiua-01");
    assert_eq!(delivered.plane, Plane::Beacon);
    assert_eq!(delivered.inner, b"b-to-a");
}

#[tokio::test]
async fn link_reconnects_after_relay_restart() {
    let sk_a = SigningKey::generate(&mut OsRng);
    let sk_b = SigningKey::generate(&mut OsRng);
    let vk_a = sk_a.verifying_key();
    let vk_b = sk_b.verifying_key();
    let keys: Vec<(&str, &VerifyingKey)> =
        vec![("mac-jane-aiua-01", &vk_a), ("mbp-jane-aiua-01", &vk_b)];

    // Fixed relay identity + address reused across the "restart".
    let identity = RelayIdentity::generate().unwrap();
    let pinned = identity.cert_der.clone();

    let (endpoint1, server1) = bind_relay("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let relay_addr = endpoint1.local_addr().unwrap();
    tokio::spawn(serve_relay(endpoint1.clone(), server1, resolver(&keys)));

    let cfg = RelayLinkConfig {
        base_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_millis(400),
        ..RelayLinkConfig::new(
            relay_addr,
            pinned.clone(),
            "mac-jane-aiua-01".into(),
            Arc::new(sk_a),
        )
    };
    let (link, _inbound) = RelayLink::spawn(cfg);
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Drop the first relay; the link's connection breaks.
    endpoint1.close(0u32.into(), b"restart");
    drop(endpoint1);
    tokio::time::sleep(Duration::from_millis(250)).await;

    // New relay on the SAME address + identity.
    let (endpoint2, server2) = loop {
        match bind_relay(relay_addr, &identity) {
            Ok(pair) => break pair,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };
    tokio::spawn(serve_relay(endpoint2, server2, resolver(&keys)));

    let mut b = connect_relay(relay_addr, pinned.clone(), "mbp-jane-aiua-01".into(), &sk_b)
        .await
        .unwrap();

    // Within a few backoff cycles the link should reconnect and a send reach B.
    for _ in 0..30 {
        if link
            .try_send(
                "mbp-jane-aiua-01",
                Plane::Execution,
                b"after-reconnect".to_vec(),
            )
            .is_ok()
            && let Ok(Ok(Some(ServerMsg::Deliver { inner_b64, .. }))) =
                tokio::time::timeout(Duration::from_millis(300), b.recv()).await
        {
            assert_eq!(decode_inner(&inner_b64).unwrap(), b"after-reconnect");
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("link did not reconnect and deliver after relay restart");
}
