#[path = "../../../crates/aiua/src/service/scoped_vault_activation.rs"]
pub mod scoped_vault_activation;
// Compile actual listener/adapter and actual vault resolve/encrypt/decrypt
// functions. Graph storage and cipher key/nonce are isolated synthetic fixtures;
// no environment/keychain, general IPC, credential files or live hotel are used.
extern crate self as ansible_mesh_core;
#[path = "../../../crates/aiua/src/service/scoped_vault.rs"]
pub mod scoped_vault;
#[path = "../../../crates/aiua/src/service/scoped_vault_resolver.rs"]
pub mod scoped_vault_resolver;
pub mod domain {
    #[derive(Clone)]
    pub struct GraphDomain(pub std::sync::Arc<std::sync::Mutex<State>>);
    #[derive(Clone)]
    pub struct Record {
        pub allowed_roles: Vec<String>,
        pub allowed_guests: Vec<String>,
        pub nonce_b64: String,
        pub ciphertext_b64: String,
    }
    pub struct State {
        pub record: Record,
        pub lookups: Vec<String>,
        pub replace_after_read: bool,
    }
    impl GraphDomain {
        pub fn get_secret(&self, reference: &str) -> Result<Option<Record>, std::io::Error> {
            let mut state = self.0.lock().unwrap();
            state.lookups.push(reference.into());
            let snapshot = state.record.clone();
            if state.replace_after_read {
                state.record.allowed_roles.clear();
                state.record.allowed_guests.clear();
                let (ciphertext, nonce) = crate::vault::fixture_encrypt("mk_other_synthetic");
                state.record.ciphertext_b64 = ciphertext;
                state.record.nonce_b64 = nonce;
            }
            Ok(Some(snapshot))
        }
    }
}
pub mod vault {
    use super::domain::{GraphDomain, Record as SecretRecord};
    use aes_gcm::{
        Aes256Gcm, Key, Nonce,
        aead::{Aead, KeyInit},
    };
    use anyhow::{Context, Result, bail};
    use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
    pub struct SecretAccess {
        pub role: String,
        pub guest_id: String,
    }
    // Only the synthetic cipher provider is substituted. The production
    // resolver/check/decryption code below is compiled directly from vault.rs.
    fn cipher() -> Result<Aes256Gcm> {
        Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&[7; 32])))
    }
    fn random_nonce() -> [u8; 12] {
        [3; 12]
    }
    include!(concat!(env!("OUT_DIR"), "/vault_functions.rs"));
    pub fn fixture_encrypt(value: &str) -> (String, String) {
        encrypt(value).unwrap()
    }
}
#[cfg(test)]
mod adapter_tests {
    use super::*;
    use scoped_vault::{GUEST, Policy, ROLE, Resolver};
    fn fixture(roles: Vec<&str>, guests: Vec<&str>, race: bool) -> domain::GraphDomain {
        let (ciphertext, nonce) = vault::fixture_encrypt("mk_synthetic");
        domain::GraphDomain(std::sync::Arc::new(std::sync::Mutex::new(domain::State {
            record: domain::Record {
                allowed_roles: roles.into_iter().map(str::to_owned).collect(),
                allowed_guests: guests.into_iter().map(str::to_owned).collect(),
                nonce_b64: nonce,
                ciphertext_b64: ciphertext,
            },
            lookups: vec![],
            replace_after_read: race,
        })))
    }
    #[test]
    fn actual_adapter_and_encrypted_resolver_require_exact_acls_and_one_read() {
        let reference = "secret://hotel/default/percival-muninn-observe/synthetic";
        let policy = Policy::new(1234, 999, reference.into()).unwrap();
        for (roles, guests, allowed) in [
            (vec![ROLE], vec![GUEST], true),
            (vec![], vec![GUEST], false),
            (vec![ROLE], vec![], false),
            (vec![ROLE, "hotel.internal"], vec![GUEST], false),
            (vec![ROLE], vec![GUEST, "other"], false),
        ] {
            let graph = fixture(roles, guests, false);
            let result = scoped_vault_resolver::HotelVaultResolver(graph.clone()).resolve(&policy);
            assert_eq!(result.is_ok(), allowed);
            if allowed {
                assert_eq!(result.unwrap().as_str(), "mk_synthetic");
            }
            assert_eq!(graph.0.lock().unwrap().lookups, [reference]);
        }
    }
    #[test]
    fn concurrent_acl_and_ciphertext_replacement_cannot_change_checked_snapshot() {
        let reference = "secret://hotel/default/percival-muninn-observe/synthetic";
        let graph = fixture(vec![ROLE], vec![GUEST], true);
        let policy = Policy::new(1234, 999, reference.into()).unwrap();
        let result = scoped_vault_resolver::HotelVaultResolver(graph.clone())
            .resolve(&policy)
            .unwrap();
        assert_eq!(result.as_str(), "mk_synthetic"); // Not the concurrently replaced unrestricted record.
        assert_eq!(graph.0.lock().unwrap().lookups, [reference]);
        assert!(
            scoped_vault_resolver::HotelVaultResolver(graph)
                .resolve(&policy)
                .is_err()
        );
    }
    #[test]
    fn legacy_resolver_retains_its_existing_wildcard_semantics() {
        let graph = fixture(vec![], vec![], false);
        assert!(
            vault::resolve_secret(
                &graph,
                "synthetic",
                &vault::SecretAccess {
                    role: ROLE.into(),
                    guest_id: GUEST.into()
                }
            )
            .unwrap()
            .is_some()
        );
        assert!(
            vault::resolve_secret_exact_acl(
                &graph,
                "synthetic",
                &vault::SecretAccess {
                    role: ROLE.into(),
                    guest_id: GUEST.into()
                }
            )
            .is_err()
        );
    }
}
