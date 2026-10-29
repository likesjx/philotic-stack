use ed25519_dalek::SigningKey;
use native_memory::{response::*, *};
use std::collections::BTreeMap;
fn fixture() -> (Envelope, Response, Vec<ResponseVerifier>) {
    let grant = Grant {
        issuer: "ingress".into(),
        reply_node: "ingress".into(),
        reply_guest: "mcp".into(),
        principal: "alice".into(),
        owner: "alice".into(),
        namespace: "notes".into(),
        destination: "destination".into(),
        runner: "memory".into(),
        operations: vec!["remember".into()],
        revision: 1,
        expires_at: 1000,
        revoked: false,
    };
    let ingress = Ingress::new(
        "ingress".into(),
        "issuer-key".into(),
        SigningKey::from_bytes(&[7; 32]),
        BTreeMap::from([("caller".into(), grant)]),
    );
    let request = ingress
        .issue(
            "caller",
            Command::Remember {
                id: "note".into(),
                subject: "practice".into(),
                content: "synthetic observation".into(),
                source: "synthetic".into(),
                source_version: "1".into(),
                lineage: "independent".into(),
            },
            "event".into(),
            "nonce".into(),
            100,
        )
        .unwrap();
    let result = Response {
        receipt: Receipt {
            event: "event".into(),
            operation: "remember".into(),
            status: "committed".into(),
            committed_at: 100,
        },
        observations: vec![],
    };
    let verifiers = vec![ResponseVerifier {
        destination: "destination".into(),
        runner: "memory".into(),
        key_id: "result-key".into(),
        public_key: SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes(),
    }];
    (request, result, verifiers)
}
#[test]
fn signed_result_binds_entire_request_and_result() {
    let (request, result, verifiers) = fixture();
    let signer = ResponseSigner::new("result-key".into(), [8; 32]);
    let reply = signer.sign(&request, result.clone(), 101).unwrap();
    let expectation = ResponseExpectation::new(request.clone(), &verifiers).unwrap();
    assert_eq!(expectation.verify(&reply, &verifiers, 101).unwrap(), result);
    let mut tampered = reply.clone();
    tampered.result.receipt.event = "another".into();
    assert!(expectation.verify(&tampered, &verifiers, 101).is_err());
    let mut different = request.clone();
    different.claims.nonce = "other-request".into();
    assert!(
        ResponseExpectation::new(different, &verifiers)
            .unwrap()
            .verify(&reply, &verifiers, 101)
            .is_err()
    );
    let mut different = request;
    different.request.owner = "bob".into();
    assert!(
        ResponseExpectation::new(different, &verifiers)
            .unwrap()
            .verify(&reply, &verifiers, 101)
            .is_err()
    );
}
#[test]
fn expired_removed_wrong_and_ambiguous_response_keys_fail_closed() {
    let (request, result, verifiers) = fixture();
    let expectation = ResponseExpectation::new(request.clone(), &verifiers).unwrap();
    let reply = ResponseSigner::new("result-key".into(), [8; 32])
        .sign(&request, result.clone(), 101)
        .unwrap();
    assert_eq!(
        expectation.verify(&reply, &verifiers, request.claims.expires_at),
        Err(Error::Expired)
    );
    assert!(expectation.verify(&reply, &[], 101).is_err());
    let wrong = ResponseSigner::new("result-key".into(), [9; 32])
        .sign(&request, result, 101)
        .unwrap();
    assert!(expectation.verify(&wrong, &verifiers, 101).is_err());
    assert!(
        expectation
            .verify(&reply, &[verifiers[0].clone(), verifiers[0].clone()], 101)
            .is_err()
    );
}
#[test]
fn trusted_signer_cannot_substitute_receipt_or_admit_other_owner() {
    let (request, mut result, verifiers) = fixture();
    let expectation = ResponseExpectation::new(request.clone(), &verifiers).unwrap();
    result.receipt.operation = "recall".into();
    let wrong = ResponseSigner::new("result-key".into(), [8; 32])
        .sign(&request, result, 101)
        .unwrap();
    assert!(expectation.verify(&wrong, &verifiers, 101).is_err());
    let (_, mut result, _) = fixture();
    result.observations.push(Observation {
        id: "x".into(),
        owner: "bob".into(),
        subject: "s".into(),
        authenticated_writer: "bob".into(),
        content: "other private data".into(),
        source: "s".into(),
        source_version: "1".into(),
        lineage: "l".into(),
        kind: "observation".into(),
    });
    let wrong = ResponseSigner::new("result-key".into(), [8; 32])
        .sign(&request, result, 101)
        .unwrap();
    assert!(expectation.verify(&wrong, &verifiers, 101).is_err());
}

#[test]
fn recalled_content_is_authenticated_and_owner_checked_independently() {
    let (mut request, mut result, verifiers) = fixture();
    request.request.command = Command::Recall {
        query: "synthetic".into(),
        limit: 1,
    };
    request.claims.operation = "recall".into();
    result.receipt.operation = "recall".into();
    result.observations.push(Observation {
        id: "x".into(),
        owner: "alice".into(),
        subject: "s".into(),
        authenticated_writer: "alice".into(),
        content: "original".into(),
        source: "s".into(),
        source_version: "1".into(),
        lineage: "l".into(),
        kind: "observation".into(),
    });
    let expectation = ResponseExpectation::new(request.clone(), &verifiers).unwrap();
    let signer = ResponseSigner::new("result-key".into(), [8; 32]);
    let mut reply = signer.sign(&request, result.clone(), 101).unwrap();
    assert!(expectation.verify(&reply, &verifiers, 101).is_ok());
    reply.result.observations[0].content = "substituted".into();
    assert!(expectation.verify(&reply, &verifiers, 101).is_err());
    result.observations[0].owner = "bob".into();
    let reply = signer.sign(&request, result, 101).unwrap();
    assert!(expectation.verify(&reply, &verifiers, 101).is_err());
}

#[test]
fn transport_metadata_cannot_change_the_strict_signed_envelope() {
    let (request, result, verifiers) = fixture();
    let expectation = ResponseExpectation::new(request.clone(), &verifiers).unwrap();
    let signed = ResponseSigner::new("result-key".into(), [8; 32])
        .sign(&request, result, 101)
        .unwrap();
    let mut wire = serde_json::to_value(RoutedRequest::new(request)).unwrap();
    wire["delivery_target_guest_id"] = "untrusted-metadata".into();
    wire["caller"] = "admin".into();
    let decoded: RoutedRequest = serde_json::from_str(&wire.to_string()).unwrap();
    assert_eq!(decoded.signed_request.claims.caller, "alice");
    assert!(expectation.verify(&signed, &verifiers, 101).is_ok());
    wire["signed_request"]["caller"] = "admin".into();
    assert!(serde_json::from_str::<RoutedRequest>(&wire.to_string()).is_err());
}
