//! Execution safety floor for the Philotic Stack: the L0 compiled-in,
//! unrecoverable-command blocklist.
//!
//! This crate implements **only** L0 of the layered execution safety floor
//! described in the Execution Safety Floor proposal
//! (`docs/architecture/EXEC_SAFETY_FLOOR_PROPOSAL.md`, "Slice 0"). L1
//! (operator deny globs), L2 (allowlist + approval modes), L3 (approval
//! context binding), and L4 (`explain` diagnostics) are later slices and are
//! deliberately not implemented here.
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

mod normalize;
mod patterns;

use std::sync::LazyLock;

use regex::{RegexSet, RegexSetBuilder};

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
