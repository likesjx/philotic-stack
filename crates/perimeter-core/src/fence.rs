use ansible_mesh_core::ExposureTier;

/// Decision returned by the ingress fence gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressDecision {
    Allow,
    /// Request denied by the fence. `status` is the HTTP status code (401/403).
    Deny {
        status: u16,
        message: String,
    },
}

impl IngressDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, IngressDecision::Allow)
    }
}

/// Listener-level ingress check run before any per-route auth.
///
/// The fence checks structural requirements (is a token present?) not validity
/// (is the token correct?). Per-route auth does the cryptographic check.
///
/// Policy:
/// - Local:    Always allow. Loopback-only listener; trust is implicit.
/// - Lan:      Always allow. Optional bearer at route level only.
/// - Mesh:     Bearer token required unless request is from loopback
///             (loopback callers are local hotel tooling, not remote mesh clients).
/// - Internet: Bearer token required. No loopback bypass — callers must authenticate.
pub fn check_ingress(
    tier: ExposureTier,
    auth_header: Option<&str>,
    is_loopback: bool,
) -> IngressDecision {
    match tier {
        ExposureTier::Local | ExposureTier::Lan => IngressDecision::Allow,
        ExposureTier::Mesh => {
            if is_loopback {
                return IngressDecision::Allow;
            }
            if has_bearer(auth_header) {
                IngressDecision::Allow
            } else {
                IngressDecision::Deny {
                    status: 401,
                    message: "Mesh-tier listener requires Authorization: Bearer token".into(),
                }
            }
        }
        ExposureTier::Internet => {
            if has_bearer(auth_header) {
                IngressDecision::Allow
            } else {
                IngressDecision::Deny {
                    status: 401,
                    message: "Internet-tier listener requires Authorization: Bearer token".into(),
                }
            }
        }
    }
}

fn has_bearer(auth_header: Option<&str>) -> bool {
    auth_header
        .map(|h| h.trim_start().to_ascii_lowercase().starts_with("bearer "))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_always_allowed() {
        assert!(check_ingress(ExposureTier::Local, None, false).is_allowed());
        assert!(check_ingress(ExposureTier::Local, None, true).is_allowed());
    }

    #[test]
    fn lan_always_allowed() {
        assert!(check_ingress(ExposureTier::Lan, None, false).is_allowed());
    }

    #[test]
    fn mesh_requires_bearer_from_non_loopback() {
        assert!(!check_ingress(ExposureTier::Mesh, None, false).is_allowed());
        assert!(!check_ingress(ExposureTier::Mesh, Some("Basic xyz"), false).is_allowed());
        assert!(check_ingress(ExposureTier::Mesh, Some("Bearer tok"), false).is_allowed());
    }

    #[test]
    fn mesh_loopback_bypasses_bearer() {
        assert!(check_ingress(ExposureTier::Mesh, None, true).is_allowed());
    }

    #[test]
    fn internet_requires_bearer_no_loopback_bypass() {
        assert!(!check_ingress(ExposureTier::Internet, None, false).is_allowed());
        assert!(!check_ingress(ExposureTier::Internet, None, true).is_allowed());
        assert!(check_ingress(ExposureTier::Internet, Some("Bearer tok"), true).is_allowed());
    }

    #[test]
    fn bearer_case_insensitive() {
        assert!(check_ingress(ExposureTier::Internet, Some("bearer mytoken"), false).is_allowed());
        assert!(check_ingress(ExposureTier::Internet, Some("BEARER mytoken"), false).is_allowed());
    }
}
