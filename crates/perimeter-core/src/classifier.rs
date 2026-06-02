use std::net::{IpAddr, Ipv4Addr};

use ansible_mesh_core::ExposureTier;

/// Classify a bind address into an ExposureTier.
///
/// `has_public_ip`: whether the host has any detected public IP. Used to disambiguate
/// `0.0.0.0` bindings — unspecified on a machine with a public IP is Internet-exposed.
/// When false, `0.0.0.0` conservatively maps to Lan with a warning.
pub fn classify_bind_addr(addr: IpAddr, has_public_ip: bool) -> ClassifyResult {
    match addr {
        IpAddr::V4(v4) => classify_v4(v4, has_public_ip),
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                ClassifyResult::Definite(ExposureTier::Local)
            } else {
                // IPv6 global unicast — treat as Internet until richer classification is added
                ClassifyResult::Definite(ExposureTier::Internet)
            }
        }
    }
}

fn classify_v4(addr: Ipv4Addr, has_public_ip: bool) -> ClassifyResult {
    if addr.is_loopback() {
        return ClassifyResult::Definite(ExposureTier::Local);
    }
    if is_tailscale_cgnat(addr) {
        return ClassifyResult::Definite(ExposureTier::Mesh);
    }
    if is_rfc1918(addr) {
        return ClassifyResult::Definite(ExposureTier::Lan);
    }
    if addr.is_unspecified() {
        // 0.0.0.0 — depends on whether the host has a public IP
        return if has_public_ip {
            ClassifyResult::Definite(ExposureTier::Internet)
        } else {
            ClassifyResult::Conservative {
                tier: ExposureTier::Lan,
                reason: "0.0.0.0 bind with no detected public IP — assuming Lan",
            }
        };
    }
    ClassifyResult::Definite(ExposureTier::Internet)
}

/// 100.64.0.0/10 — Tailscale CGNAT range (RFC 6598 shared address space)
fn is_tailscale_cgnat(addr: Ipv4Addr) -> bool {
    let o = addr.octets();
    o[0] == 100 && (o[1] & 0xC0) == 0x40 // 100.64.x.x – 100.127.x.x
}

fn is_rfc1918(addr: Ipv4Addr) -> bool {
    let o = addr.octets();
    o[0] == 10 || (o[0] == 172 && (o[1] & 0xF0) == 16) || (o[0] == 192 && o[1] == 168)
}

pub enum ClassifyResult {
    Definite(ExposureTier),
    /// Classification succeeded but required a conservative assumption — caller should log.
    Conservative {
        tier: ExposureTier,
        reason: &'static str,
    },
}

impl ClassifyResult {
    pub fn tier(&self) -> ExposureTier {
        match self {
            ClassifyResult::Definite(t) | ClassifyResult::Conservative { tier: t, .. } => *t,
        }
    }

    pub fn is_conservative(&self) -> bool {
        matches!(self, ClassifyResult::Conservative { .. })
    }

    pub fn warning(&self) -> Option<&'static str> {
        match self {
            ClassifyResult::Conservative { reason, .. } => Some(reason),
            ClassifyResult::Definite(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_local() {
        let r = classify_bind_addr("127.0.0.1".parse().unwrap(), false);
        assert_eq!(r.tier(), ExposureTier::Local);
    }

    #[test]
    fn tailscale_is_mesh() {
        let r = classify_bind_addr("100.64.1.5".parse().unwrap(), false);
        assert_eq!(r.tier(), ExposureTier::Mesh);
        let r2 = classify_bind_addr("100.127.255.255".parse().unwrap(), false);
        assert_eq!(r2.tier(), ExposureTier::Mesh);
    }

    #[test]
    fn tailscale_boundary() {
        // 100.128.0.0 is outside the CGNAT range
        let r = classify_bind_addr("100.128.0.1".parse().unwrap(), true);
        assert_eq!(r.tier(), ExposureTier::Internet);
    }

    #[test]
    fn rfc1918_ranges_are_lan() {
        for addr in ["10.0.0.1", "172.16.0.1", "172.31.255.255", "192.168.1.1"] {
            let r = classify_bind_addr(addr.parse().unwrap(), false);
            assert_eq!(r.tier(), ExposureTier::Lan, "expected Lan for {addr}");
        }
    }

    #[test]
    fn unspecified_with_public_ip_is_internet() {
        let r = classify_bind_addr("0.0.0.0".parse().unwrap(), true);
        assert_eq!(r.tier(), ExposureTier::Internet);
        assert!(!r.is_conservative());
    }

    #[test]
    fn unspecified_without_public_ip_is_conservative_lan() {
        let r = classify_bind_addr("0.0.0.0".parse().unwrap(), false);
        assert_eq!(r.tier(), ExposureTier::Lan);
        assert!(r.is_conservative());
    }

    #[test]
    fn public_ip_is_internet() {
        let r = classify_bind_addr("8.8.8.8".parse().unwrap(), true);
        assert_eq!(r.tier(), ExposureTier::Internet);
    }

    #[test]
    fn exposure_tier_ordering() {
        assert!(ExposureTier::Local < ExposureTier::Lan);
        assert!(ExposureTier::Lan < ExposureTier::Mesh);
        assert!(ExposureTier::Mesh < ExposureTier::Internet);
    }
}
