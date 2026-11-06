//! Content-free diagnostic labels. These observations never authorize routing.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn fingerprint(domain: &str, value: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    // Bound work even for an untrusted diagnostic identifier. Include length so
    // distinct oversized inputs do not collapse merely because prefixes match.
    hash.update(value.len().to_le_bytes());
    hash.update(&value.as_bytes()[..value.len().min(1024)]);
    format!("{:x}", hash.finalize())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRoute {
    #[serde(default)]
    pub target_role: String,
    pub phase: String,
    pub reason: String,
    pub tier: Option<u8>,
}

impl AttemptRoute {
    pub fn projected(&self) -> Self {
        Self {
            target_role: match self.target_role.as_str() {
                "model" => "model",
                "model.openrouter" => "model.openrouter",
                "model.ollama" => "model.ollama",
                "model.openai" => "model.openai",
                "model.anthropic" => "model.anthropic",
                _ => "unknown",
            }
            .into(),
            phase: match self.phase.as_str() {
                "initial" => "initial",
                "fallback" => "fallback",
                "probe" => "probe",
                "retry" => "retry",
                _ => "unknown",
            }
            .into(),
            reason: match self.reason.as_str() {
                "operator_pin" => "operator_pin",
                "sticky_fallback" => "sticky_fallback",
                "explicit_or_remote" => "explicit_or_remote",
                "role_ladder" => "role_ladder",
                "legacy_or_default" => "legacy_or_default",
                "fallback_ladder" => "fallback_ladder",
                "origin_probe" => "origin_probe",
                "same_provider_retry" => "same_provider_retry",
                _ => "unknown",
            }
            .into(),
            tier: self.tier.filter(|tier| *tier < 16),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_diagnostic_labels_and_identifiers_never_echo() {
        let secret = "Bearer private-prompt\ncredential";
        let route = AttemptRoute {
            phase: secret.into(),
            reason: secret.into(),
            tier: Some(255),
            ..Default::default()
        };
        let json = serde_json::to_string(&route.projected()).unwrap();
        assert!(!json.contains(secret));
        assert_eq!(route.projected().tier, None);
        assert_eq!(fingerprint("turn", secret).len(), 64);
        assert_ne!(fingerprint("turn", secret), fingerprint("model", secret));
    }
}
