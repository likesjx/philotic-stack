//! Adapter inside the hotel's existing vault trust boundary; no general IPC.
use super::scoped_vault::{GUEST, Policy, ROLE, Resolver};
use crate::vault::{SecretAccess, resolve_secret_exact_acl_classified};
use ansible_mesh_core::domain::GraphDomain;
use std::io;
use zeroize::Zeroizing;

pub struct HotelVaultResolver(pub GraphDomain);
impl Resolver for HotelVaultResolver {
    fn resolve(&self, policy: &Policy) -> io::Result<Zeroizing<String>> {
        let unavailable = || io::Error::other("credential_unavailable");
        let value = resolve_secret_exact_acl_classified(
            &self.0,
            policy.secret_ref(),
            &SecretAccess {
                role: ROLE.into(),
                guest_id: GUEST.into(),
            },
        )
        .map_err(|reason| {
            // Fixed enum only: the client still receives the same generic error.
            eprintln!("scoped_vault_resolution_failed reason={}", reason.code());
            unavailable()
        })?
        .ok_or_else(|| {
            eprintln!("scoped_vault_resolution_failed reason=record_absent");
            unavailable()
        })?;
        Ok(Zeroizing::new(value))
    }
}
