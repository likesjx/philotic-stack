//! Telegram webhook secret-token contract.
//!
//! # Why this exists (audit flag)
//!
//! The Telegram gateway runs in **long-polling** mode today, so no webhook
//! endpoint is exposed. But the moment a webhook listener *is* stood up, the
//! only thing standing between the public internet and the guest's inbound
//! turn pipeline is Telegram's [secret-token contract]: `setWebhook` is called
//! with a `secret_token`, and Telegram then stamps every delivery with the
//! `X-Telegram-Bot-Api-Secret-Token` header. A listener that does not verify
//! that header will accept forged updates from anyone who learns the URL.
//!
//! This module is the **contract, not the server**. It exists ahead of any
//! webhook listener so the contract can never be an afterthought:
//!
//! - [`generate_webhook_secret`] mints a crypto-random secret (stored as a
//!   vault ref, exactly like the bot token).
//! - [`validate_webhook_secret`] compares the presented header against the
//!   configured secret in **constant time** (BLAKE3, the same primitive
//!   `membrane-mcp`'s `auth.rs` uses).
//! - [`resolve_transport`] is a **hard refusal**: webhook mode without a
//!   configured secret cannot produce a [`WebhookRuntime`], and only a
//!   `WebhookRuntime` unlocks [`authorize_webhook_request`]. Any future
//!   listener is therefore structurally forced through the secret check.
//!
//! [secret-token contract]: https://core.telegram.org/bots/api#setwebhook

use anyhow::{Result, bail};
use rand::RngCore;
use rand::rngs::OsRng;

/// The header Telegram stamps on every webhook delivery when `setWebhook` was
/// called with a `secret_token`. Case-insensitive on the wire per HTTP, but
/// this is the canonical spelling.
pub const WEBHOOK_SECRET_HEADER: &str = "X-Telegram-Bot-Api-Secret-Token";

/// Number of random bytes behind a generated secret (256 bits of entropy).
const SECRET_BYTES: usize = 32;

/// Telegram permits a `secret_token` of 1–256 characters. We refuse anything
/// shorter than this so a misconfigured single character can't slip through.
pub const MIN_SECRET_LEN: usize = 16;

/// Telegram's hard upper bound on `secret_token` length.
pub const MAX_SECRET_LEN: usize = 256;

/// Telegram restricts the `secret_token` charset to `A-Z`, `a-z`, `0-9`,
/// `_` and `-`. A generated hex secret is a strict subset of this.
pub fn is_valid_secret_shape(secret: &str) -> bool {
    let len = secret.len();
    if len < MIN_SECRET_LEN || len > MAX_SECRET_LEN {
        return false;
    }
    secret
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Mint a fresh crypto-random webhook secret.
///
/// Returns 64 lowercase hex characters (256 bits from the OS CSPRNG), which is
/// well within Telegram's 1–256 length bound and its allowed charset. The
/// caller stores the returned string as a vault ref — the same lifecycle as the
/// bot token — and hands the raw value to `setWebhook` when standing up a
/// listener.
pub fn generate_webhook_secret() -> String {
    let mut bytes = [0u8; SECRET_BYTES];
    // OsRng (not `thread_rng().gen()`): `gen` is a reserved keyword under
    // edition 2024, and OsRng is the CSPRNG used elsewhere in the tree.
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Constant-time comparison of the presented header value against the expected
/// secret.
///
/// Returns `false` for a missing header or an empty expected secret (a missing
/// secret must never validate). Both operands are hashed with BLAKE3 and the
/// fixed-width 32-byte `blake3::Hash` values are compared — `blake3::Hash`'s
/// `PartialEq` is constant-time, the exact primitive `membrane-mcp`'s
/// `auth.rs` relies on. Hashing both sides (rather than storing a hash at rest)
/// is correct here because the secret is plaintext-retrievable, and it yields a
/// length-independent, timing-safe compare.
pub fn validate_webhook_secret(expected: &str, presented: Option<&str>) -> bool {
    if expected.is_empty() {
        return false;
    }
    let Some(presented) = presented else {
        return false;
    };
    blake3::hash(expected.as_bytes()) == blake3::hash(presented.as_bytes())
}

/// Derive the webhook-secret config key from the bot-token config key:
/// `telegram_bot_token_{agent_key}` → `telegram_webhook_secret_{agent_key}`,
/// with the un-suffixed global fallback for single-seat mode. Mirrors
/// [`crate::telegram_allowed_users_key`] so the secret rides the same
/// per-seat config/vault convention as the bot token itself.
pub fn telegram_webhook_secret_key(telegram_token_key: &str) -> String {
    if let Some(suffix) = telegram_token_key.strip_prefix("telegram_bot_token_") {
        format!("telegram_webhook_secret_{suffix}")
    } else {
        "telegram_webhook_secret".to_string()
    }
}

/// How a seat receives updates from Telegram.
///
/// Polling is the only transport wired today; `Webhook` is the future mode this
/// contract guards. The `secret_ref` is a vault ref (or `None` when unset),
/// deliberately *not* the raw secret — resolution happens in
/// [`resolve_transport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramTransport {
    /// Long-polling `getUpdates` — the active transport. No secret required.
    Polling,
    /// Webhook delivery. `secret_ref` points at the vault entry holding the
    /// `secret_token`; `None` means no secret is configured, which
    /// [`resolve_transport`] refuses to start.
    Webhook { secret_ref: Option<String> },
}

/// A validated, ready-to-serve webhook transport.
///
/// This type is the gate: it is only constructible via [`resolve_transport`],
/// which refuses to build one without a configured, well-formed secret. A
/// future listener holds a `WebhookRuntime` and calls
/// [`authorize_webhook_request`] on every delivery — there is no path to
/// process a webhook body without first passing the secret check.
#[derive(Debug, Clone)]
pub struct WebhookRuntime {
    secret: String,
}

impl WebhookRuntime {
    /// The raw secret to hand to `setWebhook` when registering the webhook.
    pub fn secret(&self) -> &str {
        &self.secret
    }
}

/// Resolve a [`TelegramTransport`] into a runtime, enforcing the secret contract.
///
/// - `Polling` → `Ok(None)`: no webhook, nothing to guard.
/// - `Webhook { secret_ref: None }` → **hard error**: refuse to start webhook
///   mode without a secret.
/// - `Webhook { secret_ref: Some(r) }` → resolve `r` through `resolve_secret`,
///   validate its shape, and return a [`WebhookRuntime`]. An empty or malformed
///   resolved secret is also a hard error.
///
/// `resolve_secret` is injected (rather than reaching into IPC here) so the
/// contract is a pure, testable function; the live wiring passes a closure that
/// fetches the vault ref via the hotel.
pub fn resolve_transport(
    transport: &TelegramTransport,
    resolve_secret: impl FnOnce(&str) -> Result<String>,
) -> Result<Option<WebhookRuntime>> {
    match transport {
        TelegramTransport::Polling => Ok(None),
        TelegramTransport::Webhook { secret_ref: None } => {
            bail!(
                "refusing to start Telegram webhook mode: no webhook secret configured. \
                 Generate one with `generate_webhook_secret`, store it as a vault ref, and \
                 set the `{}`-style config key before enabling webhook delivery.",
                telegram_webhook_secret_key("telegram_bot_token"),
            )
        }
        TelegramTransport::Webhook {
            secret_ref: Some(secret_ref),
        } => {
            let secret = resolve_secret(secret_ref).map_err(|e| {
                anyhow::anyhow!(
                    "refusing to start Telegram webhook mode: webhook secret '{secret_ref}' \
                     could not be resolved: {e}"
                )
            })?;
            if !is_valid_secret_shape(&secret) {
                bail!(
                    "refusing to start Telegram webhook mode: webhook secret '{secret_ref}' \
                     is malformed (must be {MIN_SECRET_LEN}-{MAX_SECRET_LEN} chars of \
                     [A-Za-z0-9_-])"
                );
            }
            Ok(Some(WebhookRuntime { secret }))
        }
    }
}

/// Authorize a single inbound webhook request.
///
/// Any future webhook listener MUST call this before touching the update body.
/// `header` is the value of the [`WEBHOOK_SECRET_HEADER`] on the request (or
/// `None` if absent). Returns `Ok(())` only on a constant-time match.
pub fn authorize_webhook_request(runtime: &WebhookRuntime, header: Option<&str>) -> Result<()> {
    if validate_webhook_secret(&runtime.secret, header) {
        Ok(())
    } else {
        bail!("rejected Telegram webhook request: missing or mismatched secret token")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secret_has_expected_shape_and_entropy() {
        let secret = generate_webhook_secret();
        // 32 bytes → 64 hex chars.
        assert_eq!(secret.len(), 64);
        assert!(is_valid_secret_shape(&secret));
        assert!(secret.bytes().all(|b| b.is_ascii_hexdigit()));

        // Two draws must differ (entropy sanity — collision probability ~2^-256).
        let other = generate_webhook_secret();
        assert_ne!(secret, other);
    }

    #[test]
    fn shape_validation_bounds_and_charset() {
        assert!(is_valid_secret_shape("aZ09_-aZ09_-aZ09")); // 16 chars, all legal
        assert!(!is_valid_secret_shape("tooshort")); // < MIN_SECRET_LEN
        assert!(!is_valid_secret_shape(&"a".repeat(MAX_SECRET_LEN + 1))); // > MAX
        assert!(!is_valid_secret_shape("has spaces here!")); // illegal chars
        assert!(!is_valid_secret_shape("has.dots.and/slash")); // illegal chars
        assert!(is_valid_secret_shape(&"a".repeat(MAX_SECRET_LEN))); // exactly MAX
    }

    #[test]
    fn validate_accepts_matching_header() {
        let secret = generate_webhook_secret();
        assert!(validate_webhook_secret(&secret, Some(&secret)));
    }

    #[test]
    fn validate_rejects_wrong_header() {
        let secret = generate_webhook_secret();
        let wrong = generate_webhook_secret();
        assert!(!validate_webhook_secret(&secret, Some(&wrong)));
    }

    #[test]
    fn validate_rejects_missing_header() {
        let secret = generate_webhook_secret();
        assert!(!validate_webhook_secret(&secret, None));
    }

    #[test]
    fn validate_rejects_empty_expected() {
        // A missing/empty configured secret must never validate, even against
        // an empty presented value.
        assert!(!validate_webhook_secret("", Some("")));
        assert!(!validate_webhook_secret("", None));
    }

    #[test]
    fn validate_rejects_prefix_and_length_mismatch() {
        let secret = "abcdef0123456789abcdef";
        assert!(!validate_webhook_secret(secret, Some("abcdef0123456789abcde"))); // truncated
        assert!(!validate_webhook_secret(secret, Some("abcdef0123456789abcdef0"))); // extended
    }

    #[test]
    fn secret_key_derivation_matches_token_key_convention() {
        assert_eq!(
            telegram_webhook_secret_key("telegram_bot_token_jane"),
            "telegram_webhook_secret_jane"
        );
        assert_eq!(
            telegram_webhook_secret_key("telegram_bot_token"),
            "telegram_webhook_secret"
        );
    }

    #[test]
    fn polling_transport_needs_no_secret() {
        let runtime = resolve_transport(&TelegramTransport::Polling, |_| {
            panic!("polling must not resolve a secret")
        })
        .expect("polling resolves");
        assert!(runtime.is_none());
    }

    #[test]
    fn webhook_without_secret_is_hard_refused() {
        let result = resolve_transport(
            &TelegramTransport::Webhook { secret_ref: None },
            |_| Ok("unused".to_string()),
        );
        let err = result.expect_err("webhook without secret must refuse");
        assert!(err.to_string().contains("no webhook secret configured"));
    }

    #[test]
    fn webhook_with_valid_secret_resolves() {
        let secret = generate_webhook_secret();
        let secret_for_closure = secret.clone();
        let runtime = resolve_transport(
            &TelegramTransport::Webhook {
                secret_ref: Some("vault/telegram_webhook_secret_jane".into()),
            },
            move |r| {
                assert_eq!(r, "vault/telegram_webhook_secret_jane");
                Ok(secret_for_closure.clone())
            },
        )
        .expect("valid secret resolves")
        .expect("webhook yields a runtime");
        assert_eq!(runtime.secret(), secret);
    }

    #[test]
    fn webhook_with_malformed_secret_is_refused() {
        let result = resolve_transport(
            &TelegramTransport::Webhook {
                secret_ref: Some("vault/bad".into()),
            },
            |_| Ok("short".to_string()),
        );
        let err = result.expect_err("malformed secret must refuse");
        assert!(err.to_string().contains("malformed"));
    }

    #[test]
    fn webhook_with_unresolvable_secret_is_refused() {
        let result = resolve_transport(
            &TelegramTransport::Webhook {
                secret_ref: Some("vault/missing".into()),
            },
            |_| bail!("vault lookup failed"),
        );
        let err = result.expect_err("unresolvable secret must refuse");
        assert!(err.to_string().contains("could not be resolved"));
    }

    #[test]
    fn authorize_request_gates_on_the_header() {
        let secret = generate_webhook_secret();
        let secret_for_closure = secret.clone();
        let runtime = resolve_transport(
            &TelegramTransport::Webhook {
                secret_ref: Some("vault/s".into()),
            },
            move |_| Ok(secret_for_closure.clone()),
        )
        .unwrap()
        .unwrap();

        assert!(authorize_webhook_request(&runtime, Some(&secret)).is_ok());
        assert!(authorize_webhook_request(&runtime, Some("forged")).is_err());
        assert!(authorize_webhook_request(&runtime, None).is_err());
    }
}
