//! Execution safety floor for the Philotic Stack: the L0 compiled-in,
//! unrecoverable-command blocklist.
//!
//! This crate implements L0 of the layered execution safety floor — the
//! compiled-in, non-configurable hardline blocklist ([`detect_hardline`]) — and
//! the *detector* half of an L1 network-egress fence
//! ([`detect_network_egress`]). Both are pure detectors that read no config;
//! the difference is what a `Some(_)` means. An L0 hit is an unconditional
//! block no layer above can lift. An L1 network-egress hit is a *classification*
//! the caller turns into a policy decision (loopback/tailnet allowed, everything
//! else redirected to the governed egress fabric) — see [`detect_network_egress`]
//! and the `net_egress` module. The remaining L2 (allowlist + approval modes),
//! L3 (approval context binding), and full L4 (`explain` diagnostics) layers are
//! later slices and are deliberately not implemented here.
//!
//! [`detect_hardline`] must be called at the **last hop before process
//! spawn** in every raw-shell dispatch point in the stack — currently
//! `philote::runtime::run_bash_command` and
//! `tool_runner::execute_bash_tool` — and its `Some(_)` result must always
//! short-circuit the spawn. No policy record, session flag
//! (`auto_approve_all`), or approval state can reach this check: the pattern
//! table is a `const`/function-local list compiled into the binary, there is
//! no env var read, and there is deliberately no way to pass configuration
//! into [`detect_hardline`] at all.
//!
//! Threat model: an *honest-but-wrong* agent (a looping philote, a
//! prompt-injected turn, a stale trust flag) — not a sandbox against a
//! process that already controls the guest binary. See the proposal's
//! "Risks and Non-Goals" section.

mod net_egress;
mod normalize;
mod patterns;

use std::sync::LazyLock;

use regex::{RegexSet, RegexSetBuilder};

pub use net_egress::NetworkEgressMatch;
pub use normalize::normalize_command;

/// A command matched against the L0 hardline blocklist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardlineMatch {
    /// Human-readable description of the matched rule (e.g. "recursive
    /// delete of root filesystem"), suitable for logging and for the L4
    /// `explain` diagnostic in a later slice.
    pub description: &'static str,
}

impl HardlineMatch {
    /// The message to return to the model as the tool result when a command
    /// is denied. Explicitly tells the model not to retry or rephrase —
    /// no wording of the same command will ever pass this floor.
    pub fn denial_message(&self) -> String {
        format!(
            "blocked by the compiled-in execution safety floor ({desc}) — \
             this command cannot be run via the agent, not even with an \
             approval or auto_approve_all. Do not retry or rephrase it; \
             run it yourself in a terminal if you need this done.",
            desc = self.description
        )
    }
}

struct CompiledHardline {
    set: RegexSet,
    descriptions: Vec<&'static str>,
}

static HARDLINE: LazyLock<CompiledHardline> = LazyLock::new(|| {
    let rules = &*patterns::HARDLINE_PATTERNS;
    let pattern_strs: Vec<&str> = rules.iter().map(|rule| rule.pattern.as_str()).collect();
    let set = RegexSetBuilder::new(&pattern_strs)
        .case_insensitive(true)
        .build()
        .expect("exec-guard: hardline patterns must compile — this is a compile-time invariant");
    let descriptions = rules.iter().map(|rule| rule.description).collect();
    CompiledHardline { set, descriptions }
});

/// Checks `command` against the L0 hardline blocklist.
///
/// Returns `Some(HardlineMatch)` for the first rule (in declaration order)
/// that matches the normalized command, or `None` if nothing in the floor
/// applies. `None` is not an approval — it only means L0 has no opinion;
/// higher layers (L1-L4, not implemented in this slice) still apply.
pub fn detect_hardline(command: &str) -> Option<HardlineMatch> {
    let normalized = normalize_command(command);
    let idx = HARDLINE.set.matches(&normalized).iter().next()?;
    Some(HardlineMatch {
        description: HARDLINE.descriptions[idx],
    })
}

/// Detects raw network egress (`curl`, `wget`, `nc`, `/dev/tcp`, interpreter
/// sockets) in `command`, returning the primitive and the target host when the
/// host is written literally.
///
/// This is a **detector, not a policy** — like [`detect_hardline`] it reads no
/// config. Unlike [`detect_hardline`], a `Some(_)` result is *not* an
/// unconditional block: the caller decides, using the returned
/// [`NetworkEgressMatch::host`] against its egress policy (loopback/tailnet are
/// normally allowed; everything else, and any `None` host, is denied and
/// redirected to the governed `http:<binding>.request` fabric). See the
/// `net_egress` module for scope and the fail-closed rationale.
pub fn detect_network_egress(command: &str) -> Option<NetworkEgressMatch> {
    let normalized = normalize_command(command);
    net_egress::detect(&normalized)
}

#[cfg(test)]
mod tests {
    use super::detect_hardline;

    #[test]
    fn returns_none_for_ordinary_commands() {
        assert!(detect_hardline("git status").is_none());
        assert!(detect_hardline("ls -la /tmp").is_none());
    }

    #[test]
    fn denial_message_names_not_retry() {
        let m = detect_hardline("rm -rf /").expect("must match");
        let msg = m.denial_message();
        assert!(msg.contains("Do not retry"));
        assert!(msg.contains("recursive delete of root filesystem"));
    }
}
