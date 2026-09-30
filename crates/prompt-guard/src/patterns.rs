//! The prompt safety floor's pattern tables.
//!
//! Two tiers, kept deliberately small. Everything here is compiled with
//! `case_insensitive` and `multi_line`, so `^` anchors at each line start.
//! Hidden control characters and embedded exec-guard hardline commands are
//! checked in code (`lib.rs`), not here.
//!
//! Rules of thumb for adding a pattern:
//! - Dangerous: the text is an *instruction to the reader* that would change
//!   what a future agent does with the operator's data or authority, or is
//!   unreadable by the operator (hidden chars, opaque blobs). Rewording does
//!   not make it safe.
//! - Caution: the text is *suspicious in this position* but could be a
//!   legitimate quote, note, or test fixture. A human should look before it
//!   lands, which is what the unconditional approval tier is for.

pub struct Rule {
    pub pattern: &'static str,
    pub description: &'static str,
}

/// Rejected outright. See module docs.
pub static DANGEROUS_PATTERNS: [Rule; 9] = [
    Rule {
        pattern: r"\b(ignore|disregard|forget|override|bypass)\b[^.\n]{0,40}\b(all |any |your |the )?(previous|prior|above|earlier|preceding|original|system|standing)\b[^.\n]{0,30}\b(instructions?|prompts?|rules?|directives?|orders|guidelines|constraints)\b",
        description: "instruction override (ignore previous instructions)",
    },
    Rule {
        pattern: r"\b(new|updated|revised) system prompt\b|\bsystem prompt (is|becomes|now reads)\b",
        description: "instruction override (system prompt replacement)",
    },
    Rule {
        pattern: r"\b(you are now|enter|enable|activate|switch to|act as)\b[^.\n]{0,20}\b(developer|dan|jailbreak|jailbroken|unrestricted|unfiltered|god|sudo|root|admin) mode\b",
        description: "instruction override (mode escalation)",
    },
    Rule {
        pattern: r"https?://[^\s\]\)]*[?&](api_?key|apikey|token|access_token|secret|password|passwd|pwd|bearer|auth|credential)=",
        description: "exfiltration (credential in URL)",
    },
    Rule {
        pattern: r"\b(send|post|upload|exfiltrate|forward|transmit|leak|copy|ship|push)\b[^\n]{0,60}?(\bapi[ _-]?keys?\b|\btokens?\b|\bsecrets?\b|\bpasswords?\b|\bcredentials?\b|\.env\b|\bssh[ _-]?keys?\b|\bprivate keys?\b|\bkeychain\b|\bvault contents?\b|\bid_rsa\b|\bid_ed25519\b)[^\n]{0,60}?\b(to|at|into)\b[^\n]{0,25}?(https?://|\b[a-z0-9-]+(\.[a-z0-9-]+)*\.(com|net|io|org|dev|xyz|ru|cn|sh|app)\b)",
        description: "exfiltration (send credentials to a remote endpoint)",
    },
    Rule {
        pattern: r"\b(curl|wget|nc|ncat|python[0-9.]* -c|bash -c)\b[^\n]{0,80}\$\{?[A-Z_]*(KEY|TOKEN|SECRET|PASSWORD|PASSWD)[A-Z_]*\}?",
        description: "exfiltration (secret variable piped to a network tool)",
    },
    Rule {
        pattern: r"\b(cat|echo|printf|base64)\b[^\n]{0,40}(~/\.ssh/|/\.env\b|id_rsa|id_ed25519|\.aws/credentials|\.netrc|keychain)[^\n]{0,80}\|\s*(curl|wget|nc|ncat)\b",
        description: "exfiltration (secret file piped to a network tool)",
    },
    Rule {
        pattern: r"[A-Za-z0-9+/]{200,}={0,2}",
        description: "opaque base64 blob",
    },
    Rule {
        pattern: r"\b(then|and|also|finally)?\s*(delete|wipe|clear|erase|remove)\b[^.\n]{0,20}\b(all |the |your |every )?(logs?|audit (trail|records?)|history|evidence|traces?)\b[^.\n]{0,30}\b(so|to|before)\b[^.\n]{0,30}\b(nobody|no one|the operator|the user|they)\b[^.\n]{0,20}\b(sees?|notices?|finds?|knows?|can tell)\b",
        description: "anti-forensics instruction (erase evidence so nobody sees)",
    },
];

/// Proceeds only through the unconditional human-approval tier.
pub static CAUTION_PATTERNS: [Rule; 7] = [
    Rule {
        pattern: r#"</?tool_call>|</?function_call>|</?tool_calls>|"tool_calls"\s*:|\[TOOL_CALL\]|<\|im_start\|>|<\|im_end\|>|<\|system\|>|<\|user\|>|<\|assistant\|>|\[INST\]|\[/INST\]|<<SYS>>|<</SYS>>"#,
        description: "tool-call or chat-template smuggling marker",
    },
    Rule {
        pattern: r"^\s*(system|assistant|developer|tool)\s*:\s",
        description: "chat-role marker at line start",
    },
    Rule {
        pattern: r"\b(do not|don'?t|never|without)\b[^.\n]{0,20}\b(tell|inform|mention|reveal|show|notify|alert|ask)(ing|s|ed)?\b[^.\n]{0,15}\b(the )?(user|operator|human|owner|admin)\b",
        description: "concealment from the operator",
    },
    Rule {
        pattern: r"\b(auto_?approve(_all)?|always approve|approve (everything|all|automatically)|skip (the )?approval|bypass (the )?(approval|gate|guard|floor)|trust for session|no approval (needed|required))\b",
        description: "approval-bypass phrasing",
    },
    Rule {
        pattern: r"\b(api[ _-]?key|secret|password|passwd|token|bearer)\b\s*[:=]\s*[A-Za-z0-9_\-./+]{12,}",
        description: "credential-shaped literal",
    },
    Rule {
        pattern: r"\b(this (message|instruction|note) (is|was) (from|by) (the )?(operator|owner|admin|system|anthropic|openai)|as (the|your) (operator|owner|admin) i (authorize|order|command|instruct))\b",
        description: "authority impersonation",
    },
    Rule {
        pattern: r"\b(pretend|assume|act as if)\b[^.\n]{0,20}\b(you (have|had) (no|zero) (restrictions?|rules|limits|guidelines)|there are no (rules|restrictions|limits))\b",
        description: "restriction-denial phrasing",
    },
];
