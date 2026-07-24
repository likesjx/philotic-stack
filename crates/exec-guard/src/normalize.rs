//! Command-string de-obfuscation used before hardline pattern matching.
//!
//! Ported (as a deliberately reduced subset — see module docs on
//! [`crate::detect_hardline`]) from Hermes' `_normalize_command_for_detection`
//! (`scratchpad/hermes-agent/tools/approval.py`). Each step defeats one shell
//! idiom a model could use to keep the *literal* dangerous string out of the
//! command text while the shell still executes it identically.

use std::sync::LazyLock;

use regex::Regex;

/// `\<newline>` (optionally `\r\n`) is how a shell spells "join this line with
/// the next" — `rm -rf \<newline>/` executes as `rm -rf /`. Must run before
/// any other rewrite touches the backslash.
static LINE_CONTINUATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\\r?\n").expect("static regex"));

/// Empty adjacent quote pairs split a command word without changing what the
/// shell executes: `r''m` and `r""m` both run `rm`. A single non-recursive
/// pass mirrors the shell's own token-level (not char-level) removal and
/// matches Hermes' own single-pass `re.sub` — see module docs.
static EMPTY_QUOTES_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"''|"""#).expect("static regex"));

/// `$IFS` / `${IFS...}` word-separator expansion. Every POSIX shell defaults
/// `IFS` to `<space><tab><newline>`, so `rm${IFS}-rf${IFS}/` runs identically
/// to `rm -rf /`. Because the hardline patterns anchor on literal `\s`
/// between command and arguments, leaving `$IFS` unexpanded would let this
/// slip past every pattern, including the root-delete floor.
static IFS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{IFS\b[^}]*\}|\$IFS\b").expect("static regex"));

/// `$HOME` / `${HOME}` folded to `~` so a single pattern (`~` in
/// [`crate::patterns::HARDLINE_PATTERNS`]) catches every home-directory
/// spelling the corpus exercises, instead of three parallel alternations.
/// Exact-case only: POSIX environment variable names are case-sensitive, so
/// `$home` is not `$HOME` in a real shell and folding it would be a false
/// floor, not a real one.
static HOME_VAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{HOME\}|\$HOME\b").expect("static regex"));

/// Reduce a raw command string to the form hardline patterns are written
/// against.
///
/// This is intentionally a *subset* of Hermes' full normalization (no NFKC
/// Unicode fold, no absolute-home-path resolution via `$HOME` env lookup, no
/// quote-aware tokenizer for multi-word deobfuscation) — see
/// [`crate::detect_hardline`] for what is deferred to a later slice and why
/// none of it is required for the L0 corpus this slice ships against.
pub fn normalize_command(command: &str) -> String {
    let no_nulls: std::borrow::Cow<'_, str> = if command.contains('\0') {
        command.replace('\0', "").into()
    } else {
        command.into()
    };
    let joined = LINE_CONTINUATION_RE.replace_all(&no_nulls, "");
    let no_empty_quotes = EMPTY_QUOTES_RE.replace_all(&joined, "");
    let ifs_expanded = IFS_RE.replace_all(&no_empty_quotes, " ");
    HOME_VAR_RE.replace_all(&ifs_expanded, "~").into_owned()
}

#[cfg(test)]
mod tests {
    use super::normalize_command;

    #[test]
    fn collapses_backslash_newline_continuation() {
        assert_eq!(normalize_command("rm -rf \\\n/"), "rm -rf /");
    }

    #[test]
    fn strips_empty_quote_pairs() {
        assert_eq!(normalize_command("r''m -rf /"), "rm -rf /");
        assert_eq!(normalize_command(r#"r""m -rf /"#), "rm -rf /");
    }

    #[test]
    fn expands_ifs_to_space() {
        assert_eq!(normalize_command("rm${IFS}-rf${IFS}/"), "rm -rf /");
        assert_eq!(normalize_command("rm$IFS-rf$IFS/"), "rm -rf /");
    }

    #[test]
    fn folds_home_var_to_tilde() {
        assert_eq!(normalize_command("rm -rf $HOME"), "rm -rf ~");
        assert_eq!(normalize_command("rm -rf ${HOME}"), "rm -rf ~");
    }

    #[test]
    fn leaves_lowercase_home_var_alone() {
        // $home is not a POSIX-valid reference to $HOME; folding it would be
        // a false floor rather than a real one.
        assert_eq!(normalize_command("rm -rf $home"), "rm -rf $home");
    }

    #[test]
    fn is_a_no_op_on_ordinary_commands() {
        assert_eq!(normalize_command("git status"), "git status");
    }
}
