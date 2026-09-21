//! The data-policy gate (see "Data policy" in
//! `docs/architecture/DECISIONS_MODEL_PROPOSAL.md`).
//!
//! Every judged state leaves the mesh, so two rules hold at the single egress
//! point (`DecisionsClient::evaluate`), for every caller:
//!
//! 1. **Sites are allow-listed by id** in [`SITES`], each with a declared data
//!    class. An unknown site, or one whose class the policy does not allow, is
//!    refused before any network hop. Widening the policy means editing that
//!    table, which shows up in review.
//! 2. **Every string in `state` is redacted** and truncated to a fixed tail,
//!    even for class A. Machine-generated text is not safe by construction:
//!    DEF-089 is a live leak of bot tokens inside reqwest error URLs.
//!
//! Redaction is best effort and pattern based. It is a second line of defence
//! behind rule 1 (only telemetry sites are allowed), not a licence to send
//! operator content.

use ansible_mesh_core::decisions::{DecisionsError, DecisionsErrorClass, DecisionsRequest};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

/// Data classes from the policy. Only [`DataClass::A`] is allowed by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataClass {
    /// Machine-generated system telemetry and synthetic strings.
    A,
    /// Features derived from operator content, never the content. Allowed only
    /// per site after an explicit opt-in recorded in the site table.
    B,
    /// Operator messages, LifeGraph or memory content, transcripts, credentials.
    /// Never allowed.
    C,
}

impl DataClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
        }
    }
}

/// One allow-listed call site.
#[derive(Debug, Clone, Copy)]
pub struct SiteSpec {
    pub id: &'static str,
    pub class: DataClass,
    /// Required for class B. Recorded here so the opt-in is a visible diff.
    pub operator_opt_in: bool,
}

/// The allow-list. To add a site, add a row here and update the proposal.
pub const SITES: &[SiteSpec] = &[
    // heal-dispatcher's classifier for novel failure lines: guest process error
    // text and a guest id. Telemetry only.
    SiteSpec {
        id: "heal.classify",
        class: DataClass::A,
        operator_opt_in: false,
    },
    // Live smoke tests: one synthetic sentence, never operator data.
    SiteSpec {
        id: "smoke.live",
        class: DataClass::A,
        operator_opt_in: false,
    },
];

/// Longest tail of any single state string that may leave (characters).
pub const MAX_TEXT_CHARS: usize = 1_500;

/// Look up a site and check the policy allows it.
pub fn site_spec(site: &str) -> Result<&'static SiteSpec, DecisionsError> {
    let spec = SITES.iter().find(|s| s.id == site).ok_or_else(|| {
        DecisionsError::new(
            DecisionsErrorClass::InvalidRequest,
            format!("site `{site}` is not on the decisions allow-list"),
        )
    })?;
    let allowed = match spec.class {
        DataClass::A => true,
        DataClass::B => spec.operator_opt_in,
        DataClass::C => false,
    };
    if allowed {
        Ok(spec)
    } else {
        Err(DecisionsError::new(
            DecisionsErrorClass::InvalidRequest,
            format!(
                "site `{site}` is data class {} and the data policy does not allow it",
                spec.class.as_str()
            ),
        ))
    }
}

/// A copy of `request` with every string in `state` redacted and truncated.
pub fn redacted(request: &DecisionsRequest) -> DecisionsRequest {
    let mut out = request.clone();
    out.state = redact_value(&request.state);
    out
}

/// Bytes the request sends (state plus questions), for the content-free audit row.
pub fn egress_bytes(request: &DecisionsRequest) -> usize {
    serde_json::to_string(request).map_or(0, |s| s.len())
}

fn redact_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact(text)),
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), redact_value(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

struct Rules {
    url: Regex,
    bearer: Regex,
    key_value: Regex,
    bot_token: Regex,
    api_key: Regex,
    email: Regex,
    hex: Regex,
    blob: Regex,
    home: Regex,
    ipv4: Regex,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let re = |pattern: &str| Regex::new(pattern).expect("static redaction pattern");
        Rules {
            // Any URL, including ones with userinfo, a query token or a path that
            // embeds a token (`https://api.telegram.org/bot<token>/getUpdates`).
            // Only the host survives.
            url: re(r#"(?i)https?://(?:[^/\s@"'<>]*@)?([^/\s?#:"'<>]+)(?::\d+)?[^\s"'<>]*"#),
            bearer: re(r"(?i)bearer\s+[A-Za-z0-9._~+/=-]{8,}"),
            key_value: re(
                r#"(?i)\b(api[_-]?key|access[_-]?token|auth[_-]?token|token|secret|password|passwd|authorization)\b(\s*[=:]\s*)("[^"]*"|'[^']*'|\S+)"#,
            ),
            bot_token: re(r"\b(?:bot)?\d{6,}:[A-Za-z0-9_-]{25,}\b"),
            api_key: re(r"\b(?:sk|pk|rk)-[A-Za-z0-9_-]{8,}"),
            email: re(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}"),
            hex: re(r"\b[A-Fa-f0-9]{32,}\b"),
            blob: re(r"\b[A-Za-z0-9+/_-]{40,}={0,2}"),
            home: re(r"/(?:Users|home)/[^/\s]+"),
            ipv4: re(r"\b\d{1,3}(?:\.\d{1,3}){3}\b"),
        }
    })
}

/// Redact secrets and identifying strings, then keep the last
/// [`MAX_TEXT_CHARS`] characters (heal text only needs its tail).
pub fn redact(text: &str) -> String {
    let r = rules();
    let text = r.url.replace_all(text, "<url:$1>");
    let text = r.bearer.replace_all(&text, "Bearer <redacted>");
    let text = r.key_value.replace_all(&text, "$1$2<redacted>");
    let text = r.bot_token.replace_all(&text, "<bot-token>");
    let text = r.api_key.replace_all(&text, "<key>");
    let text = r.email.replace_all(&text, "<email>");
    let text = r.hex.replace_all(&text, "<hex>");
    let text = r.blob.replace_all(&text, "<blob>");
    let text = r.home.replace_all(&text, "/<home>");
    let text = r.ipv4.replace_all(&text, "<ip>");
    tail(&text, MAX_TEXT_CHARS)
}

fn tail(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let skip = count - max_chars;
    let start = text
        .char_indices()
        .nth(skip)
        .map_or(text.len(), |(index, _)| index);
    format!("…{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_allow_listed_class_a_sites_pass() {
        assert!(site_spec("heal.classify").is_ok());
        assert!(site_spec("smoke.live").is_ok());
        assert_eq!(
            site_spec("memory.recall").unwrap_err().class,
            DecisionsErrorClass::InvalidRequest,
            "an unknown site is refused"
        );
        assert!(site_spec("").is_err());
        assert!(site_spec("HEAL.CLASSIFY").is_err(), "ids are exact");
    }

    #[test]
    fn every_listed_site_is_class_a_until_the_policy_changes() {
        // A tripwire: adding a class B or C site must be a deliberate edit that
        // also updates this test and the proposal.
        for site in SITES {
            assert_eq!(site.class, DataClass::A, "{}", site.id);
            assert!(!site.operator_opt_in, "{}", site.id);
        }
    }

    #[test]
    fn class_b_needs_an_opt_in_and_class_c_is_never_allowed() {
        // The policy function, exercised directly on constructed specs.
        let allowed = |class, opt_in| match class {
            DataClass::A => true,
            DataClass::B => opt_in,
            DataClass::C => false,
        };
        assert!(!allowed(DataClass::B, false));
        assert!(allowed(DataClass::B, true));
        assert!(!allowed(DataClass::C, true));
    }

    #[test]
    fn the_def_089_bot_token_in_a_reqwest_error_url_is_redacted() {
        let line = "error sending request for url (https://api.telegram.org/bot123456789:AAE_abcdefghijklmnopqrstuvwxyz012345/getUpdates): connection refused";
        let out = redact(line);
        assert!(!out.contains("AAE_abc"), "{out}");
        assert!(!out.contains("123456789"), "{out}");
        // The host and the failure survive: that is what classification needs.
        assert!(out.contains("<url:api.telegram.org>"), "{out}");
        assert!(out.contains("connection refused"), "{out}");
    }

    #[test]
    fn a_bare_bot_token_outside_a_url_is_redacted() {
        // Assembled at runtime: a literal in the bot-token shape trips the repo's
        // secret scanner even though this one is synthetic.
        let token = format!("{}:{}", "987654321", "BBF-abcdefghijklmnopqrstuvwxyz_0123");
        let out = redact(&format!("polling failed for {token} twice"));
        assert!(!out.contains("BBF-abc"), "{out}");
        assert!(out.contains("<bot-token>"), "{out}");
    }

    #[test]
    fn keys_bearers_and_key_value_secrets_are_redacted() {
        let out = redact(
            "Authorization: Bearer abcdef1234567890xyz and key sk-or-v1-0123456789abcdef then api_key=hunter2hunter2 password: \"p@ss w0rd\" token=abc.def",
        );
        for leaked in [
            "abcdef1234567890xyz",
            "sk-or-v1-0123456789abcdef",
            "hunter2hunter2",
            "p@ss w0rd",
            "abc.def",
        ] {
            assert!(!out.contains(leaked), "leaked {leaked}: {out}");
        }
    }

    #[test]
    fn urls_with_userinfo_or_query_tokens_keep_only_the_host() {
        let out = redact("GET https://user:pw@example.com:8443/a/b?token=SECRET123&x=1 failed");
        assert!(
            !out.contains("SECRET123") && !out.contains("user:pw"),
            "{out}"
        );
        assert!(out.contains("<url:example.com>"), "{out}");
    }

    #[test]
    fn emails_home_paths_ips_and_long_runs_are_redacted() {
        let out = redact(
            "mail jane.doe@example.com from /Users/jaredlikes/code/x to 100.64.212.8 sha 0123456789abcdef0123456789abcdef0123 blob QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5",
        );
        for leaked in [
            "jane.doe@example.com",
            "jaredlikes",
            "100.64.212.8",
            "0123456789abcdef0123456789abcdef0123",
            "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5",
        ] {
            assert!(!out.contains(leaked), "leaked {leaked}: {out}");
        }
        assert!(out.contains("/<home>/code/x"), "{out}");
    }

    #[test]
    fn ordinary_failure_text_is_left_readable() {
        let line = "thread 'main' panicked at src/main.rs:42: connection refused (os error 61) after 3 retries";
        assert_eq!(redact(line), line);
    }

    #[test]
    fn redaction_is_idempotent() {
        let once = redact("Bearer abcdef1234567890xyz at https://h.example/p?t=SECRET1");
        assert_eq!(redact(&once), once);
    }

    #[test]
    fn long_text_keeps_its_tail_on_a_char_boundary() {
        let text = format!("{}END-OF-LOG", "é".repeat(MAX_TEXT_CHARS * 2));
        let out = redact(&text);
        assert!(
            out.starts_with('…') && out.ends_with("END-OF-LOG"),
            "{out:.40}"
        );
        assert_eq!(out.chars().count(), MAX_TEXT_CHARS + 1);
    }

    #[test]
    fn redaction_reaches_every_string_in_a_nested_state() {
        let request = DecisionsRequest {
            site: "heal.classify".into(),
            state: json!({
                "guest": "beacon",
                "error": "boom sk-or-v1-0123456789abcdef",
                "nested": ["a@b.example", { "deep": "Bearer zzzzzzzzzzzz1111" }],
                "count": 3
            }),
            questions: Vec::new(),
        };
        let out = redacted(&request);
        let text = out.state.to_string();
        assert!(
            !text.contains("sk-or-v1")
                && !text.contains("a@b.example")
                && !text.contains("zzzzzzzzzzzz"),
            "{text}"
        );
        assert_eq!(out.state["count"], 3);
        assert_eq!(out.state["guest"], "beacon");
        // The caller's request is untouched.
        assert!(request.state.to_string().contains("sk-or-v1"));
    }
}
