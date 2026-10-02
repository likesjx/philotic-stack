//! The data-policy gate (see "Data policy" in
//! `docs/architecture/DECISIONS_MODEL_PROPOSAL.md`).
//!
//! Every judged state leaves the mesh, so these rules hold at the single egress
//! point ([`admit`], called by `DecisionsClient::evaluate`) for every caller:
//!
//! 1. **Sites are allow-listed by id** in [`SITES`], each with a declared data
//!    class. An unknown site, or one whose class the policy does not allow on
//!    this transport, is refused before any network hop. Widening the policy
//!    means editing that table, which shows up in review.
//! 2. **Operator content (class C) leaves only under zero data retention.** The
//!    operator allowed full prompts on 2026-09-30, on condition that they go only
//!    where nothing is kept: a class-C site needs an opt-in row AND a transport
//!    that pins ZDR on every request (OpenRouter; see
//!    `DecisionsTransport::enforces_zero_retention`). The native TypeSafe API
//!    offers ZDR to enterprise accounts only, so class C is refused there.
//! 3. **Class A and B state that looks like a conversation payload is refused**
//!    ([`screen`]). The allow-list keys on the site id, not on what the state
//!    contains, and a provider error body can echo the request that failed. This
//!    is a fail-closed heuristic, not a guarantee.
//! 4. **Every string in `state` is redacted.** Class A and B get full redaction
//!    and a fixed tail ([`redact`]): machine-generated text is not safe by
//!    construction (DEF-089 leaks bot tokens inside reqwest error URLs). Class C
//!    keeps its full text for judgment but still loses credentials
//!    ([`redact_secrets`]); its size is bounded by the request's token budget.
//!
//! A refusal is `DecisionsErrorClass::PolicyRefused`: nothing left the machine.
//!
//! Redaction is best effort and pattern based, a second line of defence behind
//! rules 1 and 2.

use ansible_mesh_core::decisions::{
    DecisionsError, DecisionsErrorClass, DecisionsRequest, DecisionsTransport,
};
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
    /// Operator messages, LifeGraph or memory content, transcripts. Allowed only
    /// per site after an opt-in recorded in the site table, and only on a
    /// transport that pins zero data retention. Credentials are redacted even
    /// here.
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
    /// Required for class B and C. Recorded here so the opt-in is a visible diff.
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
    // philote's distill pre-screen, shadow only: the operator's message, a
    // tool summary and the reply, asked "is there a reusable lesson here?".
    // Operator opt-in for class C under zero data retention: 2026-09-30.
    SiteSpec {
        id: "distill.prescreen",
        class: DataClass::C,
        operator_opt_in: true,
    },
];

/// Longest tail of any single state string that may leave (characters).
pub const MAX_TEXT_CHARS: usize = 1_500;

/// Look up a site and check the policy allows it on a transport that keeps
/// data (the conservative answer: class C is refused). [`admit`] is the
/// transport-aware check.
pub fn site_spec(site: &str) -> Result<&'static SiteSpec, DecisionsError> {
    let spec = lookup(site)?;
    if class_allowed(spec.class, spec.operator_opt_in, false) {
        Ok(spec)
    } else {
        Err(DecisionsError::new(
            DecisionsErrorClass::PolicyRefused,
            format!(
                "site `{site}` is data class {} and the data policy does not allow it",
                spec.class.as_str()
            ),
        ))
    }
}

fn lookup(site: &str) -> Result<&'static SiteSpec, DecisionsError> {
    SITES.iter().find(|s| s.id == site).ok_or_else(|| {
        DecisionsError::new(
            DecisionsErrorClass::PolicyRefused,
            format!("site `{site}` is not on the decisions allow-list"),
        )
    })
}

/// The policy: class A always; class B with an opt-in; class C with an opt-in
/// and only where every request is pinned to zero data retention.
pub fn class_allowed(class: DataClass, operator_opt_in: bool, zero_retention: bool) -> bool {
    match class {
        DataClass::A => true,
        DataClass::B => operator_opt_in,
        DataClass::C => operator_opt_in && zero_retention,
    }
}

/// The single egress check. Returns the state that may leave (screened and
/// redacted for its class), or `PolicyRefused` with nothing sent.
pub fn admit(
    request: &DecisionsRequest,
    transport: DecisionsTransport,
) -> Result<DecisionsRequest, DecisionsError> {
    admit_as(lookup(&request.site)?, request, transport)
}

fn admit_as(
    spec: &SiteSpec,
    request: &DecisionsRequest,
    transport: DecisionsTransport,
) -> Result<DecisionsRequest, DecisionsError> {
    if !class_allowed(
        spec.class,
        spec.operator_opt_in,
        transport.enforces_zero_retention(),
    ) {
        return Err(DecisionsError::new(
            DecisionsErrorClass::PolicyRefused,
            format!(
                "site `{}` is data class {} and the data policy does not allow it on the {} transport",
                spec.id,
                spec.class.as_str(),
                transport.as_str()
            ),
        ));
    }
    match spec.class {
        DataClass::A | DataClass::B => {
            screen(request)?;
            Ok(redacted(request))
        }
        DataClass::C => {
            let mut out = request.clone();
            out.state = map_strings(&request.state, &redact_secrets);
            Ok(out)
        }
    }
}

/// Substrings that mark text as a conversation or request payload rather than
/// machine telemetry. Provider error bodies can echo part of the request that
/// failed, and `heal-dispatcher` sees those (only `model-router`'s
/// `emit_failure` pushes untriaged heal entries, with the provider's error text).
/// Redaction cannot recognise operator prose, so text carrying these markers is
/// refused outright: fail closed, the incumbent decision stands.
const PAYLOAD_ECHO_MARKERS: &[&str] = &[
    "\"role\"",
    "\"messages\"",
    "\"content\"",
    "\"parts\"",
    "\"prompt\"",
    "\"system_instruction\"",
];

/// Refuse a state that looks like a conversation payload (data class C risk).
/// This is a heuristic: it lowers the chance of operator content leaving through
/// a class-A site, it does not remove it. See the residual-risk note in the
/// proposal's data policy.
pub fn screen(request: &DecisionsRequest) -> Result<(), DecisionsError> {
    match first_payload_marker(&request.state) {
        None => Ok(()),
        Some(marker) => Err(DecisionsError::new(
            DecisionsErrorClass::PolicyRefused,
            format!(
                "state for site `{}` resembles a conversation payload (`{marker}`) and was not sent",
                request.site
            ),
        )),
    }
}

fn first_payload_marker(value: &Value) -> Option<&'static str> {
    match value {
        Value::String(text) => {
            // A payload quoted inside a log line arrives JSON-escaped (`\"role\"`),
            // so compare with the escapes removed.
            let flat = text.replace('\\', "").to_ascii_lowercase();
            PAYLOAD_ECHO_MARKERS
                .iter()
                .find(|marker| flat.contains(*marker))
                .copied()
        }
        Value::Array(items) => items.iter().find_map(first_payload_marker),
        Value::Object(map) => map.values().find_map(first_payload_marker),
        _ => None,
    }
}

/// A copy of `request` with every string in `state` redacted and truncated.
pub fn redacted(request: &DecisionsRequest) -> DecisionsRequest {
    let mut out = request.clone();
    out.state = map_strings(&request.state, &redact);
    out
}

/// Apply `f` to every string in a JSON value, keeping its shape.
fn map_strings(value: &Value, f: &dyn Fn(&str) -> String) -> Value {
    match value {
        Value::String(text) => Value::String(f(text)),
        Value::Array(items) => Value::Array(items.iter().map(|v| map_strings(v, f)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), map_strings(v, f)))
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

/// Credentials only, for class C: bearer tokens, `key=value` secrets, bot tokens
/// and `sk-`-style keys. The text is otherwise left whole (no URL, address or
/// path rewriting, no truncation) because the operator's words are what is
/// being judged.
pub fn redact_secrets(text: &str) -> String {
    let r = rules();
    let text = r.bearer.replace_all(text, "Bearer <redacted>");
    let text = r.key_value.replace_all(&text, "$1$2<redacted>");
    let text = r.bot_token.replace_all(&text, "<bot-token>");
    r.api_key.replace_all(&text, "<key>").into_owned()
}

/// The last `max_chars` characters of `text`, prefixed with `…` if it was cut.
/// Char-boundary safe.
pub fn tail(text: &str, max_chars: usize) -> String {
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
            DecisionsErrorClass::PolicyRefused,
            "an unknown site is refused"
        );
        assert!(site_spec("").is_err());
        assert!(site_spec("HEAL.CLASSIFY").is_err(), "ids are exact");
    }

    #[test]
    fn the_site_table_is_exactly_the_reviewed_one() {
        // A tripwire: adding or reclassifying a site must be a deliberate edit
        // that also updates this test and the proposal's data policy.
        let table: Vec<(&str, DataClass, bool)> = SITES
            .iter()
            .map(|s| (s.id, s.class, s.operator_opt_in))
            .collect();
        assert_eq!(
            table,
            [
                ("heal.classify", DataClass::A, false),
                ("smoke.live", DataClass::A, false),
                // Operator opt-in 2026-09-30, zero data retention only.
                ("distill.prescreen", DataClass::C, true),
            ]
        );
    }

    #[test]
    fn distill_prescreen_leaves_only_on_the_zero_retention_transport() {
        let mut request = heal_state("x");
        request.site = "distill.prescreen".into();
        request.state = json!({ "user_message": "no, that's wrong — use the staging db" });
        assert_eq!(
            admit(&request, DecisionsTransport::Native)
                .unwrap_err()
                .class,
            DecisionsErrorClass::PolicyRefused
        );
        let out = admit(&request, DecisionsTransport::OpenRouter).unwrap();
        assert_eq!(
            out.state["user_message"],
            "no, that's wrong — use the staging db"
        );
        // The class-A-only lookup used for trace labels still refuses it.
        assert!(site_spec("distill.prescreen").is_err());
    }

    #[test]
    fn class_b_needs_an_opt_in_and_class_c_needs_an_opt_in_and_zero_retention() {
        assert!(class_allowed(DataClass::A, false, false));
        assert!(!class_allowed(DataClass::B, false, true));
        assert!(class_allowed(DataClass::B, true, false));
        assert!(!class_allowed(DataClass::C, false, true), "no opt-in");
        assert!(
            !class_allowed(DataClass::C, true, false),
            "retaining transport"
        );
        assert!(class_allowed(DataClass::C, true, true));
    }

    const OPERATOR_SITE: SiteSpec = SiteSpec {
        id: "test.operator",
        class: DataClass::C,
        operator_opt_in: true,
    };

    fn operator_request(state: Value) -> DecisionsRequest {
        let mut request = heal_state("x");
        request.site = OPERATOR_SITE.id.into();
        request.state = state;
        request
    }

    #[test]
    fn class_c_is_refused_on_the_native_transport() {
        let request = operator_request(json!("remind me to call Mara about the lease"));
        let err = admit_as(&OPERATOR_SITE, &request, DecisionsTransport::Native).unwrap_err();
        assert_eq!(err.class, DecisionsErrorClass::PolicyRefused);
    }

    #[test]
    fn class_c_leaves_whole_under_zero_retention_minus_credentials() {
        // A conversation payload and prose longer than the class-A tail: both are
        // the point of a class-C site, so neither the screen nor the tail applies.
        let long = format!(
            "{} and my api_key=sk-live-abcdef123456 ok",
            "word ".repeat(600)
        );
        let request = operator_request(json!({
            "messages": [{ "role": "user", "content": long }],
            "note": "email me at jo@example.com about https://example.com/page"
        }));
        let out = admit_as(&OPERATOR_SITE, &request, DecisionsTransport::OpenRouter).unwrap();
        let text = out.state["messages"][0]["content"].as_str().unwrap();
        assert!(text.starts_with("word word"), "not truncated to a tail");
        assert!(!text.contains("sk-live"), "credentials still redacted");
        assert_eq!(
            out.state["note"], "email me at jo@example.com about https://example.com/page",
            "operator prose is not rewritten"
        );
    }

    #[test]
    fn class_a_keeps_the_screen_and_full_redaction_on_every_transport() {
        let request = heal_state("{\"role\":\"user\",\"content\":\"hi\"}");
        for transport in [DecisionsTransport::OpenRouter, DecisionsTransport::Native] {
            assert_eq!(
                admit(&request, transport).unwrap_err().class,
                DecisionsErrorClass::PolicyRefused
            );
        }
        let ok = heal_state(
            "connect to https://api.telegram.org/bot123456789:AAE_abcdefghijklmnopqrstuvwxyz012345/x failed",
        );
        let out = admit(&ok, DecisionsTransport::OpenRouter).unwrap();
        assert!(!out.state.to_string().contains("AAE_"));
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

    fn heal_state(error: &str) -> DecisionsRequest {
        DecisionsRequest {
            site: "heal.classify".into(),
            state: json!({ "guest": "beacon", "error": error }),
            questions: Vec::new(),
        }
    }

    #[test]
    fn the_screen_refuses_conversation_payloads_even_when_json_escaped() {
        for echoed in [
            r#"400 {"messages":[{"role":"user","content":"hello"}]}"#,
            r#"openai: 400 {\"messages\":[{\"role\":\"user\"}]}"#,
            r#"gemini: invalid payload {"contents":[{"parts":[{"text":"x"}]}]}"#,
            r#"bad request {"Prompt": "summarise my notes"}"#,
            r#"anthropic {"system_instruction":"be terse"}"#,
        ] {
            let err = screen(&heal_state(echoed)).expect_err(echoed);
            assert_eq!(err.class, DecisionsErrorClass::PolicyRefused, "{echoed}");
            // The refusal names the marker, never the content.
            assert!(!err.message.contains("hello") && !err.message.contains("summarise"));
        }
    }

    #[test]
    fn the_screen_lets_ordinary_telemetry_through() {
        for line in [
            "connection refused (os error 61) after 3 retries",
            "[beacon][text.generate] openai: HTTP 429 rate limit exceeded",
            "thread 'main' panicked at src/main.rs:42: index out of bounds",
            "Provider invocation failed: request timed out after 8s",
        ] {
            assert!(screen(&heal_state(line)).is_ok(), "{line}");
        }
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
