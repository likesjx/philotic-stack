//! Real-QUIC end-to-end: two hotels dial a bound relay over actual UDP sockets,
//! pin its self-signed cert, authenticate, and relay a frame A -> B. Same
//! milestone as the in-process duplex test, over the production transport.

use ansible_mesh_core::membership::verifying_key_to_base64url;
use ed25519_dalek::SigningKey;
use mesh_relay::StaticKeyResolver;
use mesh_relay::protocol::{Plane, ServerMsg};
use mesh_relay::quic::{RelayIdentity, bind_relay, connect_relay, serve_relay};
use rand::rngs::OsRng;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn quic_frame_relays_from_a_to_b() {
    let mut resolver = StaticKeyResolver::new();
    let sk_a = SigningKey::generate(&mut OsRng);
    let sk_b = SigningKey::generate(&mut OsRng);
    resolver.insert(
        "mac-jane-aiua-01",
        verifying_key_to_base64url(&sk_a.verifying_key()),
    );
    resolver.insert(
        "mbp-jane-aiua-01",
        verifying_key_to_base64url(&sk_b.verifying_key()),
    );
    let resolver: Arc<dyn mesh_relay::KeyResolver> = Arc::new(resolver);

    let identity = RelayIdentity::generate().expect("relay identity");
    let pinned = identity.cert_der.clone();
    let (endpoint, server) = bind_relay("127.0.0.1:0".parse().unwrap(), &identity).expect("bind");
    let relay_addr = endpoint.local_addr().expect("relay addr");

    tokio::spawn(serve_relay(endpoint, server, resolver));

    let mut a = connect_relay(relay_addr, pinned.clone(), "mac-jane-aiua-01".into(), &sk_a)
        .await
        .expect("A dials + authenticates");
    let mut b = connect_relay(relay_addr, pinned.clone(), "mbp-jane-aiua-01".into(), &sk_b)
        .await
        .expect("B dials + authenticates");

    // Give B's registration a beat to land before A sends.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let payload = b"opaque-hmac-signed-mesh-frame-over-quic";
    a.send("mbp-jane-aiua-01", Plane::Execution, payload)
        .await
        .expect("A sends");

    let delivered = tokio::time::timeout(Duration::from_secs(3), b.recv())
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
            assert_eq!(
                mesh_relay::transport::decode_inner(&inner_b64).unwrap(),
                payload
            );
        }
        other => panic!("expected Deliver, got {other:?}"),
    }
}

#[tokio::test]
async fn quic_wrong_pinned_cert_is_rejected() {
    // A client that pins the WRONG cert must fail the TLS handshake — proves the
    // pin is actually enforced, not decorative.
    let mut resolver = StaticKeyResolver::new();
    let sk = SigningKey::generate(&mut OsRng);
    resolver.insert(
        "mac-jane-aiua-01",
        verifying_key_to_base64url(&sk.verifying_key()),
    );
    let resolver: Arc<dyn mesh_relay::KeyResolver> = Arc::new(resolver);

    let identity = RelayIdentity::generate().expect("relay identity");
    let (endpoint, server) = bind_relay("127.0.0.1:0".parse().unwrap(), &identity).expect("bind");
    let relay_addr = endpoint.local_addr().expect("addr");
    tokio::spawn(serve_relay(endpoint, server, resolver));

    // Pin a DIFFERENT self-signed cert than the one the relay presents.
    let wrong = RelayIdentity::generate().expect("other identity");
    let result = connect_relay(
        relay_addr,
        wrong.cert_der.clone(),
        "mac-jane-aiua-01".into(),
        &sk,
    )
    .await;
    assert!(result.is_err(), "wrong pinned cert must be rejected at TLS");
}
