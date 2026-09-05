//! Outbound-dial QUIC relay so hotels stay meshed through vps when the direct
//! (Tailscale) path is down.
//!
//! This crate is deliberately transport-light at this stage: it defines the
//! relay [`protocol`] — the wire messages and the ed25519 challenge-response
//! that authenticates a connection as a specific mesh node — plus the
//! [`KeyResolver`] seam the relay uses to look up published member public keys
//! without depending on the hotel's storage layer. The QUIC server and client
//! are layered on top of these primitives in subsequent slices.
//!
//! See [`protocol`] for the trust model.

pub mod protocol;
pub mod transport;

/// Resolves a mesh node's **published** ed25519 member public key (base64url),
/// as stored in `config:mesh_member_public_key:<hotel>`.
///
/// The relay uses this to obtain the key it will verify a challenge against. It
/// must come from the relay's own trusted store — never from the connecting
/// client — or the impersonation-to-receive protection is void. Kept as a trait
/// so the relay is decoupled from the hotel graph/DB and can be exercised in
/// tests with an in-memory map.
pub trait KeyResolver: Send + Sync {
    /// Return the base64url member public key for `node_id`, or `None` if this
    /// node is not a known mesh member (the relay then refuses the connection).
    fn member_public_key(&self, node_id: &str) -> Option<String>;
}

/// A fixed in-memory [`KeyResolver`], for tests and simple static deployments.
#[derive(Debug, Default, Clone)]
pub struct StaticKeyResolver {
    keys: std::collections::HashMap<String, String>,
}

impl StaticKeyResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `node_id`'s base64url member public key.
    pub fn insert(&mut self, node_id: impl Into<String>, member_public_key_b64: impl Into<String>) {
        self.keys
            .insert(node_id.into(), member_public_key_b64.into());
    }
}

impl KeyResolver for StaticKeyResolver {
    fn member_public_key(&self, node_id: &str) -> Option<String> {
        self.keys.get(node_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_resolver_returns_registered_key_only() {
        let mut r = StaticKeyResolver::new();
        r.insert("mac-jane-aiua-01", "pk-b64");
        assert_eq!(
            r.member_public_key("mac-jane-aiua-01"),
            Some("pk-b64".to_string())
        );
        assert_eq!(r.member_public_key("unknown-node"), None);
    }
}
