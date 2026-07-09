//! The L0 hardline blocklist: a compiled-in, non-configurable set of command
//! patterns with no recovery path if they run.
//!
//! Ported 1:1 in scope from Hermes' `HARDLINE_PATTERNS`
//! (`scratchpad/hermes-agent/tools/approval.py`, lines 366-403): recursive
//! `rm` of `/`, protected system roots, and home; raw filesystem format and
//! block-device overwrite; the classic fork bomb; `kill -1`; and
//! command-position-anchored shutdown/reboot spellings. Two Philotic-specific
//! self-termination entries are added in the same spirit — see
//! [`HARDLINE_PATTERNS`] below.
//!
//! The list is deliberately tiny: only things with no recovery path.
//! Recoverable-but-costly operations (`git reset --hard`, `chmod -R 777`,
//! `curl | sh`) belong in a future L1/L2 layer where an operator's
//! `auto_approve_all` or an allowlist entry can legitimately pass them
//! through — that is what those layers are *for*. This floor exists so
//! nothing, not even an operator's own "trust everything" flag, can.

/// Regex fragment matching the *start* of a command — the position where a
/// shell would begin parsing a new command word. Used so `shutdown`,
/// `reboot`, etc. only trip the floor when they are a real command, not
/// prose inside a quoted argument (`grep 'shutdown' log.txt`, `echo reboot`).
///
/// Matches: start of string; after a command separator (`;`, `&`, `|`,
/// newline); after a subshell opener (`` $( `` or a backtick); optionally
/// followed by `sudo [flags]`, `env VAR=VAL...`, and/or `exec`/`nohup`/
/// `setsid`/`time` wrapper words, so `sudo rm -rf /*` and
/// `env FOO=bar exec reboot` are still caught at the real command.
const CMDPOS: &str = concat!(
    r"(?:^|[;&|\n`]|\$\()",
    r"\s*",
    r"(?:sudo\s+(?:-[^\s]+\s+)*)?",
    r"(?:env\s+(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*)?",
    r"(?:(?:exec|nohup|setsid|time)\s+)*",
    r"\s*",
);

/// Protected system roots whose recursive deletion has no recovery path.
const HARDLINE_SYSTEM_DIRS: &str = concat!(
    r"/home|/home/\*|/root|/root/\*|/etc|/etc/\*|/usr|/usr/\*|",
    r"/var|/var/\*|/bin|/bin/\*|/sbin|/sbin/\*|/boot|/boot/\*|/lib|/lib/\*",
);

/// Any root-anchored path whose components collapse back to `/` in the
/// shell: a bare `/`, repeated slashes (`//`), and `.`/`..` segments all
/// resolve to root, optionally with a trailing glob (`/*`). A longer dot run
/// or any real path segment falls through — `/tmp`, `/home`, `/.config` are
/// NOT root and must not trip this rule.
const HARDLINE_ROOT_PATH: &str = r"/(?:(?:\.\.?)?/)*(?:\.\.?)?\**|/ \*";

/// `normalize_command` folds `$HOME`/`${HOME}` to `~`, so a single tilde
/// alternative (optionally followed by `/` or `/*`) covers every home
/// spelling the floor needs to catch.
const HARDLINE_HOME_PATH: &str = r"~(?:/?|/\*)?";

/// Builds the destructive-path argument matcher shared by the three `rm`
/// hardline rules.
///
/// The path token in `rm -rf /` is almost always written quoted in real
/// shells (`rm -rf "/"`, `rm -rf "$HOME"`). A bare-token anchor alone misses
/// these: the surrounding quote breaks the trailing whitespace/end-of-string
/// terminator, letting `rm -rf "/"` slip past the floor entirely. Accept the
/// path either fully wrapped in a matching quote pair, or bare with a
/// terminator — whitespace, end-of-string, or a shell metacharacter that can
/// follow a command substitution (`` `rm -rf /` ``, `$(rm -rf /)`).
fn hardline_rm_path(path_alt: &str) -> String {
    const TAIL: &str = r#"(?:\s|$|[)`;|&])"#;
    format!(r#"(?:["'](?:{path_alt})["']|(?:{path_alt}){TAIL})"#)
}

/// `rm` plus its flag group, shared by the three `rm` hardline rules.
/// Anchored to [`CMDPOS`] so the rule fires only when `rm` is an actual
/// command word — not when the literal string "rm -rf /" appears as *data*
/// inside another command's argument, e.g.
/// `git commit -m "block rm -rf / spellings"`.
fn rm_flag_prefix() -> String {
    format!(r"{CMDPOS}rm\s+(?:-[^\s]*\s+)*")
}

/// One hardline rule: a compiled-pattern source string paired with the
/// human-readable description surfaced in [`crate::HardlineMatch`].
pub(crate) struct HardlineRule {
    pub(crate) pattern: String,
    pub(crate) description: &'static str,
}

/// The compiled-in L0 blocklist. See module docs for scope and rationale.
///
/// A `LazyLock` (not a `const`) because each pattern is assembled with
/// `format!`/`concat!` from the shared [`CMDPOS`] fragment — the *content*
/// is still fully static and compiled into the binary; nothing here reads
/// an env var, a config record, or any other runtime input.
pub(crate) static HARDLINE_PATTERNS: std::sync::LazyLock<Vec<HardlineRule>> =
    std::sync::LazyLock::new(|| {
        vec![
            HardlineRule {
                pattern: rm_flag_prefix() + &hardline_rm_path(HARDLINE_ROOT_PATH),
                description: "recursive delete of root filesystem",
            },
            HardlineRule {
                pattern: rm_flag_prefix() + &hardline_rm_path(HARDLINE_SYSTEM_DIRS),
                description: "recursive delete of system directory",
            },
            HardlineRule {
                pattern: rm_flag_prefix() + &hardline_rm_path(HARDLINE_HOME_PATH),
                description: "recursive delete of home directory",
            },
            HardlineRule {
                pattern: r"\bmkfs(?:\.[a-z0-9]+)?\b".to_string(),
                description: "format filesystem (mkfs)",
            },
            HardlineRule {
                pattern: r"\bdd\b[^\n]*\bof=/dev/(?:sd|nvme|hd|mmcblk|vd|xvd)[a-z0-9]*".to_string(),
                description: "dd to raw block device",
            },
            HardlineRule {
                pattern: r">\s*/dev/(?:sd|nvme|hd|mmcblk|vd|xvd)[a-z0-9]*\b".to_string(),
                description: "redirect to raw block device",
            },
            HardlineRule {
                pattern: r":\(\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;\s*:".to_string(),
                description: "fork bomb",
            },
            HardlineRule {
                pattern: r"\bkill\s+(?:-[^\s]+\s+)*-1\b".to_string(),
                description: "kill all processes",
            },
            HardlineRule {
                pattern: format!(r"{CMDPOS}(?:shutdown|reboot|halt|poweroff)\b"),
                description: "system shutdown/reboot",
            },
            HardlineRule {
                pattern: format!(r"{CMDPOS}init\s+[06]\b"),
                description: "init 0/6 (shutdown/reboot)",
            },
            HardlineRule {
                pattern: format!(r"{CMDPOS}systemctl\s+(?:poweroff|reboot|halt|kexec)\b"),
                description: "systemctl poweroff/reboot",
            },
            HardlineRule {
                pattern: format!(r"{CMDPOS}telinit\s+[06]\b"),
                description: "telinit 0/6 (shutdown/reboot)",
            },
            // ── Philotic-specific self-termination entries ─────────────────
            // Same spirit as the shutdown/reboot rules above: an agent tearing
            // down its own hotel process is unrecoverable from inside the
            // agent (there is no one left running to undo it).
            HardlineRule {
                pattern: format!(
                    r"{CMDPOS}launchctl\s+(?:bootout|unload|kill)\b[^;&|\n]*\bcom\.philotic\."
                ),
                description: "launchctl teardown of a philotic hotel service",
            },
            HardlineRule {
                pattern: format!(r"{CMDPOS}(?:pkill|killall)\s+(?:-[^\s]+\s+)*aiua\b"),
                description: "kill the aiua hotel process",
            },
        ]
    });
